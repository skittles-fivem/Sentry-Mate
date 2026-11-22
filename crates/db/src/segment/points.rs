//! The columns of the two kinds of points kept in segments. Tags never change meaning; a new field gets a new tag
//! (and the format version goes up, so an older program says so instead of dropping it).

use super::columns::{Col, Derived, Point};
use model::{ChargeKind, ChargePoint, Gear, Sample};

macro_rules! f64_col {
    ($tag:expr, $name:expr, $t:ty, $($field:tt)+) => {
        Col::F64 {
            tag: $tag,
            name: $name,
            get: |p: &$t| p.$($field)+,
            set: |p: &mut $t, v| p.$($field)+ = Some(v),
            derived: None,
        }
    };
}

/// Battery power as the timeline works it out: `battery_voltage_v * battery_current_a / 1000.0`, in that order. A
/// day where every power is exactly that stores no power at all.
fn power_from_v_i(s: &Sample) -> Option<f64> {
    Some(s.battery_voltage_v? * s.battery_current_a? / 1000.0)
}

fn gear_code(g: Gear) -> u8 {
    match g {
        Gear::P => 1,
        Gear::R => 2,
        Gear::N => 3,
        Gear::D => 4,
    }
}

fn set_gear(s: &mut Sample, c: u8) -> bool {
    s.gear = Some(match c {
        1 => Gear::P,
        2 => Gear::R,
        3 => Gear::N,
        4 => Gear::D,
        _ => return false,
    });
    true
}

fn kind_code(k: ChargeKind) -> u8 {
    match k {
        ChargeKind::Ac => 1,
        ChargeKind::Dc => 2,
    }
}

fn set_kind(s: &mut Sample, c: u8) -> bool {
    s.charge_kind = Some(match c {
        1 => ChargeKind::Ac,
        2 => ChargeKind::Dc,
        _ => return false,
    });
    true
}

fn set_charging(s: &mut Sample, c: u8) -> bool {
    s.charging = Some(match c {
        0 => false,
        1 => true,
        _ => return false,
    });
    true
}

static SAMPLE_COLUMNS: [Col<Sample>; 30] = [
    f64_col!(1, "lat", Sample, lat),
    f64_col!(2, "lon", Sample, lon),
    f64_col!(3, "gps_accuracy_m", Sample, gps_accuracy_m),
    f64_col!(4, "elevation_m", Sample, elevation_m),
    f64_col!(5, "speed_kmh", Sample, speed_kmh),
    Col::U8 { tag: 6, name: "gear", get: |s: &Sample| s.gear.map(gear_code), set: set_gear },
    f64_col!(7, "odometer_km", Sample, odometer_km),
    f64_col!(8, "soc", Sample, soc),
    f64_col!(9, "soc_bms", Sample, soc_bms),
    f64_col!(10, "range_km", Sample, range_km),
    f64_col!(11, "ideal_range_km", Sample, ideal_range_km),
    f64_col!(12, "energy_remaining_kwh", Sample, energy_remaining_kwh),
    f64_col!(13, "battery_voltage_v", Sample, battery_voltage_v),
    f64_col!(14, "battery_current_a", Sample, battery_current_a),
    Col::F64 {
        tag: 15,
        name: "battery_power_kw",
        get: |s: &Sample| s.battery_power_kw,
        set: |s: &mut Sample, v| s.battery_power_kw = Some(v),
        derived: Some(Derived { derive: power_from_v_i, from: &[13, 14] }),
    },
    f64_col!(16, "battery_temp_c", Sample, battery_temp_c),
    f64_col!(17, "outside_temp_c", Sample, outside_temp_c),
    f64_col!(18, "inside_temp_c", Sample, inside_temp_c),
    Col::U8 { tag: 19, name: "charging", get: |s: &Sample| s.charging.map(u8::from), set: set_charging },
    Col::U8 { tag: 20, name: "charge_kind", get: |s: &Sample| s.charge_kind.map(kind_code), set: set_kind },
    f64_col!(21, "charge_power_kw", Sample, charge_power_kw),
    f64_col!(22, "charger_voltage_v", Sample, charger_voltage_v),
    f64_col!(23, "charger_current_a", Sample, charger_current_a),
    f64_col!(24, "total_charge_kwh", Sample, total_charge_kwh),
    f64_col!(25, "total_discharge_kwh", Sample, total_discharge_kwh),
    f64_col!(26, "lv_voltage_v", Sample, lv_voltage_v),
    f64_col!(27, "tpms_fl_bar", Sample, tpms_bar[0]),
    f64_col!(28, "tpms_fr_bar", Sample, tpms_bar[1]),
    f64_col!(29, "tpms_rl_bar", Sample, tpms_bar[2]),
    f64_col!(30, "tpms_rr_bar", Sample, tpms_bar[3]),
];

impl Point for Sample {
    const SERIES: &'static str = "samples";
    fn t_ms(&self) -> u64 {
        self.t_ms
    }
    fn set_t_ms(&mut self, t: u64) {
        self.t_ms = t;
    }
    fn columns() -> &'static [Col<Self>] {
        &SAMPLE_COLUMNS
    }
}

static CHARGE_COLUMNS: [Col<ChargePoint>; 6] = [
    f64_col!(1, "power_kw", ChargePoint, power_kw),
    f64_col!(2, "voltage_v", ChargePoint, voltage_v),
    f64_col!(3, "current_a", ChargePoint, current_a),
    f64_col!(4, "soc", ChargePoint, soc),
    f64_col!(5, "battery_temp_c", ChargePoint, battery_temp_c),
    f64_col!(6, "range_km", ChargePoint, range_km),
];

impl Point for ChargePoint {
    const SERIES: &'static str = "charge";
    fn t_ms(&self) -> u64 {
        self.t_ms
    }
    fn set_t_ms(&mut self, t: u64) {
        self.t_ms = t;
    }
    fn columns() -> &'static [Col<Self>] {
        &CHARGE_COLUMNS
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::super::columns::{decode, decode_fields, encode};
    use super::*;

    #[test]
    fn every_sample_field_round_trips() {
        let full = |t: u64| Sample {
            extra: Default::default(),
            t_ms: t,
            lat: Some(10.123_456),
            lon: Some(-20.5),
            gps_accuracy_m: Some(3.2),
            elevation_m: Some(250.0),
            speed_kmh: Some(4014.0 * 0.08 - 40.0),
            gear: Some(Gear::R),
            odometer_km: Some(12_345.678),
            soc: Some(71.0),
            soc_basis: None,
            soc_bms: Some(996.0 * 0.1),
            range_km: Some(256.0 * 1.609_344),
            ideal_range_km: Some(1.0 / 3.0),
            energy_remaining_kwh: Some(67.86),
            battery_voltage_v: Some(401.39),
            battery_current_a: Some(-0.4),
            battery_power_kw: Some(401.39 * -0.4 / 1000.0),
            battery_temp_c: Some(21.5),
            outside_temp_c: Some(-1.5),
            inside_temp_c: Some(23.5),
            charging: Some(true),
            charge_kind: Some(ChargeKind::Dc),
            charge_power_kw: Some(150.2),
            charger_voltage_v: Some(7466.0 * 0.0333),
            charger_current_a: Some(-3.0),
            total_charge_kwh: Some(25_597.77),
            total_discharge_kwh: Some(24_715.038),
            lv_voltage_v: Some(28_859.0 * 0.000_544),
            tpms_bar: [Some(2.9), Some(116.0 * 0.025), None, Some(3.0)],
        };
        let mut pts = vec![Sample { t_ms: 5, ..Sample::default() }, full(1000), full(2000), full(2001)];
        pts[2].gear = Some(Gear::D);
        pts[2].charging = Some(false);
        pts[2].charge_kind = Some(ChargeKind::Ac);
        pts[3].gear = Some(Gear::N);
        pts[3].lat = Some(f64::MIN_POSITIVE);
        let back: Vec<Sample> = decode(&encode(&pts)).unwrap();
        assert_eq!(back.len(), pts.len());
        for (a, b) in back.iter().zip(&pts) {
            // bit for bit: compare the JSON with every float spelled out exactly
            assert_eq!(format!("{a:?}"), format!("{b:?}"));
        }
        let none: Vec<Sample> = decode(&encode::<Sample>(&[])).unwrap();
        assert!(none.is_empty());
        // odd floats come back bit for bit too
        let odd: Vec<Sample> = [-0.0, 0.0, f64::NAN, f64::INFINITY, 1e-300, -2.5]
            .iter()
            .enumerate()
            .map(|(i, &v)| Sample { t_ms: i as u64, soc: Some(v), lat: Some(-0.0), ..Sample::default() })
            .collect();
        let back: Vec<Sample> = decode(&encode(&odd)).unwrap();
        for (a, b) in back.iter().zip(&odd) {
            assert_eq!(a.soc.map(f64::to_bits), b.soc.map(f64::to_bits));
            assert_eq!(a.lat.map(f64::to_bits), b.lat.map(f64::to_bits));
        }
    }

    #[test]
    fn power_worked_out_from_v_and_i_costs_nothing() {
        let day = |exact: bool| -> Vec<Sample> {
            (0..1000u64)
                .map(|i| {
                    let v = 400.0 + (i % 7) as f64 * 0.01;
                    let a = (i % 50) as f64 * 0.1 - 2.0;
                    let p = if exact || i != 500 { v * a / 1000.0 } else { 1.0 };
                    Sample {
                        t_ms: i * 1000,
                        battery_voltage_v: Some(v),
                        battery_current_a: Some(a),
                        battery_power_kw: Some(p),
                        ..Sample::default()
                    }
                })
                .collect()
        };
        let (exact, not) = (encode(&day(true)), encode(&day(false)));
        assert!(exact.len() + 4000 < not.len(), "{} vs {}", exact.len(), not.len());
        assert_eq!(decode::<Sample>(&exact).unwrap(), day(true));
        assert_eq!(decode::<Sample>(&not).unwrap(), day(false));
        // asked for alone, it is still worked out
        let f = decode_fields::<Sample>(&exact, &["battery_power_kw", "gear"]).unwrap();
        assert_eq!(f.t_ms.len(), 1000);
        assert_eq!(f.values[0].1[10], day(true)[10].battery_power_kw);
        assert_eq!(f.values[1].1[10], None);
        assert!(decode_fields::<Sample>(&exact, &["no_such_field"]).is_err());
    }

    #[test]
    fn damaged_columns_are_refused() {
        let pts: Vec<ChargePoint> = (0..50)
            .map(|i| ChargePoint { t_ms: 1000 * i, power_kw: Some(i as f64 * 0.1), ..ChargePoint::default() })
            .collect();
        let body = encode(&pts);
        for cut in 0..body.len() {
            assert!(decode::<ChargePoint>(&body[..cut]).is_err(), "cut at {cut}");
        }
        let mut big = body.clone();
        big[0] = 0xff;
        big[1] = 0xff;
        assert!(decode::<ChargePoint>(&big).is_err());
    }
}
