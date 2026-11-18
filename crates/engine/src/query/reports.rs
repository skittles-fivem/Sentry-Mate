//! `/statistics`, `/mileage`, `/totals`, `/updates`, `/fleet/versions`.

use super::{
    CarRef, List, Money, Paging, Period, QResult, QueryError, Scoped, car_of, money, paginate, r, scoped, source_word,
};
use crate::Engine;
use crate::pipeline::all_rows;
use crate::tz::{self, Period as P};
use db::UpdateRow;
use model::{CarId, Source};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatRow {
    pub car: CarRef,
    pub time_zone: String,
    pub period_start_ms: u64,
    pub period_end_ms: u64,
    pub label: String,
    pub time_driven_ms: u64,
    pub drives: u64,
    pub distance_odometer_km: Option<f64>,
    pub distance_logged_km: f64,
    pub outside_avg_c: Option<f64>,
    pub driving_efficiency_pct: Option<f64>,
    pub energy_used_kwh: f64,
    pub energy_standing_kwh: Option<f64>,
    pub energy_added_kwh: f64,
    pub energy_grid_kwh: Option<f64>,
    pub charges: u64,
    pub energy_per_charge_kwh: Option<f64>,
    pub cost_by_currency: Vec<Money>,
    pub price_per_kwh: Vec<serde_json::Value>,
    pub cost_per_100km: Vec<Money>,
    pub net_wh_per_km: Option<f64>,
    pub gross_wh_per_km: Option<f64>,
    pub overhead_pct: Option<f64>,
    pub completeness: serde_json::Value,
}

#[derive(Default)]
struct Acc {
    start: u64,
    end: u64,
    label: String,
    driven_ms: u64,
    drives: u64,
    km: f64,
    kwh: f64,
    odo_min: Option<f64>,
    odo_max: Option<f64>,
    temp_sum: f64,
    temp_n: u64,
    charges: u64,
    added: f64,
    grid: f64,
    grid_n: u64,
    cost: BTreeMap<String, f64>,
    tm_rows: u64,
}

/// `/statistics` for the cars given: one row per car and period (day, week, month, year in the car's zone, or `all`),
/// newest first, clipped to the period; drives and charges count by their start.
pub fn statistics(
    e: &Engine,
    cars: &[CarId],
    period_kind: &str,
    period: &Period,
    paging: &Paging,
) -> QResult<List<StatRow>> {
    let p = tz::Period::parse(period_kind).ok_or_else(|| QueryError::bad("period", "day, week, month, year or all"))?;
    let (a, b) = period.bounds()?;
    let mut rows = Vec::new();
    for &car in cars {
        let c = car_of(e, car)?;
        let zone = tz::zone(&c.time_zone);
        let mut by: BTreeMap<u64, Acc> = BTreeMap::new();
        let mut slot = |t: u64| -> u64 {
            let (s, en, label) = tz::period_of(t, p, zone);
            let s2 = if p == P::All { a } else { s.max(a) };
            let en2 = if p == P::All { b.min(crate::now_ms()) } else { en.min(b) };
            by.entry(s2).or_insert_with(|| Acc { start: s2, end: en2, label, ..Acc::default() });
            s2
        };
        let drives = super::drives::rows(e, car, period, None)?;
        let charges = super::charges::rows(e, car, period, None)?;
        let cx = super::charges::ctx(e, &c)?;
        let mut keys_d = Vec::new();
        for d in &drives {
            keys_d.push(slot(d.start_ms));
        }
        let mut keys_c = Vec::new();
        for ch in &charges {
            keys_c.push(slot(ch.start_ms));
        }
        for (d, k) in drives.iter().zip(keys_d) {
            let x = by.entry(k).or_default();
            x.drives += 1;
            x.driven_ms += d.end_ms.saturating_sub(d.start_ms);
            x.km += d.distance_km.unwrap_or(0.0);
            x.kwh += d.energy_used_kwh.unwrap_or(0.0);
            for o in [d.start_odometer_km, d.end_odometer_km].into_iter().flatten() {
                x.odo_min = Some(x.odo_min.map_or(o, |m: f64| m.min(o)));
                x.odo_max = Some(x.odo_max.map_or(o, |m: f64| m.max(o)));
            }
            if let Some(t) = d.outside_temp_avg_c {
                x.temp_sum += t;
                x.temp_n += 1;
            }
            x.tm_rows += u64::from(d.source == Source::Teslamate);
        }
        for (ch, k) in charges.iter().zip(keys_c) {
            let v = super::charges::view(e, &c, ch, &cx)?;
            let x = by.entry(k).or_default();
            x.charges += 1;
            x.added += ch.energy_added_kwh.unwrap_or(0.0);
            if let Some(g) = ch.energy_used_kwh {
                x.grid += g;
                x.grid_n += 1;
            }
            if let Some(cost) = v.cost {
                *x.cost.entry(cost.currency).or_default() += cost.amount;
            }
            x.tm_rows += u64::from(ch.source == Source::Teslamate);
        }
        for x in by.into_values() {
            let net = (x.km > 0.1).then(|| x.kwh * 1000.0 / x.km);
            let per100: Vec<Money> = x
                .cost
                .iter()
                .filter(|_| x.km > 0.1)
                .map(|(cur, amt)| Money { currency: cur.clone(), amount: (amt / x.km * 100.0 * 100.0).round() / 100.0 })
                .collect();
            let ppk: Vec<serde_json::Value> = x
                .cost
                .iter()
                .filter(|_| x.added > 0.0)
                .map(|(cur, amt)| serde_json::json!({ "currency": cur, "value": r(Some(amt / if x.grid_n > 0 { x.grid } else { x.added }), 4) }))
                .collect();
            rows.push(StatRow {
                car: CarRef(car),
                time_zone: c.time_zone.clone(),
                period_start_ms: x.start,
                period_end_ms: x.end,
                label: x.label,
                time_driven_ms: x.driven_ms,
                drives: x.drives,
                distance_odometer_km: r(x.odo_max.zip(x.odo_min).map(|(h, l)| h - l), 3),
                distance_logged_km: r(Some(x.km), 3).unwrap_or(0.0),
                outside_avg_c: r((x.temp_n > 0).then(|| x.temp_sum / x.temp_n as f64), 1),
                driving_efficiency_pct: r(c.efficiency_wh_km.zip(net).map(|(rt, n)| rt / n * 100.0), 1),
                energy_used_kwh: r(Some(x.kwh), 3).unwrap_or(0.0),
                energy_standing_kwh: None,
                energy_added_kwh: r(Some(x.added), 3).unwrap_or(0.0),
                energy_grid_kwh: r((x.grid_n > 0).then_some(x.grid), 3),
                charges: x.charges,
                energy_per_charge_kwh: r((x.charges > 0).then(|| x.added / x.charges as f64), 3),
                cost_by_currency: money(x.cost.clone()),
                price_per_kwh: ppk,
                cost_per_100km: per100,
                net_wh_per_km: r(net, 1),
                gross_wh_per_km: r(net, 1),
                overhead_pct: None,
                completeness: serde_json::json!({
                    "recorded_pct": null, "unknown_ms": null, "dropped_stretches": null,
                    "open_drive": false, "open_charge": false, "teslamate_rows": x.tm_rows,
                }),
            });
        }
    }
    let sig = format!("statistics|{cars:?}|{period_kind}|{}", period.sig());
    paginate(rows, |row| (row.period_start_ms as f64, row.car.0.to_string()), true, &sig, paging, false)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MileageRow {
    pub period_start_ms: u64,
    pub period_end_ms: u64,
    pub label: String,
    pub distance_km: Option<f64>,
    pub odometer_start_km: Option<f64>,
    pub odometer_end_km: Option<f64>,
    pub logged_km: f64,
    pub not_logged_km: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Mileage {
    pub items: Vec<MileageRow>,
    pub projection: serde_json::Value,
}

/// `/cars/{car}/mileage` (`group=day|week|month|year`, default month): by odometer, and what the drives logged.
pub fn mileage(e: &Engine, car: CarId, period: &Period, group: Option<&str>) -> QResult<Scoped<Mileage>> {
    let c = car_of(e, car)?;
    let p = match group.unwrap_or("month") {
        "all" => return Err(QueryError::bad("group", "day, week, month or year")),
        g => tz::Period::parse(g).ok_or_else(|| QueryError::bad("group", "day, week, month or year"))?,
    };
    let zone = tz::zone(&c.time_zone);
    let mut by: BTreeMap<u64, MileageRow> = BTreeMap::new();
    for d in super::drives::rows(e, car, period, None)? {
        let (s, en, label) = tz::period_of(d.start_ms, p, zone);
        let row = by.entry(s).or_insert(MileageRow {
            period_start_ms: s,
            period_end_ms: en,
            label,
            distance_km: None,
            odometer_start_km: None,
            odometer_end_km: None,
            logged_km: 0.0,
            not_logged_km: None,
        });
        row.logged_km += d.distance_km.unwrap_or(0.0);
        if let Some(o) = d.start_odometer_km {
            row.odometer_start_km = Some(row.odometer_start_km.map_or(o, |m| m.min(o)));
        }
        if let Some(o) = d.end_odometer_km {
            row.odometer_end_km = Some(row.odometer_end_km.map_or(o, |m| m.max(o)));
        }
    }
    let mut items: Vec<MileageRow> = by.into_values().collect();
    for row in &mut items {
        row.distance_km = r(row.odometer_end_km.zip(row.odometer_start_km).map(|(b, a)| b - a), 3);
        row.not_logged_km = r(row.distance_km.map(|d| (d - row.logged_km).max(0.0)), 3);
        row.logged_km = r(Some(row.logged_km), 3).unwrap_or(0.0);
    }
    let first = items.first().map(|r| r.period_start_ms);
    let last = items.last().map(|r| r.period_end_ms.min(crate::now_ms()));
    let total: f64 = items.iter().filter_map(|r| r.distance_km).sum();
    let days = first.zip(last).map(|(a, b)| b.saturating_sub(a) as f64 / 86_400_000.0).filter(|d| *d > 0.0);
    items.reverse();
    Ok(scoped(
        &c,
        Mileage {
            items,
            projection: serde_json::json!({
                "km_per_year": r(days.map(|d| total / d * 365.25), 0),
                "basis_days": days.map(|d| d.round()),
            }),
        },
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Totals {
    pub drives: u64,
    pub distance_km: f64,
    pub odometer_first_km: Option<f64>,
    pub odometer_last_km: Option<f64>,
    pub odometer_last_ms: Option<u64>,
    pub not_logged_km: Option<f64>,
    pub charges: u64,
    pub energy_added_kwh: f64,
    pub energy_grid_kwh: f64,
    pub grid_coverage_pct: Option<f64>,
    pub charging_efficiency_pct: Option<f64>,
    pub ac: serde_json::Value,
    pub dc: serde_json::Value,
    pub updates: u64,
}

/// `/cars/{car}/totals` (`source=can|teslamate|all`): the whole history's counts and totals from the fleet summary,
/// with the odometer and the AC/DC split from the rows.
pub fn totals(e: &Engine, car: CarId, source: Option<&str>) -> QResult<Scoped<Totals>> {
    let c = car_of(e, car)?;
    let src = super::parse_source(source)?;
    let x = e.db().entry(car)?.ok_or(QueryError::NoSuchCar)?;
    let t = &x.summary.tally;
    let parts: Vec<&db::Totals> = match src {
        Some(Source::Can) => vec![&t.can],
        Some(Source::Teslamate) => vec![&t.teslamate],
        None => vec![&t.can, &t.teslamate],
    };
    let sum = |f: fn(&db::Totals) -> i64| parts.iter().map(|p| f(p)).sum::<i64>();
    let drives = super::drives::rows(e, car, &Period::all(), src)?;
    let charges = super::charges::rows(e, car, &Period::all(), src)?;
    let first = drives.iter().find_map(|d| d.start_odometer_km);
    let last = drives.iter().rev().find_map(|d| d.end_odometer_km.map(|o| (o, d.end_ms)));
    let logged: f64 = drives.iter().filter_map(|d| d.distance_km).sum();
    let kind = |k: model::ChargeKind| {
        let v: Vec<&model::Charge> = charges.iter().filter(|ch| ch.kind == Some(k)).collect();
        serde_json::json!({ "charges": v.len(), "kwh": r(Some(v.iter().filter_map(|ch| ch.energy_added_kwh).sum()), 3) })
    };
    let added = sum(|p| p.charge_added_wh) as f64 / 1000.0;
    let grid = sum(|p| p.charge_used_wh) as f64 / 1000.0;
    let with_grid = charges.iter().filter(|ch| ch.energy_used_kwh.is_some()).count();
    let added_with_grid: f64 =
        charges.iter().filter(|ch| ch.energy_used_kwh.is_some()).filter_map(|ch| ch.energy_added_kwh).sum();
    Ok(scoped(
        &c,
        Totals {
            drives: u64::try_from(sum(|p| p.drives)).unwrap_or(0),
            distance_km: r(Some(sum(|p| p.distance_m) as f64 / 1000.0), 3).unwrap_or(0.0),
            odometer_first_km: r(first, 3),
            odometer_last_km: r(last.map(|l| l.0), 3),
            odometer_last_ms: last.map(|l| l.1),
            not_logged_km: r(first.zip(last).map(|(f, l)| (l.0 - f - logged).max(0.0)), 3),
            charges: u64::try_from(sum(|p| p.charges)).unwrap_or(0),
            energy_added_kwh: r(Some(added), 3).unwrap_or(0.0),
            energy_grid_kwh: r(Some(grid), 3).unwrap_or(0.0),
            grid_coverage_pct: r((!charges.is_empty()).then(|| with_grid as f64 / charges.len() as f64 * 100.0), 1),
            charging_efficiency_pct: r((grid > 0.0).then(|| added_with_grid / grid * 100.0), 1),
            ac: kind(model::ChargeKind::Ac),
            dc: kind(model::ChargeKind::Dc),
            updates: u64::try_from(t.updates).unwrap_or(0),
        },
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpdateView {
    pub id: String,
    pub car: CarRef,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub until_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub since_previous_ms: Option<u64>,
    pub version: Option<String>,
    pub version_source: Option<&'static str>,
    pub detected_by: &'static str,
    pub build_id: Option<String>,
    pub changed_modules: Vec<String>,
    pub charges: u64,
    pub full_range_km: Option<f64>,
    pub full_pack_kwh: Option<f64>,
    pub source: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Updates {
    pub items: Vec<UpdateView>,
    pub next: Option<String>,
    pub summary: serde_json::Value,
}

/// `/cars/{car}/updates`, newest first. (CAN logs give no firmware version yet, so updates come from imported
/// history for now.)
pub fn updates(e: &Engine, car: CarId, period: &Period, paging: &Paging) -> QResult<Scoped<Updates>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let all: Vec<UpdateRow> = all_rows(e.db(), car, 0, u64::MAX, None)?;
    let charges = super::charges::rows(e, car, &Period::all(), None)?;
    let mut items = Vec::new();
    for (i, u) in all.iter().enumerate() {
        let up = &u.update;
        let until = all.get(i + 1).map(|n| n.update.start_ms);
        let since = i.checked_sub(1).and_then(|j| all.get(j)).map(|p| up.start_ms.saturating_sub(p.update.start_ms));
        let n_charges =
            charges.iter().filter(|ch| ch.start_ms >= up.start_ms && until.is_none_or(|t| ch.start_ms < t)).count();
        let tm = up.source == Source::Teslamate;
        items.push(UpdateView {
            id: u.id.to_string(),
            car: CarRef(car),
            start_ms: up.start_ms,
            end_ms: up.end_ms,
            until_ms: until,
            duration_ms: up.end_ms.map(|e| e.saturating_sub(up.start_ms)),
            since_previous_ms: since,
            version: Some(up.version.clone()).filter(|v| !v.is_empty()),
            version_source: tm.then_some("teslamate"),
            detected_by: if tm { "teslamate" } else { "build_change" },
            build_id: None,
            changed_modules: Vec::new(),
            charges: n_charges as u64,
            full_range_km: None,
            full_pack_kwh: None,
            source: source_word(up.source),
        });
    }
    let mut gaps: Vec<u64> = items.iter().filter_map(|u| u.since_previous_ms).collect();
    gaps.sort_unstable();
    let current = items.last().cloned();
    let summary = serde_json::json!({
        "count": items.len(),
        "median_gap_ms": gaps.get(gaps.len() / 2),
        "current_version": current.as_ref().and_then(|u| u.version.clone()),
        "current_version_source": current.as_ref().and_then(|u| u.version_source),
        "current_version_since_ms": current.as_ref().map(|u| u.start_ms),
        "build_changed_since": null,
    });
    items.retain(|u| u.start_ms >= a && u.start_ms < b);
    let sig = format!("updates|{car}|{}", period.sig());
    let page = paginate(items, |u| (u.start_ms as f64, u.id.clone()), true, &sig, paging, false)?;
    Ok(scoped(&c, Updates { items: page.items, next: page.next, summary }))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VersionRow {
    pub car: CarRef,
    pub version: Option<String>,
    pub version_source: Option<&'static str>,
    pub build_changed_since: Option<bool>,
    pub last_update_ms: Option<u64>,
    pub time_zone: String,
}

/// `/fleet/versions`: each car's current software version.
pub fn versions(e: &Engine, cars: &[CarId], search: Option<&str>, paging: &Paging) -> QResult<List<VersionRow>> {
    let mut items = Vec::new();
    let needle = search.map(str::to_lowercase).filter(|s| !s.is_empty());
    for &car in cars {
        let c = car_of(e, car)?;
        let last: Option<UpdateRow> =
            e.db().list::<UpdateRow>(car, &db::Query::default().limit(1))?.items.into_iter().next();
        let row = VersionRow {
            car: CarRef(car),
            version: last.as_ref().map(|u| u.update.version.clone()).filter(|v| !v.is_empty()),
            version_source: last.as_ref().filter(|u| u.update.source == Source::Teslamate).map(|_| "teslamate"),
            build_changed_since: None,
            last_update_ms: last.as_ref().map(|u| u.update.start_ms),
            time_zone: c.time_zone.clone(),
        };
        if needle.as_ref().is_some_and(|n| {
            !c.name.to_lowercase().contains(n) && !row.version.as_deref().is_some_and(|v| v.contains(n.as_str()))
        }) {
            continue;
        }
        items.push(row);
    }
    let sig = format!("versions|{cars:?}|{search:?}");
    paginate(items, |v| (v.last_update_ms.map_or(-1.0, |t| t as f64), v.car.0.to_string()), true, &sig, paging, false)
}
