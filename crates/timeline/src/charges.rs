//! Charges: the seconds the car says it is charging, with interruptions shorter than [`Params::charge_gap_ms`] (a
//! charger pausing, a cable reseated) merged into one charge. A drive in between always splits them. A stretch with no
//! data at all (a lost log, an app update: unknown, but too short to be a settled span) is no interruption: charging on
//! both sides of it is one charge, as a drive goes on across one.

use crate::calc::{Piece, add_span, counters_in, covered_ms, integrate_pieces, max, mean, position};
use crate::timeline::{Params, RULES_VERSION, charge_id};
use model::{CarId, Charge, ChargeKind, ChargePoint, EnergySpans, Sample, Source};

/// The lifetime counter is used for the energy added when it was read this close to both ends of the charge (ms);
/// else the counter may have missed part of it and the charge power is integrated instead.
const COUNTER_EDGE_MS: u64 = 60_000;

/// The energy added and the grid energy are compared (an efficiency, losses) only when neither has more than this
/// share of itself in time the other was not measured over ([`Charge::energy_spans`])...
const SPAN_SLACK: f64 = 0.03;

/// ... or this much (kWh): a few seconds at an edge of a short charge are no reason not to.
const SPAN_SLACK_KWH: f64 = 0.02;

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
/// before it since the last drive, for where the car is when the charge itself has no GPS fix. `holds`: how long each
/// of `s` counts across a gap in the energy sums (ms; empty: [`Params::max_hold_ms`] for all).
pub(crate) fn make(car: CarId, s: &[&Sample], before: &[&Sample], params: &Params, holds: &[u64]) -> Option<Charge> {
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

    let hold = params.max_hold_ms;
    let hold_of = |i: usize| holds.get(i).copied().unwrap_or(hold);
    // into the pack, second by second: the charge power, else the pack's own power while it takes energy (a second
    // whose charging state came from another input has the pack's power but no charge power of its own)
    let pack_kw = |x: &Sample| x.charge_power_kw.or_else(|| x.battery_power_kw.map(|p| -p).filter(|p| *p > 0.0));
    // (a second that says the car is not charging ends the reading before it: a pause in the charge)
    let off = |x: &Sample| x.charging == Some(false);
    let pack = integrate_pieces(s, end_ms, hold_of, pack_kw, off, counters_in);
    // into the pack: the lifetime counter where it covers the charge (from its first reading to its last, that second
    // included, with the pack's power before and after them), else the power integrated (read from the second before
    // the charge when there is one: the first charging second already counted)
    let just_before = before.last().filter(|p| start_ms.saturating_sub(p.t_ms) <= hold);
    let has = |x: &&&Sample| x.total_charge_kwh.is_some();
    let counted = just_before.filter(has).or_else(|| s.iter().find(has)).zip(s.iter().rev().find(has));
    let by_counter = counted
        .filter(|(a, b)| {
            a.t_ms.saturating_sub(start_ms) <= COUNTER_EDGE_MS && end_ms.saturating_sub(b.t_ms) <= COUNTER_EDGE_MS
        })
        .and_then(|(a, b)| Some((counters_in(a, b)?, a.t_ms, b.t_ms)));
    let (energy_added_kwh, added_spans) = match (by_counter, &pack) {
        (Some((kwh, a, b)), _) => {
            let edges: Vec<Piece> = pack.iter().flat_map(|(_, v)| v.iter()).filter_map(|p| edge(p, a, b)).collect();
            let mut spans = Vec::new();
            for p in edges.iter().filter(|p| p.to <= a) {
                add_span(&mut spans, p.from, p.to);
            }
            add_span(&mut spans, a.max(start_ms), b.saturating_add(1000).min(end_ms));
            for p in edges.iter().filter(|p| p.from > b) {
                add_span(&mut spans, p.from, p.to);
            }
            (Some(kwh + edges.iter().map(Piece::kwh).sum::<f64>()), spans)
        }
        (None, Some((kwh, v))) => (Some(*kwh), spans_of(v)),
        (None, None) => (None, Vec::new()),
    };
    // from the grid: what went into the onboard charger (AC only; a DC charger's own losses are out of sight)
    let grid = match kind {
        Some(ChargeKind::Ac) => integrate_pieces(s, end_ms, hold_of, grid_kw, off, |_, _| None),
        _ => None,
    };
    let energy_used_kwh = grid.as_ref().map(|g| g.0);
    // the two compared only over one span: each with little of its energy where the other was not measured
    let energy_spans = match (energy_added_kwh, &grid, &pack) {
        (Some(added), Some((used, g)), pack) => {
            let grid_spans = spans_of(g);
            let off = |p: &Piece, spans: &[(u64, u64)]| {
                let ms = p.to.saturating_sub(p.from);
                p.kwh() * ms.saturating_sub(covered_ms(spans, p.from, p.to)) as f64 / ms.max(1) as f64
            };
            let grid_off: f64 = g.iter().map(|p| off(p, &added_spans)).sum();
            let added_off: f64 = pack.iter().flat_map(|(_, v)| v.iter()).map(|p| off(p, &grid_spans)).sum();
            let too_much = |x: f64, of: f64| x > SPAN_SLACK_KWH.max(SPAN_SLACK * of.abs());
            (too_much(grid_off, *used) || too_much(added_off, added)).then(|| {
                let len = |v: &[(u64, u64)]| covered_ms(v, start_ms, end_ms);
                let both: u64 =
                    grid_spans.iter().map(|&(a, b)| covered_ms(&added_spans, a.max(start_ms), b.min(end_ms))).sum();
                EnergySpans { added_ms: len(&added_spans), grid_ms: len(&grid_spans), both_ms: both }
            })
        }
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
        energy_spans,
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

/// One phase's power is not told from a whole number of phases below this (kW): the line's own power has 0.1 kW
/// steps.
const PHASES_FROM_KW: f64 = 0.5;

/// The power on the AC line, kW: the charger's volts times amps, which are one phase's, times the phases in use.
/// How many: the line's own power where the car sends it (coarse, so it only says how many times one phase's power
/// it is), else the phase count the input gives, else one.
fn grid_kw(x: &Sample) -> Option<f64> {
    let one = x.charger_voltage_v.zip(x.charger_current_a).map(|(v, a)| v * a / 1000.0)?;
    let by_line = x.extra.charger_line_kw.filter(|_| one >= PHASES_FROM_KW).map(|kw| (kw / one).round());
    let phases = by_line.or(x.extra.charger_phases).filter(|n| (1.0..=3.0).contains(n)).unwrap_or(1.0);
    Some(one * phases.round())
}

/// The part of a piece of the pack's power outside the lifetime counter's readings `[a, b]` (the second of `b`
/// counted by it), if any.
fn edge(p: &Piece, a: u64, b: u64) -> Option<Piece> {
    if p.from < a {
        Some(Piece { to: p.to.min(a), ..*p })
    } else if p.from > b {
        Some(*p)
    } else {
        None
    }
}

/// The time pieces cover, as sorted spans apart from each other.
fn spans_of(pieces: &[Piece]) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    for p in pieces {
        add_span(&mut out, p.from, p.to);
    }
    out
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
