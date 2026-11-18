//! What the engine keeps for itself, in the db's sealed key/value store: which logs it has decoded (so a restart carries
//! on and nothing is decoded twice), each log's facts for the timeline, the idle cut points, windows being rebuilt,
//! rebuild jobs, CAN keys waiting for a car, and the costs charges keep.
//!
//! Per car, one record per UTC day the car's logs start on (a log is found from its name alone: the name carries its
//! UTC start), so the records a window needs are a few small files however long the history:
//!
//! ```text
//! engine  car/<id>              CarMeta: the days that have records, first and newest data
//! engine  car/<id>/day/<day>    DayRec: the logs that start that day (facts, or why their data is not used), cut points
//! engine  car/<id>/dirty        windows written to but not yet rebuilt (a crash between the two)
//! engine  car/<id>/rebuild      a rebuild of the car under way
//! engine  car/<id>/costs/<mon>  the costs of the charges that start that month (UTC), kept across rebuilds
//! engine  keys/waiting          CAN keys no car claims yet
//! engine  tokens/cars           upload tokens made on a car's page, and that car
//! engine  car/<id>/now_at       when each of the car's newest values was recorded
//! ```

use crate::Result;
use db::Db;
use model::CarId;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use timeline::LogFacts;

pub(crate) const NS: &str = "engine";

/// The version of how logs are decoded here (with the timeline's rules): entries made by another are made again.
/// 2: a left-out log's hole is held to a few minutes around its name's start (its sidecar's span is not signed).
pub const DECODE_VERSION: u32 = 2;

/// The version stamp an entry carries: this crate's decoding and the timeline's rules.
pub(crate) fn rules() -> u32 {
    DECODE_VERSION * 1000 + timeline::RULES_VERSION
}

pub(crate) const DAY_MS: u64 = 86_400_000;

pub(crate) fn day_of(ms: u64) -> u32 {
    u32::try_from(ms / DAY_MS).unwrap_or(u32::MAX)
}

/// The UTC start of a log, from its name.
pub(crate) fn name_start_ms(name: &str) -> Option<u64> {
    let n = trust::LogName::parse(name)?;
    u64::try_from(n.started_ms).ok()
}

/// What became of one log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// Decoded: its facts, and the span its samples were written to.
    Decoded { facts: LogFacts, samples_from_ms: Option<u64>, samples_to_ms: Option<u64>, samples: u32 },
    /// Not used (its check failed, or it could not be read): a hole in the car's history, unknown, never asleep.
    Hole { from_ms: u64, to_ms: u64, reason: String },
}

/// One log's entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    /// The CAN key the log came from.
    pub key: String,
    pub outcome: Outcome,
    /// The DBC set it was decoded with (`tesla-3y`, `tesla-sx-legacy`).
    pub profile: String,
    /// [`DECODE_VERSION`] and the timeline's rules it was made with.
    pub rules: u32,
    /// Whether its check allowed its data when it was worked on.
    pub usable: bool,
    /// What its check said (`verified`, `unsigned`, `failed`, ...), and why.
    pub status: String,
    pub reason: Option<String>,
    /// When it was worked on.
    pub done_ms: u64,
}

impl LogEntry {
    pub fn span(&self) -> (u64, u64) {
        match &self.outcome {
            Outcome::Decoded { facts, .. } => facts.span(),
            Outcome::Hole { from_ms, to_ms, .. } => (*from_ms, *to_ms),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DayRec {
    pub logs: BTreeMap<String, LogEntry>,
    /// The timeline's idle cut points that fall on this day (ms, sorted).
    #[serde(default)]
    pub cuts: Vec<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CarMeta {
    /// Days (since 1970, UTC) that have a [`DayRec`].
    pub days: BTreeSet<u32>,
    /// The earliest and latest data of any log worked on.
    pub first_ms: Option<u64>,
    pub newest_ms: Option<u64>,
    /// Logs decoded and logs left out, in all.
    pub decoded: u64,
    pub holes: u64,
    /// When a log was last worked on, and how long it took (ms).
    pub last_done_ms: Option<u64>,
    pub last_took_ms: Option<u64>,
}

/// A rebuild of a car under way (kept so a restart carries on with it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RebuildMark {
    pub from_ms: u64,
    pub to_ms: u64,
    pub started_ms: u64,
    /// Whether the window was cleared (all of it): then carrying on only works on the logs not done again yet.
    #[serde(default)]
    pub cleared: bool,
}

/// A CAN key no car claims yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitingKey {
    pub first_seen_ms: u64,
    pub last_upload_ms: u64,
    pub logs_waiting: u64,
    /// An admin took it from its car: it waits for an admin, whatever [`crate::NewKeys`] says.
    #[serde(default)]
    pub held: bool,
}

/// What a charge cost, kept with the charge across rebuilds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostEntry {
    pub amount: f64,
    pub currency: String,
    /// `tariff` (the place's price when the charge ended), `manual` (typed in), `free_supercharging`, `teslamate`.
    pub source: String,
    /// For a manual cost: `total`, `per_kwh` or `per_minute`, the amount as entered, and the energy it is over.
    pub mode: Option<String>,
    pub entered: Option<f64>,
    pub energy_basis: Option<String>,
    /// The place it was priced at, and the parts per rate (tariff costs).
    pub place: Option<u32>,
    #[serde(default)]
    pub parts: Vec<CostPart>,
    pub session_fee: Option<f64>,
    pub gap_ms: u64,
    pub at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostPart {
    pub rate: Option<String>,
    pub per_kwh: f64,
    pub kwh: f64,
    pub cost: f64,
}

pub(crate) fn get<T: DeserializeOwned>(db: &Db, ns: &str, key: &str) -> Result<Option<T>> {
    match db.kv().get(ns, key)? {
        Some(b) => Ok(Some(serde_json::from_slice(&b).map_err(|e| crate::Error::Damaged(format!("{key}: {e}")))?)),
        None => Ok(None),
    }
}

pub(crate) fn put<T: Serialize>(db: &Db, ns: &str, key: &str, v: &T) -> Result<()> {
    let b = serde_json::to_vec(v).map_err(|e| crate::Error::Damaged(format!("{key}: {e}")))?;
    Ok(db.kv().put(ns, key, &b)?)
}

fn car_key(car: CarId) -> String {
    format!("car/{car}")
}

pub(crate) fn meta(db: &Db, car: CarId) -> Result<CarMeta> {
    Ok(get(db, NS, &car_key(car))?.unwrap_or_default())
}

pub(crate) fn put_meta(db: &Db, car: CarId, m: &CarMeta) -> Result<()> {
    put(db, NS, &car_key(car), m)
}

pub(crate) fn day(db: &Db, car: CarId, d: u32) -> Result<DayRec> {
    Ok(get(db, NS, &format!("car/{car}/day/{d}"))?.unwrap_or_default())
}

pub(crate) fn put_day(db: &Db, car: CarId, d: u32, rec: &DayRec) -> Result<()> {
    let key = format!("car/{car}/day/{d}");
    if rec.logs.is_empty() && rec.cuts.is_empty() {
        db.kv().delete(NS, &key)?;
        return Ok(());
    }
    put(db, NS, &key, rec)
}

/// The entry of one log, if it was worked on.
pub(crate) fn entry(db: &Db, car: CarId, name: &str) -> Result<Option<LogEntry>> {
    let Some(start) = name_start_ms(name) else { return Ok(None) };
    Ok(day(db, car, day_of(start))?.logs.remove(name))
}

pub(crate) fn dirty(db: &Db, car: CarId) -> Result<Vec<(u64, u64)>> {
    Ok(get(db, NS, &format!("car/{car}/dirty"))?.unwrap_or_default())
}

pub(crate) fn put_dirty(db: &Db, car: CarId, spans: &[(u64, u64)]) -> Result<()> {
    let key = format!("car/{car}/dirty");
    if spans.is_empty() {
        db.kv().delete(NS, &key)?;
        return Ok(());
    }
    put(db, NS, &key, &spans)
}

pub(crate) fn rebuild_mark(db: &Db, car: CarId) -> Result<Option<RebuildMark>> {
    get(db, NS, &format!("car/{car}/rebuild"))
}

pub(crate) fn put_rebuild_mark(db: &Db, car: CarId, m: Option<&RebuildMark>) -> Result<()> {
    let key = format!("car/{car}/rebuild");
    match m {
        Some(m) => put(db, NS, &key, m),
        None => {
            db.kv().delete(NS, &key)?;
            Ok(())
        }
    }
}

pub(crate) fn waiting_keys(db: &Db) -> Result<BTreeMap<String, WaitingKey>> {
    Ok(get(db, NS, "keys/waiting")?.unwrap_or_default())
}

/// Upload tokens made on a car's page (token id → car): their key goes to that car.
pub(crate) fn token_cars(db: &Db) -> Result<BTreeMap<String, CarId>> {
    Ok(get(db, NS, "tokens/cars")?.unwrap_or_default())
}

pub(crate) fn put_token_cars(db: &Db, m: &BTreeMap<String, CarId>) -> Result<()> {
    put(db, NS, "tokens/cars", m)
}

/// When each value of a car's Now was recorded (field → ms).
pub(crate) fn now_at(db: &Db, car: CarId) -> Result<BTreeMap<String, u64>> {
    Ok(get(db, NS, &format!("car/{car}/now_at"))?.unwrap_or_default())
}

pub(crate) fn put_now_at(db: &Db, car: CarId, m: &BTreeMap<String, u64>) -> Result<()> {
    put(db, NS, &format!("car/{car}/now_at"), m)
}

pub(crate) fn put_waiting_keys(db: &Db, keys: &BTreeMap<String, WaitingKey>) -> Result<()> {
    put(db, NS, "keys/waiting", keys)
}

fn month_of(ms: u64) -> String {
    let (y, m, _) = trust::name::civil_from_days(i64::try_from(ms / DAY_MS).unwrap_or(0));
    format!("{y:04}-{m:02}")
}

pub(crate) fn costs(db: &Db, car: CarId, start_ms: u64) -> Result<BTreeMap<u64, CostEntry>> {
    Ok(get(db, NS, &format!("car/{car}/costs/{}", month_of(start_ms)))?.unwrap_or_default())
}

pub(crate) fn cost(db: &Db, car: CarId, charge: u64, start_ms: u64) -> Result<Option<CostEntry>> {
    Ok(costs(db, car, start_ms)?.remove(&charge))
}

pub(crate) fn put_cost(db: &Db, car: CarId, charge: u64, start_ms: u64, c: Option<CostEntry>) -> Result<()> {
    let key = format!("car/{car}/costs/{}", month_of(start_ms));
    let mut all: BTreeMap<u64, CostEntry> = get(db, NS, &key)?.unwrap_or_default();
    match c {
        Some(c) => {
            all.insert(charge, c);
        }
        None => {
            all.remove(&charge);
        }
    }
    put(db, NS, &key, &all)
}

/// Every entry of a car whose span meets `[from_ms, to_ms)` (and, with `around`, the nearest decoded log before and
/// after it), in time order. Reads only the days that can hold them.
pub(crate) fn entries_between(
    db: &Db,
    car: CarId,
    meta: &CarMeta,
    from_ms: u64,
    to_ms: u64,
) -> Result<Vec<(String, LogEntry)>> {
    // a log starts at most a few minutes before its data ends; a day either side covers what spills over
    let lo = day_of(from_ms).saturating_sub(1);
    let hi = day_of(to_ms.saturating_sub(1).max(from_ms));
    let mut out = Vec::new();
    for &d in meta.days.range(lo..=hi) {
        for (name, e) in day(db, car, d)?.logs {
            let (a, b) = e.span();
            if b >= from_ms && a < to_ms {
                out.push((name, e));
            }
        }
    }
    out.sort_by_key(|(n, e)| (e.span().0, n.clone()));
    Ok(out)
}

/// The nearest decoded log that ends before `ms` (`before`) or starts at or after it, looking day by day.
pub(crate) fn nearest_decoded(db: &Db, car: CarId, meta: &CarMeta, ms: u64, before: bool) -> Result<Option<LogFacts>> {
    let d0 = day_of(ms);
    let days: Vec<u32> = if before {
        meta.days.range(..=d0.saturating_add(1)).rev().copied().collect()
    } else {
        meta.days.range(d0.saturating_sub(1)..).copied().collect()
    };
    for (i, d) in days.into_iter().enumerate() {
        let rec = day(db, car, d)?;
        let mut best: Option<LogFacts> = None;
        for e in rec.logs.values() {
            let Outcome::Decoded { facts, .. } = &e.outcome else { continue };
            let (a, b) = facts.span();
            let better = |cur: &LogFacts| if before { b > cur.span().1 } else { a < cur.span().0 };
            if (before && b < ms || !before && a >= ms) && best.as_ref().is_none_or(better) {
                best = Some(facts.clone());
            }
        }
        if best.is_some() {
            return Ok(best);
        }
        // the first two days looked at can hold the answer on either side of `ms`; past them, any log of the day is it
        if i > 64 {
            break;
        }
    }
    Ok(None)
}

/// The cut points of a car in `(from_ms, to_ms)` (both open), sorted.
pub(crate) fn cuts_between(db: &Db, car: CarId, meta: &CarMeta, from_ms: u64, to_ms: u64) -> Result<Vec<u64>> {
    let mut out = Vec::new();
    for &d in meta.days.range(day_of(from_ms)..=day_of(to_ms.saturating_sub(1).max(from_ms))) {
        out.extend(day(db, car, d)?.cuts.into_iter().filter(|&c| c > from_ms && c < to_ms));
    }
    out.sort_unstable();
    Ok(out)
}

/// Replaces the cut points in `(from_ms, to_ms)` with `cuts`.
pub(crate) fn replace_cuts(
    db: &Db,
    car: CarId,
    meta: &mut CarMeta,
    from_ms: u64,
    to_ms: u64,
    cuts: &[u64],
) -> Result<()> {
    let mut days: BTreeSet<u32> =
        meta.days.range(day_of(from_ms)..=day_of(to_ms.saturating_sub(1).max(from_ms))).copied().collect();
    days.extend(cuts.iter().map(|&c| day_of(c)));
    for d in days {
        let mut rec = day(db, car, d)?;
        let before = rec.cuts.clone();
        rec.cuts.retain(|&c| c <= from_ms || c >= to_ms);
        rec.cuts.extend(cuts.iter().copied().filter(|&c| day_of(c) == d && c > from_ms && c < to_ms));
        rec.cuts.sort_unstable();
        rec.cuts.dedup();
        if rec.cuts != before {
            put_day(db, car, d, &rec)?;
            if rec.logs.is_empty() && rec.cuts.is_empty() {
                meta.days.remove(&d);
            } else {
                meta.days.insert(d);
            }
        }
    }
    Ok(())
}
