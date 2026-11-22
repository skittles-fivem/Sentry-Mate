//! One log's decoded values → [`model::Sample`] once a second.
//!
//! Each value of a sample is the last value of its signal up to the end of that second; a signal's value is held for
//! [`SampleParams::hold_ms`] after it was sent (most are sent at 1 to 100 Hz; a missed frame or two doesn't blank the
//! value, a signal that stopped does). Where several signals can give a value, the best one with a value wins, and a
//! value the car marks "not available" clears what was held. A second with no frames gives no sample.
//!
//! The second the log's first frame falls in gives no sample either: it is cut (the log started inside it, so a
//! signal sent once a second may not have come yet, and "the first signal with a value decides" could pick a worse
//! one), and the previous log already gave that second, with the whole context of its own minute.

use crate::signals::{CHARGE_STATUS, Field, KEEP_CLIMATE, Kind, Table, charger_of, gear_of, table};
use crate::vin::VinCollector;
use model::{ChargeKind, Sample, SampleExtra, SocBasis, VinInfo};

/// How long values are held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleParams {
    /// A value is used this long after its signal was last sent (ms).
    pub hold_ms: u64,
    /// The same for a GPS position: a fix older than this isn't where the car is now.
    pub gps_hold_ms: u64,
    /// The same for the car's configuration (paint, wheels ...: 0x7FF), which is sent rarely and doesn't change.
    pub config_hold_ms: u64,
}

impl Default for SampleParams {
    fn default() -> Self {
        SampleParams { hold_ms: 5_000, gps_hold_ms: 2_000, config_hold_ms: 600_000 }
    }
}

/// A charge is "charging" by its power alone (the legacy set has no charge status) above this, kW.
const LEGACY_CHARGING_KW: f64 = 0.1;
/// An AC line current above this, while charging, says AC when no signal names the charger (A).
const AC_CURRENT_A: f64 = 0.5;

/// The pack's full energy and its buffer are held this long (ms): they change over weeks, and 0x352 sends them on
/// pages of their own, so one may be a while behind the other.
const PACK_HOLD_MS: u64 = 120_000;
/// The screen's % is not used while it is more than this many points off the % the pack's energy (or the rated range)
/// gives, and is used again once it is within [`SOC_AGREE`]. The screen rounds to a whole %, and Tesla's own figure
/// is close to the energy formula, so a point or so is normal.
const SOC_DISAGREE: f64 = 2.0;
const SOC_AGREE: f64 = 1.0;
/// A range this short (km) is too little to scale a % from.
const MIN_RANGE_KM: f64 = 10.0;

/// The battery % from the pack's energy, as the screen's figure is worked out: (energy remaining less the buffer)
/// over (full pack less the buffer). `None` when a value is missing or the three don't fit together.
pub(crate) fn soc_from_energy(remaining: Option<f64>, full: Option<f64>, buffer: Option<f64>) -> Option<f64> {
    let (rem, full, buf) = (remaining?, full?, buffer?);
    let usable = full - buf;
    if !(buf >= 0.0 && usable > 10.0 && (0.0..=full * 1.05).contains(&rem)) {
        return None;
    }
    Some(((rem - buf) / usable * 100.0).clamp(0.0, 100.0))
}

/// How the screen's % has held up against the pack's energy so far in the log.
#[derive(Debug, Clone, Copy, Default)]
struct SocCheck {
    /// The screen's % is off the pack's energy: the worked-out one is used until they agree again.
    distrust: bool,
    /// A checked % and the rated range (km) at that second: the range scales it while the energy is missing.
    anchor: Option<(f64, f64)>,
}

/// Builds the samples of one log (or of several logs of one car, fed in order). Its memory is fixed: one value per
/// wanted signal, plus the samples not taken yet.
#[derive(Debug)]
pub struct SampleBuilder {
    table: &'static Table,
    params: SampleParams,
    /// Per wanted signal: its value as kept, and when (µs).
    slots: Vec<Option<(f64, u64)>>,
    /// The second being filled (s since 1970), and whether it is the first one (cut, so not given).
    second: Option<u64>,
    first: bool,
    out: Vec<Sample>,
    vin: VinCollector,
    soc_check: SocCheck,
}

impl SampleBuilder {
    pub fn new(profile: dbc::Profile, params: SampleParams) -> SampleBuilder {
        let table = table(profile);
        SampleBuilder {
            table,
            params,
            slots: vec![None; table.cands.len()],
            second: None,
            first: true,
            out: Vec::new(),
            vin: VinCollector::new(),
            soc_check: SocCheck::default(),
        }
    }

    /// A raw frame: its time counts as a second with frames, and 0x405 / 0x508 are read for the VIN.
    pub fn frame(&mut self, frame: &logfmt::Frame) {
        self.tick(frame.time_us);
        self.vin.add_frame(frame);
    }

    /// One decoded value (frame time in µs).
    pub fn signal(&mut self, time_us: u64, signal: &dbc::Signal, value: f64) {
        self.value(time_us, &signal.series, value);
    }

    /// One decoded value by series name (`dbc::Signal::series`).
    pub fn value(&mut self, time_us: u64, series: &str, value: f64) {
        self.tick(time_us);
        if let Some(cands) = self.table.by_series.get(series) {
            for &i in cands {
                self.slots[i] = self.table.cands[i].read(value).map(|v| (v, time_us));
            }
        }
    }

    /// A frame and every value the decoder gets from it.
    pub fn feed(&mut self, decoder: &mut dbc::Decoder, frame: &logfmt::Frame) {
        self.frame(frame);
        for (sig, v) in decoder.decode(frame) {
            self.value(frame.time_us, &sig.series, v);
        }
    }

    /// The VIN, once 0x405 or 0x508 gave all of it.
    pub fn vin(&self) -> Option<&VinInfo> {
        self.vin.vin()
    }

    /// The samples of the seconds finished so far.
    pub fn take(&mut self) -> Vec<Sample> {
        std::mem::take(&mut self.out)
    }

    /// How many samples wait to be taken. (A sample is made at every change of second, so frames whose times go back
    /// and forth make one each: a caller reading untrusted input bounds this, as [`crate::read_log`] does.)
    pub fn pending(&self) -> usize {
        self.out.len()
    }

    /// Ends the log: its last second is given too.
    pub fn finish(mut self) -> (Vec<Sample>, Option<VinInfo>) {
        if let (Some(s), false) = (self.second, self.first) {
            let sample = self.sample(s);
            self.out.push(sample);
        }
        (self.out, self.vin.vin().cloned())
    }

    fn tick(&mut self, time_us: u64) {
        let sec = time_us / 1_000_000;
        match self.second {
            None => self.second = Some(sec),
            Some(s) if s != sec => {
                if !self.first {
                    let sample = self.sample(s);
                    self.out.push(sample);
                }
                self.first = false;
                self.second = Some(sec);
            }
            Some(_) => {}
        }
    }

    /// A slot's value if it was sent within `hold` before `end_us`.
    fn fresh(&self, i: usize, end_us: u64, hold_us: u64) -> Option<f64> {
        let (v, t) = self.slots[i]?;
        (end_us.saturating_sub(t) <= hold_us).then_some(v)
    }

    /// The best signal of a field with a fresh value: (its value, its kind, its candidate).
    fn best_cand(&self, field: Field, end_us: u64) -> Option<(f64, Kind, usize)> {
        let hold = self.params.hold_ms.saturating_mul(1000);
        let config_hold = self.params.config_hold_ms.saturating_mul(1000);
        self.table.fields[field as usize].iter().find_map(|&i| {
            let kind = self.table.cands[i].kind;
            let hold = if kind.is_config() { config_hold } else { hold };
            self.fresh(i, end_us, hold).map(|v| (v, kind, i))
        })
    }

    /// The best signal of a field with a fresh value: (its value, its kind).
    fn best(&self, field: Field, end_us: u64) -> Option<(f64, Kind)> {
        self.best_cand(field, end_us).map(|(v, k, _)| (v, k))
    }

    fn sample(&mut self, sec: u64) -> Sample {
        // (saturating: a frame time near the end of u64 must not overflow)
        let end_us = sec.saturating_add(1).saturating_mul(1_000_000);
        let range_km = self.best(Field::Range, end_us).map(|(v, _)| v);
        let (soc, soc_basis) = self.soc(end_us, range_km);
        let num = |f: Field| self.best(f, end_us).map(|(v, _)| v);

        let battery_voltage_v = num(Field::BattVolts);
        let battery_current_a = num(Field::BattAmps);
        let battery_power_kw = battery_voltage_v.zip(battery_current_a).map(|(v, i)| v * i / 1000.0);

        let charger_kw = num(Field::ChargerPower);
        let charging = match self.best(Field::Charging, end_us) {
            Some((v, Kind::ChargingWhen(on))) => Some(v.round() == on),
            _ => charger_kw.map(|kw| kw > LEGACY_CHARGING_KW),
        };
        // carCalc's charging power: the pack's power, turned round, while the car says it is charging; else the
        // legacy charger's own figure
        let charge_power_kw = match charging {
            Some(false) => None,
            Some(true) if battery_power_kw.is_some() => battery_power_kw.map(|p| -p),
            _ => charger_kw,
        };

        let dc = match (num(Field::DcVolts), num(Field::DcAmps), num(Field::DcStale)) {
            (Some(v), Some(a), stale) if v > 0.0 && a > 0.0 && stale != Some(1.0) => Some((v, a)),
            _ => None,
        };
        let (ac_v, ac_a) = (num(Field::AcVolts), num(Field::AcAmps));
        let (charger_voltage_v, charger_current_a) = match dc {
            Some((v, a)) => (Some(v), Some(a)),
            None => (ac_v, ac_a),
        };
        let charge_kind = match num(Field::ChargerType) {
            Some(code) => charger_of(code),
            None if charging == Some(true) && dc.is_some() => Some(ChargeKind::Dc),
            None if charging == Some(true) && ac_a.is_some_and(|a| a > AC_CURRENT_A) => Some(ChargeKind::Ac),
            None => None,
        };

        let battery_temp_c = match (num(Field::BrickTempMax), num(Field::BrickTempMin)) {
            (Some(a), Some(b)) => Some((a + b) / 2.0),
            _ => num(Field::BattTemp),
        };

        let (lat, lon, gps_accuracy_m) = self.position(end_us);

        let (tpms_bar, tires_mapped) = self.tyres(end_us);
        let extra = self.extra(end_us, &tpms_bar, tires_mapped);

        Sample {
            extra,
            t_ms: sec * 1000,
            lat,
            lon,
            gps_accuracy_m,
            elevation_m: num(Field::Elevation),
            speed_kmh: num(Field::Speed),
            gear: num(Field::Gear).and_then(gear_of),
            odometer_km: num(Field::Odometer),
            soc,
            soc_basis,
            soc_bms: num(Field::SocBms),
            range_km,
            ideal_range_km: num(Field::IdealRange),
            energy_remaining_kwh: num(Field::EnergyRemaining),
            battery_voltage_v,
            battery_current_a,
            battery_power_kw,
            battery_temp_c,
            outside_temp_c: num(Field::OutsideTemp),
            inside_temp_c: num(Field::InsideTemp),
            charging,
            charge_kind,
            charge_power_kw,
            charger_voltage_v,
            charger_current_a,
            total_charge_kwh: num(Field::TotalCharge),
            total_discharge_kwh: num(Field::TotalDischarge),
            lv_voltage_v: num(Field::LvVolts),
            tpms_bar,
        }
    }

    /// The less frequent values. Codes are kept in the slots; the few words are made here.
    fn extra(&self, end_us: u64, tpms_bar: &[Option<f64>; 4], tires_mapped: Option<bool>) -> SampleExtra {
        let num = |f: Field| self.best(f, end_us).map(|(v, _)| v);
        let flag = |f: Field| num(f).map(|v| v >= 0.5);
        let config = |f: Field| self.best_cand(f, end_us).map(|(v, _, i)| self.table.cands[i].label(v));
        let word = |f: Field, words: &[&str]| num(f).and_then(|c| words.get(c as usize)).map(|w| w.to_string());

        let hvac = num(Field::Hvac);
        let (rec_front, rec_rear) = (num(Field::TireRecFront), num(Field::TireRecRear));
        let preconditioning_battery = match (flag(Field::PrecondBattery), flag(Field::BatteryHeating)) {
            (None, None) => None,
            (a, b) => Some(a == Some(true) || b == Some(true)),
        };
        let phases = [Field::PhaseA, Field::PhaseB, Field::PhaseC].map(flag);
        let charger_phases =
            phases.iter().any(Option::is_some).then(|| phases.iter().filter(|p| **p == Some(true)).count() as f64);
        // the lowest of the limits the car is told (a 0 is no limit given)
        let limit = [(Field::PilotLimit, "pilot"), (Field::CableLimit, "cable"), (Field::CarLimit, "car")]
            .into_iter()
            .filter_map(|(f, src)| num(f).filter(|a| *a > 0.0).map(|a| (a, src)))
            .min_by(|a, b| a.0.total_cmp(&b.0));

        SampleExtra {
            paint: config(Field::Paint),
            wheels: config(Field::Wheels),
            spoiler: config(Field::Spoiler),
            roof: config(Field::Roof),
            right_hand_drive: flag(Field::RightHand),
            heading_deg: num(Field::Heading),
            sentry_mode: flag(Field::Sentry),
            service_mode: flag(Field::Service),
            display_on: flag(Field::Display),
            keep_climate: word(Field::KeepClimate, &KEEP_CLIMATE),
            door_fl: flag(Field::DoorFl),
            door_fr: flag(Field::DoorFr),
            door_rl: flag(Field::DoorRl),
            door_rr: flag(Field::DoorRr),
            trunk_open: flag(Field::Trunk),
            frunk_open: flag(Field::Frunk),
            user_present: flag(Field::UserPresent),
            // ON, PRECONDITION and the overheat protection (the fan runs) are climate on
            climate_on: hvac.map(|c| c >= 1.0),
            preconditioning_cabin: hvac.map(|c| c == 2.0),
            preconditioning_battery,
            usable_soc: num(Field::UsableSoc),
            plugged_in: flag(Field::Cable),
            charge_port_open: flag(Field::ChargeDoor),
            charge_status: word(Field::ChargeStatus, &CHARGE_STATUS),
            charge_limit_pct: num(Field::ChargeLimit),
            charge_current_request_a: num(Field::ChargeRequest),
            charge_current_limit_a: limit.map(|l| l.0),
            charge_limit_source: limit.map(|l| l.1.to_string()),
            charger_phases,
            time_to_full_min: num(Field::TimeToFull),
            tire_low: flag(Field::TpmsWarning)
                .or_else(|| tire_low(tpms_bar, [rec_front, rec_rear], tires_mapped == Some(true))),
            tire_recommended_front_bar: rec_front,
            tire_recommended_rear_bar: rec_rear,
            tires_mapped,
            energy_at_destination_kwh: num(Field::EnergyAtDest),
            navigating: flag(Field::Navigating),
            update_installing: flag(Field::Ota),
        }
    }

    /// The four tyre pressures (bar), and whether they are in wheel order (front left, front right, rear left, rear
    /// right). 0x219's four slots when it sends any: put in wheel order when its four locations (pages 0-3) name each
    /// wheel once, else kept in the car's slot order and not mapped (an incomplete, unknown or doubled location says
    /// nothing sure about any slot). Else 0x31F's, which names the wheels. Each group is read whole: a slot of one is
    /// never filled from the other, whose order may differ.
    fn tyres(&self, end_us: u64) -> ([Option<f64>; 4], Option<bool>) {
        let hold = self.params.hold_ms;
        let slots = SLOT_PRESSURES.map(|n| self.series(n, end_us, hold));
        if slots.iter().any(Option::is_some) {
            // the slots' wheels change only when the tyres are moved: held as the car's configuration is
            let locs = SLOT_LOCATIONS.map(|n| self.series(n, end_us, self.params.config_hold_ms));
            return match wheel_of_slots(locs) {
                Some(wheel) => {
                    let mut out = [None; 4];
                    for (slot, w) in wheel.into_iter().enumerate() {
                        out[w] = slots[slot];
                    }
                    (out, Some(true))
                }
                None => (slots, Some(false)),
            };
        }
        let named = NAMED_PRESSURES.map(|n| self.series(n, end_us, hold));
        let any = named.iter().any(Option::is_some);
        (named, any.then_some(true))
    }

    /// A slot's value by series name, if it was sent within `hold_ms` before `end_us`.
    fn series(&self, name: &str, end_us: u64, hold_ms: u64) -> Option<f64> {
        let &i = self.table.by_series.get(name)?.first()?;
        self.fresh(i, end_us, hold_ms.saturating_mul(1000))
    }

    /// The battery %, and what it rests on. The screen's figure while it follows the pack's energy (within
    /// [`SOC_DISAGREE`] points of it); else the % the energy gives. Where the energy is not there, the rated range
    /// scales the last checked % of the log, and the screen's figure is used while it follows that. A car whose set
    /// has no energy signals (the legacy S/X) keeps the screen's figure as it is.
    fn soc(&mut self, end_us: u64, range_km: Option<f64>) -> (Option<f64>, Option<SocBasis>) {
        let hold = self.params.hold_ms;
        let ui = self.best(Field::Soc, end_us).map(|(v, _)| v);
        // the nominal energy remaining only: the expected one is what a cold pack can give, not the screen's
        let energy = soc_from_energy(
            self.series("BMS_nominalEnergyRemaining", end_us, hold),
            self.series("BMS_nominalFullPackEnergy", end_us, PACK_HOLD_MS),
            self.series("BMS_energyBuffer", end_us, PACK_HOLD_MS),
        );
        let range = range_km.filter(|r| *r >= MIN_RANGE_KM);
        let check = &mut self.soc_check;
        let judge = |check: &mut SocCheck, ui: f64, other: f64| {
            let off = (ui - other).abs();
            if off > SOC_DISAGREE {
                check.distrust = true;
            } else if off <= SOC_AGREE {
                check.distrust = false;
            }
        };
        let (soc, basis) = match (ui, energy) {
            (Some(u), Some(e)) => {
                judge(check, u, e);
                if check.distrust { (e, SocBasis::BmsEnergy) } else { (u, SocBasis::Screen) }
            }
            (None, Some(e)) => (e, SocBasis::BmsEnergy),
            (ui, None) => {
                let by_range = check.anchor.zip(range).map(|((s0, r0), r)| (s0 * r / r0).clamp(0.0, 100.0));
                match (ui, by_range) {
                    (Some(u), Some(r)) => {
                        judge(check, u, r);
                        if check.distrust { (r, SocBasis::RatedRange) } else { (u, SocBasis::Screen) }
                    }
                    (Some(u), None) => return (Some(u), Some(SocBasis::Screen)),
                    (None, Some(r)) => return (Some(r), Some(SocBasis::RatedRange)),
                    (None, None) => return (None, None),
                }
            }
        };
        // a % checked against the energy is where the range scales from while the energy is missing
        if energy.is_some()
            && let Some(r) = range
        {
            check.anchor = Some((soc, r));
        }
        (Some(soc), Some(basis))
    }

    /// Latitude, longitude and accuracy of one message, the best with all three fresh. A position is kept only when
    /// it is plausible: on the globe, not 0,0, and with a known accuracy.
    fn position(&self, end_us: u64) -> (Option<f64>, Option<f64>, Option<f64>) {
        let hold = self.params.gps_hold_ms.saturating_mul(1000);
        for &[la, lo, ac] in &self.table.gps {
            let (lat, lon, acc) =
                (self.fresh(la, end_us, hold), self.fresh(lo, end_us, hold), self.fresh(ac, end_us, hold));
            match (lat, lon, acc) {
                (Some(lat), Some(lon), Some(acc)) if !(lat == 0.0 && lon == 0.0) => {
                    return (Some(lat), Some(lon), Some(acc));
                }
                (_, _, Some(acc)) => return (None, None, Some(acc)),
                _ => {}
            }
        }
        (None, None, None)
    }
}

/// 0x219's pressure slots and the wheel of each (override.dbc; the locations are the base layout's).
const SLOT_PRESSURES: [&str; 4] =
    ["VCSEC_TPMSPressure0", "VCSEC_TPMSPressure1", "VCSEC_TPMSPressure2", "VCSEC_TPMSPressure3"];
const SLOT_LOCATIONS: [&str; 4] =
    ["VCSEC_TPMSLocation0", "VCSEC_TPMSLocation1", "VCSEC_TPMSLocation2", "VCSEC_TPMSLocation3"];
/// 0x31F's pressures (chassis bus; the legacy set's too), by wheel: front left, front right, rear left, rear right.
const NAMED_PRESSURES: [&str; 4] = ["TPMSFLpressure31F", "TPMSFRpressure31F", "TPMSRLpressure31F", "TPMSRRpressure31F"];

/// The wheel (0 front left, 1 front right, 2 rear left, 3 rear right) of each of 0x219's four slots, when the four
/// locations are all known and name each wheel once; else `None`.
pub(crate) fn wheel_of_slots(locs: [Option<f64>; 4]) -> Option<[usize; 4]> {
    let mut wheel = [0usize; 4];
    let mut seen = [false; 4];
    for (slot, loc) in locs.into_iter().enumerate() {
        let l = loc?;
        if !(0.0..=3.0).contains(&l) || l.fract() != 0.0 {
            return None;
        }
        let w = l as usize;
        if seen[w] {
            return None;
        }
        seen[w] = true;
        wheel[slot] = w;
    }
    Some(wheel)
}

/// A tyre low, when the car's own warning isn't there: [`model::tyre_low`] against the recommended pressures
/// (front, rear), each tyre against its own axle's when the pressures are in wheel order, else against the lower of
/// the two (no false alarm while which wheel each slot is is not known).
fn tire_low(bar: &[Option<f64>; 4], recommended: [Option<f64>; 2], mapped: bool) -> Option<bool> {
    model::tyre_low(bar, recommended, mapped)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use model::Gear;

    const T0: u64 = 1_767_225_600_000_000; // 2026-01-01T00:00:00Z, µs

    fn at(s: f64) -> u64 {
        T0 + (s * 1e6) as u64
    }

    #[test]
    fn one_sample_a_second_with_the_last_values_and_the_first_cut_second_left_out() {
        let mut b = SampleBuilder::new(dbc::Profile::Tesla3Y, SampleParams::default());
        b.value(at(0.6), "DI_vehicleSpeed", 10.0);
        b.value(at(1.1), "DI_vehicleSpeed", 20.0);
        b.value(at(1.2), "DI_gear", 4.0);
        b.value(at(1.9), "DI_vehicleSpeed", 30.0);
        b.value(at(2.5), "DI_vehicleSpeed", 40.0);
        // nothing in second 3
        b.value(at(4.2), "DI_vehicleSpeed", 50.0);
        let (s, vin) = b.finish();
        assert!(vin.is_none());
        let t: Vec<u64> = s.iter().map(|x| (x.t_ms - T0 / 1000) / 1000).collect();
        assert_eq!(t, vec![1, 2, 4], "second 0 is cut, second 3 had no frames");
        assert_eq!(s[0].speed_kmh, Some(30.0));
        assert_eq!(s[0].gear, Some(Gear::D));
        assert_eq!(s[1].speed_kmh, Some(40.0));
        assert_eq!(s[2].speed_kmh, Some(50.0));
        assert_eq!(s[2].gear, Some(Gear::D), "held");
    }

    #[test]
    fn values_are_held_a_while_then_dropped_and_not_available_clears_them() {
        let mut b = SampleBuilder::new(dbc::Profile::Tesla3Y, SampleParams::default());
        b.value(at(0.5), "UI_SOC", 50.0);
        b.value(at(1.5), "UI_SOC", 71.0);
        b.value(at(1.5), "DI_gear", 1.0);
        for s in 2..10 {
            b.value(at(s as f64 + 0.5), "DI_vehicleSpeed", 0.0);
            if s == 3 {
                b.value(at(3.6), "DI_gear", 7.0); // DI_GEAR_SNA
            }
        }
        let (s, _) = b.finish();
        let soc: Vec<Option<f64>> = s.iter().map(|x| x.soc).collect();
        // sent at 1.5 s: held through the second that ends 5 s later (6.0 ≤ 1.5 + 5 is false at 7.0)
        assert_eq!(soc, vec![Some(71.0), Some(71.0), Some(71.0), Some(71.0), Some(71.0), None, None, None, None]);
        assert_eq!(s[1].gear, Some(Gear::P));
        assert_eq!(s[2].gear, None, "SNA clears the held gear");
    }

    #[test]
    fn the_first_signal_with_a_value_decides_charging() {
        let mut b = SampleBuilder::new(dbc::Profile::Tesla3Y, SampleParams::default());
        b.value(at(0.1), "PCS_hvChargeStatus", 2.0); // PCS_CHARGE_ENABLED
        b.value(at(1.1), "PCS_hvChargeStatus", 2.0);
        b.value(at(2.1), "BMS_uiChargeStatus", 4.0); // BMS_CHARGE_COMPLETE
        b.value(at(3.1), "BMS_uiChargeStatus", 3.0); // BMS_CHARGING
        b.value(at(3.1), "BattVoltage132", 400.0);
        b.value(at(3.1), "SmoothBattCurrent132", -25.0);
        let (s, _) = b.finish();
        assert_eq!(s[0].charging, Some(true), "only PCS says");
        assert_eq!(s[1].charging, Some(false), "BMS says complete: it decides");
        assert_eq!(s[2].charging, Some(true));
        assert_eq!(s[2].battery_power_kw, Some(-10.0), "into the pack");
        assert_eq!(s[2].charge_power_kw, Some(10.0));
    }

    #[test]
    fn positions_need_an_accuracy_and_are_not_zero_zero() {
        let mut b = SampleBuilder::new(dbc::Profile::Tesla3Y, SampleParams::default());
        b.value(at(0.2), "DI_gear", 1.0);
        let fix = |b: &mut SampleBuilder, s: f64, lat: f64, lon: f64, acc: Option<f64>| {
            b.value(at(s), "GPSLatitude04F", lat);
            b.value(at(s), "GPSLongitude04F", lon);
            if let Some(a) = acc {
                b.value(at(s), "GPSAccuracy04F", a);
            }
        };
        fix(&mut b, 1.2, 10.5, 20.25, Some(3.0));
        fix(&mut b, 2.2, 0.0, 0.0, Some(3.0));
        fix(&mut b, 3.2, 10.5, 20.25, Some(3.0));
        b.value(at(4.2), "DI_gear", 1.0);
        b.value(at(5.2), "DI_gear", 1.0);
        fix(&mut b, 6.2, 10.5, 20.25, Some(25.4)); // all bits set: no fix
        let (s, _) = b.finish();
        assert_eq!((s[0].lat, s[0].lon, s[0].gps_accuracy_m), (Some(10.5), Some(20.25), Some(3.0)));
        assert_eq!((s[1].lat, s[1].gps_accuracy_m), (None, Some(3.0)), "0,0 is no position");
        assert_eq!(s[3].lat, Some(10.5), "held for a missed frame");
        assert_eq!(s[4].lat, None, "a fix is held 2 s at most");
        assert_eq!((s[5].lat, s[5].gps_accuracy_m), (None, None));
    }

    #[test]
    fn charger_kind_from_the_charge_port_or_from_what_flows() {
        let mut b = SampleBuilder::new(dbc::Profile::Tesla3Y, SampleParams::default());
        b.value(at(0.5), "BMS_uiChargeStatus", 3.0);
        b.value(at(1.5), "BMS_uiChargeStatus", 3.0);
        b.value(at(1.5), "CP_evseOutputDcVoltage", 380.0);
        b.value(at(1.5), "CP_evseOutputDcCurrent", 300.0);
        b.value(at(1.5), "CP_evseOutputDcCurrentStale", 0.0);
        b.value(at(2.5), "CP_evseChargeType", 2.0); // AC_CHARGER_PRESENT wins over the physical guess
        let (s, _) = b.finish();
        assert_eq!(s[0].charge_kind, Some(ChargeKind::Dc));
        assert_eq!((s[0].charger_voltage_v, s[0].charger_current_a), (Some(380.0), Some(300.0)));
        assert_eq!(s[1].charge_kind, Some(ChargeKind::Ac));
    }

    #[test]
    fn a_frame_time_at_the_end_of_u64_does_not_overflow() {
        // (a crafted line "(18446744073709.551615) can0 ..." parses to u64::MAX µs)
        let mut b = SampleBuilder::new(
            dbc::Profile::Tesla3Y,
            SampleParams { hold_ms: u64::MAX, gps_hold_ms: u64::MAX, config_hold_ms: u64::MAX },
        );
        b.value(5_000_000, "UI_SOC", 50.0);
        b.value(u64::MAX, "UI_SOC", 60.0);
        b.value(u64::MAX - 1, "GPSAccuracy04F", 3.0);
        b.value(7_000_000, "UI_SOC", 70.0);
        let (s, _) = b.finish();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].soc, Some(60.0));
    }

    /// A Model Y's pack on made-up figures: 78 kWh full, 3 kWh buffer (75 usable), `pct` % of it left.
    fn pack(b: &mut SampleBuilder, t: f64, pct: f64, pages: bool) {
        b.value(at(t), "BMS_nominalEnergyRemaining", 3.0 + 0.75 * pct);
        if pages {
            b.value(at(t), "BMS_nominalFullPackEnergy", 78.0);
            b.value(at(t), "BMS_energyBuffer", 3.0);
        }
    }

    fn socs(s: &[Sample]) -> Vec<(Option<f64>, Option<SocBasis>)> {
        s.iter().map(|x| (x.soc.map(|v| (v * 10.0).round() / 10.0), x.soc_basis)).collect()
    }

    #[test]
    fn the_battery_percent_from_the_pack_energy() {
        assert_eq!(soc_from_energy(Some(56.25), Some(78.0), Some(3.0)), Some(71.0));
        assert_eq!(soc_from_energy(Some(2.0), Some(78.0), Some(3.0)), Some(0.0), "inside the buffer: 0 %");
        assert_eq!(soc_from_energy(Some(56.25), Some(78.0), None), None, "no buffer: not worked out");
        assert_eq!(soc_from_energy(Some(90.0), Some(78.0), Some(3.0)), None, "more left than the pack holds");
        assert_eq!(soc_from_energy(Some(5.0), Some(12.0), Some(3.0)), None, "too small a pack to be one");
    }

    #[test]
    fn the_screen_percent_is_kept_while_it_follows_the_pack_energy() {
        let mut b = SampleBuilder::new(dbc::Profile::Tesla3Y, SampleParams::default());
        for (k, (ui, pct)) in [(71.0, 71.0), (71.0, 71.0), (71.0, 71.4), (72.0, 71.6), (72.0, 72.3)].iter().enumerate()
        {
            b.value(at(k as f64 + 0.5), "UI_SOC", *ui);
            pack(&mut b, k as f64 + 0.6, *pct, true);
        }
        let (s, _) = b.finish();
        assert_eq!(
            socs(&s),
            vec![(Some(71.0), Some(SocBasis::Screen)); 2]
                .into_iter()
                .chain([(Some(72.0), Some(SocBasis::Screen)); 2])
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_screen_percent_stuck_while_a_charge_adds_energy_gives_way_to_the_pack_energy() {
        // UI_SOC stays at 71 % while the pack takes energy and the rated range rises (as a Model Y's logs showed)
        let mut b = SampleBuilder::new(dbc::Profile::Tesla3Y, SampleParams::default());
        for k in 0..8 {
            let t = k as f64 + 0.5;
            b.value(at(t), "UI_SOC", 71.0);
            b.value(at(t), "BMS_uiChargeStatus", 3.0); // BMS_CHARGING
            pack(&mut b, t, 71.0 + 0.5 * k as f64, true);
            b.value(at(t), "UI_Range", 228.0 + 1.6 * k as f64);
        }
        let (s, _) = b.finish();
        assert!(s.iter().all(|x| x.charging == Some(true)));
        let got = socs(&s);
        // the first seconds agree within 2 points: the screen's figure
        assert_eq!(&got[..4], &[(Some(71.0), Some(SocBasis::Screen)); 4]);
        // 73.5 % from the energy is 2.5 points off: the energy's figure from then on
        assert_eq!(
            &got[4..],
            &[
                (Some(73.5), Some(SocBasis::BmsEnergy)),
                (Some(74.0), Some(SocBasis::BmsEnergy)),
                (Some(74.5), Some(SocBasis::BmsEnergy))
            ]
        );
        // a charge over those seconds shows the % rising, not 71 % to 71 %
        assert!(s.last().unwrap().soc.unwrap() > s[0].soc.unwrap() + 3.0);
    }

    #[test]
    fn a_screen_percent_off_the_energy_while_parked_is_replaced_and_comes_back_once_they_agree() {
        let mut b = SampleBuilder::new(dbc::Profile::Tesla3Y, SampleParams::default());
        let seq = [(71.0, 77.0), (71.0, 77.0), (75.0, 76.6), (77.0, 77.0)];
        for (k, (ui, pct)) in seq.iter().enumerate() {
            let t = k as f64 + 0.5;
            b.value(at(t), "UI_SOC", *ui);
            pack(&mut b, t, *pct, true);
        }
        b.value(at(4.5), "UI_SOC", 77.0);
        let (s, _) = b.finish();
        assert_eq!(
            socs(&s),
            vec![
                (Some(77.0), Some(SocBasis::BmsEnergy)),
                (Some(76.6), Some(SocBasis::BmsEnergy)), // 1.6 off: not yet within a point of agreeing again
                (Some(77.0), Some(SocBasis::Screen)),
                (Some(77.0), Some(SocBasis::Screen)), // the energy is still held from 3.5 s
            ]
        );
    }

    #[test]
    fn without_the_energy_the_rated_range_checks_the_screen_percent() {
        let mut b = SampleBuilder::new(dbc::Profile::Tesla3Y, SampleParams::default());
        // one second with the energy: 72 % at 230 mi checked
        b.value(at(0.5), "UI_SOC", 72.0);
        b.value(at(0.5), "UI_Range", 230.0);
        pack(&mut b, 0.5, 72.0, true);
        b.value(at(1.5), "UI_SOC", 72.0);
        b.value(at(1.5), "UI_Range", 230.0);
        pack(&mut b, 1.5, 72.0, true);
        // then 0x352 stops: the range rises while the screen stays at 72 %
        for k in 2..14 {
            let t = k as f64 * 5.0 + 0.5;
            b.value(at(t), "UI_SOC", 72.0);
            b.value(at(t), "UI_Range", 230.0 + 2.0 * (k - 1) as f64);
        }
        let (s, _) = b.finish();
        let got = socs(&s);
        assert_eq!(got[0], (Some(72.0), Some(SocBasis::Screen)));
        // within two points of 72 × range / 230: the screen's
        let t = |k: u64| s.iter().position(|x| x.t_ms == (T0 / 1000) + k * 5000).unwrap();
        assert_eq!(got[t(2)], (Some(72.0), Some(SocBasis::Screen)));
        // 72 × 254 / 230 = 79.5: more than two points off
        let late = &s[t(13)];
        assert_eq!(late.soc_basis, Some(SocBasis::RatedRange));
        assert!((late.soc.unwrap() - 72.0 * 254.0 / 230.0).abs() < 1e-6, "{late:?}");
    }

    #[test]
    fn with_neither_the_screen_percent_stands_and_the_legacy_set_is_unchanged() {
        let mut b = SampleBuilder::new(dbc::Profile::Tesla3Y, SampleParams::default());
        b.value(at(0.5), "UI_SOC", 64.0);
        b.value(at(1.5), "UI_SOC", 64.0);
        b.value(at(1.5), "UI_Range", 200.0);
        let (s, _) = b.finish();
        assert_eq!(socs(&s), vec![(Some(64.0), Some(SocBasis::Screen))], "no energy, no checked range: the screen's");
        // the energy alone (UI_SOC late): the energy's figure
        let mut b = SampleBuilder::new(
            dbc::Profile::Tesla3Y,
            SampleParams { hold_ms: 500, gps_hold_ms: 500, ..SampleParams::default() },
        );
        b.value(at(0.2), "UI_SOC", 64.0);
        pack(&mut b, 1.8, 50.0, true);
        b.value(at(2.2), "DI_gear", 1.0);
        let (s, _) = b.finish();
        // (the gear's second after it: nothing fresh, no % held over)
        assert_eq!(socs(&s), vec![(Some(50.0), Some(SocBasis::BmsEnergy)), (None, None)]);
        let mut b = SampleBuilder::new(dbc::Profile::TeslaSxLegacy, SampleParams::default());
        b.value(at(0.5), "SOCUI302", 55.0);
        b.value(at(1.5), "SOCUI302", 55.0);
        let (s, _) = b.finish();
        assert_eq!(socs(&s), vec![(Some(55.0), Some(SocBasis::Screen))]);
    }

    #[test]
    fn unknown_series_still_count_as_frames() {
        let mut b = SampleBuilder::new(dbc::Profile::Tesla3Y, SampleParams::default());
        b.value(at(0.5), "SomethingElse", 1.0);
        b.value(at(1.5), "SomethingElse", 1.0);
        b.value(at(2.5), "SomethingElse", 1.0);
        let (s, _) = b.finish();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0], Sample { t_ms: T0 / 1000 + 1000, ..Sample::default() });
    }
}
