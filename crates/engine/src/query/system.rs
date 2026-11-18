//! Overlaps between imported and CAN history, a car's logs and their check results, `/cars/{car}/health`,
//! `/cars/{car}/storage`, and the engine's own figures for `/system/health`.

use super::{List, Paging, Period, QResult, QueryError, Scoped, car_of, paginate, scoped};
use crate::pipeline::all_rows;
use crate::records::{self, NS, Outcome};
use crate::{Engine, now_ms};
use db::StateRow;
use model::{CarId, Source, State};
use serde::{Deserialize, Serialize};

/// Which side of an overlap a span shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flip {
    pub start_ms: u64,
    pub end_ms: u64,
    /// `can` or `teslamate`.
    pub shown: String,
}

fn flips(e: &Engine, car: CarId) -> QResult<Vec<Flip>> {
    Ok(records::get(e.db(), NS, &format!("car/{car}/overlaps"))?.unwrap_or_default())
}

/// The spans the CAN history covers (online or asleep), joined.
fn can_coverage(e: &Engine, car: CarId, from: u64, to: u64) -> QResult<Vec<(u64, u64)>> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    for s in all_rows::<StateRow>(e.db(), car, from, to, Some(Source::Can))? {
        if s.span.state == State::Unknown {
            continue;
        }
        match out.last_mut() {
            Some(last) if s.span.start_ms <= last.1 + 60_000 => last.1 = last.1.max(s.span.end_ms),
            _ => out.push((s.span.start_ms, s.span.end_ms)),
        }
    }
    Ok(out)
}

/// Whether a row is shown: with `source=all` an imported row inside CAN history is hidden (CAN wins), unless that
/// overlap was flipped to show the imported side.
pub(crate) struct Shown {
    filter: Option<Source>,
    coverage: Vec<(u64, u64)>,
    flips: Vec<Flip>,
}

impl Shown {
    pub(crate) fn new(e: &Engine, car: CarId, filter: Option<Source>) -> QResult<Shown> {
        let imported = e.db().entry(car)?.is_some_and(|x| {
            let t = &x.summary.tally.teslamate;
            t.drives > 0 || t.charges > 0 || x.summary.tally.states > 0 && t.drives + t.charges > 0
        });
        if filter.is_some() || !imported {
            return Ok(Shown { filter, coverage: Vec::new(), flips: Vec::new() });
        }
        Ok(Shown { filter, coverage: can_coverage(e, car, 0, u64::MAX)?, flips: flips(e, car)? })
    }

    pub(crate) fn keep(&self, source: Source, t: u64) -> bool {
        if let Some(f) = self.filter {
            return source == f;
        }
        let flipped = self.flips.iter().find(|f| f.start_ms <= t && t < f.end_ms).map(|f| f.shown.as_str());
        match source {
            Source::Can => flipped != Some("teslamate"),
            Source::Teslamate => {
                let i = self.coverage.partition_point(|s| s.1 <= t);
                let covered = self.coverage.get(i).is_some_and(|s| s.0 <= t);
                !covered || flipped == Some("teslamate")
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Counts {
    pub drives: u64,
    pub charges: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Overlap {
    pub id: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub shown: String,
    pub can: Counts,
    pub teslamate: Counts,
}

/// `/cars/{car}/overlaps`: spans where imported and CAN history both exist.
pub fn overlaps(e: &Engine, car: CarId, paging: &Paging) -> QResult<Scoped<List<Overlap>>> {
    let c = car_of(e, car)?;
    let db = e.db();
    let tm_drives: Vec<model::Drive> = all_rows(db, car, 0, u64::MAX, Some(Source::Teslamate))?;
    let tm_charges: Vec<model::Charge> = all_rows(db, car, 0, u64::MAX, Some(Source::Teslamate))?;
    let can_drives: Vec<model::Drive> = all_rows(db, car, 0, u64::MAX, Some(Source::Can))?;
    let can_charges: Vec<model::Charge> = all_rows(db, car, 0, u64::MAX, Some(Source::Can))?;
    let fl = flips(e, car)?;
    let mut items = Vec::new();
    if !(tm_drives.is_empty() && tm_charges.is_empty()) {
        for (a, b) in can_coverage(e, car, 0, u64::MAX)? {
            let inside = |t: u64| a <= t && t < b;
            let tm = Counts {
                drives: tm_drives.iter().filter(|d| inside(d.start_ms)).count() as u64,
                charges: tm_charges.iter().filter(|d| inside(d.start_ms)).count() as u64,
            };
            if tm.drives + tm.charges == 0 {
                continue;
            }
            let can = Counts {
                drives: can_drives.iter().filter(|d| inside(d.start_ms)).count() as u64,
                charges: can_charges.iter().filter(|d| inside(d.start_ms)).count() as u64,
            };
            let shown = fl.iter().find(|f| f.start_ms == a).map_or_else(|| "can".to_string(), |f| f.shown.clone());
            items.push(Overlap { id: format!("overlap:{a}"), start_ms: a, end_ms: b, shown, can, teslamate: tm });
        }
    }
    let page = paginate(items, |o| (o.start_ms as f64, o.id.clone()), true, &format!("overlaps|{car}"), paging, false)?;
    Ok(scoped(&c, page))
}

/// Flips an overlap (`PATCH /cars/{car}/overlaps` with `{ id, shown }`).
pub fn set_overlap(e: &Engine, car: CarId, id: &str, shown: &str) -> QResult<Overlap> {
    if shown != "can" && shown != "teslamate" {
        return Err(QueryError::invalid("shown", "can or teslamate"));
    }
    let all = overlaps(e, car, &Paging::first(super::MAX_LIMIT))?.body.items;
    let mut o = all.into_iter().find(|o| o.id == id).ok_or(QueryError::NotFound("no_such_overlap"))?;
    let mut fl = flips(e, car)?;
    fl.retain(|f| f.start_ms != o.start_ms);
    if shown == "teslamate" {
        fl.push(Flip { start_ms: o.start_ms, end_ms: o.end_ms, shown: shown.into() });
    }
    records::put(e.db(), NS, &format!("car/{car}/overlaps"), &fl)?;
    o.shown = shown.to_string();
    e.emit(crate::Event::Rows {
        car,
        kinds: vec!["drives", "charges", "states", "visits"],
        from_ms: o.start_ms,
        to_ms: o.end_ms,
    });
    Ok(o)
}

/// One log of a car and what became of it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogRow {
    pub name: String,
    pub key: String,
    pub start_ms: u64,
    /// `verified`, `unsigned`, `failed`, `could_not_sign`, `waiting`.
    pub result: String,
    pub reason: Option<String>,
    pub used: bool,
    pub override_use: bool,
    pub arrived_ms: u64,
    /// `decoded`, `left_out` or `not_yet` (not worked on yet).
    pub decoded: &'static str,
    pub samples: Option<u32>,
    pub dropped: Option<u32>,
    pub buses: Vec<String>,
    pub data_from_ms: Option<u64>,
    pub data_to_ms: Option<u64>,
    pub decoded_ms: Option<u64>,
}

/// A car's logs (every CAN key of it), newest first, with their check results and what was decoded.
pub fn logs(e: &Engine, car: CarId, period: &Period, paging: &Paging) -> QResult<Scoped<List<LogRow>>> {
    let c = car_of(e, car)?;
    let rt = e.handle()?;
    let (a, b) = period.bounds()?;
    let mut items = Vec::new();
    for key in &c.vehicle_keys {
        let book = rt.block_on(e.judge().book(key));
        let mut day_cache: Option<(u32, records::DayRec)> = None;
        for (name, rec) in book.logs {
            let Some(start) = records::name_start_ms(&name) else { continue };
            if start < a || start >= b {
                continue;
            }
            let d = records::day_of(start);
            if day_cache.as_ref().is_none_or(|(x, _)| *x != d) {
                day_cache = Some((d, records::day(e.db(), car, d)?));
            }
            let entry = day_cache.as_ref().and_then(|(_, r)| r.logs.get(&name));
            let (decoded, samples, dropped, buses, from, to) = match entry.map(|x| &x.outcome) {
                Some(Outcome::Decoded { facts, samples, samples_from_ms, samples_to_ms }) => (
                    "decoded",
                    Some(*samples),
                    Some(facts.dropped),
                    facts.buses.clone(),
                    *samples_from_ms,
                    *samples_to_ms,
                ),
                Some(Outcome::Hole { from_ms, to_ms, .. }) => {
                    ("left_out", None, None, Vec::new(), Some(*from_ms), Some(*to_ms))
                }
                None => ("not_yet", None, None, Vec::new(), None, None),
            };
            let result = match rec.status {
                ingest::judge::Status::Verified => "verified",
                ingest::judge::Status::Failed => "failed",
                ingest::judge::Status::CouldNotSign => "could_not_sign",
                ingest::judge::Status::Unsigned => "unsigned",
                ingest::judge::Status::Waiting => "waiting",
            };
            items.push(LogRow {
                used: rec.usable(),
                override_use: rec.override_use,
                arrived_ms: rec.arrived_ms,
                result: result.into(),
                reason: rec.reason.clone(),
                name: name.clone(),
                key: key.clone(),
                start_ms: start,
                decoded,
                samples,
                dropped,
                buses,
                data_from_ms: from,
                data_to_ms: to,
                decoded_ms: entry.map(|x| x.done_ms),
            });
        }
    }
    let sig = format!("logs|{car}|{}", period.sig());
    let page = paginate(items, |l| (l.start_ms as f64, l.name.clone()), true, &sig, paging, false)?;
    Ok(scoped(&c, page))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LastTrust {
    pub result: String,
    pub reason: Option<String>,
    pub log_start_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KeyHealth {
    pub key: String,
    pub last_upload_ms: Option<u64>,
    pub preferred: bool,
    pub device: Option<String>,
    pub logs_waiting: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Span {
    pub start_ms: u64,
    pub end_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnknownSpan {
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub reason: &'static str,
}

/// The check result of the car's newest judged log (logs still waiting for their sidecar are not judged yet).
fn last_judged(e: &Engine, car: CarId, meta: &records::CarMeta) -> QResult<Option<LastTrust>> {
    let db = e.db();
    for &d in meta.days.iter().rev() {
        let rec = records::day(db, car, d)?;
        let newest = rec
            .logs
            .iter()
            .filter(|(_, en)| en.status != "waiting")
            .filter_map(|(n, en)| records::name_start_ms(n).map(|t| (t, n, en)))
            .max_by(|x, y| (x.0, x.1).cmp(&(y.0, y.1)));
        if let Some((t, _, en)) = newest {
            return Ok(Some(LastTrust { result: en.status.clone(), reason: en.reason.clone(), log_start_ms: Some(t) }));
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Health {
    pub healthy: bool,
    pub reasons: Vec<&'static str>,
    pub last_upload_ms: Option<u64>,
    pub last_log_trust: Option<LastTrust>,
    pub keys: Vec<KeyHealth>,
    pub chain_gaps: Vec<serde_json::Value>,
    pub dropped_frames: Vec<Span>,
    pub unknown_spans: Vec<UnknownSpan>,
    pub unknown_ms: u64,
    pub logs_waiting: u64,
    pub rebuild_pending: bool,
    /// The web server fills this from the upload settings.
    pub uploads_enabled: Option<bool>,
}

/// `/cars/{car}/health` over a period (default the last 30 days).
pub fn health(e: &Engine, car: CarId, period: &Period) -> QResult<Scoped<Health>> {
    let c = car_of(e, car)?;
    let rt = e.handle()?;
    let now = now_ms();
    let db = e.db();
    let meta = records::meta(db, car)?;
    // the last 30 days of the car's data (a car that stopped uploading still shows its last month)
    let end = meta.newest_ms.map_or(now, |n| n.saturating_add(1).min(now));
    let (a, b) = period.window(30 * 86_400_000, end)?;
    let mut keys = Vec::new();
    let mut waiting = 0u64;
    let mut chain_gaps = Vec::new();
    let mut last_upload: Option<u64> = None;
    for key in &c.vehicle_keys {
        let s = rt.block_on(e.judge().summary(key));
        let w = s.as_ref().map_or(0, |s| s.waiting as u64);
        waiting += w;
        let latest = rt.block_on(e.judge().latest_usable(key, 1)).unwrap_or_default();
        let lu = latest.first().and_then(|n| records::name_start_ms(n));
        last_upload = last_upload.max(lu);
        keys.push(KeyHealth {
            key: key.clone(),
            last_upload_ms: lu,
            preferred: c.preferred_key.as_deref() == Some(key),
            device: None,
            logs_waiting: w,
        });
    }
    for (x, y) in super::timeline::chain_gaps(e, car)? {
        if y > a && x < b {
            chain_gaps.push(serde_json::json!({ "from_ms": x, "to_ms": y }));
        }
    }
    let entries = records::entries_between(db, car, &meta, a, b)?;
    let mut dropped = Vec::new();
    let mut failed = false;
    for (_, en) in &entries {
        if let Outcome::Decoded { facts, .. } = &en.outcome
            && facts.dropped > 0
        {
            let (x, y) = facts.span();
            dropped.push(Span { start_ms: x, end_ms: Some(y) });
        }
        failed |= en.status == "failed" && !en.usable;
    }
    let last_trust = last_judged(e, car, &meta)?;
    let why = super::timeline::why_unknown(e, car, a, b)?;
    let mut unknown = Vec::new();
    let spans = all_rows::<StateRow>(db, car, a, b, Some(Source::Can))?.into_iter().map(|r| r.span).collect();
    for s in super::timeline::joined(spans) {
        let (x, y) = (s.start_ms.max(a), s.end_ms.min(b));
        if s.state == State::Unknown && y > x {
            let (reason, _) = super::timeline::classify(&why, x, y);
            unknown.push(UnknownSpan { start_ms: x, end_ms: Some(y), reason });
        }
    }
    let unknown_ms = unknown.iter().map(|u| u.end_ms.unwrap_or(b).saturating_sub(u.start_ms)).sum();
    let mut reasons = Vec::new();
    let last_seen = meta.newest_ms.or(last_upload);
    if last_seen.is_none_or(|t| now.saturating_sub(t) > 24 * super::now::STALE_MS) {
        reasons.push("upload_silence");
    }
    if failed || last_trust.as_ref().is_some_and(|t| t.result == "failed") {
        reasons.push("trust_failed");
    }
    if waiting > 0 {
        reasons.push("logs_waiting_trust");
    }
    let rebuild_pending = e.rebuild_pending(car);
    if rebuild_pending {
        reasons.push("rebuild_pending");
    }
    Ok(scoped(
        &c,
        Health {
            healthy: reasons.is_empty(),
            reasons,
            last_upload_ms: meta.last_done_ms.or(last_upload),
            last_log_trust: last_trust,
            keys,
            chain_gaps,
            dropped_frames: dropped,
            unknown_spans: unknown,
            unknown_ms,
            logs_waiting: waiting,
            rebuild_pending,
            uploads_enabled: None,
        },
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Kind {
    pub kind: &'static str,
    pub label: &'static str,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StorageDay {
    pub series: &'static str,
    pub source: &'static str,
    pub date: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Storage {
    pub kinds: Vec<Kind>,
    pub total_bytes: u64,
    pub items: Vec<StorageDay>,
    pub next: Option<String>,
}

/// `/cars/{car}/storage`: the bytes of its time series per day, newest first (`kinds=series,curves` filters them).
pub fn storage(e: &Engine, car: CarId, kinds: Option<&str>, paging: &Paging) -> QResult<Scoped<Storage>> {
    let c = car_of(e, car)?;
    let db = e.db();
    let want: Vec<&str> = kinds.map_or_else(|| vec!["series", "curves"], |k| k.split(',').collect());
    let mut items = Vec::new();
    let (mut series_b, mut curves_b) = (0u64, 0u64);
    for source in [Source::Can, Source::Teslamate] {
        for d in db.days::<model::Sample>(car, source)? {
            series_b += d.bytes;
            if want.contains(&"series") {
                items.push(StorageDay {
                    series: "samples",
                    source: super::source_word(source),
                    date: d.date.clone(),
                    bytes: d.bytes,
                });
            }
        }
        for d in db.days::<model::ChargePoint>(car, source)? {
            curves_b += d.bytes;
            if want.contains(&"curves") {
                items.push(StorageDay {
                    series: "charge",
                    source: super::source_word(source),
                    date: d.date.clone(),
                    bytes: d.bytes,
                });
            }
        }
    }
    let total = db.segment_bytes(car).unwrap_or(series_b + curves_b);
    let sig = format!("storage|{car}|{kinds:?}");
    let page = paginate(items, |d| (0.0, format!("{}|{}|{}", d.date, d.series, d.source)), true, &sig, paging, false)?;
    Ok(scoped(
        &c,
        Storage {
            kinds: vec![
                Kind { kind: "series", label: "Samples", bytes: series_b },
                Kind { kind: "curves", label: "Charge curves", bytes: curves_b },
            ],
            total_bytes: total,
            items: page.items,
            next: page.next,
        },
    ))
}

/// Engine figures for `/system/health`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SystemHealth {
    pub engine: crate::EngineStats,
    pub db: serde_json::Value,
    pub versions: serde_json::Value,
    pub timings_ms: serde_json::Value,
}

pub fn system_health(e: &Engine) -> SystemHealth {
    let s = e.stats();
    let d = e.db().stats();
    let per = |total: u64, n: u64| if n == 0 { None } else { Some(total as f64 / n as f64) };
    SystemHealth {
        timings_ms: serde_json::json!({
            "decode_per_log": per(s.decode_ms_total, s.logs_decoded + s.logs_left_out),
            "rebuild_per_window": per(s.rebuild_ms_total, s.windows_rebuilt),
        }),
        versions: serde_json::json!({
            "db_format": db::FORMAT_VERSION,
            "timeline_rules": timeline::RULES_VERSION,
            "decode": records::DECODE_VERSION,
            "import_mapping": import::MAPPING_VERSION,
        }),
        db: serde_json::json!({
            "cars": d.cars, "fleet_bytes": d.fleet_bytes, "open_cars": d.open_cars, "open_bytes": d.open_bytes,
            "cars_opened": d.cars_opened, "tables_read": d.tables_read,
        }),
        engine: s,
    }
}
