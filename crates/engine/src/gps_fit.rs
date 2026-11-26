//! Finding the layout of the Screenmate's own GPS frame (0x04F on its internal bus, `can3`): where it keeps latitude
//! and longitude, worked out from a car's stored logs and the positions its other inputs reported at the same
//! moments (the Tesla API, TeslaFi, local TeslaMate, and their imports). `sentry-mate gps-layout` and
//! `POST /api/v1/cars/{car}/gps-layout/fit` (the "Find GPS layout" button of Data health) both run it.
//!
//! **Pairs** ([`pair`]): each frame is paired with a position of one input: the nearest within [`PAIR_MS`], or,
//! while the car stood, the one before when it and the one after (of the same input) agree. A live input (local
//! TeslaMate, the Tesla API, TeslaFi's API) is taken first; an import only for a frame no live input pairs, and not
//! at all when its positions and the live inputs' at the same moments disagree ([`IMPORT_AGREE_M`]: its times are
//! off). Places the car stood at long are thinned to [`PER_PLACE`] pairs, so where it parked doesn't outweigh where
//! it drove.
//!
//! **Layouts** ([`search`]): every layout of a field is tried: start bit, width ([`MIN_WIDTH`]..=[`MAX_WIDTH`]),
//! Intel or Motorola, signed or unsigned, and for each
//! - a scale of [`SCALES`] with an offset of 0 or a power of two of the scale (what a bit off would add),
//! - a scale of [`SCALES`] with any offset (the median the pairs need),
//! - any straight line from the raw value to degrees, fitted to the pairs (a median slope, then least squares over
//!   the pairs near it): any scale and offset.
//!
//! The last two must explain how the car moved ([`EXPLAINS`], and positions that spread at least [`WEAK_M`]), so a
//! field that hardly changes can't pass for one with the place as its offset. Each is scored by how far its
//! positions are from the paired ones (median and 95th percentile, in metres); of those that do as well the plainer
//! wins (a power-of-two offset, then any offset, then any scale).
//!
//! **Outliers** ([`fit`]): a first look ranks the layouts by their median (which pairs of wrong moments can't move),
//! then the pairs whose error is more than [`TRIM_MADS`] median absolute deviations (at least [`TRIM_FLOOR_M`])
//! from it are left out, and the layouts are tried again on the rest. Each input's pairs are reported with their
//! own errors, so an input whose times are off shows.
//!
//! **The line and its drift**: for each coordinate the straight line of the best layout's bits is reported (slope,
//! intercept, residuals), and whether its offset stays the same over time, with the latitude, and with the value
//! itself ([`Drift`]).
//!
//! What it answers ([`Outcome`]) holds only layouts (their scales and offsets), counts and errors in metres (an
//! error over [`FAR_M`] only as "over 10 km": a far-off longitude's error in kilometres would tell the latitude it
//! was measured at), never a position, a time or a place, so it can be passed on.
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
/// Pairs of one place (about 100 m square) used at most, spread over the period.
pub const PER_PLACE: usize = 20;
const PLACE_DEG: f64 = 0.001;
/// Fewer pairs than this can't tell layouts apart.
pub const MIN_PAIRS: usize = 20;
/// Field widths tried.
pub const MIN_WIDTH: u8 = 24;
pub const MAX_WIDTH: u8 = 32;
/// Errors over this (m) are only said to be over it.
pub const FAR_M: f64 = 10_000.0;
/// A layout whose positions are farther than this from the paired ones (m, by what it is ranked on) is none.
pub const NEAR_M: f64 = 5_000.0;
/// A layout whose positions are at most this far from the paired ones (95th percentile, m) reads the frames right.
pub const GOOD_P95_M: f64 = 200.0;
/// Positions that spread less than this (95th percentile from their middle, m) can't tell layouts apart well, and
/// no offset or scale is fitted to them.
pub const WEAK_M: f64 = 500.0;
/// A fitted offset or scale must leave errors of at most this share of how far the positions spread (by what it is
/// ranked on): it explains how the car moved.
pub const EXPLAINS: f64 = 0.25;
/// An import whose positions and the live inputs' at the same moments (within [`PAIR_MS`]) are farther apart than
/// this (95th percentile, m) is left out: its times are off.
pub const IMPORT_AGREE_M: f64 = 500.0;
/// Outliers: a pair whose error is more than this many median absolute deviations (at least [`TRIM_FLOOR_M`]) from
/// the median of the first look's best layout (one within [`TRIM_FROM_M`] by its median) is left out.
pub const TRIM_MADS: f64 = 5.0;
pub const TRIM_FLOOR_M: f64 = 100.0;
pub const TRIM_FROM_M: f64 = 1_000.0;
/// An offset that changes by less than this (m) over the period, the latitudes and the values is constant.
pub const DRIFT_M: f64 = 20.0;
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
/// The streams whose positions frames are paired with: the live ones first, then the imports.
pub const REFERENCES: [Stream; 5] =
    [Stream::TeslamateLocal, Stream::TeslaApi, Stream::TeslafiApi, Stream::Teslamate, Stream::Teslafi];
const M_PER_DEG: f64 = 111_195.0;
/// A median absolute deviation as a standard deviation's estimate.
const MAD_SIGMA: f64 = 1.4826;

/// A GPS frame and the position paired with it.
#[derive(Debug, Clone, Copy)]
pub struct Pair {
    /// The frame's 8 bytes read little-endian and big-endian.
    pub le: u64,
    pub be: u64,
    pub lat: f64,
    pub lon: f64,
    /// When the frame was sent (ms since 1970; 0 when not known), and the input the position is of.
    pub t_ms: u64,
    pub source: Option<Stream>,
}

impl Pair {
    pub fn new(payload: &[u8], lat: f64, lon: f64) -> Pair {
        let mut b = [0u8; 8];
        let n = payload.len().min(8);
        b[..n].copy_from_slice(&payload[..n]);
        Pair { le: u64::from_le_bytes(b), be: u64::from_be_bytes(b), lat, lon, t_ms: 0, source: None }
    }

    /// The pair with its frame's time and the position's input.
    pub fn at(self, t_ms: u64, source: Stream) -> Pair {
        Pair { t_ms, source: Some(source), ..self }
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

    fn dbc(self) -> Option<GpsField> {
        match self {
            Axis::Lat => dbc_field("GPSLatitude04F"),
            Axis::Lon => dbc_field("GPSLongitude04F"),
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

/// What layouts are ranked on: the median (a first look, which pairs of wrong moments can't move) or the 95th
/// percentile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    Median,
    P95,
}

impl By {
    fn of(self, (median, p95): (f64, f64)) -> f64 {
        match self {
            By::Median => median,
            By::P95 => p95,
        }
    }
}

/// How plain a layout is: a scale of [`SCALES`] with an offset of 0 or a power of two of it, one of them with any
/// offset, or any scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Step,
    Offset,
    Line,
}

impl Kind {
    pub fn of(f: &GpsField) -> Kind {
        if !SCALES.contains(&f.scale) {
            return Kind::Line;
        }
        let near = |o: f64| (f.offset - o).abs() <= 1e-9 * o.abs().max(1.0);
        if offsets(f.scale).any(near) { Kind::Step } else { Kind::Offset }
    }
}

/// The `q` quantile (0 to 1) of values (reordered).
fn quantile(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let k = ((v.len() - 1) as f64 * q).round() as usize;
    *v.select_nth_unstable_by(k, f64::total_cmp).1
}

fn median(v: &mut [f64]) -> f64 {
    quantile(v, 0.5)
}

/// The middle of the values' middle half (the mean of their quartiles): steadier than the median where they are
/// spread evenly over a step (a value rounded to its field's steps), and as robust to a quarter of outliers.
fn midhinge(v: &mut [f64]) -> f64 {
    (quantile(v, 0.25) + quantile(v, 0.75)) / 2.0
}

/// A field's value in a frame.
fn value(f: &GpsField, p: &Pair) -> f64 {
    f.raw_of(p.le, p.be) as f64 * f.scale + f.offset
}

/// A field's error at each pair, signed (m; positive: the frame's value is north or east of the paired position).
fn errors(f: &GpsField, pairs: &[Pair], axis: Axis) -> Vec<f64> {
    pairs.iter().map(|p| (value(f, p) - axis.of(p)) * axis.metres(p)).collect()
}

/// How far a field's values are from the pairs' (median, 95th percentile, m).
pub fn score(field: &GpsField, pairs: &[Pair], axis: Axis) -> (f64, f64) {
    let mut err: Vec<f64> = errors(field, pairs, axis).into_iter().map(f64::abs).collect();
    let p95 = quantile(&mut err, 0.95);
    (median(&mut err), p95)
}

/// How far the pairs' positions spread along an axis (median and 95th percentile from their middle, m).
fn spread(pairs: &[Pair], axis: Axis) -> (f64, f64) {
    let mut v: Vec<f64> = pairs.iter().map(|p| axis.of(p)).collect();
    let mid = median(&mut v);
    let mut d: Vec<f64> = pairs.iter().map(|p| (axis.of(p) - mid).abs() * axis.metres(p)).collect();
    let p95 = quantile(&mut d, 0.95);
    (median(&mut d), p95)
}

/// Whether errors this far (median, 95th percentile) explain how positions that spread this far moved.
fn explains(err: (f64, f64), spread: (f64, f64), by: By) -> bool {
    spread.1 >= WEAK_M && by.of(err) <= EXPLAINS * by.of(spread)
}

/// The offsets a field may have: 0, and plus or minus a power of two of its scale (2^0 to 2^32 steps).
fn offsets(scale: f64) -> impl Iterator<Item = f64> {
    std::iter::once(0.0).chain((0..=32).flat_map(move |m| {
        let o = scale.abs() * 2f64.powi(m);
        [o, -o]
    }))
}

/// The offset the pairs need at a scale (the [`midhinge`] of theirs).
fn needed_offset(raws: &[i64], scale: f64, pairs: &[Pair], axis: Axis) -> f64 {
    let mut need: Vec<f64> = raws.iter().zip(pairs).map(|(&r, p)| axis.of(p) - r as f64 * scale).collect();
    midhinge(&mut need)
}

/// The offset of `offsets(scale)` nearest `want`.
fn snapped(scale: f64, want: f64) -> f64 {
    offsets(scale).min_by(|a, b| (a - want).abs().total_cmp(&(b - want).abs())).unwrap_or(0.0)
}

/// Degrees to 1e-7 (1 cm).
fn round_deg(x: f64) -> f64 {
    (x * 1e7).round() / 1e7
}

/// A number to 9 significant digits.
fn significant(x: f64) -> f64 {
    format!("{x:.8e}").parse().unwrap_or(x)
}

/// A robust slope of `ys` on `xs`: the median of the slopes between points half and a quarter of the list apart
/// (pairs whose `xs` are less than `min_dx` apart left out).
fn slope(xs: &[f64], ys: &[f64], min_dx: f64) -> Option<f64> {
    let n = xs.len().min(ys.len());
    let mut s = Vec::new();
    for gap in [n / 2, n / 4] {
        if gap == 0 {
            continue;
        }
        for i in 0..n - gap {
            let dx = xs[i + gap] - xs[i];
            if dx.abs() >= min_dx && dx != 0.0 {
                s.push((ys[i + gap] - ys[i]) / dx);
            }
        }
    }
    let m = median(&mut s);
    m.is_finite().then_some(m)
}

/// The straight line from a field's raw values (`raws`, of `pairs`) to the coordinate, robust to outliers: a median
/// slope and offset, then twice least squares over the pairs within [`TRIM_MADS`] median absolute deviations (at
/// least 30 m) of it. The slope to 9 significant digits, the offset (what the pairs need at it) to 1e-7°.
fn line(raws: &[i64], pairs: &[Pair], axis: Axis) -> Option<(f64, f64)> {
    let xs: Vec<f64> = raws.iter().map(|&r| r as f64).collect();
    let ys: Vec<f64> = pairs.iter().map(|p| axis.of(p)).collect();
    let mut s = slope(&xs, &ys, 1.0)?;
    let offset = |s: f64| {
        let mut need: Vec<f64> = xs.iter().zip(&ys).map(|(x, y)| y - s * x).collect();
        midhinge(&mut need)
    };
    let mut o = offset(s);
    for _ in 0..2 {
        let r: Vec<f64> = xs.iter().zip(&ys).zip(pairs).map(|((x, y), p)| (s * x + o - y) * axis.metres(p)).collect();
        let mut c = r.clone();
        let mid = median(&mut c);
        let mut dev: Vec<f64> = r.iter().map(|v| (v - mid).abs()).collect();
        let lim = (TRIM_MADS * MAD_SIGMA * median(&mut dev)).max(30.0);
        let keep: Vec<usize> = (0..r.len()).filter(|&i| (r[i] - mid).abs() <= lim).collect();
        if keep.len() < 2 {
            break;
        }
        let k = keep.len() as f64;
        let (mx, my) = (keep.iter().map(|&i| xs[i]).sum::<f64>() / k, keep.iter().map(|&i| ys[i]).sum::<f64>() / k);
        let (mut sxy, mut sxx) = (0.0, 0.0);
        for &i in &keep {
            sxy += (xs[i] - mx) * (ys[i] - my);
            sxx += (xs[i] - mx) * (xs[i] - mx);
        }
        if sxx <= 0.0 {
            break;
        }
        s = sxy / sxx;
        o = my - s * mx;
    }
    let s = significant(s);
    let o = round_deg(offset(s));
    (s.is_finite() && s != 0.0 && o.is_finite()).then_some((s, o))
}

/// Every layout of `kind`s tried on the pairs that comes within [`NEAR_M`] (by `by`), not yet ranked. Layouts whose
/// values never change are left out, and those with a fitted offset or scale that don't explain how the car moved.
fn candidates(pairs: &[Pair], axis: Axis, by: By) -> Vec<Fit> {
    if pairs.is_empty() {
        return Vec::new();
    }
    // a first look on a few pairs, then the layouts that are not far off on all of them
    let step = (pairs.len() / 256).max(1);
    let few: Vec<Pair> = pairs.iter().step_by(step).copied().collect();
    let few_spread = spread(&few, axis);
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
                    let mut take = |f: GpsField| {
                        let err = score(&f, &few, axis);
                        if by.of(err) <= NEAR_M && (Kind::of(&f) == Kind::Step || explains(err, few_spread, by)) {
                            found.push(f);
                        }
                    };
                    for &scale in SCALES {
                        let need = needed_offset(&raws, scale, &few, axis);
                        let snap = snapped(scale, need);
                        take(GpsField { scale, offset: snap, ..base });
                        let free = round_deg(need);
                        if (free - snap).abs() > 2.0 * scale.abs() {
                            take(GpsField { scale, offset: free, ..base });
                        }
                    }
                    if let Some((scale, offset)) = line(&raws, &few, axis) {
                        take(GpsField { scale, offset, ..base });
                    }
                }
            }
        }
    }
    let all_spread = spread(pairs, axis);
    found
        .into_iter()
        .filter_map(|f| {
            let raws: Vec<i64> = pairs.iter().map(|p| f.raw_of(p.le, p.be)).collect();
            let field = match Kind::of(&f) {
                Kind::Step => GpsField { offset: snapped(f.scale, needed_offset(&raws, f.scale, pairs, axis)), ..f },
                Kind::Offset => GpsField { offset: round_deg(needed_offset(&raws, f.scale, pairs, axis)), ..f },
                Kind::Line => line(&raws, pairs, axis).map_or(f, |(scale, offset)| GpsField { scale, offset, ..f }),
            };
            let (median_m, p95_m) = score(&field, pairs, axis);
            let fits = by.of((median_m, p95_m)) <= NEAR_M
                && (Kind::of(&field) == Kind::Step || explains((median_m, p95_m), all_spread, by));
            fits.then_some(Fit { field, median_m, p95_m })
        })
        .collect()
}

/// Ranks layouts, best first (by `by`; of those that do as well, the plainest kind, then the DBC's own bits, then
/// the widest, the smallest offset, Intel, signed).
fn rank(fits: &mut [Fit], axis: Axis, by: By) {
    // over a car's few hundred kilometres a field's top bits only repeat its sign, so narrower fields (and ones
    // with an offset) fit as well: of those, the plainest first, then the DBC's own bits, then the widest
    let dbc = axis.dbc();
    let key = |f: &Fit| {
        let g = f.field;
        let dbc_bits =
            dbc.is_some_and(|d| (d.start, d.width, d.motorola, d.signed) == (g.start, g.width, g.motorola, g.signed));
        (Kind::of(&g), !dbc_bits, std::cmp::Reverse(g.width), g.offset.abs().to_bits(), g.motorola, !g.signed)
    };
    let m = |f: &Fit| by.of((f.median_m, f.p95_m));
    // tiers of those that do as well: from the best of each, within 1 m and 2 %
    fits.sort_by(|a, b| m(a).total_cmp(&m(b)));
    let mut tiers = Vec::with_capacity(fits.len());
    let (mut tier, mut from) = (0usize, f64::NAN);
    for f in fits.iter() {
        if from.is_nan() || m(f) > from + 1.0 + 0.02 * from {
            (tier, from) = (tier + 1, m(f));
        }
        tiers.push(tier);
    }
    let mut ranked: Vec<(usize, Fit)> = tiers.into_iter().zip(fits.iter().copied()).collect();
    ranked.sort_by(|(ta, a), (tb, b)| ta.cmp(tb).then_with(|| key(a).cmp(&key(b))).then(m(a).total_cmp(&m(b))));
    for (slot, (_, f)) in fits.iter_mut().zip(ranked) {
        *slot = f;
    }
}

/// Every layout tried on the pairs that comes within [`NEAR_M`] at the 95th percentile, best first.
pub fn search(pairs: &[Pair], axis: Axis) -> Vec<Fit> {
    search_by(pairs, axis, By::P95)
}

/// [`search`] ranked by `by`.
pub fn search_by(pairs: &[Pair], axis: Axis, by: By) -> Vec<Fit> {
    let mut fits = candidates(pairs, axis, by);
    rank(&mut fits, axis, by);
    fits
}

/// Layouts found by the median on the same pairs, ranked by the 95th percentile instead (those within [`NEAR_M`],
/// the fitted ones that explain how the car moved).
fn rerank(mut fits: Vec<Fit>, pairs: &[Pair], axis: Axis) -> Vec<Fit> {
    let sp = spread(pairs, axis);
    fits.retain(|f| {
        let e = (f.median_m, f.p95_m);
        e.1 <= NEAR_M && (Kind::of(&f.field) == Kind::Step || explains(e, sp, By::P95))
    });
    rank(&mut fits, axis, By::P95);
    fits
}

/// The best layout and the best one that is not the same bits read another way (another start, byte order or
/// scale of [`SCALES`]), when there are any.
pub fn best_two(fits: &[Fit]) -> (Option<Fit>, Option<Fit>) {
    let Some(&best) = fits.first() else { return (None, None) };
    let other = fits.iter().find(|f| {
        let (a, b) = (f.field, best.field);
        let fitted = Kind::of(&a) == Kind::Line || Kind::of(&b) == Kind::Line;
        (a.start, a.motorola) != (b.start, b.motorola) || (!fitted && a.scale.to_bits() != b.scale.to_bits())
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

/// A position of an input: (ms since 1970, latitude, longitude).
pub type Position = (u64, f64, f64);

fn apart_m(a: &Position, b: &Position) -> f64 {
    let dy = (a.1 - b.1) * M_PER_DEG;
    let dx = (a.2 - b.2) * M_PER_DEG * a.1.to_radians().cos();
    dy.hypot(dx)
}

/// The position of one input's (sorted) a frame at `t` pairs with: the nearest within [`PAIR_MS`], else, while the
/// car stood, the one before when it and the one after (each within [`STILL_MS`]) are within [`STILL_M`]. With
/// whether it was near in time, and how far in time it (or, standing, the one after it) is.
fn pick(t: u64, refs: &[Position]) -> Option<(Position, bool, u64)> {
    let i = refs.partition_point(|r| r.0 < t);
    let before = i.checked_sub(1).map(|j| &refs[j]);
    let after = refs.get(i);
    let close =
        [before, after].into_iter().flatten().filter(|r| r.0.abs_diff(t) <= PAIR_MS).min_by_key(|r| r.0.abs_diff(t));
    if let Some(r) = close {
        return Some((*r, true, r.0.abs_diff(t)));
    }
    match (before, after) {
        (Some(a), Some(z)) if t - a.0 <= STILL_MS && z.0 - t <= STILL_MS && apart_m(a, z) <= STILL_M => {
            Some((*a, false, z.0 - a.0))
        }
        _ => None,
    }
}

/// How far an import's positions are from the live inputs' (`live`, sorted) at the same moments (within
/// [`PAIR_MS`]; 95th percentile, m), when they share at least [`MIN_PAIRS`] moments.
fn disagreement(import: &[Position], live: &[Position]) -> Option<f64> {
    let mut d: Vec<f64> = import
        .iter()
        .filter_map(|r| {
            let i = live.partition_point(|l| l.0 < r.0);
            [i.checked_sub(1), Some(i)]
                .into_iter()
                .flatten()
                .filter_map(|j| live.get(j))
                .filter(|l| l.0.abs_diff(r.0) <= PAIR_MS)
                .min_by_key(|l| l.0.abs_diff(r.0))
                .map(|l| apart_m(r, l))
        })
        .collect();
    (d.len() >= MIN_PAIRS).then(|| quantile(&mut d, 0.95))
}

/// What pairing found.
#[derive(Debug, Clone, Default)]
pub struct Paired {
    /// The pairs used (at most [`PER_PLACE`] of a place, [`MAX_PAIRS`] in all), in the frames' order.
    pub pairs: Vec<Pair>,
    /// The frames paired, before they were thinned.
    pub paired: usize,
    /// The imports left out, with how far their positions were from the live inputs' (95th percentile, m).
    pub left_out: Vec<(Stream, f64)>,
}

/// Pairs frames (sorted) with the positions of the inputs (`refs`: each input's, sorted): with the live inputs'
/// first (the nearest in time of those within [`PAIR_MS`], else one where the car stood, see [`pick`]), then for a
/// frame none of them pairs, an import's that agrees with the live inputs ([`IMPORT_AGREE_M`]). Then places are
/// thinned to [`PER_PLACE`] pairs and all to [`MAX_PAIRS`], spread over the period.
pub fn pair(frames: &[(u64, [u8; 8])], refs: &[(Stream, Vec<Position>)]) -> Paired {
    let mut live: Vec<Position> =
        refs.iter().filter(|(s, _)| !s.is_imported()).flat_map(|(_, r)| r.iter().copied()).collect();
    live.sort_by_key(|r| r.0);
    let mut left_out = Vec::new();
    let mut used: Vec<(Stream, &[Position])> = Vec::new();
    for (s, r) in refs {
        if s.is_imported()
            && let Some(p95) = disagreement(r, &live)
            && p95 > IMPORT_AGREE_M
        {
            left_out.push((*s, p95));
            continue;
        }
        used.push((*s, r));
    }
    let mut out = Vec::new();
    for &(t, b) in frames {
        let mut best: Option<(Stream, Position, bool, u64)> = None;
        for imported in [false, true] {
            for &(s, r) in used.iter().filter(|(s, _)| s.is_imported() == imported) {
                if let Some((p, close, dt)) = pick(t, r) {
                    let better = best.is_none_or(|(_, _, bc, bdt)| (close && !bc) || (close == bc && dt < bdt));
                    if better {
                        best = Some((s, p, close, dt));
                    }
                }
            }
            if best.is_some() {
                break;
            }
        }
        if let Some((s, p, _, _)) = best {
            out.push(Pair::new(&b, p.1, p.2).at(t, s));
        }
    }
    let paired = out.len();
    Paired { pairs: thin(out), paired, left_out }
}

/// At most [`PER_PLACE`] pairs of a place and [`MAX_PAIRS`] in all, each spread over the period.
fn thin(pairs: Vec<Pair>) -> Vec<Pair> {
    let place = |p: &Pair| ((p.lat / PLACE_DEG).floor() as i64, (p.lon / PLACE_DEG).floor() as i64);
    let mut count: HashMap<(i64, i64), usize> = HashMap::new();
    for p in &pairs {
        *count.entry(place(p)).or_default() += 1;
    }
    let mut seen: HashMap<(i64, i64), usize> = HashMap::new();
    let mut out: Vec<Pair> = pairs
        .into_iter()
        .filter(|p| {
            let k = place(p);
            let every = count.get(&k).copied().unwrap_or(1).div_ceil(PER_PLACE);
            let i = seen.entry(k).or_default();
            *i += 1;
            (*i - 1).is_multiple_of(every)
        })
        .collect();
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

// ------------------------------------------------------------------------------------------------- the line

/// The straight line from a field's raw values to the coordinate, fitted to the pairs (robust to outliers).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineFit {
    /// The field's bits it was fitted on (`28|28@1-`).
    pub bits: String,
    /// The line: degrees per step and degrees at a raw 0, the layout `bits (slope,intercept)`. `None` when no line
    /// comes within 5 km or explains how the car moved ([`EXPLAINS`]): then the intercept could be a place.
    pub slope: Option<f64>,
    pub intercept: Option<f64>,
    /// The scale of [`SCALES`] nearest the slope, and how far the slope is from it (`slope / scale - 1`).
    pub nominal: Option<f64>,
    pub scale_error: Option<f64>,
    /// How far its positions are from the paired ones (m; `None`: over 10 km): median, 95th percentile, and the
    /// median absolute deviation of the errors (as a standard deviation).
    pub median_m: Option<f64>,
    pub p95_m: Option<f64>,
    pub mad_m: Option<f64>,
    /// Whether the offset at the nominal scale stays the same.
    pub drift: Option<Drift>,
}

/// Whether a field's offset at a scale stays the same: the errors of the field with the median offset, over time,
/// with the latitude and with the value itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Drift {
    /// The scale it was looked at with.
    pub scale: f64,
    /// The median error (signed, m; `None`: over 10 km) of each quarter of the pairs in time order.
    pub quarters_m: Vec<Option<f64>>,
    /// How much the error grows per km north (the longitude only) and per km along the coordinate itself (m).
    pub per_km_north_m: Option<f64>,
    pub per_km_value_m: Option<f64>,
    /// Within [`DRIFT_M`] over the period, the latitudes and the values the pairs have.
    pub constant: bool,
}

/// The scale of [`SCALES`] nearest a slope (same sign, by ratio).
fn nominal(s: f64) -> Option<f64> {
    SCALES
        .iter()
        .copied()
        .filter(|c| c.signum() == s.signum())
        .min_by(|a, b| (s / a).ln().abs().total_cmp(&(s / b).ln().abs()))
}

/// A signed distance as shown: whole metres (one decimal under 10), `None` when over [`FAR_M`].
fn shown_signed(m: f64) -> Option<f64> {
    (m.is_finite() && m.abs() <= FAR_M).then(|| if m.abs() < 10.0 { (m * 10.0).round() / 10.0 } else { m.round() })
}

/// Whether the offset of the field's bits at `scale` stays the same over the pairs (in time order).
fn drift(bits: &GpsField, scale: f64, pairs: &[Pair], axis: Axis) -> Drift {
    let raws: Vec<i64> = pairs.iter().map(|p| bits.raw_of(p.le, p.be)).collect();
    let f = GpsField { scale, offset: needed_offset(&raws, scale, pairs, axis), ..*bits };
    let err = errors(&f, pairs, axis);
    let n = err.len();
    let quarters_m = (0..4)
        .map(|q| {
            let mut part = err[q * n / 4..(q + 1) * n / 4].to_vec();
            shown_signed(median(&mut part))
        })
        .collect::<Vec<_>>();
    let mut lats: Vec<f64> = pairs.iter().map(|p| p.lat).collect();
    let mid_lat = median(&mut lats);
    let north: Vec<f64> = pairs.iter().map(|p| (p.lat - mid_lat) * M_PER_DEG / 1000.0).collect();
    let mut vals: Vec<f64> = pairs.iter().map(|p| value(&f, p)).collect();
    let mid_val = median(&mut vals);
    let along: Vec<f64> = pairs.iter().map(|p| (value(&f, p) - mid_val) * axis.metres(p) / 1000.0).collect();
    let km = |v: &[f64]| {
        let mut a: Vec<f64> = v.iter().map(|x| x.abs()).collect();
        2.0 * quantile(&mut a, 0.95)
    };
    let per = |x: &[f64]| slope(x, &err, 0.1);
    let per_km_north_m = if axis == Axis::Lon { per(&north) } else { None };
    let per_km_value_m = per(&along);
    let mut all = err.clone();
    let mid = median(&mut all);
    let constant = quarters_m.iter().all(|q| q.is_some_and(|q| (q - mid).abs() <= DRIFT_M))
        && per_km_north_m.is_none_or(|s| s.abs() * km(&north) <= DRIFT_M)
        && per_km_value_m.is_none_or(|s| s.abs() * km(&along) <= DRIFT_M);
    let round = |s: f64| (s * 100.0).round() / 100.0;
    Drift {
        scale,
        quarters_m,
        per_km_north_m: per_km_north_m.map(round),
        per_km_value_m: per_km_value_m.map(round),
        constant,
    }
}

/// The line of a field's bits on the pairs, and whether its offset drifts.
fn line_fit(bits: &GpsField, pairs: &[Pair], axis: Axis) -> LineFit {
    let base = GpsField { scale: 1.0, offset: 0.0, ..*bits };
    let mut out = LineFit {
        bits: base.to_string().split(' ').next().unwrap_or_default().to_string(),
        slope: None,
        intercept: None,
        nominal: None,
        scale_error: None,
        median_m: None,
        p95_m: None,
        mad_m: None,
        drift: None,
    };
    let raws: Vec<i64> = pairs.iter().map(|p| base.raw_of(p.le, p.be)).collect();
    let Some((s, o)) = line(&raws, pairs, axis) else { return out };
    let f = GpsField { scale: s, offset: o, ..base };
    let err = score(&f, pairs, axis);
    if err.1 > NEAR_M || !explains(err, spread(pairs, axis), By::P95) {
        return out;
    }
    let mut e = errors(&f, pairs, axis);
    let mid = median(&mut e.clone());
    let mut dev: Vec<f64> = e.iter_mut().map(|v| (*v - mid).abs()).collect();
    (out.slope, out.intercept) = (Some(s), Some(o));
    out.nominal = nominal(s);
    out.scale_error = out.nominal.map(|n| significant(s / n - 1.0));
    (out.median_m, out.p95_m, out.mad_m) = (shown(err.0), shown(err.1), shown(MAD_SIGMA * median(&mut dev)));
    out.drift = out.nominal.map(|n| drift(&base, n, pairs, axis));
    out
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

/// One coordinate's layouts: the DBC's, the best found, the best that is other bits, and the straight line of the
/// best's bits (the DBC's when there is none).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AxisFit {
    pub dbc: Option<FieldFit>,
    pub best: Option<FieldFit>,
    pub runner_up: Option<FieldFit>,
    #[serde(default)]
    pub line: Option<LineFit>,
}

/// An input's positions and pairs, and how far its pairs are from the frames read with the best layouts (the DBC's
/// for a coordinate that has none).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceCount {
    /// The input's stream name (`teslamate-local`, `tesla-api`, ...).
    pub input: String,
    pub positions: usize,
    /// Its pairs used, and those of them left out as outliers.
    #[serde(default)]
    pub pairs: usize,
    #[serde(default)]
    pub trimmed: usize,
    /// Median and 95th percentile of its pairs' errors (m; `None`: over 10 km, or no pairs).
    #[serde(default)]
    pub lat_median_m: Option<f64>,
    #[serde(default)]
    pub lat_p95_m: Option<f64>,
    #[serde(default)]
    pub lon_median_m: Option<f64>,
    #[serde(default)]
    pub lon_p95_m: Option<f64>,
    /// Why none of its positions were paired with (an import whose times are off), when so.
    #[serde(default)]
    pub left_out: Option<String>,
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
    /// Frames paired with a position (used, after places were thinned), and the positions of each input.
    pub pairs: usize,
    pub sources: Vec<SourceCount>,
    /// Frames paired before places were thinned ([`PER_PLACE`]), and pairs left out as outliers.
    #[serde(default)]
    pub paired: usize,
    #[serde(default)]
    pub trimmed: usize,
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
            self.paired.max(self.pairs),
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
            "{} pairs used (at most {PER_PLACE} of a place), {} of them left out as outliers (over {TRIM_MADS} \
             median absolute deviations, and {TRIM_FLOOR_M} m, from the first look's best layout)\n",
            self.pairs, self.trimmed
        );
        s += "Pairs per input (how far from the frames read with the best layouts: median, 95th percentile):\n";
        for c in self.sources.iter().filter(|c| c.positions > 0) {
            match &c.left_out {
                Some(why) => s += &format!("  {:<16} {:>8} positions   left out: {why}\n", c.input, c.positions),
                None => {
                    s += &format!(
                        "  {:<16} {:>8} positions {:>7} pairs ({} outliers)   latitude {}, {}   longitude {}, {}\n",
                        c.input,
                        c.positions,
                        c.pairs,
                        c.trimmed,
                        maybe(c.pairs, c.lat_median_m),
                        maybe(c.pairs, c.lat_p95_m),
                        maybe(c.pairs, c.lon_median_m),
                        maybe(c.pairs, c.lon_p95_m),
                    )
                }
            }
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
            if let Some(l) = &a.line {
                s += &line_text(l);
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

/// The `line` and `drift` rows of a coordinate.
fn line_text(l: &LineFit) -> String {
    let (Some(slope), Some(intercept)) = (l.slope, l.intercept) else {
        return format!("  line        {}: no straight line from its raw values fits the paired positions\n", l.bits);
    };
    let mut s = format!(
        "  line        {} ({},{}): slope {}° per step{}, intercept {}°; errors median {}, 95th percentile {}, MAD \
         {}\n",
        l.bits,
        num(slope),
        num(intercept),
        num(slope),
        match (l.nominal, l.scale_error) {
            (Some(n), Some(e)) => format!(" ({:+.4} % from {n:e})", e * 100.0),
            _ => String::new(),
        },
        num(intercept),
        metres(l.median_m),
        metres(l.p95_m),
        metres(l.mad_m),
    );
    if let Some(d) = &l.drift {
        let q: Vec<String> = d.quarters_m.iter().map(|q| signed(*q)).collect();
        let per = |v: Option<f64>, what: &str| v.map(|v| format!("; {v:+.2} m per km {what}")).unwrap_or_default();
        s += &format!(
            "  offset      at {:e}, by quarter of the period {}{}{}: {}\n",
            d.scale,
            q.join(", "),
            per(d.per_km_north_m, "north"),
            per(d.per_km_value_m, "along the value"),
            if d.constant { "constant" } else { "it drifts (more than 20 m): the scale or the field is not this one" }
        );
    }
    s
}

/// A number as short as it stays exact.
fn num(x: f64) -> String {
    let plain = format!("{x}");
    let sci = format!("{x:e}");
    if sci.len() < plain.len() { sci } else { plain }
}

/// A distance for people: `4 m`, `1.4 km`; `None` (over 10 km): only that.
fn metres(m: Option<f64>) -> String {
    match m {
        None => "over 10 km".into(),
        Some(m) if m < 1_000.0 => format!("{m:.0} m"),
        Some(m) => format!("{:.1} km", m / 1_000.0),
    }
}

/// A signed distance: `+4 m`, `-0.4 m`, `+1.4 km`; `None`: over 10 km.
fn signed(m: Option<f64>) -> String {
    match m {
        None => "over 10 km".into(),
        Some(m) if m.abs() < 10.0 => format!("{m:+.1} m"),
        Some(m) if m.abs() < 1_000.0 => format!("{m:+.0} m"),
        Some(m) => format!("{:+.1} km", m / 1_000.0),
    }
}

/// [`metres`], or `-` without pairs.
fn maybe(pairs: usize, m: Option<f64>) -> String {
    if pairs == 0 { "-".into() } else { metres(m) }
}

/// What was read for a fit.
#[derive(Debug, Clone, Default)]
pub struct Collected {
    pub pairs: Vec<Pair>,
    /// Frames paired before places were thinned (`pairs.len()` when 0).
    pub paired: usize,
    /// Positions of each input read (every one of [`REFERENCES`], in order).
    pub sources: Vec<(Stream, usize)>,
    /// The imports left out (their times are off), with how far their positions were from the live inputs'.
    pub left_out: Vec<(Stream, f64)>,
    pub logs: usize,
    pub logs_skipped: usize,
    pub days_read: usize,
    pub days_skipped: usize,
}

/// The pairs to leave out: those whose error is more than [`TRIM_MADS`] median absolute deviations (at least
/// [`TRIM_FLOOR_M`]) from the median of a coordinate's first-look best (when it is within [`TRIM_FROM_M`]).
fn outliers(pairs: &[Pair], firsts: &[(Axis, Option<&Fit>)]) -> Vec<bool> {
    let mut out = vec![false; pairs.len()];
    for &(axis, f) in firsts {
        let Some(f) = f.filter(|f| f.median_m <= TRIM_FROM_M) else { continue };
        let e = errors(&f.field, pairs, axis);
        let mid = median(&mut e.clone());
        let mut dev: Vec<f64> = e.iter().map(|v| (v - mid).abs()).collect();
        let lim = (TRIM_MADS * MAD_SIGMA * median(&mut dev)).max(TRIM_FLOOR_M);
        for (o, v) in out.iter_mut().zip(&e) {
            *o |= (v - mid).abs() > lim;
        }
    }
    out
}

/// Fits the pairs read: a first look ranked by the median, the outliers left out, then every layout of each
/// coordinate tried on the rest, the best of each, the straight line of its bits, and how far they are off.
pub fn fit(c: &Collected, days: u32, car: CarId) -> Outcome {
    let all = &c.pairs;
    let mut o = Outcome {
        car,
        days,
        logs: c.logs,
        logs_skipped: c.logs_skipped,
        days_read: c.days_read,
        days_skipped: c.days_skipped,
        pairs: all.len(),
        sources: c
            .sources
            .iter()
            .map(|(s, n)| SourceCount {
                input: s.dir_name().into(),
                positions: *n,
                pairs: 0,
                trimmed: 0,
                lat_median_m: None,
                lat_p95_m: None,
                lon_median_m: None,
                lon_p95_m: None,
                left_out: c.left_out.iter().find(|(l, _)| l == s).map(|(_, m)| {
                    format!(
                        "its positions are {} from the live inputs' at the same moments (95th percentile): its times \
                         are off",
                        metres(shown(*m))
                    )
                }),
            })
            .collect(),
        paired: c.paired.max(all.len()),
        trimmed: 0,
        enough: all.len() >= MIN_PAIRS,
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
    // a first look by the median, which pairs of wrong moments can't move; the pairs far from it are left out
    let axes = [Axis::Lat, Axis::Lon];
    let mut first: Vec<Vec<Fit>> = axes.iter().map(|&a| search_by(all, a, By::Median)).collect();
    let out = outliers(all, &[(Axis::Lat, first[0].first()), (Axis::Lon, first[1].first())]);
    let mut kept: Vec<Pair> = all.iter().zip(&out).filter(|(_, o)| !**o).map(|(p, _)| *p).collect();
    if kept.len() < MIN_PAIRS {
        kept = all.clone();
    }
    o.trimmed = all.len() - kept.len();
    // how much the positions vary: a car that stood still the whole time can't tell layouts apart
    let (ns, ew) = (spread(&kept, Axis::Lat).1, spread(&kept, Axis::Lon).1);
    (o.spread_ns_m, o.spread_ew_m, o.weak) = (shown(ns), shown(ew), ns.max(ew) < WEAK_M);
    let mut chosen = [None, None];
    for (i, &axis) in axes.iter().enumerate() {
        let dbc = axis.dbc();
        // nothing left out: the first look's layouts, by the 95th percentile
        let fits =
            if o.trimmed == 0 { rerank(std::mem::take(&mut first[i]), &kept, axis) } else { search(&kept, axis) };
        let (best, other) = best_two(&fits);
        let bits = best.map(|b| b.field).or(dbc);
        let a = AxisFit {
            dbc: dbc.map(|f| FieldFit::of(&f, score(&f, &kept, axis))),
            best: best.map(|b| FieldFit::of(&b.field, (b.median_m, b.p95_m))),
            runner_up: other.map(|b| FieldFit::of(&b.field, (b.median_m, b.p95_m))),
            line: bits.map(|b| line_fit(&b, &kept, axis)),
        };
        if i == 0 {
            o.lat = a;
        } else {
            o.lon = a;
        }
        chosen[i] = best.map(|b| b.field);
    }
    // each input's pairs, read with the best of each (the DBC's when none)
    let read = [chosen[0].or(Axis::Lat.dbc()), chosen[1].or(Axis::Lon.dbc())];
    for sc in &mut o.sources {
        let idx: Vec<usize> =
            (0..all.len()).filter(|&i| all[i].source.is_some_and(|s| s.dir_name() == sc.input)).collect();
        sc.pairs = idx.len();
        sc.trimmed = idx.iter().filter(|&&i| out[i]).count();
        let mine: Vec<Pair> = idx.iter().map(|&i| all[i]).collect();
        if mine.is_empty() {
            continue;
        }
        if let Some(f) = read[0] {
            let (m, p) = score(&f, &mine, Axis::Lat);
            (sc.lat_median_m, sc.lat_p95_m) = (shown(m), shown(p));
        }
        if let Some(f) = read[1] {
            let (m, p) = score(&f, &mine, Axis::Lon);
            (sc.lon_median_m, sc.lon_p95_m) = (shown(m), shown(p));
        }
    }
    if let [Some(lat), Some(lon)] = chosen {
        let l = GpsLayout { lat, lon, accuracy: None };
        let mut d: Vec<f64> = kept
            .iter()
            .map(|p| {
                let dy = (value(&lat, p) - p.lat) * M_PER_DEG;
                let dx = (value(&lon, p) - p.lon) * Axis::Lon.metres(p);
                dy.hypot(dx)
            })
            .collect();
        let p95 = quantile(&mut d, 0.95);
        o.median_m = shown(median(&mut d));
        o.p95_m = shown(p95);
        o.layout = Some(l.to_string());
        o.is_dbc = [Axis::Lat.dbc(), Axis::Lon.dbc()] == [Some(lat), Some(lon)];
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
    /// Why the fit stops here (the server is stopping, or its data moved away); `None`: it goes on.
    fn stopped(&self) -> Option<String> {
        None
    }
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
    /// The days of positions and the logs the whole fit reads (known from the first step: the bar is weighed by
    /// them), 0 before.
    pub positions_total: u64,
    pub logs_total: u64,
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
    let logs_total = names.len() as u64;
    // the positions of the car's other inputs, a day at a time
    let mut lists = Vec::new();
    for stream in REFERENCES {
        lists.push((stream, pos.days(stream, from, to).map_err(|e| format!("listing the days of {stream:?}: {e}"))?));
    }
    let total = lists.iter().map(|(_, d)| d.len() as u64).sum();
    let mut done = 0;
    let positions_total = total;
    progress(Step { phase: Phase::Positions, done, total, positions_total, logs_total });
    let mut refs: Vec<(Stream, Vec<Position>)> = Vec::new();
    for (stream, list) in lists {
        let mut n = 0usize;
        let mut mine = Vec::new();
        for day in list {
            if let Some(why) = pos.stopped() {
                return Err(why);
            }
            match pos.day(stream, day, from, to) {
                db::DayRead::Read(pts) => {
                    c.days_read += 1;
                    for s in pts {
                        if let (Some(la), Some(lo)) = (s.lat, s.lon)
                            && timeline::real_fix(la, lo)
                        {
                            mine.push((s.t_ms, la, lo));
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
            progress(Step { phase: Phase::Positions, done, total, positions_total, logs_total });
        }
        c.sources.push((stream, n));
        mine.sort_by_key(|r| r.0);
        refs.push((stream, mine));
    }
    let total = logs_total;
    progress(Step { phase: Phase::Logs, done: 0, total, positions_total, logs_total });
    let mut frames = Vec::new();
    // the logs read on the other cores, a few ahead of the one taken in order here: told on in their order, as
    // one by one, so what is found is the same; while a rebuild goes first (`progress` waits) the readers stop at
    // the logs ahead ([`read_frames`])
    let threads = crate::ahead::Ahead::threads().max(1);
    let r = read_frames(store, rt, &names, threads, |i, read| {
        if let Some(why) = pos.stopped() {
            return Err(why);
        }
        match read {
            Ok(Some(f)) => {
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
        progress(Step { phase: Phase::Logs, done: i as u64 + 1, total, positions_total, logs_total });
        Ok(())
    });
    r?;
    frames.sort_by_key(|f| f.0);
    let p = pair(&frames, &refs);
    (c.pairs, c.paired, c.left_out) = (p.pairs, p.paired, p.left_out);
    Ok(c)
}

/// A log's Screenmate GPS frames ([`internal_frames`]), read from the store (`None`: not stored).
type Frames = Result<Option<Vec<(u64, [u8; 8])>>, ingest::ApiError>;

/// Logs read ahead at most, per thread.
const FRAMES_AHEAD: usize = 4;

/// Reads the GPS frames of `names` on `threads` threads, and hands each log's to `each` in their order (its index,
/// what it read); `each` stopping (an error) stops the readers after the logs they are on. They read at most
/// [`FRAMES_AHEAD`] logs a thread ahead of the one `each` has, so they wait while it does.
fn read_frames(
    store: &ingest::Store,
    rt: &tokio::runtime::Handle,
    names: &[String],
    threads: usize,
    mut each: impl FnMut(usize, Frames) -> Result<(), String>,
) -> Result<(), String> {
    let read = |name: &str| -> Frames {
        rt.block_on(async {
            let Some(log) = store.read(name).await? else { return Ok(None) };
            let side = store.read(&format!("{name}.json")).await?;
            let side: serde_json::Value = side.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
            Ok(Some(internal_frames(&log, &side)))
        })
    };
    if threads <= 1 || names.len() < 2 {
        for (i, name) in names.iter().enumerate() {
            each(i, read(name))?;
        }
        return Ok(());
    }
    struct State {
        next: usize,
        wanted: usize,
        done: std::collections::HashMap<usize, Frames>,
        stop: bool,
    }
    let state = std::sync::Mutex::new(State { next: 0, wanted: 0, done: HashMap::new(), stop: false });
    let moved = std::sync::Condvar::new();
    let ahead = threads * FRAMES_AHEAD;
    let wait = |st| moved.wait(st).unwrap_or_else(std::sync::PoisonError::into_inner);
    std::thread::scope(|s| {
        for t in 0..threads.min(names.len()) {
            let (state, moved, read) = (&state, &moved, &read);
            let worker = move || {
                loop {
                    let i = {
                        let mut st = lock(state);
                        loop {
                            if st.stop || st.next >= names.len() {
                                return;
                            }
                            if st.next < st.wanted + ahead {
                                break;
                            }
                            st = moved.wait(st).unwrap_or_else(std::sync::PoisonError::into_inner);
                        }
                        st.next += 1;
                        st.next - 1
                    };
                    let r = read(&names[i]);
                    lock(state).done.insert(i, r);
                    moved.notify_all();
                }
            };
            // (a thread that can't start leaves its logs to the others)
            let _ = std::thread::Builder::new().name(format!("gps-fit-read-{t}")).spawn_scoped(s, worker);
        }
        let r = (|| {
            for (i, name) in names.iter().enumerate() {
                let got = {
                    let mut st = lock(&state);
                    loop {
                        if let Some(r) = st.done.remove(&i) {
                            break Some(r);
                        }
                        // taken by no thread yet (none started): read here
                        if st.next <= i {
                            st.next = i + 1;
                            break None;
                        }
                        st = wait(st);
                    }
                };
                let got = got.unwrap_or_else(|| read(name));
                {
                    lock(&state).wanted = i + 1;
                    moved.notify_all();
                }
                each(i, got)?;
            }
            Ok(())
        })();
        lock(&state).stop = true;
        moved.notify_all();
        r
    })
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
    /// The days of positions and the logs the fit reads in all (0 until known): its percent is weighed by them.
    #[serde(default)]
    pub positions_total: u64,
    #[serde(default)]
    pub logs_total: u64,
    /// When the logs began to be read: their speed, and the time left, are measured from it.
    #[serde(default)]
    pub logs_started_ms: Option<u64>,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub error: Option<String>,
    pub outcome: Option<Outcome>,
    /// While it waits for a rebuild to end ([`Engine::rebuild_running`]): why. Its phase and counts stay where
    /// they were, and it goes on from there.
    #[serde(default)]
    pub waiting: Option<String>,
    /// When the layout it found was applied from it, and by whom (an account's name).
    #[serde(default)]
    pub applied_ms: Option<u64>,
    #[serde(default)]
    pub applied_by: Option<String>,
    /// How long the fit waited for rebuilds so far (ms): not counted as the speed it reads at.
    #[serde(default)]
    pub waited_ms: u64,
}

impl FitStatus {
    /// How far it is, 0 to 100, by the work each step has: a day of positions and a log each count one unit (the
    /// logs are most of them, so reading 22 000 logs is nearly all of the bar), the fit itself a fiftieth of
    /// them (at least one). Running, it never reaches 100.
    pub fn percent(&self) -> u32 {
        let (p, l) = (self.positions_total, self.logs_total);
        let fit = ((p + l) / 50).max(1);
        let all = (p + l + fit) as f64;
        let part = |done: u64, total: u64| if total == 0 { 0 } else { done.min(total) };
        let units = match (self.state, self.phase) {
            (FitState::Done | FitState::Failed, _) => return 100,
            (_, None | Some(Phase::Queued)) => 0,
            (_, Some(Phase::Positions)) => part(self.done, p),
            (_, Some(Phase::Logs)) => p + part(self.done, l),
            (_, Some(Phase::Fitting)) => p + l,
        };
        ((units as f64 / all) * 100.0).floor() as u32
    }

    /// The time left (ms) while the logs are read, at the speed they were read so far; `None` before there is a
    /// speed (no log done yet, or none that took any time) and outside that step (the days of positions are a
    /// sliver of the work and the fit itself too short to tell).
    pub fn eta_ms(&self, now: u64) -> Option<u64> {
        if self.state != FitState::Running || self.phase != Some(Phase::Logs) || self.waiting.is_some() {
            return None;
        }
        // (the time spent waiting for a rebuild is not the speed of reading)
        let elapsed = now.saturating_sub(self.logs_started_ms?).saturating_sub(self.waited_ms);
        let done = self.done.min(self.logs_total);
        if done == 0 || elapsed == 0 {
            return None;
        }
        let left = self.logs_total - done;
        Some((left as f64 * elapsed as f64 / done as f64).round() as u64)
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
    pub(crate) jobs: HashMap<CarId, FitStatus>,
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

    fn stopped(&self) -> Option<String> {
        self.e.inner.stopping.load(std::sync::atomic::Ordering::SeqCst).then(|| STOPPED.to_string())
    }
}

/// Why a fit ended when the server stopped under it (a restart, or its data moved to another server).
pub const STOPPED: &str = "stopped: the server stopped (a restart, or its data moved to another server)";

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
            let id = self.gps_fit_next_id().map_err(|x| FitError::Other(x.to_string()))?;
            let s = FitStatus {
                id,
                car,
                days,
                state: FitState::Running,
                phase: Some(Phase::Queued),
                done: 0,
                total: 0,
                positions_total: 0,
                logs_total: 0,
                logs_started_ms: None,
                started_ms: now_ms(),
                finished_ms: None,
                error: None,
                outcome: None,
                waiting: None,
                applied_ms: None,
                applied_by: None,
                waited_ms: 0,
            };
            fits.jobs.insert(car, s.clone());
            s
        };
        let e = self.clone();
        let id = status.id;
        let spawned = std::thread::Builder::new().name("gps-fit".into()).spawn(move || {
            let r = {
                let _one = lock(&e.inner.gps_fit_one);
                // waited for another fit while the server stopped: none starts
                if e.inner.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                    Err(STOPPED.to_string())
                } else {
                    // (told between two days or logs, and before the layouts are tried: a rebuild under way is let
                    // go first; a stop of the server ends the wait, and the next day or log ends the fit)
                    let set = |st: Step| {
                        e.gps_fit_update(car, id, |s| {
                            if st.phase == Phase::Logs && s.phase != Some(Phase::Logs) {
                                s.logs_started_ms = Some(now_ms());
                            }
                            (s.phase, s.done, s.total) = (Some(st.phase), st.done, st.total);
                            if st.positions_total + st.logs_total > 0 {
                                (s.positions_total, s.logs_total) = (st.positions_total, st.logs_total);
                            }
                        });
                        e.gps_fit_yield(car, id);
                    };
                    let live = Live { e: &e, car };
                    let keys = rec.vehicle_keys.clone();
                    collect(&live, e.store(), e.judge(), &rt, &keys, days, now_ms(), &set).and_then(|c| {
                        set(Step { phase: Phase::Fitting, done: 0, total: 0, positions_total: 0, logs_total: 0 });
                        // stopped while it waited for a rebuild before the layouts: none is tried
                        if let Some(why) = live.stopped() {
                            return Err(why);
                        }
                        Ok(fit(&c, days, car))
                    })
                }
            };
            e.gps_fit_update(car, id, |s| {
                s.finished_ms = Some(now_ms());
                s.phase = None;
                s.waiting = None;
                match r {
                    Ok(o) => (s.state, s.outcome) = (FitState::Done, Some(o)),
                    Err(why) => (s.state, s.error) = (FitState::Failed, Some(why)),
                }
            });
            // every run is kept, to be looked at again after the server restarts (not one the server stopped under:
            // a server whose data moved away writes nothing more)
            if let Some(done) = e.gps_fit_status(car).filter(|s| s.id == id)
                && !e.is_stopping()
                && let Err(x) = e.gps_fit_store(&done)
            {
                tracing::warn!("GPS layout fit: the run could not be kept: {x}");
            }
        });
        if let Err(err) = spawned {
            self.gps_fit_update(car, id, |s| {
                (s.state, s.phase, s.finished_ms) = (FitState::Failed, None, Some(now_ms()));
                s.error = Some(format!("starting the fit: {err}"));
            });
            if let Some(done) = self.gps_fit_status(car).filter(|s| s.id == id) {
                let _ = self.gps_fit_store(&done);
            }
        }
        Ok(self.gps_fit_status(car).unwrap_or(status))
    }

    /// A fit and a rebuild don't run at once: both read and decode the car's stored logs, and the rebuild is what
    /// the history waits for. While a full rebuild or a rebuild job runs, the fit waits here (between two of its
    /// days or logs: its progress kept, `waiting` saying why) and goes on once none runs. So a fit asked during a
    /// rebuild is never lost, and a rebuild started during a fit waits for one log of the fit at most.
    fn gps_fit_yield(&self, car: CarId, id: u64) {
        let began = now_ms();
        let mut waited = false;
        while let Some(why) = self.rebuild_running() {
            if self.inner.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            waited = true;
            self.gps_fit_update(car, id, |s| s.waiting = Some(why));
            std::thread::sleep(*lock(&self.inner.gps_fit_wait));
        }
        let spent = if waited { now_ms().saturating_sub(began) } else { 0 };
        self.gps_fit_update(car, id, |s| {
            s.waiting = None;
            s.waited_ms += spent;
        });
    }

    /// What a fit waits for: a full rebuild or a rebuild job running (one put off, paused, or waiting to start
    /// takes nothing yet, so a fit doesn't wait for it). `None`: nothing.
    pub fn rebuild_running(&self) -> Option<String> {
        if self.full_worker_running() {
            return Some(
                "the full rebuild goes first (both read the same logs); this fit goes on from here once it ends".into(),
            );
        }
        if self.jobs().iter().any(|j| j.status == crate::JobStatus::Running && !j.waiting) {
            return Some(
                "a rebuild goes first (both read the same logs); this fit goes on from here once it ends".into(),
            );
        }
        None
    }

    /// Tests: how often a waiting fit looks again whether the rebuild ended.
    pub fn set_gps_fit_wait(&self, every: Duration) {
        *lock(&self.inner.gps_fit_wait) = every;
    }

    fn gps_fit_update(&self, car: CarId, id: u64, f: impl FnOnce(&mut FitStatus)) {
        if let Some(s) = lock(&self.inner.gps_fits).jobs.get_mut(&car)
            && s.id == id
        {
            f(s);
        }
    }

    /// The car's newest fit (under way, or the newest kept: runs are there after a restart).
    pub fn gps_fit_status(&self, car: CarId) -> Option<FitStatus> {
        self.gps_fit_newest(car)
    }

    /// What else the server is busy with that a person waiting may want to know of: a full rebuild and how far it
    /// is (`rebuild 34 %`).
    pub fn busy_words(&self) -> Option<String> {
        let r = self.full_rebuild_status().ok().flatten()?;
        // one waiting (its grace, tonight) or paused takes nothing yet
        if !r.running() || r.held(now_ms()) {
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

    /// Pairs of frames encoded with `lat` and `lon` and the local TeslaMate's positions, one a second.
    fn pairs_for(lat: &GpsField, lon: &GpsField, n: usize) -> Vec<Pair> {
        drive(n)
            .into_iter()
            .enumerate()
            .map(|(i, (la, lo))| {
                Pair::new(&frame(lat, lon, la, lo), la, lo).at(i as u64 * 1000, Stream::TeslamateLocal)
            })
            .collect()
    }

    fn outcome(pairs: Vec<Pair>, sources: Vec<(Stream, usize)>, logs: usize, car: CarId) -> Outcome {
        fit(&Collected { pairs, sources, logs, ..Collected::default() }, 30, car)
    }

    /// The logs' frames read on several threads are handed on in the logs' order, the same as one by one, and a
    /// stop asked by the one taking them ends the reading there.
    #[test]
    fn frames_read_on_several_threads_come_in_the_order_of_the_logs() {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
        let store = ingest::Store::new(data.join("can"), Arc::new(v));
        let sha = |b: &[u8]| {
            use sha2::Digest;
            hex::encode(sha2::Sha256::digest(b))
        };
        let logs: Vec<synth::LogFile> =
            synth::Scenario::new(7).days(1).rates(synth::Rates::Light).logs().take(41).collect();
        let mut names = Vec::new();
        rt.block_on(async {
            for l in &logs[..40] {
                let side = l.sidecar_bytes();
                for (n, b) in [(l.name.clone(), l.bytes.clone()), (l.sidecar_name(), side)] {
                    store.receive(&n, b.clone().into(), 0, b.len() as u64, &sha(&b)).await.unwrap();
                }
                names.push(l.name.clone());
            }
        });
        // (a name not stored: told as such, in its place)
        names.insert(7, logs[40].name.clone());
        let h = rt.handle().clone();
        let read = |threads: usize, stop_at: Option<usize>| {
            let mut got = Vec::new();
            let r = read_frames(&store, &h, &names, threads, |i, f| {
                got.push((i, f.unwrap()));
                if stop_at == Some(i) { Err("stopped".into()) } else { Ok(()) }
            });
            (r, got)
        };
        let (r, one) = read(1, None);
        assert!(r.is_ok());
        assert_eq!(one.len(), names.len());
        assert!(one[7].1.is_none(), "not stored");
        assert!(one.iter().filter_map(|(_, f)| f.as_ref()).map(Vec::len).sum::<usize>() > 100, "frames read");
        let (r, many) = read(4, None);
        assert!(r.is_ok());
        assert_eq!(many, one, "the same, in the same order");
        let (r, cut) = read(4, Some(12));
        assert_eq!(r, Err("stopped".to_string()));
        assert_eq!(cut, one[..13].to_vec(), "up to the stop");
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
        let p = pair(&frames, &[(Stream::TeslamateLocal, refs.to_vec())]);
        // the second: the car stood between two positions 2 m apart; the third: the positions around it are far
        // apart; the last: none after it
        let p = p.pairs;
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].lat, 1.00001);
        assert_eq!(p[1].lat, 1.00001);
        assert_eq!((p[0].t_ms, p[0].source), (10_900, Some(Stream::TeslamateLocal)));
        // where the car stood is said by one input's positions, not one of each
        let other = [(1_400_000, 1.00002, 2.0)];
        let p = pair(&[(1_500_000, b)], &[(Stream::TeslamateLocal, refs.to_vec()), (Stream::TeslaApi, other.to_vec())]);
        assert!(p.pairs.is_empty(), "{:?}", p.pairs);
    }

    /// A made-up line of positions, one a second from `t0`: the `i`th at index `i + shift` of [`drive`].
    fn line_of(t0: u64, n: usize, shift: usize) -> Vec<Position> {
        let d = drive(n + shift);
        (0..n).map(|i| (t0 + i as u64 * 1000, d[i + shift].0, d[i + shift].1)).collect()
    }

    #[test]
    fn live_inputs_come_first_and_an_import_whose_times_are_off_is_left_out() {
        let t0 = 1_000_000_000;
        let live = line_of(t0, 100, 0);
        // a TeslaMate import of the same moments whose times are off: each of its positions is the one of 50 s later
        let off = line_of(t0, 300, 50);
        // a TeslaFi import that agrees with the live input, and goes on for 100 s after it
        let good = line_of(t0, 200, 0);
        let frames: Vec<(u64, [u8; 8])> = (0..200).map(|i| (t0 + i * 1000 + 300, [0u8; 8])).collect();
        let refs = vec![(Stream::TeslamateLocal, live), (Stream::Teslamate, off), (Stream::Teslafi, good)];
        let p = pair(&frames, &refs);
        assert_eq!(p.left_out.len(), 1, "{:?}", p.left_out);
        assert_eq!(p.left_out[0].0, Stream::Teslamate);
        assert!(p.left_out[0].1 > IMPORT_AGREE_M, "{:?}", p.left_out);
        assert_eq!(p.paired, 200);
        let of = |s: Stream| p.pairs.iter().filter(|x| x.source == Some(s)).count();
        assert_eq!(of(Stream::Teslamate), 0);
        // the live input for the first 100 s (and the frame just after its last position), the import after
        assert!(of(Stream::TeslamateLocal) >= 100 && of(Stream::Teslafi) >= 98, "{}", of(Stream::Teslafi));
        assert!(p.pairs.iter().take(100).all(|x| x.source == Some(Stream::TeslamateLocal)));
        // the report says why it was left out, in metres only
        let c = Collected {
            pairs: p.pairs,
            paired: p.paired,
            sources: refs.iter().map(|(s, r)| (*s, r.len())).collect(),
            left_out: p.left_out,
            logs: 1,
            ..Collected::default()
        };
        let o = fit(&c, 30, 1);
        let tm = o.sources.iter().find(|s| s.input == "teslamate").unwrap();
        assert!(tm.left_out.as_deref().unwrap().contains("its times are off"), "{tm:?}");
        assert!(o.text().contains("teslamate") && o.text().contains("left out: its positions are"), "{}", o.text());
        let fi = o.sources.iter().find(|s| s.input == "teslafi").unwrap();
        assert!(fi.pairs >= 98 && fi.left_out.is_none(), "{fi:?}");
    }

    /// The pairs of `pairs_for`, the `every`th of them (from the first) paired with the position of an import an hour
    /// off (the drive 3,600 positions later), the rest with the local TeslaMate's.
    fn polluted(lat: &GpsField, lon: &GpsField, n: usize, every: usize) -> Vec<Pair> {
        let d = drive(n + 3600);
        (0..n)
            .map(|i| {
                let (la, lo) = d[i];
                let b = frame(lat, lon, la, lo);
                if i % every == 0 {
                    let (wa, wo) = d[i + 3600];
                    Pair::new(&b, wa, wo).at(i as u64 * 1000, Stream::Teslamate)
                } else {
                    Pair::new(&b, la, lo).at(i as u64 * 1000, Stream::TeslamateLocal)
                }
            })
            .collect()
    }

    /// No made-up coordinate (around -30, -20) in a report or its JSON.
    fn no_position(o: &Outcome) {
        let json = serde_json::to_string(o).unwrap();
        for bad in ["-30.", "-20.", "29.9", "19.9", "-29.", "-19."] {
            assert!(!o.text().contains(bad), "{bad} in {}", o.text());
            assert!(!json.contains(bad), "{bad} in {json}");
        }
    }

    #[test]
    fn a_longitude_with_any_offset_is_found() {
        let lat = dbc_field("GPSLatitude04F").unwrap();
        // made up: an offset that is no power of two of the scale
        let off = GpsField { offset: 16.9234567, ..dbc_field("GPSLongitude04F").unwrap() };
        let p = pairs_for(&lat, &off, 400);
        let best = best_two(&search(&p, Axis::Lon)).0.unwrap();
        assert_eq!(GpsField { offset: 0.0, ..best.field }, GpsField { offset: 0.0, ..off }, "{best:?}");
        assert_eq!(Kind::of(&best.field), Kind::Offset, "{best:?}");
        assert!((best.field.offset - 16.9234567).abs() < 2e-7 && best.p95_m < 1.0, "{best:?}");
        let o = outcome(p, vec![(Stream::TeslamateLocal, 400)], 3, 1);
        assert!(o.good && !o.is_dbc, "{}", o.text());
        let found = o.found().unwrap();
        assert_eq!(found.lon, best.field);
        // as the car's setting takes it, and read back the same
        assert_eq!(found.to_string().parse::<GpsLayout>().unwrap(), found);
        // the line of its bits: the scale, the offset, constant
        let l = o.lon.line.as_ref().unwrap();
        assert_eq!(l.bits, "28|28@1-");
        assert!((l.slope.unwrap() / 1e-6 - 1.0).abs() < 1e-5 && l.p95_m == Some(0.0), "{l:?}");
        assert!(l.drift.as_ref().unwrap().constant, "{l:?}");
        assert!(o.text().contains("  line        28|28@1- ("), "{}", o.text());
        assert!(o.text().contains(": constant"), "{}", o.text());
        no_position(&o);
    }

    #[test]
    fn a_longitude_whose_scale_is_a_little_off_is_found_by_its_line() {
        let lat = dbc_field("GPSLatitude04F").unwrap();
        // made up: half a percent more degrees per step than the DBC's, and an offset of no power of two
        let scaled = GpsField { scale: 1.005e-6, offset: 3.1234567, ..dbc_field("GPSLongitude04F").unwrap() };
        let p = pairs_for(&lat, &scaled, 400);
        let best = best_two(&search(&p, Axis::Lon)).0.unwrap();
        assert_eq!(Kind::of(&best.field), Kind::Line, "{best:?}");
        assert_eq!((best.field.start, best.field.width, best.field.signed), (28, 28, true), "{best:?}");
        assert!((best.field.scale / 1.005e-6 - 1.0).abs() < 1e-5 && best.p95_m < 1.0, "{best:?}");
        let o = outcome(p, vec![(Stream::TeslamateLocal, 400)], 3, 1);
        assert!(o.good, "{}", o.text());
        assert_eq!(o.found().unwrap().lon, best.field);
        let l = o.lon.line.as_ref().unwrap();
        assert_eq!(l.nominal, Some(1e-6));
        assert!((l.scale_error.unwrap() - 0.005).abs() < 1e-5, "{l:?}");
        // at the DBC's scale the offset grows with the value: not constant
        let d = l.drift.as_ref().unwrap();
        assert!(!d.constant && d.per_km_value_m.unwrap().abs() > 1.0, "{d:?}");
        assert!(o.text().contains("+0.50") || o.text().contains("+0.49"), "{}", o.text());
        assert!(o.text().contains("% from 1e-6"), "{}", o.text());
        assert!(o.text().contains("it drifts"), "{}", o.text());
        // the latitude's is the DBC's, its offset constant
        let la = o.lat.line.as_ref().unwrap();
        assert!(la.drift.as_ref().unwrap().constant && la.scale_error.unwrap().abs() < 1e-5, "{la:?}");
        no_position(&o);
    }

    #[test]
    fn pairs_of_wrong_moments_are_left_out_before_scoring() {
        let lat = dbc_field("GPSLatitude04F").unwrap();
        let off = GpsField { offset: 16.9234567, ..dbc_field("GPSLongitude04F").unwrap() };
        // one pair in seven of an import an hour off
        let p = polluted(&lat, &off, 700, 7);
        // scored on them all, nothing comes within 5 km at the 95th percentile
        assert!(search(&p, Axis::Lat).is_empty() && search(&p, Axis::Lon).is_empty());
        let sources = vec![(Stream::TeslamateLocal, 600), (Stream::Teslamate, 100)];
        let o = outcome(p, sources, 3, 1);
        assert_eq!(o.trimmed, 100, "{}", o.text());
        assert!(o.good, "{}", o.text());
        let found = o.found().unwrap();
        assert_eq!(found.lat, lat);
        assert!((found.lon.offset - 16.9234567).abs() < 2e-7, "{found}");
        assert!(o.p95_m.unwrap() < 2.0, "{}", o.text());
        // each input's pairs and their own errors: the import's are far off, and all left out
        let tm = o.sources.iter().find(|s| s.input == "teslamate").unwrap();
        assert_eq!((tm.pairs, tm.trimmed, tm.lat_median_m), (100, 100, None), "{tm:?}");
        let local = o.sources.iter().find(|s| s.input == "teslamate-local").unwrap();
        assert_eq!((local.pairs, local.trimmed), (600, 0), "{local:?}");
        assert!(local.lat_p95_m.unwrap() < 1.0 && local.lon_p95_m.unwrap() < 1.0, "{local:?}");
        let text = o.text();
        assert!(text.contains("100 of them left out as outliers"), "{text}");
        assert!(text.contains("teslamate-local") && text.contains("(100 outliers)"), "{text}");
        no_position(&o);
    }

    fn status(phase: Phase, done: u64, positions_total: u64, logs_total: u64) -> FitStatus {
        FitStatus {
            id: 1,
            car: 1,
            days: 30,
            state: FitState::Running,
            phase: Some(phase),
            done,
            total: if phase == Phase::Positions { positions_total } else { logs_total },
            positions_total,
            logs_total,
            logs_started_ms: Some(1_000_000),
            started_ms: 0,
            finished_ms: None,
            error: None,
            outcome: None,
            waiting: None,
            waited_ms: 0,
            applied_ms: None,
            applied_by: None,
        }
    }

    /// The bar is weighed by the work: 150 days of positions and 22 402 logs, so 272 logs read is not 30 %.
    #[test]
    fn the_percentage_is_weighed_by_the_work_of_each_step() {
        let (p, l) = (150, 22_402);
        assert_eq!(status(Phase::Queued, 0, 0, 0).percent(), 0);
        assert_eq!(status(Phase::Positions, 0, p, l).percent(), 0);
        // every day of positions read: under 1 % of the work
        assert_eq!(status(Phase::Positions, 150, p, l).percent(), 0);
        assert_eq!(status(Phase::Logs, 0, p, l).percent(), 0);
        // the owner's report: 272 of 22 402 logs
        assert_eq!(status(Phase::Logs, 272, p, l).percent(), 1);
        assert_eq!(status(Phase::Logs, 11_201, p, l).percent(), 49);
        assert_eq!(status(Phase::Logs, 22_402, p, l).percent(), 98);
        let fitting = status(Phase::Fitting, 0, p, l);
        assert!(fitting.percent() >= 98 && fitting.percent() < 100, "{}", fitting.percent());
        // a car with almost no logs: the days of positions are most of it
        assert_eq!(status(Phase::Positions, 75, 150, 10).percent(), 46);
        let mut s = status(Phase::Logs, 1, p, l);
        s.state = FitState::Done;
        assert_eq!(s.percent(), 100);
        // a count past its total never passes it
        assert!(status(Phase::Logs, 99_999, p, l).percent() < 100);
    }

    /// While it waits for a rebuild there is no time left, and the time waited is not part of the speed.
    #[test]
    fn the_time_left_ignores_the_time_waited_for_a_rebuild() {
        let at = 1_000_000;
        let mut s = status(Phase::Logs, 2_200, 150, 22_402);
        s.waiting = Some("a full rebuild".into());
        assert_eq!(s.eta_ms(at + 22_000), None, "waiting");
        s.waiting = None;
        // 22 s since the logs began, 12 of them waited: 2 200 logs in 10 s
        s.waited_ms = 12_000;
        assert_eq!(s.eta_ms(at + 22_000), Some(91_827));
        // all of it waited: no speed yet
        s.waited_ms = 22_000;
        assert_eq!(s.eta_ms(at + 22_000), None);
    }

    /// The time left is from the speed the logs were read at, `None` until there is one.
    #[test]
    fn the_time_left_follows_the_speed_of_the_logs() {
        let (p, l) = (150, 22_402);
        let at = 1_000_000;
        assert_eq!(status(Phase::Logs, 0, p, l).eta_ms(at + 5_000), None, "no log done");
        assert_eq!(status(Phase::Logs, 100, p, l).eta_ms(at), None, "no time passed");
        // 2 200 logs in 22 s: 100 a second, 20 202 left: 202 s
        assert_eq!(status(Phase::Logs, 2_200, p, l).eta_ms(at + 22_000), Some(202_020));
        // the last log done: nothing left
        assert_eq!(status(Phase::Logs, 22_402, p, l).eta_ms(at + 10_000), Some(0));
        assert_eq!(status(Phase::Positions, 10, p, l).eta_ms(at + 10_000), None);
        assert_eq!(status(Phase::Fitting, 0, p, l).eta_ms(at + 10_000), None);
        let mut s = status(Phase::Logs, 5, p, l);
        s.logs_started_ms = None;
        assert_eq!(s.eta_ms(at + 10_000), None);
        s.logs_started_ms = Some(at);
        s.state = FitState::Done;
        assert_eq!(s.eta_ms(at + 10_000), None);
    }
}
