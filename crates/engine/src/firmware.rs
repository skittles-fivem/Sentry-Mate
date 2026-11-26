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
//! the first time the new one was (each value's own times when one log saw both). Changes whose windows meet are one
//! step (an update reflashes several modules while the car is parked). After an install the modules restart and a
//! fingerprint can change several times, even go back and forth, before the set settles: steps less than
//! [`SETTLE_MS`] apart (and starting at most [`INSTALL_MS`] after the first new value was seen) are one install, one
//! update, each field's change from its value before the install to its value after it (a value that went back is
//! no change; an install that changed nothing is no update). It is placed between the last span with the old set
//! and the first with the settled one. It becomes a row of the car's software updates ([`db::UpdateRow`], source
//! live, input SMO, an id with [`FW_BIT`]), so the updates list, the timeline, MQTT's `software_changed` and the
//! totals show it.
//!
//! **Gaps.** The window is the readings next to the change: the last day with the old set and the first with the new
//! one. When a whole UTC day between them has no readings at all (the logs had none, or none came), the window says
//! only that the update came somewhere in there: the event is marked [`FwEvent::gap`] and its row does not stretch
//! over the days nobody read; it is placed where the new set was first seen (start and end both), so the timeline
//! shows no update of days and the list no install time.
//!
//! **Names.** An owner can type the version of an update (`2025.38.6`): it is kept for that update and for its new
//! fingerprint set (every field's value after it), so a later update to the same set gets the name by itself
//! (`named_by: "auto"`). Clearing an update's name keeps it without one, even when its set is named elsewhere.
//! An update's id and set are hashes of its changes and of the set after it, so both change when its changes do (a
//! late log confirms one more changed field, an earlier update with the same changes goes): its name follows it, by
//! its changes and window ([`Names::follow`]).
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

/// Fingerprint changes less than this after the last one are the same install: the set has not settled yet.
pub const SETTLE_MS: u64 = 30 * 60_000;

/// An install takes the changes that start at most this long after its first new value was seen.
pub const INSTALL_MS: u64 = 60 * 60_000;

/// How updates are found from the runs; a stored history found another way finds them again ([`Engine::fw_regroup`]).
const GROUPING: u32 = 1;

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
    /// When the values of a field the log saw more than one value of were first and last seen (index into the set,
    /// first, last; ms): which came first, and when, inside the log. Others take the log's `from` and `to`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    at: Vec<(u32, u64, u64)>,
}

impl LogRef {
    /// When the set's tally `i` was first and last seen in this log.
    fn seen(&self, i: usize) -> (u64, u64) {
        match self.at.iter().find(|a| a.0 as usize == i) {
            Some(&(_, first, last)) => (first, last),
            None => (self.from, self.to),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DayLogs {
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
    /// No readings at all between `from_ms` and `to_ms` for a whole UTC day or more: when in there it came is not
    /// known (see the module's notes).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gap: bool,
}

impl FwEvent {
    /// Where its update row goes: the window, or, across a gap, the first time the new set was seen.
    pub fn row_span(&self) -> (u64, u64) {
        if self.gap { (self.to_ms, self.to_ms) } else { (self.from_ms, self.to_ms) }
    }
}

/// Whether a whole UTC day between `from_ms` and `to_ms` had no readings (`days`: the days that had some).
pub(crate) fn gap_between(days: &BTreeSet<u32>, from_ms: u64, to_ms: u64) -> bool {
    let (a, b) = (records::day_of(from_ms), records::day_of(to_ms));
    b > a.saturating_add(1) && (a.saturating_add(1)..b).any(|d| !days.contains(&d))
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
    /// How its updates were found ([`GROUPING`]; 0: before installs were grouped).
    #[serde(default)]
    pub grouping: u32,
}

/// The versions an owner typed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Names {
    /// Update id → its version, or `None`: cleared (no name, not even one its set has).
    events: BTreeMap<u64, Option<String>>,
    /// Fingerprint set → the version, and the update it was typed on.
    sets: BTreeMap<String, (String, u64)>,
    /// What each update in `events` was when last seen (its changes and window): an update's id and set change
    /// when its changes do (one more field joins it, an update with the same changes goes), and its name follows
    /// it by this ([`Names::follow`]). Names kept before this was have none (their id is taken as it is).
    #[serde(default)]
    keys: BTreeMap<u64, Key>,
}

/// What an update was when it was named: its changes and window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Key {
    changes: Vec<Change>,
    from_ms: u64,
    to_ms: u64,
}

impl Key {
    fn of(ev: &FwEvent) -> Key {
        Key { changes: ev.changes.clone(), from_ms: ev.from_ms, to_ms: ev.to_ms }
    }

    /// Whether `ev` is the update this was: a window that meets this one's, and one of its changes (or, for the
    /// changes of an install grouped since, a change of one of its fields).
    fn is(&self, ev: &FwEvent) -> bool {
        self.from_ms <= ev.to_ms
            && ev.from_ms <= self.to_ms
            && self.changes.iter().any(|c| ev.changes.iter().any(|e| e == c || e.field == c.field))
    }
}

impl Names {
    /// The version `ev` shows, and who named it (`owner` or `auto`).
    fn lookup(&self, ev: &FwEvent) -> Option<(String, &'static str)> {
        let own = self.events.get(&ev.id).filter(|_| self.keys.get(&ev.id).is_none_or(|k| k.is(ev)));
        match own {
            Some(Some(v)) => Some((v.clone(), "owner")),
            Some(None) => None,
            None => self.sets.get(&ev.set).map(|(v, by)| {
                let mine = *by == ev.id && self.keys.get(by).is_none_or(|k| k.is(ev));
                (v.clone(), if mine { "owner" } else { "auto" })
            }),
        }
    }

    /// Whether an owner cleared `ev`'s name (so no name is given to it from elsewhere either).
    fn cleared(&self, ev: &FwEvent) -> bool {
        matches!(self.events.get(&ev.id), Some(None)) && self.keys.get(&ev.id).is_none_or(|k| k.is(ev))
    }

    /// An owner names `ev` (or, with `None`, clears its name).
    fn set(&mut self, ev: &FwEvent, version: Option<&str>) {
        self.keys.insert(ev.id, Key::of(ev));
        match version {
            Some(v) => {
                self.events.insert(ev.id, Some(v.to_string()));
                self.sets.insert(ev.set.clone(), (v.to_string(), ev.id));
            }
            None => {
                self.events.insert(ev.id, None);
                if self.sets.get(&ev.set).is_some_and(|(_, by)| *by == ev.id) {
                    self.sets.remove(&ev.set);
                }
            }
        }
    }

    /// Moves the names from the updates `old` to the same updates in `new` (their ids and sets made again): a name
    /// stays with its update when another field's change joins it, or when the id it had goes to another update.
    /// A name whose update is not in `new` is kept, for when it comes back. `true` when something changed.
    fn follow(&mut self, old: &[FwEvent], new: &[FwEvent]) -> bool {
        if self.events.is_empty() {
            return false;
        }
        let before = self.clone();
        for ev in old {
            if self.events.contains_key(&ev.id) {
                self.keys.entry(ev.id).or_insert_with(|| Key::of(ev));
            }
        }
        // new event index → the named id it is; an id it keeps first, then one whose key it matches
        let mut of: BTreeMap<usize, u64> = BTreeMap::new();
        let mut taken: BTreeSet<u64> = BTreeSet::new();
        for (i, ev) in new.iter().enumerate() {
            if self.events.contains_key(&ev.id) && self.keys.get(&ev.id).is_none_or(|k| k.is(ev)) {
                of.insert(i, ev.id);
                taken.insert(ev.id);
            }
        }
        for (i, ev) in new.iter().enumerate() {
            if of.contains_key(&i) {
                continue;
            }
            // several named updates grouped into one install since: a typed name over a cleared one, then the last
            let found = self
                .keys
                .iter()
                .filter(|(id, k)| !taken.contains(id) && self.events.contains_key(id) && k.is(ev))
                .max_by_key(|(id, k)| (matches!(self.events.get(id), Some(Some(_))), k.to_ms, k.from_ms));
            if let Some((&id, _)) = found {
                of.insert(i, id);
                taken.insert(id);
            }
        }
        let mut events: BTreeMap<u64, Option<String>> = BTreeMap::new();
        let mut keys: BTreeMap<u64, Key> = BTreeMap::new();
        for (id, v) in &self.events {
            if !taken.contains(id) {
                events.insert(*id, v.clone());
                if let Some(k) = self.keys.get(id) {
                    keys.insert(*id, k.clone());
                }
            }
        }
        let mut moved: BTreeMap<u64, &FwEvent> = BTreeMap::new();
        for (&i, &id) in &of {
            let ev = &new[i];
            events.insert(ev.id, self.events.get(&id).cloned().flatten());
            keys.insert(ev.id, Key::of(ev));
            moved.insert(id, ev);
        }
        // a set named on an update that moved goes with it (to its new set), over one named elsewhere
        let old_sets = std::mem::take(&mut self.sets);
        let mut sets: BTreeMap<String, (String, u64)> = BTreeMap::new();
        for (set, (v, by)) in &old_sets {
            if !moved.contains_key(by) {
                sets.insert(set.clone(), (v.clone(), *by));
            }
        }
        for (v, by) in old_sets.into_values() {
            if let Some(ev) = moved.get(&by) {
                sets.insert(ev.set.clone(), (v, ev.id));
            }
        }
        self.events = events;
        self.keys = keys;
        self.sets = sets;
        *self != before
    }
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
            let (first, last) = l.seen(rank);
            let e = span.entry((t.field.clone(), t.value)).or_insert((0, first, last, rank));
            e.0 = e.0.saturating_add(t.count);
            e.1 = e.1.min(first);
            e.2 = e.2.max(last);
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

/// Runs in time order with the neighbours of one value joined. Runs seen over the same time keep the order they
/// come in (in a span: the order first seen), never the values' order.
fn join(mut runs: Vec<Run>) -> Vec<Run> {
    runs.sort_by_key(|r| (r.first_ms, r.last_ms));
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

/// The updates in a history's runs (see the module's notes), oldest first; `days` are the days with readings.
pub(crate) fn events_of(car: CarId, runs: &BTreeMap<String, Vec<Run>>, days: &BTreeSet<u32>) -> Vec<FwEvent> {
    events_settled(car, runs, days, SETTLE_MS)
}

/// The updates in the runs, steps less than `settle` apart one install (0: each step its own update, as updates were
/// found before installs were grouped).
fn events_settled(car: CarId, runs: &BTreeMap<String, Vec<Run>>, days: &BTreeSet<u32>, settle: u64) -> Vec<FwEvent> {
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
    // steps: changes whose windows meet (one reflash of several modules seen at once)
    let mut steps: Vec<(u64, u64, Vec<C<'_>>)> = Vec::new();
    for c in changes {
        match steps.iter_mut().rev().find(|g| c.from <= g.1 && c.to >= g.0) {
            Some(g) => {
                g.0 = g.0.max(c.from);
                g.1 = g.1.min(c.to);
                g.2.push(c);
            }
            None => steps.push((c.from, c.to, vec![c])),
        }
    }
    steps.sort_by_key(|g| (g.0, g.1));
    // installs: steps that follow each other before the set settled (the modules restarting after an install
    // flap and report in turn), each field's change from its value before the first to its value after the last
    struct Install<'a> {
        from: u64,
        to: u64,
        first_to: u64,
        steps: usize,
        cs: Vec<C<'a>>,
    }
    let mut installs: Vec<Install<'_>> = Vec::new();
    for (from, to, cs) in steps {
        match installs.last_mut() {
            Some(i) if from < i.to.saturating_add(settle) && from <= i.first_to.saturating_add(INSTALL_MS) => {
                i.to = i.to.max(to);
                i.steps += 1;
                i.cs.extend(cs);
            }
            _ => installs.push(Install { from, to, first_to: to, steps: 1, cs }),
        }
    }
    let mut groups: Vec<(u64, u64, Vec<C<'_>>)> = Vec::new();
    for mut i in installs {
        // one step: when all its changes were seen to have happened; more: from the last time the old set was
        // seen to the first time the settled one was
        if i.steps > 1 {
            i.from = i.cs.iter().map(|c| c.from).min().unwrap_or(i.from);
            i.to = i.cs.iter().map(|c| c.to).max().unwrap_or(i.to);
        }
        i.cs.sort_by_key(|c| (c.to, c.from));
        let mut net: Vec<C<'_>> = Vec::new();
        for c in i.cs {
            match net.iter_mut().find(|n| n.field == c.field) {
                Some(n) => n.new = c.new,
                None => net.push(c),
            }
        }
        // a value that went back to what it was is no change
        net.retain(|c| c.old != c.new);
        if !net.is_empty() {
            groups.push((i.from, i.to, net));
        }
    }
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
            gap: gap_between(days, from, to),
        });
    }
    out
}

fn names(db: &db::Db, car: CarId) -> Result<Names> {
    Ok(records::get(db, NS, &names_key(car))?.unwrap_or_default())
}

/// The version an update found from fingerprints shows, and who named it (`owner` or `auto`).
pub fn name_of(db: &db::Db, car: CarId, ev: &FwEvent) -> Result<Option<(String, &'static str)>> {
    Ok(names(db, car)?.lookup(ev))
}

/// The name `ev` shows, who gave it (`owner`, or `auto` from another update to the same set), and whether an owner
/// cleared it: one read of the names.
pub fn naming_of(db: &db::Db, car: CarId, ev: &FwEvent) -> Result<(Option<(String, &'static str)>, bool)> {
    let n = names(db, car)?;
    Ok((n.lookup(ev), n.cleared(ev)))
}

/// The log a fingerprint frame at `ms` came in: the log of that UTC day whose fingerprint frames span `ms`, else the
/// first one after it that day (null: none kept). One day's log list read.
pub fn log_at(db: &db::Db, car: CarId, ms: u64) -> Result<Option<String>> {
    let Some(day) = records::get::<DayLogs>(db, NS, &day_key(car, records::day_of(ms)))? else { return Ok(None) };
    let mut logs: Vec<(&String, &LogRef)> = day.logs.iter().collect();
    logs.sort_by_key(|(n, l)| (l.from, (*n).clone()));
    let hit = logs.iter().find(|(_, l)| l.from <= ms && ms <= l.to).or_else(|| logs.iter().find(|(_, l)| l.from >= ms));
    Ok(hit.map(|(n, _)| (*n).clone()))
}

impl Engine {
    /// Notes one log's fingerprints (`None`: it has none now: left out, or none in it), with whether it closed
    /// `quiet`. The car's lock is held.
    pub(crate) fn fw_note_log(&self, car: CarId, name: &str, fw: Option<(&LogFirmware, bool)>) -> Result<()> {
        let Some(start) = records::name_start_ms(name) else { return Ok(()) };
        let d = records::day_of(start);
        let db = &self.inner.db;
        // a full rebuild's day of logs: noted in memory, written once the day's logs are done ([`Engine::fw_hold`])
        {
            let mine = self.holds_for(car);
            let mut held = lock(&self.inner.fw_held);
            if let Some(h) = held.as_mut().filter(|_| mine) {
                let day = match h.entry((car, d)) {
                    std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
                    std::collections::btree_map::Entry::Vacant(v) => {
                        v.insert(records::get(db, NS, &day_key(car, d))?.unwrap_or_default())
                    }
                };
                note_in(day, name, start, fw);
                return Ok(());
            }
        }
        let mut day: DayLogs = records::get(db, NS, &day_key(car, d))?.unwrap_or_default();
        let before = day.clone();
        note_in(&mut day, name, start, fw);
        if day == before {
            return Ok(());
        }
        self.fw_put_day(car, d, &day, false)
    }

    /// Holds the firmware notes of logs in memory (`on`) until [`Engine::fw_write_held`] writes them, a day at a
    /// time as the note of each log would have; `false`: no longer, and what was not written is not (as a kill would
    /// leave it).
    pub(crate) fn fw_hold(&self, on: bool) {
        *lock(&self.inner.fw_held) = on.then(BTreeMap::new);
    }

    /// Writes the firmware notes held ([`Engine::fw_hold`]) of each day, as its last log's note would have, and
    /// holds on.
    pub(crate) fn fw_write_held(&self) -> Result<()> {
        let days = match lock(&self.inner.fw_held).as_mut() {
            Some(h) => std::mem::take(h),
            None => return Ok(()),
        };
        for ((c, d), day) in days {
            let before: DayLogs = records::get(&self.inner.db, NS, &day_key(c, d))?.unwrap_or_default();
            if day != before {
                self.fw_put_day(c, d, &day, false)?;
            }
        }
        Ok(())
    }

    /// Notes made-up fingerprints for one log, for tests that lay a car's history down by hand: each value
    /// `(field, value, first_ms, last_ms)` taken as confirmed. Takes the car's lock.
    #[doc(hidden)]
    pub fn fw_note_values_for_tests(&self, car: CarId, name: &str, values: &[(&str, u64, u64, u64)]) -> Result<()> {
        let l = self.car_lock(car);
        let _held = lock(&l);
        let f = LogFirmware {
            seen: values
                .iter()
                .map(|&(field, value, first_ms, last_ms)| timeline::firmware::Seen {
                    field: field.to_string(),
                    value,
                    count: CONFIRM,
                    first_ms,
                    last_ms,
                })
                .collect(),
            hints: 0,
        };
        self.fw_note_log(car, name, Some((&f, true)))
    }

    /// Stores the car's fingerprint updates as they were found before an install's changes were one update (every
    /// step its own update, the history marked as found that old way), for tests of [`Engine::fw_regroup`]. Takes the
    /// car's lock.
    #[doc(hidden)]
    pub fn fw_store_ungrouped_for_tests(&self, car: CarId) -> Result<()> {
        let l = self.car_lock(car);
        let _held = lock(&l);
        let db = &self.inner.db;
        let mut h = history(db, car)?;
        let events = events_settled(car, &h.runs, &h.days, 0);
        let mut n = names(db, car)?;
        if n.follow(&h.events, &events) {
            records::put(db, NS, &names_key(car), &n)?;
        }
        h.events = events;
        h.grouping = 0;
        records::put(db, NS, &history_key(car), &h)?;
        self.fw_sync_rows(car, &h)
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
        let events = events_of(car, &h.runs, &h.days);
        let changed = events != h.events;
        let force = force || h.grouping != GROUPING;
        h.grouping = GROUPING;
        let db = &self.inner.db;
        if changed {
            let mut n = names(db, car)?;
            if n.follow(&h.events, &events) {
                records::put(db, NS, &names_key(car), &n)?;
            }
        }
        h.events = events;
        let empty = h.days.is_empty() && h.runs.is_empty() && h.events.is_empty();
        put_or_delete(db, &history_key(car), &h, empty)?;
        if changed || force {
            self.fw_sync_rows(car, &h)?;
        }
        Ok(())
    }

    /// Finds the updates of a history stored before they were found the way they are now ([`GROUPING`]): the
    /// changes of one install become one update, its name kept. One read when it is up to date. The car's lock is
    /// held.
    pub(crate) fn fw_regroup(&self, car: CarId) -> Result<()> {
        let h = history(&self.inner.db, car)?;
        if h.grouping == GROUPING || (h.runs.is_empty() && h.events.is_empty()) {
            return Ok(());
        }
        self.fw_finish(car, h, true)
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
                    start_ms: ev.row_span().0,
                    end_ms: Some(ev.row_span().1),
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
        n.set(ev, version);
        records::put(db, NS, &names_key(car), &n)?;
        self.fw_sync_rows(car, &h)?;
        Ok(true)
    }
}

/// One log's tallies and its entry (`set` still to fill in): the log's first and last fingerprint frame, and each
/// value's own first and last time for a field the log saw more than one value of.
fn log_ref(f: &LogFirmware, start: u64, quiet: bool) -> (Vec<Tally>, LogRef) {
    let set: Vec<Tally> =
        f.seen.iter().map(|s| Tally { field: s.field.clone(), value: s.value, count: s.count.min(CONFIRM) }).collect();
    let from = f.seen.iter().map(|s| s.first_ms).min().unwrap_or(start);
    let to = f.seen.iter().map(|s| s.last_ms).max().unwrap_or(start);
    let at = f
        .seen
        .iter()
        .enumerate()
        .filter(|(_, s)| f.seen.iter().any(|o| o.field == s.field && o.value != s.value))
        .map(|(i, s)| (u32::try_from(i).unwrap_or(u32::MAX), s.first_ms, s.last_ms))
        .collect();
    (set, LogRef { set: 0, from, to, quiet, hints: f.hints, at })
}

/// Drops the sets no log uses any more (indexes kept in step).
/// One log's fingerprints noted in its day's (none: taken out), the day's sets kept to those its logs use.
fn note_in(day: &mut DayLogs, name: &str, start: u64, fw: Option<(&LogFirmware, bool)>) {
    let had = day.logs.remove(name);
    let mut changed = had.is_some();
    if let Some((f, quiet)) = fw.filter(|(f, _)| !f.is_empty()) {
        let (set, mut log) = log_ref(f, start, quiet);
        let i = match day.sets.iter().position(|s| *s == set) {
            Some(i) => i,
            None => {
                day.sets.push(set);
                day.sets.len() - 1
            }
        };
        log.set = u32::try_from(i).unwrap_or(u32::MAX);
        changed |= had.as_ref() != Some(&log);
        day.logs.insert(name.to_string(), log);
    }
    if changed {
        compact(day);
    }
}

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
            d.logs.insert(
                (*name).into(),
                LogRef { set: i as u32, from: *at, to: *at + 55_000, quiet: *quiet, hints: 0, at: Vec::new() },
            );
        }
        d
    }

    #[test]
    fn confirmed_over_a_span_not_from_one_frame() {
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
        let r = |v, a: u64, b: u64| Run { value: v, first_ms: a * U, last_ms: b * U };
        let mut runs = BTreeMap::new();
        runs.insert("BMS_appCrc".to_string(), vec![r(1, 0, 100), r(2, 200, 300)]);
        // seen a little later after the update (a module sends it later)
        runs.insert("DIR_applicationCrc".to_string(), vec![r(5, 0, 90), r(6, 250, 300)]);
        // unchanged
        runs.insert("BMS_bootCrc".to_string(), vec![r(9, 0, 300)]);
        // a second update much later
        runs.get_mut("BMS_appCrc").unwrap().push(r(3, 1000, 1100));
        let ev = events_of(7, &runs, &BTreeSet::new());
        assert_eq!(ev.len(), 2, "{ev:?}");
        assert_eq!((ev[0].from_ms, ev[0].to_ms), (100 * U, 200 * U));
        let f: Vec<&str> = ev[0].changes.iter().map(|c| c.field.as_str()).collect();
        assert_eq!(f, vec!["BMS_appCrc", "DIR_applicationCrc"]);
        assert_eq!((ev[1].from_ms, ev[1].to_ms, ev[1].changes.len()), (300 * U, 1000 * U, 1));
        assert!(ev.iter().all(|e| is_fingerprint(e.id) && e.id >> 40 & 0x3F_FFFF == 7));
        assert_ne!(ev[0].set, ev[1].set);
        // the same runs, the same ids
        assert_eq!(events_of(7, &runs, &BTreeSet::new()), ev);
        // no change: no update
        runs.remove("BMS_appCrc");
        runs.remove("DIR_applicationCrc");
        assert!(events_of(7, &runs, &BTreeSet::new()).is_empty());
    }

    #[test]
    fn a_day_that_only_grows_extends_and_one_that_shrinks_does_not() {
        let r = |v, a: u64, b: u64| Run { value: v, first_ms: a * U, last_ms: b * U };
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

    const T: u64 = 1_767_225_600_000;
    /// The unit of the small made-up times: 100 of them are an hour.
    const U: u64 = 36_000;
    const MIN: u64 = 60_000;

    /// Made-up runs: per field, (value, first, last) in minutes after `T`.
    /// One field's made-up runs: (value, first, last) in minutes after `T`.
    type Field<'a> = (&'a str, &'a [(u64, i64, i64)]);

    fn runs_of(fields: &[Field<'_>]) -> BTreeMap<String, Vec<Run>> {
        let at = |m: i64| T.checked_add_signed(m * 60_000).unwrap();
        fields
            .iter()
            .map(|(f, rs)| {
                let rs = rs.iter().map(|&(value, a, b)| Run { value, first_ms: at(a), last_ms: at(b) }).collect();
                (f.to_string(), rs)
            })
            .collect()
    }

    #[test]
    fn fingerprints_that_flap_after_an_install_are_one_update_with_the_settled_set() {
        // parked from T: the front body controller's app CRC goes 06ac6a37 → 01b6621d → back → 01b6621d → 5e17c0de
        // → 01b6621d over ten minutes while the modules restart; its boot CRC goes away and back; the battery's
        // changes once, a little later; the rear drive unit's never
        let (a, b, c) = (0x06ac_6a37, 0x01b6_621d, 0x5e17_c0de);
        let runs = runs_of(&[
            ("VCFRONT_infoAppCrc", &[(a, -120, 0), (b, 9, 10), (a, 11, 12), (b, 13, 14), (c, 15, 16), (b, 18, 600)]),
            ("VCFRONT_infoBootCrc", &[(0x77, -120, 9), (0x78, 10, 11), (0x77, 13, 600)]),
            ("BMS_appCrc", &[(0x11, -120, 0), (0x22, 20, 600)]),
            ("DIR_applicationCrc", &[(0x99, -120, 600)]),
        ]);
        let ev = events_of(7, &runs, &BTreeSet::new());
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(
            ev[0].changes,
            vec![
                Change { field: "BMS_appCrc".into(), old: 0x11, new: 0x22 },
                Change { field: "VCFRONT_infoAppCrc".into(), old: a, new: b },
            ],
            "the boot CRC went back: no change"
        );
        // from the last time the old set was seen to the first time the settled one was
        assert_eq!((ev[0].from_ms, ev[0].to_ms), (T, T + 20 * MIN));
        // its set is the settled one: every field's value after it
        let settled: BTreeMap<&str, u64> = [
            ("BMS_appCrc", 0x22),
            ("DIR_applicationCrc", 0x99),
            ("VCFRONT_infoAppCrc", b),
            ("VCFRONT_infoBootCrc", 0x77),
        ]
        .into_iter()
        .collect();
        assert_eq!(ev[0].set, set_hash(&settled));
        // one step at a time (as updates were found before): six of them
        assert_eq!(events_settled(7, &runs, &BTreeSet::new(), 0).len(), 5);
    }

    #[test]
    fn a_value_that_flips_back_is_no_update_and_a_later_install_is_its_own() {
        let (a, b) = (0x06ac_6a37, 0x01b6_621d);
        // a flap alone: away and back within minutes
        let runs = runs_of(&[("VCFRONT_infoAppCrc", &[(a, -60, 0), (b, 3, 4), (a, 6, 300)])]);
        assert!(events_of(7, &runs, &BTreeSet::new()).is_empty());
        // a real update a week later, flapping too: two installs, one update
        let week = 7 * 24 * 60;
        let runs = runs_of(&[(
            "VCFRONT_infoAppCrc",
            &[
                (a, -60, 0),
                (b, 3, 4),
                (a, 6, 300),
                (b, week, week + 2),
                (a, week + 4, week + 5),
                (b, week + 8, 2 * week),
            ],
        )]);
        let ev = events_of(7, &runs, &BTreeSet::new());
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!((ev[0].changes[0].old, ev[0].changes[0].new), (a, b));
        assert_eq!(ev[0].from_ms, T + 300 * MIN);
        assert_eq!(ev[0].to_ms, T.checked_add_signed((week + 8) * 60_000).unwrap());
        // changes a set has not settled from for half an hour are a new install
        let runs = runs_of(&[("BMS_appCrc", &[(1, -60, 0), (2, 5, 40), (3, 75, 200)])]);
        assert_eq!(events_of(7, &runs, &BTreeSet::new()).len(), 2);
        // nor does an install go on for ever: one change after another, 20 minutes apart, for two hours
        let steps: Vec<(u64, i64, i64)> = (0..7).map(|i| (i + 1, i as i64 * 20, i as i64 * 20 + 1)).collect();
        let runs = runs_of(&[("BMS_appCrc", &steps)]);
        let ev = events_of(7, &runs, &BTreeSet::new());
        assert_eq!(ev.len(), 2, "{ev:?}");
        assert_eq!(ev.iter().map(|e| e.changes.len()).sum::<usize>(), 2);
    }

    #[test]
    fn a_name_typed_on_one_of_an_installs_steps_stays_with_the_install() {
        let (a, b, c) = (0x06ac_6a37, 0x01b6_621d, 0x5e17_c0de);
        let runs = runs_of(&[
            ("VCFRONT_infoAppCrc", &[(a, -120, 0), (b, 9, 10), (a, 11, 12), (c, 15, 16), (b, 18, 600)]),
            ("BMS_appCrc", &[(0x11, -120, 0), (0x22, 20, 600)]),
        ]);
        let steps = events_settled(7, &runs, &BTreeSet::new(), 0);
        assert_eq!(steps.len(), 4, "{steps:?}");
        // the owner cleared the first step's name and typed the version on the third (made-up versions)
        let mut n = Names::default();
        n.set(&steps[0], None);
        n.set(&steps[2], Some("2026.44.1"));
        let one = events_of(7, &runs, &BTreeSet::new());
        assert_eq!(one.len(), 1);
        assert!(n.follow(&steps, &one));
        assert_eq!(n.lookup(&one[0]), Some(("2026.44.1".to_string(), "owner")));
        assert!(!n.cleared(&one[0]));
        // its settled set is named: a later update to the same set gets the name by itself
        assert_eq!(n.sets.get(&one[0].set), Some(&("2026.44.1".to_string(), one[0].id)));
        // the steps' names that did not move are kept, for when they come back; nothing more moves
        assert!(!n.follow(&one, &one));
    }

    #[test]
    fn an_update_inside_one_log_keeps_its_direction() {
        // a one-log wake-up in which the value goes from 0x20 to 0x10 (seen first: 0x20)
        let d = day(&[("a", T, vec![tally("BMS_appCrc", 0x20, 3), tally("BMS_appCrc", 0x10, 3)], true)]);
        let s = summarize(&d);
        let values: Vec<u64> = s.runs["BMS_appCrc"].iter().map(|r| r.value).collect();
        assert_eq!(values, vec![0x20, 0x10]);
        // the next day still says 0x10: one update, 0x20 → 0x10
        let mut runs = BTreeMap::new();
        let mut all = s.runs["BMS_appCrc"].clone();
        all.push(Run { value: 0x10, first_ms: T + 86_400_000, last_ms: T + 86_455_000 });
        runs.insert("BMS_appCrc".to_string(), join(all));
        let ev = events_of(7, &runs, &BTreeSet::new());
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0].changes, vec![Change { field: "BMS_appCrc".into(), old: 0x20, new: 0x10 }]);
    }

    #[test]
    fn each_values_own_times_place_an_update_inside_a_log() {
        let seen = |field: &str, value, first_ms, last_ms| timeline::firmware::Seen {
            field: field.into(),
            value,
            count: 5,
            first_ms,
            last_ms,
        };
        let f = LogFirmware {
            seen: vec![
                seen("BMS_appCrc", 0x20, T, T + 20_000),
                seen("BMS_bootCrc", 9, T + 1_000, T + 54_000),
                seen("BMS_appCrc", 0x10, T + 25_000, T + 55_000),
            ],
            hints: 0,
        };
        let (set, log) = log_ref(&f, T, true);
        assert_eq!((log.from, log.to), (T, T + 55_000));
        // only the field with two values keeps each value's times
        assert_eq!(log.at, vec![(0, T, T + 20_000), (2, T + 25_000, T + 55_000)]);
        let mut d = DayLogs { sets: vec![set], logs: BTreeMap::new() };
        d.logs.insert("a".into(), log);
        let s = summarize(&d);
        assert_eq!(
            s.runs["BMS_appCrc"],
            vec![
                Run { value: 0x20, first_ms: T, last_ms: T + 20_000 },
                Run { value: 0x10, first_ms: T + 25_000, last_ms: T + 55_000 }
            ]
        );
        assert_eq!(s.runs["BMS_bootCrc"], vec![Run { value: 9, first_ms: T, last_ms: T + 55_000 }]);
        let ev = events_of(7, &s.runs, &BTreeSet::new());
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!((ev[0].from_ms, ev[0].to_ms), (T + 20_000, T + 25_000));
        assert_eq!((ev[0].changes[0].old, ev[0].changes[0].new), (0x20, 0x10));
    }

    #[test]
    fn a_name_follows_its_update_when_another_change_joins_it() {
        let r = |v, a: u64, b: u64| Run { value: v, first_ms: a * U, last_ms: b * U };
        let mut runs = BTreeMap::new();
        runs.insert("BMS_appCrc".to_string(), vec![r(1, 0, 100), r(2, 200, 300)]);
        let one = events_of(7, &runs, &BTreeSet::new());
        let mut n = Names::default();
        n.set(&one[0], Some("2025.38.6"));
        // a late log confirms one more changed field whose window meets the update's: a new id and set
        runs.insert("DIR_applicationCrc".to_string(), vec![r(5, 0, 100), r(6, 200, 300)]);
        let two = events_of(7, &runs, &BTreeSet::new());
        assert_eq!(two.len(), 1);
        assert!(two[0].id != one[0].id && two[0].set != one[0].set);
        assert!(n.follow(&one, &two));
        assert_eq!(n.lookup(&two[0]), Some(("2025.38.6".to_string(), "owner")));
        // its new set is named: a later update to the same set gets the name by itself
        assert_eq!(n.sets.get(&two[0].set), Some(&("2025.38.6".to_string(), two[0].id)));
        assert!(!n.sets.contains_key(&one[0].set));
        // nothing changed: nothing moves
        assert!(!n.follow(&two, &two));
    }

    #[test]
    fn a_name_stays_with_its_update_when_an_earlier_one_with_the_same_changes_goes() {
        let r = |v, a: u64, b: u64| Run { value: v, first_ms: a * U, last_ms: b * U };
        let mut runs = BTreeMap::new();
        runs.insert("BMS_appCrc".to_string(), vec![r(1, 0, 100), r(2, 200, 300), r(1, 400, 500), r(2, 600, 700)]);
        let all = events_of(7, &runs, &BTreeSet::new());
        assert_eq!(all.len(), 3);
        let mut n = Names::default();
        n.set(&all[0], Some("2026.2.6"));
        n.set(&all[2], Some("2026.8.1"));
        // the first update's logs go (a window cleared): the third's changes (1 → 2) now take the first's id
        runs.insert("BMS_appCrc".to_string(), vec![r(2, 200, 300), r(1, 400, 500), r(2, 600, 700)]);
        let left = events_of(7, &runs, &BTreeSet::new());
        assert_eq!(left.len(), 2);
        assert_eq!(left[1].id, all[0].id, "the same changes, no salt now");
        assert!(n.follow(&all, &left));
        assert_eq!(n.lookup(&left[1]), Some(("2026.8.1".to_string(), "owner")));
        assert_eq!(n.lookup(&left[0]), None);
    }

    #[test]
    fn a_window_over_days_without_readings_is_a_gap_and_its_row_does_not_stretch() {
        const DAY: u64 = 86_400_000;
        let r = |v, a, b| Run { value: v, first_ms: a, last_ms: b };
        // old set last read on day 0 at 22:53, new set first read on day 15 at 00:19; nothing read in between
        let (x, y) = (T + 22 * 3_600_000 + 53 * 60_000, T + 15 * DAY + 19 * 60_000);
        let mut runs = BTreeMap::new();
        runs.insert("VCFRONT_infoAppCrc".to_string(), vec![r(0x1111_0001, T - 5 * DAY, x), r(0x1111_0002, y, y + DAY)]);
        runs.insert("BMS_appCrc".to_string(), vec![r(0x2222_0001, T - 5 * DAY, x), r(0x2222_0002, y, y + DAY)]);
        runs.insert("BMS_bootCrc".to_string(), vec![r(0x3333_0001, T - 5 * DAY, y + DAY)]);
        let d0 = records::day_of(T);
        let mut days: BTreeSet<u32> = (d0 - 5..=d0).chain(d0 + 15..=d0 + 16).collect();
        let ev = events_of(7, &runs, &days);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!((ev[0].from_ms, ev[0].to_ms, ev[0].gap), (x, y, true));
        assert_eq!(ev[0].changes.len(), 2);
        // its row: where the new set was first seen, not fifteen days of installing
        assert_eq!(ev[0].row_span(), (y, y));
        // readings on every day between: the window is what it is, not a gap
        days.extend(d0 + 1..d0 + 15);
        let ev = events_of(7, &runs, &days);
        assert_eq!((ev[0].gap, ev[0].row_span()), (false, (x, y)));
        // overnight (the next day) is never a gap, even with nothing read in between
        assert!(!gap_between(&BTreeSet::new(), x, T + DAY + 8 * 3_600_000));
        // one whole day without readings is
        assert!(gap_between(&BTreeSet::new(), x, T + 2 * DAY + 60_000));
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
