#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Edges of drives and charges: a drive an API read up to the reading that sees it parked, a bad last odometer
//! reading, sparse GPS fixes at a stop, and a few seconds of charging. Made-up values only; positions in the open sea.

mod common;

use common::{Car, T0, build};
use model::{ChargeKind, Gear, Inputs, Sample};
use timeline::{ApiSpan, ApiState, FuseParams, Input, Params, fuse};

/// The API's view of a car driving at 60 km/h from `a` to `b` (seconds from T0), one sample every `every` s, parked
/// (no gear, as the API says it) before and after.
fn api_drive(from: u64, a: u64, b: u64, to: u64, every: u64) -> Vec<Sample> {
    (from..to)
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
                soc: Some(80.0 - driven as f64 / 600.0),
                battery_power_kw: Some(if moving { 15.0 } else { 0.3 }),
                ..Sample::default()
            }
        })
        .collect()
}

// A drive read by an API alone runs up to the first reading in P: the stretch after its last reading in D counts.
#[test]
fn api_only_drive_keeps_its_last_leg() {
    // 6 minutes at 60 km/h = 6.0 km and 1.5 kWh, one reading every 15 s
    let api = api_drive(0, 600, 960, 1500, 15);
    let f = fuse(&[], &api, &FuseParams::default());
    let facts =
        [ApiSpan { start_ms: T0, end_ms: T0 + 1500 * 1000, state: ApiState::Online, data: true, inputs: Inputs::NONE }];
    let input = Input {
        car: 1,
        from_ms: T0,
        to_ms: T0 + 1500 * 1000,
        samples: &f.samples,
        logs: &[],
        holes: &[],
        api: &facts,
        marks: &f.marks,
        order: Default::default(),
    };
    let t = timeline::build(&input, &Params::default());
    assert_eq!(t.drives.len(), 1, "{:?}", t.drives);
    let d = &t.drives[0];
    assert!((d.distance_km.unwrap() - 6.0).abs() < 0.05, "distance {:?}", d.distance_km);
    assert!((d.end_odometer_km.unwrap() - 1006.0).abs() < 0.05, "end odometer {:?}", d.end_odometer_km);
    assert!((d.end_ms - d.start_ms) >= 355_000, "duration {} ms", d.end_ms - d.start_ms);
}

// Three seconds of charging at plug-in are no charge of their own.
#[test]
fn a_few_seconds_of_charging_is_not_a_charge() {
    let mut car = Car::new(1, T0);
    car.park(60);
    car.charge(3, 7.0, ChargeKind::Ac);
    car.plugged(4 * 3600, ChargeKind::Ac);
    car.charge(1800, 7.0, ChargeKind::Ac);
    car.park(300);
    let end = car.t;
    let (samples, logs) = car.finish();
    let t = build(1, T0, end, &samples, &logs);
    assert_eq!(t.charges.len(), 1, "{:?}", t.charges.iter().map(|c| c.end_ms - c.start_ms).collect::<Vec<_>>());
}

// One bad odometer reading in a drive's last second does not set the drive's distance.
#[test]
fn a_bad_last_odometer_reading_does_not_set_the_distance() {
    let mut car = Car::new(1, T0);
    car.park(120);
    car.drive(300, 60.0, 15.0); // 5 km
    car.park(300);
    let end = car.t;
    let (mut samples, logs) = car.finish();
    let last_d = samples.iter().rposition(|s| s.gear == Some(Gear::D)).unwrap();
    samples[last_d].odometer_km = Some(1_000_000.0);
    let t = build(1, T0, end, &samples, &logs);
    assert_eq!(t.drives.len(), 1);
    let km = t.drives[0].distance_km.unwrap();
    assert!((km - 5.0).abs() < 0.5, "distance {km} km in 300 s");
}

// With no odometer, a long GPS step that ends at a stop is still driven distance: sparse fixes lose no km.
#[test]
fn sparse_gps_fixes_at_a_stop_do_not_lose_distance() {
    // no odometer; 600 s at 60 km/h = 10 km, kept as one fix every 30 s; every other kept fix reads speed 0 (a light)
    let mut car = Car::new(1, T0);
    car.odo = None;
    car.park(120);
    car.drive(600, 60.0, 15.0);
    car.park(300);
    let end = car.t;
    let (all, logs) = car.finish();
    let mut k = 0u32;
    let samples: Vec<Sample> = all
        .into_iter()
        .filter_map(|mut s| {
            if s.gear != Some(Gear::D) {
                return Some(s);
            }
            let sec = (s.t_ms - T0) / 1000 - 120;
            sec.is_multiple_of(30).then(|| {
                k += 1;
                if k.is_multiple_of(2) {
                    s.speed_kmh = Some(0.0);
                }
                s
            })
        })
        .collect();
    let t = build(1, T0, end, &samples, &logs);
    assert_eq!(t.drives.len(), 1, "{:?}", t.drives);
    let km = t.drives[0].distance_km.unwrap();
    assert!((km - 10.0).abs() < 0.5, "10 km driven, stored {km}");
}
