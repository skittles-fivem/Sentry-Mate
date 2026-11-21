//! The car's firmware fingerprints, from its logs: what each module's info frames on the vehicle bus said
//! ([`dbc::firmware`], tallied per log by [`timeline::firmware`]), confirmed per awake span, kept as a history per
//! field, and the "Tesla update installed" events found where a confirmed value changed between two spans.
//!
//! Kept in the db's sealed key/value store, all of it made again from the logs (a log decoded again gives the same),
//! except the names an owner typed:
//!
//! ```text
//! engine  car/<id>/fw/day/<day>   the tallies of the logs that start that UTC day (deduplicated: most share one)
//! engine  car/<id>/fw/sum/<day>   that day's confirmed values per field, as runs (value, first and last seen)
//! engine  car/<id>/fw/history     every day's runs joined, the days that have some, and the updates found
//! engine  car/<id>/fw/names       the versions an owner typed for updates, and the fingerprint sets they name
//! ```
//!
//! **Confirmed.** The logs of an awake span (logs with fingerprints less than [`SPAN_GAP_MS`] apart, the span ending
//! with a log closed `quiet`, and at UTC midnight) are counted together; a value is taken once it was seen
//! [`timeline::firmware::CONFIRM`] times in the span. A corrupt frame (a value seen once) never makes a run, so it
//! never looks like an update. A field a span did not confirm says nothing: an unknown, not a change.
//!
//! **Updates.** Where a field's runs change value, the update came between the last time the old value was seen and
//! the first time the new one was. Changes whose windows meet are one update (an update reflashes several modules
//! while the car is parked): it is placed between the last span with the old set and the first with the new one. It
//! becomes a row of the car's software updates ([`db::UpdateRow`], source live, input SMO, an id with
//! [`FW_BIT`]), so the updates list, the timeline, MQTT's `software_changed` and the totals show it.
//!
//! **Names.** An owner can type the version of an update (`2025.38.6`): it is kept for that update and for its new
//! fingerprint set (every field's value after it), so a later update to the same set gets the name by itself
//! (`named_by: "auto"`). Clearing an update's name keeps it without one, even when its set is named elsewhere.
//!
//! Bounds: per log a few dozen values ([`timeline::firmware::MAX_VALUES`] at most); a day's tallies share one set
//! when nothing changed; a day's runs are one or two per field; the history is a few runs per field and an update a
//! few times a year. A normal log only extends runs (one day's records and the history written); a change of what
//! was seen (a log taken out, a value that does not fit) reads every day's runs once.

use crate::records::{self, NS};
use crate::{Engine, Event, Result, lock};
use db::UpdateRow;
use model::{CarId, Inputs, Source, Update};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use timeline::firmware::{CONFIRM, LogFirmware};

/// The id bit of an update found from fingerprints (imported rows have bit 63; the timeline's ids use neither).
pub const FW_BIT: u64 = 1 << 62;

/// Whether an update id is one found from fingerprints.
pub fn is_fingerprint(id: u64) -> bool {
    id & FW_BIT != 0 && id & (1 << 63) == 0
}

/// Logs with fingerprints this far apart (or more) are two awake spans.
pub const SPAN_GAP_MS: u64 = 10 * 60_000;

/// One distinct set of tallies (most logs of a day share one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Tally {
    #[serde(rename = "f")]
    field: String,
    #[serde(rename = "v")]
    value: u64,
    /// Sightings, at most [`CONFIRM`] (more says nothing more, and lets logs share a set).
    #[serde(rename = "n")]
    count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LogRef {
    /// Index into the day's sets.
    set: u32,
    /// The first and last fingerprint frame (ms).
    from: u64,
    to: u64,
    /// Closed `quiet`: the car went to sleep after it.
    #[serde(default)]
    quiet: bool,
    #[serde(default)]
    hints: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct DayLogs {
    sets: Vec<Vec<Tally>>,
    logs: BTreeMap<String, LogRef>,
}

/// A value of one field, from when it was first seen to when it was last seen (ms).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub value: u64,
    pub first_ms: u64,
    pub last_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct DaySum {
    /// Field name → its runs, oldest first.
    runs: BTreeMap<String, Vec<Run>>,
    hints: u32,
    hint_last_ms: Option<u64>,
}

/// One field's change in an update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub field: String,
    pub old: u64,
    pub new: u64,
}

/// An update found from fingerprints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FwEvent {
    pub id: u64,
    /// The last time an old value was seen, and the first time a new one was (ms).
    pub from_ms: u64,
    pub to_ms: u64,
    /// By field name.
    pub changes: Vec<Change>,
    /// The fingerprint set after it (every field's value), hashed: what a typed version names.
    pub set: String,
}

/// A car's fingerprint history.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    /// The days with runs.
    pub days: BTreeSet<u32>,
    pub runs: BTreeMap<String, Vec<Run>>,
    pub events: Vec<FwEvent>,
    /// The bridge's `# firmware changed:` lines (a hint only), and the newest one's log.
    pub hints: u64,
    pub hint_last_ms: Option<u64>,
}

/// The versions an owner typed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Names {
    /// Update id → its version, or `None`: cleared (no name, not even one its set has).
    events: BTreeMap<u64, Option<String>>,
    /// Fingerprint set → the version, and the update it was typed on.
    sets: BTreeMap<String, (String, u64)>,
}

fn day_key(car: CarId, d: u32) -> String {
    format!("car/{car}/fw/day/{d}")
}
fn sum_key(car: CarId, d: u32) -> String {
    format!("car/{car}/fw/sum/{d}")
}
fn history_key(car: CarId) -> String {
    format!("car/{car}/fw/history")
}
fn names_key(car: CarId) -> String {
    format!("car/{car}/fw/names")
}

fn put_or_delete<T: Serialize>(db: &db::Db, key: &str, v: &T, empty: bool) -> Result<()> {
    if empty {
        db.kv().delete(NS, key)?;
        Ok(())
    } else {
        records::put(db, NS, key, v)
    }
}

/// A car's fingerprint history (empty when its logs had none).
pub fn history(db: &db::Db, car: CarId) -> Result<History> {
    Ok(records::get(db, NS, &history_key(car))?.unwrap_or_default())
}

/// The day's confirmed runs, from its logs' tallies (see the module's notes).
fn summarize(d: &DayLogs) -> DaySum {
    let mut logs: Vec<(&String, &LogRef)> = d.logs.iter().collect();
    logs.sort_by_key(|(n, l)| (l.from, (*n).clone()));
    let mut sum = DaySum::default();
    // (field, value) → (count, first, last, rank in its first log)
    let mut span: BTreeMap<(String, u64), (u32, u64, u64, usize)> = BTreeMap::new();
    let mut obs: Vec<(String, Run, usize)> = Vec::new();
    let flush = |span: &mut BTreeMap<(String, u64), (u32, u64, u64, usize)>, obs: &mut Vec<(String, Run, usize)>| {
        for ((field, value), (n, first, last, rank)) in std::mem::take(span) {
            if n >= CONFIRM {
                obs.push((field, Run { value, first_ms: first, last_ms: last }, rank));
            }
        }
    };
    let mut prev: Option<&LogRef> = None;
    for (_, l) in logs {
        if l.hints > 0 {
            sum.hints = sum.hints.saturating_add(l.hints);
            sum.hint_last_ms = sum.hint_last_ms.max(Some(l.to));
        }
        if prev.is_some_and(|p| p.quiet || l.from.saturating_sub(p.to) >= SPAN_GAP_MS) {
            flush(&mut span, &mut obs);
        }
        let Some(set) = d.sets.get(l.set as usize) else { continue };
        for (rank, t) in set.iter().enumerate() {
            let e = span.entry((t.field.clone(), t.value)).or_insert((0, l.from, l.to, rank));
            e.0 = e.0.saturating_add(t.count);
            e.1 = e.1.min(l.from);
            e.2 = e.2.max(l.to);
        }
        prev = Some(l);
    }
    flush(&mut span, &mut obs);
    obs.sort_by(|a, b| (a.1.first_ms, a.2, &a.0).cmp(&(b.1.first_ms, b.2, &b.0)));
    for (field, run, _) in obs {
        sum.runs.entry(field).or_default().push(run);
    }
    for runs in sum.runs.values_mut() {
        *runs = join(std::mem::take(runs));
    }
    sum
}

/// Runs in time order with the neighbours of one value joined.
fn join(mut runs: Vec<Run>) -> Vec<Run> {
    runs.sort_by_key(|r| (r.first_ms, r.last_ms, r.value));
    let mut out: Vec<Run> = Vec::with_capacity(runs.len());
    for r in runs {
        match out.last_mut() {
            Some(l) if l.value == r.value => {
                l.first_ms = l.first_ms.min(r.first_ms);
                l.last_ms = l.last_ms.max(r.last_ms);
            }
            _ => out.push(r),
        }
    }
    out
}

/// Whether `new` only adds to `old`: every old run is inside a new run of its value (a log added to the day).
fn grows(old: &DaySum, new: &DaySum) -> bool {
    new.hints >= old.hints
        && old.runs.iter().all(|(f, rs)| {
            let Some(n) = new.runs.get(f) else { return false };
            rs.iter()
                .all(|r| n.iter().any(|x| x.value == r.value && x.first_ms <= r.first_ms && x.last_ms >= r.last_ms))
        })
}

/// Joins `add` into `runs`; `false` when a run of another value would fall inside one already there (the history
/// is then made again from every day).
fn merge_into(runs: &mut Vec<Run>, add: &[Run]) -> bool {
    let mut all = runs.clone();
    all.extend_from_slice(add);
    let joined = join(all);
    // (a value changing within one log overlaps the old one by that log's minute)
    if joined.windows(2).any(|w| w[1].first_ms.saturating_add(60_000) < w[0].last_ms) {
        return false;
    }
    *runs = joined;
    true
}

/// The hash of a fingerprint set: lowercase hex, 16 digits.
fn set_hash(set: &BTreeMap<&str, u64>) -> String {
    let mut h = Sha256::new();
    for (f, v) in set {
        h.update(format!("{f}={v}\n"));
    }
    hex::encode(&h.finalize()[..8])
}

/// The updates in a history's runs (see the module's notes), oldest first.
pub(crate) fn events_of(car: CarId, runs: &BTreeMap<String, Vec<Run>>) -> Vec<FwEvent> {
    struct C<'a> {
        field: &'a str,
        old: u64,
        new: u64,
        from: u64,
        to: u64,
    }
    let mut changes: Vec<C<'_>> = Vec::new();
    for (f, rs) in runs {
        for w in rs.windows(2) {
            if w[0].value != w[1].value {
                let to = w[1].first_ms;
                changes.push(C { field: f, old: w[0].value, new: w[1].value, from: w[0].last_ms.min(to), to });
            }
        }
    }
    changes.sort_by_key(|c| (c.to, c.from));
    let mut groups: Vec<(u64, u64, Vec<C<'_>>)> = Vec::new();
    for c in changes {
        match groups.iter_mut().rev().find(|g| c.from <= g.1 && c.to >= g.0) {
            Some(g) => {
                g.0 = g.0.max(c.from);
                g.1 = g.1.min(c.to);
                g.2.push(c);
            }
            None => groups.push((c.from, c.to, vec![c])),
        }
    }
    groups.sort_by_key(|g| (g.0, g.1));
    let mut used = BTreeSet::new();
    let mut out = Vec::new();
    for (from, to, mut cs) in groups {
        cs.sort_by(|a, b| a.field.cmp(b.field));
        // every field's value after the update: the run in effect at its end (else the field's first)
        let mut after: BTreeMap<&str, u64> = BTreeMap::new();
        for (f, rs) in runs {
            if let Some(r) = rs.iter().rev().find(|r| r.first_ms <= to).or(rs.first()) {
                after.insert(f.as_str(), r.value);
            }
        }
        for c in &cs {
            after.insert(c.field, c.new);
        }
        let mut salt = 0u32;
        let id = loop {
            let mut h = Sha256::new();
            for c in &cs {
                h.update(format!("{}:{}>{}\n", c.field, c.old, c.new));
            }
            h.update(salt.to_le_bytes());
            let d = h.finalize();
            let low = u64::from_le_bytes([d[0], d[1], d[2], d[3], d[4], 0, 0, 0]);
            let id = FW_BIT | ((u64::from(car) & 0x3F_FFFF) << 40) | low;
            if used.insert(id) {
                break id;
            }
            salt += 1;
        };
        out.push(FwEvent {
            id,
            from_ms: from,
            to_ms: to,
            changes: cs.iter().map(|c| Change { field: c.field.to_string(), old: c.old, new: c.new }).collect(),
            set: set_hash(&after),
        });
    }
    out
}

fn names(db: &db::Db, car: CarId) -> Result<Names> {
    Ok(records::get(db, NS, &names_key(car))?.unwrap_or_default())
}

/// The version an update found from fingerprints shows, and who named it (`owner` or `auto`).
pub fn name_of(db: &db::Db, car: CarId, ev: &FwEvent) -> Result<Option<(String, &'static str)>> {
    let n = names(db, car)?;
    Ok(match n.events.get(&ev.id) {
        Some(Some(v)) => Some((v.clone(), "owner")),
        Some(None) => None,
        None => n.sets.get(&ev.set).map(|(v, by)| (v.clone(), if *by == ev.id { "owner" } else { "auto" })),
    })
}

impl Engine {
    /// Notes one log's fingerprints (`None`: it has none now: left out, or none in it), with whether it closed
    /// `quiet`. The car's lock is held.
    pub(crate) fn fw_note_log(&self, car: CarId, name: &str, fw: Option<(&LogFirmware, bool)>) -> Result<()> {
        let Some(start) = records::name_start_ms(name) else { return Ok(()) };
        let d = records::day_of(start);
        let db = &self.inner.db;
        let mut day: DayLogs = records::get(db, NS, &day_key(car, d))?.unwrap_or_default();
        let before = day.clone();
        day.logs.remove(name);
        if let Some((f, quiet)) = fw.filter(|(f, _)| !f.is_empty()) {
            let set: Vec<Tally> = f
                .seen
                .iter()
                .map(|s| Tally { field: s.field.clone(), value: s.value, count: s.count.min(CONFIRM) })
                .collect();
            let from = f.seen.iter().map(|s| s.first_ms).min().unwrap_or(start);
            let to = f.seen.iter().map(|s| s.last_ms).max().unwrap_or(start);
            let i = match day.sets.iter().position(|s| *s == set) {
                Some(i) => i,
                None => {
                    day.sets.push(set);
                    day.sets.len() - 1
                }
            };
            let set = u32::try_from(i).unwrap_or(u32::MAX);
            day.logs.insert(name.to_string(), LogRef { set, from, to, quiet, hints: f.hints });
        }
        if day == before {
            return Ok(());
        }
        compact(&mut day);
        self.fw_put_day(car, d, &day, false)
    }

    /// Takes these logs (by name) out of the fingerprints: a key moved to another car, a window cleared. The car's
    /// lock is held. The history is made again from every day.
    pub(crate) fn fw_forget(&self, car: CarId, names: &[String]) -> Result<()> {
        let db = &self.inner.db;
        let mut by_day: BTreeMap<u32, Vec<&String>> = BTreeMap::new();
        for n in names {
            if let Some(s) = records::name_start_ms(n) {
                by_day.entry(records::day_of(s)).or_default().push(n);
            }
        }
        let mut changed = false;
        for (d, ns) in by_day {
            let Some(mut day) = records::get::<DayLogs>(db, NS, &day_key(car, d))? else { continue };
            let n = day.logs.len();
            for name in ns {
                day.logs.remove(name);
            }
            if day.logs.len() != n {
                compact(&mut day);
                self.fw_put_day_only(car, d, &day)?;
                changed = true;
            }
        }
        if changed {
            self.fw_rebuild_history(car)?;
        }
        Ok(())
    }

    fn fw_put_day_only(&self, car: CarId, d: u32, day: &DayLogs) -> Result<DaySum> {
        let db = &self.inner.db;
        put_or_delete(db, &day_key(car, d), day, day.logs.is_empty())?;
        let sum = summarize(day);
        put_or_delete(db, &sum_key(car, d), &sum, sum.runs.is_empty() && sum.hints == 0)?;
        Ok(sum)
    }

    /// Writes a day's tallies and its runs, and brings the history up to date.
    fn fw_put_day(&self, car: CarId, d: u32, day: &DayLogs, force: bool) -> Result<()> {
        let db = &self.inner.db;
        let old: DaySum = records::get(db, NS, &sum_key(car, d))?.unwrap_or_default();
        let sum = self.fw_put_day_only(car, d, day)?;
        if sum == old && !force {
            return Ok(());
        }
        let mut h = history(db, car)?;
        let has = !sum.runs.is_empty() || sum.hints > 0;
        if !grows(&old, &sum) {
            return self.fw_rebuild_history(car);
        }
        let mut ok = true;
        for (f, rs) in &sum.runs {
            ok &= merge_into(h.runs.entry(f.clone()).or_default(), rs);
        }
        if !ok {
            return self.fw_rebuild_history(car);
        }
        if has {
            h.days.insert(d);
        }
        h.hints = h.hints.saturating_add(u64::from(sum.hints.saturating_sub(old.hints)));
        h.hint_last_ms = h.hint_last_ms.max(sum.hint_last_ms);
        self.fw_finish(car, h, false)
    }

    /// Makes the history again from every day's runs.
    pub(crate) fn fw_rebuild_history(&self, car: CarId) -> Result<()> {
        let db = &self.inner.db;
        let old = history(db, car)?;
        let meta = records::meta(db, car)?;
        let mut h = History::default();
        let mut all: BTreeMap<String, Vec<Run>> = BTreeMap::new();
        // the days that had runs, and the days with logs (a day whose runs are new is among them)
        for &d in old.days.iter().chain(meta.days.iter()).collect::<BTreeSet<_>>() {
            let Some(sum) = records::get::<DaySum>(db, NS, &sum_key(car, d))? else { continue };
            for (f, rs) in sum.runs {
                all.entry(f).or_default().extend(rs);
            }
            h.hints = h.hints.saturating_add(u64::from(sum.hints));
            h.hint_last_ms = h.hint_last_ms.max(sum.hint_last_ms);
            h.days.insert(d);
        }
        h.runs = all.into_iter().map(|(f, rs)| (f, join(rs))).collect();
        h.events = old.events;
        self.fw_finish(car, h, true)
    }

    /// Finds the updates of `h`, writes it, and makes the update rows match when they changed (or `force`).
    fn fw_finish(&self, car: CarId, mut h: History, force: bool) -> Result<()> {
        let events = events_of(car, &h.runs);
        let changed = events != h.events;
        h.events = events;
        let db = &self.inner.db;
        let empty = h.days.is_empty() && h.runs.is_empty() && h.events.is_empty();
        put_or_delete(db, &history_key(car), &h, empty)?;
        if changed || force {
            self.fw_sync_rows(car, &h)?;
        }
        Ok(())
    }

    /// The car's update rows found from fingerprints made to match its history's updates (with their names).
    pub(crate) fn fw_sync_rows(&self, car: CarId, h: &History) -> Result<()> {
        let db = &self.inner.db;
        let live = crate::pipeline::all_rows::<UpdateRow>(db, car, 0, u64::MAX, Some(Source::Live))?;
        let mut want: Vec<UpdateRow> = Vec::with_capacity(h.events.len());
        for ev in &h.events {
            let version = name_of(db, car, ev)?.map(|n| n.0).unwrap_or_default();
            want.push(UpdateRow {
                id: ev.id,
                update: Update {
                    car,
                    start_ms: ev.from_ms,
                    end_ms: Some(ev.to_ms),
                    version,
                    source: Source::Live,
                    inputs: Inputs::SMO,
                },
            });
        }
        let mut have: Vec<UpdateRow> = live.iter().filter(|u| is_fingerprint(u.id)).cloned().collect();
        have.sort_by_key(|u| u.id);
        let mut sorted = want.clone();
        sorted.sort_by_key(|u| u.id);
        if have == sorted {
            return Ok(());
        }
        let gone: Vec<u64> = have.iter().map(|u| u.id).filter(|id| !sorted.iter().any(|w| w.id == *id)).collect();
        db.delete::<UpdateRow>(car, &gone)?;
        db.upsert::<UpdateRow>(car, &sorted)?;
        self.emit(Event::Rows { car, kinds: vec!["updates"], from_ms: 0, to_ms: u64::MAX });
        Ok(())
    }

    /// An owner names (or, with `None`, clears the name of) an update found from fingerprints. `false` when the car
    /// has no such update.
    pub(crate) fn fw_set_name(&self, car: CarId, id: u64, version: Option<&str>) -> Result<bool> {
        let l = self.car_lock(car);
        let _held = lock(&l);
        let db = &self.inner.db;
        let h = history(db, car)?;
        let Some(ev) = h.events.iter().find(|e| e.id == id) else { return Ok(false) };
        let mut n = names(db, car)?;
        match version {
            Some(v) => {
                n.events.insert(id, Some(v.to_string()));
                n.sets.insert(ev.set.clone(), (v.to_string(), id));
            }
            None => {
                n.events.insert(id, None);
                if n.sets.get(&ev.set).is_some_and(|(_, by)| *by == id) {
                    n.sets.remove(&ev.set);
                }
            }
        }
        records::put(db, NS, &names_key(car), &n)?;
        self.fw_sync_rows(car, &h)?;
        Ok(true)
    }
}

/// Drops the sets no log uses any more (indexes kept in step).
fn compact(day: &mut DayLogs) {
    let used: BTreeSet<u32> = day.logs.values().map(|l| l.set).collect();
    if used.len() == day.sets.len() {
        return;
    }
    let mut map = BTreeMap::new();
    let mut sets = Vec::with_capacity(used.len());
    for (i, s) in std::mem::take(&mut day.sets).into_iter().enumerate() {
        let i = u32::try_from(i).unwrap_or(u32::MAX);
        if used.contains(&i) {
            map.insert(i, u32::try_from(sets.len()).unwrap_or(u32::MAX));
            sets.push(s);
        }
    }
    day.sets = sets;
    for l in day.logs.values_mut() {
        l.set = map.get(&l.set).copied().unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn tally(field: &str, value: u64, count: u32) -> Tally {
        Tally { field: field.into(), value, count }
    }

    fn day(logs: &[(&str, u64, Vec<Tally>, bool)]) -> DayLogs {
        let mut d = DayLogs::default();
        for (name, at, set, quiet) in logs {
            let i = match d.sets.iter().position(|s| s == set) {
                Some(i) => i,
                None => {
                    d.sets.push(set.clone());
                    d.sets.len() - 1
                }
            };
            d.logs
                .insert((*name).into(), LogRef { set: i as u32, from: *at, to: *at + 55_000, quiet: *quiet, hints: 0 });
        }
        d
    }

    #[test]
    fn confirmed_over_a_span_not_from_one_frame() {
        const T: u64 = 1_767_225_600_000;
        let d = day(&[
            ("a", T, vec![tally("BMS_appCrc", 1, 1), tally("BMS_bootCrc", 9, 3)], false),
            ("b", T + 60_000, vec![tally("BMS_appCrc", 1, 2), tally("BMS_bootCrc", 7, 1)], true),
            // after the sleep: a new span, one sighting only
            ("c", T + 120_000, vec![tally("BMS_appCrc", 1, 1)], false),
        ]);
        let s = summarize(&d);
        assert_eq!(s.runs["BMS_appCrc"], vec![Run { value: 1, first_ms: T, last_ms: T + 115_000 }]);
        assert_eq!(s.runs["BMS_bootCrc"], vec![Run { value: 9, first_ms: T, last_ms: T + 55_000 }]);
        assert_eq!(s.runs.len(), 2);
    }

    #[test]
    fn updates_group_changes_whose_windows_meet() {
        let r = |v, a, b| Run { value: v, first_ms: a, last_ms: b };
        let mut runs = BTreeMap::new();
        runs.insert("BMS_appCrc".to_string(), vec![r(1, 0, 100), r(2, 200, 300)]);
        // seen a little later after the update (a module sends it later)
        runs.insert("DIR_applicationCrc".to_string(), vec![r(5, 0, 90), r(6, 250, 300)]);
        // unchanged
        runs.insert("BMS_bootCrc".to_string(), vec![r(9, 0, 300)]);
        // a second update much later
        runs.get_mut("BMS_appCrc").unwrap().push(r(3, 1000, 1100));
        let ev = events_of(7, &runs);
        assert_eq!(ev.len(), 2, "{ev:?}");
        assert_eq!((ev[0].from_ms, ev[0].to_ms), (100, 200));
        let f: Vec<&str> = ev[0].changes.iter().map(|c| c.field.as_str()).collect();
        assert_eq!(f, vec!["BMS_appCrc", "DIR_applicationCrc"]);
        assert_eq!((ev[1].from_ms, ev[1].to_ms, ev[1].changes.len()), (300, 1000, 1));
        assert!(ev.iter().all(|e| is_fingerprint(e.id) && e.id >> 40 & 0x3F_FFFF == 7));
        assert_ne!(ev[0].set, ev[1].set);
        // the same runs, the same ids
        assert_eq!(events_of(7, &runs), ev);
        // no change: no update
        runs.remove("BMS_appCrc");
        runs.remove("DIR_applicationCrc");
        assert!(events_of(7, &runs).is_empty());
    }

    #[test]
    fn a_day_that_only_grows_extends_and_one_that_shrinks_does_not() {
        let r = |v, a, b| Run { value: v, first_ms: a, last_ms: b };
        let mut old = DaySum::default();
        old.runs.insert("f".into(), vec![r(1, 10, 20)]);
        let mut new = old.clone();
        new.runs.insert("f".into(), vec![r(1, 10, 30)]);
        assert!(grows(&old, &new) && !grows(&new, &old));
        let mut runs = vec![r(1, 0, 15)];
        assert!(merge_into(&mut runs, &[r(1, 10, 30)]));
        assert_eq!(runs, vec![r(1, 0, 30)]);
        // another value inside a run: made again from every day
        let mut runs = vec![r(1, 0, 10_000_000)];
        assert!(!merge_into(&mut runs, &[r(2, 500_000, 600_000)]));
    }

    #[test]
    fn unused_sets_go() {
        let mut d = day(&[("a", 0, vec![tally("f", 1, 3)], false), ("b", 0, vec![tally("f", 2, 3)], false)]);
        d.logs.remove("a");
        compact(&mut d);
        assert_eq!(d.sets, vec![vec![tally("f", 2, 3)]]);
        assert_eq!(d.logs["b"].set, 0);
    }
}
