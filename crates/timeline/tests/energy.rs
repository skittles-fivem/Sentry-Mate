#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Energy and distance sums where the readings are far apart, late or of more than one phase. Made-up values only;
//! positions in the open sea.

use model::{ChargeKind, Gear, Inputs, Sample, SampleExtra};
use timeline::{ApiSpan, ApiState, Closed, FuseParams, Input, LogFacts, Params, Timeline, fuse};

/// 2026-01-01T00:00:00Z.
const T0: u64 = 1_767_225_600_000;

/// One log a minute over `secs` seconds.
fn logs(secs: u64) -> Vec<LogFacts> {
    (0..secs.div_ceil(60))
        .map(|i| LogFacts {
            first_frame_ms: Some(T0 + i * 60_000),
            last_frame_ms: Some(T0 + i * 60_000 + 60_000),
            buses: vec!["can0".into()],
            ..LogFacts::new(T0 + i * 60_000, T0 + i * 60_000 + 60_000, Closed::Rotate)
        })
        .collect()
}

fn build_smo(smo: &[Sample], secs: u64) -> Timeline {
    let logs = logs(secs);
    let input = Input {
        car: 1,
        from_ms: T0,
        to_ms: T0 + secs * 1000,
        samples: smo,
        logs: &logs,
        holes: &[],
        api: &[],
        marks: &[],
        order: Default::default(),
    };
    timeline::build(&input, &Params::default())
}

/// The API's view of an AC charge at `kw` from `from_s` to `to_s`, a reading every `every_s`, twenty minutes parked
/// after it.
fn api_charge(from_s: u64, to_s: u64, every_s: u64, kw: f64) -> Vec<Sample> {
    (0..=(to_s + 1200))
        .step_by(every_s as usize)
        .map(|t| {
            let on = (from_s..to_s).contains(&t);
            Sample {
                t_ms: T0 + t * 1000,
                lat: Some(0.5),
                lon: Some(0.5),
                charging: Some(on),
                charge_power_kw: on.then_some(kw),
                charger_voltage_v: on.then_some(230.0),
                charger_current_a: on.then_some(kw * 1000.0 / 230.0),
                charge_kind: on.then_some(ChargeKind::Ac),
                soc: Some(50.0),
                odometer_km: Some(1000.0),
                ..Sample::default()
            }
        })
        .collect()
}

fn api_drive(a: u64, b: u64, to: u64, every: u64) -> Vec<Sample> {
    (0..to)
        .step_by(every as usize)
        .map(|t| {
            let moving = (a..b).contains(&t);
            let driven = t.clamp(a, b) - a;
            Sample {
                t_ms: T0 + t * 1000,
                lat: Some(0.5 + driven as f64 / 60.0 / 111.195),
                lon: Some(0.5),
                speed_kmh: moving.then_some(60.0),
                gear: moving.then_some(Gear::D),
                odometer_km: Some(1000.0 + driven as f64 / 60.0),
                soc: Some(80.0),
                battery_power_kw: Some(if moving { 15.0 } else { 0.3 }),
                ..Sample::default()
            }
        })
        .collect()
}

/// The timeline of an API's samples laid over SMO's (none: the API alone).
fn build_fused(smo: &[Sample], api: &[Sample], to_s: u64) -> Timeline {
    let f = fuse(smo, api, &FuseParams::default());
    let facts =
        [ApiSpan { start_ms: T0, end_ms: T0 + to_s * 1000, state: ApiState::Online, data: true, inputs: Inputs::NONE }];
    let logs = if smo.is_empty() { Vec::new() } else { logs(to_s) };
    let input = Input {
        car: 1,
        from_ms: T0,
        to_ms: T0 + to_s * 1000,
        samples: &f.samples,
        logs: &logs,
        holes: &[],
        api: &facts,
        marks: &f.marks,
        order: Default::default(),
    };
    timeline::build(&input, &Params::default())
}

#[test]
fn a_charge_seen_by_the_api_alone_has_its_energy_at_every_pace() {
    // two hours at 11 kW: 22 kWh, whether read every 15 s or every 2 minutes (the gentle pace)
    for every in [15u64, 30, 60, 120] {
        let api = api_charge(600, 600 + 7200, every, 11.0);
        let t = build_fused(&[], &api, 600 + 7200 + 1200);
        assert_eq!(t.charges.len(), 1, "every {every} s");
        let added = t.charges[0].energy_added_kwh.unwrap();
        assert!((added - 22.0).abs() < 0.5, "every {every} s: energy added {added}");
        let grid = t.charges[0].energy_used_kwh.unwrap();
        assert!((grid - 22.0).abs() < 0.5, "every {every} s: grid energy {grid}");
    }
}

#[test]
fn a_drive_seen_by_the_api_alone_has_its_energy_at_every_pace() {
    // an hour at 15 kW: 15 kWh
    for every in [15u64, 30, 60, 120] {
        let api = api_drive(600, 4200, 5400, every);
        let t = build_fused(&[], &api, 5400);
        assert_eq!(t.drives.len(), 1, "every {every} s");
        let e = t.drives[0].energy_used_kwh.unwrap();
        assert!((e - 15.0).abs() < 0.6, "every {every} s: energy {e}");
    }
}

#[test]
fn a_reading_is_held_until_the_next_second_that_has_the_value() {
    // 10 kW for an hour; every other second lacks the charge power
    let smo: Vec<Sample> = (0..3600u64)
        .map(|t| Sample {
            t_ms: T0 + t * 1000,
            charging: Some(true),
            charge_power_kw: (t % 2 == 0).then_some(10.0),
            charge_kind: Some(ChargeKind::Dc),
            soc: Some(50.0),
            ..Sample::default()
        })
        .collect();
    let t = build_smo(&smo, 3600);
    assert_eq!(t.charges.len(), 1);
    let e = t.charges[0].energy_added_kwh.unwrap();
    assert!((e - 10.0).abs() < 0.01, "{e}");
    // a value missing for longer than the hold time still counts for the hold time only: one reading a minute
    let smo: Vec<Sample> = (0..3600u64)
        .map(|t| Sample {
            t_ms: T0 + t * 1000,
            charging: Some(true),
            charge_power_kw: (t % 60 == 0).then_some(10.0),
            charge_kind: Some(ChargeKind::Dc),
            soc: Some(50.0),
            ..Sample::default()
        })
        .collect();
    let t = build_smo(&smo, 3600);
    let e = t.charges[0].energy_added_kwh.unwrap();
    assert!((e - 10.0 / 6.0).abs() < 0.01, "{e}");
}

#[test]
fn an_apis_power_beside_smo_seconds_without_it_is_held_as_the_apis() {
    // SMO sends every second but no charge or pack power (a bus without them); the API has the power at each poll
    let smo: Vec<Sample> = (0..(600 + 7200 + 1200u64))
        .map(|t| Sample { t_ms: T0 + t * 1000, soc: Some(50.0), ..Sample::default() })
        .collect();
    for every in [15u64, 30, 60, 120] {
        let api = api_charge(600, 600 + 7200, every, 11.0);
        let t = build_fused(&smo, &api, 600 + 7200 + 1200);
        assert_eq!(t.charges.len(), 1, "every {every} s");
        let added = t.charges[0].energy_added_kwh.unwrap();
        assert!((added - 22.0).abs() < 0.5, "every {every} s: energy added {added}");
    }
}

/// A two-hour AC charge at 16 A and 230 V a phase, as the car says it.
fn ac_charge(extra: SampleExtra, pack_kw: f64) -> Timeline {
    let smo: Vec<Sample> = (0..7200u64)
        .map(|t| Sample {
            t_ms: T0 + t * 1000,
            charging: Some(true),
            charge_power_kw: Some(pack_kw),
            charger_voltage_v: Some(230.0),
            charger_current_a: Some(16.0),
            charge_kind: Some(ChargeKind::Ac),
            soc: Some(50.0),
            extra: extra.clone(),
            ..Sample::default()
        })
        .collect();
    build_smo(&smo, 7200)
}

#[test]
fn grid_energy_counts_every_phase() {
    // three phases in use: 3 x 230 V x 16 A = 11.04 kW, 22.08 kWh in two hours
    let t = ac_charge(SampleExtra { charger_phases: Some(3.0), ..SampleExtra::default() }, 10.0);
    let c = &t.charges[0];
    assert!((c.energy_used_kwh.unwrap() - 22.08).abs() < 0.01, "{:?}", c.energy_used_kwh);
    assert!((c.energy_added_kwh.unwrap() - 20.0).abs() < 0.01);
    assert_eq!(c.energy_spans, None);
    // the line power the car sends decides where it is known: 11 kW over 3.68 kW a phase is three phases, whatever
    // the phase count says
    let t = ac_charge(SampleExtra { charger_line_kw: Some(11.0), ..SampleExtra::default() }, 10.0);
    assert!((t.charges[0].energy_used_kwh.unwrap() - 22.08).abs() < 0.01);
    let t =
        ac_charge(SampleExtra { charger_line_kw: Some(3.7), charger_phases: Some(3.0), ..SampleExtra::default() }, 3.3);
    assert!((t.charges[0].energy_used_kwh.unwrap() - 7.36).abs() < 0.01);
    // one phase, or nothing said: volts times amps
    for phases in [Some(1.0), None, Some(0.0)] {
        let t = ac_charge(SampleExtra { charger_phases: phases, ..SampleExtra::default() }, 3.3);
        assert!((t.charges[0].energy_used_kwh.unwrap() - 7.36).abs() < 0.01, "{phases:?}");
    }
}

#[test]
fn an_apis_phase_count_comes_with_its_current() {
    // SMO without the charger's values; the API's volts, amps and phases fill its seconds
    let smo: Vec<Sample> = (0..(600 + 7200 + 1200u64))
        .map(|t| Sample { t_ms: T0 + t * 1000, soc: Some(50.0), ..Sample::default() })
        .collect();
    let mut api = api_charge(600, 600 + 7200, 15, 11.0);
    for x in api.iter_mut().filter(|x| x.charging == Some(true)) {
        x.charger_current_a = Some(16.0);
        x.extra.charger_phases = Some(3.0);
    }
    let t = build_fused(&smo, &api, 600 + 7200 + 1200);
    let grid = t.charges[0].energy_used_kwh.unwrap();
    assert!((grid - 22.08).abs() < 0.3, "{grid}");
}

/// An hour at 60 km/h from second 60, the odometer read from `odo_from` to `odo_to` only.
fn drive_with_odometer(odo_from: u64, odo_to: u64, gps: bool) -> Timeline {
    let smo: Vec<Sample> = (0..4200u64)
        .map(|t| {
            let moving = (60..3660).contains(&t);
            let driven = t.clamp(60, 3660) - 60;
            Sample {
                t_ms: T0 + t * 1000,
                lat: gps.then_some(0.5 + driven as f64 / 60.0 / 111.195),
                lon: gps.then_some(0.5),
                speed_kmh: Some(if moving { 60.0 } else { 0.0 }),
                gear: Some(if moving { Gear::D } else { Gear::P }),
                odometer_km: (odo_from..odo_to).contains(&t).then_some(1000.0 + driven as f64 / 60.0),
                battery_power_kw: Some(15.0),
                ..Sample::default()
            }
        })
        .collect();
    build_smo(&smo, 4200)
}

#[test]
fn a_drive_whose_odometer_comes_late_or_stops_early_has_its_whole_distance() {
    // read all the way: the odometer's own difference
    let t = drive_with_odometer(0, 4200, true);
    let d = &t.drives[0];
    assert!((d.distance_km.unwrap() - 60.0).abs() < 0.05, "{:?}", d.distance_km);
    assert!((d.start_odometer_km.unwrap() - 1000.0).abs() < 0.05);
    // first read ten minutes in: the GPS track gives the ten kilometres before it
    let t = drive_with_odometer(660, 4200, true);
    let d = &t.drives[0];
    assert!((d.distance_km.unwrap() - 60.0).abs() < 0.2, "{:?}", d.distance_km);
    assert!((d.start_odometer_km.unwrap() - 1000.0).abs() < 0.2, "{:?}", d.start_odometer_km);
    assert!((d.end_odometer_km.unwrap() - 1060.0).abs() < 0.05, "{:?}", d.end_odometer_km);
    // last read ten minutes before the end
    let t = drive_with_odometer(0, 3060, true);
    let d = &t.drives[0];
    assert!((d.distance_km.unwrap() - 60.0).abs() < 0.2, "{:?}", d.distance_km);
    assert!((d.end_odometer_km.unwrap() - 1060.0).abs() < 0.2, "{:?}", d.end_odometer_km);
    // no GPS to say what the odometer missed: what the odometer has
    let t = drive_with_odometer(660, 4200, false);
    let d = &t.drives[0];
    assert!((d.distance_km.unwrap() - 50.0).abs() < 0.05, "{:?}", d.distance_km);
    assert!((d.start_odometer_km.unwrap() - 1010.0).abs() < 0.05);
}
