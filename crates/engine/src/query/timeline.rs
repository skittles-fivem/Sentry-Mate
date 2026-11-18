//! `/cars/{car}/timeline`, `/states`, `/activity`, `/gaps`.

use super::{List, Paging, Period, QResult, QueryError, Scoped, car_of, paginate, parse_place, r, scoped, source_word};
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
    /// `no_log`, `not_recorded`, `failed_check`, `dropped_frames`.
    pub gap_reason: Option<&'static str>,
    pub source: &'static str,
    pub version: Option<String>,
    pub version_source: Option<&'static str>,
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

/// Every timeline row of a car in `[a, b)` (by start), oldest first.
pub(crate) fn all_rows_of(e: &Engine, car: CarId, a: u64, b: u64) -> QResult<Vec<Row>> {
    let c = car_of(e, car)?;
    let db = e.db();
    let dx = super::drives::ctx(e, &c)?;
    let cx = super::charges::ctx(e, &c)?;
    // a little before the period, for the stretch the first row continues
    let drives: Vec<Drive> =
        super::drives::rows(e, car, &Period::new(Some(a.saturating_sub(7 * 86_400_000)), Some(b)), None)?;
    let charges: Vec<Charge> =
        super::charges::rows(e, car, &Period::new(Some(a.saturating_sub(7 * 86_400_000)), Some(b)), None)?;
    let states: Vec<StateSpan> = all_rows::<StateRow>(db, car, a.saturating_sub(7 * 86_400_000), b, Some(Source::Can))?
        .into_iter()
        .map(|s| s.span)
        .collect();
    let mut out = Vec::new();
    // drives and charges, then what lies between them
    let mut pieces: Vec<(u64, u64, Option<Row>)> = Vec::new();
    for d in &drives {
        let v = super::drives::view(&c, d, &dx);
        let row = Row {
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
            source: v.source,
            version: None,
            version_source: None,
        };
        pieces.push((d.start_ms, d.end_ms, Some(row)));
    }
    for ch in &charges {
        let v = super::charges::view(e, &c, ch, &cx)?;
        let place = v.place.as_ref().map(|p| super::PlaceBrief { id: p.id.clone(), name: p.name.clone() });
        let row = Row {
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
            energy_basis: Some(if ch.source == Source::Can { "measured" } else { "estimated" }),
            outside_avg_c: v.outside_avg_c,
            cost: v.cost.clone(),
            reference: Some(serde_json::json!({ "charge": v.id })),
            parked_ms: None,
            awake_reasons: vec!["charging"],
            gap_reason: None,
            source: v.source,
            version: None,
            version_source: None,
        };
        pieces.push((ch.start_ms, ch.end_ms, Some(row)));
    }
    pieces.sort_by_key(|p| p.0);
    // stretches between them: parked, cut at long unknown spans (gaps)
    let mut between = Vec::new();
    let mut prev: Option<&(u64, u64, Option<Row>)> = None;
    for p in &pieces {
        if let Some(q) = prev
            && p.0 > q.1
        {
            between.push((q.1, p.0, q.2.as_ref(), p.2.as_ref()));
        }
        prev = Some(p);
    }
    for (x, y, before, after) in between {
        let unknowns: Vec<(u64, u64)> = states
            .iter()
            .filter(|s| s.state == State::Unknown && s.end_ms > x && s.start_ms < y)
            .map(|s| (s.start_ms.max(x), s.end_ms.min(y)))
            .filter(|(a, b)| b - a >= GAP_MS)
            .collect();
        let mut cur = x;
        let at = before.and_then(|b| b.end.clone().or(b.start.clone()));
        let soc0 = before.and_then(|b| b.soc_end_pct);
        let soc1 = after.and_then(|b| b.soc_end_pct.zip(b.soc_delta_pct).map(|(e, d)| e - d));
        let push_parked = |from: u64, to: u64, out: &mut Vec<Row>| {
            if to <= from {
                return;
            }
            out.push(Row {
                id: format!("parked:{from}"),
                kind: "parked",
                start_ms: from,
                end_ms: Some(to),
                start: at.clone(),
                end: None,
                odometer_km: before.and_then(|b| b.odometer_km),
                distance_km: None,
                duration_ms: to - from,
                soc_end_pct: soc1,
                soc_delta_pct: r(soc1.zip(soc0).map(|(a, b)| a - b), 1),
                range_end_km: None,
                range_delta_km: None,
                energy_delta_kwh: None,
                energy_basis: None,
                outside_avg_c: None,
                cost: None,
                reference: None,
                parked_ms: Some(split(&states, from, to)),
                awake_reasons: Vec::new(),
                gap_reason: None,
                source: "can",
                version: None,
                version_source: None,
            });
        };
        for (ga, gb) in unknowns {
            push_parked(cur, ga, &mut out);
            out.push(Row {
                id: format!("gap:{ga}"),
                kind: "gap",
                start_ms: ga,
                end_ms: Some(gb),
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
                gap_reason: Some("no_log"),
                source: "can",
                version: None,
                version_source: None,
            });
            cur = gb;
        }
        push_parked(cur, y, &mut out);
    }
    out.extend(pieces.into_iter().filter_map(|p| p.2));
    for u in all_rows::<UpdateRow>(db, car, a, b, None)? {
        let up = &u.update;
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
            source: source_word(up.source),
            version: Some(up.version.clone()).filter(|v| !v.is_empty()),
            version_source: (up.source == Source::Teslamate).then_some("teslamate"),
        });
    }
    out.retain(|row| row.start_ms >= a && row.start_ms < b);
    out.sort_by_key(|row| row.start_ms);
    let _ = dx;
    Ok(out)
}

/// `/cars/{car}/timeline`, newest first.
pub fn timeline(e: &Engine, car: CarId, q: &TimelineQuery, paging: &Paging) -> QResult<Scoped<List<Row>>> {
    let c = car_of(e, car)?;
    let (a, b) = q.period.bounds()?;
    let kinds: Vec<&str> = match q.kinds.as_deref().filter(|k| !k.is_empty()) {
        None => KINDS.to_vec(),
        Some(k) => {
            let v: Vec<&str> = k.split(',').collect();
            if let Some(bad) = v.iter().find(|k| !KINDS.contains(k)) {
                return Err(QueryError::bad("kinds", &format!("{bad}: drive, charge, parked, gap or update")));
            }
            v
        }
    };
    let place = match q.place.as_deref().filter(|p| !p.is_empty()) {
        Some(p) => Some(
            parse_place(p)
                .filter(|&id| e.places().get(id).is_some())
                .ok_or_else(|| QueryError::bad("place", "no such place"))?,
        ),
        None => None,
    };
    let needle = q.q.as_deref().map(str::to_lowercase).filter(|s| !s.is_empty());
    let rows: Vec<Row> = all_rows_of(e, car, a, b)?
        .into_iter()
        .filter(|r| kinds.contains(&r.kind))
        .filter(|r| {
            place.is_none_or(|id| {
                [&r.start, &r.end]
                    .into_iter()
                    .flatten()
                    .any(|p| p.place.as_ref().and_then(|x| parse_place(&x.id)) == Some(id))
            })
        })
        .filter(|r| needle.as_ref().is_none_or(|n| words(r).contains(n.as_str())))
        .collect();
    let sig = format!("timeline|{car}|{}|{:?}|{:?}|{:?}", q.period.sig(), q.kinds, q.place, q.q);
    Ok(scoped(&c, paginate(rows, |r| (r.start_ms as f64, r.id.clone()), true, &sig, paging, false)?))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StateItem {
    pub id: String,
    /// `online`, `asleep`, `unknown`.
    pub state: &'static str,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub buses: Vec<&'static str>,
    pub source: &'static str,
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
            Some(l) if l.end_ms == s.start_ms && l.state == s.state && l.buses == s.buses && l.source == s.source => {
                l.end_ms = s.end_ms;
            }
            _ => out.push(s),
        }
    }
    out
}

/// `/cars/{car}/states`, newest first.
pub fn states(e: &Engine, car: CarId, period: &Period, paging: &Paging) -> QResult<Scoped<States>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let spans =
        joined(all_rows::<StateRow>(e.db(), car, a, b, Some(Source::Can))?.into_iter().map(|r| r.span).collect());
    let now = e.db().entry(car)?.and_then(|x| x.summary.now);
    let last = spans.last().cloned();
    let (b_eff, mut known, mut driving, mut asleep, mut online) = (b.min(crate::now_ms()), 0u64, 0u64, 0u64, 0u64);
    for s in &spans {
        let (x, y) = (s.start_ms.max(a), s.end_ms.min(b_eff));
        if y <= x {
            continue;
        }
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
    for d in super::drives::rows(e, car, period, Some(Source::Can))? {
        driving += d.end_ms.min(b_eff).saturating_sub(d.start_ms.max(a));
    }
    let whole = b_eff.saturating_sub(a.max(spans.first().map_or(a, |s| s.start_ms)));
    let pct = |x: u64, of: u64| r((of > 0).then(|| x as f64 / of as f64 * 100.0), 1);
    let items: Vec<StateItem> = spans
        .iter()
        .map(|s| StateItem {
            id: s.start_ms.to_string(),
            state: state_word(s.state),
            start_ms: s.start_ms,
            end_ms: if last.as_ref().is_some_and(|l| l.start_ms == s.start_ms) && now.is_some() {
                None
            } else {
                Some(s.end_ms)
            },
            buses: bus_words(&s.buses),
            source: source_word(s.source),
        })
        .collect();
    let sig = format!("states|{car}|{}", period.sig());
    let page = paginate(items, |s| (s.start_ms as f64, s.id.clone()), true, &sig, paging, false)?;
    Ok(scoped(
        &c,
        States {
            items: page.items,
            next: page.next,
            current: Current {
                state: last.as_ref().map_or("unknown", |s| state_word(s.state)),
                since_ms: last.as_ref().map(|s| s.start_ms),
                at_ms: now.map(|n| n.at_ms),
            },
            last_change_ms: last.as_ref().map(|s| s.start_ms),
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
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub source: &'static str,
}

/// `/cars/{car}/activity`: segments for the strip, clipped to the period (default the last 24 hours).
pub fn activity(e: &Engine, car: CarId, period: &Period) -> QResult<Scoped<Vec<Segment>>> {
    let c = car_of(e, car)?;
    let (a, b) = period.window(86_400_000, crate::now_ms())?;
    let mut busy: Vec<Segment> = Vec::new();
    for d in super::drives::rows(e, car, &Period::new(Some(a.saturating_sub(86_400_000)), Some(b)), None)? {
        if d.end_ms > a {
            busy.push(Segment {
                kind: "driving",
                start_ms: d.start_ms.max(a),
                end_ms: d.end_ms.min(b),
                reference: Some(d.id.to_string()),
                source: source_word(d.source),
            });
        }
    }
    for ch in super::charges::rows(e, car, &Period::new(Some(a.saturating_sub(7 * 86_400_000)), Some(b)), None)? {
        if ch.end_ms > a {
            busy.push(Segment {
                kind: "charging",
                start_ms: ch.start_ms.max(a),
                end_ms: ch.end_ms.min(b),
                reference: Some(ch.id.to_string()),
                source: source_word(ch.source),
            });
        }
    }
    busy.sort_by_key(|s| s.start_ms);
    let mut out = Vec::new();
    for s in joined(all_rows::<StateRow>(e.db(), car, a, b, None)?.into_iter().map(|r| r.span).collect()) {
        let kind = match s.state {
            State::Asleep => "asleep",
            State::Unknown => "unknown",
            State::Online if s.buses.iter().any(|x| x == "can1") || s.source == Source::Teslamate => "online",
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
                    source: source_word(s.source),
                });
            }
            cur = cur.max(x.end_ms);
        }
        if end > cur {
            out.push(Segment { kind, start_ms: cur, end_ms: end, reference: None, source: source_word(s.source) });
        }
    }
    out.extend(busy.into_iter().filter(|s| s.end_ms > s.start_ms));
    out.sort_by_key(|s| s.start_ms);
    Ok(scoped(&c, out))
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
    pub source: &'static str,
    pub inside: Option<serde_json::Value>,
}

/// `/cars/{car}/gaps`: unknown spans of `GAP_MS` or more, newest first, with how far the odometer moved across.
pub fn gaps(e: &Engine, car: CarId, period: &Period, paging: &Paging) -> QResult<Scoped<List<GapRow>>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let drives = super::drives::rows(e, car, &Period::all(), Some(Source::Can))?;
    let mut items = Vec::new();
    for s in joined(all_rows::<StateRow>(e.db(), car, a, b, Some(Source::Can))?.into_iter().map(|r| r.span).collect()) {
        if s.state != State::Unknown || s.end_ms - s.start_ms < GAP_MS || s.start_ms < a {
            continue;
        }
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
            cause: "no_log",
            gap_reason: "no_log",
            source: "can",
            inside: None,
        });
    }
    let sig = format!("gaps|{car}|{}", period.sig());
    Ok(scoped(&c, paginate(items, |g| (g.start_ms as f64, g.id.clone()), true, &sig, paging, false)?))
}
