//! Two live inputs over the same time, made one: SMO's samples (1 Hz, from the car's own buses) and the Tesla API's
//! (every few seconds while streaming, every half minute to minute when polled).
//!
//! - **Inside SMO's coverage** (seconds between two SMO samples at most [`FuseParams::smo_gap_ms`] apart), SMO's
//!   sample is the second, and every value it has is SMO's. A value it lacks is taken from the nearest API sample
//!   within that value's hold time ([`FuseParams`]). No API sample becomes a second of its own there: the rates are
//!   never mixed and no second is counted twice.
//! - **Outside it** (SMO offline, not installed, a failed log), the API's samples are the seconds, at their own times.
//!
//! [`Fused::marks`] says which seconds are not SMO's alone (API-only seconds, and SMO seconds with values from the
//! API): the rows built from them name both inputs. Pure and deterministic: the same samples give the same answer,
//! and a second's answer only looks at samples within the longest hold time of it, so a window that brings that much
//! API data on each side gives every second inside it as a longer window would.

use model::{Inputs, Sample};
use serde::{Deserialize, Serialize};

/// What the Tesla API said about the car's state over a span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiState {
    Online,
    Asleep,
    Offline,
}

/// A span the Tesla API saw the car in one state. `data`: the API sent the car's values in it (vehicle data or the
/// stream), not only its state in the vehicle list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApiSpan {
    pub start_ms: u64,
    pub end_ms: u64,
    pub state: ApiState,
    #[serde(default)]
    pub data: bool,
}

/// How long an API value may stand in for a value SMO did not send that second (ms).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FuseParams {
    /// SMO samples this close are one stretch of coverage.
    pub smo_gap_ms: u64,
    /// Speed, gear, power, charging and the charger.
    pub fast_ms: u64,
    /// Position and elevation.
    pub position_ms: u64,
    /// Battery level and range.
    pub battery_ms: u64,
    pub odometer_ms: u64,
    /// Temperatures and tire pressures.
    pub slow_ms: u64,
}

impl Default for FuseParams {
    fn default() -> Self {
        FuseParams {
            smo_gap_ms: 5_000,
            fast_ms: 10_000,
            position_ms: 30_000,
            battery_ms: 120_000,
            odometer_ms: 60_000,
            slow_ms: 300_000,
        }
    }
}

impl FuseParams {
    /// How far around a window API samples must be read for every second inside it to fuse as in a longer one.
    pub fn margin_ms(&self) -> u64 {
        self.fast_ms.max(self.position_ms).max(self.battery_ms).max(self.odometer_ms).max(self.slow_ms)
    }
}

/// The fused samples, by time, one per second, and the seconds that are not SMO's alone (by time).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Fused {
    pub samples: Vec<Sample>,
    /// `(t_ms, inputs)` for every second whose values are not all SMO's: `TESLA_API` for an API second, `BOTH` for
    /// an SMO second with values from the API. A second not here is SMO's.
    pub marks: Vec<(u64, Inputs)>,
}

impl Fused {
    /// The inputs of the second at `t_ms` (SMO when it is not marked).
    pub fn inputs_at(&self, t_ms: u64) -> Inputs {
        inputs_at(&self.marks, t_ms)
    }
}

/// The inputs of a second in sorted marks (SMO when it is not marked).
pub fn inputs_at(marks: &[(u64, Inputs)], t_ms: u64) -> Inputs {
    match marks.binary_search_by_key(&t_ms, |m| m.0) {
        Ok(i) => marks[i].1,
        Err(_) => Inputs::SMO,
    }
}

/// Sorted by time, one per second (of two with the same second, the later one).
fn seconds(v: &[Sample]) -> Vec<&Sample> {
    let mut s: Vec<&Sample> = v.iter().collect();
    s.sort_by_key(|x| x.t_ms);
    let mut out: Vec<&Sample> = Vec::with_capacity(s.len());
    for x in s {
        match out.last_mut() {
            Some(l) if l.t_ms == x.t_ms => *l = x,
            _ => out.push(x),
        }
    }
    out
}

/// The times of the API samples that have one field, for the nearest-within-hold look-ups.
struct Field<'a> {
    at: Vec<(u64, &'a Sample)>,
}

impl<'a> Field<'a> {
    fn new(api: &[&'a Sample], has: impl Fn(&Sample) -> bool) -> Field<'a> {
        Field { at: api.iter().filter(|s| has(s)).map(|s| (s.t_ms, *s)).collect() }
    }

    /// The API sample with this field nearest `t` within `hold` (the earlier one on a tie).
    fn near(&self, t: u64, hold: u64) -> Option<&'a Sample> {
        let i = self.at.partition_point(|(x, _)| *x < t);
        let after = self.at.get(i).filter(|(x, _)| x - t <= hold);
        let before = i.checked_sub(1).map(|j| &self.at[j]).filter(|(x, _)| t - x <= hold);
        match (before, after) {
            (Some(b), Some(a)) => Some(if a.0 - t < t - b.0 { a.1 } else { b.1 }),
            (Some(b), None) => Some(b.1),
            (None, Some(a)) => Some(a.1),
            (None, None) => None,
        }
    }
}

/// One value of a sample that the API can fill in: its hold time, whether a sample has it, and its copy.
struct Slot {
    hold: fn(&FuseParams) -> u64,
    has: fn(&Sample) -> bool,
    copy: fn(&mut Sample, &Sample),
}

macro_rules! slots {
    ($($hold:ident: $($f:ident),+);+) => {
        &[$($(Slot {
            hold: |p| p.$hold,
            has: |x| x.$f.is_some(),
            copy: |to, from| to.$f = from.$f,
        },)+)+
        Slot { hold: |p| p.slow_ms, has: |x| x.tpms_bar[0].is_some(), copy: |to, from| to.tpms_bar[0] = from.tpms_bar[0] },
        Slot { hold: |p| p.slow_ms, has: |x| x.tpms_bar[1].is_some(), copy: |to, from| to.tpms_bar[1] = from.tpms_bar[1] },
        Slot { hold: |p| p.slow_ms, has: |x| x.tpms_bar[2].is_some(), copy: |to, from| to.tpms_bar[2] = from.tpms_bar[2] },
        Slot { hold: |p| p.slow_ms, has: |x| x.tpms_bar[3].is_some(), copy: |to, from| to.tpms_bar[3] = from.tpms_bar[3] },
        ]
    };
}

const SLOTS: &[Slot] = slots!(
    fast_ms: speed_kmh, gear, battery_power_kw, battery_voltage_v, battery_current_a, charging, charge_kind,
        charge_power_kw, charger_voltage_v, charger_current_a;
    position_ms: lat, lon, gps_accuracy_m, elevation_m;
    battery_ms: soc, soc_bms, range_km, ideal_range_km, energy_remaining_kwh;
    odometer_ms: odometer_km;
    slow_ms: outside_temp_c, inside_temp_c, battery_temp_c
);

/// Fuses SMO's samples and the API's (each any order). See the module's notes.
pub fn fuse(smo: &[Sample], api: &[Sample], p: &FuseParams) -> Fused {
    let smo = seconds(smo);
    let api = seconds(api);
    if api.is_empty() {
        return Fused { samples: smo.into_iter().cloned().collect(), marks: Vec::new() };
    }
    let smo_t: Vec<u64> = smo.iter().map(|s| s.t_ms).collect();
    // whether a time falls in SMO's coverage: an SMO sample that second, or two around it close enough
    let covered = |t: u64| {
        let i = smo_t.partition_point(|&x| x < t);
        if smo_t.get(i) == Some(&t) {
            return true;
        }
        match (i.checked_sub(1).map(|j| smo_t[j]), smo_t.get(i)) {
            (Some(a), Some(&b)) => b - a <= p.smo_gap_ms,
            _ => false,
        }
    };
    // only the API samples near SMO's seconds are looked at for filling
    let near_smo: Vec<&Sample> = api
        .iter()
        .copied()
        .filter(|a| {
            let i = smo_t.partition_point(|&x| x < a.t_ms);
            let m = p.margin_ms();
            smo_t.get(i).is_some_and(|&x| x - a.t_ms <= m) || i.checked_sub(1).is_some_and(|j| a.t_ms - smo_t[j] <= m)
        })
        .collect();
    let mut out: Vec<(Sample, Inputs)> = Vec::with_capacity(smo.len() + api.len());
    if near_smo.is_empty() {
        out.extend(smo.iter().map(|s| ((*s).clone(), Inputs::SMO)));
    } else {
        let looks: Vec<Field<'_>> = SLOTS.iter().map(|slot| Field::new(&near_smo, slot.has)).collect();
        for s in &smo {
            let mut x = (*s).clone();
            let api_vals = fill_one(&mut x, p, &looks);
            out.push((x, if api_vals { Inputs::BOTH } else { Inputs::SMO }));
        }
    }
    out.extend(api.iter().filter(|a| !covered(a.t_ms)).map(|a| ((*a).clone(), Inputs::TESLA_API)));
    out.sort_by_key(|(s, _)| s.t_ms);
    let marks = out.iter().filter(|(_, i)| *i != Inputs::SMO).map(|(s, i)| (s.t_ms, *i)).collect();
    Fused { samples: out.into_iter().map(|(s, _)| s).collect(), marks }
}

fn fill_one(s: &mut Sample, p: &FuseParams, looks: &[Field<'_>]) -> bool {
    let mut any = false;
    for (slot, look) in SLOTS.iter().zip(looks) {
        if !(slot.has)(s)
            && let Some(a) = look.near(s.t_ms, (slot.hold)(p))
        {
            (slot.copy)(s, a);
            any = true;
        }
    }
    any
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    fn smo(t: u64) -> Sample {
        Sample {
            t_ms: t * 1000,
            speed_kmh: Some(50.0),
            soc: Some(70.0),
            lat: Some(1.0),
            lon: Some(2.0),
            ..Sample::default()
        }
    }

    fn api(t: u64) -> Sample {
        Sample {
            t_ms: t * 1000,
            speed_kmh: Some(48.0),
            soc: Some(69.0),
            lat: Some(1.1),
            lon: Some(2.1),
            inside_temp_c: Some(21.0),
            ..Sample::default()
        }
    }

    #[test]
    fn smo_wins_inside_its_coverage_and_the_api_fills_what_it_lacks() {
        let s: Vec<Sample> = (0..60).map(smo).collect();
        let a: Vec<Sample> = (0..6).map(|i| api(i * 10 + 5)).collect();
        let f = fuse(&s, &a, &FuseParams::default());
        assert_eq!(f.samples.len(), 60, "no API second of its own inside SMO's coverage");
        assert!(f.samples.iter().all(|x| x.speed_kmh == Some(50.0) && x.soc == Some(70.0) && x.lat == Some(1.0)));
        assert!(f.samples.iter().all(|x| x.inside_temp_c == Some(21.0)), "the cabin temperature from the API");
        assert_eq!(f.marks.len(), 60);
        assert!(f.marks.iter().all(|m| m.1 == Inputs::BOTH));
    }

    #[test]
    fn outside_smo_the_api_samples_are_the_seconds() {
        // SMO for a minute, then nothing for ten, the API every 30 s all along
        let s: Vec<Sample> = (0..60).map(smo).collect();
        let a: Vec<Sample> = (0..22).map(|i| api(i * 30)).collect();
        let f = fuse(&s, &a, &FuseParams::default());
        let api_only: Vec<u64> = f.marks.iter().filter(|m| m.1 == Inputs::TESLA_API).map(|m| m.0 / 1000).collect();
        assert_eq!(api_only, (2..22).map(|i| i * 30).collect::<Vec<u64>>());
        assert_eq!(f.samples.len(), 60 + 20);
        assert_eq!(f.inputs_at(600_000), Inputs::TESLA_API);
        assert_eq!(f.samples.iter().find(|x| x.t_ms == 600_000).unwrap().speed_kmh, Some(48.0));
        assert!(f.samples.windows(2).all(|w| w[0].t_ms < w[1].t_ms));
    }

    #[test]
    fn values_are_held_only_as_long_as_their_kind_allows() {
        let s: Vec<Sample> = (100..101).map(|t| Sample { t_ms: t * 1000, ..Sample::default() }).collect();
        // the API's sample 60 s before: its position (30 s) and speed (10 s) are too old, its level (120 s) is not
        let a = vec![api(40)];
        let f = fuse(&s, &a, &FuseParams::default());
        let x = f.samples.iter().find(|x| x.t_ms == 100_000).unwrap();
        assert_eq!((x.lat, x.speed_kmh, x.soc, x.inside_temp_c), (None, None, Some(69.0), Some(21.0)));
    }

    #[test]
    fn without_api_data_nothing_changes() {
        let s: Vec<Sample> = (0..10).map(smo).collect();
        let f = fuse(&s, &[], &FuseParams::default());
        assert_eq!(f.samples, s);
        assert!(f.marks.is_empty());
    }

    #[test]
    fn the_same_second_in_any_window() {
        let s: Vec<Sample> = (0..300).filter(|t| !(100..200).contains(t)).map(smo).collect();
        let a: Vec<Sample> = (0..30).map(|i| api(i * 10 + 3)).collect();
        let p = FuseParams::default();
        let whole = fuse(&s, &a, &p);
        // a window from 150 s to 250 s, with the API data the margin asks for around it
        let (lo, hi) = (150_000, 250_000);
        let m = p.margin_ms();
        let sw: Vec<Sample> = s.iter().filter(|x| x.t_ms >= lo && x.t_ms < hi).cloned().collect();
        let aw: Vec<Sample> = a.iter().filter(|x| x.t_ms + m >= lo && x.t_ms < hi + m).cloned().collect();
        let part = fuse(&sw, &aw, &p);
        let inside = |f: &Fused| -> Vec<(Sample, Inputs)> {
            f.samples
                .iter()
                .filter(|x| x.t_ms >= lo + 10_000 && x.t_ms < hi - 10_000)
                .map(|x| (x.clone(), f.inputs_at(x.t_ms)))
                .collect()
        };
        assert_eq!(inside(&whole), inside(&part));
    }
}
