//! `/cars/{car}/series`: the car's samples as columns, bucketed so a chart gets at most `max_points`, with minima and
//! maxima kept, the state that took most of each bucket, and how much of it has data.

use super::{QResult, QueryError, Scoped, car_of, r, scoped};
use crate::Engine;
use crate::pipeline::all_rows;
use db::StateRow;
use model::{CarId, Sample, Source, State};
use serde::Serialize;
use std::collections::BTreeMap;

/// The fields a series can have, and how each is read from a sample.
pub const FIELDS: &[&str] = &[
    "soc_pct",
    "soc_bms_pct",
    "usable_soc_pct",
    "range_km",
    "ideal_range_km",
    "energy_remaining_kwh",
    "speed_kph",
    "power_kw",
    "odometer_km",
    "outside_c",
    "cabin_c",
    "battery_c",
    "setpoint_left_c",
    "setpoint_right_c",
    "elevation_m",
    "tyre_fl_kpa",
    "tyre_fr_kpa",
    "tyre_rl_kpa",
    "tyre_rr_kpa",
    "lat",
    "lon",
    "accuracy_m",
    "lv_v",
    "charge_power_kw",
    "battery_heat_kw",
    "fan_level",
];

fn get(s: &Sample, f: &str) -> Option<f64> {
    match f {
        "soc_pct" => s.soc,
        "soc_bms_pct" => s.soc_bms,
        "range_km" => s.range_km,
        "ideal_range_km" => s.ideal_range_km,
        "energy_remaining_kwh" => s.energy_remaining_kwh,
        "speed_kph" => s.speed_kmh,
        "power_kw" => s.battery_power_kw,
        "odometer_km" => s.odometer_km,
        "outside_c" => s.outside_temp_c,
        "cabin_c" => s.inside_temp_c,
        "battery_c" => s.battery_temp_c,
        "elevation_m" => s.elevation_m,
        "tyre_fl_kpa" => s.tpms_bar[0].map(|b| b * 100.0),
        "tyre_fr_kpa" => s.tpms_bar[1].map(|b| b * 100.0),
        "tyre_rl_kpa" => s.tpms_bar[2].map(|b| b * 100.0),
        "tyre_rr_kpa" => s.tpms_bar[3].map(|b| b * 100.0),
        "lat" => s.lat,
        "lon" => s.lon,
        "accuracy_m" => s.gps_accuracy_m,
        "lv_v" => s.lv_voltage_v,
        "charge_power_kw" => s.charge_power_kw,
        _ => None,
    }
}

/// Fields whose plain column is the bucket's last value, not its mean.
fn last_by_default(f: &str) -> bool {
    matches!(f, "lat" | "lon" | "accuracy_m" | "odometer_km" | "heading_deg")
}

const BUCKETS: &[u64] = &[
    1_000,
    2_000,
    5_000,
    10_000,
    15_000,
    30_000,
    60_000,
    120_000,
    300_000,
    600_000,
    900_000,
    1_800_000,
    3_600_000,
    7_200_000,
    10_800_000,
    21_600_000,
    43_200_000,
    86_400_000,
    172_800_000,
    604_800_000,
];

/// The smallest bucket that fits `[a, b)` in `max` buckets (aligned to UTC; the first holds `a`).
pub fn bucket_for(a: u64, b: u64, max: u64) -> u64 {
    let fits = |w: u64| (b.saturating_sub(1) / w).saturating_sub(a / w) < max;
    for &w in BUCKETS {
        if fits(w) {
            return w;
        }
    }
    let mut days = 8u64;
    while !fits(days * 86_400_000) {
        days = days.saturating_mul(2);
    }
    days * 86_400_000
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Columns {
    pub t_ms: Vec<u64>,
    pub values: BTreeMap<String, Vec<Option<f64>>>,
    pub bucket_ms: u64,
    pub coverage_pct: Vec<f64>,
    pub state: Vec<Option<&'static str>>,
}

/// `/series` options.
#[derive(Debug, Clone, Default)]
pub struct SeriesQuery {
    pub fields: Vec<String>,
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
    pub max_points: Option<u64>,
    /// `last`, `min`, `max`, `mean`, `p15`, `p85`.
    pub agg: Vec<String>,
    pub source: Option<String>,
}

fn agg(vals: &mut [f64], how: &str) -> Option<f64> {
    if vals.is_empty() {
        return None;
    }
    match how {
        "last" => vals.last().copied(),
        "min" => vals.iter().copied().min_by(f64::total_cmp),
        "max" => vals.iter().copied().max_by(f64::total_cmp),
        "mean" => Some(vals.iter().sum::<f64>() / vals.len() as f64),
        "p15" | "p85" => {
            vals.sort_by(f64::total_cmp);
            let q = if how == "p15" { 0.15 } else { 0.85 };
            let i = ((vals.len() - 1) as f64 * q).round() as usize;
            vals.get(i).copied()
        }
        _ => None,
    }
}

/// `/cars/{car}/series` (default the last 24 hours, 1000 points).
pub fn series(e: &Engine, car: CarId, q: &SeriesQuery, now_ms: u64) -> QResult<Scoped<Columns>> {
    let c = car_of(e, car)?;
    let (a, b) = super::Period::new(q.from_ms, q.to_ms).window(86_400_000, now_ms)?;
    let max = q.max_points.unwrap_or(1000);
    if !(2..=20_000).contains(&max) {
        return Err(QueryError::bad("max_points", "2 to 20000"));
    }
    if q.fields.is_empty() {
        return Err(QueryError::bad("fields", "name at least one field"));
    }
    if let Some(bad) = q.fields.iter().find(|f| !FIELDS.contains(&f.as_str())) {
        return Err(QueryError::bad("fields", &format!("{bad} is not a series field")));
    }
    let aggs: Vec<&str> = q.agg.iter().map(String::as_str).collect();
    if let Some(bad) = aggs.iter().find(|x| !["last", "min", "max", "mean", "p15", "p85"].contains(x)) {
        return Err(QueryError::bad("agg", &format!("{bad}: last, min, max, mean, p15 or p85")));
    }
    let source = super::parse_source(q.source.as_deref())?;
    let w = bucket_for(a, b, max);
    let first = a / w * w;
    let n = usize::try_from((b.saturating_sub(1) / w).saturating_sub(a / w) + 1).unwrap_or(0).min(max as usize);
    let mut samples: Vec<Sample> = Vec::new();
    for s in [Source::Can, Source::Teslamate] {
        if source.is_none_or(|x| x == s) {
            samples.extend(e.db().read_points::<Sample>(car, s, a, b)?);
        }
    }
    samples.sort_by_key(|s| s.t_ms);
    let mut buckets: Vec<Vec<&Sample>> = vec![Vec::new(); n];
    for s in &samples {
        let i = usize::try_from((s.t_ms - first) / w).unwrap_or(usize::MAX);
        if let Some(bk) = buckets.get_mut(i) {
            bk.push(s);
        }
    }
    let t_ms: Vec<u64> = (0..n as u64).map(|i| first + i * w).collect();
    let mut values: BTreeMap<String, Vec<Option<f64>>> = BTreeMap::new();
    for f in &q.fields {
        let mut cols: BTreeMap<String, Vec<Option<f64>>> = BTreeMap::new();
        let names: Vec<(String, &str)> = if aggs.is_empty() {
            let plain = if last_by_default(f) { "last" } else { "mean" };
            let mut v = vec![(f.clone(), plain)];
            if w > 1000 {
                v.push((format!("{f}.min"), "min"));
                v.push((format!("{f}.max"), "max"));
            }
            v
        } else {
            let mut v = vec![(f.clone(), aggs[0])];
            if aggs.len() > 1 {
                v.extend(aggs.iter().map(|x| (format!("{f}.{x}"), *x)));
            }
            v
        };
        for bk in &buckets {
            let vals: Vec<f64> = bk.iter().filter_map(|s| get(s, f)).filter(|v| v.is_finite()).collect();
            for (name, how) in &names {
                let mut v = vals.clone();
                cols.entry(name.clone()).or_default().push(r(agg(&mut v, how), 4));
            }
        }
        values.extend(cols);
    }
    // coverage and state per bucket, from the state spans
    let spans: Vec<model::StateSpan> =
        all_rows::<StateRow>(e.db(), car, first, b, source)?.into_iter().map(|s| s.span).collect();
    let mut coverage = Vec::with_capacity(n);
    let mut state = Vec::with_capacity(n);
    let drives =
        super::drives::rows(e, car, &super::Period::new(Some(first.saturating_sub(86_400_000)), Some(b)), source)?;
    let charges =
        super::charges::rows(e, car, &super::Period::new(Some(first.saturating_sub(7 * 86_400_000)), Some(b)), source)?;
    for &t in &t_ms {
        let (x, y) = (t, t + w);
        let mut by: [u64; 5] = [0; 5]; // driving, charging, parked, asleep, unknown
        let mut known = 0;
        for s in spans.iter().filter(|s| s.end_ms > x && s.start_ms < y) {
            let len = s.end_ms.min(y) - s.start_ms.max(x);
            match s.state {
                State::Online => {
                    by[2] += len;
                    known += len;
                }
                State::Asleep => {
                    by[3] += len;
                    known += len;
                }
                State::Unknown => by[4] += len,
            }
        }
        let overlap = |s: u64, en: u64| en.min(y).saturating_sub(s.max(x));
        let dr: u64 = drives.iter().map(|d| overlap(d.start_ms, d.end_ms)).sum();
        let ch: u64 = charges.iter().map(|c| overlap(c.start_ms, c.end_ms)).sum();
        by[0] = dr;
        by[1] = ch;
        by[2] = by[2].saturating_sub(dr + ch);
        by[4] += w.saturating_sub(known.max(dr + ch) + by[4]);
        coverage.push(r(Some(known.max(dr + ch).min(w) as f64 / w as f64 * 100.0), 1).unwrap_or(0.0));
        let words = ["driving", "charging", "parked", "asleep", "unknown"];
        let best = by.iter().enumerate().max_by_key(|(_, v)| **v).map(|(i, _)| words[i]);
        state.push(best);
    }
    Ok(scoped(&c, Columns { t_ms, values, bucket_ms: w, coverage_pct: coverage, state }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_smallest_bucket_that_fits() {
        assert_eq!(bucket_for(0, 1000 * 1000, 1000), 1000);
        assert_eq!(bucket_for(0, 86_400_000, 1000), 120_000);
        assert_eq!(bucket_for(500, 2500, 2), 2_000);
        assert!(bucket_for(0, 1000 * 86_400_000, 10).is_multiple_of(86_400_000));
    }
}
