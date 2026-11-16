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

use crate::signals::{Field, Kind, Table, charger_of, gear_of, table};
use crate::vin::VinCollector;
use model::{ChargeKind, Sample, VinInfo};

/// How long values are held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleParams {
    /// A value is used this long after its signal was last sent (ms).
    pub hold_ms: u64,
    /// The same for a GPS position: a fix older than this isn't where the car is now.
    pub gps_hold_ms: u64,
}

impl Default for SampleParams {
    fn default() -> Self {
        SampleParams { hold_ms: 5_000, gps_hold_ms: 2_000 }
    }
}

/// A charge is "charging" by its power alone (the legacy set has no charge status) above this, kW.
const LEGACY_CHARGING_KW: f64 = 0.1;
/// An AC line current above this, while charging, says AC when no signal names the charger (A).
const AC_CURRENT_A: f64 = 0.5;

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
        if let Some(&i) = self.table.by_series.get(series) {
            self.slots[i] = self.table.cands[i].read(value).map(|v| (v, time_us));
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

    /// The best signal of a field with a fresh value: (its value, its kind).
    fn best(&self, field: Field, end_us: u64) -> Option<(f64, Kind)> {
        let hold = self.params.hold_ms * 1000;
        self.table.fields[field as usize]
            .iter()
            .find_map(|&i| self.fresh(i, end_us, hold).map(|v| (v, self.table.cands[i].kind)))
    }

    fn sample(&self, sec: u64) -> Sample {
        let end_us = (sec + 1) * 1_000_000;
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

        Sample {
            t_ms: sec * 1000,
            lat,
            lon,
            gps_accuracy_m,
            elevation_m: num(Field::Elevation),
            speed_kmh: num(Field::Speed),
            gear: num(Field::Gear).and_then(gear_of),
            odometer_km: num(Field::Odometer),
            soc: num(Field::Soc),
            soc_bms: num(Field::SocBms),
            range_km: num(Field::Range),
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
            tpms_bar: [num(Field::Tire0), num(Field::Tire1), num(Field::Tire2), num(Field::Tire3)],
        }
    }

    /// Latitude, longitude and accuracy of one message, the best with all three fresh. A position is kept only when
    /// it is plausible: on the globe, not 0,0, and with a known accuracy.
    fn position(&self, end_us: u64) -> (Option<f64>, Option<f64>, Option<f64>) {
        let hold = self.params.gps_hold_ms * 1000;
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
