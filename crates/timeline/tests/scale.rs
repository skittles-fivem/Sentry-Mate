#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Many cars: everything is per car, so 2,000 cars are worked on in parallel with nothing shared but the read-only DBC
//! tables, each car's results don't depend on the others, and no two cars share an id. A car's long history costs the
//! window lookup next to nothing.

mod common;

use common::{Car, T0};
use model::{ChargeKind, Source, State, StateSpan};
use std::collections::HashSet;
use std::time::Instant;
use timeline::{Input, Params, SampleBuilder, SampleParams, rebuild_window};

const CARS: u32 = 2_000;

/// What one car's work leaves: its ids and a few figures (everything else is dropped as soon as the car is done).
struct Summary {
    car: u32,
    drive_ids: Vec<u64>,
    charge_ids: Vec<u64>,
    distance_km: f64,
    energy_added_kwh: f64,
    samples_from_log: usize,
}

/// One car, from scratch: a short log through the sample builder, then 45 minutes of history into a timeline.
fn one_car(car: u32) -> Summary {
    // a 20 s log of decoded values (10 frames a second), each car on its own builder
    let mut b = SampleBuilder::new(dbc::Profile::Tesla3Y, SampleParams::default());
    let start_us = (T0 + u64::from(car) * 1000) * 1000;
    for k in 0..200u64 {
        let t = start_us + k * 100_000;
        b.value(t, "DI_gear", 4.0);
        b.value(t, "DI_vehicleSpeed", 40.0 + (car % 7) as f64);
        b.value(t, "BattVoltage132", 400.0);
        b.value(t, "SmoothBattCurrent132", 30.0);
        b.value(t, "UI_SOC", 70.0);
    }
    let (log_samples, _) = b.finish();

    // the same day for every car, at the same times: only the car differs
    let mut c = Car::new(car, T0);
    c.park(60);
    c.drive(600, 50.0, 15.0);
    c.park(120);
    c.charge(500, 11.0, ChargeKind::Ac);
    c.plugged(60, ChargeKind::Ac);
    c.charge(400, 11.0, ChargeKind::Ac);
    c.park(60);
    c.sleep(900);
    c.park(30);
    let end = c.t;
    let (samples, logs) = c.finish();
    let input = Input {
        car,
        from_ms: T0,
        to_ms: end,
        samples: &samples,
        logs: &logs,
        holes: &[],
        api: &[],
        marks: &[],
        order: Default::default(),
    };
    let t = timeline::build(&input, &Params::default());
    Summary {
        car,
        drive_ids: t.drives.iter().map(|d| d.id).collect(),
        charge_ids: t.charges.iter().map(|c| c.id).collect(),
        distance_km: t.drives.iter().filter_map(|d| d.distance_km).sum(),
        energy_added_kwh: t.charges.iter().filter_map(|c| c.energy_added_kwh).sum(),
        samples_from_log: log_samples.len(),
    }
}

#[test]
fn two_thousand_cars_in_parallel() {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 16) as u32;
    let started = Instant::now();
    let summaries: Vec<Summary> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|w| s.spawn(move || (0..CARS).filter(|c| c % threads == w).map(one_car).collect::<Vec<_>>()))
            .collect();
        workers.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    let took = started.elapsed();
    eprintln!("{CARS} cars on {threads} threads: {took:?} ({:?} a car)", took / CARS);

    assert_eq!(summaries.len(), CARS as usize);
    let first = summaries.iter().find(|s| s.car == 0).unwrap();
    let mut ids = HashSet::new();
    for s in &summaries {
        assert_eq!((s.drive_ids.len(), s.charge_ids.len()), (1, 1), "car {}", s.car);
        // every car's history is worked out alone: the same day gives the same figures, whatever the other cars do
        assert_eq!(s.distance_km, first.distance_km);
        assert_eq!(s.energy_added_kwh, first.energy_added_kwh);
        assert_eq!(s.samples_from_log, 19, "20 seconds of frames, the first one cut");
        for id in s.drive_ids.iter().chain(&s.charge_ids) {
            assert_eq!((id >> 32) as u32, s.car, "the car is in the id");
        }
        ids.extend(s.drive_ids.iter().map(|&i| (0, i)));
        ids.extend(s.charge_ids.iter().map(|&i| (1, i)));
    }
    assert_eq!(ids.len(), 2 * CARS as usize, "no two cars share an id");
}

#[test]
fn a_long_history_costs_the_window_lookup_next_to_nothing() {
    // ten years of a car that sleeps every night: 7,300 spans
    let day = 86_400_000u64;
    let mut spans = Vec::new();
    for d in 0..3650u64 {
        let t = T0 + d * day;
        let span = |state, a: u64, b: u64| StateSpan {
            car: 1,
            state,
            start_ms: a,
            end_ms: b,
            buses: Vec::new(),
            source: Source::Live,
            inputs: model::Inputs::SMO,
        };
        spans.push(span(State::Online, t, t + day / 2));
        spans.push(span(State::Asleep, t + day / 2, t + day));
    }
    let params = Params::default();
    let started = Instant::now();
    let mut widest = 0;
    for k in 0..10_000u64 {
        let d = k * 3650 / 10_000;
        let log_from = T0 + d * day + 3_600_000;
        let w = rebuild_window(&spans, &[], log_from, log_from + 60_000, &params);
        let to = w.to_ms.unwrap();
        assert!(w.from_ms <= log_from && to >= log_from + 60_000);
        // (0: nothing settled before, from the car's first data)
        widest = widest.max(to - w.from_ms.max(T0));
    }
    let took = started.elapsed();
    eprintln!("10,000 window lookups over 7,300 spans: {took:?}");
    // the window is the day around the log (the night before to the night after), never the ten years
    assert!(widest <= 2 * day, "{widest}");
    assert!(took.as_secs_f64() < 2.0, "{took:?}");
}

#[test]
fn a_car_that_never_sleeps_costs_the_window_lookup_next_to_nothing_too() {
    // a year of a car that is never asleep (Sentry mode every day), parked most of the time: its online stretches
    // are split at an idle cut point every five minutes, 105,120 spans and as many cut points
    let step = 300_000u64;
    let n = 365 * 288u64;
    let spans: Vec<StateSpan> = (0..n)
        .map(|k| StateSpan {
            car: 1,
            state: State::Online,
            start_ms: T0 + k * step,
            end_ms: T0 + (k + 1) * step,
            buses: vec!["can0".into()],
            source: Source::Live,
            inputs: model::Inputs::SMO,
        })
        .collect();
    let cuts: Vec<u64> = (1..n).map(|k| T0 + k * step).collect();
    let params = Params::default();
    let started = Instant::now();
    let mut widest = 0;
    for k in 0..10_000u64 {
        let log_from = T0 + (k * (n - 2) / 10_000 + 1) * step + 100_000;
        let w = rebuild_window(&spans, &cuts, log_from, log_from + 60_000, &params);
        let to = w.to_ms.unwrap();
        assert!(w.from_ms <= log_from && to >= log_from + 60_000);
        // (0: nothing before, from the car's first data)
        widest = widest.max(to - w.from_ms.max(T0));
    }
    let took = started.elapsed();
    eprintln!("10,000 window lookups over {n} spans and cut points: {took:?}");
    // a quarter of an hour around the log, not the year (nor, without the cut points, everything since it began)
    assert!(widest <= 3 * step, "{widest}");
    assert!(took.as_secs_f64() < 2.0, "{took:?}");
    let without = rebuild_window(&spans, &[], T0 + 100 * step, T0 + 100 * step + 60_000, &params);
    assert_eq!(without, timeline::Window { from_ms: 0, to_ms: None });
}
