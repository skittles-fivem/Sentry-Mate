//! `/cars/{car}/timeline`, `/states`, `/activity`, `/gaps`.

use super::{List, Paging, Period, QResult, QueryError, Scoped, car_of, paginate, parse_place, r, scoped};
use crate::Engine;
use crate::pipeline::all_rows;
use db::{StateRow, UpdateRow};
use model::{CarId, Charge, Drive, Source, State, StateSpan};
use serde::Serialize;

/// Unknown for this long between drives and charges is a gap row of its own.
pub const GAP_MS: u64 = 600_000;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Point {
    pub place: Option<super::PlaceBrief>,
    pub address: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ParkedMs {
    pub asleep: u64,
    pub awake: u64,
    pub awake_vehicle_bus_only: u64,
    pub unknown: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    /// `<kind>:<start_ms>`.
    pub id: String,
    /// `drive`, `charge`, `parked`, `gap`, `update`.
    pub kind: &'static str,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub start: Option<Point>,
    pub end: Option<Point>,
    pub odometer_km: Option<f64>,
    pub distance_km: Option<f64>,
    pub duration_ms: u64,
    pub soc_end_pct: Option<f64>,
    pub soc_delta_pct: Option<f64>,
    pub range_end_km: Option<f64>,
    pub range_delta_km: Option<f64>,
    pub energy_delta_kwh: Option<f64>,
    pub energy_basis: Option<&'static str>,
    pub outside_avg_c: Option<f64>,
    pub cost: Option<super::charges::Cost>,
    #[serde(rename = "ref")]
    pub reference: Option<serde_json::Value>,
    pub parked_ms: Option<ParkedMs>,
    pub awake_reasons: Vec<&'static str>,
    /// `no_log`, `not_recorded`, `failed_check`, `unreadable`, `other_vin`, `dropped_frames`.
    pub gap_reason: Option<&'static str>,
    #[serde(flatten)]
    pub from: super::Sources,
    /// The fields an import filled where the car's own inputs had nothing ([`super::gapfill`]): field -> the imports
    /// that gave it. Empty on every row but a drive that was filled.
    pub value_sources: super::gapfill::ValueSources,
    /// A drive's or charge's flags (`in_progress`, ...); null on the other rows.
    pub flags: Option<serde_json::Value>,
    pub version: Option<String>,
    pub version_source: Option<&'static str>,
    /// An update row found from the modules' firmware fingerprints: `fingerprint`, with the modules it changed
    /// (`bms`, `vcfront` ...); left out on every other row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_by: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_modules: Option<Vec<String>>,
}

fn point(w: &super::Where) -> Point {
    Point { place: w.place.clone(), address: w.address.short.clone(), lat: w.lat, lon: w.lon, label: w.label.clone() }
}

/// Splits a car's time by state over `[a, b)`: asleep, awake (both buses), awake on the vehicle bus only, unknown.
pub(crate) fn split(spans: &[StateSpan], a: u64, b: u64) -> ParkedMs {
    let mut p = ParkedMs::default();
    let mut covered = 0;
    for s in spans {
        let (x, y) = (s.start_ms.max(a), s.end_ms.min(b));
        if y <= x {
            continue;
        }
        covered += y - x;
        match s.state {
            State::Asleep => p.asleep += y - x,
            State::Unknown => p.unknown += y - x,
            State::Online if s.buses.iter().any(|b| b == "can1") => p.awake += y - x,
            State::Online => p.awake_vehicle_bus_only += y - x,
        }
    }
    p.unknown += b.saturating_sub(a).saturating_sub(covered);
    p
}

/// `/timeline` filters.
#[derive(Debug, Clone, Default)]
pub struct TimelineQuery {
    pub period: Period,
    /// `drive,charge,parked,gap,update` (absent: all).
    pub kinds: Option<String>,
    pub place: Option<String>,
    pub q: Option<String>,
}

const KINDS: [&str; 5] = ["drive", "charge", "parked", "gap", "update"];

/// Words a row is found by, besides its codes.
fn words(r: &Row) -> String {
    let mut s = String::from(match r.kind {
        "drive" => "drive driving",
        "charge" => "charge charging",
        "parked" => "parked",
        "gap" => "gap in the data",
        _ => "software update",
    });
    if let Some(g) = r.gap_reason {
        s.push(' ');
        s.push_str(g);
        s.push(' ');
        s.push_str(match g {
            "no_log" => "no log arrived for this time",
            "not_recorded" => "the bridge was not recording",
            "failed_check" => "a log failed its check",
            "unreadable" => "a log could not be read",
            "other_vin" => "a log names another car",
            _ => "frames were dropped",
        });
    }
    for p in [&r.start, &r.end].into_iter().flatten() {
        if let Some(l) = &p.label {
            s.push(' ');
            s.push_str(l);
        }
    }
    if let Some(v) = &r.version {
        s.push(' ');
        s.push_str(v);
    }
    s.to_lowercase()
}

/// A drive or charge row, and the spell between rows.
type Piece = (u64, u64, Row);

/// Rows of the stretch `[x, y)` between `before` and `after` (`after` none: the stretch still going, `y` now): parked,
/// cut at unknown spans of [`GAP_MS`] or more into gap rows. `unknowns` are the stretch's unknown spans (with their
/// reason when it is known already).
#[allow(clippy::too_many_arguments)]
fn stretch_rows(
    x: u64,
    y: u64,
    before: &Row,
    after: Option<&Row>,
    unknowns: &[(u64, u64, Option<&'static str>)],
    states: &[StateSpan],
    why: &mut dyn FnMut(u64, u64) -> &'static str,
    now_soc: (Option<f64>, Option<f64>),
    out: &mut Vec<Row>,
) {
    let open = after.is_none();
    let at = before.end.clone().or(before.start.clone());
    let (soc0, range0) = (before.soc_end_pct, before.range_end_km);
    let (soc1, range1) = match after {
        Some(n) => (
            r(n.soc_end_pct.zip(n.soc_delta_pct).map(|(e, d)| e - d), 1),
            r(n.range_end_km.zip(n.range_delta_km).map(|(e, d)| e - d), 1),
        ),
        None => (r(now_soc.0, 1), r(now_soc.1, 1)),
    };
    let parked = |from: u64, to: u64, out: &mut Vec<Row>| {
        if to <= from {
            return;
        }
        out.push(Row {
            id: format!("parked:{from}"),
            kind: "parked",
            start_ms: from,
            end_ms: (!(open && to == y)).then_some(to),
            start: at.clone(),
            end: None,
            odometer_km: before.odometer_km,
            distance_km: None,
            duration_ms: to - from,
            soc_end_pct: soc1,
            soc_delta_pct: r(soc1.zip(soc0).map(|(a, b)| a - b), 1),
            range_end_km: range1,
            range_delta_km: r(range1.zip(range0).map(|(a, b)| a - b), 1),
            // the energy is read for the rows of a page only ([`fill_energy`])
            energy_delta_kwh: None,
            energy_basis: None,
            outside_avg_c: None,
            cost: None,
            reference: None,
            parked_ms: Some(split(states, from, to)),
            // the only reason the decoded signals give yet is charging, and a stretch holds no charge
            awake_reasons: Vec::new(),
            gap_reason: None,
            from: super::Sources::family(model::Source::Live),
            value_sources: Default::default(),
            flags: None,
            version: None,
            version_source: None,
            detected_by: None,
            changed_modules: None,
        });
    };
    let mut cur = x;
    for &(ga, gb, known) in unknowns {
        let (ga, gb) = (ga.max(cur), gb.min(y));
        if gb <= ga || gb - ga < GAP_MS {
            continue;
        }
        parked(cur, ga, out);
        out.push(Row {
            id: format!("gap:{ga}"),
            kind: "gap",
            start_ms: ga,
            end_ms: (!(open && gb == y)).then_some(gb),
            start: at.clone(),
            end: after.and_then(|a| a.start.clone()),
            odometer_km: None,
            distance_km: None,
            duration_ms: gb - ga,
            soc_end_pct: None,
            soc_delta_pct: None,
            range_end_km: None,
            range_delta_km: None,
            energy_delta_kwh: None,
            energy_basis: None,
            outside_avg_c: None,
            cost: None,
            reference: None,
            parked_ms: Some(ParkedMs { unknown: gb - ga, ..ParkedMs::default() }),
            awake_reasons: Vec::new(),
            gap_reason: Some(known.unwrap_or_else(|| why(ga, gb))),
            from: super::Sources::family(model::Source::Live),
            value_sources: Default::default(),
            flags: None,
            version: None,
            version_source: None,
            detected_by: None,
            changed_modules: None,
        });
        cur = gb;
    }
    parked(cur, y, out);
}

/// Calls `f` with each of the car's state spans from the one that holds `from` on, oldest first: rows joined where
/// the same state and buses touch (they are stored cut at idle points), read a page at a time; `f` answers whether
/// to go on. Rows of both sources come in time order; only touching rows of one source join.
pub(crate) fn each_span(
    e: &Engine,
    car: CarId,
    from: u64,
    source: Option<Source>,
    mut f: impl FnMut(StateSpan) -> QResult<bool>,
) -> QResult<()> {
    let mut q = db::Query::between(from, u64::MAX).oldest_first().limit(db::MAX_PAGE);
    q.source = source;
    let mut cur: Option<StateSpan> = None;
    loop {
        let page = e.db().list::<StateRow>(car, &q)?;
        for row in page.items {
            let s = row.span;
            match cur.as_mut() {
                Some(l)
                    if l.end_ms == s.start_ms
                        && l.state == s.state
                        && l.buses == s.buses
                        && l.source == s.source
                        && l.inputs == s.inputs =>
                {
                    l.end_ms = l.end_ms.max(s.end_ms);
                }
                _ => {
                    if let Some(done) = cur.replace(s)
                        && !f(done)?
                    {
                        return Ok(());
                    }
                }
            }
        }
        match page.next {
            Some(c) => q.after = Some(c),
            None => break,
        }
    }
    if let Some(done) = cur {
        f(done)?;
    }
    Ok(())
}

/// The car's newest `k` state spans (CAN), newest first, joined as [`each_span`] joins them: read back from the
/// newest row until `k` spans are whole.
pub(crate) fn newest_spans(e: &Engine, car: CarId, k: usize) -> QResult<Vec<StateSpan>> {
    let mut q = db::Query { source: Some(Source::Live), ..db::Query::default().limit(64) };
    let mut out: Vec<StateSpan> = Vec::new();
    let mut cur: Option<StateSpan> = None;
    loop {
        let page = e.db().list::<StateRow>(car, &q)?;
        for row in page.items {
            let s = row.span;
            match cur.as_mut() {
                Some(l) if s.end_ms == l.start_ms && l.state == s.state && l.buses == s.buses => {
                    l.start_ms = l.start_ms.min(s.start_ms);
                }
                _ => {
                    if let Some(done) = cur.replace(s) {
                        out.push(done);
                        if out.len() >= k {
                            return Ok(out);
                        }
                    }
                }
            }
        }
        match page.next {
            Some(c) => q.after = Some(c),
            None => break,
        }
    }
    out.extend(cur);
    Ok(out)
}

/// The start of the car's oldest row of any kind (state, drive, charge, update), if it has any.
fn oldest_ms(e: &Engine, car: CarId) -> QResult<Option<u64>> {
    let q = db::Query::default().oldest_first().limit(1);
    let db = e.db();
    let t = [
        db.list::<StateRow>(car, &q)?.items.first().map(|s| s.span.start_ms),
        db.list::<Drive>(car, &q)?.items.first().map(|d| d.start_ms),
        db.list::<Charge>(car, &q)?.items.first().map(|c| c.start_ms),
        db.list::<UpdateRow>(car, &q)?.items.first().map(|u| u.update.start_ms),
    ];
    Ok(t.into_iter().flatten().min())
}

/// Every timeline row of a car whose start is in `[a, b)`, oldest first. Each row is whole whatever the period: a
/// stretch between drives and charges runs from the one before (however long before) to the next one (after `b`
/// too), and the stretch after the last one runs to now (its last row still going), unless that one is still going.
pub(crate) fn all_rows_of(e: &Engine, car: CarId, a: u64, b: u64) -> QResult<Vec<Row>> {
    let c = car_of(e, car)?;
    if a >= b {
        return Ok(Vec::new());
    }
    let db = e.db();
    let now = crate::now_ms();
    let dx = super::drives::ctx(e, &c)?;
    let cx = super::charges::ctx(e, &c)?;
    let shown = super::system::Shown::new(e, car, None)?;
    // from the drive or charge before the period: the stretch after it may reach in
    let from = super::parked::busy_start_before(e, car, &shown, a)?.unwrap_or(a).min(a);
    let per = Period::new(Some(from), Some(b));
    // the drives as the drives list shows them: with what the chosen import fills ([`super::gapfill`])
    let gaps = super::gapfill::Gaps::new(e, car, None)?;
    let drive_row = |d: &Drive| -> QResult<Row> {
        let (v, _) = super::drives::view_filled(&c, d, &dx, &gaps)?;
        Ok(Row {
            id: format!("drive:{}", d.start_ms),
            kind: "drive",
            start_ms: d.start_ms,
            end_ms: v.end_ms,
            start: Some(point(&v.start)),
            end: v.end_ms.map(|_| point(&v.end)),
            odometer_km: v.end_odometer_km,
            distance_km: v.distance_km,
            duration_ms: v.duration_ms,
            soc_end_pct: v.end_soc_pct,
            soc_delta_pct: r(v.end_soc_pct.zip(v.start_soc_pct).map(|(x, y)| x - y), 1),
            range_end_km: v.end_range_km,
            range_delta_km: r(v.end_range_km.zip(v.start_range_km).map(|(x, y)| x - y), 1),
            energy_delta_kwh: v.energy_kwh.map(|k| -k),
            energy_basis: Some(v.energy_basis),
            outside_avg_c: v.outside_avg_c,
            cost: None,
            reference: Some(serde_json::json!({ "drive": v.id })),
            parked_ms: None,
            awake_reasons: Vec::new(),
            gap_reason: None,
            from: v.from.clone(),
            value_sources: v.value_sources.clone(),
            flags: serde_json::to_value(&v.flags).ok(),
            version: None,
            version_source: None,
            detected_by: None,
            changed_modules: None,
        })
    };
    let charge_row = |ch: &Charge| -> QResult<Row> {
        let v = super::charges::view(e, &c, ch, &cx)?;
        let place = v.place.as_ref().map(|p| super::PlaceBrief { id: p.id.clone(), name: p.name.clone() });
        Ok(Row {
            id: format!("charge:{}", ch.start_ms),
            kind: "charge",
            start_ms: ch.start_ms,
            end_ms: v.end_ms,
            start: Some(Point {
                label: place.as_ref().map(|p| p.name.clone()).or(v.address.short.clone()),
                place,
                address: v.address.short.clone(),
                lat: v.position.as_ref().map(|p| p.lat),
                lon: v.position.as_ref().map(|p| p.lon),
            }),
            end: None,
            odometer_km: v.odometer_km,
            distance_km: None,
            duration_ms: v.charging_ms,
            soc_end_pct: v.end_soc_pct,
            soc_delta_pct: r(v.end_soc_pct.zip(v.start_soc_pct).map(|(x, y)| x - y), 1),
            range_end_km: v.end_range_km,
            range_delta_km: v.range_added_km,
            energy_delta_kwh: v.energy_added_kwh,
            energy_basis: Some(
                if ch.source == Source::Live && (ch.inputs.is_empty() || ch.inputs.contains(model::Input::Smo)) {
                    "measured"
                } else {
                    "estimated"
                },
            ),
            outside_avg_c: v.outside_avg_c,
            cost: v.cost.clone(),
            reference: Some(serde_json::json!({ "charge": v.id })),
            parked_ms: None,
            awake_reasons: vec!["charging"],
            gap_reason: None,
            from: v.from.clone(),
            value_sources: Default::default(),
            flags: serde_json::to_value(&v.flags).ok(),
            version: None,
            version_source: None,
            detected_by: None,
            changed_modules: None,
        })
    };
    let mut pieces: Vec<Piece> = Vec::new();
    for d in super::drives::rows(e, car, &per, None)? {
        pieces.push((d.start_ms, d.end_ms, drive_row(&d)?));
    }
    for ch in super::charges::rows(e, car, &per, None)? {
        pieces.push((ch.start_ms, ch.end_ms, charge_row(&ch)?));
    }
    // the first one after the period ends the stretch the period's last one starts
    let next = match super::parked::next_busy(e, car, &shown, b)? {
        Some(Ok(d)) => Some((d.start_ms, d.end_ms, drive_row(&d)?)),
        Some(Err(ch)) => Some((ch.start_ms, ch.end_ms, charge_row(&ch)?)),
        None => None,
    };
    pieces.sort_by_key(|p| p.0);
    let has_next = next.is_some();
    pieces.extend(next);
    // the stretches between them; after the last, to now (unless it is still going)
    let mut between: Vec<(u64, u64, usize, Option<usize>)> = Vec::new();
    let mut prev: Option<(usize, u64)> = None;
    for (i, p) in pieces.iter().enumerate() {
        if let Some((j, end)) = prev
            && p.0 > end
        {
            between.push((end, p.0, j, Some(i)));
        }
        if prev.is_none_or(|(_, end)| p.1 >= end) {
            prev = Some((i, p.1));
        }
    }
    // the newest data: what follows it up to now is unknown
    let data_end = newest_spans(e, car, 1)?.first().map(|s| s.end_ms);
    if !has_next && let Some((j, end)) = prev {
        if pieces[j].2.end_ms.is_some() {
            if end < now {
                between.push((end, now, j, None));
            }
        } else if let Some(t) = data_end.map(|t| t.max(end)).filter(|&t| now.saturating_sub(t) >= GAP_MS) {
            // the drive or charge still going as far as the car's last data says, and the car silent since: a gap
            // from its last data to now
            between.push((t, now, j, None));
        }
    }
    between.retain(|&(x, y, _, _)| y > a && x < b);
    let mut out = Vec::new();
    if let (Some(lo), Some(hi)) = (between.iter().map(|s| s.0).min(), between.iter().map(|s| s.1).max()) {
        // the stretches' state spans, and the unknown time in them: unknown spans, and what follows the newest data
        let mut states: Vec<StateSpan> = Vec::new();
        each_span(e, car, lo.saturating_sub(1), Some(Source::Live), |s| {
            if s.start_ms >= hi {
                return Ok(false);
            }
            if s.end_ms > lo {
                states.push(s);
            }
            Ok(true)
        })?;
        let mut unknowns: Vec<(u64, u64, Option<&'static str>)> =
            states.iter().filter(|s| s.state == State::Unknown).map(|s| (s.start_ms, s.end_ms, None)).collect();
        if let Some(t) = data_end.filter(|&t| t < now) {
            // no log has come since the newest data
            unknowns.push((t, now, Some("no_log")));
        }
        let now_soc = db.entry(car)?.and_then(|x| x.summary.now).map(|n| (n.sample.soc, n.sample.range_km));
        // why the unknown spans are unknown, read once the first one is met
        let mut why_cache: Option<QResult<Vec<Why>>> = None;
        let mut why = |ga: u64, gb: u64| {
            let w = why_cache.get_or_insert_with(|| why_unknown(e, car, lo, hi));
            match w {
                Ok(w) => classify(w, ga, gb).0,
                Err(_) => "no_log",
            }
        };
        for &(x, y, j, k) in &between {
            let inside: Vec<(u64, u64, Option<&'static str>)> =
                unknowns.iter().copied().filter(|&(p, q, _)| q > x && p < y).collect();
            stretch_rows(
                x,
                y,
                &pieces[j].2,
                k.map(|k| &pieces[k].2),
                &inside,
                &states,
                &mut why,
                now_soc.unwrap_or_default(),
                &mut out,
            );
        }
    }
    out.extend(pieces.into_iter().map(|p| p.2));
    let fw = crate::firmware::history(db, car)?;
    // (an update both imports have is one row, as the updates list shows it)
    let listed: std::collections::HashSet<u64> =
        super::software::shown_updates(e, car, None)?.iter().map(|u| u.id).collect();
    for u in all_rows::<UpdateRow>(db, car, a, b, None)?.into_iter().filter(|u| listed.contains(&u.id)) {
        let up = &u.update;
        let ev = fw.events.iter().find(|ev| crate::firmware::is_fingerprint(u.id) && ev.id == u.id);
        let fw_name = match ev {
            Some(ev) => crate::firmware::name_of(db, car, ev)?,
            None => None,
        };
        out.push(Row {
            id: format!("update:{}", up.start_ms),
            kind: "update",
            start_ms: up.start_ms,
            end_ms: up.end_ms,
            start: None,
            end: None,
            odometer_km: None,
            distance_km: None,
            duration_ms: up.end_ms.unwrap_or(up.start_ms).saturating_sub(up.start_ms),
            soc_end_pct: None,
            soc_delta_pct: None,
            range_end_km: None,
            range_delta_km: None,
            energy_delta_kwh: None,
            energy_basis: None,
            outside_avg_c: None,
            cost: None,
            reference: Some(serde_json::json!({ "update": u.id.to_string() })),
            parked_ms: None,
            awake_reasons: Vec::new(),
            gap_reason: None,
            from: super::Sources::of(up.source, up.inputs),
            value_sources: Default::default(),
            flags: None,
            version: match ev {
                Some(_) => fw_name.as_ref().map(|n| n.0.clone()),
                None => Some(up.version.clone()).filter(|v| !v.is_empty()),
            },
            version_source: match (ev, &fw_name) {
                (Some(_), Some((_, "auto"))) => Some("build_match"),
                (Some(_), Some(_)) => Some("owner"),
                (Some(_), None) => None,
                (None, _) => up.source.site().map(model::ImportSite::word),
            },
            detected_by: ev.map(|_| "fingerprint"),
            changed_modules: ev.map(super::firmware::modules_of),
        });
    }
    out.retain(|row| row.start_ms >= a && row.start_ms < b);
    out.sort_by_key(|row| row.start_ms);
    Ok(out)
}

/// The energy the pack lost over each parked row (negative, `energy_basis` `measured` from its own counter or
/// energy remaining, `estimated` from the range lost), as `/parked` measures a spell.
fn fill_energy(e: &Engine, car: CarId, rated_wh_km: Option<f64>, rows: &mut [Row]) -> QResult<()> {
    let now = crate::now_ms();
    let mut sampler: Option<super::parked::Sampler<'_>> = None;
    for row in rows.iter_mut().filter(|r| r.kind == "parked") {
        let (x, y) = (row.start_ms, row.end_ms.unwrap_or(now));
        let s = match sampler.as_mut() {
            Some(s) => s,
            None => sampler.insert(super::parked::Sampler::new(e, car, Source::Live, &super::parked::END_FIELDS)?),
        };
        let first = s.at(x, x.saturating_sub(super::parked::EDGE_MS), y)?;
        let last = s.at(y, x, y.saturating_add(super::parked::EDGE_MS))?;
        let range1 = row.range_end_km;
        let range0 = range1.zip(row.range_delta_km).map(|(e, d)| e - d);
        let (k, method) = super::parked::energy_of(Some(&first), Some(&last), range0, range1, rated_wh_km);
        row.energy_delta_kwh = r(k.map(|k| -k), 3);
        row.energy_basis = k.map(|_| if method == "range" { "estimated" } else { "measured" });
    }
    Ok(())
}

/// Splits `kinds=drive,charge` (spaces and empty parts are let be).
fn parse_kinds(k: Option<&str>) -> QResult<Vec<&'static str>> {
    let parts: Vec<&str> = k.unwrap_or("").split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Ok(KINDS.to_vec());
    }
    let mut out = Vec::new();
    for p in parts {
        let Some(k) = KINDS.iter().find(|k| **k == p) else {
            return Err(QueryError::bad("kinds", &format!("{p}: drive, charge, parked, gap or update")));
        };
        if !out.contains(k) {
            out.push(*k);
        }
    }
    Ok(out)
}

/// How long a stretch of time the timeline reads at first for a page; it doubles while the page is not full.
const WINDOW_MS: u64 = 3 * 86_400_000;

/// `/cars/{car}/timeline`, newest first. A page reads the history back from its cursor a window at a time (each
/// window's rows whole, as [`all_rows_of`] makes them), and stops once the page is full: never the whole period.
pub fn timeline(e: &Engine, car: CarId, q: &TimelineQuery, paging: &Paging) -> QResult<Scoped<List<Row>>> {
    let c = car_of(e, car)?;
    let (a, b) = q.period.bounds()?;
    let kinds = parse_kinds(q.kinds.as_deref())?;
    let place = match q.place.as_deref().filter(|p| !p.is_empty()) {
        Some(p) => Some(
            parse_place(p)
                .filter(|&id| e.places().get(id).is_some())
                .ok_or_else(|| QueryError::bad("place", "no such place"))?,
        ),
        None => None,
    };
    let needle = q.q.as_deref().map(str::to_lowercase).filter(|s| !s.is_empty());
    let sig = format!("timeline|{car}|{}|{:?}|{:?}|{:?}", q.period.sig(), q.kinds, q.place, q.q);
    let limit = paging.limit()?;
    let after = super::read_after(paging, &sig)?;
    let key = |r: &Row| (r.start_ms as f64, r.id.clone());
    let keep = |r: &Row| {
        kinds.contains(&r.kind)
            && place.is_none_or(|id| {
                [&r.start, &r.end]
                    .into_iter()
                    .flatten()
                    .any(|p| p.place.as_ref().and_then(|x| parse_place(&x.id)) == Some(id))
            })
            && needle.as_ref().is_none_or(|n| words(r).contains(n.as_str()))
    };
    let mut rows: Vec<Row> = Vec::new();
    if let Some(lo) = oldest_ms(e, car)?.map(|t| t.max(a)) {
        // nothing starts after now
        let top = b.min(crate::now_ms().saturating_add(1));
        let mut hi = after.as_ref().map_or(top, |(k, _)| (*k as u64).saturating_add(1).min(top));
        let mut w = WINDOW_MS;
        while hi > lo && rows.len() <= limit {
            let wa = hi.saturating_sub(w).max(lo);
            let mut got: Vec<Row> = all_rows_of(e, car, wa, hi)?
                .into_iter()
                .filter(|r| keep(r))
                .filter(|r| {
                    after.as_ref().is_none_or(|at| super::cmp_key(&key(r), at, true) == std::cmp::Ordering::Greater)
                })
                .collect();
            got.sort_by(|x, y| super::cmp_key(&key(x), &key(y), true));
            rows.extend(got);
            hi = wa;
            w = w.saturating_mul(2);
        }
    }
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = match rows.last() {
        Some(last) if more => Some(super::make_after(&key(last), &sig)?),
        _ => None,
    };
    fill_energy(e, car, c.efficiency_wh_km, &mut rows)?;
    Ok(scoped(&c, List { items: rows, next, total: None }))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StateItem {
    pub id: String,
    /// `online`, `asleep`, `unknown`.
    pub state: &'static str,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub buses: Vec<&'static str>,
    #[serde(flatten)]
    pub from: super::Sources,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Current {
    pub state: &'static str,
    pub since_ms: Option<u64>,
    pub at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Shares {
    pub parked: Option<f64>,
    pub asleep: Option<f64>,
    pub awake: Option<f64>,
    pub unknown: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct States {
    pub items: Vec<StateItem>,
    pub next: Option<String>,
    pub current: Current,
    pub last_change_ms: Option<u64>,
    pub share_pct: Shares,
}

pub(crate) fn state_word(s: State) -> &'static str {
    match s {
        State::Online => "online",
        State::Asleep => "asleep",
        State::Unknown => "unknown",
    }
}

fn bus_words(b: &[String]) -> Vec<&'static str> {
    let mut v = Vec::new();
    if b.iter().any(|x| x == "can0") {
        v.push("vehicle");
    }
    if b.iter().any(|x| x == "can1") {
        v.push("chassis");
    }
    v
}

/// The state spans of a car, joined where the same state and buses touch (the timeline splits them at idle cut
/// points).
pub(crate) fn joined(spans: Vec<StateSpan>) -> Vec<StateSpan> {
    let mut out: Vec<StateSpan> = Vec::new();
    for s in spans {
        match out.last_mut() {
            Some(l)
                if l.end_ms == s.start_ms
                    && l.state == s.state
                    && l.buses == s.buses
                    && l.source == s.source
                    && l.inputs == s.inputs =>
            {
                l.end_ms = s.end_ms;
            }
            _ => out.push(s),
        }
    }
    out
}

/// `/cars/{car}/states`, newest first. A span belongs to the period by its start and keeps its own start, end and
/// id (the stored rows are joined across the period's edges); the shares are of the period, clipped. `current` and
/// `last_change_ms` are the car's newest span, whatever the period; only that span has `end_ms` null. The spans
/// stream past (a page and the sums are kept, never the period's rows).
pub fn states(e: &Engine, car: CarId, period: &Period, paging: &Paging) -> QResult<Scoped<States>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let sig = format!("states|{car}|{}", period.sig());
    let limit = paging.limit()?;
    let after = super::read_after(paging, &sig)?;
    let now = e.db().entry(car)?.and_then(|x| x.summary.now);
    let current = newest_spans(e, car, 1)?.into_iter().next();
    let b_eff = b.min(crate::now_ms());
    let (mut known, mut driving, mut asleep, mut online) = (0u64, 0u64, 0u64, 0u64);
    let mut first: Option<u64> = None;
    let mut best = super::drives::Best::new(limit + 1, true, after);
    if a < b {
        each_span(e, car, a.saturating_sub(1), Some(Source::Live), |s| {
            if s.start_ms >= b {
                return Ok(false);
            }
            let (x, y) = (s.start_ms.max(a), s.end_ms.min(b_eff));
            if y > x {
                first.get_or_insert(x);
                match s.state {
                    State::Asleep => {
                        asleep += y - x;
                        known += y - x;
                    }
                    State::Online => {
                        online += y - x;
                        known += y - x;
                    }
                    State::Unknown => {}
                }
            }
            if s.start_ms >= a {
                let going = now.is_some() && current.as_ref().is_some_and(|l| l.start_ms == s.start_ms);
                let item = StateItem {
                    id: s.start_ms.to_string(),
                    state: state_word(s.state),
                    start_ms: s.start_ms,
                    end_ms: (!going).then_some(s.end_ms),
                    buses: bus_words(&s.buses),
                    from: super::Sources::of(s.source, s.inputs),
                };
                best.offer((s.start_ms as f64, item.id.clone()), item);
            }
            Ok(true)
        })?;
        for d in super::drives::rows(e, car, period, Some(super::SourceFilter::Live))? {
            driving += d.end_ms.min(b_eff).saturating_sub(d.start_ms.max(a));
        }
    }
    let (items, next) = super::drives::page_of(best, limit, &sig)?;
    let whole = b_eff.saturating_sub(first.unwrap_or(a).max(a));
    let pct = |x: u64, of: u64| r((of > 0).then(|| x as f64 / of as f64 * 100.0), 1);
    Ok(scoped(
        &c,
        States {
            items,
            next,
            current: Current {
                state: current.as_ref().map_or("unknown", |s| state_word(s.state)),
                since_ms: current.as_ref().map(|s| s.start_ms),
                at_ms: now.map(|n| n.at_ms),
            },
            last_change_ms: current.as_ref().map(|s| s.start_ms),
            share_pct: Shares {
                parked: pct(known.saturating_sub(driving), known),
                asleep: pct(asleep, whole),
                awake: pct(online, whole),
                unknown: pct(whole.saturating_sub(known), whole),
            },
        },
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Segment {
    /// `driving`, `charging`, `online`, `online_vehicle_bus_only`, `asleep`, `unknown`, `updating`.
    pub kind: &'static str,
    pub start_ms: u64,
    pub end_ms: u64,
    /// `{ drive: id }`, `{ charge: id }` or `{ update: id }`; null for the states between them.
    #[serde(rename = "ref")]
    pub reference: Option<serde_json::Value>,
    #[serde(flatten)]
    pub from: super::Sources,
}

/// The strip's segments of a period, and the period they cover (`to_ms` no later than now).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Activity {
    pub from_ms: u64,
    pub to_ms: u64,
    pub items: Vec<Segment>,
    /// Always null: the strip is one list.
    pub next: Option<String>,
}

/// The strip's segments of a period, clipped to it (default the last 24 hours) and to now, every one of them (what
/// a trip's time in each state is summed from). Where CAN and imported state spans overlap, the side shown for that
/// time is used, as the drive and charge lists choose.
pub fn activity(e: &Engine, car: CarId, period: &Period) -> QResult<Scoped<Activity>> {
    let c = car_of(e, car)?;
    let now = crate::now_ms();
    let (a, b) = period.window(86_400_000, now)?;
    let b = b.min(now).max(a);
    let mut busy: Vec<Segment> = Vec::new();
    for d in super::drives::rows(e, car, &Period::new(Some(a.saturating_sub(86_400_000)), Some(b)), None)? {
        if d.end_ms > a {
            busy.push(Segment {
                kind: "driving",
                start_ms: d.start_ms.max(a),
                end_ms: d.end_ms.min(b),
                reference: Some(serde_json::json!({ "drive": d.id.to_string() })),
                from: super::Sources::of(d.source, d.inputs),
            });
        }
    }
    for ch in super::charges::rows(e, car, &Period::new(Some(a.saturating_sub(7 * 86_400_000)), Some(b)), None)? {
        if ch.end_ms > a {
            busy.push(Segment {
                kind: "charging",
                start_ms: ch.start_ms.max(a),
                end_ms: ch.end_ms.min(b),
                reference: Some(serde_json::json!({ "charge": ch.id.to_string() })),
                from: super::Sources::of(ch.source, ch.inputs),
            });
        }
    }
    let listed: std::collections::HashSet<u64> =
        super::software::shown_updates(e, car, None)?.iter().map(|u| u.id).collect();
    let updates = all_rows::<UpdateRow>(e.db(), car, a.saturating_sub(86_400_000), b, None)?;
    for u in updates.into_iter().filter(|u| listed.contains(&u.id)) {
        let end = u.update.end_ms.unwrap_or(b);
        if end > a {
            busy.push(Segment {
                kind: "updating",
                start_ms: u.update.start_ms.max(a),
                end_ms: end.min(b),
                reference: Some(serde_json::json!({ "update": u.id.to_string() })),
                from: super::Sources::of(u.update.source, u.update.inputs),
            });
        }
    }
    busy.sort_by_key(|s| s.start_ms);
    let mut out = Vec::new();
    let shown = super::system::Shown::new(e, car, None)?;
    let mut spans: Vec<StateSpan> = Vec::new();
    each_span(e, car, a, None, |s| {
        if s.start_ms >= b {
            return Ok(false);
        }
        if s.end_ms > a && shown.keep(s.source, s.inputs, s.start_ms.max(a)) {
            spans.push(s);
        }
        Ok(true)
    })?;
    for s in spans {
        let kind = match s.state {
            State::Asleep => "asleep",
            State::Unknown => "unknown",
            State::Online if s.buses.iter().any(|x| x == "can1") || s.source.is_imported() => "online",
            State::Online => "online_vehicle_bus_only",
        };
        // the span with the drives and charges taken out
        let mut cur = s.start_ms.max(a);
        let end = s.end_ms.min(b);
        let start = cur;
        for x in busy.iter().filter(|x| x.end_ms > start && x.start_ms < end) {
            if x.end_ms <= cur {
                continue;
            }
            if x.start_ms > cur {
                out.push(Segment {
                    kind,
                    start_ms: cur,
                    end_ms: x.start_ms,
                    reference: None,
                    from: super::Sources::of(s.source, s.inputs),
                });
            }
            cur = cur.max(x.end_ms);
        }
        if end > cur {
            out.push(Segment {
                kind,
                start_ms: cur,
                end_ms: end,
                reference: None,
                from: super::Sources::of(s.source, s.inputs),
            });
        }
    }
    out.extend(busy.into_iter().filter(|s| s.end_ms > s.start_ms));
    out.sort_by_key(|s| s.start_ms);
    Ok(scoped(&c, Activity { from_ms: a, to_ms: b, items: out, next: None }))
}

/// How many segments `/activity` answers at most (about one per pixel of a wide strip).
pub const STRIP_SEGMENTS: u64 = 1500;

/// `/cars/{car}/activity` for the strip: [`activity`] with segments shorter than the period's `1/max` folded into
/// the one before them (or after, at the start), so the answer stays near `max` segments however long the period.
pub fn activity_strip(e: &Engine, car: CarId, period: &Period, max: u64) -> QResult<Scoped<Activity>> {
    let mut act = activity(e, car, period)?;
    let min = (act.body.to_ms.saturating_sub(act.body.from_ms) / max.max(1)).max(1);
    act.body.items = fold(std::mem::take(&mut act.body.items), min);
    Ok(act)
}

/// Folds segments shorter than `min` into their neighbour, and joins touching segments of one kind with no ref.
fn fold(segs: Vec<Segment>, min: u64) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    for mut s in segs {
        let len = s.end_ms.saturating_sub(s.start_ms);
        if let Some(l) = out.last_mut()
            && l.end_ms >= s.start_ms
        {
            let same = l.kind == s.kind && l.reference.is_none() && s.reference.is_none() && l.from == s.from;
            if len < min || same {
                l.end_ms = l.end_ms.max(s.end_ms);
                continue;
            }
            if l.end_ms.saturating_sub(l.start_ms) < min {
                // the one before is too short to show: this one takes its time
                s.start_ms = s.start_ms.min(l.start_ms);
                out.pop();
            }
        }
        out.push(s);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GapRow {
    pub id: String,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub duration_ms: u64,
    pub odometer_jump_km: Option<f64>,
    pub from_place: Option<super::PlaceBrief>,
    pub to_place: Option<super::PlaceBrief>,
    /// `no_log`, `not_recorded`, `chain_gap`, `unknown`.
    pub cause: &'static str,
    pub gap_reason: &'static str,
    #[serde(flatten)]
    pub from: super::Sources,
    pub inside: Option<serde_json::Value>,
}

/// A span the car's records say has no usable data, and why: the timeline's word (`failed_check`, `unreadable`,
/// `other_vin`, `dropped_frames`, `not_recorded`, `no_log`) and the gap's cause (`unknown`, `not_recorded`,
/// `chain_gap`, `no_log`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Why {
    pub from_ms: u64,
    pub to_ms: u64,
    pub reason: &'static str,
    pub cause: &'static str,
}

/// How close a reason must lie to an unknown span to be its reason (a pause ends where recording starts again; lost
/// frames sit beside the span they leave unknown).
const NEAR_MS: u64 = 2_000;

/// The timeline's word and the gap's cause for a log the records keep as a hole, from the reason the pipeline wrote
/// ([`crate::records::Outcome::Hole`]): `not_stored` is no log; a log that could not be decoded (`unreadable: ...`)
/// and one that names another car's VIN (`other_vin`) are said as that, not as a log that failed its check.
pub(crate) fn hole_why(reason: &str) -> (&'static str, &'static str) {
    match reason {
        "not_stored" => ("no_log", "no_log"),
        "other_vin" => ("other_vin", "unknown"),
        r if r.starts_with("unreadable") => ("unreadable", "unknown"),
        _ => ("failed_check", "unknown"),
    }
}

/// The reasons the records keep for `[a, b)`: logs left out (failed their check, named another VIN, could not be
/// read, or never stored), pauses in recording and lost frames of decoded logs, and gaps in each key's chain.
pub(crate) fn why_unknown(e: &Engine, car: CarId, a: u64, b: u64) -> QResult<Vec<Why>> {
    use crate::records::{self, Outcome};
    let db = e.db();
    let meta = records::meta(db, car)?;
    let mut out = Vec::new();
    for (_, en) in records::entries_between(db, car, &meta, a, b)? {
        match &en.outcome {
            Outcome::Hole { from_ms, to_ms, reason } => {
                let (reason, cause) = hole_why(reason);
                out.push(Why { from_ms: *from_ms, to_ms: *to_ms, reason, cause });
            }
            Outcome::Decoded { facts, .. } => {
                for n in &facts.not_recorded {
                    let from = n.paused_ms.map_or(n.at_ms.saturating_sub(1), |p| n.at_ms.saturating_sub(p));
                    out.push(Why { from_ms: from, to_ms: n.at_ms, reason: "not_recorded", cause: "not_recorded" });
                }
                if facts.dropped > 0 {
                    let (x, y) = facts.span();
                    out.push(Why { from_ms: x, to_ms: y, reason: "dropped_frames", cause: "unknown" });
                }
            }
        }
    }
    for (x, y) in chain_gaps(e, car)? {
        if y > a && x < b {
            out.push(Why { from_ms: x, to_ms: y, reason: "no_log", cause: "chain_gap" });
        }
    }
    Ok(out)
}

/// The spans of each key's missing chain links: from the minute of the log before the gap to the start of the log
/// after it.
/// Kept while the car's keys' records stay the same ([`ingest::Judge::changes`]).
pub(crate) fn chain_gaps(e: &Engine, car: CarId) -> QResult<Vec<(u64, u64)>> {
    let c = car_of(e, car)?;
    let rt = e.handle()?;
    let at: Vec<(&String, Option<(u64, usize)>)> =
        c.vehicle_keys.iter().map(|k| (k, rt.block_on(e.judge().changes(k)))).collect();
    let kept = e.memo(car, "chain-gaps", crate::memo::On::Imported, crate::memo::key(&at), || chain_gaps_of(e, &c))?;
    Ok(kept.as_ref().clone())
}

fn chain_gaps_of(e: &Engine, c: &model::Car) -> QResult<Vec<(u64, u64)>> {
    let rt = e.handle()?;
    let mut out = Vec::new();
    for key in &c.vehicle_keys {
        let chain = rt.block_on(e.judge().chain(key));
        let gaps: Vec<(u64, u64)> = chain
            .report
            .issues
            .iter()
            .filter_map(|i| match i {
                trust::ChainIssue::Gap { from, to, .. } => Some((*from, *to)),
                _ => None,
            })
            .collect();
        if gaps.is_empty() {
            continue;
        }
        let book = rt.block_on(e.judge().book(key));
        let at: std::collections::BTreeMap<u64, u64> = book
            .logs
            .iter()
            .filter_map(|(n, r)| Some((r.link.as_ref()?.index, crate::records::name_start_ms(n)?)))
            .collect();
        for (from, to) in gaps {
            let before = from.checked_sub(1).and_then(|i| at.get(&i)).map(|t| t + 60_000);
            let after = to.checked_add(1).and_then(|i| at.get(&i)).copied();
            if let (Some(x), Some(y)) = (before, after)
                && y > x
            {
                out.push((x, y));
            }
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// Why `[x, y)` is unknown: the timeline's word and the gap's cause. A log that failed its check comes first, then
/// one that could not be read, one that named another VIN and one that lost frames, then a gap in the chain, then a
/// pause in recording; else no log came.
pub(crate) fn classify(why: &[Why], x: u64, y: u64) -> (&'static str, &'static str) {
    let near = |w: &&Why| w.from_ms < y.saturating_add(NEAR_MS) && w.to_ms.saturating_add(NEAR_MS) > x;
    let first = |reason: &str, cause: &str| why.iter().filter(near).find(|w| w.reason == reason && w.cause == cause);
    let order = [
        ("failed_check", "unknown"),
        ("unreadable", "unknown"),
        ("other_vin", "unknown"),
        ("dropped_frames", "unknown"),
        ("no_log", "chain_gap"),
        ("not_recorded", "not_recorded"),
    ];
    for (reason, cause) in order {
        if let Some(w) = first(reason, cause) {
            return (w.reason, w.cause);
        }
    }
    ("no_log", "no_log")
}

/// `/cars/{car}/gaps`: unknown spans of `GAP_MS` or more, newest first, with how far the odometer moved across.
pub fn gaps(e: &Engine, car: CarId, period: &Period, paging: &Paging) -> QResult<Scoped<List<GapRow>>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let drives = super::drives::rows(e, car, &Period::all(), Some(super::SourceFilter::Live))?;
    let mut items = Vec::new();
    let why = why_unknown(e, car, a, b)?;
    let charges: Vec<Charge> = all_rows(e.db(), car, a.saturating_sub(7 * 86_400_000), b, Some(Source::Live))?;
    for s in joined(all_rows::<StateRow>(e.db(), car, a, b, Some(Source::Live))?.into_iter().map(|r| r.span).collect())
    {
        if s.state != State::Unknown || s.end_ms - s.start_ms < GAP_MS || s.start_ms < a {
            continue;
        }
        let (gap_reason, cause) = classify(&why, s.start_ms, s.end_ms);
        let inside = if cause == "chain_gap" {
            charges
                .iter()
                .find(|c| c.start_ms <= s.start_ms && s.end_ms <= c.end_ms)
                .map(|c| serde_json::json!({ "charge": c.id.to_string() }))
        } else {
            None
        };
        let before = drives.iter().rev().find(|d| d.end_ms <= s.start_ms);
        let after = drives.iter().find(|d| d.start_ms >= s.end_ms);
        let jump =
            before.and_then(|d| d.end_odometer_km).zip(after.and_then(|d| d.start_odometer_km)).map(|(x, y)| y - x);
        let places = e.places();
        let brief = |p: Option<&model::Position>| {
            p.and_then(|p| places.place_at(p.lat, p.lon))
                .map(|pl| super::PlaceBrief { id: super::place_ref(pl.id), name: pl.name.clone() })
        };
        items.push(GapRow {
            id: format!("gap:{}", s.start_ms),
            start_ms: s.start_ms,
            end_ms: Some(s.end_ms),
            duration_ms: s.end_ms - s.start_ms,
            odometer_jump_km: r(jump.filter(|j| *j > 0.05), 3),
            from_place: brief(before.and_then(|d| d.end.as_ref())),
            to_place: brief(after.and_then(|d| d.start.as_ref())),
            cause,
            gap_reason,
            from: super::Sources::family(model::Source::Live),
            inside,
        });
    }
    let sig = format!("gaps|{car}|{}", period.sig());
    Ok(scoped(&c, paginate(items, |g| (g.start_ms as f64, g.id.clone()), true, &sig, paging, false)?))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn seg(kind: &'static str, a: u64, b: u64) -> Segment {
        Segment {
            kind,
            start_ms: a,
            end_ms: b,
            reference: None,
            from: crate::query::Sources::family(model::Source::Live),
        }
    }

    #[test]
    fn short_segments_fold_into_their_neighbours() {
        let segs = vec![
            seg("asleep", 0, 100),
            seg("online", 100, 102),
            seg("asleep", 102, 200),
            seg("unknown", 200, 201),
            seg("online", 201, 300),
        ];
        let out = fold(segs, 10);
        let got: Vec<(&str, u64, u64)> = out.iter().map(|s| (s.kind, s.start_ms, s.end_ms)).collect();
        assert_eq!(got, vec![("asleep", 0, 201), ("online", 201, 300)]);
        // a short first one is taken by the one after it
        let out = fold(vec![seg("online", 0, 2), seg("asleep", 2, 100)], 10);
        assert_eq!(out.iter().map(|s| (s.kind, s.start_ms)).collect::<Vec<_>>(), vec![("asleep", 0)]);
    }

    #[test]
    fn kinds_let_spaces_and_empty_parts_be() {
        assert_eq!(parse_kinds(Some("drive, charge")).unwrap(), vec!["drive", "charge"]);
        assert_eq!(parse_kinds(Some(",drive,,drive,")).unwrap(), vec!["drive"]);
        assert_eq!(parse_kinds(Some(" , ")).unwrap(), KINDS.to_vec());
        assert_eq!(parse_kinds(None).unwrap(), KINDS.to_vec());
        assert_eq!(parse_kinds(Some("drive, walk")).unwrap_err().code(), "bad_input");
    }
}
