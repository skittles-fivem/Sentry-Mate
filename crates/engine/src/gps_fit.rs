//! Finding the layout of the Screenmate's own GPS frame (0x04F on its internal bus, `can3`): where it keeps latitude
//! and longitude, worked out from a car's stored logs and the positions its other inputs reported at the same
//! moments (the Tesla API, TeslaFi, local TeslaMate, and their imports). `sentry-mate gps-layout` and
//! `POST /api/v1/cars/{car}/gps-layout/fit` (the "Find GPS layout" button of Data health) both run it.
//!
//! Each frame is paired with the nearest such position within [`PAIR_MS`] (or, while the car stood, with the
//! positions around it when they agree). Then every layout of a field is tried: start bit, width
//! ([`MIN_WIDTH`]..=[`MAX_WIDTH`]), Intel or Motorola, signed or unsigned, a scale of [`SCALES`], and an offset that
//! is 0 or a power of two of the scale (what a bit off would add), and scored by how far its positions are from the
//! paired ones (median and 95th percentile, in metres).
//!
//! What it answers ([`Outcome`]) holds only layouts, counts and errors in metres (an error over [`FAR_M`] only as
//! "over 10 km": a far-off longitude's error in kilometres would tell the latitude it was measured at), never a
//! position, a time or a place, so it can be passed on.
//!
//! The positions are read a day at a time through [`Positions`]: the engine's own db inside the server, or
//! [`db::Reader`] beside it, where a day the server keeps changing is skipped ([`db::DayRead::Changing`]) and
//! counted. Stored logs are only ever added whole (a finished log, then moved into its day's pack, never changed),
//! so they are read as they are; one that won't read is skipped and counted too.
//!
//! **In the server** ([`Engine::gps_fit_start`]): a background job per car, one fit at a time, with its progress
//! ([`FitStatus`]); kept in memory until the next fit of the car or a restart.
//!
//! **For a command beside the running server** ([`ask`], [`answer`], [`serve_requests`]): the command can't sign in,
//! so it asks through the data folder: a sealed request in `<data>/.gps-fit/` (a dot folder: an export or a move
//! never carries it), which the server's [`watch_requests`] takes, starting the same job, and answers with the job's
//! status next to it until it ends.

use crate::{Engine, lock, now_ms};
use model::gps::{GpsField, GpsLayout};
use model::{CarId, Sample, Stream};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// A frame and a position at most this far apart in time are a pair (ms).
pub const PAIR_MS: u64 = 2_000;
/// While the car stands: the positions before and after a frame, at most this far from it (ms) and at most
/// [`STILL_M`] apart, are where the car was.
pub const STILL_MS: u64 = 15 * 60_000;
pub const STILL_M: f64 = 30.0;
/// Pairs used at most (spread over the period).
pub const MAX_PAIRS: usize = 20_000;
/// Fewer pairs than this can't tell layouts apart.
pub const MIN_PAIRS: usize = 20;
/// Field widths tried.
pub const MIN_WIDTH: u8 = 24;
pub const MAX_WIDTH: u8 = 32;
/// Errors over this (m) are only said to be over it.
pub const FAR_M: f64 = 10_000.0;
/// A layout whose positions are at most this far from the paired ones (95th percentile, m) reads the frames right.
pub const GOOD_P95_M: f64 = 200.0;
/// The days read at most.
pub const MAX_DAYS: u32 = 366;
/// Degrees per step tried: decimal ones, binary ones (2^-n degrees), the semicircle (180 / 2^31), and the decimal
/// ones negated (a field that counts westward or southward).
pub const SCALES: &[f64] = &[
    1e-6,
    1e-7,
    1e-5,
    -1e-6,
    -1e-7,
    180.0 / 2_147_483_648.0,
    1.0 / 65_536.0,
    1.0 / 131_072.0,
    1.0 / 262_144.0,
    1.0 / 524_288.0,
    1.0 / 1_048_576.0,
    1.0 / 2_097_152.0,
    1.0 / 4_194_304.0,
    1.0 / 8_388_608.0,
    1.0 / 16_777_216.0,
    1.0 / 33_554_432.0,
    1.0 / 67_108_864.0,
];
/// The streams whose positions frames are paired with.
pub const REFERENCES: [Stream; 5] =
    [Stream::TeslamateLocal, Stream::TeslaApi, Stream::TeslafiApi, Stream::Teslamate, Stream::Teslafi];
const M_PER_DEG: f64 = 111_195.0;

/// A GPS frame and the position paired with it.
#[derive(Debug, Clone, Copy)]
pub struct Pair {
    /// The frame's 8 bytes read little-endian and big-endian.
    pub le: u64,
    pub be: u64,
    pub lat: f64,
    pub lon: f64,
}

impl Pair {
    pub fn new(payload: &[u8], lat: f64, lon: f64) -> Pair {
        let mut b = [0u8; 8];
        let n = payload.len().min(8);
        b[..n].copy_from_slice(&payload[..n]);
        Pair { le: u64::from_le_bytes(b), be: u64::from_be_bytes(b), lat, lon }
    }
}

/// Which coordinate a field is fitted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Lat,
    Lon,
}

impl Axis {
    fn of(self, p: &Pair) -> f64 {
        match self {
            Axis::Lat => p.lat,
            Axis::Lon => p.lon,
        }
    }

    /// Metres per degree there.
    fn metres(self, p: &Pair) -> f64 {
        match self {
            Axis::Lat => M_PER_DEG,
            Axis::Lon => M_PER_DEG * p.lat.to_radians().cos().abs(),
        }
    }
}

/// A layout of one field and how far its values are from the paired positions (m).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub field: GpsField,
    pub median_m: f64,
    pub p95_m: f64,
}

/// The `q` quantile (0 to 1) of values (reordered).
fn quantile(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let k = ((v.len() - 1) as f64 * q).round() as usize;
    *v.select_nth_unstable_by(k, f64::total_cmp).1
}

/// How far a field's values are from the pairs' (median, 95th percentile, m).
pub fn score(field: &GpsField, pairs: &[Pair], axis: Axis) -> (f64, f64) {
    let mut err: Vec<f64> = pairs
        .iter()
        .map(|p| {
            let v = field.raw_of(p.le, p.be) as f64 * field.scale + field.offset;
            (v - axis.of(p)).abs() * axis.metres(p)
        })
        .collect();
    let p95 = quantile(&mut err, 0.95);
    (quantile(&mut err, 0.5), p95)
}

/// The offsets a field may have: 0, and plus or minus a power of two of its scale (2^0 to 2^32 steps).
fn offsets(scale: f64) -> impl Iterator<Item = f64> {
    std::iter::once(0.0).chain((0..=32).flat_map(move |m| {
        let o = scale.abs() * 2f64.powi(m);
        [o, -o]
    }))
}

/// The offset of `offsets(scale)` nearest the median of what the pairs need.
fn snapped_offset(raws: &[i64], scale: f64, pairs: &[Pair], axis: Axis) -> f64 {
    let mut need: Vec<f64> = raws.iter().zip(pairs).map(|(&r, p)| axis.of(p) - r as f64 * scale).collect();
    let want = quantile(&mut need, 0.5);
    offsets(scale).min_by(|a, b| (a - want).abs().total_cmp(&(b - want).abs())).unwrap_or(0.0)
}

/// Every layout tried on the pairs, best first (by the 95th percentile; of those that do as well, the one with no
/// offset, then the DBC's own bits, then the widest, the smallest offset, Intel, signed). Layouts whose values never
/// change are left out.
pub fn search(pairs: &[Pair], axis: Axis) -> Vec<Fit> {
    if pairs.is_empty() {
        return Vec::new();
    }
    // a first look on a few pairs, then the layouts that are not far off on all of them
    let step = (pairs.len() / 256).max(1);
    let few: Vec<Pair> = pairs.iter().step_by(step).copied().collect();
    let mut found = Vec::new();
    for motorola in [false, true] {
        for start in 0u8..64 {
            for width in MIN_WIDTH..=MAX_WIDTH {
                for signed in [true, false] {
                    let base = GpsField { start, width, motorola, signed, scale: 1.0, offset: 0.0 };
                    if !base.valid() {
                        continue;
                    }
                    let raws: Vec<i64> = few.iter().map(|p| base.raw_of(p.le, p.be)).collect();
                    if raws.iter().all(|&r| r == raws[0]) {
                        continue;
                    }
                    for &scale in SCALES {
                        let offset = snapped_offset(&raws, scale, &few, axis);
                        let f = GpsField { scale, offset, ..base };
                        let (_, p95) = score(&f, &few, axis);
                        if p95 <= 5_000.0 {
                            found.push(f);
                        }
                    }
                }
            }
        }
    }
    let mut fits: Vec<Fit> = found
        .into_iter()
        .map(|f| {
            let raws: Vec<i64> = pairs.iter().map(|p| f.raw_of(p.le, p.be)).collect();
            let field = GpsField { offset: snapped_offset(&raws, f.scale, pairs, axis), ..f };
            let (median_m, p95_m) = score(&field, pairs, axis);
            Fit { field, median_m, p95_m }
        })
        .collect();
    // over a car's few hundred kilometres a field's top bits only repeat its sign, so narrower fields (and ones
    // with an offset) fit as well: of those, no offset first, then the DBC's own bits, then the widest
    let dbc = match axis {
        Axis::Lat => dbc_field("GPSLatitude04F"),
        Axis::Lon => dbc_field("GPSLongitude04F"),
    };
    let rank = |f: &Fit| {
        let g = f.field;
        let dbc_bits = dbc.is_some_and(|d| GpsField { offset: 0.0, ..d } == GpsField { offset: 0.0, ..g });
        (g.offset != 0.0, !dbc_bits, std::cmp::Reverse(g.width), g.offset.abs().to_bits(), g.motorola, !g.signed)
    };
    fits.sort_by(|a, b| {
        let same = (a.p95_m - b.p95_m).abs() <= 1.0 + 0.02 * a.p95_m.min(b.p95_m);
        if same { rank(a).cmp(&rank(b)).then(a.median_m.total_cmp(&b.median_m)) } else { a.p95_m.total_cmp(&b.p95_m) }
    });
    fits
}

/// The best layout and the best one that is not the same bits read another way (another start, byte order or
/// scale), when there are any.
pub fn best_two(fits: &[Fit]) -> (Option<Fit>, Option<Fit>) {
    let Some(&best) = fits.first() else { return (None, None) };
    let other = fits.iter().find(|f| {
        (f.field.start, f.field.motorola, f.field.scale.to_bits())
            != (best.field.start, best.field.motorola, best.field.scale.to_bits())
    });
    (Some(best), other.copied())
}

/// The DBC's own layout of 0x04F's signal (`GPSLatitude04F`, ...).
pub fn dbc_field(series: &str) -> Option<GpsField> {
    let m = dbc::Profile::Tesla3Y.dbc().messages.get(&dbc::INTERNAL_MESSAGE)?;
    let s = m.signals.iter().find(|s| s.series == series)?;
    Some(GpsField {
        start: u8::try_from(s.start).ok()?,
        width: u8::try_from(s.length).ok()?,
        motorola: !s.little,
        signed: s.signed,
        scale: s.factor,
        offset: s.offset,
    })
}

/// A log's Screenmate GPS frames (0x04F on an internal bus), one a second at most: (ms since 1970, 8 bytes).
pub fn internal_frames(log: &[u8], side: &serde_json::Value) -> Vec<(u64, [u8; 8])> {
    let buses: Vec<u8> =
        dbc::internal_buses(side).iter().filter_map(|i| i.strip_prefix("can").and_then(|n| n.parse().ok())).collect();
    let mut out: Vec<(u64, [u8; 8])> = Vec::new();
    if buses.is_empty() {
        return out;
    }
    let _ = logfmt::read_log(log, logfmt::Limits::default(), |l| {
        let logfmt::Line::Frame(f) = logfmt::Line::parse(l) else { return };
        if f.id != dbc::INTERNAL_MESSAGE || !buses.contains(&f.bus) {
            return;
        }
        let t = f.time_us / 1000;
        if out.last().is_some_and(|(u, _)| u / 1000 == t / 1000) {
            return;
        }
        let mut b = [0u8; 8];
        let p = f.payload();
        let n = p.len().min(8);
        b[..n].copy_from_slice(&p[..n]);
        out.push((t, b));
    });
    out
}

/// Pairs frames with positions (`refs`: (ms, lat, lon), sorted): the nearest within [`PAIR_MS`], else, while the
/// car stood, the one before when it and the one after (each within [`STILL_MS`]) are within [`STILL_M`].
pub fn pair(frames: &[(u64, [u8; 8])], refs: &[(u64, f64, f64)]) -> Vec<Pair> {
    let near = |a: &(u64, f64, f64), b: &(u64, f64, f64)| {
        let dy = (a.1 - b.1) * M_PER_DEG;
        let dx = (a.2 - b.2) * M_PER_DEG * a.1.to_radians().cos();
        dy.hypot(dx) <= STILL_M
    };
    let mut out = Vec::new();
    for &(t, b) in frames {
        let i = refs.partition_point(|r| r.0 < t);
        let before = i.checked_sub(1).map(|j| &refs[j]);
        let after = refs.get(i);
        let close = [before, after]
            .into_iter()
            .flatten()
            .filter(|r| r.0.abs_diff(t) <= PAIR_MS)
            .min_by_key(|r| r.0.abs_diff(t));
        let pick = close.or_else(|| match (before, after) {
            (Some(a), Some(z)) if t - a.0 <= STILL_MS && z.0 - t <= STILL_MS && near(a, z) => Some(a),
            _ => None,
        });
        if let Some(r) = pick {
            out.push(Pair::new(&b, r.1, r.2));
        }
    }
    if out.len() > MAX_PAIRS {
        let step = out.len().div_ceil(MAX_PAIRS);
        out = out.into_iter().step_by(step).collect();
    }
    out
}

/// `c7` or `7`.
pub fn parse_car(s: &str) -> Option<CarId> {
    s.strip_prefix('c').unwrap_or(s).parse().ok()
}

// ------------------------------------------------------------------------------------------------- the answer

/// An error as shown: whole metres, `None` when over [`FAR_M`] (or not known).
fn shown(m: f64) -> Option<f64> {
    (m.is_finite() && m <= FAR_M).then(|| m.round())
}

/// A layout of one field and how far its positions are from the paired ones (m; `None`: over 10 km).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldFit {
    pub layout: String,
    pub median_m: Option<f64>,
    pub p95_m: Option<f64>,
}

impl FieldFit {
    fn of(f: &GpsField, (median, p95): (f64, f64)) -> FieldFit {
        FieldFit { layout: f.to_string(), median_m: shown(median), p95_m: shown(p95) }
    }
}

/// One coordinate's layouts: the DBC's, the best found, and the best that is other bits.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AxisFit {
    pub dbc: Option<FieldFit>,
    pub best: Option<FieldFit>,
    pub runner_up: Option<FieldFit>,
}

/// How many positions of an input were paired from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceCount {
    /// The input's stream name (`teslamate-local`, `tesla-api`, ...).
    pub input: String,
    pub positions: usize,
}

/// What a fit found: counts, layouts and errors in metres only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub car: CarId,
    /// The days back from now it read.
    pub days: u32,
    /// Logs with the Screenmate's GPS frames; logs that would not read (left out).
    pub logs: usize,
    pub logs_skipped: usize,
    /// Days of positions read, and those left out because the server kept changing them while they were read.
    pub days_read: usize,
    pub days_skipped: usize,
    /// Frames paired with a position, and the positions of each input.
    pub pairs: usize,
    pub sources: Vec<SourceCount>,
    /// Enough pairs to tell layouts apart ([`MIN_PAIRS`]).
    pub enough: bool,
    /// How far the paired positions spread north-south and east-west (95th percentile from their middle, m;
    /// `None`: over 10 km), and whether that is too little to tell layouts apart well.
    pub spread_ns_m: Option<f64>,
    pub spread_ew_m: Option<f64>,
    pub weak: bool,
    pub lat: AxisFit,
    pub lon: AxisFit,
    /// The best of each, as `PUT /cars/{car}/gps-layout` takes it, and how far its positions are from the paired
    /// ones (both coordinates; median and 95th percentile, m).
    pub layout: Option<String>,
    pub median_m: Option<f64>,
    pub p95_m: Option<f64>,
    /// It is the DBC's own layout (the frames are read right without a layout of the car's own).
    pub is_dbc: bool,
    /// Close enough to trust ([`GOOD_P95_M`], and not weak).
    pub good: bool,
}

impl Outcome {
    /// The layout found, as the car's setting takes it.
    pub fn found(&self) -> Option<GpsLayout> {
        self.layout.as_deref().and_then(|l| l.parse().ok())
    }

    /// The report as `sentry-mate gps-layout` prints it (no position, time or place).
    pub fn text(&self) -> String {
        let car = self.car;
        let mut s = String::new();
        let from: Vec<String> =
            self.sources.iter().filter(|c| c.positions > 0).map(|c| format!("{} {}", c.input, c.positions)).collect();
        s += &format!(
            "gps-layout c{car}: the last {} days, {} logs with the Screenmate's GPS frames (0x04F on its internal \
             bus), {} frames paired with the car's positions from its other inputs ({})\n",
            self.days,
            self.logs,
            self.pairs,
            if from.is_empty() { "none".into() } else { from.join(", ") }
        );
        if self.days_skipped > 0 {
            s += &format!(
                "{} of {} days of positions were left out: the server kept changing them while they were read.\n",
                self.days_skipped,
                self.days_read + self.days_skipped
            );
        }
        if self.logs_skipped > 0 {
            s += &format!("{} logs were left out: they would not read.\n", self.logs_skipped);
        }
        if !self.enough {
            s += "Too few pairs to tell layouts apart: let the car drive with the Screenmate and its other inputs on \
                  for a day, then run this again (or give --days more days).\n";
            return s;
        }
        s += &format!(
            "The paired positions spread {} north-south and {} east-west (95th percentile from their middle){}\n",
            metres(self.spread_ns_m),
            metres(self.spread_ew_m),
            if self.weak { ": little movement, so the fit is weak" } else { "" }
        );
        for (name, a) in [("latitude", &self.lat), ("longitude", &self.lon)] {
            s += &format!("\n{name}\n");
            let line = |label: &str, f: &FieldFit| {
                format!(
                    "  {label:<11} {:<26} median {:>10}   95th percentile {:>10}\n",
                    f.layout,
                    metres(f.median_m),
                    metres(f.p95_m)
                )
            };
            if let Some(f) = &a.dbc {
                s += &line("now (DBC)", f);
            }
            match &a.best {
                Some(b) => s += &line("best", b),
                None => s += "  best        no layout comes within 5 km of the paired positions\n",
            }
            match &a.runner_up {
                Some(o) => s += &line("runner-up", o),
                None if a.best.is_some() => s += "  runner-up   none: no other bits come within 5 km\n",
                None => {}
            }
        }
        if let Some(l) = &self.layout {
            s += &format!(
                "\nWith the best of each, a frame's position is {} from the paired one (median), {} at the 95th \
                 percentile.\n",
                metres(self.median_m),
                metres(self.p95_m)
            );
            if self.is_dbc {
                s += "That is the layout the DBC has: the Screenmate's frames are read right.\n";
            } else {
                s += &format!(
                    "\nlayout: {l}\n\nTo read this car's Screenmate GPS frames with it (an admin; the car's history is \
                     rebuilt): Apply under Find GPS layout on the car's Data health page, or PUT \
                     /api/v1/cars/c{car}/gps-layout with {{\"layout\": \"{l}\"}}. Check the numbers above first: a \
                     95th percentile of tens of metres is a layout, one of kilometres is not.\n"
                );
            }
        }
        s
    }
}

/// A distance for people: `4 m`, `1.4 km`; `None` (over 10 km): only that.
fn metres(m: Option<f64>) -> String {
    match m {
        None => "over 10 km".into(),
        Some(m) if m < 1_000.0 => format!("{m:.0} m"),
        Some(m) => format!("{:.1} km", m / 1_000.0),
    }
}

/// What was read for a fit.
#[derive(Debug, Clone, Default)]
pub struct Collected {
    pub pairs: Vec<Pair>,
    /// Positions of each input paired from (every one of [`REFERENCES`], in order).
    pub sources: Vec<(Stream, usize)>,
    pub logs: usize,
    pub logs_skipped: usize,
    pub days_read: usize,
    pub days_skipped: usize,
}

/// Fits the pairs read: every layout of each coordinate tried, the best of each, and how far they are off.
pub fn fit(c: &Collected, days: u32, car: CarId) -> Outcome {
    let pairs = &c.pairs;
    let mut o = Outcome {
        car,
        days,
        logs: c.logs,
        logs_skipped: c.logs_skipped,
        days_read: c.days_read,
        days_skipped: c.days_skipped,
        pairs: pairs.len(),
        sources: c.sources.iter().map(|(s, n)| SourceCount { input: s.dir_name().into(), positions: *n }).collect(),
        enough: pairs.len() >= MIN_PAIRS,
        spread_ns_m: None,
        spread_ew_m: None,
        weak: true,
        lat: AxisFit::default(),
        lon: AxisFit::default(),
        layout: None,
        median_m: None,
        p95_m: None,
        is_dbc: false,
        good: false,
    };
    if !o.enough {
        return o;
    }
    // how much the positions vary: a car that stood still the whole time can't tell layouts apart
    let spread = |axis: Axis| {
        let mut v: Vec<f64> = pairs.iter().map(|p| axis.of(p)).collect();
        let mid = quantile(&mut v, 0.5);
        let mut d: Vec<f64> = pairs.iter().map(|p| (axis.of(p) - mid).abs() * axis.metres(p)).collect();
        quantile(&mut d, 0.95)
    };
    let (ns, ew) = (spread(Axis::Lat), spread(Axis::Lon));
    (o.spread_ns_m, o.spread_ew_m, o.weak) = (shown(ns), shown(ew), ns.max(ew) < 500.0);
    let mut chosen = [None, None];
    for (i, (axis, series)) in [(Axis::Lat, "GPSLatitude04F"), (Axis::Lon, "GPSLongitude04F")].into_iter().enumerate() {
        let dbc = dbc_field(series).map(|f| FieldFit::of(&f, score(&f, pairs, axis)));
        let (best, other) = best_two(&search(pairs, axis));
        let a = AxisFit {
            dbc,
            best: best.map(|b| FieldFit::of(&b.field, (b.median_m, b.p95_m))),
            runner_up: other.map(|b| FieldFit::of(&b.field, (b.median_m, b.p95_m))),
        };
        if i == 0 {
            o.lat = a;
        } else {
            o.lon = a;
        }
        chosen[i] = best.map(|b| b.field);
    }
    if let [Some(lat), Some(lon)] = chosen {
        let l = GpsLayout { lat, lon, accuracy: None };
        let mut d: Vec<f64> = pairs
            .iter()
            .map(|p| {
                let dy = (lat.raw_of(p.le, p.be) as f64 * lat.scale + lat.offset - p.lat) * M_PER_DEG;
                let dx = (lon.raw_of(p.le, p.be) as f64 * lon.scale + lon.offset - p.lon) * Axis::Lon.metres(p);
                dy.hypot(dx)
            })
            .collect();
        let p95 = quantile(&mut d, 0.95);
        o.median_m = shown(quantile(&mut d, 0.5));
        o.p95_m = shown(p95);
        o.layout = Some(l.to_string());
        o.is_dbc = [dbc_field("GPSLatitude04F"), dbc_field("GPSLongitude04F")] == [Some(lat), Some(lon)];
        o.good = !o.weak && p95 <= GOOD_P95_M;
    }
    o
}

// ------------------------------------------------------------------------------------------------- reading

/// Where a fit reads the car's positions from, a day at a time.
pub trait Positions {
    /// The days (their UTC starts) a series has in `[from, to)`.
    fn days(&self, stream: Stream, from: u64, to: u64) -> Result<Vec<u64>, String>;
    /// A day's points in `[from, to)`, or why it was left out.
    fn day(&self, stream: Stream, day_ms: u64, from: u64, to: u64) -> db::DayRead<Sample>;
}

/// What a fit is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Waiting for another car's fit to end (one runs at a time).
    Queued,
    /// Reading the other inputs' positions (days).
    Positions,
    /// Reading the stored logs' GPS frames (logs).
    Logs,
    /// Trying the layouts.
    Fitting,
}

impl Phase {
    pub fn word(self) -> &'static str {
        match self {
            Phase::Queued => "waiting for another car's fit",
            Phase::Positions => "reading the other inputs' positions",
            Phase::Logs => "reading the logs' GPS frames",
            Phase::Fitting => "trying the layouts",
        }
    }
}

/// How far a fit is: its phase, and the days or logs done of those it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub phase: Phase,
    pub done: u64,
    pub total: u64,
}

/// Reads what a fit of the last `days` days (to `now`) needs: the positions of the car's other inputs through
/// `pos`, and the Screenmate's GPS frames of its keys' usable logs. Runs the store's reads on `rt` (call it from a
/// thread that is not one of the runtime's).
#[allow(clippy::too_many_arguments)]
pub fn collect(
    pos: &dyn Positions,
    store: &ingest::Store,
    judge: &ingest::Judge,
    rt: &tokio::runtime::Handle,
    keys: &[String],
    days: u32,
    now: u64,
    progress: &dyn Fn(Step),
) -> Result<Collected, String> {
    let to = now;
    let from = to.saturating_sub(u64::from(days) * db::DAY_MS);
    let mut c = Collected::default();
    // the positions of the car's other inputs, a day at a time
    let mut lists = Vec::new();
    for stream in REFERENCES {
        lists.push((stream, pos.days(stream, from, to).map_err(|e| format!("listing the days of {stream:?}: {e}"))?));
    }
    let total = lists.iter().map(|(_, d)| d.len() as u64).sum();
    let mut done = 0;
    progress(Step { phase: Phase::Positions, done, total });
    let mut refs: Vec<(u64, f64, f64)> = Vec::new();
    for (stream, list) in lists {
        let mut n = 0usize;
        for day in list {
            match pos.day(stream, day, from, to) {
                db::DayRead::Read(pts) => {
                    c.days_read += 1;
                    for s in pts {
                        if let (Some(la), Some(lo)) = (s.lat, s.lon)
                            && timeline::real_fix(la, lo)
                        {
                            refs.push((s.t_ms, la, lo));
                            n += 1;
                        }
                    }
                }
                db::DayRead::Changing(why) => {
                    c.days_skipped += 1;
                    tracing::debug!("GPS layout fit: a day of positions left out: {why}");
                }
            }
            done += 1;
            progress(Step { phase: Phase::Positions, done, total });
        }
        c.sources.push((stream, n));
    }
    refs.sort_by_key(|r| r.0);
    // the Screenmate's GPS frames of the car's logs in the period, a log a minute at most, newest first
    let most = usize::try_from(days).unwrap_or(usize::MAX).saturating_mul(1_500).saturating_add(100);
    let mut names = Vec::new();
    for key in keys {
        let usable = rt.block_on(judge.latest_usable(key, most)).map_err(|e| format!("listing the logs: {e}"))?;
        names.extend(usable.into_iter().filter(|n| {
            trust::LogName::parse(n)
                .and_then(|l| u64::try_from(l.started_ms).ok())
                .is_some_and(|t| (from..to).contains(&t))
        }));
    }
    let total = names.len() as u64;
    progress(Step { phase: Phase::Logs, done: 0, total });
    let mut frames = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let read = rt.block_on(async {
            let Some(log) = store.read(name).await? else { return Ok(None) };
            let side = store.read(&format!("{name}.json")).await?;
            Ok::<_, ingest::ApiError>(Some((log, side)))
        });
        match read {
            Ok(Some((log, side))) => {
                let side: serde_json::Value = side.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
                let f = internal_frames(&log, &side);
                if !f.is_empty() {
                    c.logs += 1;
                    frames.extend(f);
                }
            }
            // gone since it was listed (a log taken out): nothing to read
            Ok(None) => {}
            Err(e) => {
                c.logs_skipped += 1;
                tracing::debug!("GPS layout fit: a log left out: {e}");
            }
        }
        progress(Step { phase: Phase::Logs, done: i as u64 + 1, total });
    }
    frames.sort_by_key(|f| f.0);
    c.pairs = pair(&frames, &refs);
    Ok(c)
}

// ------------------------------------------------------------------------------------------------- the server's job

/// Where a fit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FitState {
    Running,
    Done,
    Failed,
}

/// A fit run by the server, and what it found once done.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FitStatus {
    pub id: u64,
    pub car: CarId,
    pub days: u32,
    pub state: FitState,
    /// While it runs: what it does, and the days or logs done of those it has.
    pub phase: Option<Phase>,
    pub done: u64,
    pub total: u64,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub error: Option<String>,
    pub outcome: Option<Outcome>,
}

impl FitStatus {
    /// How far it is, 0 to 100 (the phases weighed: positions to 30, logs to 90, then the fit).
    pub fn percent(&self) -> u32 {
        let part = if self.total == 0 { 0.0 } else { self.done.min(self.total) as f64 / self.total as f64 };
        let p = match (self.state, self.phase) {
            (FitState::Done | FitState::Failed, _) => 100.0,
            (_, None | Some(Phase::Queued)) => 0.0,
            (_, Some(Phase::Positions)) => 30.0 * part,
            (_, Some(Phase::Logs)) => 30.0 + 60.0 * part,
            (_, Some(Phase::Fitting)) => 90.0,
        };
        p.floor() as u32
    }
}

/// Why a fit didn't start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FitError {
    NoSuchCar,
    Other(String),
}

impl std::fmt::Display for FitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FitError::NoSuchCar => write!(f, "no such car"),
            FitError::Other(s) => write!(f, "{s}"),
        }
    }
}

/// The fits of the cars (the newest of each), kept in memory.
#[derive(Default)]
pub(crate) struct Fits {
    jobs: HashMap<CarId, FitStatus>,
    next: u64,
}

/// The engine's own db, read inside the server (nothing is skipped: its reads see one moment).
struct Live<'a> {
    e: &'a Engine,
    car: CarId,
}

impl Positions for Live<'_> {
    fn days(&self, stream: Stream, from: u64, to: u64) -> Result<Vec<u64>, String> {
        let days = self.e.db().days::<Sample>(self.car, stream).map_err(|e| e.to_string())?;
        Ok(days.into_iter().map(|d| d.start_ms).filter(|&d| d + db::DAY_MS > from && d < to).collect())
    }

    fn day(&self, stream: Stream, day_ms: u64, from: u64, to: u64) -> db::DayRead<Sample> {
        let (a, b) = (from.max(day_ms), to.min(day_ms + db::DAY_MS));
        match self.e.db().read_points::<Sample>(self.car, stream, a, b) {
            Ok(p) => db::DayRead::Read(p),
            Err(e) => db::DayRead::Changing(e.to_string()),
        }
    }
}

impl Engine {
    /// Starts finding the layout of the car's Screenmate GPS frame from the last `days` days (1 to [`MAX_DAYS`]),
    /// in the background; a fit of the car under way already is answered instead. One fit runs at a time (the
    /// others wait, [`Phase::Queued`]).
    pub fn gps_fit_start(&self, car: CarId, days: u32) -> Result<FitStatus, FitError> {
        let days = days.clamp(1, MAX_DAYS);
        let rec = self.db().car(car).map_err(|e| FitError::Other(e.to_string()))?.ok_or(FitError::NoSuchCar)?;
        let rt = tokio::runtime::Handle::try_current()
            .ok()
            .or_else(|| self.handle().ok())
            .ok_or_else(|| FitError::Other("the server is not running".into()))?;
        let status = {
            let mut fits = lock(&self.inner.gps_fits);
            if let Some(s) = fits.jobs.get(&car)
                && s.state == FitState::Running
            {
                return Ok(s.clone());
            }
            fits.next += 1;
            let s = FitStatus {
                id: fits.next,
                car,
                days,
                state: FitState::Running,
                phase: Some(Phase::Queued),
                done: 0,
                total: 0,
                started_ms: now_ms(),
                finished_ms: None,
                error: None,
                outcome: None,
            };
            fits.jobs.insert(car, s.clone());
            s
        };
        let e = self.clone();
        let id = status.id;
        let spawned = std::thread::Builder::new().name("gps-fit".into()).spawn(move || {
            let r = {
                let _one = lock(&e.inner.gps_fit_one);
                let set = |st: Step| {
                    e.gps_fit_update(car, id, |s| (s.phase, s.done, s.total) = (Some(st.phase), st.done, st.total))
                };
                let live = Live { e: &e, car };
                let keys = rec.vehicle_keys.clone();
                collect(&live, e.store(), e.judge(), &rt, &keys, days, now_ms(), &set).map(|c| {
                    set(Step { phase: Phase::Fitting, done: 0, total: 0 });
                    fit(&c, days, car)
                })
            };
            e.gps_fit_update(car, id, |s| {
                s.finished_ms = Some(now_ms());
                s.phase = None;
                match r {
                    Ok(o) => (s.state, s.outcome) = (FitState::Done, Some(o)),
                    Err(why) => (s.state, s.error) = (FitState::Failed, Some(why)),
                }
            });
        });
        if let Err(err) = spawned {
            self.gps_fit_update(car, id, |s| {
                (s.state, s.phase, s.finished_ms) = (FitState::Failed, None, Some(now_ms()));
                s.error = Some(format!("starting the fit: {err}"));
            });
        }
        Ok(self.gps_fit_status(car).unwrap_or(status))
    }

    fn gps_fit_update(&self, car: CarId, id: u64, f: impl FnOnce(&mut FitStatus)) {
        if let Some(s) = lock(&self.inner.gps_fits).jobs.get_mut(&car)
            && s.id == id
        {
            f(s);
        }
    }

    /// The car's newest fit (under way, or done since the server started).
    pub fn gps_fit_status(&self, car: CarId) -> Option<FitStatus> {
        lock(&self.inner.gps_fits).jobs.get(&car).cloned()
    }

    /// What else the server is busy with that a person waiting may want to know of: a full rebuild and how far it
    /// is (`rebuild 34 %`).
    pub fn busy_words(&self) -> Option<String> {
        let r = self.full_rebuild_status().ok().flatten()?;
        if !r.running() {
            return None;
        }
        let pct = |d: u64, t: u64| (t > 0).then(|| (d.min(t) * 100 / t) as u32);
        match pct(r.logs_done, r.logs_total).or_else(|| pct(r.days_done, r.days_total)) {
            Some(p) => Some(format!("rebuild {p} %")),
            None => Some("rebuild starting".into()),
        }
    }
}

// ------------------------------------------------------------------------------------------------- the command's way in

/// The folder of the requests and answers, in the data folder.
pub const DIR: &str = ".gps-fit";
/// How often the server looks for requests.
pub const WATCH_EVERY: Duration = Duration::from_secs(1);

/// A command's request for a fit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub car: CarId,
    pub days: u32,
    /// The command's own mark: the answer carries it back.
    pub nonce: String,
}

/// The server's answer to a request: the fit's status (`None`: no such car), and what else it is busy with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub nonce: String,
    pub status: Option<FitStatus>,
    pub busy: Option<String>,
}

fn request_name(car: CarId) -> String {
    format!("request-c{car}")
}

fn answer_name(car: CarId) -> String {
    format!("answer-c{car}")
}

fn file(data: &Path, name: &str) -> (PathBuf, String) {
    (data.join(DIR).join(name), format!("{DIR}/{name}"))
}

fn write_sealed<T: Serialize>(data: &Path, vault: &vault::Vault, name: &str, v: &T) -> Result<(), String> {
    std::fs::create_dir_all(data.join(DIR)).map_err(|e| e.to_string())?;
    let (path, label) = file(data, name);
    let b = serde_json::to_vec(v).map_err(|e| e.to_string())?;
    vault.write_file(&path, &label, &b).map_err(|e| e.to_string())
}

fn read_sealed<T: serde::de::DeserializeOwned>(data: &Path, vault: &vault::Vault, name: &str) -> Option<T> {
    let (path, label) = file(data, name);
    let b = vault.read_file(&path, &label).ok()??;
    serde_json::from_slice(&b).ok()
}

/// Asks the running server for a fit of the car (a command beside it). Returns the request's mark, which
/// [`answer`] looks for.
pub fn ask(data: &Path, vault: &vault::Vault, car: CarId, days: u32) -> Result<String, String> {
    let nonce = format!("{}-{}", std::process::id(), now_ms());
    write_sealed(data, vault, &request_name(car), &Request { car, days, nonce: nonce.clone() })?;
    Ok(nonce)
}

/// The server's answer to the request marked `nonce`, once it has one.
pub fn answer(data: &Path, vault: &vault::Vault, car: CarId, nonce: &str) -> Option<Answer> {
    read_sealed::<Answer>(data, vault, &answer_name(car)).filter(|a| a.nonce == nonce)
}

/// One look by the server: takes the requests waiting (starting their fits), then writes the answers of the
/// requests it has (`asked`: car to mark), dropping those whose fit ended. Returns how many requests it took.
pub fn serve_requests(e: &Engine, data: &Path, vault: &vault::Vault, asked: &mut HashMap<CarId, String>) -> usize {
    let mut took = 0;
    if let Ok(rd) = std::fs::read_dir(data.join(DIR)) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            let Some(car) = name.strip_prefix("request-c").and_then(|c| c.parse::<CarId>().ok()) else { continue };
            let r: Option<Request> = read_sealed(data, vault, &name);
            let _ = std::fs::remove_file(ent.path());
            let Some(r) = r.filter(|r| r.car == car) else { continue };
            took += 1;
            match e.gps_fit_start(car, r.days) {
                Ok(_) => {
                    asked.insert(car, r.nonce);
                }
                Err(err) => {
                    let status = None;
                    if err != FitError::NoSuchCar {
                        tracing::warn!("GPS layout fit asked by a command: {err}");
                    }
                    let a = Answer { nonce: r.nonce, status, busy: None };
                    if let Err(x) = write_sealed(data, vault, &answer_name(car), &a) {
                        tracing::warn!("GPS layout fit: the answer to a command could not be written: {x}");
                    }
                }
            }
        }
    }
    let busy = e.busy_words();
    asked.retain(|&car, nonce| {
        let status = e.gps_fit_status(car);
        let ended = status.as_ref().is_none_or(|s| s.state != FitState::Running);
        let a = Answer { nonce: nonce.clone(), status, busy: busy.clone() };
        if let Err(x) = write_sealed(data, vault, &answer_name(car), &a) {
            tracing::warn!("GPS layout fit: the answer to a command could not be written: {x}");
        }
        !ended
    });
    took
}

/// The server's watch of the commands' requests, every [`WATCH_EVERY`], until the runtime stops.
pub async fn watch_requests(e: Engine, data: PathBuf, vault: Arc<vault::Vault>) {
    let mut asked = HashMap::new();
    loop {
        let (e2, d2, v2) = (e.clone(), data.clone(), vault.clone());
        let mut a = std::mem::take(&mut asked);
        match tokio::task::spawn_blocking(move || {
            serve_requests(&e2, &d2, &v2, &mut a);
            a
        })
        .await
        {
            Ok(a) => asked = a,
            Err(err) => tracing::warn!("GPS layout fit: the watch of the commands' requests failed: {err}"),
        }
        tokio::time::sleep(WATCH_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    /// A made-up drive (straight lines around a made-up point in the open sea), one position a second.
    pub(crate) fn drive(n: usize) -> Vec<(f64, f64)> {
        (0..n)
            .map(|i| {
                let k = i as f64;
                (-30.0 + 0.0004 * k + 0.002 * (k / 40.0).sin(), -20.0 + 0.0003 * k + 0.002 * (k / 55.0).cos())
            })
            .collect()
    }

    /// A frame with the latitude and longitude encoded by `lat` and `lon`.
    pub(crate) fn frame(lat: &GpsField, lon: &GpsField, la: f64, lo: f64) -> [u8; 8] {
        let mut le = 0u64;
        let mut be = 0u64;
        for (f, v) in [(lat, la), (lon, lo)] {
            let raw = ((v - f.offset) / f.scale).round() as i64;
            let bits = (raw as u64) & ((1u64 << f.width) - 1);
            if f.motorola {
                let l = model::gps::motorola_index(f.start);
                be |= bits << (64 - l - u32::from(f.width));
            } else {
                le |= bits << f.start;
            }
        }
        let mut b = le.to_le_bytes();
        for (x, y) in b.iter_mut().zip(be.to_be_bytes()) {
            *x |= y;
        }
        b
    }

    fn pairs_for(lat: &GpsField, lon: &GpsField, n: usize) -> Vec<Pair> {
        drive(n).into_iter().map(|(la, lo)| Pair::new(&frame(lat, lon, la, lo), la, lo)).collect()
    }

    fn outcome(pairs: Vec<Pair>, sources: Vec<(Stream, usize)>, logs: usize, car: CarId) -> Outcome {
        fit(&Collected { pairs, sources, logs, ..Collected::default() }, 30, car)
    }

    #[test]
    fn the_dbc_layout_is_found_when_the_frames_follow_it() {
        let (lat, lon) = (dbc_field("GPSLatitude04F").unwrap(), dbc_field("GPSLongitude04F").unwrap());
        let p = pairs_for(&lat, &lon, 400);
        let (best, _) = best_two(&search(&p, Axis::Lon));
        let best = best.unwrap();
        assert_eq!(best.field, lon, "{best:?}");
        assert!(best.p95_m < 1.0);
        let o = outcome(p, vec![(Stream::TeslamateLocal, 400)], 3, 1);
        assert!(o.text().contains("That is the layout the DBC has"), "{}", o.text());
        assert!(o.is_dbc && o.good);
        assert_eq!(o.found().unwrap().lat, lat);
    }

    #[test]
    fn a_longitude_one_bit_off_and_one_moved_are_found() {
        let lat = dbc_field("GPSLatitude04F").unwrap();
        // the frames carry the longitude 2^24 steps low (what a decoder reading them right would show 16.78 degrees
        // west): the DBC's bits with an offset of 2^24 steps
        let low = GpsField { offset: 16.777216, ..dbc_field("GPSLongitude04F").unwrap() };
        let p = pairs_for(&lat, &low, 400);
        let now = score(&dbc_field("GPSLongitude04F").unwrap(), &p, Axis::Lon);
        assert!(now.0 > 1_000_000.0, "the DBC's layout is 1,400 km off: {now:?}");
        let best = best_two(&search(&p, Axis::Lon)).0.unwrap().field;
        assert_eq!(GpsField { offset: 0.0, ..best }, GpsField { offset: 0.0, ..low }, "{best:?}");
        assert!((best.offset - 16.777216).abs() < 1e-9, "{best:?}");
        let o = outcome(p, vec![(Stream::TeslamateLocal, 400)], 3, 1);
        let text = o.text();
        assert!(text.contains("over 10 km") && !text.contains("1616") && !text.contains("1618"), "{text}");
        assert_eq!(o.lon.dbc.as_ref().unwrap().median_m, None, "over 10 km is only said so");
        // another start bit, width and byte order, 1e-7 steps
        let moved = GpsField { start: 39, width: 32, motorola: true, signed: true, scale: 1e-7, offset: 0.0 };
        let lat2 = GpsField { start: 0, width: 28, motorola: false, signed: true, scale: 1e-6, offset: 0.0 };
        let p = pairs_for(&lat2, &moved, 400);
        let (best, runner) = best_two(&search(&p, Axis::Lon));
        assert_eq!(best.unwrap().field, moved, "{best:?} {runner:?}");
        let o = outcome(p, vec![(Stream::TeslafiApi, 400)], 2, 7);
        assert!(o.text().contains("PUT /api/v1/cars/c7/gps-layout"), "{}", o.text());
        assert!(!o.is_dbc);
        assert_eq!(o.found().unwrap().lon, moved);
    }

    #[test]
    fn the_report_never_shows_a_position() {
        let (lat, lon) = (dbc_field("GPSLatitude04F").unwrap(), dbc_field("GPSLongitude04F").unwrap());
        let p = pairs_for(&lat, &lon, 300);
        let o = outcome(p.clone(), vec![(Stream::TeslamateLocal, 300)], 1, 1);
        let json = serde_json::to_string(&o).unwrap();
        for bad in ["-30.", "-20.", "29.9", "19.9"] {
            assert!(!o.text().contains(bad), "{bad} in {}", o.text());
            assert!(!json.contains(bad), "{bad} in {json}");
        }
        let o = outcome(p[..5].to_vec(), vec![], 1, 1);
        assert!(o.text().contains("Too few pairs") && o.found().is_none() && !o.enough);
        // what was left out is said
        let o = fit(&Collected { days_read: 20, days_skipped: 3, logs_skipped: 1, ..Collected::default() }, 30, 1);
        assert!(o.text().contains("3 of 23 days of positions were left out"), "{}", o.text());
        assert!(o.text().contains("1 logs were left out"), "{}", o.text());
    }

    #[test]
    fn frames_pair_with_the_nearest_position_or_where_the_car_stood() {
        let b = [0u8; 8];
        let refs = [(10_000, 1.0, 2.0), (11_000, 1.00001, 2.0), (600_000, 1.00002, 2.0), (2_000_000, 5.0, 5.0)];
        let frames = [(10_900, b), (300_000, b), (1_500_000, b), (5_000_000, b)];
        let p = pair(&frames, &refs);
        // the second: the car stood between two positions 2 m apart; the third: the positions around it are far
        // apart; the last: none after it
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].lat, 1.00001);
        assert_eq!(p[1].lat, 1.00001);
    }

    #[test]
    fn the_percentage_weighs_the_phases() {
        let mut s = FitStatus {
            id: 1,
            car: 1,
            days: 30,
            state: FitState::Running,
            phase: Some(Phase::Positions),
            done: 5,
            total: 10,
            started_ms: 0,
            finished_ms: None,
            error: None,
            outcome: None,
        };
        assert_eq!(s.percent(), 15);
        (s.phase, s.done, s.total) = (Some(Phase::Logs), 1, 2);
        assert_eq!(s.percent(), 60);
        s.phase = Some(Phase::Queued);
        assert_eq!(s.percent(), 0);
        s.state = FitState::Done;
        assert_eq!(s.percent(), 100);
    }
}
