//! The live inputs over the same time, made one: SMO's samples (1 Hz, from the car's own buses), the Tesla API's
//! (every few seconds while streaming, every half minute to minute when polled), TeslaFi's (TeslaFi's own polls,
//! about once a minute, read from its API) and the local TeslaMate's. They are laid in the car's order
//! ([`fuse_ordered`], a [`SourceOrder`]; by default SMO, the Tesla API, TeslaFi, local TeslaMate, as [`fuse_all`]
//! lays them), the lowest first: each input over what is below it (inside its coverage, [`FuseParams::smo_gap_ms`]
//! for SMO, [`FuseParams::api_gap_ms`] for an API, its seconds, the inputs below filling the values it lacks). With
//! SMO on top, as below (with an API on top, the same with that API in SMO's place).
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

use model::{Input, Inputs, Sample, SourceOrder};
use serde::{Deserialize, Serialize};

/// What the Tesla API said about the car's state over a span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiState {
    Online,
    Asleep,
    Offline,
}

/// A span an API saw the car in one state. `data`: the API sent the car's values in it (vehicle data or the
/// stream), not only its state in the vehicle list. `inputs`: which API saw it (empty, as older servers wrote it:
/// the Tesla API).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApiSpan {
    pub start_ms: u64,
    pub end_ms: u64,
    pub state: ApiState,
    #[serde(default)]
    pub data: bool,
    #[serde(default, skip_serializing_if = "Inputs::is_empty")]
    pub inputs: Inputs,
}

impl ApiSpan {
    /// The input that saw it (the Tesla API when none is named).
    pub fn input(&self) -> Inputs {
        if self.inputs.is_empty() { Inputs::TESLA_API } else { self.inputs }
    }
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
    /// Tesla API samples this close are one stretch of its coverage, over TeslaFi's (it is polled about once a
    /// minute, as TeslaFi is: inside that, no TeslaFi second of its own between two of the Tesla API's).
    #[serde(default = "default_api_gap_ms")]
    pub api_gap_ms: u64,
}

fn default_api_gap_ms() -> u64 {
    90_000
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
            api_gap_ms: default_api_gap_ms(),
        }
    }
}

impl FuseParams {
    /// How far around a window API samples must be read for every second inside it to fuse as in a longer one.
    pub fn margin_ms(&self) -> u64 {
        self.fast_ms.max(self.position_ms).max(self.battery_ms).max(self.odometer_ms).max(self.slow_ms)
    }

    /// How far around a window TeslaFi's samples must be read (they fill the Tesla API's seconds, which fill SMO's).
    pub fn lower_margin_ms(&self) -> u64 {
        self.margin_ms().saturating_mul(2).saturating_add(self.api_gap_ms)
    }

    /// How far around a window an input's samples must be read for [`fuse_ordered`], by its place in the car's
    /// order (`rank` 0: the top one). The top input is read a margin too unless it is SMO (an API's coverage is
    /// wide), the second [`Self::margin_ms`], the ones below it [`Self::lower_margin_ms`].
    pub fn read_margin_ms(&self, input: Input, rank: usize) -> u64 {
        match (rank, input) {
            (0, Input::Smo) => 0,
            (0 | 1, Input::Smo | Input::TeslaApi | Input::TeslafiApi | Input::TeslamateLocal) => self.margin_ms(),
            (_, Input::Smo | Input::TeslaApi | Input::TeslafiApi | Input::TeslamateLocal) => self.lower_margin_ms(),
        }
    }
}

/// The fused samples, by time, one per second, and the seconds that are not SMO's alone (by time).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Fused {
    pub samples: Vec<Sample>,
    /// `(t_ms, inputs)` for every second whose values are not all SMO's: `TESLA_API` for an API second, `BOTH` for
    /// an SMO second with values from the API (and with `TESLAFI_API` where TeslaFi gave values). A second not here
    /// is SMO's.
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
pub(crate) fn seconds(v: &[Sample]) -> Vec<&Sample> {
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

/// The times of the lower samples (or an import's, [`crate::gapfill`]) that have one field, for the
/// nearest-within-hold look-ups.
pub(crate) struct Field<'a> {
    at: Vec<(u64, &'a Sample, Inputs)>,
}

impl<'a> Field<'a> {
    pub(crate) fn new(below: &[&'a (Sample, Inputs)], has: impl Fn(&Sample) -> bool) -> Field<'a> {
        Field { at: below.iter().filter(|s| has(&s.0)).map(|s| (s.0.t_ms, &s.0, s.1)).collect() }
    }

    /// The same over samples of no particular input (an import's).
    pub(crate) fn of(samples: &[&'a Sample], has: impl Fn(&Sample) -> bool) -> Field<'a> {
        Field { at: samples.iter().filter(|s| has(s)).map(|s| (s.t_ms, *s, Inputs::NONE)).collect() }
    }

    /// The lower sample with this field nearest `t` within `hold` (the earlier one on a tie), and its inputs.
    pub(crate) fn near(&self, t: u64, hold: u64) -> Option<(&'a Sample, Inputs)> {
        let i = self.at.partition_point(|(x, _, _)| *x < t);
        let after = self.at.get(i).filter(|(x, _, _)| x - t <= hold);
        let before = i.checked_sub(1).map(|j| &self.at[j]).filter(|(x, _, _)| t - x <= hold);
        let pick = match (before, after) {
            (Some(b), Some(a)) => Some(if a.0 - t < t - b.0 { a } else { b }),
            (Some(b), None) => Some(b),
            (None, Some(a)) => Some(a),
            (None, None) => None,
        };
        pick.map(|(_, s, i)| (*s, *i))
    }
}

/// One value of a sample that the API (or an import, [`crate::gapfill`]) can fill in: the sample field's name, its
/// hold time, whether a sample has it, and its copy.
pub(crate) struct Slot {
    pub(crate) name: &'static str,
    pub(crate) hold: fn(&FuseParams) -> u64,
    pub(crate) has: fn(&Sample) -> bool,
    pub(crate) copy: fn(&mut Sample, &Sample),
}

macro_rules! slots {
    ($($hold:ident: $($f:ident),+);+) => {
        &[$($(Slot {
            name: stringify!($f),
            hold: |p| p.$hold,
            has: |x| x.$f.is_some(),
            copy: |to, from| to.$f = from.$f,
        },)+)+
        // the four tyre pressures are one value: an input's four may be in its own slot order (SMO's 0x219 while
        // which wheel each slot is is not known) and another's in wheel order (the APIs'), so a sample takes all
        // four from one input or none, never a slot from each
        Slot {
            name: "tpms_bar",
            hold: |p| p.slow_ms,
            has: |x| x.tpms_bar.iter().any(Option::is_some),
            copy: |to, from| {
                to.tpms_bar = from.tpms_bar;
                to.extra.tires_mapped = from.extra.tires_mapped;
            },
        },
        ]
    };
}

pub(crate) const SLOTS: &[Slot] = slots!(
    fast_ms: speed_kmh, gear, battery_power_kw, battery_voltage_v, battery_current_a, charging, charge_kind,
        charge_power_kw, charger_voltage_v, charger_current_a;
    position_ms: lat, lon, gps_accuracy_m, elevation_m;
    battery_ms: soc, soc_bms, range_km, ideal_range_km, energy_remaining_kwh;
    odometer_ms: odometer_km;
    slow_ms: outside_temp_c, inside_temp_c, battery_temp_c
);

/// Fuses SMO's samples and the Tesla API's (each any order), SMO first. See the module's notes.
pub fn fuse(smo: &[Sample], api: &[Sample], p: &FuseParams) -> Fused {
    fuse_all(smo, &[(api, Inputs::TESLA_API)], p)
}

/// Fuses SMO's samples and those of the APIs (`lower`, each any order, highest first: the Tesla API's, then
/// TeslaFi's, then the local TeslaMate's) in the default order (SMO first), each lower input filling the one above
/// it value by value. See the module's notes; [`fuse_ordered`] takes a car's own order.
pub fn fuse_all(smo: &[Sample], lower: &[(&[Sample], Inputs)], p: &FuseParams) -> Fused {
    let mut layers: Vec<(&[Sample], Inputs)> = Vec::with_capacity(lower.len() + 1);
    layers.push((smo, Inputs::SMO));
    layers.extend(lower.iter().copied());
    layered(&layers, p)
}

/// Fuses the live inputs' samples in a car's own order: `by_input` holds each input's samples (any order; an input
/// not there has none), and they are laid highest first as `order` says, each lower input filling the one above it
/// value by value. Inside an input's coverage its seconds are the seconds (SMO's coverage is
/// [`FuseParams::smo_gap_ms`], an API's [`FuseParams::api_gap_ms`]); outside it the next input's are.
pub fn fuse_ordered(by_input: &[(&[Sample], Input)], order: &SourceOrder, p: &FuseParams) -> Fused {
    let layers: Vec<(&[Sample], Inputs)> = order
        .iter()
        .map(|i| (by_input.iter().find(|(_, x)| *x == i).map_or(&[][..], |(s, _)| *s), Inputs::from(i)))
        .collect();
    layered(&layers, p)
}

/// How long a gap between two of an input's samples still counts as its coverage.
fn gap_of(inputs: Inputs, p: &FuseParams) -> u64 {
    if inputs == Inputs::SMO { p.smo_gap_ms } else { p.api_gap_ms }
}

/// The inputs' samples (`layers`, highest first), each filling the one above it: the bottom one first, each higher
/// one laid over what is below it.
fn layered(layers: &[(&[Sample], Inputs)], p: &FuseParams) -> Fused {
    let mut below: Vec<(Sample, Inputs)> = Vec::new();
    for (samples, inputs) in layers.iter().rev() {
        let top = seconds(samples);
        below = if below.is_empty() {
            top.into_iter().map(|s| (s.clone(), *inputs)).collect()
        } else {
            lay(&top, *inputs, &below, gap_of(*inputs, p), p)
        };
    }
    let marks = below.iter().filter(|(_, i)| *i != Inputs::SMO).map(|(s, i)| (s.t_ms, *i)).collect();
    Fused { samples: below.into_iter().map(|(s, _)| s).collect(), marks }
}

/// `top`'s seconds (one per second, sorted) with the values they lack from `below` (sorted, one per second), and
/// `below`'s seconds outside `top`'s coverage (seconds between two of `top`'s at most `gap` apart), by time.
fn lay(top: &[&Sample], top_in: Inputs, below: &[(Sample, Inputs)], gap: u64, p: &FuseParams) -> Vec<(Sample, Inputs)> {
    let top_t: Vec<u64> = top.iter().map(|s| s.t_ms).collect();
    // whether a time falls in the top input's coverage: a sample that second, or two around it close enough
    let covered = |t: u64| {
        let i = top_t.partition_point(|&x| x < t);
        if top_t.get(i) == Some(&t) {
            return true;
        }
        match (i.checked_sub(1).map(|j| top_t[j]), top_t.get(i)) {
            (Some(a), Some(&b)) => b - a <= gap,
            _ => false,
        }
    };
    // only the lower samples near the top's seconds are looked at for filling
    let m = p.margin_ms();
    let near_top: Vec<&(Sample, Inputs)> = below
        .iter()
        .filter(|(a, _)| {
            let i = top_t.partition_point(|&x| x < a.t_ms);
            top_t.get(i).is_some_and(|&x| x - a.t_ms <= m) || i.checked_sub(1).is_some_and(|j| a.t_ms - top_t[j] <= m)
        })
        .collect();
    let mut out: Vec<(Sample, Inputs)> = Vec::with_capacity(top.len() + below.len());
    if near_top.is_empty() {
        out.extend(top.iter().map(|s| ((*s).clone(), top_in)));
    } else {
        let looks: Vec<Field<'_>> = SLOTS.iter().map(|slot| Field::new(&near_top, slot.has)).collect();
        for s in top {
            let mut x = (*s).clone();
            let filled = fill_one(&mut x, p, &looks);
            out.push((x, top_in.union(filled)));
        }
    }
    out.extend(below.iter().filter(|(a, _)| !covered(a.t_ms)).cloned());
    out.sort_by_key(|(s, _)| s.t_ms);
    out
}

/// Fills what `s` lacks; returns the inputs of the values taken.
fn fill_one(s: &mut Sample, p: &FuseParams, looks: &[Field<'_>]) -> Inputs {
    let mut from = Inputs::NONE;
    for (slot, look) in SLOTS.iter().zip(looks) {
        if !(slot.has)(s)
            && let Some((a, i)) = look.near(s.t_ms, (slot.hold)(p))
        {
            (slot.copy)(s, a);
            from = from.union(i);
        }
    }
    from
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
    fn the_four_tyres_come_from_one_input() {
        // SMO's 0x219 slots (order not known) with one slot missing: never filled from the API's named wheels
        let s: Vec<Sample> = (0..30)
            .map(|t| {
                let mut x = smo(t);
                x.tpms_bar = [Some(2.5), None, Some(2.6), Some(2.7)];
                x.extra.tires_mapped = Some(false);
                x
            })
            .collect();
        let a: Vec<Sample> = (0..3)
            .map(|i| {
                let mut x = api(i * 10 + 5);
                x.tpms_bar = [Some(2.9), Some(2.91), Some(2.92), Some(2.93)];
                x.extra.tires_mapped = Some(true);
                x
            })
            .collect();
        let f = fuse(&s, &a, &FuseParams::default());
        assert!(f.samples.iter().all(|x| x.tpms_bar == [Some(2.5), None, Some(2.6), Some(2.7)]));
        assert!(f.samples.iter().all(|x| x.extra.tires_mapped == Some(false)));
        // SMO with no tyres at all: the API's four, with their order
        let s: Vec<Sample> = (0..30).map(smo).collect();
        let f = fuse(&s, &a, &FuseParams::default());
        assert!(f.samples.iter().all(|x| x.tpms_bar == [Some(2.9), Some(2.91), Some(2.92), Some(2.93)]));
        assert!(f.samples.iter().all(|x| x.extra.tires_mapped == Some(true)));
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

    fn fi(t: u64) -> Sample {
        Sample {
            t_ms: t * 1000,
            speed_kmh: Some(47.0),
            soc: Some(68.0),
            lat: Some(1.2),
            lon: Some(2.2),
            inside_temp_c: Some(19.0),
            outside_temp_c: Some(9.0),
            ..Sample::default()
        }
    }

    #[test]
    fn smo_then_the_tesla_api_then_teslafi() {
        // SMO for the first minute, the Tesla API every 30 s for the first five, TeslaFi every 60 s for ten
        let s: Vec<Sample> = (0..60).map(smo).collect();
        let a: Vec<Sample> = (0..10).map(|i| api(i * 30)).collect();
        let t: Vec<Sample> = (0..10).map(|i| fi(i * 60 + 7)).collect();
        let p = FuseParams::default();
        let f = fuse_all(&s, &[(&a, Inputs::TESLA_API), (&t, Inputs::TESLAFI_API)], &p);
        let at = |sec: u64| f.samples.iter().find(|x| x.t_ms == sec * 1000).cloned();
        // inside SMO's coverage: SMO's values, the cabin from the Tesla API, the outside from TeslaFi
        let x = at(30).unwrap();
        assert_eq!((x.speed_kmh, x.inside_temp_c, x.outside_temp_c), (Some(50.0), Some(21.0), Some(9.0)));
        let all = Inputs::SMO.union(Inputs::TESLA_API).union(Inputs::TESLAFI_API);
        assert_eq!(f.inputs_at(30_000), all);
        // inside the Tesla API's coverage (to 270 s): its seconds, TeslaFi's only to fill
        let x = at(120).unwrap();
        assert_eq!((x.speed_kmh, x.outside_temp_c), (Some(48.0), Some(9.0)));
        assert_eq!(f.inputs_at(120_000), Inputs::TESLA_API.union(Inputs::TESLAFI_API));
        assert!(at(127).is_none(), "no TeslaFi second between two of the Tesla API's");
        // after it: TeslaFi's own seconds
        let x = at(367).unwrap();
        assert_eq!((x.speed_kmh, x.soc), (Some(47.0), Some(68.0)));
        assert_eq!(f.inputs_at(367_000), Inputs::TESLAFI_API);
        assert!(f.samples.windows(2).all(|w| w[0].t_ms < w[1].t_ms));
        // the two-input call is the same as before
        assert_eq!(fuse(&s, &a, &p), fuse_all(&s, &[(&a, Inputs::TESLA_API)], &p));
    }

    #[test]
    fn a_car_that_puts_the_tesla_api_above_smo() {
        // SMO each second for two minutes (with the odometer, which the API does not send), the Tesla API every 10 s
        // for the first minute (one of its samples without the level)
        let s: Vec<Sample> = (0..120).map(|t| Sample { odometer_km: Some(1000.0), ..smo(t) }).collect();
        let a: Vec<Sample> = (0..6)
            .map(|i| {
                let mut x = api(i * 10 + 5);
                if i == 2 {
                    x.soc = None;
                }
                x
            })
            .collect();
        let order = SourceOrder::new(&[Input::TeslaApi, Input::Smo, Input::TeslafiApi, Input::TeslamateLocal]).unwrap();
        let p = FuseParams::default();
        let f = fuse_ordered(&[(&s, Input::Smo), (&a, Input::TeslaApi)], &order, &p);
        let at = |sec: u64| f.samples.iter().find(|x| x.t_ms == sec * 1000).cloned();
        // inside the Tesla API's coverage its seconds are the seconds, its values win value by value
        let x = at(15).unwrap();
        assert_eq!((x.speed_kmh, x.soc, x.lat, x.inside_temp_c), (Some(48.0), Some(69.0), Some(1.1), Some(21.0)));
        assert_eq!(x.odometer_km, Some(1000.0), "SMO fills what the API lacks");
        assert_eq!(at(25).unwrap().soc, Some(70.0), "the level the API did not send that second: SMO's");
        assert_eq!(f.inputs_at(15_000), Inputs::BOTH);
        assert!(at(16).is_none() && at(30).is_none(), "no SMO second of its own between two of the API's");
        // after it: SMO's own seconds
        let x = at(100).unwrap();
        assert_eq!((x.speed_kmh, x.soc), (Some(50.0), Some(70.0)));
        assert_eq!(f.inputs_at(100_000), Inputs::SMO);
        assert!(f.samples.windows(2).all(|w| w[0].t_ms < w[1].t_ms));
        // the default order is fuse_all's
        let d = fuse_ordered(&[(&a, Input::TeslaApi), (&s, Input::Smo)], &SourceOrder::default(), &p);
        assert_eq!(d, fuse_all(&s, &[(&a, Inputs::TESLA_API)], &p));
        assert_eq!(d.samples.iter().find(|x| x.t_ms == 15_000).unwrap().speed_kmh, Some(50.0), "SMO first by default");
    }

    #[test]
    fn local_teslamate_fills_under_teslafi() {
        // TeslaFi every 60 s for the first five minutes, the local TeslaMate every 10 s for ten, its own cabin value
        let t: Vec<Sample> = (0..5).map(|i| fi(i * 60 + 7)).collect();
        let m: Vec<Sample> = (0..60)
            .map(|i| Sample {
                t_ms: (i * 10 + 3) * 1000,
                soc: Some(66.0),
                inside_temp_c: Some(18.0),
                ..Sample::default()
            })
            .collect();
        let p = FuseParams::default();
        let f =
            fuse_all(&[], &[(&[], Inputs::TESLA_API), (&t, Inputs::TESLAFI_API), (&m, Inputs::TESLAMATE_LOCAL)], &p);
        let at = |sec: u64| f.samples.iter().find(|x| x.t_ms == sec * 1000).cloned();
        // inside TeslaFi's coverage: TeslaFi wins, no TeslaMate second of its own
        assert_eq!(at(67).unwrap().soc, Some(68.0));
        assert!(at(73).is_none());
        // after it: the local TeslaMate's own seconds
        assert_eq!(at(503).unwrap().soc, Some(66.0));
        assert_eq!(f.inputs_at(503_000), Inputs::TESLAMATE_LOCAL);
    }

    #[test]
    fn teslafi_alone_or_under_smo_alone() {
        let s: Vec<Sample> = (0..60).map(smo).collect();
        let t: Vec<Sample> = (0..5).map(|i| fi(i * 60 + 7)).collect();
        let f = fuse_all(&s, &[(&[], Inputs::TESLA_API), (&t, Inputs::TESLAFI_API)], &FuseParams::default());
        assert_eq!(f.inputs_at(10_000), Inputs::SMO.union(Inputs::TESLAFI_API));
        assert_eq!(f.inputs_at(187_000), Inputs::TESLAFI_API);
        let only = fuse_all(&[], &[(&[], Inputs::TESLA_API), (&t, Inputs::TESLAFI_API)], &FuseParams::default());
        assert_eq!(only.samples.len(), 5);
        assert!(only.marks.iter().all(|m| m.1 == Inputs::TESLAFI_API));
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
