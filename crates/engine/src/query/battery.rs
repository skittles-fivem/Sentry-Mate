//! `/battery`, `/battery/capacity`, `/parked`, `/efficiency`, `/temperatures`.

use super::{Paging, Period, QResult, QueryError, Scoped, car_of, paginate, r, scoped, source_word};
use crate::Engine;
use crate::pipeline::all_rows;
use db::StateRow;
use model::{CarId, Charge, ChargeKind, Drive, Source};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reading {
    pub t_ms: u64,
    pub odometer_km: Option<f64>,
    pub full_pack_kwh: f64,
    pub usable_kwh: Option<f64>,
    pub source: &'static str,
    /// `charge` (energy added over the % it added) or `bms`.
    pub method: &'static str,
    pub charge: Option<String>,
}

/// One reading per finished charge that added at least 10 %: the energy it added over the % it added.
fn readings(charges: &[Charge]) -> Vec<Reading> {
    charges
        .iter()
        .filter_map(|c| {
            let (a, b) = (c.start_soc?, c.end_soc?);
            let kwh = c.energy_added_kwh?;
            (b - a >= 10.0 && kwh > 0.0).then(|| Reading {
                t_ms: c.start_ms,
                odometer_km: r(c.position.as_ref().and_then(|p| p.odometer_km), 3),
                full_pack_kwh: (kwh / (b - a) * 100.0 * 100.0).round() / 100.0,
                usable_kwh: None,
                source: source_word(c.source),
                method: "charge",
                charge: Some(c.id.to_string()),
            })
        })
        .collect()
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Capacity {
    pub items: Vec<Reading>,
    pub trend: serde_json::Value,
    pub next: Option<String>,
}

/// `/cars/{car}/battery/capacity` (`group=charge`, the default; `x=time|odometer`).
pub fn capacity(
    e: &Engine,
    car: CarId,
    period: &Period,
    x: Option<&str>,
    paging: &Paging,
) -> QResult<Scoped<Capacity>> {
    let c = car_of(e, car)?;
    let by_odo = match x {
        None | Some("time") => false,
        Some("odometer") => true,
        Some(_) => return Err(QueryError::bad("x", "time or odometer")),
    };
    let charges = super::charges::rows(e, car, period, None)?;
    let items = readings(&charges);
    // the slope per 10,000 km (least squares over the readings with an odometer)
    let pts: Vec<(f64, f64)> = items.iter().filter_map(|r| r.odometer_km.map(|o| (o, r.full_pack_kwh))).collect();
    let slope = if pts.len() >= 2 {
        let n = pts.len() as f64;
        let (sx, sy) = pts.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + x, b + y));
        let (mx, my) = (sx / n, sy / n);
        let (num, den) =
            pts.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + (x - mx) * (y - my), b + (x - mx) * (x - mx)));
        (den > 0.0).then(|| num / den * 10_000.0)
    } else {
        None
    };
    let sig = format!("capacity|{car}|{}|{by_odo}", period.sig());
    let page = paginate(
        items,
        |rd| {
            (
                if by_odo { rd.odometer_km.unwrap_or(-1.0) } else { rd.t_ms as f64 },
                rd.charge.clone().unwrap_or_default(),
            )
        },
        false,
        &sig,
        paging,
        false,
    )?;
    Ok(scoped(
        &c,
        Capacity {
            items: page.items,
            trend: serde_json::json!({ "slope_kwh_per_10000_km": r(slope, 3), "projection": null }),
            next: page.next,
        },
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Battery {
    pub as_of_ms: Option<u64>,
    pub capacity: serde_json::Value,
    pub range: serde_json::Value,
    pub energy_now_kwh: Option<f64>,
    pub soc_pct: Option<f64>,
    pub usable_soc_pct: Option<f64>,
    pub band: serde_json::Value,
    pub chemistry: serde_json::Value,
    pub brick_spread_v: Option<f64>,
    pub lifetime: serde_json::Value,
    pub notes: Vec<&'static str>,
}

/// `/cars/{car}/battery`: the pack now, from the charges' readings and the newest values.
pub fn battery(e: &Engine, car: CarId) -> QResult<Scoped<Battery>> {
    let c = car_of(e, car)?;
    let charges = super::charges::rows(e, car, &Period::all(), None)?;
    let drives = super::drives::rows(e, car, &Period::all(), None)?;
    let rs = readings(&charges);
    let now = e.db().entry(car)?.and_then(|x| x.summary.now);
    let s = now.as_ref().map(|n| n.sample.clone()).unwrap_or_default();
    let latest = median(rs.iter().rev().take(5).map(|r| r.full_pack_kwh).collect());
    let first_ms = rs.first().map(|r| r.t_ms);
    let early = median(
        rs.iter().filter(|r| first_ms.is_some_and(|f| r.t_ms < f + 30 * 86_400_000)).map(|r| r.full_pack_kwh).collect(),
    );
    let new_source = if rs.first().is_some_and(|r| r.source == "teslamate") { "imported_charges" } else { "early_bms" };
    let degradation = latest.zip(early).filter(|(_, n)| *n > 0.0).map(|(l, n)| (1.0 - l / n) * 100.0);
    let full_now = s.range_km.zip(s.soc).filter(|(_, soc)| *soc > 5.0).map(|(rg, soc)| rg / soc * 100.0);
    let full_new = drives
        .iter()
        .filter_map(|d| {
            d.start_range_km.zip(d.start_soc).filter(|(_, soc)| *soc > 50.0).map(|(rg, soc)| rg / soc * 100.0)
        })
        .take(20)
        .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
    let mut notes = Vec::new();
    if rs.is_empty() {
        notes.push("no_history");
    } else if new_source == "imported_charges" {
        notes.push("new_value_from_imported_charges");
    } else {
        notes.push("new_value_from_early_readings");
    }
    let sum = |k: Option<ChargeKind>| -> f64 {
        charges.iter().filter(|ch| k.is_none_or(|k| ch.kind == Some(k))).filter_map(|ch| ch.energy_added_kwh).sum()
    };
    let (ac, dc) = (sum(Some(ChargeKind::Ac)), sum(Some(ChargeKind::Dc)));
    let drive_kwh: f64 = drives.iter().filter_map(|d| d.energy_used_kwh).sum();
    let charged = ac + dc;
    Ok(scoped(
        &c,
        Battery {
            as_of_ms: now.as_ref().map(|n| n.at_ms),
            capacity: serde_json::json!({
                "full_pack_kwh": r(latest, 2), "buffer_kwh": null, "usable_kwh": null, "spread_kwh": null,
                "readings": rs.len(), "new_kwh": r(early, 2), "new_source": early.map(|_| new_source),
                "difference_kwh": r(early.zip(latest).map(|(n, l)| n - l), 2),
                "degradation_pct": r(degradation, 1), "health_pct": r(degradation.map(|d| 100.0 - d), 1),
                "warranty_pct": 70.0,
            }),
            range: serde_json::json!({
                "full_now_km": r(full_now, 1), "full_now_from_energy_km": null, "full_new_km": r(full_new, 1),
                "new_source": full_new.map(|_| "first_30_days"),
                "lost_km": r(full_new.zip(full_now).map(|(n, x)| n - x), 1),
            }),
            energy_now_kwh: r(s.energy_remaining_kwh, 2),
            soc_pct: r(s.soc, 1),
            usable_soc_pct: None,
            band: serde_json::json!({ "low_pct": 20, "high_pct": 80, "charge_limit_pct": null }),
            chemistry: serde_json::json!({ "value": null, "source": null }),
            brick_spread_v: None,
            lifetime: serde_json::json!({
                "charged_kwh": r(Some(charged), 1), "discharged_kwh": r(s.total_discharge_kwh, 1),
                "ac_kwh": r(Some(ac), 1), "dc_kwh": r(Some(dc), 1), "regen_kwh": null,
                "drive_kwh": r(Some(drive_kwh), 1), "standing_kwh": null,
                "equivalent_cycles": r(latest.filter(|l| *l > 0.0).map(|l| charged / l), 1),
                "basis": "charges",
            }),
            notes,
        },
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Spell {
    pub id: String,
    pub car: super::CarRef,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub duration_ms: u64,
    pub clipped: bool,
    pub asleep_ms: u64,
    pub awake_ms: u64,
    pub awake_vehicle_bus_only_ms: u64,
    pub unknown_ms: u64,
    pub soc_start_pct: Option<f64>,
    pub soc_end_pct: Option<f64>,
    pub range_start_km: Option<f64>,
    pub range_end_km: Option<f64>,
    pub range_lost_km: Option<f64>,
    pub range_lost_per_hour_km: Option<f64>,
    pub energy_kwh: Option<f64>,
    pub energy_method: &'static str,
    pub avg_power_w: Option<f64>,
    pub awake_avg_power_w: Option<f64>,
    pub battery_min_c: Option<f64>,
    pub outside_avg_c: Option<f64>,
    pub cold: Option<bool>,
    pub place: Option<super::PlaceBrief>,
    pub address: Option<String>,
    pub awake_reasons: Vec<&'static str>,
    pub source: &'static str,
}

/// What ends a parked spell (a drive or a charge): start, end, soc and range at both ends, where, source.
type Busy<'a> = (u64, u64, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<&'a model::Position>, Source);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Parked {
    pub items: Vec<Spell>,
    pub summary: serde_json::Value,
    pub next: Option<String>,
}

/// `/cars/{car}/parked`: stays parked off a charger, newest first, clipped to the period; `min_hours` leaves out
/// shorter ones.
pub fn parked(
    e: &Engine,
    car: CarId,
    period: &Period,
    min_hours: Option<f64>,
    paging: &Paging,
) -> QResult<Scoped<Parked>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let b = b.min(crate::now_ms());
    let wide = Period::new(Some(a.saturating_sub(30 * 86_400_000)), Some(b));
    let drives = super::drives::rows(e, car, &wide, None)?;
    let charges = super::charges::rows(e, car, &wide, None)?;
    // what ends a spell: a drive or a charge (start, end, soc and range at both ends)
    let mut busy: Vec<Busy<'_>> = drives
        .iter()
        .map(|d: &Drive| {
            (d.start_ms, d.end_ms, d.start_soc, d.end_soc, d.start_range_km, d.end_range_km, d.end.as_ref(), d.source)
        })
        .chain(charges.iter().map(|c: &Charge| {
            (
                c.start_ms,
                c.end_ms,
                c.start_soc,
                c.end_soc,
                c.start_range_km,
                c.end_range_km,
                c.position.as_ref(),
                c.source,
            )
        }))
        .collect();
    busy.sort_by_key(|x| x.0);
    let states: Vec<model::StateSpan> =
        all_rows::<StateRow>(e.db(), car, a, b, Some(Source::Can))?.into_iter().map(|s| s.span).collect();
    let places = e.places();
    let mut items = Vec::new();
    let rated = c.efficiency_wh_km;
    for w in busy.windows(2) {
        let (p, n) = (&w[0], &w[1]);
        let (s0, s1) = (p.1, n.0);
        if s1 <= s0 || s1 <= a || s0 >= b {
            continue;
        }
        let (x, y) = (s0.max(a), s1.min(b));
        let dur = y - x;
        if min_hours.is_some_and(|h| (dur as f64) < h * 3_600_000.0) {
            continue;
        }
        let sp = super::timeline::split(&states, x, y);
        let lost = p.5.zip(n.4).map(|(r0, r1)| r0 - r1);
        let hours = (s1 - s0) as f64 / 3_600_000.0;
        let energy = lost.zip(rated).map(|(km, wh)| km * wh / 1000.0);
        let place = p.6.and_then(|pos| places.place_at(pos.lat, pos.lon));
        items.push(Spell {
            id: format!("parked:{s0}"),
            car: super::CarRef(car),
            start_ms: s0,
            end_ms: Some(s1),
            duration_ms: s1 - s0,
            clipped: s0 < a || s1 > b,
            asleep_ms: sp.asleep,
            awake_ms: sp.awake,
            awake_vehicle_bus_only_ms: sp.awake_vehicle_bus_only,
            unknown_ms: sp.unknown,
            soc_start_pct: r(p.3, 1),
            soc_end_pct: r(n.2, 1),
            range_start_km: r(p.5, 1),
            range_end_km: r(n.4, 1),
            range_lost_km: r(lost, 1),
            range_lost_per_hour_km: r(lost.filter(|_| hours > 0.0).map(|l| l / hours), 2),
            energy_kwh: r(energy, 3),
            energy_method: "range",
            avg_power_w: r(energy.filter(|_| hours > 0.0).map(|k| k * 1000.0 / hours), 0),
            awake_avg_power_w: None,
            battery_min_c: None,
            outside_avg_c: None,
            cold: None,
            place: place.map(|pl| super::PlaceBrief { id: super::place_ref(pl.id), name: pl.name.clone() }),
            address: None,
            awake_reasons: Vec::new(),
            source: source_word(p.7),
        });
    }
    let total_kwh: f64 = items.iter().filter_map(|s| s.energy_kwh).sum();
    let hours: f64 = items.iter().map(|s| s.duration_ms as f64 / 3_600_000.0).sum();
    let summary = serde_json::json!({
        "spells": items.len(), "energy_kwh": r(Some(total_kwh), 3), "hours": r(Some(hours), 1),
        "kwh_per_day": r((hours > 0.0).then(|| total_kwh / hours * 24.0), 3), "cost": null,
    });
    let sig = format!("parked|{car}|{}|{min_hours:?}", period.sig());
    let page = paginate(items, |s| (s.start_ms as f64, s.id.clone()), true, &sig, paging, false)?;
    Ok(scoped(&c, Parked { items: page.items, summary, next: page.next }))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TempGroup {
    pub temp_c: f64,
    pub temp_to_c: f64,
    pub drives: u64,
    pub distance_km: f64,
    pub energy_kwh: f64,
    pub wh_per_km: Option<f64>,
    pub range_efficiency_pct: Option<f64>,
    pub avg_kph: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Efficiency {
    pub net_wh_per_km: Option<f64>,
    pub gross_wh_per_km: Option<f64>,
    pub parked_kwh: Option<f64>,
    pub distance_km: f64,
    pub drives: u64,
    pub estimated_drives: u64,
    pub all_drives: serde_json::Value,
    pub by_temperature: Vec<TempGroup>,
    pub no_temperature: serde_json::Value,
    pub factor: serde_json::Value,
}

/// `/cars/{car}/efficiency`: `min_distance_km` (default 1), `temp_group_c` (1 to 20, default 5).
pub fn efficiency(
    e: &Engine,
    car: CarId,
    period: &Period,
    min_distance_km: Option<f64>,
    temp_group_c: Option<f64>,
) -> QResult<Scoped<Efficiency>> {
    let c = car_of(e, car)?;
    let min_km = min_distance_km.unwrap_or(1.0);
    let g = temp_group_c.unwrap_or(5.0);
    if !(1.0..=20.0).contains(&g) {
        return Err(QueryError::bad("temp_group_c", "1 to 20"));
    }
    let all = super::drives::rows(e, car, period, None)?;
    let (all_km, all_kwh) = all
        .iter()
        .fold((0.0, 0.0), |(k, w), d| (k + d.distance_km.unwrap_or(0.0), w + d.energy_used_kwh.unwrap_or(0.0)));
    let kept: Vec<&Drive> = all.iter().filter(|d| d.distance_km.is_some_and(|k| k >= min_km)).collect();
    let (km, kwh) = kept.iter().fold((0.0, 0.0), |(k, w), d| match (d.distance_km, d.energy_used_kwh) {
        (Some(a), Some(b)) => (k + a, w + b),
        _ => (k, w),
    });
    let mut groups: std::collections::BTreeMap<i64, (u64, f64, f64, u64)> = Default::default();
    let (mut no_t, mut no_t_km) = (0u64, 0.0);
    for d in &kept {
        match d.outside_temp_avg_c {
            Some(t) => {
                let gr = groups.entry((t / g).floor() as i64).or_default();
                gr.0 += 1;
                gr.1 += d.distance_km.unwrap_or(0.0);
                gr.2 += d.energy_used_kwh.unwrap_or(0.0);
                gr.3 += d.end_ms.saturating_sub(d.start_ms);
            }
            None => {
                no_t += 1;
                no_t_km += d.distance_km.unwrap_or(0.0);
            }
        }
    }
    let rated = c.efficiency_wh_km;
    let by_temperature = groups
        .into_iter()
        .map(|(i, (n, dk, de, ms))| {
            let w = (dk > 0.1).then(|| de * 1000.0 / dk);
            TempGroup {
                temp_c: r(Some(i as f64 * g), 1).unwrap_or(0.0),
                temp_to_c: r(Some((i + 1) as f64 * g), 1).unwrap_or(0.0),
                drives: n,
                distance_km: r(Some(dk), 3).unwrap_or(0.0),
                energy_kwh: r(Some(de), 3).unwrap_or(0.0),
                wh_per_km: r(w, 1),
                range_efficiency_pct: r(rated.zip(w).map(|(rt, w)| rt / w * 100.0), 1),
                avg_kph: r((ms > 0).then(|| dk / (ms as f64 / 3_600_000.0)), 1),
            }
        })
        .collect();
    Ok(scoped(
        &c,
        Efficiency {
            net_wh_per_km: r((km > 0.1).then(|| kwh * 1000.0 / km), 1),
            gross_wh_per_km: r((all_km > 0.1).then(|| all_kwh * 1000.0 / all_km), 1),
            parked_kwh: None,
            distance_km: r(Some(km), 3).unwrap_or(0.0),
            drives: kept.len() as u64,
            estimated_drives: kept.iter().filter(|d| d.source == Source::Teslamate).count() as u64,
            all_drives: serde_json::json!({ "drives": all.len(), "distance_km": r(Some(all_km), 3), "energy_kwh": r(Some(all_kwh), 3) }),
            by_temperature,
            no_temperature: serde_json::json!({ "drives": no_t, "distance_km": r(Some(no_t_km), 3) }),
            factor: serde_json::json!({
                "rated_wh_per_km": rated, "ideal_wh_per_km": null,
                "source": if rated.is_some() { "owner" } else { "energy_and_range" }, "readings": null, "as_of_ms": null,
            }),
        },
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TempStat {
    pub min_c: Option<f64>,
    pub avg_c: Option<f64>,
    pub max_c: Option<f64>,
    pub coverage_pct: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Temperatures {
    pub outside: TempStat,
    pub cabin: TempStat,
    pub battery: TempStat,
    pub daily: Vec<serde_json::Value>,
}

/// `/cars/{car}/temperatures` (default the last 30 days): every reading, so the lowest and highest are true.
pub fn temperatures(e: &Engine, car: CarId, period: &Period) -> QResult<Scoped<Temperatures>> {
    let c = car_of(e, car)?;
    let (a, b) = period.window(30 * 86_400_000, crate::now_ms())?;
    let tz = crate::tz::zone(&c.time_zone);
    let f = e.db().read_fields::<model::Sample>(
        car,
        Source::Can,
        a,
        b,
        &["outside_temp_c", "inside_temp_c", "battery_temp_c"],
    )?;
    let span = b.saturating_sub(a).max(1) as f64;
    let stat = |col: &[Option<f64>]| -> TempStat {
        let v: Vec<f64> = col.iter().flatten().copied().collect();
        TempStat {
            min_c: r(v.iter().copied().min_by(f64::total_cmp), 1),
            max_c: r(v.iter().copied().max_by(f64::total_cmp), 1),
            avg_c: r((!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64), 1),
            coverage_pct: r(Some(v.len() as f64 * 1000.0 / span * 100.0), 1),
        }
    };
    let col = |i: usize| f.values.get(i).map(|(_, v)| v.as_slice()).unwrap_or(&[]);
    let mut days: std::collections::BTreeMap<String, (u64, Vec<f64>, u64)> = Default::default();
    for (i, &t) in f.t_ms.iter().enumerate() {
        let d = days
            .entry(crate::tz::date_str(t, tz))
            .or_insert_with(|| (crate::tz::start_of(crate::tz::date(t, tz), tz), Vec::new(), 0));
        d.2 += 1;
        if let Some(v) = col(0).get(i).copied().flatten() {
            d.1.push(v);
        }
    }
    let daily = days
        .into_iter()
        .map(|(date, (t0, mut v, n))| {
            v.sort_by(f64::total_cmp);
            let q = |p: f64| v.get(((v.len().max(1) - 1) as f64 * p).round() as usize).copied();
            serde_json::json!({
                "date": date, "t_ms": t0, "min_c": r(v.first().copied(), 1), "p15_c": r(q(0.15), 1),
                "p85_c": r(q(0.85), 1), "max_c": r(v.last().copied(), 1),
                "coverage_pct": r(Some(n as f64 * 1000.0 / 86_400_000.0 * 100.0), 1),
            })
        })
        .collect();
    Ok(scoped(&c, Temperatures { outside: stat(col(0)), cabin: stat(col(1)), battery: stat(col(2)), daily }))
}
