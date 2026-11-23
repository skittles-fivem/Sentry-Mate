//! `sentry-mate gps-layout --car c1`: works out where the Screenmate's own GPS frame (0x04F on its internal bus,
//! `can3`) keeps its latitude and longitude, from the stored logs of a car and the positions its other inputs
//! reported at the same moments (the Tesla API, TeslaFi, local TeslaMate, and their imports).
//!
//! Each frame is paired with the nearest such position within [`PAIR_MS`] (or, while the car stood, with the
//! positions around it when they agree). Then every layout of a field is tried: start bit, width
//! ([`MIN_WIDTH`]..=[`MAX_WIDTH`]), Intel or Motorola, signed or unsigned, a scale of [`SCALES`], and an offset that
//! is 0 or a power of two of the scale (what a bit off would add), and scored by how far its positions are from the
//! paired ones (median and 95th percentile, in metres).
//!
//! It prints only layouts, counts and errors in metres: never a position, a time or a place, so what it prints can
//! be passed on. It runs beside the server: the data folder is only read (the db through [`db::Reader`], a day at a
//! time while the folder's write gate is paused, the logs as `log decode` reads them).

use model::gps::{GpsField, GpsLayout};
use model::{CarId, Sample, Stream};
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
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
/// Field widths tried.
pub const MIN_WIDTH: u8 = 24;
pub const MAX_WIDTH: u8 = 32;
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

/// A distance for people: `4 m`, `1.4 km`; over 10 km only that (a far-off longitude's error in kilometres would
/// tell the latitude it was measured at).
fn metres(m: f64) -> String {
    if !m.is_finite() {
        "-".into()
    } else if m > 10_000.0 {
        "over 10 km".into()
    } else if m < 1_000.0 {
        format!("{m:.0} m")
    } else {
        format!("{:.1} km", m / 1_000.0)
    }
}

/// The report of a fit, as printed (no position, time or place).
pub fn report(
    pairs: &[Pair],
    sources: &[(Stream, usize)],
    logs: usize,
    days: u32,
    car: CarId,
) -> (String, Option<GpsLayout>) {
    let mut s = String::new();
    let from: Vec<String> =
        sources.iter().filter(|(_, n)| *n > 0).map(|(st, n)| format!("{} {n}", st.dir_name())).collect();
    s += &format!(
        "gps-layout c{car}: the last {days} days, {logs} logs with the Screenmate's GPS frames (0x04F on its internal bus), {} \
         frames paired with the car's positions from its other inputs ({})\n",
        pairs.len(),
        if from.is_empty() { "none".into() } else { from.join(", ") }
    );
    if pairs.len() < 20 {
        s += "Too few pairs to tell layouts apart: let the car drive with the Screenmate and its other inputs on for a day, \
              then run this again (or give --days more days).\n";
        return (s, None);
    }
    // how much the positions vary: a car that stood still the whole time can't tell layouts apart
    let spread = |axis: Axis| {
        let mut v: Vec<f64> = pairs.iter().map(|p| axis.of(p)).collect();
        let mid = quantile(&mut v, 0.5);
        let mut d: Vec<f64> = pairs.iter().map(|p| (axis.of(p) - mid).abs() * axis.metres(p)).collect();
        quantile(&mut d, 0.95)
    };
    let (lat_spread, lon_spread) = (spread(Axis::Lat), spread(Axis::Lon));
    s += &format!(
        "The paired positions spread {} north-south and {} east-west (95th percentile from their middle){}\n",
        metres(lat_spread),
        metres(lon_spread),
        if lat_spread.max(lon_spread) < 500.0 { ": little movement, so the fit is weak" } else { "" }
    );
    let mut chosen = [None, None];
    for (i, (axis, name, series)) in
        [(Axis::Lat, "latitude", "GPSLatitude04F"), (Axis::Lon, "longitude", "GPSLongitude04F")].into_iter().enumerate()
    {
        s += &format!("\n{name}\n");
        let line = |label: &str, f: &GpsField, (med, p95): (f64, f64)| {
            format!(
                "  {label:<11} {:<26} median {:>10}   95th percentile {:>10}\n",
                f.to_string(),
                metres(med),
                metres(p95)
            )
        };
        if let Some(f) = dbc_field(series) {
            s += &line("now (DBC)", &f, score(&f, pairs, axis));
        }
        let fits = search(pairs, axis);
        let (best, other) = best_two(&fits);
        match best {
            Some(b) => s += &line("best", &b.field, (b.median_m, b.p95_m)),
            None => s += "  best        no layout comes within 5 km of the paired positions\n",
        }
        match other {
            Some(o) => s += &line("runner-up", &o.field, (o.median_m, o.p95_m)),
            None if best.is_some() => s += "  runner-up   none: no other bits come within 5 km\n",
            None => {}
        }
        chosen[i] = best.map(|b| b.field);
    }
    let layout = match chosen {
        [Some(lat), Some(lon)] => Some(GpsLayout { lat, lon, accuracy: None }),
        _ => None,
    };
    if let Some(l) = layout {
        let mut d: Vec<f64> = pairs
            .iter()
            .map(|p| {
                let dy = (l.lat.raw_of(p.le, p.be) as f64 * l.lat.scale + l.lat.offset - p.lat) * M_PER_DEG;
                let dx = (l.lon.raw_of(p.le, p.be) as f64 * l.lon.scale + l.lon.offset - p.lon) * Axis::Lon.metres(p);
                dy.hypot(dx)
            })
            .collect();
        let p95 = quantile(&mut d, 0.95);
        s += &format!(
            "\nWith the best of each, a frame's position is {} from the paired one (median), {} at the 95th percentile.\n",
            metres(quantile(&mut d, 0.5)),
            metres(p95)
        );
        let now = [dbc_field("GPSLatitude04F"), dbc_field("GPSLongitude04F")];
        if now == [Some(l.lat), Some(l.lon)] {
            s += "That is the layout the DBC has: the Screenmate's frames are read right.\n";
        } else {
            s += &format!(
                "\nlayout: {l}\n\nTo read this car's Screenmate GPS frames with it (an admin; the car's history is rebuilt): \
                 PUT /api/v1/cars/c{car}/gps-layout with {{\"layout\": \"{l}\"}}. Check the numbers above first: a \
                 95th percentile of tens of metres is a layout, one of kilometres is not.\n"
            );
        }
    }
    (s, layout)
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

/// Runs the command over the data folder (only read).
pub async fn run(
    data: &Path,
    keys: &Path,
    car: CarId,
    days: u32,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let vault = Arc::new(vault::Vault::open(data, keys)?);
    let gate = gate::Gate::open(data)?;
    let reader = db::Reader::new(data, vault.clone());
    let wait = Duration::from_secs(30);
    // each read of the db while the folder's writes pause (a few milliseconds each)
    let paused = |f: &mut dyn FnMut() -> db::Result<()>| -> Result<(), Box<dyn std::error::Error>> {
        let _pause = gate.pause(wait).map_err(|e| format!("the data folder stayed busy: {e:?}"))?;
        f()?;
        Ok(())
    };
    let mut record = None;
    paused(&mut || {
        record = reader.car(car)?;
        Ok(())
    })?;
    let Some(c) = record else {
        writeln!(err, "no car c{car} (the car's id is in its page's address, /cars/c1/...)")?;
        return Ok(ExitCode::FAILURE);
    };
    let to = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let from = to.saturating_sub(u64::from(days) * db::DAY_MS);
    // the positions of the car's other inputs, a day at a time
    let mut refs: Vec<(u64, f64, f64)> = Vec::new();
    let mut sources = Vec::new();
    for stream in REFERENCES {
        let mut n = 0usize;
        let list = reader.days::<Sample>(car, stream, from, to)?;
        for day in list {
            let mut pts: Vec<Sample> = Vec::new();
            paused(&mut || {
                pts = reader.points(car, stream, day.max(from), (day + db::DAY_MS).min(to))?;
                Ok(())
            })?;
            for s in pts {
                if let (Some(la), Some(lo)) = (s.lat, s.lon)
                    && timeline::real_fix(la, lo)
                {
                    refs.push((s.t_ms, la, lo));
                    n += 1;
                }
            }
        }
        sources.push((stream, n));
    }
    refs.sort_by_key(|r| r.0);
    // the Screenmate's GPS frames of the car's logs in the period
    let store = ingest::Store::new(data.join("can"), vault);
    let judge = ingest::Judge::new(store.clone());
    let mut frames = Vec::new();
    let mut logs = 0usize;
    for key in &c.vehicle_keys {
        // a log a minute at most, newest first
        let most = usize::try_from(days).unwrap_or(usize::MAX).saturating_mul(1_500).saturating_add(100);
        let names = judge.latest_usable(key, most).await?.into_iter().filter(|n| {
            trust::LogName::parse(n)
                .and_then(|l| u64::try_from(l.started_ms).ok())
                .is_some_and(|t| (from..to).contains(&t))
        });
        for name in names {
            let Some(log) = store.read(&name).await? else { continue };
            let side: serde_json::Value = store
                .read(&format!("{name}.json"))
                .await?
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            let f = internal_frames(&log, &side);
            if !f.is_empty() {
                logs += 1;
                frames.extend(f);
            }
        }
    }
    frames.sort_by_key(|f| f.0);
    let pairs = pair(&frames, &refs);
    let (text, _) = report(&pairs, &sources, logs, days, car);
    out.write_all(text.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    /// A made-up drive (straight lines around a made-up point in the open sea), one position a second.
    fn drive(n: usize) -> Vec<(f64, f64)> {
        (0..n)
            .map(|i| {
                let k = i as f64;
                (-30.0 + 0.0004 * k + 0.002 * (k / 40.0).sin(), -20.0 + 0.0003 * k + 0.002 * (k / 55.0).cos())
            })
            .collect()
    }

    /// A frame with the latitude and longitude encoded by `lat` and `lon`.
    fn frame(lat: &GpsField, lon: &GpsField, la: f64, lo: f64) -> [u8; 8] {
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

    #[test]
    fn the_dbc_layout_is_found_when_the_frames_follow_it() {
        let (lat, lon) = (dbc_field("GPSLatitude04F").unwrap(), dbc_field("GPSLongitude04F").unwrap());
        let p = pairs_for(&lat, &lon, 400);
        let (best, _) = best_two(&search(&p, Axis::Lon));
        let best = best.unwrap();
        assert_eq!(best.field, lon, "{best:?}");
        assert!(best.p95_m < 1.0);
        let (text, layout) = report(&p, &[(Stream::TeslamateLocal, 400)], 3, 30, 1);
        assert!(text.contains("That is the layout the DBC has"), "{text}");
        assert_eq!(layout.unwrap().lat, lat);
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
        let (text, _) = report(&p, &[(Stream::TeslamateLocal, 400)], 3, 30, 1);
        assert!(text.contains("over 10 km") && !text.contains("1616") && !text.contains("1618"), "{text}");
        // another start bit, width and byte order, 1e-7 steps
        let moved = GpsField { start: 39, width: 32, motorola: true, signed: true, scale: 1e-7, offset: 0.0 };
        let lat2 = GpsField { start: 0, width: 28, motorola: false, signed: true, scale: 1e-6, offset: 0.0 };
        let p = pairs_for(&lat2, &moved, 400);
        let (best, runner) = best_two(&search(&p, Axis::Lon));
        assert_eq!(best.unwrap().field, moved, "{best:?} {runner:?}");
        let (text, layout) = report(&p, &[(Stream::TeslafiApi, 400)], 2, 30, 7);
        assert!(text.contains("PUT /api/v1/cars/c7/gps-layout"), "{text}");
        assert_eq!(layout.unwrap().lon, moved);
    }

    #[test]
    fn the_report_never_shows_a_position() {
        let (lat, lon) = (dbc_field("GPSLatitude04F").unwrap(), dbc_field("GPSLongitude04F").unwrap());
        let p = pairs_for(&lat, &lon, 300);
        let (text, _) = report(&p, &[(Stream::TeslamateLocal, 300)], 1, 30, 1);
        for bad in ["-30.", "-20.", "29.9", "19.9"] {
            assert!(!text.contains(bad), "{bad} in {text}");
        }
        let (text, layout) = report(&p[..5], &[], 1, 30, 1);
        assert!(text.contains("Too few pairs") && layout.is_none());
    }

    /// `YYYYMMDDTHHMMSSZ` of ms since 1970.
    fn stamp(ms: u64) -> String {
        let (days, secs) = ((ms / 86_400_000) as i64, ms / 1000 % 86_400);
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
    }

    /// The whole command over a data folder: a made-up car whose Screenmate sends its longitude 2^24 steps low,
    /// and its local TeslaMate positions, with the db open (as the running server keeps it) while the command reads.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_command_reads_a_data_folder_beside_the_server_and_finds_the_layout() {
        let k = crate::testkit::kit();
        let vault = Arc::new(vault::Vault::open(&k.data, &k.keys).unwrap());
        let db = db::Db::open(&k.data, vault, db::Options::default()).unwrap();
        let mut car = model::Car {
            id: 0,
            name: "Made-up".into(),
            vehicle_keys: vec![crate::testkit::CAR.into()],
            preferred_key: None,
            vin: None,
            profile_setting: None,
            time_zone: "UTC".into(),
            time_zone_typed: None,
            efficiency_wh_km: None,
            created_ms: 0,
            inputs: model::Inputs::NONE,
        };
        car = db.create_car(car).unwrap();
        let _gate = gate::Gate::open(&k.data).unwrap();
        let (lat, dbc_lon) = (dbc_field("GPSLatitude04F").unwrap(), dbc_field("GPSLongitude04F").unwrap());
        let low = GpsField { offset: 16.777216, ..dbc_lon };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let t0 = (now - 2 * db::DAY_MS) / 60_000 * 60_000;
        let path = drive(600);
        let refs: Vec<Sample> = path
            .iter()
            .enumerate()
            .map(|(i, &(la, lo))| Sample {
                t_ms: t0 + i as u64 * 1000,
                lat: Some(la),
                lon: Some(lo),
                ..Sample::default()
            })
            .collect();
        db.write_points(car.id, Stream::TeslamateLocal, t0, t0 + 600_000, &refs).unwrap();
        // ten minutes of logs: the Screenmate's frame on can3 every 200 ms, half a second after each position
        for minute in 0..10u64 {
            let start = t0 + minute * 60_000;
            let mut text = String::from("# canbridge v1 app=test\n");
            for j in 0..300u64 {
                let t = start + j * 200 + 500;
                let i = ((t - t0) / 1000) as usize;
                let (la, lo) = path[i.min(path.len() - 1)];
                let hex: String = frame(&lat, &low, la, lo).iter().map(|b| format!("{b:02X}")).collect();
                text += &format!("({}.{:06}) can3 04F#{hex}\n", t / 1000, (t % 1000) * 1000);
            }
            let mut xz = Vec::new();
            lzma_rs::xz_compress(&mut text.as_bytes(), &mut xz).unwrap();
            let name = format!("{}_{}_{}.log.xz", crate::testkit::CAR, stamp(start), minute + 1);
            let side = serde_json::json!({ "format": "canbridge-candump-xz/1", "bus": { "can0": "vehicle",
                "can1": "chassis", "can3": "screenmate-internal" } });
            crate::testkit::upload(&k, &name, &xz).await;
            crate::testkit::upload(&k, &format!("{name}.json"), &serde_json::to_vec(&side).unwrap()).await;
        }
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&k.data, &k.keys, car.id, 30, &mut out, &mut err).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(code, ExitCode::SUCCESS, "{text} {}", String::from_utf8_lossy(&err));
        assert!(text.contains("10 logs") && text.contains("teslamate-local 600"), "{text}");
        assert!(text.contains("layout: lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)"), "{text}");
        // the db the "server" holds still works, and an unknown car is said so
        assert_eq!(db.read_points::<Sample>(car.id, Stream::TeslamateLocal, 0, u64::MAX).unwrap().len(), 600);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run(&k.data, &k.keys, car.id + 9, 30, &mut out, &mut err).await.unwrap(), ExitCode::FAILURE);
        assert!(String::from_utf8_lossy(&err).contains("no car"));
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
}
