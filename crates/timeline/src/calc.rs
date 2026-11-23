//! Small sums over a run of samples: energy from power, distance from GPS or speed, climbs, averages.

use model::{Position, Sample};

/// kW held for a number of milliseconds, in kWh.
fn kwh(kw: f64, ms: u64) -> f64 {
    kw * ms as f64 / 3_600_000.0
}

/// The integral of a power over samples (kWh): each sample's power counts until the next sample that has the power
/// (a second without it says nothing about it), or for that sample's hold time at most across a gap (`max_hold_ms`
/// of the sample's index: a reading from an API stands for longer than one of the car's own seconds). A longer gap
/// is bridged with `bridge(before, after)` when it can say what happened (the pack's lifetime counters), else only
/// the hold time of it counts. The last one counts until `end_ms`. `None` when no sample has the power.
pub(crate) fn integrate_kwh(
    s: &[&Sample],
    end_ms: u64,
    max_hold_ms: impl Fn(usize) -> u64,
    power: impl Fn(&Sample) -> Option<f64>,
    bridge: impl Fn(&Sample, &Sample) -> Option<f64>,
) -> Option<f64> {
    integrate(s, end_ms, max_hold_ms, power, |_| false, bridge, |_, _, _| {})
}

/// One sample's share of an integral ([`integrate_pieces`]): its power over `[from, to)` (kW; over a bridged gap, the
/// bridge's energy spread over the gap).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Piece {
    pub(crate) from: u64,
    pub(crate) to: u64,
    pub(crate) kw: f64,
}

impl Piece {
    pub(crate) fn kwh(&self) -> f64 {
        kwh(self.kw, self.to.saturating_sub(self.from))
    }
}

/// [`integrate_kwh`], and the pieces its sum is made of, by time. A sample that is `off` (it says nothing flows: the
/// car is not charging) ends the hold of the reading before it.
pub(crate) fn integrate_pieces(
    s: &[&Sample],
    end_ms: u64,
    max_hold_ms: impl Fn(usize) -> u64,
    power: impl Fn(&Sample) -> Option<f64>,
    off: impl Fn(&Sample) -> bool,
    bridge: impl Fn(&Sample, &Sample) -> Option<f64>,
) -> Option<(f64, Vec<Piece>)> {
    let mut out = Vec::new();
    let sum = integrate(s, end_ms, max_hold_ms, power, off, bridge, |from, to, e| {
        if to > from {
            out.push(Piece { from, to, kw: e * 3_600_000.0 / (to - from) as f64 });
        }
    })?;
    Some((sum, out))
}

/// Adds `[a, b)` to sorted spans apart from each other (`a` no earlier than the last one's start).
pub(crate) fn add_span(spans: &mut Vec<(u64, u64)>, a: u64, b: u64) {
    if b <= a {
        return;
    }
    match spans.last_mut() {
        Some(last) if a <= last.1 => last.1 = last.1.max(b),
        _ => spans.push((a, b)),
    }
}

/// How much of `[a, b)` sorted spans apart from each other cover (ms).
pub(crate) fn covered_ms(spans: &[(u64, u64)], a: u64, b: u64) -> u64 {
    let i = spans.partition_point(|s| s.1 <= a);
    spans[i..].iter().take_while(|s| s.0 < b).map(|s| s.1.min(b).saturating_sub(s.0.max(a))).sum()
}

fn integrate(
    s: &[&Sample],
    end_ms: u64,
    max_hold_ms: impl Fn(usize) -> u64,
    power: impl Fn(&Sample) -> Option<f64>,
    off: impl Fn(&Sample) -> bool,
    bridge: impl Fn(&Sample, &Sample) -> Option<f64>,
    mut piece: impl FnMut(u64, u64, f64),
) -> Option<f64> {
    let mut sum = 0.0;
    // the sample with the power that is waiting for the next one
    let mut held: Option<(usize, f64)> = None;
    let mut any = false;
    // (`bridged`: the next sample has the power too, so the lifetime counters may say what a long gap held)
    let mut close = |i: usize, p: f64, next: Option<&Sample>, bridged: bool| {
        let x = s[i];
        let hold = max_hold_ms(i);
        let to = next.map_or(end_ms, |n| n.t_ms);
        let dt = to.saturating_sub(x.t_ms);
        let (e, until) = if dt <= hold {
            (kwh(p, dt), to)
        } else if let Some(e) = next.filter(|_| bridged).and_then(|n| bridge(x, n)) {
            (e, to)
        } else {
            (kwh(p, hold), x.t_ms.saturating_add(hold))
        };
        sum += e;
        piece(x.t_ms, until, e);
    };
    for (i, x) in s.iter().enumerate() {
        match power(x) {
            Some(p) => {
                any = true;
                if let Some((j, q)) = held.replace((i, p)) {
                    close(j, q, Some(x), true);
                }
            }
            None if off(x) => {
                if let Some((j, q)) = held.take() {
                    close(j, q, Some(x), false);
                }
            }
            None => {}
        }
    }
    if let Some((j, q)) = held {
        close(j, q, None, false);
    }
    any.then_some(sum)
}

/// Energy out of the pack between two samples by its lifetime counters (discharged less charged), when both have
/// them and they only went up.
pub(crate) fn counters_net_out(a: &Sample, b: &Sample) -> Option<f64> {
    let out = b.total_discharge_kwh? - a.total_discharge_kwh?;
    let into = b.total_charge_kwh? - a.total_charge_kwh?;
    (out >= 0.0 && into >= 0.0).then_some(out - into)
}

/// Energy into the pack between two samples by its lifetime charge counter.
pub(crate) fn counters_in(a: &Sample, b: &Sample) -> Option<f64> {
    let into = b.total_charge_kwh? - a.total_charge_kwh?;
    (into >= 0.0).then_some(into)
}

const EARTH_KM: f64 = 6371.0088;

/// Great-circle distance, km.
pub(crate) fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, la2) = (a.0.to_radians(), b.0.to_radians());
    let dla = la2 - la1;
    let dlo = (b.1 - a.1).to_radians();
    let h = (dla / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlo / 2.0).sin().powi(2);
    2.0 * EARTH_KM * h.sqrt().min(1.0).asin()
}

/// No car moves faster than this between two fixes (km/h): a step that implies more is a bad fix on one side.
pub const MAX_FIX_KMH: f64 = 400.0;

/// Steps this short always pass the speed check (km): a fix's noise, and two inputs' fixes of the same moment
/// that disagree (an API's position lags the car's own by up to its hold time). What is left out is a jump to where
/// the car cannot have been, not a fix a little off.
const FIX_SLACK_KM: f64 = 3.0;

/// How many runs of agreeing fixes back a run looks for one it can follow (keeps a noisy day linear).
const RUN_LOOKBACK: usize = 256;

/// The value a 28-bit signed position field with all but its sign bit clear decodes to (0x04F's lowest raw value,
/// 1e-6 degrees a step): a placeholder, never a real latitude or longitude.
pub const FIX_PLACEHOLDER_DEG: f64 = 134.217728;

/// Whether a latitude, longitude pair can be a real fix: finite, on the globe, not 0,0 and not a placeholder.
pub fn real_fix(lat: f64, lon: f64) -> bool {
    let placeholder = |v: f64| (v.abs() - FIX_PLACEHOLDER_DEG).abs() < 1e-6;
    lat.is_finite()
        && lon.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon)
        && !(lat == 0.0 && lon == 0.0)
        && !placeholder(lat)
        && !placeholder(lon)
}

/// Whether going from fix `a` to fix `b` (lat, lon, ms) is possible at `max_kmh`.
fn reachable(a: (f64, f64, u64), b: (f64, f64, u64), max_kmh: f64) -> bool {
    let hours = b.2.abs_diff(a.2) as f64 / 3_600_000.0;
    haversine_km((a.0, a.1), (b.0, b.1)) <= max_kmh * hours + FIX_SLACK_KM
}

/// Takes out the fixes (latitude and longitude, with the accuracy) that cannot be real: a placeholder, or a fix
/// that implies a speed over `max_kmh` from its neighbours. The fixes (in time order) are cut into runs where each
/// step is possible; the runs kept are the chain with the most fixes in which each run can be reached from the one
/// before it, so a few bad fixes (a jump away and back, a stale fix at the start, a stretch decoded wrong) are left
/// out whichever side of them the good ones are. The other values of those samples stay. Returns how many fixes
/// were taken out.
pub fn drop_bad_fixes(s: &mut [Sample], max_kmh: f64) -> u64 {
    let mut dropped = 0u64;
    let mut fixes: Vec<(usize, (f64, f64, u64))> = Vec::new();
    for (i, x) in s.iter_mut().enumerate() {
        match (x.lat, x.lon) {
            (Some(lat), Some(lon)) if real_fix(lat, lon) => fixes.push((i, (lat, lon, x.t_ms))),
            (None, None) => {}
            _ => {
                // half a fix, or a placeholder
                (x.lat, x.lon, x.gps_accuracy_m) = (None, None, None);
                dropped += 1;
            }
        }
    }
    // runs of fixes whose every step is possible: (first, last) indexes into `fixes`
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for k in 0..fixes.len() {
        match runs.last_mut() {
            Some(r) if reachable(fixes[r.1].1, fixes[k].1, max_kmh) => r.1 = k,
            _ => runs.push((k, k)),
        }
    }
    if runs.len() < 2 {
        return dropped;
    }
    // the chain of runs with the most fixes: best[j] = (fixes in the best chain ending with run j, the run before)
    let size = |r: (usize, usize)| r.1 - r.0 + 1;
    let mut best: Vec<(usize, Option<usize>)> = Vec::with_capacity(runs.len());
    for j in 0..runs.len() {
        let mut pick = (size(runs[j]), None);
        for i in j.saturating_sub(RUN_LOOKBACK)..j {
            let n = best[i].0 + size(runs[j]);
            if n > pick.0 && reachable(fixes[runs[i].1].1, fixes[runs[j].0].1, max_kmh) {
                pick = (n, Some(i));
            }
        }
        best.push(pick);
    }
    let mut keep = vec![false; runs.len()];
    let mut at = best.iter().enumerate().max_by(|a, b| a.1.0.cmp(&b.1.0).then(b.0.cmp(&a.0))).map(|(j, _)| j);
    while let Some(j) = at {
        keep[j] = true;
        at = best[j].1;
    }
    for run in runs.iter().zip(&keep).filter(|(_, k)| !**k).map(|(r, _)| r) {
        for &(i, _) in &fixes[run.0..=run.1] {
            let x = &mut s[i];
            (x.lat, x.lon, x.gps_accuracy_m) = (None, None, None);
            dropped += 1;
        }
    }
    dropped
}

/// Fixes in a row that agree with each other but not with the last good one: that one was the bad fix (a stale
/// position at the start), so the track goes on from them.
const REANCHOR_AFTER: usize = 3;

/// The length of the GPS track, km: steps between fixes, leaving out jumps faster than `max_kmh` (a bad fix) and
/// steps while the car says it is standing (the fix wanders at a stop). `None` with fewer than two fixes.
pub(crate) fn gps_km(s: &[&Sample], max_kmh: f64) -> Option<f64> {
    let plausible = |a: (f64, f64, u64), b: (f64, f64, u64)| {
        let hours = b.2.saturating_sub(a.2).max(1) as f64 / 3_600_000.0;
        haversine_km((a.0, a.1), (b.0, b.1)) / hours <= max_kmh
    };
    let mut last: Option<(f64, f64, u64)> = None;
    // fixes left out since the last good one, as (the newest, how many in a row agree with each other)
    let mut left_out: Option<((f64, f64, u64), usize)> = None;
    let mut total = 0.0;
    let mut steps = 0;
    for x in s {
        let (Some(lat), Some(lon)) = (x.lat, x.lon) else { continue };
        let here = (lat, lon, x.t_ms);
        if let Some((la, lo, t)) = last {
            let d = haversine_km((la, lo), (lat, lon));
            if !plausible((la, lo, t), here) {
                // keep measuring from the last good fix, unless the fixes left out agree with each other
                let run = match left_out {
                    Some((prev, n)) if plausible(prev, here) => n + 1,
                    _ => 1,
                };
                left_out = Some((here, run));
                if run >= REANCHOR_AFTER {
                    last = Some(here);
                    left_out = None;
                }
                continue;
            }
            left_out = None;
            if x.speed_kmh.is_none_or(|v| v.abs() > 0.5) {
                total += d;
            }
            steps += 1;
        }
        last = Some((lat, lon, x.t_ms));
    }
    (steps > 0).then_some(total)
}

/// Distance from the speed alone, km (only to judge whether a drive moved at all).
pub(crate) fn speed_km(s: &[&Sample], end_ms: u64, max_hold_ms: u64) -> Option<f64> {
    integrate_kwh(s, end_ms, |_| max_hold_ms, |x| x.speed_kmh.map(f64::abs), |_, _| None)
}

/// Elevation changes smaller than this are noise (Elevation3D8 has 1 m steps), m.
const CLIMB_STEP_M: f64 = 2.0;

/// Metres climbed and descended, counted in steps of at least [`CLIMB_STEP_M`].
pub(crate) fn climb(s: &[&Sample]) -> (Option<f64>, Option<f64>) {
    let mut reference: Option<f64> = None;
    let (mut up, mut down) = (0.0, 0.0);
    for e in s.iter().filter_map(|x| x.elevation_m) {
        match reference {
            None => reference = Some(e),
            Some(r) if e - r >= CLIMB_STEP_M => {
                up += e - r;
                reference = Some(e);
            }
            Some(r) if r - e >= CLIMB_STEP_M => {
                down += r - e;
                reference = Some(e);
            }
            Some(_) => {}
        }
    }
    if reference.is_some() { (Some(up), Some(down)) } else { (None, None) }
}

pub(crate) fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, n) = values.fold((0.0, 0u64), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / n as f64)
}

pub(crate) fn max(values: impl Iterator<Item = f64>) -> Option<f64> {
    values.fold(None, |m, v| Some(m.map_or(v, |m: f64| m.max(v))))
}

pub(crate) fn min(values: impl Iterator<Item = f64>) -> Option<f64> {
    values.fold(None, |m, v| Some(m.map_or(v, |m: f64| m.min(v))))
}

/// A sample as a position, when it has one.
pub(crate) fn position(x: &Sample) -> Option<Position> {
    Some(Position {
        t_ms: x.t_ms,
        lat: x.lat?,
        lon: x.lon?,
        elevation_m: x.elevation_m,
        speed_kmh: x.speed_kmh,
        power_kw: x.battery_power_kw,
        soc: x.soc,
        odometer_km: x.odometer_km,
        range_km: x.range_km,
        outside_temp_c: x.outside_temp_c,
        inside_temp_c: x.inside_temp_c,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    fn s(t: u64, p: Option<f64>) -> Sample {
        Sample { t_ms: t, battery_power_kw: p, ..Sample::default() }
    }

    #[test]
    fn power_integrates_with_holds_and_bridges() {
        let v = [s(0, Some(36.0)), s(1000, Some(36.0)), s(2000, None), s(3000, Some(-36.0))];
        let r: Vec<&Sample> = v.iter().collect();
        // 36 kW for 1 s, then for 2 s (held across the sample without power: it says nothing about it), −36 kW for
        // its 1 s: 0.02 kWh
        let e = integrate_kwh(&r, 4000, |_| 10_000, |x| x.battery_power_kw, |_, _| None).unwrap();
        assert!((e - 0.02).abs() < 1e-12, "{e}");
        // held for the hold time at most, however many samples without the power lie in between
        let v = [s(0, Some(36.0)), s(1000, None), s(30_000, None), s(60_000, Some(36.0))];
        let r: Vec<&Sample> = v.iter().collect();
        let e = integrate_kwh(&r, 61_000, |_| 10_000, |x| x.battery_power_kw, |_, _| None).unwrap();
        assert!((e - 0.11).abs() < 1e-12, "{e}");
        // each sample has its own hold time
        let e = integrate_kwh(&r, 61_000, |i| if i == 0 { 20_000 } else { 500 }, |x| x.battery_power_kw, |_, _| None);
        assert!((e.unwrap() - 0.205).abs() < 1e-12, "{e:?}");
        // a 60 s gap: 10 s of it counts, or what the bridge says
        let v = [s(0, Some(36.0)), s(60_000, Some(36.0))];
        let r: Vec<&Sample> = v.iter().collect();
        let held = integrate_kwh(&r, 61_000, |_| 10_000, |x| x.battery_power_kw, |_, _| None).unwrap();
        assert!((held - 0.11).abs() < 1e-12, "{held}");
        let bridged = integrate_kwh(&r, 61_000, |_| 10_000, |x| x.battery_power_kw, |_, _| Some(0.5)).unwrap();
        assert!((bridged - 0.51).abs() < 1e-12);
        assert!(integrate_kwh(&[], 0, |_| 1, |x| x.battery_power_kw, |_, _| None).is_none());
    }

    #[test]
    fn distances() {
        // one degree of latitude is about 111.2 km
        let d = haversine_km((10.0, 20.0), (11.0, 20.0));
        assert!((d - 111.195).abs() < 0.01, "{d}");
        let pts = [(0.0, 0.0001), (0.0, 0.0002), (5.0, 5.0), (0.0, 0.0003)];
        let v: Vec<Sample> = pts
            .iter()
            .enumerate()
            .map(|(i, &(la, lo))| Sample { t_ms: i as u64 * 1000, lat: Some(la), lon: Some(lo), ..Sample::default() })
            .collect();
        let r: Vec<&Sample> = v.iter().collect();
        let km = gps_km(&r, 300.0).unwrap();
        // the jump to 5,5 and back is left out: 0.0002° of longitude at the equator
        assert!((km - 0.0222).abs() < 0.0005, "{km}");
    }

    #[test]
    fn a_bad_first_fix_does_not_hold_the_track_back() {
        // a stale fix 2 km south, then the car driving north at 36 km/h (10 m a second) for a minute
        let mut v = vec![Sample { t_ms: 0, lat: Some(-0.018), lon: Some(0.0), ..Sample::default() }];
        for i in 1..=60u64 {
            let lat = (i as f64 * 0.01) / 111.195;
            v.push(Sample { t_ms: i * 1000, lat: Some(lat), lon: Some(0.0), ..Sample::default() });
        }
        let r: Vec<&Sample> = v.iter().collect();
        let km = gps_km(&r, 300.0).unwrap();
        // the drive's 0.6 km, less the first steps the track waited out; not the 2 km jump from the stale fix
        assert!((0.55..=0.6).contains(&km), "{km}");
    }

    /// A made-up drive due east at about 72 km/h (20 m a second), one fix a second, from 40 N 100 W.
    fn eastward(n: u64) -> Vec<Sample> {
        let step = 0.02 / (111.195 * 40f64.to_radians().cos());
        (0..n)
            .map(|i| Sample {
                t_ms: i * 1000,
                lat: Some(40.0),
                lon: Some(-100.0 + i as f64 * step),
                gps_accuracy_m: Some(3.0),
                speed_kmh: Some(72.0),
                ..Sample::default()
            })
            .collect()
    }

    #[test]
    fn a_stretch_of_fixes_far_away_at_the_same_latitude_is_left_out() {
        // 120 s of driving; seconds 60 to 69 have a longitude 16.78 degrees east (one bit of the field off): a
        // straight line 1400 km east and a small hook there, at the same latitude
        let mut v = eastward(120);
        for x in &mut v[60..70] {
            x.lon = x.lon.map(|l| l + 16.777216);
        }
        let dropped = drop_bad_fixes(&mut v, MAX_FIX_KMH);
        assert_eq!(dropped, 10);
        assert!(v[60..70].iter().all(|x| x.lat.is_none() && x.lon.is_none() && x.gps_accuracy_m.is_none()));
        assert!(v[60..70].iter().all(|x| x.speed_kmh == Some(72.0)), "the other values stay");
        assert!(v[..60].iter().chain(&v[70..]).all(|x| x.lat == Some(40.0)));
        let r: Vec<&Sample> = v.iter().collect();
        let km = gps_km(&r, 300.0).unwrap();
        assert!((km - 2.38).abs() < 0.05, "{km}");
    }

    #[test]
    fn bad_fixes_at_the_start_or_end_and_placeholders_are_left_out() {
        let mut v = eastward(60);
        v[0].lon = Some(-80.0); // a stale fix from elsewhere
        v[59].lon = Some(-80.0);
        v[30].lon = Some(-FIX_PLACEHOLDER_DEG);
        v[31].lon = None; // half a fix
        assert_eq!(drop_bad_fixes(&mut v, MAX_FIX_KMH), 4);
        assert_eq!(v.iter().filter(|x| x.lat.is_some()).count(), 56);
        // a good drive loses nothing, and a long time between two fixes far apart is no jump
        let mut v = eastward(60);
        v.push(Sample { t_ms: 10 * 3_600_000, lat: Some(40.0), lon: Some(-90.0), ..Sample::default() });
        assert_eq!(drop_bad_fixes(&mut v, MAX_FIX_KMH), 0);
        assert!(real_fix(40.0, -100.0) && !real_fix(0.0, 0.0) && !real_fix(91.0, 0.0) && !real_fix(f64::NAN, 1.0));
    }

    #[test]
    fn climbs_ignore_small_wobbles() {
        let v: Vec<Sample> = [100.0, 101.0, 100.0, 103.0, 110.0, 109.0, 104.0]
            .iter()
            .map(|&e| Sample { elevation_m: Some(e), ..Sample::default() })
            .collect();
        let r: Vec<&Sample> = v.iter().collect();
        assert_eq!(climb(&r), (Some(10.0), Some(6.0)));
        assert_eq!(climb(&[]), (None, None));
    }
}
