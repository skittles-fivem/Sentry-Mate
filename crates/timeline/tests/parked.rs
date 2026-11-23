#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The sample builder on a parked, plugged-in car with its charge complete (`fixtures/parked-plugged-in.signals.csv`:
//! made-up values, times and counters in the shape of a real car's, no position).

use model::{ChargeKind, State};
use timeline::{Closed, Input, LogFacts, Params, SampleBuilder, SampleParams};

/// (time µs, series, value) rows of the fixture.
fn rows() -> Vec<(u64, String, f64)> {
    include_str!("fixtures/parked-plugged-in.signals.csv")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("timestamp_utc"))
        .map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            let (s, us) = f[0].split_once('.').unwrap();
            (
                s.parse::<u64>().unwrap() * 1_000_000 + us.parse::<u64>().unwrap(),
                f[1].to_string(),
                f[2].parse().unwrap(),
            )
        })
        .collect()
}

#[test]
fn a_parked_plugged_in_car_with_its_charge_complete() {
    let mut b = SampleBuilder::new(dbc::Profile::Tesla3Y, SampleParams::default());
    for (t, series, v) in rows() {
        b.value(t, &series, v);
    }
    let (samples, vin) = b.finish();
    assert!(vin.is_none());
    // the first, cut second gives none; then one a second
    assert_eq!(samples.len(), 3);
    let s = &samples[1];
    assert_eq!(s.t_ms, 1_767_225_602_000);
    // BMS_uiChargeStatus says BMS_CHARGE_COMPLETE: not charging, though the charge port and the PCS say enabled
    assert_eq!(s.charging, Some(false));
    assert_eq!(s.charge_power_kw, None);
    assert_eq!(s.charge_kind, Some(ChargeKind::Ac), "AC_CHARGER_PRESENT");
    // the screen's %, while the battery controller's 0x292 says far more
    assert_eq!(s.soc, Some(60.0));
    assert!((s.soc_bms.unwrap() - 93.5).abs() < 1e-9);
    assert_eq!(s.outside_temp_c, Some(8.0));
    assert_eq!(s.inside_temp_c, Some(20.0));
    // UI_Range is in miles in the DBC
    assert!((s.range_km.unwrap() - 200.0 * 1.609344).abs() < 1e-9);
    assert!((s.ideal_range_km.unwrap() - 400.0 * 1.609344).abs() < 1e-9);
    assert_eq!(s.energy_remaining_kwh, Some(50.5));
    assert_eq!(s.battery_temp_c, Some(30.5), "between the warmest and coolest brick");
    for p in s.tpms_bar {
        assert!((p.unwrap() - 3.0).abs() < 1e-9);
    }
    assert!((s.lv_voltage_v.unwrap() - 13.8).abs() < 0.1);
    assert!((s.battery_voltage_v.unwrap() - 380.3).abs() < 0.1);
    assert!(s.battery_power_kw.unwrap().abs() < 0.1, "about nothing in or out");
    // the AC line the car runs from while it stays awake
    assert!((s.charger_voltage_v.unwrap() - 240.0).abs() < 0.5);
    assert_eq!(s.charger_current_a, Some(3.0));
    assert_eq!((s.total_charge_kwh, s.total_discharge_kwh), (Some(12000.5), Some(11500.25)));
    // parked with no inverter messages: no gear, no speed; the GPS fix isn't in the fixture
    assert_eq!((s.gear, s.speed_kmh, s.odometer_km), (None, None, None));
    assert_eq!((s.lat, s.lon), (None, None));
    // (the fixture has a row only when a value changed, so the accuracy is there once: in the first sample, as a fix
    // is held 2 s)
    assert_eq!((samples[0].lat, samples[0].gps_accuracy_m), (None, Some(10.0)));
    assert!(samples.iter().all(|x| x.charging == Some(false) && x.soc == Some(60.0)));

    // its timeline: online on the vehicle bus (and the Screenmate's own), nothing else
    let facts = LogFacts {
        first_frame_ms: Some(1_767_225_600_007),
        last_frame_ms: Some(1_767_225_603_953),
        buses: vec!["can0".into(), "can3".into()],
        ..LogFacts::new(1_767_225_600_007, 1_767_225_603_953, Closed::Rotate)
    };
    let logs = [facts];
    let input = Input {
        car: 1,
        from_ms: 1_767_225_600_007,
        to_ms: 1_767_225_603_953,
        samples: &samples,
        logs: &logs,
        holes: &[],
        api: &[],
        marks: &[],
        order: Default::default(),
    };
    let t = timeline::build(&input, &Params::default());
    assert_eq!(t.states.len(), 1);
    assert_eq!((t.states[0].state, t.states[0].buses.clone()), (State::Online, vec!["can0".into(), "can3".into()]));
    assert!(t.drives.is_empty() && t.charges.is_empty() && t.positions.is_empty());
}
