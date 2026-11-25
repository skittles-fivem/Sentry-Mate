//! The purposes of drives (the owner's own, kept apart from the drives a rebuild makes again), the purpose rules that
//! fill them in, and `/cars/{car}/logbook` (rows, totals per purpose, and a plain PDF of them).
//!
//! Kept in the db's sealed key/value store, under the namespace `drives`:
//!
//! ```text
//! drives  car/<id>/purposes/<YYYY-MM>   the owner's purposes of the drives that start that month (UTC), by drive id
//! drives  car/<id>/purpose-rules        the car's purpose rules
//! ```
//!
//! A drive keeps its id when it is made again (the car and its start second), so its purpose stays with it. An
//! owner's purpose wins over a rule; of the rules, the first one that matches (the oldest) gives the purpose.

use super::drives::DriveView;
use super::{List, Paging, Period, QResult, QueryError, Scoped, car_of, paginate, parse_place, r, scoped};
use crate::Engine;
use crate::records;
use chrono::{Datelike, TimeZone};
use model::{Car, CarId, Drive};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

pub(crate) const NS: &str = "drives";

/// The purposes a drive can have.
pub const PURPOSES: [&str; 3] = ["business", "private", "commute"];
/// Rules a car keeps at most.
pub const MAX_RULES: usize = 100;
/// Logbook rows in one answer at most (a longer period is cut, and says so in `truncated`).
pub const MAX_LOGBOOK_ROWS: usize = 20_000;
pub const MAX_RELATION: usize = 100;
pub const MAX_NOTE: usize = 500;

/// One change to these records at a time (a change reads a record, changes it and writes it back).
static WRITES: Mutex<()> = Mutex::new(());

fn writing() -> std::sync::MutexGuard<'static, ()> {
    WRITES.lock().unwrap_or_else(|e| e.into_inner())
}

/// The purpose an owner gave a drive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OwnPurpose {
    pub purpose: String,
    pub relation: Option<String>,
    pub note: Option<String>,
    pub at_ms: u64,
}

/// The owner's purposes of `drives` (id, start) on `car`, to go with the drives to another car.
pub(crate) fn read_purposes(
    db: &db::Db,
    car: CarId,
    drives: &[(u64, u64)],
) -> crate::Result<Vec<(u64, u64, OwnPurpose)>> {
    let mut by_key: BTreeMap<String, Vec<(u64, u64)>> = BTreeMap::new();
    for (id, start) in drives {
        by_key.entry(purposes_key(car, *start)).or_default().push((*id, *start));
    }
    let mut out = Vec::new();
    for (key, ids) in by_key {
        let all: BTreeMap<u64, OwnPurpose> = records::get(db, NS, &key)?.unwrap_or_default();
        for (id, start) in ids {
            if let Some(p) = all.get(&id) {
                out.push((id, start, p.clone()));
            }
        }
    }
    Ok(out)
}

/// Takes the owner's purposes of `drives` (id, start) off `car` (once they are on the car the drives went to).
pub(crate) fn take_purposes(db: &db::Db, car: CarId, drives: &[(u64, u64)]) -> crate::Result<()> {
    let _w = writing();
    let mut by_key: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for (id, start) in drives {
        by_key.entry(purposes_key(car, *start)).or_default().push(*id);
    }
    for (key, ids) in by_key {
        let mut all: BTreeMap<u64, OwnPurpose> = records::get(db, NS, &key)?.unwrap_or_default();
        let before = all.len();
        for id in ids {
            all.remove(&id);
        }
        if all.len() != before {
            records::put(db, NS, &key, &all)?;
        }
    }
    Ok(())
}

/// Puts purposes on `car`'s drives (id, start, purpose), as [`take_purposes`] took them.
pub(crate) fn put_purposes(db: &db::Db, car: CarId, items: &[(u64, u64, OwnPurpose)]) -> crate::Result<()> {
    let _w = writing();
    let mut by_key: BTreeMap<String, Vec<(u64, OwnPurpose)>> = BTreeMap::new();
    for (id, start, p) in items {
        by_key.entry(purposes_key(car, *start)).or_default().push((*id, p.clone()));
    }
    for (key, v) in by_key {
        let mut all: BTreeMap<u64, OwnPurpose> = records::get(db, NS, &key)?.unwrap_or_default();
        all.extend(v);
        records::put(db, NS, &key, &all)?;
    }
    Ok(())
}

/// What [`relink_purposes`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Relinked {
    /// Purposes put on the drive they belong to now.
    pub relinked: u64,
    /// Purposes kept aside (`car/<id>/purposes-unmatched/<YYYY-MM>`) because no drive is theirs, now.
    pub unmatched: u64,
}

/// The drive a purpose given to drive `id` belongs to now, when no drive has that id (the table was written again and
/// the drive made again from the logs under the id its start gives): a drive of this car made from logs that
/// overlaps the second the id names, as a start moved by another input keeps its id ([`crate::pipeline::carry_ids`]).
/// An imported row's id is the import's and never moves: it is the row, or none.
fn home_of(db: &db::Db, car: CarId, id: u64) -> crate::Result<Option<Drive>> {
    if crate::importer::is_imported(id) || CarId::try_from(id >> 32).ok() != Some(car) {
        return Ok(None);
    }
    let from = (id & 0xFFFF_FFFF) * 1000;
    let mut q = db::Query::between(from, from + 1000).oldest_first().limit(10);
    q.source = Some(model::Source::Live);
    Ok(db.list::<Drive>(car, &q)?.items.into_iter().next())
}

/// How many purposes are kept aside for want of a drive (`car/<id>/purposes-unmatched`).
pub(crate) fn unmatched_count(db: &db::Db, car: CarId) -> crate::Result<u64> {
    let aside = format!("car/{car}/purposes-unmatched/");
    let mut n = 0;
    for key in db.kv().keys(NS)?.iter().filter(|k| k.starts_with(&aside)) {
        n += records::get::<BTreeMap<u64, OwnPurpose>>(db, NS, key)?.map_or(0, |m| m.len() as u64);
    }
    Ok(n)
}

/// Puts the purposes whose drive is gone on the drive they belong to now ([`home_of`]); those with none are kept
/// under `car/<id>/purposes-unmatched/<YYYY-MM>` (the month they were in), never dropped, and put back on a drive
/// that has their id again, or one that is theirs, whenever this runs. Purposes on a drive that is there stay.
pub(crate) fn relink_purposes(db: &db::Db, car: CarId) -> crate::Result<Relinked> {
    let _w = writing();
    let (live, aside) = (format!("car/{car}/purposes/"), format!("car/{car}/purposes-unmatched/"));
    let mut out = Relinked::default();
    let keys = db.kv().keys(NS)?;
    // (the month, in which of the two) → what is there
    let mut kept: BTreeMap<(String, bool), BTreeMap<u64, OwnPurpose>> = BTreeMap::new();
    let mut changed = std::collections::BTreeSet::new();
    let mut moves: Vec<(u64, u64, OwnPurpose, bool)> = Vec::new();
    for key in &keys {
        let (month, is_aside) = match (key.strip_prefix(&live), key.strip_prefix(&aside)) {
            (Some(m), _) => (m.to_string(), false),
            (_, Some(m)) => (m.to_string(), true),
            _ => continue,
        };
        let all: BTreeMap<u64, OwnPurpose> = records::get(db, NS, key)?.unwrap_or_default();
        let mut stay = BTreeMap::new();
        for (id, p) in all {
            if db.get::<Drive>(car, id)?.is_some() {
                if is_aside {
                    // its drive is there again: back with it
                    let start = db.get::<Drive>(car, id)?.map_or(0, |d| d.start_ms);
                    moves.push((id, start, p, false));
                    changed.insert((month.clone(), true));
                } else {
                    stay.insert(id, p);
                }
            } else if let Some(d) = home_of(db, car, id)? {
                moves.push((d.id, d.start_ms, p, !is_aside));
                changed.insert((month.clone(), is_aside));
                out.relinked += u64::from(!is_aside);
            } else {
                if !is_aside {
                    changed.insert((month.clone(), false));
                    changed.insert((month.clone(), true));
                }
                kept.entry((month.clone(), true)).or_default().insert(id, p);
            }
        }
        if !is_aside {
            kept.entry((month, false)).or_default().extend(stay);
        }
    }
    // a drive takes one purpose: a second one meant for it is kept aside
    let mut put: Vec<(u64, u64, OwnPurpose)> = Vec::new();
    for (id, start, p, counted) in moves {
        let taken = kept.values().any(|m| m.contains_key(&id)) || put.iter().any(|x| x.0 == id);
        if taken {
            kept.entry((month_of(start), true)).or_default().insert(id, p);
            changed.insert((month_of(start), true));
            out.relinked = out.relinked.saturating_sub(u64::from(counted));
        } else {
            put.push((id, start, p));
        }
    }
    for (month, is_aside) in &changed {
        let key = format!("car/{car}/{}/{month}", if *is_aside { "purposes-unmatched" } else { "purposes" });
        match kept.get(&(month.clone(), *is_aside)).filter(|m| !m.is_empty()) {
            Some(m) => records::put(db, NS, &key, m)?,
            None => {
                db.kv().delete(NS, &key)?;
            }
        }
    }
    for (id, start, p) in &put {
        let key = purposes_key(car, *start);
        let mut all: BTreeMap<u64, OwnPurpose> = records::get(db, NS, &key)?.unwrap_or_default();
        all.insert(*id, p.clone());
        records::put(db, NS, &key, &all)?;
    }
    for key in db.kv().keys(NS)?.iter().filter(|k| k.starts_with(&aside)) {
        out.unmatched += records::get::<BTreeMap<u64, OwnPurpose>>(db, NS, key)?.map_or(0, |m| m.len() as u64);
    }
    Ok(out)
}

/// A purpose rule: drives from `from_place` to `to_place` (null: no place) on `days` get `purpose`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub from_place: Option<String>,
    pub to_place: Option<String>,
    /// `all`, or days and ranges of days: `mon-fri`, `sat,sun`.
    pub days: String,
    pub purpose: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Rules {
    next: u64,
    items: Vec<Rule>,
}

fn month_of(ms: u64) -> String {
    let t = chrono::Utc.timestamp_millis_opt(i64::try_from(ms).unwrap_or(0)).single().unwrap_or_default();
    format!("{:04}-{:02}", t.year(), t.month())
}

fn purposes_key(car: CarId, start_ms: u64) -> String {
    format!("car/{car}/purposes/{}", month_of(start_ms))
}

fn rules_key(car: CarId) -> String {
    format!("car/{car}/purpose-rules")
}

fn rules_of(e: &Engine, car: CarId) -> QResult<Rules> {
    Ok(records::get(e.db(), NS, &rules_key(car))?.unwrap_or_default())
}

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// Whether `spec` is a days rule: `all`, or a comma list of days and ranges (`mon-fri`, `sat,sun`, `fri-mon`).
pub fn valid_days(spec: &str) -> bool {
    let spec = spec.trim();
    if spec == "all" {
        return true;
    }
    !spec.is_empty()
        && spec.split(',').all(|part| {
            let ends: Vec<&str> = part.trim().split('-').collect();
            (1..=2).contains(&ends.len()) && ends.iter().all(|d| DAYS.contains(d))
        })
}

/// Whether the weekday (0 Monday .. 6 Sunday) is one of `spec`'s days (a range may wrap past Sunday).
fn day_matches(spec: &str, weekday: usize) -> bool {
    let spec = spec.trim();
    if spec.is_empty() || spec == "all" {
        return true;
    }
    spec.split(',').any(|part| {
        let ends: Vec<usize> =
            part.trim().split('-').filter_map(|d| DAYS.iter().position(|x| *x == d)).collect::<Vec<_>>();
        match ends[..] {
            [d] => d == weekday,
            [a, b] if a <= b => (a..=b).contains(&weekday),
            [a, b] => weekday >= a || weekday <= b,
            _ => false,
        }
    })
}

/// The purposes of a car's drives, read a month at a time as rows ask for them.
pub(crate) struct Purposes {
    car: CarId,
    tz: chrono_tz::Tz,
    rules: Vec<Rule>,
    months: HashMap<String, BTreeMap<u64, OwnPurpose>>,
}

impl Purposes {
    pub(crate) fn new(e: &Engine, c: &Car) -> QResult<Purposes> {
        Ok(Purposes {
            car: c.id,
            tz: crate::tz::zone(&c.time_zone),
            rules: rules_of(e, c.id)?.items,
            months: HashMap::new(),
        })
    }

    fn own(&mut self, e: &Engine, d: &Drive) -> QResult<Option<OwnPurpose>> {
        let key = purposes_key(self.car, d.start_ms);
        if !self.months.contains_key(&key) {
            let m: BTreeMap<u64, OwnPurpose> = records::get(e.db(), NS, &key)?.unwrap_or_default();
            // a long period touches many months: keep only a year of them
            if self.months.len() > 12 {
                self.months.clear();
            }
            self.months.insert(key.clone(), m);
        }
        Ok(self.months.get(&key).and_then(|m| m.get(&d.id)).cloned())
    }

    /// The drive's purpose as rows carry it: `{ value, relation, note, source: "owner" | "rule", rule? }` or null.
    pub(crate) fn of(&mut self, e: &Engine, d: &Drive, v: &DriveView) -> QResult<Option<serde_json::Value>> {
        if let Some(p) = self.own(e, d)? {
            return Ok(Some(serde_json::json!({
                "value": p.purpose, "relation": p.relation, "note": p.note, "source": "owner",
            })));
        }
        if self.rules.is_empty() {
            return Ok(None);
        }
        let from = v.start.place.as_ref().map(|p| p.id.as_str());
        // a drive still going, or whose data stopped, has no destination yet
        let to = v.end.place.as_ref().filter(|_| !v.flags.in_progress && v.end_ms.is_some()).map(|p| p.id.as_str());
        let weekday = crate::tz::date(d.start_ms, self.tz).weekday().num_days_from_monday() as usize;
        Ok(self
            .rules
            .iter()
            .find(|x| x.from_place.as_deref() == from && x.to_place.as_deref() == to && day_matches(&x.days, weekday))
            .map(|x| {
                serde_json::json!({
                    "value": x.purpose, "relation": null, "note": null, "source": "rule", "rule": x.id,
                })
            }))
    }
}

/// The drive, or `no_such_drive`.
fn drive_of(e: &Engine, car: CarId, id: u64) -> QResult<Drive> {
    e.db().get::<Drive>(car, id)?.ok_or(QueryError::NotFound("no_such_drive"))
}

/// `PUT /cars/{car}/drives/{drive}/purpose`: the owner's purpose (`business`, `private` or `commute`), with a
/// relation (at most 100 characters) and a note (at most 500); a rule no longer gives this drive its purpose.
pub fn set_purpose(
    e: &Engine,
    car: CarId,
    id: u64,
    purpose: &str,
    relation: Option<&str>,
    note: Option<&str>,
    now_ms: u64,
) -> QResult<()> {
    car_of(e, car)?;
    let d = drive_of(e, car, id)?;
    let mut fields = BTreeMap::new();
    if !PURPOSES.contains(&purpose) {
        fields.insert("purpose".to_string(), "Must be business, private or commute.".to_string());
    }
    if relation.is_some_and(|s| s.chars().count() > MAX_RELATION) {
        fields.insert("relation".to_string(), format!("At most {MAX_RELATION} characters."));
    }
    if note.is_some_and(|s| s.chars().count() > MAX_NOTE) {
        fields.insert("note".to_string(), format!("At most {MAX_NOTE} characters."));
    }
    if !fields.is_empty() {
        return Err(QueryError::Invalid { fields });
    }
    let clean = |s: Option<&str>| s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let own = OwnPurpose { purpose: purpose.to_string(), relation: clean(relation), note: clean(note), at_ms: now_ms };
    let _one = writing();
    let key = purposes_key(car, d.start_ms);
    let mut all: BTreeMap<u64, OwnPurpose> = records::get(e.db(), NS, &key)?.unwrap_or_default();
    all.insert(d.id, own);
    records::put(e.db(), NS, &key, &all)?;
    changed();
    Ok(())
}

/// `DELETE /cars/{car}/drives/{drive}/purpose`: the owner's purpose taken off (a rule may give one again). Whether
/// there was one.
pub fn clear_purpose(e: &Engine, car: CarId, id: u64) -> QResult<bool> {
    car_of(e, car)?;
    let d = drive_of(e, car, id)?;
    let _one = writing();
    let key = purposes_key(car, d.start_ms);
    let mut all: BTreeMap<u64, OwnPurpose> = records::get(e.db(), NS, &key)?.unwrap_or_default();
    let had = all.remove(&d.id).is_some();
    if all.is_empty() {
        e.db().kv().delete(NS, &key)?;
    } else {
        records::put(e.db(), NS, &key, &all)?;
    }
    changed();
    Ok(had)
}

/// `GET /cars/{car}/purpose-rules`, oldest first.
pub fn rules(e: &Engine, car: CarId, paging: &Paging) -> QResult<Scoped<List<Rule>>> {
    let c = car_of(e, car)?;
    let items = rules_of(e, car)?.items;
    let sig = format!("purpose-rules|{car}");
    let key = |x: &Rule| (x.id.trim_start_matches('r').parse::<f64>().unwrap_or(0.0), x.id.clone());
    Ok(scoped(&c, paginate(items, key, false, &sig, paging, false)?))
}

/// `GET /cars/{car}/purpose-rules/{rule}`.
pub fn rule(e: &Engine, car: CarId, id: &str) -> QResult<Scoped<Rule>> {
    let c = car_of(e, car)?;
    let r = rules_of(e, car)?.items.into_iter().find(|x| x.id == id).ok_or(QueryError::NotFound("no_such_rule"))?;
    Ok(scoped(&c, r))
}

/// `POST /cars/{car}/purpose-rules`: a new rule (places must exist, `days` as [`valid_days`]); `422` on a field
/// that breaks a rule.
pub fn add_rule(
    e: &Engine,
    car: CarId,
    from_place: Option<&str>,
    to_place: Option<&str>,
    days: &str,
    purpose: &str,
) -> QResult<Scoped<Rule>> {
    let c = car_of(e, car)?;
    let places = e.places();
    let mut fields = BTreeMap::new();
    let mut place = |name: &str, p: Option<&str>| -> Option<String> {
        let p = p?;
        match parse_place(p).and_then(|id| places.get(id)) {
            Some(pl) => Some(super::place_ref(pl.id)),
            None => {
                fields.insert(name.to_string(), "No such place.".to_string());
                None
            }
        }
    };
    let (from, to) = (place("from_place", from_place), place("to_place", to_place));
    if !PURPOSES.contains(&purpose) {
        fields.insert("purpose".to_string(), "Must be business, private or commute.".to_string());
    }
    if !valid_days(days) {
        fields.insert("days".to_string(), "Days such as mon-fri, sat,sun or all.".to_string());
    }
    let _one = writing();
    let mut all = rules_of(e, car)?;
    if all.items.len() >= MAX_RULES {
        fields.insert("purpose".to_string(), format!("A car keeps at most {MAX_RULES} rules."));
    }
    if !fields.is_empty() {
        return Err(QueryError::Invalid { fields });
    }
    all.next = all.next.max(all.items.len() as u64) + 1;
    let rule = Rule {
        id: format!("r{}", all.next),
        from_place: from,
        to_place: to,
        days: days.trim().to_string(),
        purpose: purpose.to_string(),
    };
    all.items.push(rule.clone());
    records::put(e.db(), NS, &rules_key(car), &all)?;
    changed();
    Ok(scoped(&c, rule))
}

/// `DELETE /cars/{car}/purpose-rules/{rule}`: the rule removed (the drives it gave a purpose have none again).
pub fn remove_rule(e: &Engine, car: CarId, id: &str) -> QResult<Rule> {
    car_of(e, car)?;
    let _one = writing();
    let mut all = rules_of(e, car)?;
    let i = all.items.iter().position(|x| x.id == id).ok_or(QueryError::NotFound("no_such_rule"))?;
    let gone = all.items.remove(i);
    records::put(e.db(), NS, &rules_key(car), &all)?;
    changed();
    Ok(gone)
}

/// One row of the logbook.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogRow {
    pub seq: u64,
    pub drive: String,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub start_odometer_km: Option<f64>,
    pub end_odometer_km: Option<f64>,
    pub distance_km: Option<f64>,
    pub start_address: Option<String>,
    pub end_address: Option<String>,
    pub start_place: Option<String>,
    pub end_place: Option<String>,
    pub start_place_id: Option<String>,
    pub end_place_id: Option<String>,
    pub duration_ms: u64,
    pub purpose: Option<String>,
    pub relation: Option<String>,
    pub note: Option<String>,
    pub purpose_source: Option<String>,
    pub purpose_rule: Option<String>,
    pub in_progress: bool,
    /// The odometer distance between the end of the row before and the start of this one; null on the first row.
    pub continuity_gap_km: Option<f64>,
    /// When the row before ended (null on the first row, or while it goes on): where a missing stretch starts.
    pub prev_end_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PurposeTotal {
    pub drives: u64,
    pub distance_km: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Logbook {
    pub from_ms: u64,
    pub to_ms: u64,
    pub rows: Vec<LogRow>,
    /// `business`, `private`, `commute` and `none` (drives without a purpose).
    pub totals_by_purpose: BTreeMap<String, PurposeTotal>,
    /// Whether the period held more than [`MAX_LOGBOOK_ROWS`] drives (the rows stop there; the totals count all).
    pub truncated: bool,
}

/// Missing distance below this is rounding (the odometer and the logged start agree within 50 m).
pub const GAP_KM: f64 = 0.05;

/// The continuity check of the whole period (every row, whatever the filter).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Continuity {
    /// Rows (drives) in the period.
    pub rows: u64,
    pub first_seq: Option<u64>,
    pub first_start_odometer_km: Option<f64>,
    pub last_seq: Option<u64>,
    pub last_end_odometer_km: Option<f64>,
    /// The sum of the drives' distances.
    pub logged_km: f64,
    /// The sum of the gaps over [`GAP_KM`].
    pub missing_km: f64,
    /// How many gaps over [`GAP_KM`].
    pub gaps: u64,
    /// The largest gaps (at most 4), largest first.
    pub worst: Vec<LogRow>,
}

/// The period's logbook worked out once: kept (in [`Engine::memo`]) while the car's rows, places, settings, the
/// shown sides of overlaps, the purposes and the rules stay the same, so paging through it, filtering it or exporting
/// it reads no drive again and looks up no address again.
#[derive(Debug, Clone, Default)]
struct Book {
    rows: Vec<LogRow>,
    totals: BTreeMap<String, PurposeTotal>,
    truncated: bool,
    continuity: Continuity,
    /// Rows per purpose (`none` for none).
    counts: BTreeMap<String, u64>,
    /// Rows each rule gave its purpose.
    rules_used: BTreeMap<String, u64>,
}

/// Bumped by every change to purposes or rules (kept in the key/value store, not in the car's version).
static CHANGES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn changed() {
    CHANGES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn text_of(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(serde_json::Value::as_str).map(str::to_string)
}

/// A period that ends now (a page's "this year" sends its own now as `to_ms`, a new one each load) is kept as one that
/// goes on: no drive starts after now, so the rows are the same and one kept answer serves every load.
const NOW_SLACK_MS: u64 = 5 * 60_000;

fn book(e: &Engine, c: &Car, a: u64, b: u64) -> QResult<std::sync::Arc<Book>> {
    let b = if b >= crate::now_ms().saturating_sub(NOW_SLACK_MS) { u64::MAX } else { b };
    let x = super::drives::ctx(e, c)?;
    let shown = super::system::Shown::new(e, c.id, None)?;
    let changes = CHANGES.load(std::sync::atomic::Ordering::Relaxed);
    let extra = crate::memo::key(&(a, b, x.live, x.stopped, shown.key(), changes, &c.time_zone));
    e.memo(c.id, "logbook", crate::memo::On::All, extra, || make_book(e, c, a, b, &x, &shown))
}

fn make_book(
    e: &Engine,
    c: &Car,
    a: u64,
    b: u64,
    x: &super::drives::Ctx,
    shown: &super::system::Shown,
) -> QResult<Book> {
    let mut purposes = Purposes::new(e, c)?;
    let mut totals: BTreeMap<String, PurposeTotal> =
        PURPOSES.iter().chain(&["none"]).map(|k| (k.to_string(), PurposeTotal::default())).collect();
    let mut rows = Vec::new();
    // the rows as the drives list shows them: with what the chosen import fills ([`super::gapfill`])
    let gaps = super::gapfill::Gaps::new(e, c.id, None)?;
    // the odometer the last row with one ended at, and the km of the rows since that have none
    let (mut seq, mut prev_end, mut prev_end_ms, mut truncated) = (0u64, None::<(f64, f64)>, None, false);
    super::drives::each::<Drive>(e, c.id, a, b, None, shown, false, |d| {
        let (v, filled) = super::drives::view_filled(c, &d, x, &gaps)?;
        let p = purposes.of(e, &filled, &v)?.unwrap_or(serde_json::Value::Null);
        let value = text_of(&p, "value");
        let t = totals.entry(value.clone().unwrap_or_else(|| "none".into())).or_default();
        t.drives += 1;
        t.distance_km += v.distance_km.unwrap_or(0.0);
        if rows.len() >= MAX_LOGBOOK_ROWS {
            truncated = true;
            return Ok(true);
        }
        seq += 1;
        // (the km driven in rows without an odometer since are not missing)
        let gap = prev_end.zip(v.start_odometer_km).map(|((end, between), s)| (s - end - between).max(0.0));
        prev_end = match (v.end_odometer_km, prev_end) {
            (Some(end), _) => Some((end, 0.0)),
            // a row with no odometer and no distance: nothing says what lies between
            (None, Some((end, between))) => v.distance_km.map(|km| (end, between + km)),
            (None, None) => None,
        };
        let before = prev_end_ms;
        prev_end_ms = v.end_ms;
        rows.push(LogRow {
            seq,
            drive: v.id.clone(),
            start_ms: v.start_ms,
            end_ms: v.end_ms,
            start_odometer_km: v.start_odometer_km,
            end_odometer_km: v.end_odometer_km,
            distance_km: v.distance_km,
            start_address: v.start.address.short.clone(),
            end_address: v.end.address.short.clone(),
            start_place: v.start.place.as_ref().map(|p| p.name.clone()),
            end_place: v.end.place.as_ref().map(|p| p.name.clone()),
            start_place_id: v.start.place.as_ref().map(|p| p.id.clone()),
            end_place_id: v.end.place.as_ref().map(|p| p.id.clone()),
            duration_ms: v.duration_ms,
            purpose: value,
            relation: text_of(&p, "relation"),
            note: text_of(&p, "note"),
            purpose_source: text_of(&p, "source"),
            purpose_rule: text_of(&p, "rule"),
            in_progress: v.flags.in_progress,
            continuity_gap_km: r(gap, 3),
            prev_end_ms: if seq == 1 { None } else { before },
        });
        Ok(true)
    })?;
    for t in totals.values_mut() {
        t.distance_km = r(Some(t.distance_km), 3).unwrap_or(0.0);
    }
    let mut counts: BTreeMap<String, u64> = PURPOSES.iter().chain(&["none"]).map(|k| (k.to_string(), 0)).collect();
    let mut rules_used = BTreeMap::new();
    for x in &rows {
        *counts.entry(x.purpose.clone().unwrap_or_else(|| "none".into())).or_default() += 1;
        if let Some(rule) = &x.purpose_rule {
            *rules_used.entry(rule.clone()).or_default() += 1;
        }
    }
    let continuity = continuity_of(&rows);
    Ok(Book { rows, totals, truncated, continuity, counts, rules_used })
}

fn continuity_of(rows: &[LogRow]) -> Continuity {
    let gaps: Vec<&LogRow> = rows.iter().filter(|x| x.continuity_gap_km.is_some_and(|g| g > GAP_KM)).collect();
    let mut worst = gaps.clone();
    worst.sort_by(|a, b| b.continuity_gap_km.partial_cmp(&a.continuity_gap_km).unwrap_or(std::cmp::Ordering::Equal));
    worst.truncate(4);
    let (first, last) = (rows.first(), rows.last());
    Continuity {
        rows: rows.len() as u64,
        first_seq: first.map(|x| x.seq),
        first_start_odometer_km: first.and_then(|x| x.start_odometer_km),
        last_seq: last.map(|x| x.seq),
        last_end_odometer_km: last.and_then(|x| x.end_odometer_km),
        // (`+ 0.0` turns an empty sum's -0 into 0)
        logged_km: r(Some(rows.iter().map(|x| x.distance_km.unwrap_or(0.0)).sum::<f64>() + 0.0), 3).unwrap_or(0.0),
        missing_km: r(Some(gaps.iter().map(|x| x.continuity_gap_km.unwrap_or(0.0)).sum::<f64>() + 0.0), 3)
            .unwrap_or(0.0),
        gaps: gaps.len() as u64,
        worst: worst.into_iter().cloned().collect(),
    }
}

/// The period as it is: up to now at most; from its first drive when no start was given.
fn edges(period: &Period, a: u64, b: u64, rows: &[LogRow]) -> (u64, u64) {
    let to_ms = b.min(crate::now_ms()).max(a);
    let from_ms = match period.from_ms {
        Some(_) => a,
        None => rows.first().map_or(a, |x| x.start_ms.min(to_ms)),
    };
    (from_ms, to_ms)
}

/// `/cars/{car}/logbook`: the period's drives oldest first (by start), as shown.
pub fn logbook(e: &Engine, car: CarId, period: &Period) -> QResult<Scoped<Logbook>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let bk = book(e, &c, a, b)?;
    let (from_ms, to_ms) = edges(period, a, b, &bk.rows);
    Ok(scoped(
        &c,
        Logbook {
            from_ms,
            to_ms,
            rows: bk.rows.clone(),
            totals_by_purpose: bk.totals.clone(),
            truncated: bk.truncated,
        },
    ))
}

/// The order of a logbook page: `seq` (by start, the default), `distance`, `gap` (missing distance before),
/// `odometer` (at the start), `from`, `to` (the place, else the address) or `purpose`. Ties by `seq`; a missing value
/// goes last either way.
pub const LOGBOOK_SORTS: [&str; 7] = ["seq", "distance", "gap", "odometer", "from", "to", "purpose"];

/// What a logbook page shows: its filters, order and where it starts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogQuery {
    /// `business`, `commute`, `private` or `none`.
    pub purpose: Option<String>,
    /// Words that every shown row has in its places, addresses, relation or note (any case).
    pub q: Option<String>,
    /// Only rows after distance not logged (a gap over [`GAP_KM`]).
    pub gaps: bool,
    /// One of [`LOGBOOK_SORTS`] (`seq` when none).
    pub sort: Option<String>,
    pub desc: bool,
    /// The first row (0 based, in the shown order).
    pub offset: usize,
    /// With `seq` only: start at the first row (in the shown order) on the far side of this time: starting at or after
    /// it oldest first, before it newest first (the last row when none is).
    pub at_ms: Option<u64>,
    pub limit: usize,
}

/// A page of the logbook, with what the whole period says.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogPage {
    pub from_ms: u64,
    pub to_ms: u64,
    pub items: Vec<LogRow>,
    /// Where `items` start among the shown rows (0 based).
    pub offset: usize,
    /// The rows the filters show.
    pub total: usize,
    /// Every row of the period.
    pub count: usize,
    /// Rows per purpose (`none`: without one), whatever the filters.
    pub counts_by_purpose: BTreeMap<String, u64>,
    pub totals_by_purpose: BTreeMap<String, PurposeTotal>,
    pub continuity: Continuity,
    /// Rows each rule gives its purpose, by rule id.
    pub rules_used: BTreeMap<String, u64>,
    pub truncated: bool,
}

fn checked(query: &LogQuery) -> QResult<()> {
    if let Some(p) = &query.purpose
        && !PURPOSES.contains(&p.as_str())
        && p != "none"
    {
        return Err(QueryError::bad("purpose", "business, commute, private or none"));
    }
    if let Some(s) = &query.sort
        && !LOGBOOK_SORTS.contains(&s.as_str())
    {
        return Err(QueryError::bad("sort", "seq, distance, gap, odometer, from, to or purpose"));
    }
    if query.at_ms.is_some() && query.sort.as_deref().is_some_and(|s| s != "seq") {
        return Err(QueryError::bad("at_ms", "only in the order of the drives (sort=seq)"));
    }
    Ok(())
}

/// The rows a query shows, in its order.
fn shown_rows<'a>(rows: &'a [LogRow], query: &LogQuery) -> Vec<&'a LogRow> {
    let words: Vec<String> =
        query.q.as_deref().unwrap_or("").split_whitespace().map(str::to_lowercase).collect::<Vec<_>>();
    let has = |x: &LogRow| {
        let fields = [&x.start_place, &x.end_place, &x.start_address, &x.end_address, &x.relation, &x.note];
        let text: String = fields.iter().filter_map(|f| f.as_deref()).collect::<Vec<_>>().join("\u{1f}").to_lowercase();
        words.iter().all(|w| text.contains(w.as_str()))
    };
    let mut out: Vec<&LogRow> = rows
        .iter()
        .filter(|x| query.purpose.as_deref().is_none_or(|p| x.purpose.as_deref().unwrap_or("none") == p))
        .filter(|x| words.is_empty() || has(x))
        .filter(|x| !query.gaps || x.continuity_gap_km.is_some_and(|g| g > GAP_KM))
        .collect();
    let text = |place: &Option<String>, address: &Option<String>| {
        place.as_deref().or(address.as_deref()).map(str::to_lowercase)
    };
    use std::cmp::Ordering;
    let desc = query.desc;
    // a missing value last either way; ties by seq in the chosen direction
    fn by<T: PartialOrd>(a: Option<T>, b: Option<T>, desc: bool) -> Ordering {
        match (a, b) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => {
                let o = a.partial_cmp(&b).unwrap_or(Ordering::Equal);
                if desc { o.reverse() } else { o }
            }
        }
    }
    let seq = |a: &LogRow, b: &LogRow| if desc { b.seq.cmp(&a.seq) } else { a.seq.cmp(&b.seq) };
    match query.sort.as_deref().unwrap_or("seq") {
        "distance" => out.sort_by(|a, b| by(a.distance_km, b.distance_km, desc).then_with(|| seq(a, b))),
        "gap" => out.sort_by(|a, b| by(a.continuity_gap_km, b.continuity_gap_km, desc).then_with(|| seq(a, b))),
        "odometer" => {
            out.sort_by(|a, b| by(a.start_odometer_km, b.start_odometer_km, desc).then_with(|| seq(a, b)));
        }
        "from" => out.sort_by(|a, b| {
            by(text(&a.start_place, &a.start_address), text(&b.start_place, &b.start_address), desc)
                .then_with(|| seq(a, b))
        }),
        "to" => out.sort_by(|a, b| {
            by(text(&a.end_place, &a.end_address), text(&b.end_place, &b.end_address), desc).then_with(|| seq(a, b))
        }),
        "purpose" => out.sort_by(|a, b| by(a.purpose.as_deref(), b.purpose.as_deref(), desc).then_with(|| seq(a, b))),
        _ => out.sort_by(|a, b| seq(a, b)),
    }
    out
}

/// `/cars/{car}/logbook` a page at a time (`limit`, `offset` or the web side's cursor, `at_ms`), with the purpose
/// filter, search words and an order, and the totals, counts and continuity check of the whole period.
pub fn logbook_page(e: &Engine, car: CarId, period: &Period, query: &LogQuery) -> QResult<Scoped<LogPage>> {
    let c = car_of(e, car)?;
    checked(query)?;
    let (a, b) = period.bounds()?;
    let bk = book(e, &c, a, b)?;
    let shown = shown_rows(&bk.rows, query);
    let total = shown.len();
    let offset = match query.at_ms {
        Some(at) if query.desc => shown.iter().position(|x| x.start_ms < at),
        Some(at) => shown.iter().position(|x| x.start_ms >= at),
        None => Some(query.offset),
    }
    .unwrap_or(total.saturating_sub(1))
    .min(total);
    let items = shown.iter().skip(offset).take(query.limit.max(1)).map(|x| (*x).clone()).collect();
    let (from_ms, to_ms) = edges(period, a, b, &bk.rows);
    Ok(scoped(
        &c,
        LogPage {
            from_ms,
            to_ms,
            items,
            offset,
            total,
            count: bk.rows.len(),
            counts_by_purpose: bk.counts.clone(),
            totals_by_purpose: bk.totals.clone(),
            continuity: bk.continuity.clone(),
            rules_used: bk.rules_used.clone(),
            truncated: bk.truncated,
        },
    ))
}

/// Every row a query shows, in its order (the CSV of a filtered table).
pub fn logbook_rows(e: &Engine, car: CarId, period: &Period, query: &LogQuery) -> QResult<Vec<LogRow>> {
    let c = car_of(e, car)?;
    checked(query)?;
    let (a, b) = period.bounds()?;
    let bk = book(e, &c, a, b)?;
    Ok(shown_rows(&bk.rows, query).into_iter().cloned().collect())
}

/// The logbook as a PDF (a plain table, landscape, times in the car's zone, the continuity warnings and the totals per
/// purpose), and its file name (`logbook-<car>-<from>-<to>.pdf`).
pub fn logbook_pdf(e: &Engine, car: CarId, period: &Period) -> QResult<(Vec<u8>, String)> {
    let c = car_of(e, car)?;
    let book = logbook(e, car, period)?;
    let tz = crate::tz::zone(&c.time_zone);
    let local = |ms: u64, f: &str| {
        chrono::Utc
            .timestamp_millis_opt(i64::try_from(ms).unwrap_or(0))
            .single()
            .unwrap_or_default()
            .with_timezone(&tz)
            .format(f)
            .to_string()
    };
    let km = |v: Option<f64>, w: usize| v.map_or_else(|| format!("{:>w$}", "-"), |x| format!("{x:>w$.1}"));
    let cut = |s: &str, n: usize| -> String {
        let t: String = s.chars().take(n).collect();
        format!("{t:<n$}")
    };
    let mut lines = vec![
        format!(
            "{:>5} {:16} {:5} {:>12} {:>12} {:>8}  {:30} {:30} {:9} Note",
            "#", "Start", "End", "Odo start km", "Odo end km", "km", "From", "To", "Purpose"
        ),
        String::new(),
    ];
    for x in &book.body.rows {
        if let Some(g) = x.continuity_gap_km.filter(|g| *g > GAP_KM) {
            lines.push(format!("      ! {g:.1} km not logged before this drive"));
        }
        let from = x.start_place.as_deref().or(x.start_address.as_deref()).unwrap_or("-");
        let to = x.end_place.as_deref().or(x.end_address.as_deref()).unwrap_or("-");
        lines.push(format!(
            "{:>5} {:16} {:5} {} {} {}  {} {} {:9} {}",
            x.seq,
            local(x.start_ms, "%Y-%m-%d %H:%M"),
            x.end_ms.map_or_else(|| "--:--".to_string(), |t| local(t, "%H:%M")),
            km(x.start_odometer_km, 12),
            km(x.end_odometer_km, 12),
            km(x.distance_km, 8),
            cut(from, 30),
            cut(to, 30),
            x.purpose.as_deref().unwrap_or("-"),
            cut(x.note.as_deref().unwrap_or(""), 40).trim_end(),
        ));
    }
    lines.push(String::new());
    for (k, t) in &book.body.totals_by_purpose {
        lines.push(format!("{k:>10}: {} drives, {:.1} km", t.drives, t.distance_km));
    }
    if book.body.truncated {
        lines.push(format!("Only the first {MAX_LOGBOOK_ROWS} drives are listed; the totals count every drive."));
    }
    let (a, b) = period.bounds()?;
    let until = b.min(crate::now_ms()).saturating_sub(1).max(a);
    // the period starts at its first drive when it was not given
    let start = a.max(book.body.rows.first().map_or(a, |x| x.start_ms).min(until));
    let title = format!(
        "Logbook: {} ({} to {}), times in {}",
        c.name,
        local(start, "%Y-%m-%d"),
        local(until, "%Y-%m-%d"),
        c.time_zone
    );
    let name = format!("logbook-c{car}-{}-{}.pdf", local(start, "%Y%m%d"), local(until, "%Y%m%d"));
    Ok((text_pdf(&title, &lines), name))
}

/// A plain text PDF: A4 landscape, Courier, a title on every page and page numbers. Text outside Latin-1 shows as `?`.
pub fn text_pdf(title: &str, lines: &[String]) -> Vec<u8> {
    const PER_PAGE: usize = 50;
    let esc = |s: &str| -> String {
        let mut o = String::new();
        for ch in s.chars() {
            match ch {
                '(' | ')' | '\\' => {
                    o.push('\\');
                    o.push(ch);
                }
                ' '..='~' => o.push(ch),
                // Latin-1 as WinAnsi octal escapes; everything else is not in the font's encoding
                '\u{a0}'..='\u{ff}' => o.push_str(&format!("\\{:03o}", ch as u32)),
                _ => o.push('?'),
            }
        }
        o
    };
    let pages: Vec<&[String]> = if lines.is_empty() { vec![&[][..]] } else { lines.chunks(PER_PAGE).collect() };
    let n = pages.len();
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {n} >>",
            (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect::<Vec<_>>().join(" ")
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>".to_string(),
    ];
    for (i, page) in pages.iter().enumerate() {
        let mut s = format!("BT /F1 11 Tf 28 560 Td ({}) Tj ET\n", esc(title));
        s.push_str(&format!("BT /F1 8 Tf 760 20 Td (Page {} of {n}) Tj ET\n", i + 1));
        s.push_str("BT /F1 8 Tf 10 TL 28 536 Td\n");
        for l in page.iter() {
            s.push_str(&format!("({}) Tj T*\n", esc(l)));
        }
        s.push_str("ET\n");
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 842 595] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            5 + 2 * i
        ));
        objects.push(format!("<< /Length {} >>\nstream\n{s}endstream", s.len()));
    }
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn days_rules() {
        for ok in ["all", "mon-fri", "sat,sun", "fri-mon", "wed"] {
            assert!(valid_days(ok), "{ok}");
        }
        for bad in ["", "monday", "mon-tue-wed", "mon,,tue", "1-5"] {
            assert!(!valid_days(bad), "{bad}");
        }
        assert!(day_matches("mon-fri", 0) && day_matches("mon-fri", 4) && !day_matches("mon-fri", 5));
        assert!(day_matches("fri-mon", 6) && day_matches("fri-mon", 0) && !day_matches("fri-mon", 2));
        assert!(day_matches("sat,sun", 5) && !day_matches("sat,sun", 1));
    }

    fn row(seq: u64, start_ms: u64, km: f64, gap: Option<f64>, purpose: Option<&str>, from: &str) -> LogRow {
        LogRow {
            seq,
            drive: format!("d{seq}"),
            start_ms,
            end_ms: Some(start_ms + 600_000),
            start_odometer_km: Some(1000.0 + seq as f64 * 10.0),
            end_odometer_km: Some(1000.0 + seq as f64 * 10.0 + km),
            distance_km: Some(km),
            start_address: Some(format!("{from} Street 1")),
            end_address: None,
            start_place: None,
            end_place: Some("Made-up Office".into()),
            start_place_id: None,
            end_place_id: None,
            duration_ms: 600_000,
            purpose: purpose.map(str::to_string),
            relation: None,
            note: (seq == 3).then(|| "Quarterly visit".to_string()),
            purpose_source: purpose.map(|_| "owner".to_string()),
            purpose_rule: None,
            in_progress: false,
            continuity_gap_km: gap,
            prev_end_ms: (seq > 1).then_some(start_ms - 1),
        }
    }

    fn sample() -> Vec<LogRow> {
        vec![
            row(1, 1_000, 5.0, None, Some("business"), "Elm"),
            row(2, 2_000, 12.0, Some(0.0), None, "Oak"),
            row(3, 3_000, 2.5, Some(4.2), Some("commute"), "Ash"),
            row(4, 4_000, 30.0, Some(0.04), Some("business"), "Birch"),
            row(5, 5_000, 1.0, Some(9.0), None, "Cedar"),
        ]
    }

    fn seqs(rows: &[&LogRow]) -> Vec<u64> {
        rows.iter().map(|x| x.seq).collect()
    }

    #[test]
    fn filters_words_and_orders() {
        let rows = sample();
        let q = |purpose: Option<&str>, words: Option<&str>, sort: Option<&str>, desc: bool| LogQuery {
            purpose: purpose.map(str::to_string),
            q: words.map(str::to_string),
            sort: sort.map(str::to_string),
            desc,
            ..LogQuery::default()
        };
        assert_eq!(seqs(&shown_rows(&rows, &q(None, None, None, false))), vec![1, 2, 3, 4, 5]);
        assert_eq!(seqs(&shown_rows(&rows, &q(None, None, None, true))), vec![5, 4, 3, 2, 1]);
        assert_eq!(seqs(&shown_rows(&rows, &q(Some("business"), None, None, false))), vec![1, 4]);
        assert_eq!(seqs(&shown_rows(&rows, &q(Some("none"), None, None, false))), vec![2, 5]);
        // every word, any case, in places, addresses or the note
        assert_eq!(seqs(&shown_rows(&rows, &q(None, Some("QUARTERLY office"), None, false))), vec![3]);
        assert_eq!(seqs(&shown_rows(&rows, &q(None, Some("street"), None, false))).len(), 5);
        assert!(shown_rows(&rows, &q(None, Some("nowhere"), None, false)).is_empty());
        assert_eq!(seqs(&shown_rows(&rows, &q(None, None, Some("distance"), true))), vec![4, 2, 1, 3, 5]);
        let gaps = LogQuery { gaps: true, ..LogQuery::default() };
        assert_eq!(seqs(&shown_rows(&rows, &gaps)), vec![3, 5]);
        // no gap on the first row: last either way
        assert_eq!(seqs(&shown_rows(&rows, &q(None, None, Some("gap"), true))), vec![5, 3, 4, 2, 1]);
        assert_eq!(seqs(&shown_rows(&rows, &q(None, None, Some("gap"), false))), vec![2, 4, 3, 5, 1]);
        assert_eq!(seqs(&shown_rows(&rows, &q(None, None, Some("from"), false))), vec![3, 4, 5, 1, 2]);
        assert_eq!(seqs(&shown_rows(&rows, &q(None, None, Some("purpose"), false))), vec![1, 4, 3, 2, 5]);
        assert!(checked(&q(Some("holiday"), None, None, false)).is_err());
        assert!(checked(&q(None, None, Some("colour"), false)).is_err());
        let at = LogQuery { at_ms: Some(1), sort: Some("distance".into()), ..LogQuery::default() };
        assert!(checked(&at).is_err());
    }

    #[test]
    fn the_continuity_check_of_the_whole_period() {
        let c = continuity_of(&sample());
        assert_eq!((c.rows, c.first_seq, c.last_seq, c.gaps), (5, Some(1), Some(5), 2));
        assert_eq!((c.logged_km, c.missing_km), (50.5, 13.2));
        assert_eq!(c.first_start_odometer_km, Some(1010.0));
        assert_eq!(c.last_end_odometer_km, Some(1051.0));
        assert_eq!(c.worst.iter().map(|x| x.seq).collect::<Vec<_>>(), vec![5, 3]);
        assert_eq!(continuity_of(&[]), Continuity::default());
    }

    #[test]
    fn a_pdf_with_offsets_that_hold() {
        let lines: Vec<String> = (0..120).map(|i| format!("row {i} (km) \\ é ✓")).collect();
        let pdf = text_pdf("Logbook: Test", &lines);
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.contains("/Count 3"));
        assert!(text.contains("row 7 \\(km\\) \\\\ \\351 ?"));
        let at: usize = text.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
        assert!(pdf[at..].starts_with(b"xref"));
    }
}
