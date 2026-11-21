//! `/cars/{car}/series`: the car's samples as columns, bucketed so a chart gets at most `max_points`, with minima and
//! maxima kept, the state that took most of each bucket, and how much of it has data.

use super::{QResult, QueryError, Scoped, car_of, r, scoped};
use crate::Engine;
use crate::pipeline::all_rows;
use db::StateRow;
use model::{CarId, Sample, State};
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
    "climate",
    "heading_deg",
];

/// A field's value in a sample. The fields the samples do not hold yet (`usable_soc_pct`, the set points,
/// `battery_heat_kw`, `fan_level`, `heading_deg`, and `climate`, whose column will be text) are null columns.
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

/// Fields whose plain column is the bucket's last value, not its mean (`climate` is text: only its last value means
/// anything).
fn last_by_default(f: &str) -> bool {
    matches!(f, "lat" | "lon" | "accuracy_m" | "odometer_km" | "heading_deg" | "climate")
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
    /// The fields asked for, in their order.
    pub fields: Vec<String>,
    pub t_ms: Vec<u64>,
    pub values: BTreeMap<String, Vec<Option<f64>>>,
    pub bucket_ms: u64,
    pub coverage_pct: Vec<f64>,
    pub state: Vec<Option<&'static str>>,
    /// The inputs and imports that gave the points (empty when there were none).
    pub sources: Vec<super::SourceRef>,
    pub source_label: String,
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

const DAY: u64 = 86_400_000;

/// One field's values in one bucket, as they stream past: last, min, max and sum exactly; for quantiles the values
/// themselves up to `keep`, then every second one (and so on), evenly over the bucket.
#[derive(Debug, Clone, Default)]
struct Acc {
    n: u64,
    sum: f64,
    min: Option<f64>,
    max: Option<f64>,
    last: Option<f64>,
    kept: Vec<f64>,
    stride: u64,
}

impl Acc {
    fn add(&mut self, v: f64, keep: usize) {
        if keep > 0 {
            let stride = self.stride.max(1);
            if self.n.is_multiple_of(stride) {
                self.kept.push(v);
                if self.kept.len() >= keep {
                    let mut i = 0;
                    self.kept.retain(|_| {
                        i += 1;
                        i % 2 == 1
                    });
                    self.stride = stride * 2;
                }
            }
        }
        self.n += 1;
        self.sum += v;
        self.min = Some(self.min.map_or(v, |m| m.min(v)));
        self.max = Some(self.max.map_or(v, |m| m.max(v)));
        self.last = Some(v);
    }

    fn get(&mut self, how: &str) -> Option<f64> {
        if self.n == 0 {
            return None;
        }
        match how {
            "last" => self.last,
            "min" => self.min,
            "max" => self.max,
            "mean" => Some(self.sum / self.n as f64),
            "p15" | "p85" => {
                self.kept.sort_by(f64::total_cmp);
                let q = if how == "p15" { 0.15 } else { 0.85 };
                let i = ((self.kept.len().max(1) - 1) as f64 * q).round() as usize;
                self.kept.get(i).copied()
            }
            _ => None,
        }
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
    // the fields once each, in their order; none named: the battery level and the range
    let mut fields: Vec<String> = Vec::new();
    for f in &q.fields {
        if !fields.contains(f) {
            fields.push(f.clone());
        }
    }
    if fields.is_empty() {
        fields = vec!["soc_pct".into(), "range_km".into()];
    }
    let q = &SeriesQuery { fields, ..q.clone() };
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
    // the plain column and the extra ones of each field, and how each is worked out
    let mut cols: Vec<(String, usize, &str)> = Vec::new();
    for (fi, f) in q.fields.iter().enumerate() {
        if aggs.is_empty() {
            let plain = if last_by_default(f) { "last" } else { "mean" };
            cols.push((f.clone(), fi, plain));
            if w > 1000 && !last_by_default(f) {
                cols.push((format!("{f}.min"), fi, "min"));
                cols.push((format!("{f}.max"), fi, "max"));
            }
        } else {
            cols.push((f.clone(), fi, aggs[0]));
            if aggs.len() > 1 {
                cols.extend(aggs.iter().map(|x| (format!("{f}.{x}"), fi, *x)));
            }
        }
    }
    let quantiles = cols.iter().any(|(_, _, how)| matches!(*how, "p15" | "p85"));
    // values kept per bucket for quantiles: all of them up to a budget for the whole answer, then an even thinning
    let keep = if quantiles { (4_000_000 / (n.max(1) * q.fields.len())).clamp(16, 4096) } else { 0 };
    let mut accs: Vec<Vec<Acc>> = vec![vec![Acc::default(); n]; q.fields.len()];
    // a day at a time, only the days that have samples: the whole history never sits in memory at once
    let mut days = std::collections::BTreeSet::new();
    let streams: Vec<model::Stream> = match source {
        None => model::Stream::ALL.to_vec(),
        Some(super::SourceFilter::Live) => vec![model::Stream::Smo, model::Stream::TeslaApi],
        Some(f) => vec![f.stream()],
    };
    // the streams that gave a point to the answer
    let mut gave = std::collections::BTreeSet::new();
    for &s in &streams {
        days.extend(
            e.db()
                .days::<Sample>(car, s)?
                .into_iter()
                .map(|d| d.start_ms)
                .filter(|&d| d.saturating_add(DAY) > a && d < b),
        );
    }
    for day in days {
        let (x, y) = (day.max(a), day.saturating_add(DAY).min(b));
        let mut chunk: Vec<Sample> = Vec::new();
        // SMO's and the API's seconds fused as the timeline is (inside SMO's coverage SMO's second, no API second
        // beside it), never summed
        let live = streams.contains(&model::Stream::Smo) && streams.contains(&model::Stream::TeslaApi);
        if live {
            let (pts, smo, api) = e.live_fused(car, x, y)?;
            if smo {
                gave.insert(model::Stream::Smo.origin());
            }
            if api {
                gave.insert(model::Stream::TeslaApi.origin());
            }
            chunk.extend(pts);
        }
        for &s in &streams {
            if live && matches!(s, model::Stream::Smo | model::Stream::TeslaApi) {
                continue;
            }
            let pts = e.db().read_points::<Sample>(car, s, x, y)?;
            if !pts.is_empty() {
                gave.insert(s.origin());
            }
            chunk.extend(pts);
        }
        chunk.sort_by_key(|s| s.t_ms);
        for s in &chunk {
            let Some(i) = s.t_ms.checked_sub(first).and_then(|d| usize::try_from(d / w).ok()).filter(|&i| i < n) else {
                continue;
            };
            for (fi, f) in q.fields.iter().enumerate() {
                if let Some(v) = get(s, f).filter(|v| v.is_finite()) {
                    accs[fi][i].add(v, keep);
                }
            }
        }
    }
    let t_ms: Vec<u64> = (0..n as u64).map(|i| first + i * w).collect();
    let mut values: BTreeMap<String, Vec<Option<f64>>> = BTreeMap::new();
    for (name, fi, how) in &cols {
        values.insert(name.clone(), accs[*fi].iter_mut().map(|acc| r(acc.get(how), 4)).collect());
    }
    let (coverage, state) = coverage_and_state(e, car, &t_ms, w, b, source)?;
    let gave: Vec<model::Origin> = gave.into_iter().collect();
    Ok(scoped(
        &c,
        Columns {
            fields: q.fields.clone(),
            t_ms,
            values,
            bucket_ms: w,
            coverage_pct: coverage,
            state,
            sources: super::source_refs(&gave),
            source_label: model::Origin::label_of(&gave),
        },
    ))
}

/// The streams with sample days meeting `[a, b)`, as origins (what a chart over that window draws from).
pub(crate) fn origins_between(e: &Engine, car: CarId, a: u64, b: u64) -> QResult<Vec<model::Origin>> {
    let mut out = Vec::new();
    for s in model::Stream::ALL {
        if e.db().days::<Sample>(car, s)?.iter().any(|d| d.start_ms.saturating_add(DAY) > a && d.start_ms < b) {
            out.push(s.origin());
        }
    }
    Ok(out)
}

/// Per bucket of `t_ms` (each `w` long, the last ending by `b`): the share with known data (asleep counts as known)
/// and the state that took most of it, from the car's state spans, drives and charges.
pub(crate) fn coverage_and_state(
    e: &Engine,
    car: CarId,
    t_ms: &[u64],
    w: u64,
    b: u64,
    source: Option<super::SourceFilter>,
) -> QResult<(Vec<f64>, Vec<Option<&'static str>>)> {
    let n = t_ms.len();
    let first = t_ms.first().copied().unwrap_or(b);
    let spans: Vec<model::StateSpan> =
        all_rows::<StateRow>(e.db(), car, first, b, source.map(super::SourceFilter::family))?
            .into_iter()
            .map(|s| s.span)
            .collect();
    let mut coverage = Vec::with_capacity(n);
    let mut state = Vec::with_capacity(n);
    let drives =
        super::drives::rows(e, car, &super::Period::new(Some(first.saturating_sub(86_400_000)), Some(b)), source)?;
    let charges =
        super::charges::rows(e, car, &super::Period::new(Some(first.saturating_sub(7 * 86_400_000)), Some(b)), source)?;
    // the lists are in time order and do not overlap within themselves: each bucket starts looking where the one
    // before stopped
    let (mut si, mut di, mut ci) = (0usize, 0usize, 0usize);
    let words = ["driving", "charging", "parked", "asleep", "unknown"];
    for &t in t_ms {
        let (x, y) = (t, t + w);
        let mut by: [u64; 5] = [0; 5]; // driving, charging, parked, asleep, unknown
        let mut known = 0;
        while spans.get(si).is_some_and(|s| s.end_ms <= x) {
            si += 1;
        }
        for s in spans[si.min(spans.len())..].iter().take_while(|s| s.start_ms < y) {
            let len = s.end_ms.min(y).saturating_sub(s.start_ms.max(x));
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
        while drives.get(di).is_some_and(|d| d.end_ms <= x) {
            di += 1;
        }
        while charges.get(ci).is_some_and(|c| c.end_ms <= x) {
            ci += 1;
        }
        let dr: u64 = drives[di.min(drives.len())..]
            .iter()
            .take_while(|d| d.start_ms < y)
            .map(|d| overlap(d.start_ms, d.end_ms))
            .sum();
        let ch: u64 = charges[ci.min(charges.len())..]
            .iter()
            .take_while(|c| c.start_ms < y)
            .map(|c| overlap(c.start_ms, c.end_ms))
            .sum();
        by[0] = dr;
        by[1] = ch;
        by[2] = by[2].saturating_sub(dr + ch);
        by[4] += w.saturating_sub(known.max(dr + ch) + by[4]);
        coverage.push(r(Some(known.max(dr + ch).min(w) as f64 / w as f64 * 100.0), 1).unwrap_or(0.0));
        let best = by.iter().enumerate().max_by_key(|(_, v)| **v).map(|(i, _)| words[i]);
        state.push(best);
    }
    Ok((coverage, state))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_smallest_bucket_that_fits() {
        assert_eq!(bucket_for(0, 1000 * 1000, 1000), 1000);
        assert_eq!(bucket_for(0, 86_400_000, 1000), 120_000);
        assert_eq!(bucket_for(500, 2500, 2), 2_000);
        assert!(bucket_for(0, 1000 * 86_400_000, 10).is_multiple_of(86_400_000));
    }

    #[test]
    fn buckets_sum_as_they_stream_and_quantiles_stay_close_when_thinned() {
        let mut all = Acc::default();
        let mut thin = Acc::default();
        for i in 0..10_000u32 {
            let v = f64::from((i * 7919) % 10_000);
            all.add(v, 1_000_000);
            thin.add(v, 64);
        }
        assert_eq!(all.get("min"), Some(0.0));
        assert_eq!(all.get("max"), Some(9999.0));
        assert_eq!(all.get("mean"), Some(4999.5));
        assert_eq!(all.get("last"), Some(f64::from((9999u32 * 7919) % 10_000)));
        assert_eq!(all.get("p15"), Some(1500.0));
        assert!(thin.kept.len() < 64);
        let p = thin.get("p85").unwrap();
        assert!((p - 8500.0).abs() < 600.0, "{p}");
        assert_eq!(Acc::default().get("mean"), None);
    }
}
