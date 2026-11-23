//! Small sums over a run of samples: energy from power, distance from GPS or speed, climbs, averages.

use model::{Position, Sample};

/// kW held for a number of milliseconds, in kWh.
fn kwh(kw: f64, ms: u64) -> f64 {
    kw * ms as f64 / 3_600_000.0
}

/// The integral of a power over samples (kWh): each sample's power counts until the next sample, or for `max_hold_ms`
/// at most across a gap. A longer gap is bridged with `bridge(before, after)` when it can say what happened (the
/// pack's lifetime counters), else only `max_hold_ms` of it counts. The last sample counts until `end_ms`. `None` when
/// no sample has the power.
pub(crate) fn integrate_kwh(
    s: &[&Sample],
    end_ms: u64,
    max_hold_ms: u64,
    power: impl Fn(&Sample) -> Option<f64>,
    bridge: impl Fn(&Sample, &Sample) -> Option<f64>,
) -> Option<f64> {
    integrate(s, end_ms, max_hold_ms, power, bridge, |_, _, _| {})
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

/// [`integrate_kwh`], and the pieces its sum is made of, by time.
pub(crate) fn integrate_pieces(
    s: &[&Sample],
    end_ms: u64,
    max_hold_ms: u64,
    power: impl Fn(&Sample) -> Option<f64>,
    bridge: impl Fn(&Sample, &Sample) -> Option<f64>,
) -> Option<(f64, Vec<Piece>)> {
    let mut out = Vec::new();
    let sum = integrate(s, end_ms, max_hold_ms, power, bridge, |from, to, e| {
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
    max_hold_ms: u64,
    power: impl Fn(&Sample) -> Option<f64>,
    bridge: impl Fn(&Sample, &Sample) -> Option<f64>,
    mut piece: impl FnMut(u64, u64, f64),
) -> Option<f64> {
    let mut sum = 0.0;
    let mut any = false;
    for (i, x) in s.iter().enumerate() {
        let Some(p) = power(x) else { continue };
        any = true;
        let next = s.get(i + 1);
        let to = next.map_or(end_ms, |n| n.t_ms);
        let dt = to.saturating_sub(x.t_ms);
        let (e, until) = if dt <= max_hold_ms {
            (kwh(p, dt), to)
        } else if let Some(e) = next.and_then(|n| bridge(x, n)) {
            (e, to)
        } else {
            (kwh(p, max_hold_ms), x.t_ms + max_hold_ms)
        };
        sum += e;
        piece(x.t_ms, until, e);
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
    integrate_kwh(s, end_ms, max_hold_ms, |x| x.speed_kmh.map(f64::abs), |_, _| None)
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
        // 36 kW for 1 s + 1 s, nothing for the sample without power, −36 kW for its 1 s: 0.01 kWh
        let e = integrate_kwh(&r, 4000, 10_000, |x| x.battery_power_kw, |_, _| None).unwrap();
        assert!((e - 0.01).abs() < 1e-12, "{e}");
        // a 60 s gap: 10 s of it counts, or what the bridge says
        let v = [s(0, Some(36.0)), s(60_000, Some(36.0))];
        let r: Vec<&Sample> = v.iter().collect();
        let held = integrate_kwh(&r, 61_000, 10_000, |x| x.battery_power_kw, |_, _| None).unwrap();
        assert!((held - 0.11).abs() < 1e-12, "{held}");
        let bridged = integrate_kwh(&r, 61_000, 10_000, |x| x.battery_power_kw, |_, _| Some(0.5)).unwrap();
        assert!((bridged - 0.51).abs() < 1e-12);
        assert!(integrate_kwh(&[], 0, 1, |x| x.battery_power_kw, |_, _| None).is_none());
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
