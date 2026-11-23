#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The timeline on synthetic cars: drives, charges, states, positions, determinism and rebuilding windows.

mod common;

use common::{Car, T0, build, close};
use model::{ChargeKind, State};
use timeline::{Params, RULES_VERSION, Timeline, rebuild_window};

const HOUR: u64 = 3_600_000;

/// The states as (state, start, end), in ms from T0.
fn states(t: &Timeline) -> Vec<(State, i64, i64)> {
    let rel = |ms: u64| ms as i64 - T0 as i64;
    t.states.iter().map(|s| (s.state, rel(s.start_ms), rel(s.end_ms))).collect()
}

#[test]
fn a_stop_at_a_light_in_d_is_one_drive() {
    let mut car = Car::new(1, T0);
    car.park(120);
    car.drive(300, 50.0, 15.0);
    car.stand(45);
    car.drive(300, 50.0, 15.0);
    car.park(300);
    let end = car.t;
    let (samples, logs) = car.finish();
    let t = build(1, T0, end, &samples, &logs);
    assert_eq!(t.drives.len(), 1, "{:?}", t.drives);
    let d = &t.drives[0];
    assert_eq!((d.start_ms - T0) / 1000, 120);
    assert_eq!((d.end_ms - T0) / 1000, 120 + 645);
    assert!(close(d.distance_km.unwrap(), 600.0 * 50.0 / 3600.0, 1e-6), "{:?}", d.distance_km);
    assert_eq!(d.speed_max_kmh, Some(50.0));
    assert!(close(d.energy_used_kwh.unwrap(), (600.0 * 15.0 + 45.0) / 3600.0, 1e-9));
    assert_eq!(d.rules_version, RULES_VERSION);
    assert!(d.start_soc.unwrap() > d.end_soc.unwrap());
    assert_eq!((d.outside_temp_avg_c, d.inside_temp_avg_c), (Some(10.0), Some(20.0)));
    assert_eq!((d.ascent_m, d.descent_m), (Some(0.0), Some(0.0)));
    let (a, b) = (d.start.as_ref().unwrap(), d.end.as_ref().unwrap());
    assert!(b.lat > a.lat, "drove north");
    // positions: every second of the drive, one per 5 minutes parked
    let driving = t.positions.iter().filter(|p| p.t_ms >= d.start_ms && p.t_ms < d.end_ms).count();
    assert_eq!(driving, 645);
    let parked = t.positions.len() - driving;
    assert!((2..=4).contains(&parked), "{parked}");
    assert!(t.charges.is_empty() && t.charge_points.is_empty());
}

#[test]
fn parking_for_a_minute_splits_drives_and_a_shorter_stop_in_p_does_not() {
    let mut car = Car::new(1, T0);
    car.park(60);
    car.drive(300, 40.0, 12.0);
    car.park(30); // under park_ms: the same drive
    car.drive(300, 40.0, 12.0);
    car.park(120); // parked: a new drive after this
    car.drive(200, 60.0, 20.0);
    car.park(120);
    let end = car.t;
    let (samples, logs) = car.finish();
    let t = build(1, T0, end, &samples, &logs);
    assert_eq!(t.drives.len(), 2, "{:?}", t.drives.iter().map(|d| (d.start_ms, d.end_ms)).collect::<Vec<_>>());
    assert_eq!(((t.drives[0].start_ms - T0) / 1000, (t.drives[0].end_ms - T0) / 1000), (60, 690));
    assert_eq!(((t.drives[1].start_ms - T0) / 1000, (t.drives[1].end_ms - T0) / 1000), (810, 1010));
    assert!(close(t.drives[1].distance_km.unwrap(), 200.0 * 60.0 / 3600.0, 1e-6));
    assert_ne!(t.drives[0].id, t.drives[1].id);
}

#[test]
fn without_an_odometer_the_distance_comes_from_gps() {
    let mut car = Car::new(1, T0);
    car.odo = None;
    car.park(60);
    car.drive(600, 72.0, 18.0);
    car.park(120);
    let end = car.t;
    let (samples, logs) = car.finish();
    let t = build(1, T0, end, &samples, &logs);
    let d = &t.drives[0];
    assert_eq!((d.start_odometer_km, d.end_odometer_km), (None, None));
    // 12 km north (the track's first step starts at the drive's first fix)
    assert!(close(d.distance_km.unwrap(), 12.0, 0.03), "{:?}", d.distance_km);
}

#[test]
fn tiny_moves_are_not_drives() {
    let mut car = Car::new(1, T0);
    car.park(60);
    car.drive(20, 5.0, 3.0); // 20 s
    car.park(120);
    car.drive(120, 0.0, 1.0); // two minutes in D, not moving
    car.park(120);
    let end = car.t;
    let (samples, logs) = car.finish();
    assert!(build(1, T0, end, &samples, &logs).drives.is_empty());
}

#[test]
fn regenerating_takes_energy_off_the_drive() {
    let mut car = Car::new(1, T0);
    car.park(60);
    car.drive(300, 90.0, 30.0);
    car.drive(100, 40.0, -20.0); // braking down a hill
    car.park(120);
    let end = car.t;
    let (samples, logs) = car.finish();
    let d = build(1, T0, end, &samples, &logs).drives.remove(0);
    assert!(close(d.energy_used_kwh.unwrap(), (300.0 * 30.0 - 100.0 * 20.0) / 3600.0, 1e-9));
    assert_eq!((d.power_max_kw, d.power_min_kw), (Some(30.0), Some(-20.0)));
    assert!(d.efficiency_wh_km().unwrap() > 0.0);
}

#[test]
fn a_one_minute_interruption_is_one_charge_and_a_ten_minute_one_two() {
    let mut car = Car::new(1, T0);
    car.park(60);
    car.charge(600, 7.0, ChargeKind::Ac);
    car.plugged(60, ChargeKind::Ac);
    car.charge(600, 7.0, ChargeKind::Ac);
    car.plugged(600, ChargeKind::Ac);
    car.charge(300, 7.0, ChargeKind::Ac);
    car.park(60);
    let end = car.t;
    let (samples, logs) = car.finish();
    let t = build(1, T0, end, &samples, &logs);
    let spans: Vec<(u64, u64)> = t.charges.iter().map(|c| ((c.start_ms - T0) / 1000, (c.end_ms - T0) / 1000)).collect();
    assert_eq!(spans, vec![(60, 1320), (1920, 2220)]);
    let c = &t.charges[0];
    assert_eq!(c.kind, Some(ChargeKind::Ac));
    // the counters agree with the power here: 1,200 s at 7 kW
    assert!(close(c.energy_added_kwh.unwrap(), 1200.0 * 7.0 / 3600.0, 1e-6), "{:?}", c.energy_added_kwh);
    // from the grid: 10 % more while charging, and the car's own 0.3 kW... not counted (no line current then)
    assert!(close(c.energy_used_kwh.unwrap(), 1200.0 * 7.7 / 3600.0, 1e-6), "{:?}", c.energy_used_kwh);
    assert_eq!(c.power_max_kw, Some(7.0));
    assert!(c.end_soc.unwrap() > c.start_soc.unwrap());
    assert!(c.position.is_some());
    assert_eq!((c.outside_temp_avg_c, c.battery_temp_avg_c), (Some(10.0), Some(25.0)));
    // the curve: every second of each charge, the interruption included
    assert_eq!(t.charge_points.iter().filter(|(id, _)| *id == c.id).count(), 1260);
    assert_eq!(t.charge_points.len(), 1260 + 300);
    assert!(t.drives.is_empty());
}

/// A log lost in the middle of a charge (five minutes with no data: unknown, too short to settle) is no interruption:
/// the car charged on both sides of it, as a drive goes on across a lost log. A known interruption that long (the car
/// says it is not charging) still splits it.
#[test]
fn a_lost_log_in_the_middle_of_a_charge_does_not_split_it() {
    let make = |lost: bool| {
        let mut car = Car::new(1, T0);
        car.park(60);
        car.charge(900, 11.0, ChargeKind::Ac);
        if lost {
            car.gap(300);
        } else {
            car.plugged(300, ChargeKind::Ac);
        }
        car.charge(900, 11.0, ChargeKind::Ac);
        car.park(60);
        let end = car.t;
        let (samples, logs) = car.finish();
        build(1, T0, end, &samples, &logs)
    };
    let t = make(true);
    assert!(t.states.iter().any(|s| s.state == State::Unknown && s.end_ms - s.start_ms >= 300_000));
    assert_eq!(t.charges.len(), 1, "{:?}", t.charges.iter().map(|c| (c.start_ms, c.end_ms)).collect::<Vec<_>>());
    let c = &t.charges[0];
    assert_eq!(((c.start_ms - T0) / 1000, (c.end_ms - T0) / 1000), (60, 2160));
    // what the counter says went in: the 1,800 s charged (nothing in the lost minutes here)
    assert!(close(c.energy_added_kwh.unwrap(), 1800.0 * 11.0 / 3600.0, 1e-6), "{:?}", c.energy_added_kwh);
    assert_eq!(make(false).charges.len(), 2);
}

#[test]
fn the_lifetime_counter_gives_the_energy_added_when_it_is_there() {
    let charge = |counters: bool| {
        let mut car = Car::new(1, T0);
        car.counters = counters;
        car.counter_gain = 1.05; // the counter sees 5 % more than the power integral says
        car.park(60);
        car.charge(1800, 150.0, ChargeKind::Dc);
        car.park(60);
        let end = car.t;
        let (samples, logs) = car.finish();
        build(1, T0, end, &samples, &logs).charges.remove(0)
    };
    let by_counter = charge(true);
    let integrated = 1800.0 * 150.0 / 3600.0;
    assert!(close(by_counter.energy_added_kwh.unwrap(), integrated * 1.05, 1e-6), "{:?}", by_counter.energy_added_kwh);
    assert_eq!(by_counter.kind, Some(ChargeKind::Dc));
    assert_eq!(by_counter.energy_used_kwh, None, "a DC charger's side is not known");
    let by_power = charge(false);
    assert!(close(by_power.energy_added_kwh.unwrap(), integrated, 1e-6));
}

#[test]
fn a_gap_with_no_logs_is_unknown_not_asleep() {
    let mut car = Car::new(1, T0);
    car.park(120);
    car.gap(1800);
    car.park(120);
    let end = car.t;
    let (samples, logs) = car.finish();
    let t = build(1, T0, end, &samples, &logs);
    // frames to 0.9 s into each log's last second
    assert_eq!(
        states(&t),
        vec![
            (State::Online, 0, 119_900),
            (State::Unknown, 119_900, 1_920_000),
            // the window ends 0.1 s after the last frame: still online, no sliver of unknown
            (State::Online, 1_920_000, 2_040_000),
        ]
    );
    assert!(t.states.iter().all(|s| s.state != State::Asleep));
    assert_eq!(t.states[0].buses, vec!["can0", "can1"]);
}

/// [`states`] with adjacent spans of one state joined (as a page shows them across idle cut points).
fn joined(t: &Timeline) -> Vec<(State, i64, i64)> {
    let mut out: Vec<(State, i64, i64)> = Vec::new();
    for s in states(t) {
        match out.last_mut() {
            Some(l) if l.0 == s.0 && l.2 == s.1 => l.2 = s.2,
            _ => out.push(s),
        }
    }
    out
}

#[test]
fn a_parked_awake_car_is_online_up_to_its_newest_data_and_a_real_gap_stays_unknown() {
    // parked in P at speed 0, awake, one log a minute: the newest span is online to the end of the data
    let mut car = Car::new(1, T0);
    car.park(600);
    let end = car.t;
    let (samples, logs) = car.finish();
    assert!(samples.iter().all(|s| s.gear == Some(model::Gear::P) && s.speed_kmh == Some(0.0)));
    let t = build(1, T0, end, &samples, &logs);
    assert_eq!(joined(&t), vec![(State::Online, 0, 600_000)]);
    // the window reaching on past the newest frames by more than a join: that part is not known
    let t = build(1, T0, end + 60_000, &samples, &logs);
    assert_eq!(joined(&t), vec![(State::Online, 0, 599_900), (State::Unknown, 599_900, 660_000)]);
    // nor is a log that stopped (no quiet close) followed by a gap
    let mut car = Car::new(1, T0);
    car.park(120);
    car.gap(600);
    car.park(60);
    let end = car.t;
    let (samples, logs) = car.finish();
    let t = build(1, T0, end, &samples, &logs);
    assert_eq!(
        joined(&t),
        vec![(State::Online, 0, 119_900), (State::Unknown, 119_900, 720_000), (State::Online, 720_000, 780_000)]
    );
}

#[test]
fn a_quiet_log_means_asleep_until_the_next_frames() {
    let mut car = Car::new(1, T0);
    car.drive(300, 30.0, 10.0);
    car.park(60);
    car.buses = vec!["can0".into()]; // parked, the chassis bus asleep
    car.park(60);
    car.sleep(7200);
    car.buses = vec!["can0".into(), "can1".into()];
    car.park(60);
    let end = car.t;
    let (samples, logs) = car.finish();
    let t = build(1, T0 - HOUR, end, &samples, &logs);
    assert_eq!(
        states(&t),
        vec![
            (State::Unknown, -3_600_000, 0), // no log before the window
            (State::Online, 0, 360_000),
            (State::Online, 360_000, 419_900),
            (State::Asleep, 419_900, 7_620_000), // from the last frame to the next
            (State::Online, 7_620_000, 7_680_000),
        ]
    );
    assert_eq!(t.states[1].buses, vec!["can0", "can1"]);
    assert_eq!(t.states[2].buses, vec!["can0"], "the chassis bus asleep: a parked car kept awake");
    assert!(t.states[3].buses.is_empty());
}

#[test]
fn a_recording_pause_is_unknown_even_after_a_quiet_log() {
    let mut car = Car::new(1, T0);
    car.park(60);
    car.sleep(3600);
    car.pause(1800, true); // the bridge didn't record the last half hour of that
    car.park(60);
    car.pause(600, false); // a pause of unknown length: back to the log before
    car.park(60);
    let end = car.t;
    let (samples, logs) = car.finish();
    let t = build(1, T0, end, &samples, &logs);
    assert_eq!(
        states(&t),
        vec![
            (State::Online, 0, 59_900),
            (State::Asleep, 59_900, 3_660_000),
            (State::Unknown, 3_660_000, 5_460_000), // the 1,800 s the bridge says it didn't record
            (State::Online, 5_460_000, 5_519_900),
            (State::Unknown, 5_519_900, 6_120_000), // no length given: back to the log before
            (State::Online, 6_120_000, 6_180_000),
        ]
    );
    // a hole the caller knows of (a log that failed its check) is unknown too, never asleep
    let hole = [(T0 + 1_000_000, T0 + 2_000_000)];
    let input = timeline::Input {
        car: 1,
        from_ms: T0,
        to_ms: end,
        samples: &samples,
        logs: &logs,
        holes: &hole,
        api: &[],
        marks: &[],
        order: Default::default(),
    };
    let with_hole = timeline::build(&input, &Params::default());
    assert_eq!(
        &states(&with_hole)[1..4],
        &[
            (State::Asleep, 59_900, 1_000_000),
            (State::Unknown, 1_000_000, 2_000_000),
            (State::Asleep, 2_000_000, 3_660_000)
        ]
    );
}

#[test]
fn input_order_does_not_matter() {
    let mut car = Car::new(7, T0);
    car.park(60);
    car.drive(400, 50.0, 15.0);
    car.park(300);
    car.charge(900, 11.0, ChargeKind::Ac);
    car.park(30);
    car.sleep(1800);
    car.park(60);
    let end = car.t;
    let (samples, logs) = car.finish();
    let want = build(7, T0, end, &samples, &logs);
    let mut shuffled = samples.clone();
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    for i in (1..shuffled.len()).rev() {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        shuffled.swap(i, (rng % (i as u64 + 1)) as usize);
    }
    let mut logs_rev = logs.clone();
    logs_rev.reverse();
    let got = build(7, T0, end, &shuffled, &logs_rev);
    assert_eq!(got, want);
    assert_eq!((want.drives.len(), want.charges.len()), (1, 1));
}

#[test]
fn ids_are_the_same_on_every_build_and_never_shared_between_cars() {
    let make = |id: u32| {
        let mut car = Car::new(id, T0);
        car.park(60);
        car.drive(300, 50.0, 15.0);
        car.park(120);
        car.charge(600, 7.0, ChargeKind::Ac);
        car.park(60);
        let end = car.t;
        let (samples, logs) = car.finish();
        build(id, T0, end, &samples, &logs)
    };
    let (a, b, other) = (make(1), make(1), make(2));
    assert_eq!(a, b);
    assert_eq!(a.drives[0].id, timeline::drive_id(1, a.drives[0].start_ms));
    assert_ne!(a.drives[0].id, other.drives[0].id, "same times, another car");
    assert_ne!(a.charges[0].id, other.charges[0].id);
    assert_eq!(other.drives[0].car, 2);
}

/// A day of a car: drives, a charge, sleeps, a lost log, a pause.
fn day() -> (Vec<model::Sample>, Vec<timeline::LogFacts>, u64) {
    let mut car = Car::new(3, T0);
    car.park(120);
    car.drive(900, 60.0, 18.0);
    car.stand(40);
    car.drive(600, 40.0, 12.0);
    car.park(200);
    car.sleep(5400);
    car.park(90);
    car.drive(1200, 80.0, 22.0);
    car.gap_driving(45, 80.0, 22.0); // a log lost mid-drive: still one drive
    car.drive(300, 80.0, 22.0);
    car.park(100);
    car.charge(1500, 7.0, ChargeKind::Ac);
    car.plugged(60, ChargeKind::Ac);
    car.charge(400, 7.0, ChargeKind::Ac);
    car.plugged(120, ChargeKind::Ac);
    car.sleep(3600);
    car.pause(900, true);
    car.park(60);
    car.drive(500, 50.0, 15.0);
    car.park(120);
    let end = car.t;
    let (samples, logs) = car.finish();
    (samples, logs, end)
}

#[test]
fn a_day_comes_out_whole() {
    let (samples, logs, end) = day();
    let t = build(3, T0, end, &samples, &logs);
    let drives: Vec<(u64, u64)> = t.drives.iter().map(|d| ((d.start_ms - T0) / 1000, (d.end_ms - T0) / 1000)).collect();
    assert_eq!(drives.len(), 3, "{drives:?}");
    // the drive with a lost log in it is one drive, 1,500 s of driving and the 45 s gap
    let d = &t.drives[1];
    assert_eq!((d.end_ms - d.start_ms) / 1000, 1545);
    assert!(close(d.distance_km.unwrap(), 1545.0 * 80.0 / 3600.0, 1e-6), "the odometer covers the gap");
    // the gap's energy comes from the lifetime counters
    assert!(close(d.energy_used_kwh.unwrap(), 1545.0 * 22.0 / 3600.0, 0.01), "{:?}", d.energy_used_kwh);
    assert_eq!(t.charges.len(), 1);
    assert_eq!(t.states.iter().filter(|s| s.state == State::Asleep).count(), 2);
}

/// The items of `old` outside the window, and `new`, by time.
fn merge<T: Clone>(
    old: &[T],
    new: &[T],
    span: impl Fn(&T) -> (u64, u64),
    outside: &impl Fn(u64, u64) -> bool,
) -> Vec<T> {
    let mut v: Vec<T> = old
        .iter()
        .filter(|x| {
            let (a, b) = span(x);
            outside(a, b)
        })
        .cloned()
        .chain(new.iter().cloned())
        .collect();
    v.sort_by_key(|x| span(x).0);
    v
}

/// What the store does with a rebuilt window: keeps what lies outside it, puts the window's in (the cut points strictly
/// between its ends).
fn put_window(old: &Timeline, part: &Timeline, from: u64, to: u64) -> Timeline {
    let outside = |t0: u64, t1: u64| t1 <= from || t0 >= to;
    let mut cuts: Vec<u64> =
        old.cuts.iter().copied().filter(|&c| c <= from || c >= to).chain(part.cuts.clone()).collect();
    cuts.sort_unstable();
    Timeline {
        states: merge(&old.states, &part.states, |s| (s.start_ms, s.end_ms), &outside),
        drives: merge(&old.drives, &part.drives, |d| (d.start_ms, d.end_ms), &outside),
        charges: merge(&old.charges, &part.charges, |c| (c.start_ms, c.end_ms), &outside),
        positions: merge(&old.positions, &part.positions, |p| (p.t_ms, p.t_ms + 1), &outside),
        charge_points: merge(&old.charge_points, &part.charge_points, |(_, p)| (p.t_ms, p.t_ms + 1), &outside),
        losses: part.losses.clone(),
        cuts,
    }
}

/// The window's own build: only the logs that overlap it, the one before it and the one after it, and the samples in it.
fn build_window(car: u32, from: u64, to: u64, samples: &[model::Sample], logs: &[timeline::LogFacts]) -> Timeline {
    let mut window_logs: Vec<_> = logs.iter().filter(|l| l.span().1 >= from && l.span().0 < to).cloned().collect();
    if let Some(prev) = logs.iter().filter(|l| l.span().1 < from).max_by_key(|l| l.span().1) {
        window_logs.push(prev.clone());
    }
    if let Some(next) = logs.iter().filter(|l| l.span().0 >= to).min_by_key(|l| l.span().0) {
        window_logs.push(next.clone());
    }
    let window_samples: Vec<_> = samples.iter().filter(|s| s.t_ms >= from && s.t_ms < to).cloned().collect();
    build(car, from, to, &window_samples, &window_logs)
}

fn assert_same(got: &Timeline, want: &Timeline, what: &str) {
    assert_eq!(got.states, want.states, "states, {what}");
    assert_eq!(got.drives, want.drives, "drives, {what}");
    assert_eq!(got.charges, want.charges, "charges, {what}");
    assert_eq!(got.positions, want.positions, "positions, {what}");
    assert_eq!(got.charge_points, want.charge_points, "charge points, {what}");
    assert_eq!(got.cuts, want.cuts, "cut points, {what}");
}

/// Each log of a car arriving last: the window it changes, rebuilt and put in, gives what building everything at once
/// gives. Returns the windows' lengths (ms) and how many were closed on both sides.
fn every_log_late(car: u32, samples: &[model::Sample], logs: &[timeline::LogFacts], end: u64) -> (Vec<u64>, usize) {
    let params = Params::default();
    let full = build(car, T0, end, samples, logs);
    let (mut widths, mut closed_both_sides) = (Vec::new(), 0);
    for late in 0..logs.len() {
        let (a, b) = logs[late].span();
        let others: Vec<_> = logs.iter().enumerate().filter(|(i, _)| *i != late).map(|(_, l)| l.clone()).collect();
        let without: Vec<_> = samples.iter().filter(|s| s.t_ms + 1000 <= a || s.t_ms > b).cloned().collect();
        let before = build(car, T0, end, &without, &others);
        let w = rebuild_window(&before.states, &before.cuts, a, b, &params);
        let (from, to) = (w.from_ms.max(T0), w.to_ms.unwrap_or(end));
        assert!(from <= a && to >= b, "window {from}..{to} misses the log {a}..{b}");
        widths.push(to - from);
        if w.from_ms > T0 && w.to_ms.is_some() {
            closed_both_sides += 1;
        }
        let part = build_window(car, from, to, samples, logs);
        let mut merged = put_window(&before, &part, from, to);
        merged.losses = full.losses.clone();
        assert_same(&merged, &full, &format!("late log {late}"));
    }
    (widths, closed_both_sides)
}

/// Rebuilding the window a late log changes gives what building everything at once gives.
#[test]
fn rebuilding_the_window_of_a_late_log_matches_a_full_build() {
    let (samples, logs, end) = day();
    let (widths, closed_both_sides) = every_log_late(3, &samples, &logs, end);
    assert!(widths.iter().all(|&w| w < end - T0), "every window is less than the whole history");
    assert!(closed_both_sides > 10, "{closed_both_sides} windows between two settled spans");
}

/// Five and a half hours of a car that never sleeps (Sentry mode at work, a charge, a lost log): no settled span at all.
fn awake_day() -> (Vec<model::Sample>, Vec<timeline::LogFacts>, u64) {
    let mut car = Car::new(4, T0);
    car.park(1800);
    car.drive(1200, 50.0, 15.0);
    car.stand(40);
    car.drive(600, 50.0, 15.0);
    car.park(5400);
    car.charge(1800, 11.0, ChargeKind::Ac);
    car.plugged(60, ChargeKind::Ac);
    car.charge(600, 11.0, ChargeKind::Ac);
    car.plugged(1200, ChargeKind::Ac);
    car.park(3600);
    car.gap(120);
    car.park(1800);
    car.drive(900, 60.0, 18.0);
    car.park(600);
    let end = car.t;
    let (samples, logs) = car.finish();
    (samples, logs, end)
}

/// A car that stays awake for hours still gets small windows: the idle cut points bound them where no settled span
/// does, and they never cut a drive or a charge.
#[test]
fn a_car_that_never_sleeps_gets_small_windows() {
    let (samples, logs, end) = awake_day();
    let params = Params::default();
    let full = build(4, T0, end, &samples, &logs);
    assert!(full.states.iter().all(|s| !timeline::is_settled(s, &params)), "nothing settled all day");
    assert_eq!(full.drives.len(), 2, "the stop at a light doesn't cut the drive");
    assert_eq!(full.charges.len(), 1, "nor does a one-minute pause cut the charge");
    assert!(full.cuts.len() > 30, "{} cut points", full.cuts.len());
    for c in &full.cuts {
        assert_eq!(c % 300_000, 0, "on the five-minute grid");
        let inside = |a: u64, b: u64| a < *c && *c < b;
        assert!(!full.drives.iter().any(|d| inside(d.start_ms, d.end_ms)), "a cut point in a drive");
        assert!(!full.charges.iter().any(|ch| inside(ch.start_ms, ch.end_ms)), "a cut point in a charge");
        assert!(full.states.iter().any(|s| s.start_ms == *c), "the states are split at {c}");
    }
    // without the cut points, every log would rebuild the whole day
    let (a, b) = logs[logs.len() / 2].span();
    assert_eq!(rebuild_window(&full.states, &[], a, b, &params), timeline::Window { from_ms: 0, to_ms: None });

    let (widths, _) = every_log_late(4, &samples, &logs, end);
    let widest = widths.iter().max().copied().unwrap_or(0);
    // the longest stretch without one: the charge with its pause and the margins around it
    assert!(widest <= 75 * 60_000, "widest window {} min", widest / 60_000);
}

/// Logs arriving one by one, in order: each rebuilds only the end of the timeline, and the timeline built that way is
/// the one built at once. The timeline is built up to "now", a moment (or two minutes, or a quarter of an hour: the
/// upload running late) after the newest log.
#[test]
fn logs_arriving_in_order_rebuild_only_the_end() {
    for (lag, car, (samples, logs, _)) in
        [1, 120_000, 900_000].into_iter().flat_map(|lag| [(lag, 3, day()), (lag, 4, awake_day())])
    {
        let params = Params::default();
        let mut cur = Timeline::default();
        let mut most = 0;
        for k in 0..logs.len() {
            let known = &logs[..=k];
            let now = logs[k].span().1 + lag;
            let (a, b) = logs[k].span();
            let w = rebuild_window(&cur.states, &cur.cuts, a, b, &params);
            let (from, to) = (w.from_ms.max(T0), w.to_ms.unwrap_or(now));
            let arrived: Vec<_> = samples.iter().filter(|s| s.t_ms <= b).cloned().collect();
            most = most.max(arrived.iter().filter(|s| s.t_ms >= from && s.t_ms < to).count());
            let part = build_window(car, from, to, &arrived, known);
            cur = put_window(&cur, &part, from, to);
        }
        let now = logs[logs.len() - 1].span().1 + lag;
        let full = build(car, T0, now, &samples, &logs);
        assert_same(&cur, &full, &format!("car {car} built log by log, {lag} ms behind"));
        // never the whole history (the day sleeps; the awake day has its cut points): at most the samples of the
        // longest stretch between two cut points or sleeps
        assert!(most <= 75 * 60, "car {car}: {most} samples in a window, of {}", samples.len());
    }
}
