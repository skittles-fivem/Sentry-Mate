//! `/cars/{car}/series`: the car's samples as columns, bucketed so a chart gets at most `max_points`, with minima and
//! maxima kept, the state that took most of each bucket, and how much of it has data.

use super::{QResult, QueryError, Scoped, car_of, r, scoped};
use crate::Engine;
use crate::pipeline::all_rows;
use db::StateRow;
use model::{CarId, Sample, State};
use serde::{Deserialize, Serialize};
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

/// The sample field a series field is read from (for what an import filled: [`timeline::gapfill::slot_bit`]).
fn sample_field(f: &str) -> &str {
    match f {
        "soc_pct" => "soc",
        "soc_bms_pct" => "soc_bms",
        "speed_kph" => "speed_kmh",
        "power_kw" => "battery_power_kw",
        "outside_c" => "outside_temp_c",
        "cabin_c" => "inside_temp_c",
        "battery_c" => "battery_temp_c",
        "tyre_fl_kpa" => "tpms_bar",
        "tyre_fr_kpa" => "tpms_bar",
        "tyre_rl_kpa" => "tpms_bar",
        "tyre_rr_kpa" => "tpms_bar",
        "accuracy_m" => "gps_accuracy_m",
        "lv_v" => "lv_voltage_v",
        other => other,
    }
}

/// Live seconds this far apart at most are live coverage for a series: inside it an import only fills values the
/// live second lacks; outside it the import's seconds are shown (the Tesla API alone polls every minute or so), less
/// the values a live one still holds then ([`timeline::gapfill::fill`]).
pub const SERIES_COVER_MS: u64 = 90_000;

/// Where a sample's values came from: the live inputs (SMO, the Tesla API, TeslaFi's API) and per import site the mask of
/// the values it gave (all bits: the import's own second).
#[derive(Debug, Clone, Copy, Default)]
struct Tag {
    /// The live inputs of the point (none for an import's own second).
    live: model::Inputs,
    imported: [u64; SITES],
}

/// One mask per import site ([`model::ImportSite::index`]).
const SITES: usize = model::ImportSite::ALL.len();

impl Tag {
    fn of_stream(s: model::Stream) -> Tag {
        match s.origin() {
            model::Origin::Smo => Tag { live: model::Inputs::SMO, ..Tag::default() },
            model::Origin::TeslaApi => Tag { live: model::Inputs::TESLA_API, ..Tag::default() },
            model::Origin::TeslafiApi => Tag { live: model::Inputs::TESLAFI_API, ..Tag::default() },
            model::Origin::TeslamateLocal => Tag { live: model::Inputs::TESLAMATE_LOCAL, ..Tag::default() },
            model::Origin::Import(site) => {
                let mut t = Tag::default();
                t.imported[site.index()] = u64::MAX;
                t
            }
        }
    }

    /// The origins of a value with this mask bit.
    fn origins(&self, bit: u64, out: &mut std::collections::BTreeSet<model::Origin>) {
        let own: Vec<usize> = (0..SITES).filter(|&i| self.imported[i] == u64::MAX).collect();
        let by: Vec<usize> =
            if own.is_empty() { (0..SITES).filter(|&i| self.imported[i] & bit != 0).collect() } else { own };
        if by.is_empty() {
            out.extend(self.live.iter().map(|i| model::Stream::from(i).origin()));
        }
        for i in by {
            out.insert(model::Origin::Import(model::ImportSite::ALL[i]));
        }
    }
}

impl Tag {
    /// The origins of the point itself: the import of one of its own seconds, else the live inputs and the imports
    /// that filled one of the fields asked for (`bits`).
    fn gave(&self, bits: u64, out: &mut std::collections::BTreeSet<model::Origin>) {
        let own = self.imported.contains(&u64::MAX);
        if !own {
            self.origins(0, out);
        }
        for (i, m) in self.imported.iter().enumerate() {
            if *m == u64::MAX || m & bits != 0 {
                out.insert(model::Origin::Import(model::ImportSite::ALL[i]));
            }
        }
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
    /// Per field asked for, where its values came from (an import that filled what live history lacked is named
    /// here; empty when the field had no value).
    pub value_sources: BTreeMap<String, Vec<super::SourceRef>>,
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

    /// An hour's values ([`SlotAcc`]), after those before it.
    fn merge(&mut self, s: &SlotAcc) {
        let SlotAcc(_, n, sum, lo, hi, last) = *s;
        self.n += n;
        self.sum += sum;
        self.min = Some(self.min.map_or(lo, |m| m.min(lo)));
        self.max = Some(self.max.map_or(hi, |m| m.max(hi)));
        self.last = Some(last);
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
    let streams: Vec<model::Stream> = match source {
        None => model::Stream::ALL.to_vec(),
        Some(super::SourceFilter::Live) => model::Stream::LIVE.to_vec(),
        Some(f) => f.streams(),
    };
    // where both imports cover the same time, only the chosen site's points (with `source=all` and `source=import`)
    let sites = match source {
        Some(super::SourceFilter::Import(None)) => super::system::Sites::new(e, car)?,
        _ => super::system::Sites::default(),
    };
    // with `source=all`: the live seconds, with what the chosen import fills; the import's own seconds where there is
    // no live history (and in the spans flipped to the imported side)
    let flipped = if source.is_none() { super::system::flipped_spans(e, car)? } else { Vec::new() };
    let ctx = DayCtx { source, streams, sites, flipped };
    // the streams that gave a point to the answer
    let mut gave = std::collections::BTreeSet::new();
    let mut by_field: Vec<std::collections::BTreeSet<model::Origin>> = vec![Default::default(); q.fields.len()];
    let bits: Vec<u64> = q.fields.iter().map(|f| timeline::gapfill::slot_bit(sample_field(f))).collect();
    let any_bit = bits.iter().fold(0, |a, b| a | b);
    let stamps = super::dayparts::Stamps::of(e, car)?;
    // a day at a time, only the days that have samples: the whole history never sits in memory at once
    let days = stamps.days(&ctx.streams, a, b);
    let gaps = super::gapfill::Gaps::new(e, car, source)?;
    if w.is_multiple_of(SLOT) && !quantiles {
        // buckets of whole hours: each day's sums per hour, kept for the days the window holds whole (made again when
        // the day's files, or those of the days beside it, change)
        let key = day_key(e, car, &ctx)?;
        let fuse = &e.inner.opts.fuse;
        let near = fuse.lower_margin_ms().max(fuse.margin_ms() + SERIES_COVER_MS).div_ceil(DAY).max(1) * DAY;
        // (kept for `source=all`, what the pages ask for; one source's are worked out each time)
        let whole: Vec<(u64, u64)> = days
            .iter()
            .filter(|&&d| source.is_none() && d >= a && d.saturating_add(DAY) <= b)
            .map(|&d| (d, stamps.of_days(d.saturating_sub(near), d.saturating_add(near), key)))
            .collect();
        let kept = super::dayparts::parts(
            e,
            car,
            "series-day",
            &whole,
            || super::gapfill::Gaps::new(e, car, source),
            |gaps, d| Ok(day_part(&day_chunk(e, car, &ctx, gaps, d, d + DAY)?, d)),
        )?;
        let mut kept = kept.into_iter().zip(whole.iter().map(|x| x.0)).peekable();
        for day in days {
            let part = match kept.peek() {
                Some((_, d)) if *d == day => kept.next().map(|x| x.0),
                _ => None,
            };
            let part = match part {
                Some(p) => p,
                None => {
                    let (x, y) = (day.max(a), day.saturating_add(DAY).min(b));
                    std::sync::Arc::new(day_part(&day_chunk(e, car, &ctx, &gaps, x, y)?, day))
                }
            };
            for (fi, f) in q.fields.iter().enumerate() {
                if let Some(slots) = part.fields.get(f.as_str()) {
                    for s in slots {
                        let t = day + u64::from(s.0) * SLOT;
                        let Some(i) = t.checked_sub(first).and_then(|d| usize::try_from(d / w).ok()).filter(|&i| i < n)
                        else {
                            continue;
                        };
                        accs[fi][i].merge(s);
                    }
                }
                if let Some(o) = part.origins.get(f.as_str()) {
                    by_field[fi].extend(o.iter().filter_map(|w| origin_of_word(w)));
                }
            }
            gave.extend(part.gave.iter().filter_map(|w| origin_of_word(w)));
            for (word, mask) in &part.filled {
                if mask & any_bit != 0
                    && let Some(o) = origin_of_word(word)
                {
                    gave.insert(o);
                }
            }
        }
    } else {
        for day in days {
            let (x, y) = (day.max(a), day.saturating_add(DAY).min(b));
            let chunk = day_chunk(e, car, &ctx, &gaps, x, y)?;
            for (s, tag) in &chunk {
                let Some(i) = s.t_ms.checked_sub(first).and_then(|d| usize::try_from(d / w).ok()).filter(|&i| i < n)
                else {
                    continue;
                };
                for (fi, f) in q.fields.iter().enumerate() {
                    if let Some(v) = get(s, f).filter(|v| v.is_finite()) {
                        accs[fi][i].add(v, keep);
                        tag.origins(bits[fi], &mut by_field[fi]);
                    }
                }
                // the inputs and imports that gave the answer a point
                tag.gave(any_bit, &mut gave);
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
    let value_sources = q
        .fields
        .iter()
        .zip(by_field)
        .map(|(f, o)| (f.clone(), super::source_refs(&o.into_iter().collect::<Vec<_>>())))
        .collect();
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
            value_sources,
        },
    ))
}

/// Hours: what a long period's buckets are summed from (a bucket of whole hours holds whole slots, so its sums,
/// minima, maxima and last values are those of its slots).
const SLOT: u64 = 3_600_000;

/// What a day of a series is read with: the source asked for and what it shows.
struct DayCtx {
    source: Option<super::SourceFilter>,
    streams: Vec<model::Stream>,
    sites: super::system::Sites,
    flipped: Vec<(u64, u64)>,
}

/// A key of what a day's part rests on besides the days' files: the source, which import is shown where, the spans
/// flipped to the imported side, whether imports fill gaps, how inputs are fused, and this part's own make-up (its
/// name and number: a change to how a day is read or summed takes a new number, so kept days are made again).
fn day_key(e: &Engine, car: CarId, ctx: &DayCtx) -> QResult<u64> {
    let gap_sites = match ctx.source {
        None => Some(super::system::Sites::new(e, car)?),
        Some(_) => None,
    };
    let fill = super::fleet::gap_fill_on(e, car)?;
    Ok(super::dayparts::key_of(&[
        &"series-day 2",
        &ctx.source,
        &ctx.sites,
        &gap_sites,
        &ctx.flipped,
        &fill,
        &e.inner.opts.fuse,
        &SERIES_COVER_MS,
        &FIELDS,
    ]))
}

/// The points of `[x, y)` (within one UTC day) of the series' source, in time order, each with where its values came
/// from.
fn day_chunk(
    e: &Engine,
    car: CarId,
    ctx: &DayCtx,
    gaps: &super::gapfill::Gaps<'_>,
    x: u64,
    y: u64,
) -> QResult<Vec<(Sample, Tag)>> {
    let (source, streams) = (ctx.source, &ctx.streams);
    let in_flipped = |t: u64| {
        let i = ctx.flipped.partition_point(|s| s.1 <= t);
        ctx.flipped.get(i).is_some_and(|s| s.0 <= t)
    };
    let mut chunk: Vec<(Sample, Tag)> = Vec::new();
    // SMO's and the APIs' seconds fused as the timeline is (inside SMO's coverage SMO's second, no API second
    // beside it), never summed
    let live = model::Stream::LIVE.iter().all(|s| streams.contains(s));
    if live {
        let lf = e.live_fused(car, x, y)?;
        let mut pts = lf.samples;
        if !ctx.flipped.is_empty() {
            pts.retain(|p| !in_flipped(p.t_ms));
        }
        let tag_of = |t: u64| timeline::fuse::inputs_at(&lf.marks, t);
        if source.is_none() {
            let g = gaps.samples(pts, x, y, SERIES_COVER_MS, super::gapfill::Own::Always)?;
            for p in g.samples.iter() {
                let mut tag = Tag::default();
                for (_, site, mask) in g.at(p.t_ms) {
                    tag.imported[site.index()] |= mask;
                }
                if tag.imported.iter().all(|m| *m != u64::MAX) {
                    tag.live = tag_of(p.t_ms);
                }
                chunk.push((p.clone(), tag));
            }
        } else {
            chunk.extend(pts.into_iter().map(|p| {
                let live = tag_of(p.t_ms);
                (p, Tag { live, ..Tag::default() })
            }));
        }
    }
    for &s in streams {
        if live && model::Stream::LIVE.contains(&s) {
            continue;
        }
        // (with `source=all` the import's points came with the live seconds)
        if source.is_none() && s.is_imported() {
            continue;
        }
        let mut pts = e.db().read_points::<Sample>(car, s, x, y)?;
        if let Some(site) = s.origin().site() {
            pts.retain(|p| ctx.sites.shows(site, p.t_ms));
        }
        chunk.extend(pts.into_iter().map(|p| (p, Tag::of_stream(s))));
    }
    chunk.sort_by_key(|(s, _)| s.t_ms);
    Ok(chunk)
}

/// One field's values in one hour of a day: `(hour, count, sum, min, max, last)`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct SlotAcc(
    u8,
    u64,
    #[serde(with = "super::dayparts::exact")] f64,
    #[serde(with = "super::dayparts::exact")] f64,
    #[serde(with = "super::dayparts::exact")] f64,
    #[serde(with = "super::dayparts::exact")] f64,
);

/// A UTC day of a series (or the part of it in the window), summed per hour: every field's values, and where they
/// came from (origins by their stream's name on disk, which never changes).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct DayPart {
    /// Per field with values: its hours with values, in order.
    fields: BTreeMap<String, Vec<SlotAcc>>,
    /// Per field with values: where they came from.
    origins: BTreeMap<String, Vec<String>>,
    /// Where the points came from, less the imports that only filled values (`filled`).
    gave: Vec<String>,
    /// Per import site: the values (mask bits) it filled in points that were not its own.
    filled: BTreeMap<String, u64>,
}

fn origin_word(o: model::Origin) -> Option<&'static str> {
    model::Stream::ALL.into_iter().find(|s| s.origin() == o).map(model::Stream::dir_name)
}

fn origin_of_word(w: &str) -> Option<model::Origin> {
    model::Stream::ALL.into_iter().find(|s| s.dir_name() == w).map(model::Stream::origin)
}

/// The hourly sums of a day's points (`day` its start; the points all in it, in time order).
fn day_part(chunk: &[(Sample, Tag)], day: u64) -> DayPart {
    let slots = usize::try_from(DAY / SLOT).unwrap_or(24);
    let mut accs: Vec<Vec<Acc>> = vec![vec![Acc::default(); slots]; FIELDS.len()];
    let mut origins: Vec<std::collections::BTreeSet<model::Origin>> = vec![Default::default(); FIELDS.len()];
    let bits: Vec<u64> = FIELDS.iter().map(|f| timeline::gapfill::slot_bit(sample_field(f))).collect();
    let mut gave = std::collections::BTreeSet::new();
    let mut filled = [0u64; SITES];
    for (s, tag) in chunk {
        let Some(h) = s.t_ms.checked_sub(day).and_then(|d| usize::try_from(d / SLOT).ok()).filter(|&h| h < slots)
        else {
            continue;
        };
        for (fi, f) in FIELDS.iter().enumerate() {
            if let Some(v) = get(s, f).filter(|v| v.is_finite()) {
                accs[fi][h].add(v, 0);
                tag.origins(bits[fi], &mut origins[fi]);
            }
        }
        // (the imports that filled fields asked for are added for the answer: `filled`)
        tag.gave(0, &mut gave);
        for (k, m) in tag.imported.iter().enumerate() {
            if *m != u64::MAX {
                filled[k] |= m;
            }
        }
    }
    let words = |o: &std::collections::BTreeSet<model::Origin>| -> Vec<String> {
        o.iter().filter_map(|o| origin_word(*o)).map(str::to_string).collect()
    };
    let mut part = DayPart { gave: words(&gave), ..DayPart::default() };
    for (fi, f) in FIELDS.iter().enumerate() {
        let v: Vec<SlotAcc> = accs[fi]
            .iter()
            .enumerate()
            .filter_map(|(h, a)| match (a.n, a.min, a.max, a.last) {
                (1.., Some(lo), Some(hi), Some(last)) => {
                    Some(SlotAcc(u8::try_from(h).unwrap_or(u8::MAX), a.n, a.sum, lo, hi, last))
                }
                _ => None,
            })
            .collect();
        if !v.is_empty() {
            part.fields.insert((*f).to_string(), v);
        }
        if !origins[fi].is_empty() {
            part.origins.insert((*f).to_string(), words(&origins[fi]));
        }
    }
    for (k, m) in filled.iter().enumerate() {
        if *m != 0
            && let Some(w) = origin_word(model::Origin::Import(model::ImportSite::ALL[k]))
        {
            part.filled.insert(w.to_string(), *m);
        }
    }
    part
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
