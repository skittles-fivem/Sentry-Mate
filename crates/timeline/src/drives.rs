//! Drives: the gear leaves P, and the drive ends once it has been back in P (or the inverter silent) for
//! [`Params::park_ms`]. D, R or N held at a stop (a red light, a queue) is the same drive; so is a short gap in the
//! data with the gear out of P on both sides.

use crate::calc::{climb, counters_net_out, gps_km, integrate_kwh, max, mean, min, position, speed_km};
use crate::timeline::{Params, RULES_VERSION, drive_id};
use model::{CarId, Drive, Gear, Sample, Source};

pub(crate) fn moving(s: &Sample) -> bool {
    matches!(s.gear, Some(Gear::D | Gear::R | Gear::N))
}

/// The drives of a run of samples (sorted, nothing settled in between) as index ranges `[first, last]`: from the
/// first second out of P to the last.
pub(crate) fn find(seg: &[&Sample], params: &Params) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut open: Option<(usize, usize)> = None;
    for (i, s) in seg.iter().enumerate() {
        match open.as_mut() {
            None if moving(s) => open = Some((i, i)),
            None => {}
            Some((_, last)) if moving(s) => *last = i,
            Some((first, last)) => {
                let parked_since = seg[*last].t_ms + 1000;
                if s.t_ms.saturating_sub(parked_since) >= params.park_ms {
                    out.push((*first, *last));
                    open = None;
                }
            }
        }
    }
    out.extend(open);
    out
}

/// A drive from its samples (`s[0]` the first second out of P, the last the last one), or `None` when it is too
/// short or didn't move. `prev` is the sample just before it, when there is one: where the drive started from (its
/// odometer and position before the first second moved them).
pub(crate) fn make(car: CarId, s: &[&Sample], prev: Option<&Sample>, params: &Params) -> Option<Drive> {
    let (first, last) = (s.first()?, s.last()?);
    let (start_ms, end_ms) = (first.t_ms, last.t_ms + 1000);
    let prev = prev.filter(|p| start_ms.saturating_sub(p.t_ms) <= params.max_hold_ms);
    let track: Vec<&Sample> = prev.into_iter().chain(s.iter().copied()).collect();
    let start_odometer_km = track.iter().find_map(|x| x.odometer_km);
    let end_odometer_km = s.iter().rev().find_map(|x| x.odometer_km);
    let by_odometer = start_odometer_km.zip(end_odometer_km).map(|(a, b)| b - a).filter(|d| *d >= 0.0);
    let distance_km = by_odometer.or_else(|| gps_km(&track, params.max_gps_kmh));
    if end_ms - start_ms < params.min_drive_ms {
        return None;
    }
    // with no odometer or GPS, the speed says whether it moved at all
    if distance_km.or_else(|| speed_km(s, end_ms, params.max_hold_ms)).is_some_and(|d| d < params.min_drive_km) {
        return None;
    }
    let (ascent_m, descent_m) = climb(s);
    let start = track.iter().find_map(|x| position(x));
    let end = s.iter().rev().find_map(|x| position(x));
    Some(Drive {
        id: drive_id(car, start_ms),
        car,
        start_ms,
        end_ms,
        start,
        end,
        start_place: None,
        end_place: None,
        start_address: None,
        end_address: None,
        start_odometer_km,
        end_odometer_km,
        distance_km,
        speed_max_kmh: max(s.iter().filter_map(|x| x.speed_kmh)),
        power_max_kw: max(s.iter().filter_map(|x| x.battery_power_kw)),
        power_min_kw: min(s.iter().filter_map(|x| x.battery_power_kw)),
        // regenerated energy is negative power, so it comes off by itself
        energy_used_kwh: integrate_kwh(s, end_ms, params.max_hold_ms, |x| x.battery_power_kw, counters_net_out),
        start_soc: s.iter().find_map(|x| x.soc),
        end_soc: s.iter().rev().find_map(|x| x.soc),
        start_range_km: s.iter().find_map(|x| x.range_km),
        end_range_km: s.iter().rev().find_map(|x| x.range_km),
        outside_temp_avg_c: mean(s.iter().filter_map(|x| x.outside_temp_c)),
        inside_temp_avg_c: mean(s.iter().filter_map(|x| x.inside_temp_c)),
        ascent_m,
        descent_m,
        source: Source::Live,
        inputs: model::Inputs::SMO,
        rules_version: RULES_VERSION,
    })
}
