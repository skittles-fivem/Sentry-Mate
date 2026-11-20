//! Overlaps between imported and CAN history, a car's logs and their check results, `/cars/{car}/health`,
//! `/cars/{car}/storage`, and the engine's own figures for `/system/health`.

use super::{List, Paging, Period, QResult, QueryError, Scoped, SourceFilter, car_of, paginate, scoped};
use crate::records::{self, NS, Outcome};
use crate::{Engine, now_ms};
use db::StateRow;
use model::{CarId, Inputs, Source, State};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

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

/// Whether a row is shown: with `source=all` an imported row inside CAN history is hidden (CAN wins), unless that
/// overlap was flipped to show the imported side.
pub(crate) struct Shown {
    filter: Option<SourceFilter>,
    coverage: Vec<(u64, u64)>,
    flips: Vec<Flip>,
}

impl Shown {
    pub(crate) fn new(e: &Engine, car: CarId, filter: Option<SourceFilter>) -> QResult<Shown> {
        let imported = e.db().entry(car)?.is_some_and(|x| {
            let t = &x.summary.tally.teslamate;
            t.drives > 0 || t.charges > 0 || x.summary.tally.states > 0 && t.drives + t.charges > 0
        });
        if filter.is_some() || !imported {
            return Ok(Shown { filter, coverage: Vec::new(), flips: Vec::new() });
        }
        Ok(Shown { filter, coverage: imported_coverage(e, car)?, flips: flips(e, car)? })
    }

    /// Whether a row of `source` from `inputs`, starting at `t`, is shown.
    pub(crate) fn keep(&self, source: Source, inputs: Inputs, t: u64) -> bool {
        if let Some(f) = self.filter {
            return f.keeps(source, inputs);
        }
        let flipped = self.flips.iter().find(|f| f.start_ms <= t && t < f.end_ms).map(|f| f.shown.as_str());
        match source {
            Source::Live => flipped != Some("teslamate"),
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
    /// `can` or `teslamate` (the old words, kept for one release).
    pub shown: String,
    /// `live` or `import:teslamate`.
    pub shown_source: String,
    /// The live side's counts (`can` is the same, kept for one release).
    pub live: Counts,
    /// The imported side's counts (`teslamate` is the same, kept for one release).
    pub imported: Counts,
    pub can: Counts,
    pub teslamate: Counts,
    /// The two sides, live first.
    pub sources: Vec<super::SourceRef>,
}

impl Overlap {
    fn settle_words(&mut self) {
        self.shown_source = if self.shown == "teslamate" { "import:teslamate" } else { "live" }.to_string();
        self.live = self.can.clone();
        self.imported = self.teslamate.clone();
    }
}

/// `shown` as a PATCH may write it: `live` or `can`, `import:teslamate` or `teslamate`. Answers the old word.
fn shown_word(w: &str) -> Option<&'static str> {
    match w {
        "live" | "can" | "smo" => Some("can"),
        "import:teslamate" | "teslamate" | "import" => Some("teslamate"),
        _ => None,
    }
}

/// The spans the CAN history covers (online or asleep) over `[from, to)`, joined: the state rows stream past a page at
/// a time, only the joined spans are kept.
fn can_coverage(e: &Engine, car: CarId, from: u64, to: u64) -> QResult<Vec<(u64, u64)>> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    if from >= to {
        return Ok(out);
    }
    let mut q = db::Query::between(from, to).oldest_first().limit(db::MAX_PAGE);
    q.source = Some(Source::Live);
    loop {
        let page = e.db().list::<StateRow>(car, &q)?;
        for s in &page.items {
            if s.span.state == State::Unknown {
                continue;
            }
            match out.last_mut() {
                Some(last) if s.span.start_ms <= last.1.saturating_add(60_000) => last.1 = last.1.max(s.span.end_ms),
                _ => out.push((s.span.start_ms, s.span.end_ms)),
            }
        }
        match page.next {
            Some(c) => q.after = Some(c),
            None => return Ok(out),
        }
    }
}

/// The first and last start of a car's imported drives and charges, as `[first, last + 1)`: imported history can
/// overlap CAN history only there.
fn imported_span(e: &Engine, car: CarId) -> QResult<Option<(u64, u64)>> {
    fn ends<T: db::Item>(e: &Engine, car: CarId) -> QResult<Option<(u64, u64)>> {
        let mut q = db::Query::default().oldest_first().limit(1);
        q.source = Some(Source::Teslamate);
        let Some(first) = e.db().list::<T>(car, &q)?.items.first().map(db::Item::start_ms) else { return Ok(None) };
        let mut q = db::Query::default().limit(1);
        q.source = Some(Source::Teslamate);
        let last = e.db().list::<T>(car, &q)?.items.first().map_or(first, db::Item::start_ms);
        Ok(Some((first, last.max(first).saturating_add(1))))
    }
    let join = |x: Option<(u64, u64)>, y: Option<(u64, u64)>| match (x, y) {
        (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.max(b.1))),
        (a, b) => a.or(b),
    };
    Ok(join(ends::<model::Drive>(e, car)?, ends::<model::Charge>(e, car)?))
}

/// Counts each row of a table (of one source) into the span its start falls in: one pass, oldest first and a page at
/// a time, over the spans' reach, with a pointer that only moves forward.
fn count_into<T: db::Item>(
    e: &Engine,
    car: CarId,
    source: Source,
    spans: &[(u64, u64)],
    counts: &mut [u64],
) -> QResult<()> {
    let (Some(first), Some(last)) = (spans.first(), spans.last()) else { return Ok(()) };
    let mut q = db::Query::between(first.0, last.1).oldest_first().limit(db::MAX_PAGE);
    q.source = Some(source);
    let mut i = 0usize;
    loop {
        let page = e.db().list::<T>(car, &q)?;
        for r in &page.items {
            let t = r.start_ms();
            while spans.get(i).is_some_and(|s| s.1 <= t) {
                i += 1;
            }
            if spans.get(i).is_some_and(|s| s.0 <= t)
                && let Some(n) = counts.get_mut(i)
            {
                *n += 1;
            }
        }
        match page.next {
            Some(c) => q.after = Some(c),
            None => return Ok(()),
        }
    }
}

/// The overlaps among `spans` (CAN coverage): the spans that hold imported drives or charges, with both sides' counts.
fn overlaps_of(e: &Engine, car: CarId, spans: &[(u64, u64)], fl: &[Flip]) -> QResult<Vec<Overlap>> {
    let n = spans.len();
    let (mut tm_d, mut tm_c, mut can_d, mut can_c) = (vec![0u64; n], vec![0u64; n], vec![0u64; n], vec![0u64; n]);
    count_into::<model::Drive>(e, car, Source::Teslamate, spans, &mut tm_d)?;
    count_into::<model::Charge>(e, car, Source::Teslamate, spans, &mut tm_c)?;
    let mut items = Vec::new();
    let mut held: Vec<(u64, u64)> = Vec::new();
    for (i, &(a, b)) in spans.iter().enumerate() {
        let tm = Counts { drives: tm_d.get(i).copied().unwrap_or(0), charges: tm_c.get(i).copied().unwrap_or(0) };
        if tm.drives + tm.charges == 0 {
            continue;
        }
        held.push((a, b));
        let shown = fl.iter().find(|f| f.start_ms == a).map_or_else(|| "can".to_string(), |f| f.shown.clone());
        items.push(Overlap {
            id: format!("overlap:{a}"),
            start_ms: a,
            end_ms: b,
            shown,
            shown_source: String::new(),
            live: Counts::default(),
            imported: Counts::default(),
            can: Counts::default(),
            teslamate: tm,
            sources: super::source_refs(&[model::Origin::Smo, model::Origin::Import(model::ImportSite::Teslamate)]),
        });
    }
    // the CAN side counted over the overlaps only
    can_d.truncate(held.len());
    can_c.truncate(held.len());
    count_into::<model::Drive>(e, car, Source::Live, &held, &mut can_d)?;
    count_into::<model::Charge>(e, car, Source::Live, &held, &mut can_c)?;
    for (i, o) in items.iter_mut().enumerate() {
        o.can = Counts { drives: can_d.get(i).copied().unwrap_or(0), charges: can_c.get(i).copied().unwrap_or(0) };
        o.settle_words();
    }
    Ok(items)
}

/// The CAN coverage where imported history exists (none when nothing was imported).
fn imported_coverage(e: &Engine, car: CarId) -> QResult<Vec<(u64, u64)>> {
    match imported_span(e, car)? {
        Some((lo, hi)) => can_coverage(e, car, lo, hi),
        None => Ok(Vec::new()),
    }
}

/// `/cars/{car}/overlaps`: spans where imported and CAN history both exist. Only the span imported history reaches is
/// read; each table streams past once.
pub fn overlaps(e: &Engine, car: CarId, paging: &Paging) -> QResult<Scoped<List<Overlap>>> {
    let c = car_of(e, car)?;
    let spans = imported_coverage(e, car)?;
    let items = overlaps_of(e, car, &spans, &flips(e, car)?)?;
    let page = paginate(items, |o| (o.start_ms as f64, o.id.clone()), true, &format!("overlaps|{car}"), paging, false)?;
    Ok(scoped(&c, page))
}

/// Flips an overlap (`PATCH /cars/{car}/overlaps` with `{ id, shown }`), found by its start wherever it is in the list.
pub fn set_overlap(e: &Engine, car: CarId, id: &str, shown: &str) -> QResult<Overlap> {
    let Some(shown) = shown_word(shown) else {
        return Err(QueryError::invalid("shown", "live or import:teslamate (old words: can, teslamate)"));
    };
    car_of(e, car)?;
    let none = || QueryError::NotFound("no_such_overlap");
    let start: u64 = id.strip_prefix("overlap:").and_then(|t| t.parse().ok()).ok_or_else(none)?;
    let span = imported_coverage(e, car)?.into_iter().find(|s| s.0 == start).ok_or_else(none)?;
    let mut fl = flips(e, car)?;
    let mut o = overlaps_of(e, car, &[span], &fl)?.into_iter().next().ok_or_else(none)?;
    fl.retain(|f| f.start_ms != o.start_ms);
    if shown == "teslamate" {
        fl.push(Flip { start_ms: o.start_ms, end_ms: o.end_ms, shown: shown.into() });
    }
    records::put(e.db(), NS, &format!("car/{car}/overlaps"), &fl)?;
    o.shown = shown.to_string();
    o.settle_words();
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

/// A car's logs (every CAN key of it), newest first, with their check results and what was decoded. Each key's trust
/// records are read in order up to the cursor (or the period's end) keeping only a page and one more, and only the
/// page's day records are read: a page holds `limit + 1` rows whatever the history's length.
pub fn logs(e: &Engine, car: CarId, period: &Period, paging: &Paging) -> QResult<Scoped<List<LogRow>>> {
    let c = car_of(e, car)?;
    let rt = e.handle()?;
    let (a, b) = period.bounds()?;
    let limit = paging.limit()?;
    let sig = format!("logs|{car}|{}", period.sig());
    let after = super::read_after(paging, &sig)?;
    let mut found: Vec<(u64, String, String, ingest::judge::Record)> = Vec::new();
    for key in &c.vehicle_keys {
        let (cur, cur_start) = (after.clone(), after.as_ref().map(|x| x.0));
        let keep = move |name: &str, _: &ingest::judge::Record| {
            let Some(s) = records::name_start_ms(name) else { return false };
            s >= a
                && s < b
                && cur.as_ref().is_none_or(|at| {
                    super::cmp_key(&(s as f64, name.to_string()), at, true) == std::cmp::Ordering::Greater
                })
        };
        let stop = move |name: &str| {
            records::name_start_ms(name).is_some_and(|s| s >= b || cur_start.is_some_and(|at| s as f64 > at))
        };
        let got = rt
            .block_on(e.judge().newest(key, limit + 1, keep, stop))
            .map_err(|x| QueryError::Engine(crate::Error::Io(x)))?;
        for (name, rec) in got {
            if let Some(s) = records::name_start_ms(&name) {
                found.push((s, name, key.clone(), rec));
            }
        }
    }
    found.sort_by(|x, y| super::cmp_key(&(x.0 as f64, x.1.clone()), &(y.0 as f64, y.1.clone()), true));
    let more = found.len() > limit;
    found.truncate(limit);
    let next = match found.last() {
        Some(last) if more => Some(super::make_after(&(last.0 as f64, last.1.clone()), &sig)?),
        _ => None,
    };
    let mut items = Vec::with_capacity(found.len());
    let mut day_cache: Option<(u32, records::DayRec)> = None;
    for (start, name, key, rec) in found {
        let d = records::day_of(start);
        if day_cache.as_ref().is_none_or(|(x, _)| *x != d) {
            day_cache = Some((d, records::day(e.db(), car, d)?));
        }
        let entry = day_cache.as_ref().and_then(|(_, r)| r.logs.get(&name));
        let (decoded, samples, dropped, buses, from, to) = match entry.map(|x| &x.outcome) {
            Some(Outcome::Decoded { facts, samples, samples_from_ms, samples_to_ms }) => {
                ("decoded", Some(*samples), Some(facts.dropped), facts.buses.clone(), *samples_from_ms, *samples_to_ms)
            }
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
            decoded_ms: entry.map(|x| x.done_ms),
            name,
            key,
            start_ms: start,
            decoded,
            samples,
            dropped,
            buses,
            data_from_ms: from,
            data_to_ms: to,
        });
    }
    Ok(scoped(&c, List { items, next, total: None }))
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

/// Each list of `/cars/{car}/health` holds at most this many rows (the newest); `*_total` counts them all.
pub const HEALTH_LIST_MAX: usize = 500;

/// Keeps the newest `HEALTH_LIST_MAX` of a list fed oldest first (roughly: sorted at the end), counting them all.
struct Newest<T> {
    rows: std::collections::VecDeque<T>,
    total: u64,
}

impl<T> Newest<T> {
    fn new() -> Newest<T> {
        Newest { rows: std::collections::VecDeque::new(), total: 0 }
    }

    fn push(&mut self, t: T) {
        self.total += 1;
        if self.rows.len() == HEALTH_LIST_MAX {
            self.rows.pop_front();
        }
        self.rows.push_back(t);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Health {
    pub healthy: bool,
    pub reasons: Vec<&'static str>,
    /// When the car's newest log arrived (its upload), the newest of its keys'.
    pub last_upload_ms: Option<u64>,
    pub last_log_trust: Option<LastTrust>,
    pub keys: Vec<KeyHealth>,
    /// At most [`HEALTH_LIST_MAX`] (the newest) in each list; the `*_total` counts are of the whole period.
    pub chain_gaps: Vec<serde_json::Value>,
    pub chain_gaps_total: u64,
    pub dropped_frames: Vec<Span>,
    pub dropped_frames_total: u64,
    pub unknown_spans: Vec<UnknownSpan>,
    pub unknown_spans_total: u64,
    pub unknown_ms: u64,
    pub logs_waiting: u64,
    pub rebuild_pending: bool,
    /// The web server fills this from the upload settings.
    pub uploads_enabled: Option<bool>,
}

/// `/cars/{car}/health` over a period (default the last 30 days). The day records stream past one day at a time, the
/// state spans a page at a time, and each key's chain is read once.
pub fn health(e: &Engine, car: CarId, period: &Period) -> QResult<Scoped<Health>> {
    use super::timeline::Why;
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
    let mut last_upload: Option<u64> = None;
    for key in &c.vehicle_keys {
        let s = rt.block_on(e.judge().summary(key));
        let w = s.as_ref().map_or(0, |s| s.waiting as u64);
        waiting += w;
        // when its newest log arrived, not the time in the log's name nor when the server worked on it
        let lu = rt.block_on(e.judge().last_arrived(key)).map_err(|x| QueryError::Engine(crate::Error::Io(x)))?;
        last_upload = last_upload.max(lu);
        keys.push(KeyHealth {
            key: key.clone(),
            last_upload_ms: lu,
            preferred: c.preferred_key.as_deref() == Some(key),
            device: None,
            logs_waiting: w,
        });
    }
    let mut why: Vec<Why> = Vec::new();
    let mut chain_gaps = Newest::new();
    for (x, y) in super::timeline::chain_gaps(e, car)? {
        if y > a && x < b {
            chain_gaps.push(serde_json::json!({ "from_ms": x, "to_ms": y }));
            why.push(Why { from_ms: x, to_ms: y, reason: "no_log", cause: "chain_gap" });
        }
    }
    // the day records of the period, one at a time (a log's data spills a day either side of its start's day)
    let mut dropped = Newest::new();
    let mut failed = false;
    let (lo, hi) = (records::day_of(a).saturating_sub(1), records::day_of(b.saturating_sub(1).max(a)));
    for &d in meta.days.range(lo..=hi) {
        let mut day: Vec<(u64, String, records::LogEntry)> = records::day(db, car, d)?
            .logs
            .into_iter()
            .filter_map(|(n, en)| {
                let (x, y) = en.span();
                (y >= a && x < b).then_some((x, n, en))
            })
            .collect();
        day.sort_by(|p, q| (p.0, &p.1).cmp(&(q.0, &q.1)));
        for (_, _, en) in day {
            match &en.outcome {
                Outcome::Hole { from_ms, to_ms, reason } => {
                    let (reason, cause) =
                        if reason == "not_stored" { ("no_log", "no_log") } else { ("failed_check", "unknown") };
                    why.push(Why { from_ms: *from_ms, to_ms: *to_ms, reason, cause });
                }
                Outcome::Decoded { facts, .. } => {
                    for n in &facts.not_recorded {
                        let from = n.paused_ms.map_or(n.at_ms.saturating_sub(1), |p| n.at_ms.saturating_sub(p));
                        why.push(Why { from_ms: from, to_ms: n.at_ms, reason: "not_recorded", cause: "not_recorded" });
                    }
                    if facts.dropped > 0 {
                        let (x, y) = facts.span();
                        dropped.push(Span { start_ms: x, end_ms: Some(y) });
                        why.push(Why { from_ms: x, to_ms: y, reason: "dropped_frames", cause: "unknown" });
                    }
                }
            }
            failed |= en.status == "failed" && !en.usable;
        }
    }
    let last_trust = last_judged(e, car, &meta)?;
    let mut unknown = Newest::new();
    let mut unknown_ms = 0u64;
    if a < b {
        super::timeline::each_span(e, car, a, Some(Source::Live), |s| {
            if s.start_ms >= b {
                return Ok(false);
            }
            let (x, y) = (s.start_ms.max(a), s.end_ms.min(b));
            if s.state == State::Unknown && y > x {
                let (reason, _) = super::timeline::classify(&why, x, y);
                unknown_ms += y - x;
                unknown.push(UnknownSpan { start_ms: x, end_ms: Some(y), reason });
            }
            Ok(true)
        })?;
    }
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
    let mut dropped_rows: Vec<Span> = dropped.rows.into_iter().collect();
    dropped_rows.sort_by_key(|s| s.start_ms);
    Ok(scoped(
        &c,
        Health {
            healthy: reasons.is_empty(),
            reasons,
            last_upload_ms: last_upload,
            last_log_trust: last_trust,
            keys,
            chain_gaps_total: chain_gaps.total,
            chain_gaps: chain_gaps.rows.into_iter().collect(),
            dropped_frames_total: dropped.total,
            dropped_frames: dropped_rows,
            unknown_spans_total: unknown.total,
            unknown_spans: unknown.rows.into_iter().collect(),
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
    /// The old word (`can`, `tesla_api`, `teslamate`), kept for one release.
    pub source: &'static str,
    pub sources: Vec<super::SourceRef>,
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

/// Bytes of the files right in `dir` (not its folders); 0 when it can't be read.
fn files_bytes(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten().filter_map(|f| f.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum()
}

/// Bytes of everything under `dir` (folders followed to `depth` levels; links never followed).
pub fn tree_bytes(dir: &Path, depth: u32) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0u64;
    for f in rd.flatten() {
        let Ok(t) = f.file_type() else { continue };
        if t.is_file() {
            n += f.metadata().map_or(0, |m| m.len());
        } else if t.is_dir() && depth > 0 {
            n += tree_bytes(&f.path(), depth - 1);
        }
    }
    n
}

/// The data folder (the log store's parent: `<data>/can`).
fn data_dir(e: &Engine) -> Option<&Path> {
    e.store().dir().parent()
}

/// A car's stored CAN logs (sealed, with their sidecars) whose start falls in `[a, b)`, per day of their start in
/// `tz`: `(date, bytes)`, and the total of every log (the period apart). One look at each file's size in the car's key
/// folders; logs still arriving are not counted.
fn log_days(e: &Engine, c: &model::Car, tz: chrono_tz::Tz, a: u64, b: u64) -> (BTreeMap<String, u64>, u64) {
    let mut days: BTreeMap<String, u64> = BTreeMap::new();
    let mut total = 0u64;
    for key in &c.vehicle_keys {
        let Ok(rd) = std::fs::read_dir(e.store().dir().join(key)) else { continue };
        for f in rd.flatten() {
            let Ok(m) = f.metadata() else { continue };
            if !m.is_file() {
                continue;
            }
            total += m.len();
            let name = f.file_name();
            let name = name.to_string_lossy();
            let log = name.strip_suffix(".json").unwrap_or(&name);
            if let Some(t) = records::name_start_ms(log).filter(|t| *t >= a && *t < b) {
                *days.entry(crate::tz::date_str(t, tz)).or_default() += m.len();
            }
        }
    }
    (days, total)
}

/// The kinds a car's data is kept as: `logs` (the CAN logs as uploaded, sealed), `series` (1 Hz samples), `curves`
/// (charge curves) and `rows` (drives, charges, states, updates and the rest of the car's tables).
pub const STORAGE_KINDS: [&str; 4] = ["logs", "series", "curves", "rows"];

/// `/cars/{car}/storage`: the bytes per kind, and per day and kind (newest day first, only days with data; `kinds`
/// filters the days, the period too: a log by its start, a stored day when it meets the period). Imported days carry
/// `source: "teslamate"`. Logs are grouped by the car's own (local) day; series and curves are stored per UTC day,
/// so their rows carry that UTC date. The per-kind bytes are of the whole history.
pub fn storage(
    e: &Engine,
    car: CarId,
    kinds: Option<&str>,
    period: &Period,
    paging: &Paging,
) -> QResult<Scoped<Storage>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let tz = crate::tz::zone(&c.time_zone);
    let meets = |d: &db::DayInfo| d.start_ms < b && d.start_ms.saturating_add(86_400_000) > a;
    let db = e.db();
    let want: Vec<&str> = match kinds {
        Some(k) => {
            let v: Vec<&str> = k.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
            if let Some(bad) = v.iter().find(|k| !STORAGE_KINDS.contains(k)) {
                return Err(QueryError::bad("kinds", &format!("{bad} is not one of logs, series, curves, rows")));
            }
            v
        }
        None => STORAGE_KINDS.to_vec(),
    };
    let mut items = Vec::new();
    let (mut series_b, mut curves_b) = (0u64, 0u64);
    for stream in model::Stream::ALL {
        let word = match stream {
            model::Stream::Smo => "can",
            model::Stream::TeslaApi => "tesla_api",
            model::Stream::Teslamate => "teslamate",
        };
        let sources = || super::source_refs(&[stream.origin()]);
        for d in db.days::<model::Sample>(car, stream)? {
            series_b += d.bytes;
            if want.contains(&"series") && d.bytes > 0 && meets(&d) {
                items.push(StorageDay {
                    series: "series",
                    source: word,
                    sources: sources(),
                    date: d.date,
                    bytes: d.bytes,
                });
            }
        }
        for d in db.days::<model::ChargePoint>(car, stream)? {
            curves_b += d.bytes;
            if want.contains(&"curves") && d.bytes > 0 && meets(&d) {
                items.push(StorageDay {
                    series: "curves",
                    source: word,
                    sources: sources(),
                    date: d.date,
                    bytes: d.bytes,
                });
            }
        }
    }
    let (logs, logs_b) = log_days(e, &c, tz, a, b);
    if want.contains(&"logs") {
        for (date, bytes) in logs {
            items.push(StorageDay {
                series: "logs",
                source: "can",
                sources: super::source_refs(&[model::Origin::Smo]),
                date,
                bytes,
            });
        }
    }
    let rows_b = data_dir(e).map_or(0, |d| files_bytes(&d.join("db").join("cars").join(car.to_string())));
    let order = |s: &str| STORAGE_KINDS.iter().position(|k| *k == s).unwrap_or(9);
    let sig = format!("storage|{car}|{want:?}|{}", period.sig());
    let page = paginate(
        items,
        |d| (0.0, format!("{}|{}|{}", d.date, 9 - order(d.series), d.source)),
        true,
        &sig,
        paging,
        false,
    )?;
    let kinds = vec![
        Kind { kind: "logs", label: "CAN logs (sealed)", bytes: logs_b },
        Kind { kind: "series", label: "Time series", bytes: series_b },
        Kind { kind: "curves", label: "Charge curves", bytes: curves_b },
        Kind { kind: "rows", label: "Drives, charges, states", bytes: rows_b },
    ];
    let total = kinds.iter().map(|k| k.bytes).sum();
    Ok(scoped(&c, Storage { kinds, total_bytes: total, items: page.items, next: page.next }))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Growth {
    pub date: String,
    pub bytes: u64,
}

/// `/system/storage`, but for the free space (the web server adds it).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServerStorage {
    pub kinds: Vec<Kind>,
    pub total_bytes: u64,
    /// Bytes added on each of the last 30 full UTC days (CAN data of every car: logs, series, curves).
    pub growth: Vec<Growth>,
    pub growth_per_day_bytes: Option<u64>,
}

/// The server's stored data: every car's, imported history apart, the server's own files (accounts, settings,
/// places, the fleet list) and the offline map tiles. One pass over each car's days; the per-day sums are kept only for the last 31 days.
pub fn server_storage(e: &Engine, now_ms: u64) -> QResult<ServerStorage> {
    let db = e.db();
    let utc = crate::tz::zone("UTC");
    let today = crate::tz::date_str(now_ms, utc);
    let first = crate::tz::date_str(now_ms.saturating_sub(31 * 86_400_000), utc);
    let mut growth: BTreeMap<String, u64> = BTreeMap::new();
    let mut add = |date: &str, b: u64| {
        if date >= first.as_str() && date < today.as_str() {
            *growth.entry(date.to_string()).or_default() += b;
        }
    };
    let (mut logs, mut series, mut rows, mut imported) = (0u64, 0u64, 0u64, 0u64);
    let data = data_dir(e);
    for car in super::all_cars(e)? {
        let Some(c) = db.car(car)? else { continue };
        let (days, total) = log_days(e, &c, utc, 0, u64::MAX);
        logs += total;
        for (d, b) in &days {
            add(d, *b);
        }
        for d in db
            .days::<model::Sample>(car, Source::Live)?
            .into_iter()
            .chain(db.days::<model::ChargePoint>(car, Source::Live)?)
        {
            series += d.bytes;
            add(&d.date, d.bytes);
        }
        for d in db
            .days::<model::Sample>(car, Source::Teslamate)?
            .into_iter()
            .chain(db.days::<model::ChargePoint>(car, Source::Teslamate)?)
        {
            imported += d.bytes;
        }
        rows += data.map_or(0, |d| files_bytes(&d.join("db").join("cars").join(car.to_string())));
    }
    // the uploaded TeslaMate backups an import keeps (sealed, up to 64 GB each): their own kind, not the server's
    let backups = data.map_or(0, |d| tree_bytes(&d.join("web").join("imports"), 3));
    // the server's own: the fleet list, places, settings, the key/value store, accounts, the audit log, the web side
    let own = data.map_or(0, |d| {
        files_bytes(&d.join("db"))
            + tree_bytes(&d.join("db").join("kv"), 4)
            + ["accounts", "audit", "web"].iter().map(|x| tree_bytes(&d.join(x), 4)).sum::<u64>()
    });
    let own = own.saturating_sub(backups);
    let kinds = vec![
        Kind { kind: "logs", label: "CAN logs (sealed)", bytes: logs },
        Kind { kind: "series", label: "Time series and charge curves", bytes: series },
        Kind { kind: "rows", label: "Drives, charges, states", bytes: rows },
        Kind { kind: "imported", label: "Imported from TeslaMate", bytes: imported },
        Kind { kind: "server", label: "Accounts, settings and places", bytes: own },
        Kind { kind: "import_uploads", label: "TeslaMate backups uploaded to import", bytes: backups },
        Kind { kind: "tiles", label: "Offline map tiles", bytes: data.map_or(0, |d| tree_bytes(&d.join("maps"), 3)) },
    ];
    let total = kinds.iter().map(|k| k.bytes).sum();
    let growth: Vec<Growth> = growth.into_iter().map(|(date, bytes)| Growth { date, bytes }).collect();
    let growth: Vec<Growth> = growth.into_iter().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect();
    let per_day = (!growth.is_empty()).then(|| growth.iter().map(|g| g.bytes).sum::<u64>() / growth.len() as u64);
    Ok(ServerStorage { kinds, total_bytes: total, growth, growth_per_day_bytes: per_day })
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
