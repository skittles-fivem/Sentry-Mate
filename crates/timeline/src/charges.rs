//! Charges: the seconds the car says it is charging, with interruptions shorter than [`Params::charge_gap_ms`] (a
//! charger pausing, a cable reseated) merged into one charge. A drive in between always splits them. A stretch with no
//! data at all (a lost log, an app update: unknown, but too short to be a settled span) is no interruption: charging on
//! both sides of it is one charge, as a drive goes on across one.

use crate::calc::{counters_in, integrate_kwh, max, mean, position};
use crate::timeline::{Params, RULES_VERSION, charge_id};
use model::{CarId, Charge, ChargeKind, ChargePoint, Sample, Source};

/// The lifetime counter is used for the energy added when it was read this close to both ends of the charge (ms);
/// else the counter may have missed part of it and the charge power is integrated instead.
const COUNTER_EDGE_MS: u64 = 60_000;

/// The charges of a run of samples as index ranges `[first, last]` (first and last charging second).
pub(crate) fn find(seg: &[&Sample], drives: &[(usize, usize)], params: &Params) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (i, s) in seg.iter().enumerate().filter(|(_, s)| s.charging == Some(true)) {
        if let Some((_, last)) = out.last_mut() {
            let gap = s.t_ms.saturating_sub(seg[*last].t_ms + 1000);
            // (drives are in order: the first one that starts after the last charging second)
            let next_drive = drives.partition_point(|&(a, _)| a <= *last);
            let drive_between = drives.get(next_drive).is_some_and(|&(a, _)| a < i);
            // (no sample at all in between: nothing says it stopped)
            let no_data_between = i == *last + 1;
            if (gap < params.charge_gap_ms || no_data_between) && !drive_between {
                *last = i;
                continue;
            }
        }
        out.push((i, i));
    }
    out
}

/// A charge from its samples (`s[0]` the first charging second, the last the last one); `before` are the samples
/// before it since the last drive, for where the car is when the charge itself has no GPS fix.
pub(crate) fn make(car: CarId, s: &[&Sample], before: &[&Sample], params: &Params) -> Option<Charge> {
    let (first, last) = (s.first()?, s.last()?);
    let (start_ms, end_ms) = (first.t_ms, last.t_ms + 1000);
    let charging: Vec<&Sample> = s.iter().copied().filter(|x| x.charging == Some(true)).collect();

    // which charger: most of the charging seconds say (a tie goes to DC)
    let dc = charging.iter().filter(|x| x.charge_kind == Some(ChargeKind::Dc)).count();
    let ac = charging.iter().filter(|x| x.charge_kind == Some(ChargeKind::Ac)).count();
    let kind = match (ac, dc) {
        (0, 0) => None,
        (ac, dc) if dc >= ac => Some(ChargeKind::Dc),
        _ => Some(ChargeKind::Ac),
    };

    // into the pack: the lifetime counter where it covers the charge, else the charge power integrated
    // (read from the second before the charge when there is one: the first charging second already counted)
    let just_before = before.last().filter(|p| start_ms.saturating_sub(p.t_ms) <= params.max_hold_ms);
    let has = |x: &&&Sample| x.total_charge_kwh.is_some();
    let counted = just_before.filter(has).or_else(|| s.iter().find(has)).zip(s.iter().rev().find(has));
    let by_counter = counted
        .filter(|(a, b)| {
            a.t_ms.saturating_sub(start_ms) <= COUNTER_EDGE_MS && end_ms.saturating_sub(b.t_ms) <= COUNTER_EDGE_MS
        })
        .and_then(|(a, b)| counters_in(a, b));
    let energy_added_kwh =
        by_counter.or_else(|| integrate_kwh(s, end_ms, params.max_hold_ms, |x| x.charge_power_kw, counters_in));
    // from the grid: what went into the onboard charger (AC only; a DC charger's own losses are out of sight)
    let energy_used_kwh = match kind {
        Some(ChargeKind::Ac) => integrate_kwh(
            s,
            end_ms,
            params.max_hold_ms,
            |x| x.charger_voltage_v.zip(x.charger_current_a).map(|(v, a)| v * a / 1000.0),
            |_, _| None,
        ),
        _ => None,
    };
    let position = s.iter().find_map(|x| position(x)).or_else(|| before.iter().rev().find_map(|x| position(x)));
    Some(Charge {
        id: charge_id(car, start_ms),
        car,
        start_ms,
        end_ms,
        position,
        place: None,
        address: None,
        kind,
        energy_added_kwh,
        energy_used_kwh,
        start_soc: s.iter().find_map(|x| x.soc),
        end_soc: s.iter().rev().find_map(|x| x.soc),
        start_range_km: s.iter().find_map(|x| x.range_km),
        end_range_km: s.iter().rev().find_map(|x| x.range_km),
        power_max_kw: max(s.iter().filter_map(|x| x.charge_power_kw)),
        outside_temp_avg_c: mean(s.iter().filter_map(|x| x.outside_temp_c)),
        battery_temp_avg_c: mean(s.iter().filter_map(|x| x.battery_temp_c)),
        cost: None,
        source: Source::Live,
        inputs: model::Inputs::SMO,
        rules_version: RULES_VERSION,
    })
}

/// The charge curve: every second of the charge.
pub(crate) fn points(s: &[&Sample]) -> Vec<ChargePoint> {
    s.iter()
        .map(|x| ChargePoint {
            t_ms: x.t_ms,
            power_kw: x.charge_power_kw,
            voltage_v: x.charger_voltage_v,
            current_a: x.charger_current_a,
            soc: x.soc,
            battery_temp_c: x.battery_temp_c,
            range_km: x.range_km,
        })
        .collect()
}
