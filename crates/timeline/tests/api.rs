#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The Tesla API as a second live input: its samples fused with SMO's, its view of the car's state laid over SMO's,
//! and the rows naming the inputs that gave them. Made-up values only; positions in the open sea.

use model::{Gear, Inputs, Sample, Source, State};
use timeline::{ApiSpan, ApiState, Closed, FuseParams, Input, LogFacts, Params, fuse};

/// 2026-01-01T00:00:00Z.
const T0: u64 = 1_767_225_600_000;
const M: u64 = 60_000;

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
                inside_temp_c: Some(20.0),
                ..Sample::default()
            }
        })
        .collect()
}

/// SMO's view of the same drive, every second, from `x` to `y` only (seconds from T0).
fn smo_part(x: u64, y: u64, a: u64, b: u64) -> Vec<Sample> {
    (x..y)
        .map(|t| {
            let moving = (a..b).contains(&t);
            let driven = t.clamp(a, b) - a;
            Sample {
                t_ms: T0 + t * 1000,
                lat: Some(0.5 + driven as f64 / 60.0 / 111.195),
                lon: Some(0.5),
                speed_kmh: Some(if moving { 60.0 } else { 0.0 }),
                gear: Some(if moving { Gear::D } else { Gear::P }),
                odometer_km: Some(1000.0 + driven as f64 / 60.0),
                soc: Some(80.0 - driven as f64 / 600.0),
                battery_power_kw: Some(if moving { 15.0 } else { 0.3 }),
                ..Sample::default()
            }
        })
        .collect()
}

fn online_log(x: u64, y: u64, closed: Closed) -> LogFacts {
    LogFacts {
        first_frame_ms: Some(T0 + x * 1000),
        last_frame_ms: Some(T0 + y * 1000),
        buses: vec!["can0".into(), "can1".into()],
        ..LogFacts::new(T0 + x * 1000, T0 + y * 1000, closed)
    }
}

fn span(a: u64, b: u64, state: ApiState, data: bool) -> ApiSpan {
    ApiSpan { start_ms: T0 + a * 1000, end_ms: T0 + b * 1000, state, data, inputs: Inputs::NONE }
}

#[test]
fn a_drive_from_the_api_alone() {
    // polled every 30 s: ten minutes parked, a 20-minute drive, ten minutes parked
    let api = api_drive(0, 600, 1800, 2400, 30);
    let f = fuse(&[], &api, &FuseParams::default());
    assert!(f.marks.iter().all(|m| m.1 == Inputs::TESLA_API));
    let facts = [span(0, 2400, ApiState::Online, true)];
    let input = Input {
        car: 1,
        from_ms: T0,
        to_ms: T0 + 2400 * 1000,
        samples: &f.samples,
        logs: &[],
        holes: &[],
        api: &facts,
        marks: &f.marks,
    };
    let t = timeline::build(&input, &Params::default());
    assert_eq!(t.drives.len(), 1, "{:?}", t.drives);
    let d = &t.drives[0];
    assert_eq!(d.inputs, Inputs::TESLA_API);
    assert_eq!((d.start_ms - T0) / 1000, 600);
    assert!((d.distance_km.unwrap() - 19.5).abs() < 0.6, "{:?}", d.distance_km);
    assert!(d.energy_used_kwh.unwrap() > 4.0, "power held across the 30 s between samples: {:?}", d.energy_used_kwh);
    assert!(t.states.iter().all(|s| s.state == State::Online && s.inputs == Inputs::TESLA_API), "{:?}", t.states);
    assert_eq!(t.states[0].source, Source::Live);
}

#[test]
fn one_drive_seen_by_both_is_one_row_naming_both() {
    let (a, b) = (120, 900);
    let smo = smo_part(0, 1200, a, b);
    let api = api_drive(0, a, b, 1200, 30);
    let f = fuse(&smo, &api, &FuseParams::default());
    let logs: Vec<LogFacts> = (0..20).map(|i| online_log(i * 60, i * 60 + 60, Closed::Rotate)).collect();
    let facts = [span(0, 1200, ApiState::Online, true)];
    let input = Input {
        car: 1,
        from_ms: T0,
        to_ms: T0 + 1200 * 1000,
        samples: &f.samples,
        logs: &logs,
        holes: &[],
        api: &facts,
        marks: &f.marks,
    };
    let t = timeline::build(&input, &Params::default());
    assert_eq!(t.drives.len(), 1);
    assert_eq!(t.drives[0].inputs, Inputs::BOTH, "SMO's drive, with the cabin temperature from the API");
    assert_eq!(t.drives[0].inside_temp_avg_c, Some(20.0));
    assert_eq!(t.drives[0].speed_max_kmh, Some(60.0));
    // SMO's frames give the states
    assert!(t.states.iter().all(|s| s.inputs == Inputs::SMO), "{:?}", t.states);
}

#[test]
fn smo_offline_mid_drive_the_api_fills_the_gap() {
    let (a, b) = (120, 2100);
    // SMO records the first ten minutes, then stops (the bridge lost power) until well after the drive
    let smo = smo_part(0, 600, a, b);
    let api = api_drive(0, a, b, 2700, 30);
    let f = fuse(&smo, &api, &FuseParams::default());
    let logs: Vec<LogFacts> = (0..10).map(|i| online_log(i * 60, i * 60 + 60, Closed::Rotate)).collect();
    let facts = [span(0, 2700, ApiState::Online, true)];
    let input = Input {
        car: 1,
        from_ms: T0,
        to_ms: T0 + 2700 * 1000,
        samples: &f.samples,
        logs: &logs,
        holes: &[],
        api: &facts,
        marks: &f.marks,
    };
    let t = timeline::build(&input, &Params::default());
    assert_eq!(t.drives.len(), 1, "one drive across the gap: {:?}", t.drives);
    let d = &t.drives[0];
    assert_eq!(d.inputs, Inputs::BOTH);
    // (to the last API sample in D: 2070 s)
    assert_eq!(((d.start_ms - T0) / 1000, (d.end_ms - T0) / 1000), (120, 2071));
    assert!((d.distance_km.unwrap() - 33.0).abs() < 0.6, "{:?}", d.distance_km);
    // the states: SMO's while it recorded, the API's after
    let gap: Vec<_> = t.states.iter().filter(|s| s.start_ms >= T0 + 600_000).collect();
    assert!(!gap.is_empty() && gap.iter().all(|s| s.state == State::Online && s.inputs == Inputs::TESLA_API));
    assert!(t.states.iter().any(|s| s.inputs == Inputs::SMO && s.state == State::Online));
}

#[test]
fn the_api_fills_what_smo_does_not_know_and_never_overrules_its_frames() {
    // SMO: online 0-10 min, a quiet close (asleep until the next frames at 60 min), online 60-70 min
    let logs = [online_log(0, 600, Closed::Quiet), online_log(3600, 4200, Closed::Rotate)];
    let facts = [
        // the API says asleep while SMO sleeps too, and (wrongly) asleep during SMO's frames: ignored there
        span(300, 1800, ApiState::Asleep, false),
        // the car's values came at 30-40 min: it was awake after all
        span(1800, 2400, ApiState::Online, true),
        // the vehicle list alone says online at 40-45 min: SMO's asleep stands
        span(2400, 2700, ApiState::Online, false),
    ];
    let input = Input {
        car: 1,
        from_ms: T0,
        to_ms: T0 + 80 * M,
        samples: &[],
        logs: &logs,
        holes: &[(T0 + 70 * M, T0 + 75 * M)],
        api: &facts,
        marks: &[],
    };
    let t = timeline::build(&input, &Params::default());
    let s: Vec<(State, u64, u64, Inputs)> =
        t.states.iter().map(|s| (s.state, (s.start_ms - T0) / M, (s.end_ms - T0) / M, s.inputs)).collect();
    assert_eq!(
        s,
        vec![
            (State::Online, 0, 10, Inputs::SMO),
            (State::Asleep, 10, 30, Inputs::SMO),
            (State::Online, 30, 40, Inputs::TESLA_API),
            (State::Asleep, 40, 60, Inputs::SMO),
            (State::Online, 60, 70, Inputs::SMO),
            (State::Unknown, 70, 80, Inputs::SMO),
        ]
    );
    // an unknown stretch the API saw asleep is asleep
    let facts = [span(70 * 60, 80 * 60, ApiState::Asleep, false)];
    let t = timeline::build(&Input { api: &facts, ..input }, &Params::default());
    let last = t.states.last().unwrap();
    assert_eq!((last.state, last.inputs), (State::Asleep, Inputs::TESLA_API));
}

#[test]
fn a_drive_from_teslafi_alone_names_teslafi_and_the_tesla_api_wins_over_it() {
    // TeslaFi's polls every 60 s; no SMO, no Tesla API
    let fi = api_drive(0, 600, 1800, 2400, 60);
    let p = FuseParams::default();
    let f = timeline::fuse_all(&[], &[(&[], Inputs::TESLA_API), (&fi, Inputs::TESLAFI_API)], &p);
    assert!(f.marks.iter().all(|m| m.1 == Inputs::TESLAFI_API));
    let facts = [ApiSpan { inputs: Inputs::TESLAFI_API, ..span(0, 2400, ApiState::Online, true) }];
    let input = Input {
        car: 1,
        from_ms: T0,
        to_ms: T0 + 2400 * 1000,
        samples: &f.samples,
        logs: &[],
        holes: &[],
        api: &facts,
        marks: &f.marks,
    };
    let t = timeline::build(&input, &Params::default());
    assert_eq!(t.drives.len(), 1, "{:?}", t.drives);
    assert_eq!(t.drives[0].inputs, Inputs::TESLAFI_API);
    assert!(t.states.iter().all(|s| s.inputs == Inputs::TESLAFI_API), "{:?}", t.states);

    // the Tesla API polled the same drive every 30 s: its seconds, TeslaFi's only fill
    let api = api_drive(0, 600, 1800, 2400, 30);
    let f = timeline::fuse_all(&[], &[(&api, Inputs::TESLA_API), (&fi, Inputs::TESLAFI_API)], &p);
    let both = [span(0, 2400, ApiState::Online, true), facts[0]];
    let t = timeline::build(&Input { samples: &f.samples, marks: &f.marks, api: &both, ..input }, &Params::default());
    assert_eq!(t.drives.len(), 1);
    assert_eq!(t.drives[0].inputs, Inputs::TESLA_API, "TeslaFi had nothing the Tesla API lacked");
    assert!(t.states.iter().all(|s| s.inputs == Inputs::TESLA_API), "the Tesla API's view of the state wins");
}

#[test]
fn states_name_the_api_that_saw_each_stretch() {
    let logs: Vec<LogFacts> = (0..5).map(|i| online_log(i * 60, i * 60 + 60, Closed::Rotate)).collect();
    let fi = Inputs::TESLAFI_API;
    let facts = [
        span(0, 720, ApiState::Online, false),
        span(0, 691, ApiState::Online, true),
        ApiSpan { inputs: fi, ..span(0, 1800, ApiState::Online, false) },
        ApiSpan { inputs: fi, ..span(0, 1741, ApiState::Online, true) },
    ];
    let input = Input {
        car: 1,
        from_ms: T0,
        to_ms: T0 + 2400 * 1000,
        samples: &[],
        logs: &logs,
        holes: &[],
        api: &facts,
        marks: &[],
    };
    let t = timeline::build(&input, &Params::default());
    let s: Vec<(State, u64, u64, Inputs)> =
        t.states.iter().map(|s| (s.state, (s.start_ms - T0) / 1000, (s.end_ms - T0) / 1000, s.inputs)).collect();
    assert!(s.contains(&(State::Online, 300, 691, Inputs::TESLA_API)), "{s:?}");
    assert!(s.iter().any(|x| x.0 == State::Online && x.2 == 1800 && x.3 == fi), "{s:?}");
}
