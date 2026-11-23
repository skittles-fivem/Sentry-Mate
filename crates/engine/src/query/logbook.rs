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

fn text_of(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(serde_json::Value::as_str).map(str::to_string)
}

/// `/cars/{car}/logbook`: the period's drives oldest first (by start), as shown.
pub fn logbook(e: &Engine, car: CarId, period: &Period) -> QResult<Scoped<Logbook>> {
    let c = car_of(e, car)?;
    let x = super::drives::ctx(e, &c)?;
    let mut purposes = Purposes::new(e, &c)?;
    let (a, b) = period.bounds()?;
    let mut totals: BTreeMap<String, PurposeTotal> =
        PURPOSES.iter().chain(&["none"]).map(|k| (k.to_string(), PurposeTotal::default())).collect();
    let mut rows = Vec::new();
    let (mut seq, mut prev_end, mut truncated) = (0u64, None::<Option<f64>>, false);
    let shown = super::system::Shown::new(e, car, None)?;
    super::drives::each::<Drive>(e, car, a, b, None, &shown, false, |d| {
        let v = super::drives::view(&c, &d, &x);
        let p = purposes.of(e, &d, &v)?.unwrap_or(serde_json::Value::Null);
        let value = text_of(&p, "value");
        let t = totals.entry(value.clone().unwrap_or_else(|| "none".into())).or_default();
        t.drives += 1;
        t.distance_km += v.distance_km.unwrap_or(0.0);
        if rows.len() >= MAX_LOGBOOK_ROWS {
            truncated = true;
            return Ok(true);
        }
        seq += 1;
        let gap = match prev_end {
            Some(end) => end.zip(v.start_odometer_km).map(|(e, s)| (s - e).max(0.0)),
            None => None,
        };
        prev_end = Some(v.end_odometer_km);
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
        });
        Ok(true)
    })?;
    for t in totals.values_mut() {
        t.distance_km = r(Some(t.distance_km), 3).unwrap_or(0.0);
    }
    // the period as it is: up to now at most; from its first drive when no start was given
    let to_ms = b.min(crate::now_ms()).max(a);
    let from_ms = match period.from_ms {
        Some(_) => a,
        None => rows.first().map_or(a, |x| x.start_ms.min(to_ms)),
    };
    Ok(scoped(&c, Logbook { from_ms, to_ms, rows, totals_by_purpose: totals, truncated }))
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
        if let Some(g) = x.continuity_gap_km.filter(|g| *g > 0.05) {
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
