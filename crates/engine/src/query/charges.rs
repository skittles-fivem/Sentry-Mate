//! `/charges` (one car or several), a charge, its curve, `/charging-stats`.

use super::now::kind_word;
use super::system::Shown;
use super::{
    Address, CarRef, List, Money, Paging, Period, PlaceBrief, QResult, QueryError, Scoped, car_of, money, paginate,
    place_ref, r, scoped, source_word,
};
use crate::pipeline::all_rows;
use crate::records::{self, CostEntry};
use crate::{Engine, costs};
use model::{Car, CarId, Charge, ChargePoint, Source};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChargePlace {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Position {
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cost {
    pub amount: f64,
    pub currency: String,
    /// `tariff`, `manual`, `free_supercharging`, `teslamate`.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PricePerKwh {
    pub value: f64,
    /// `grid` (over the grid energy) or `pack` (over the energy into the pack).
    pub basis: &'static str,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ChargeFlags {
    pub in_progress: bool,
    pub gap_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChargeView {
    pub id: String,
    pub car: CarRef,
    pub time_zone: String,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub charging_ms: u64,
    pub plugged_ms: u64,
    /// `ac`, `dc` or null.
    pub kind: Option<&'static str>,
    pub fast_charger: Option<serde_json::Value>,
    pub place: Option<ChargePlace>,
    pub address: Address,
    pub position: Option<Position>,
    pub odometer_km: Option<f64>,
    pub energy_added_kwh: Option<f64>,
    pub energy_grid_kwh: Option<f64>,
    pub dc_inlet_kwh: Option<f64>,
    pub losses_kwh: Option<f64>,
    pub efficiency_pct: Option<f64>,
    pub cost: Option<Cost>,
    pub price_per_kwh: Option<PricePerKwh>,
    pub start_soc_pct: Option<f64>,
    pub end_soc_pct: Option<f64>,
    pub start_range_km: Option<f64>,
    pub end_range_km: Option<f64>,
    pub range_added_km: Option<f64>,
    pub range_per_hour_km: Option<f64>,
    pub power_max_kw: Option<f64>,
    pub power_avg_kw: Option<f64>,
    pub outside_avg_c: Option<f64>,
    pub battery_avg_c: Option<f64>,
    pub source: &'static str,
    pub has_gaps: bool,
    pub flags: ChargeFlags,
}

pub(crate) struct Ctx {
    pub places: std::sync::Arc<crate::placing::Places>,
    pub live: Option<u64>,
}

pub(crate) fn ctx(e: &Engine, c: &Car) -> QResult<Ctx> {
    let live =
        e.db().entry(c.id)?.and_then(|x| x.summary.now).filter(|n| n.sample.charging == Some(true)).and_then(|n| {
            e.db()
                .list::<Charge>(c.id, &db::Query { source: Some(Source::Can), ..db::Query::default().limit(1) })
                .ok()
                .and_then(|p| p.items.into_iter().next())
                .filter(|ch| ch.end_ms + 300_000 >= n.at_ms)
                .map(|ch| ch.id)
        });
    Ok(Ctx { places: e.places(), live })
}

/// The cost a charge shows: kept, else from the place's price now (a charge still going), else the imported one.
pub(crate) fn cost_of(e: &Engine, c: &Car, ch: &Charge) -> QResult<Option<CostEntry>> {
    if let Some(k) = records::cost(e.db(), c.id, ch.id, ch.start_ms)? {
        return Ok(Some(k));
    }
    if ch.source == Source::Can {
        return Ok(costs::price_now(e, ch, &[], &c.time_zone));
    }
    Ok(None)
}

pub(crate) fn view(e: &Engine, c: &Car, ch: &Charge, x: &Ctx) -> QResult<ChargeView> {
    let live = x.live == Some(ch.id);
    let ms = ch.end_ms.saturating_sub(ch.start_ms);
    let place = ch.position.as_ref().and_then(|p| x.places.place_at(p.lat, p.lon));
    let kept = cost_of(e, c, ch)?;
    let teslamate_cost = match (&kept, ch.source, ch.cost) {
        (None, Source::Teslamate, Some(amount)) => {
            Some(Cost { amount, currency: String::new(), source: "teslamate".into() })
        }
        _ => None,
    };
    let cost = kept
        .as_ref()
        .map(|k| Cost { amount: k.amount, currency: k.currency.clone(), source: k.source.clone() })
        .or(teslamate_cost);
    let grid = ch.energy_used_kwh;
    let added = ch.energy_added_kwh;
    let price_per_kwh = cost.as_ref().and_then(|cst| {
        let (basis, kwh) = match grid {
            Some(g) => ("grid", g),
            None => ("pack", added?),
        };
        let value = match &kept {
            Some(k) if k.mode.as_deref() == Some("per_kwh") => k.entered?,
            _ if kwh > 0.0 => (cst.amount / kwh * 10_000.0).round() / 10_000.0,
            _ => return None,
        };
        Some(PricePerKwh { value, basis })
    });
    let hours = ms as f64 / 3_600_000.0;
    let range_added = ch.end_range_km.zip(ch.start_range_km).map(|(b, a)| b - a);
    let dc = ch.kind == Some(model::ChargeKind::Dc);
    Ok(ChargeView {
        id: ch.id.to_string(),
        car: CarRef(c.id),
        time_zone: c.time_zone.clone(),
        start_ms: ch.start_ms,
        end_ms: (!live).then_some(ch.end_ms),
        charging_ms: ms,
        plugged_ms: ms,
        kind: ch.kind.map(kind_word),
        fast_charger: dc.then(|| serde_json::json!({ "type": null, "brand": null, "class": null, "billed_kwh": null })),
        place: place.map(|p| ChargePlace { id: place_ref(p.id), name: p.name.clone(), kind: x.places.kind_word(p) }),
        address: Address { short: ch.address.clone(), ..Address::default() },
        position: ch.position.as_ref().map(|p| Position {
            lat: r(Some(p.lat), 6).unwrap_or(p.lat),
            lon: r(Some(p.lon), 6).unwrap_or(p.lon),
            accuracy_m: None,
        }),
        odometer_km: r(ch.position.as_ref().and_then(|p| p.odometer_km), 3),
        energy_added_kwh: r(added, 3),
        energy_grid_kwh: r(grid, 3),
        dc_inlet_kwh: None,
        losses_kwh: r(grid.zip(added).map(|(g, a)| g - a), 3),
        efficiency_pct: r(grid.zip(added).filter(|(g, _)| *g > 0.0).map(|(g, a)| a / g * 100.0), 1),
        cost,
        price_per_kwh,
        start_soc_pct: r(ch.start_soc, 1),
        end_soc_pct: r(ch.end_soc, 1),
        start_range_km: r(ch.start_range_km, 1),
        end_range_km: r(ch.end_range_km, 1),
        range_added_km: r(range_added, 1),
        range_per_hour_km: r(range_added.filter(|_| hours > 0.01).map(|ra| ra / hours), 1),
        power_max_kw: r(ch.power_max_kw, 1),
        power_avg_kw: r(added.filter(|_| hours > 0.001).map(|a| a / hours), 1),
        outside_avg_c: r(ch.outside_temp_avg_c, 1),
        battery_avg_c: r(ch.battery_temp_avg_c, 1),
        source: source_word(ch.source),
        has_gaps: false,
        flags: ChargeFlags { in_progress: live, gap_ms: 0 },
    })
}

/// The charges of a car in a period (by start), as shown.
pub(crate) fn rows(e: &Engine, car: CarId, period: &Period, source: Option<Source>) -> QResult<Vec<Charge>> {
    let (a, b) = period.bounds()?;
    let all: Vec<Charge> = all_rows(e.db(), car, a, b, source)?;
    let shown = Shown::new(e, car, source)?;
    Ok(all.into_iter().filter(|c| c.start_ms >= a && c.start_ms < b && shown.keep(c.source, c.start_ms)).collect())
}

/// `/charges` filters.
#[derive(Debug, Clone, Default)]
pub struct ChargesQuery {
    pub period: Period,
    pub places: Option<String>,
    /// `home`, `work`, `charger`, `service`, `other`.
    pub place_kind: Option<String>,
    /// `ac`, `dc`, `unknown`.
    pub kind: Option<String>,
    pub q: Option<String>,
    pub min_cost: Option<f64>,
    pub include_unpriced: Option<bool>,
    pub min_duration_min: Option<f64>,
    pub source: Option<String>,
    pub flags: Option<String>,
    /// `start` (default), `energy`, `duration`, `cost`, `power_max`.
    pub sort: Option<String>,
    pub dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Totals {
    pub count: u64,
    pub energy_added_kwh: f64,
    pub energy_grid_kwh: f64,
    pub energy_grid_known: u64,
    pub dc_inlet_kwh: Option<f64>,
    pub cost_by_currency: Vec<Money>,
    pub unpriced: u64,
    pub avg_duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChargesPage {
    pub items: Vec<ChargeView>,
    pub next: Option<String>,
    pub totals: Totals,
}

fn matches(v: &ChargeView, q: &ChargesQuery, want_places: &Option<Vec<u32>>, flags: &[&str]) -> bool {
    if let Some(ps) = want_places
        && !v.place.as_ref().and_then(|p| super::parse_place(&p.id)).is_some_and(|id| ps.contains(&id))
    {
        return false;
    }
    if let Some(k) = q.place_kind.as_deref().filter(|k| !k.is_empty())
        && v.place.as_ref().is_none_or(|p| p.kind != k)
    {
        return false;
    }
    if let Some(k) = q.kind.as_deref().filter(|k| !k.is_empty()) {
        let have = v.kind.unwrap_or("unknown");
        if !k.split(',').any(|w| w == have) {
            return false;
        }
    }
    if let Some(n) = q.q.as_deref().map(str::to_lowercase).filter(|s| !s.is_empty()) {
        let hit = v.place.as_ref().is_some_and(|p| p.name.to_lowercase().contains(&n))
            || v.address.short.as_deref().is_some_and(|a| a.to_lowercase().contains(&n));
        if !hit {
            return false;
        }
    }
    if let Some(m) = q.min_cost {
        match &v.cost {
            Some(c) if c.amount < m => return false,
            None if !q.include_unpriced.unwrap_or(false) => return false,
            _ => {}
        }
    }
    if q.min_duration_min.is_some_and(|m| (v.charging_ms as f64) < m * 60_000.0) {
        return false;
    }
    if !flags.is_empty() && !flags.iter().any(|f| if *f == "gap" { v.has_gaps } else { v.flags.in_progress }) {
        return false;
    }
    true
}

/// `/charges` for the cars given (`/cars/{car}/charges` is one car).
pub fn charges(e: &Engine, cars: &[CarId], q: &ChargesQuery, paging: &Paging) -> QResult<ChargesPage> {
    let source = super::parse_source(q.source.as_deref())?;
    if let Some(k) = q.kind.as_deref().filter(|k| !k.is_empty())
        && k.split(',').any(|w| !["ac", "dc", "unknown"].contains(&w))
    {
        return Err(QueryError::bad("kind", "ac, dc or unknown"));
    }
    let flags: Vec<&str> =
        q.flags.as_deref().map_or_else(Vec::new, |f| f.split(',').filter(|s| !s.is_empty()).collect());
    if flags.iter().any(|f| !["gap", "in_progress"].contains(f)) {
        return Err(QueryError::bad("flags", "gap or in_progress"));
    }
    let places = e.places();
    let want_places = super::drives::place_list(q.places.as_deref(), &places)?;
    let mut items = Vec::new();
    for &car in cars {
        let c = car_of(e, car)?;
        let x = ctx(e, &c)?;
        for ch in rows(e, car, &q.period, source)? {
            let v = view(e, &c, &ch, &x)?;
            if matches(&v, q, &want_places, &flags) {
                items.push(v);
            }
        }
    }
    let totals = totals(&items);
    let sort = q.sort.as_deref().unwrap_or("start");
    if !["start", "energy", "duration", "cost", "power_max"].contains(&sort) {
        return Err(QueryError::bad("sort", "start, energy, duration, cost or power_max"));
    }
    let desc = super::parse_dir(q.dir.as_deref(), true)?;
    let key = |v: &ChargeView| -> f64 {
        let n = |x: Option<f64>| x.unwrap_or(f64::NEG_INFINITY);
        match sort {
            "energy" => n(v.energy_added_kwh),
            "duration" => v.charging_ms as f64,
            "cost" => n(v.cost.as_ref().map(|c| c.amount)),
            "power_max" => n(v.power_max_kw),
            _ => v.start_ms as f64,
        }
    };
    let sig = format!("charges|{cars:?}|{}|{q:?}", q.period.sig());
    let page = paginate(items, |v| (key(v), format!("{}:{}", v.car.0, v.id)), desc, &sig, paging, false)?;
    Ok(ChargesPage { items: page.items, next: page.next, totals })
}

fn totals(items: &[ChargeView]) -> Totals {
    let mut by: BTreeMap<String, f64> = BTreeMap::new();
    let mut t = Totals {
        count: items.len() as u64,
        energy_added_kwh: 0.0,
        energy_grid_kwh: 0.0,
        energy_grid_known: 0,
        dc_inlet_kwh: None,
        cost_by_currency: Vec::new(),
        unpriced: 0,
        avg_duration_ms: None,
    };
    for v in items {
        t.energy_added_kwh += v.energy_added_kwh.unwrap_or(0.0);
        if let Some(g) = v.energy_grid_kwh {
            t.energy_grid_kwh += g;
            t.energy_grid_known += 1;
        }
        match &v.cost {
            Some(c) => *by.entry(c.currency.clone()).or_default() += c.amount,
            None => t.unpriced += 1,
        }
    }
    t.energy_added_kwh = r(Some(t.energy_added_kwh), 3).unwrap_or(0.0);
    t.energy_grid_kwh = r(Some(t.energy_grid_kwh), 3).unwrap_or(0.0);
    t.cost_by_currency = money(by);
    t.avg_duration_ms =
        (!items.is_empty()).then(|| items.iter().map(|v| v.charging_ms).sum::<u64>() / items.len() as u64);
    t
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BreakdownPart {
    pub rate: Option<String>,
    pub per_kwh: Option<f64>,
    pub per_minute: Option<f64>,
    pub kwh: Option<f64>,
    pub minutes: Option<f64>,
    pub cost: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Breakdown {
    pub parts: Vec<BreakdownPart>,
    pub session_fee: Option<f64>,
    pub total: Option<f64>,
    pub gap_ms: u64,
    pub mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChargeDetail {
    #[serde(flatten)]
    pub charge: ChargeView,
    pub prev: Option<String>,
    pub next: Option<String>,
    pub cost_breakdown: Breakdown,
    pub gaps: Vec<super::drives::Gap>,
}

/// `/cars/{car}/charges/{charge}`.
pub fn charge(e: &Engine, car: CarId, id: u64) -> QResult<ChargeDetail> {
    let c = car_of(e, car)?;
    let ch: Charge = e.db().get(car, id)?.ok_or(QueryError::NotFound("no_such_charge"))?;
    let x = ctx(e, &c)?;
    let all = rows(e, car, &Period::all(), None)?;
    let i = all.iter().position(|o| o.id == ch.id);
    let prev = i.and_then(|i| i.checked_sub(1)).and_then(|j| all.get(j)).map(|o| o.id.to_string());
    let next = i.and_then(|i| all.get(i + 1)).map(|o| o.id.to_string());
    let curve = costs::curve_of(e, car, &ch)?;
    let kept = cost_of(e, &c, &ch)?;
    let mut gaps = Vec::new();
    for w in curve.windows(2) {
        if w[1].t_ms.saturating_sub(w[0].t_ms) > 10_000 {
            gaps.push(super::drives::Gap { start_ms: w[0].t_ms + 1000, end_ms: w[1].t_ms, kind: "unknown" });
        }
    }
    let minutes = ch.end_ms.saturating_sub(ch.start_ms) as f64 / 60_000.0;
    let cost_breakdown = match &kept {
        Some(k) if k.source == "manual" => Breakdown {
            parts: vec![BreakdownPart {
                rate: Some("Entered".into()),
                per_kwh: if k.mode.as_deref() == Some("per_kwh") { k.entered } else { None },
                per_minute: if k.mode.as_deref() == Some("per_minute") { k.entered } else { None },
                kwh: r(
                    if k.energy_basis.as_deref() == Some("grid") {
                        ch.energy_used_kwh.or(ch.energy_added_kwh)
                    } else {
                        ch.energy_added_kwh
                    },
                    3,
                ),
                minutes: (k.mode.as_deref() == Some("per_minute")).then(|| (minutes * 10.0).round() / 10.0),
                cost: k.amount,
            }],
            session_fee: None,
            total: Some(k.amount),
            gap_ms: 0,
            mode: k.mode.clone(),
        },
        Some(k) => Breakdown {
            parts: k
                .parts
                .iter()
                .map(|p| BreakdownPart {
                    rate: p.rate.clone().or_else(|| Some("Standard".into())),
                    per_kwh: Some(p.per_kwh),
                    per_minute: None,
                    kwh: r(Some(p.kwh), 3),
                    minutes: None,
                    cost: (p.cost * 100.0).round() / 100.0,
                })
                .collect(),
            session_fee: k.session_fee,
            total: Some(k.amount),
            gap_ms: k.gap_ms,
            mode: Some(k.source.clone()),
        },
        None => Breakdown { parts: Vec::new(), session_fee: None, total: None, gap_ms: 0, mode: None },
    };
    let mut v = view(e, &c, &ch, &x)?;
    v.has_gaps = !gaps.is_empty();
    v.flags.gap_ms = gaps.iter().map(|g| g.end_ms - g.start_ms).sum();
    Ok(ChargeDetail { charge: v, prev, next, cost_breakdown, gaps })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Curve {
    pub t_ms: Vec<u64>,
    pub values: BTreeMap<String, Vec<Option<f64>>>,
    pub step_ms: u64,
}

/// The curve's fields this server has.
pub const CURVE_FIELDS: &[&str] = &["power_kw", "volts_v", "amps_a", "soc_pct", "battery_c", "range_km"];

/// `/cars/{car}/charges/{charge}/curve`: 1 Hz up to `max_points` (default 4000), coarser beyond (`step_ms` says).
/// A field the server does not keep comes as a column of nulls.
pub fn curve(e: &Engine, car: CarId, id: u64, fields: &[&str], max_points: Option<usize>) -> QResult<Scoped<Curve>> {
    let c = car_of(e, car)?;
    let ch: Charge = e.db().get(car, id)?.ok_or(QueryError::NotFound("no_such_charge"))?;
    let max = max_points.unwrap_or(4000);
    if !(2..=20_000).contains(&max) {
        return Err(QueryError::bad("max_points", "2 to 20000"));
    }
    let known = [
        "power_kw",
        "volts_v",
        "amps_a",
        "soc_pct",
        "usable_soc_pct",
        "battery_c",
        "range_km",
        "outside_c",
        "phases",
        "pilot_a",
        "ac_line_kw",
        "heater_kw",
        "dc_volts_v",
        "dc_amps_a",
        "ac_volts_v",
        "ac_amps_a",
    ];
    if let Some(bad) = fields.iter().find(|f| !known.contains(f)) {
        return Err(QueryError::bad("fields", &format!("{bad} is not a curve field")));
    }
    let pts = costs::curve_of(e, car, &ch)?;
    let step = ((pts.len() as u64).div_ceil(max as u64)).max(1) * 1000;
    let mut t_ms = Vec::new();
    let mut picked: Vec<&ChargePoint> = Vec::new();
    let mut last_bucket = None;
    for p in &pts {
        let b = p.t_ms / step;
        if last_bucket != Some(b) {
            last_bucket = Some(b);
            t_ms.push(p.t_ms);
            picked.push(p);
        }
    }
    let mut values = BTreeMap::new();
    for f in fields {
        let col: Vec<Option<f64>> = picked
            .iter()
            .map(|p| match *f {
                // charging power, positive
                "power_kw" => r(p.power_kw.map(f64::abs), 2),
                "volts_v" => r(p.voltage_v, 1),
                "amps_a" => r(p.current_a.map(f64::abs), 1),
                "soc_pct" => r(p.soc, 1),
                "battery_c" => r(p.battery_temp_c, 1),
                "range_km" => r(p.range_km, 1),
                _ => None,
            })
            .collect();
        values.insert((*f).to_string(), col);
    }
    Ok(scoped(&c, Curve { t_ms, values, step_ms: step }))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KindStat {
    pub kwh: f64,
    pub ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChargingStats {
    pub count: u64,
    pub energy_added_kwh: f64,
    pub by_kind: BTreeMap<&'static str, KindStat>,
    pub cost_by_currency: Vec<Money>,
    pub start_bands: Vec<serde_json::Value>,
    pub end_bands: Vec<serde_json::Value>,
    pub heatmap: serde_json::Value,
    pub runs: Vec<serde_json::Value>,
    pub share_above_80_pct: Option<f64>,
    pub share_below_20_pct: Option<f64>,
}

/// `/charging-stats` for the cars given (`/cars/{car}/charging-stats` is one car).
pub fn charging_stats(e: &Engine, cars: &[CarId], q: &ChargesQuery) -> QResult<ChargingStats> {
    let page = charges(e, cars, q, &Paging::first(super::MAX_LIMIT))?;
    // every charge, not one page
    let mut all = page.items;
    let mut next = page.next;
    while let Some(n) = next {
        let p = charges(e, cars, q, &Paging { limit: Some(super::MAX_LIMIT), after: Some(n) })?;
        all.extend(p.items);
        next = p.next;
    }
    let mut by_kind: BTreeMap<&'static str, KindStat> = BTreeMap::new();
    for k in ["ac", "dc", "unknown"] {
        by_kind.insert(k, KindStat { kwh: 0.0, ms: 0 });
    }
    let mut start_bands = [0u64; 10];
    let mut end_bands = [0u64; 10];
    let mut heat_start = vec![vec![0u64; 24]; 7];
    let mut heat_end = vec![vec![0u64; 24]; 7];
    let (mut above, mut below, mut with_end, mut with_start) = (0u64, 0u64, 0u64, 0u64);
    for v in &all {
        let k = by_kind.entry(v.kind.unwrap_or("unknown")).or_insert(KindStat { kwh: 0.0, ms: 0 });
        k.kwh += v.energy_added_kwh.unwrap_or(0.0);
        k.ms += v.charging_ms;
        let tz = crate::tz::zone(&v.time_zone);
        let (h, w) = crate::tz::hour_weekday(v.start_ms, tz);
        heat_start[w as usize][h as usize] += 1;
        let (h, w) = crate::tz::hour_weekday(v.end_ms.unwrap_or(v.start_ms + v.charging_ms), tz);
        heat_end[w as usize][h as usize] += 1;
        if let Some(s) = v.start_soc_pct {
            start_bands[((s / 10.0).floor() as usize).min(9)] += 1;
            with_start += 1;
            below += u64::from(s < 20.0);
        }
        if let Some(s) = v.end_soc_pct {
            end_bands[((s / 10.0).floor() as usize).min(9)] += 1;
            with_end += 1;
            above += u64::from(s > 80.0);
        }
    }
    for k in by_kind.values_mut() {
        k.kwh = r(Some(k.kwh), 3).unwrap_or(0.0);
    }
    let bands = |b: [u64; 10]| -> Vec<serde_json::Value> {
        b.iter()
            .enumerate()
            .map(|(i, n)| serde_json::json!({ "band": format!("{}-{}", i * 10, i * 10 + 10), "count": n }))
            .collect()
    };
    // runs: two or more charges, each starting within a day of the one before
    let mut sorted: Vec<&ChargeView> = all.iter().collect();
    sorted.sort_by_key(|v| v.start_ms);
    let mut runs = Vec::new();
    let mut cur: Vec<&ChargeView> = Vec::new();
    for v in sorted {
        if cur.last().is_some_and(|l| v.start_ms.saturating_sub(l.start_ms) > 86_400_000) {
            if cur.len() >= 2 {
                runs.push(run_of(&cur));
            }
            cur.clear();
        }
        cur.push(v);
    }
    if cur.len() >= 2 {
        runs.push(run_of(&cur));
    }
    Ok(ChargingStats {
        count: all.len() as u64,
        energy_added_kwh: page.totals.energy_added_kwh,
        by_kind,
        cost_by_currency: totals(&all).cost_by_currency,
        start_bands: bands(start_bands),
        end_bands: bands(end_bands),
        heatmap: serde_json::json!({ "start": heat_start, "end": heat_end }),
        runs,
        share_above_80_pct: r((with_end > 0).then(|| above as f64 / with_end as f64 * 100.0), 1),
        share_below_20_pct: r((with_start > 0).then(|| below as f64 / with_start as f64 * 100.0), 1),
    })
}

fn run_of(v: &[&ChargeView]) -> serde_json::Value {
    serde_json::json!({
        "charges": v.len(),
        "start_ms": v.first().map(|x| x.start_ms),
        "min_start_soc_pct": v.iter().filter_map(|x| x.start_soc_pct).min_by(f64::total_cmp),
        "max_end_soc_pct": v.iter().filter_map(|x| x.end_soc_pct).max_by(f64::total_cmp),
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlaceStat {
    pub place: Option<PlaceBrief>,
    pub label: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub sessions: u64,
    pub energy_added_kwh: f64,
    pub cost_by_currency: Vec<Money>,
    pub price_per_kwh: Option<f64>,
}

/// `/charging-stats/places`: where the cars charged, by place (charges at no saved place by their address).
pub fn charging_places(
    e: &Engine,
    cars: &[CarId],
    q: &ChargesQuery,
    sort: Option<&str>,
    paging: &Paging,
) -> QResult<List<PlaceStat>> {
    let mut all = Vec::new();
    let mut next = None;
    loop {
        let p = charges(e, cars, q, &Paging { limit: Some(super::MAX_LIMIT), after: next })?;
        all.extend(p.items);
        match p.next {
            Some(n) => next = Some(n),
            None => break,
        }
    }
    let mut by: BTreeMap<String, (PlaceStat, BTreeMap<String, f64>)> = BTreeMap::new();
    for v in all {
        let key = v
            .place
            .as_ref()
            .map_or_else(|| format!("addr:{}", v.address.short.clone().unwrap_or_default()), |p| p.id.clone());
        let e = by.entry(key).or_insert_with(|| {
            (
                PlaceStat {
                    place: v.place.as_ref().map(|p| PlaceBrief { id: p.id.clone(), name: p.name.clone() }),
                    label: v.place.as_ref().map(|p| p.name.clone()).or(v.address.short.clone()),
                    lat: v.position.as_ref().map(|p| p.lat),
                    lon: v.position.as_ref().map(|p| p.lon),
                    sessions: 0,
                    energy_added_kwh: 0.0,
                    cost_by_currency: Vec::new(),
                    price_per_kwh: None,
                },
                BTreeMap::new(),
            )
        });
        e.0.sessions += 1;
        e.0.energy_added_kwh += v.energy_added_kwh.unwrap_or(0.0);
        if let Some(c) = &v.cost {
            *e.1.entry(c.currency.clone()).or_default() += c.amount;
        }
    }
    let items: Vec<PlaceStat> = by
        .into_values()
        .map(|(mut s, m)| {
            let total: f64 = m.values().sum();
            s.price_per_kwh = r((m.len() == 1 && s.energy_added_kwh > 0.0).then(|| total / s.energy_added_kwh), 4);
            s.energy_added_kwh = r(Some(s.energy_added_kwh), 3).unwrap_or(0.0);
            s.cost_by_currency = money(m);
            s
        })
        .collect();
    let by_cost = match sort {
        None | Some("energy") => false,
        Some("cost") => true,
        Some(_) => return Err(QueryError::bad("sort", "energy or cost")),
    };
    let sig = format!("charging-places|{cars:?}|{q:?}|{sort:?}");
    paginate(
        items,
        |s| {
            let k = if by_cost { s.cost_by_currency.iter().map(|m| m.amount).sum() } else { s.energy_added_kwh };
            (k, s.label.clone().unwrap_or_default())
        },
        true,
        &sig,
        paging,
        false,
    )
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DcPoint {
    pub soc_pct: f64,
    pub power_kw: f64,
    pub battery_c: Option<f64>,
}

/// `/cars/{car}/charging-stats/dc-curve`: each DC charge's power by battery %, and the median by whole percent.
pub fn dc_curve(e: &Engine, car: CarId, period: &Period) -> QResult<Scoped<serde_json::Value>> {
    let c = car_of(e, car)?;
    let mut sessions = Vec::new();
    let mut by_pct: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for ch in rows(e, car, period, None)?.into_iter().filter(|ch| ch.kind == Some(model::ChargeKind::Dc)) {
        let pts: Vec<DcPoint> = costs::curve_of(e, car, &ch)?
            .into_iter()
            .filter_map(|p| Some(DcPoint { soc_pct: p.soc?, power_kw: p.power_kw?.abs(), battery_c: p.battery_temp_c }))
            .collect();
        for p in &pts {
            by_pct.entry(p.soc_pct.round() as i64).or_default().push(p.power_kw);
        }
        sessions.push(serde_json::json!({ "charge": ch.id.to_string(), "label": ch.address, "start_ms": ch.start_ms, "points": pts }));
    }
    let median: Vec<serde_json::Value> = by_pct
        .into_iter()
        .map(|(pct, mut v)| {
            v.sort_by(f64::total_cmp);
            serde_json::json!({ "soc_pct": pct, "power_kw": r(v.get(v.len() / 2).copied(), 1) })
        })
        .collect();
    Ok(scoped(&c, serde_json::json!({ "median": median, "sessions": sessions })))
}
