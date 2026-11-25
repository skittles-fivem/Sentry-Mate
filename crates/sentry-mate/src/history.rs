//! `sentry-mate dev history` (hidden, and only in a build with the `dev-seed` feature; also the perf tests): a long
//! made-up history for one car, the size an owner's server has after importing years of TeslaMate data, so the
//! pages can be timed on it (docs/plans/performance.md). Everything is invented here, from a seed:
//!
//! - a **TeslaMate import** (through the importer's own sink, as a real backup's rows arrive): positions about every
//!   second while driving and every 15 s while charging (about 3 million over a year), the drives, the charges with
//!   their charge curves, the car's states and its software updates, two geofences;
//! - a **TeslaFi import** over the first weeks (a point a minute), overlapping the TeslaMate one;
//! - **TeslaFi's API** and **local TeslaMate** as live inputs over the last weeks (through the engine's own input,
//!   built into the timeline like a poll);
//! - the **Tesla API** as a live input over the same weeks, shaped like the real one ([`tesla_day`]): the stream's
//!   records about every second while driving, `vehicle_data` every 60 s while awake and every 30 s while charging,
//!   nothing while asleep, each value with the digits the API gives it, converted as `tesla::convert` does.
//!
//! SMO's CAN logs come from `dev seed` (run first, with `--start` near the end of this history): the import goes to
//! the made-up car it made, matched by its VIN.

use engine::Engine;
use engine::importer::{DbSink, Match};
use import::{
    Batch, ChargePointRow, ChargeRow, DriveRefs, DriveRow, PlaceRow, PositionRow, StateRow, TmCar, UpdateRow,
};
use import::{ChargeRefs, Sink};
use model::{Car, CarId, Charge, ChargeKind, ChargePoint, Drive, Gear, Input, Inputs, Position, Sample, Source};
use model::{State, StateSpan, Update};
use timeline::{ApiSpan, ApiState};

const S: u64 = 1000;
const MIN: u64 = 60 * S;
const H: u64 = 60 * MIN;
const DAY: u64 = 24 * H;

pub struct Options {
    /// Days of TeslaMate history, ending at `end_ms`.
    pub days: u32,
    /// The UTC midnight the history ends at (exclusive).
    pub end_ms: u64,
    pub seed: u64,
    /// Drives and charges over the whole history.
    pub drives: u32,
    pub charges: u32,
    /// Seconds between TeslaMate's positions while driving.
    pub drive_step_s: u64,
    /// Days the TeslaFi import covers (from the history's start).
    pub teslafi_days: u32,
    /// Days of TeslaFi's API and local TeslaMate, live, before `end_ms`.
    pub api_days: u32,
    /// Days of the Tesla API, live, before `end_ms`.
    pub tesla_days: u32,
    /// The Tesla API's car in sentry mode: awake all day, a `vehicle_data` answer every 15 to 30 s while parked.
    pub tesla_sentry: bool,
}

impl Options {
    /// The owner's size: about 12 months, 1,300 drives, 500 charges, about 3 million positions.
    #[cfg(test)]
    pub fn big(end_ms: u64) -> Options {
        Options {
            days: 330,
            end_ms,
            seed: 1,
            drives: 1300,
            charges: 500,
            drive_step_s: 1,
            teslafi_days: 60,
            api_days: 30,
            // (the pages' timings were taken without it)
            tesla_days: 0,
            tesla_sentry: false,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Counts {
    pub positions: u64,
    pub drives: u64,
    pub charges: u64,
    pub charge_points: u64,
    pub states: u64,
    pub updates: u64,
    pub teslafi_positions: u64,
    pub api_samples: u64,
    pub tesla_samples: u64,
}

/// A small deterministic generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// In `[0, 1)`.
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.f()
    }
}

/// The places drives go between: home first.
const SPOTS: [(f64, f64); 8] = [
    (47.6101, -122.2015),
    (47.6205, -122.3493),
    (47.5480, -122.0326),
    (47.6740, -122.1215),
    (47.4502, -122.3088),
    (47.7511, -122.2015),
    (47.5301, -122.1950),
    (47.6588, -122.3130),
];

fn km_between(a: (f64, f64), b: (f64, f64)) -> f64 {
    let dlat = (b.0 - a.0) * 111.2;
    let dlon = (b.1 - a.1) * 111.2 * a.0.to_radians().cos();
    (dlat * dlat + dlon * dlon).sqrt()
}

/// The car's running state through the made-up history.
struct Car0 {
    rng: Rng,
    at: usize,
    odo: f64,
    soc: f64,
}

const PACK_KWH: f64 = 75.0;
const RATED_KM_PER_PCT: f64 = 5.1;

impl Car0 {
    fn range(&self) -> f64 {
        self.soc * RATED_KM_PER_PCT
    }

    fn outside(&self, t: u64) -> f64 {
        // a year's swing and a day's
        let year = ((t % (365 * DAY)) as f64 / (365.0 * DAY as f64) * std::f64::consts::TAU).cos();
        let day = (((t % DAY) as f64 / DAY as f64 - 0.6) * std::f64::consts::TAU).cos();
        11.0 - 8.0 * year + 4.0 * day
    }

    fn sample(&self, t: u64, p: (f64, f64)) -> Sample {
        Sample {
            t_ms: t,
            lat: Some(p.0),
            lon: Some(p.1),
            elevation_m: Some(40.0 + 30.0 * (p.0 * 900.0).sin()),
            odometer_km: Some(self.odo),
            soc: Some(self.soc),
            range_km: Some(self.range()),
            ideal_range_km: Some(self.range() * 1.06),
            outside_temp_c: Some(self.outside(t)),
            inside_temp_c: Some(21.0),
            ..Sample::default()
        }
    }

    fn position(&self, t: u64, p: (f64, f64)) -> Position {
        import::rows::position_of(&self.sample(t, p))
    }
}

/// One made-up drive, charge or software update, as every input would see it.
enum Event {
    Drive { start: u64, mins: u64, to: usize },
    Charge { start: u64, mins: u64, dc: bool, target: f64 },
}

/// The events of the history `[from, to)`: `drives` drives and `charges` charges spread over its days (drives in
/// the day, AC charges in the evening, every eighth charge a DC one at midday), in time order.
fn plan(rng: &mut Rng, from: u64, to: u64, drives: u32, charges: u32) -> Vec<Event> {
    let days = ((to - from) / DAY).max(1);
    let mut out = Vec::new();
    for i in 0..u64::from(drives) {
        let day = from + i * days / u64::from(drives.max(1)) * DAY;
        let slot = i % 4;
        let start = day + 7 * H + slot * 3 * H + slot * 30 * MIN + (rng.f() * 30.0) as u64 * MIN;
        let mins = 10 + (rng.f() * 40.0) as u64;
        out.push(Event::Drive { start, mins, to: (rng.next() % SPOTS.len() as u64) as usize });
    }
    for j in 0..u64::from(charges) {
        let day = from + j * days / u64::from(charges.max(1)) * DAY;
        let dc = j % 8 == 7;
        let (start, mins) = match dc {
            true => (day + 12 * H + 20 * MIN, 25 + (rng.f() * 15.0) as u64),
            false => (day + 19 * H + (rng.f() * 40.0) as u64 * MIN, 60 + (rng.f() * 280.0) as u64),
        };
        out.push(Event::Charge { start, mins, dc, target: if j % 5 == 0 { 90.0 } else { 80.0 } });
    }
    out.sort_by_key(|e| match e {
        Event::Drive { start, .. } | Event::Charge { start, .. } => *start,
    });
    out.retain(|e| match e {
        Event::Drive { start, .. } | Event::Charge { start, .. } => *start < to,
    });
    out
}

/// What one input (an import or a live API) gets of the events: positions every `drive_step` while driving and
/// every `charge_step` while charging, and the rows.
#[derive(Default)]
struct Day {
    samples: Vec<Sample>,
    drives: Vec<Drive>,
    charges: Vec<Charge>,
    points: Vec<ChargePoint>,
    states: Vec<StateSpan>,
}

fn drive_of(c: &mut Car0, start: u64, mins: u64, to: usize, step: u64, out: &mut Day) {
    let from_spot = SPOTS[c.at];
    let to_spot = SPOTS[if to == c.at { (to + 1) % SPOTS.len() } else { to }];
    let end = start + mins * MIN;
    let km = km_between(from_spot, to_spot).max(2.0) * 1.3;
    let (odo0, soc0, range0) = (c.odo, c.soc, c.range());
    let start_pos = c.position(start, from_spot);
    let kwh = km * c.rng.range(0.14, 0.2);
    let n = ((end - start) / step).max(2);
    let mut vmax: f64 = 0.0;
    for k in 0..=n {
        let f = k as f64 / n as f64;
        let t = start + k * step;
        let p = (from_spot.0 + (to_spot.0 - from_spot.0) * f, from_spot.1 + (to_spot.1 - from_spot.1) * f);
        c.odo = odo0 + km * f;
        c.soc = (soc0 - kwh * f / PACK_KWH * 100.0).max(3.0);
        let v = if k == 0 || k == n { 0.0 } else { 35.0 + 60.0 * (f * 9.0).sin().abs() };
        vmax = vmax.max(v);
        let mut s = c.sample(t, p);
        s.speed_kmh = Some(v);
        s.gear = Some(if k == n { Gear::P } else { Gear::D });
        s.battery_power_kw = Some(if v > 0.0 { 8.0 + v * 0.25 } else { 0.0 });
        out.samples.push(s);
    }
    c.at = SPOTS.iter().position(|&s| s == to_spot).unwrap_or(0);
    out.drives.push(Drive {
        id: 0,
        car: 0,
        start_ms: start,
        end_ms: end,
        start: Some(start_pos),
        end: Some(c.position(end, to_spot)),
        start_place: None,
        end_place: None,
        start_address: None,
        end_address: None,
        start_odometer_km: Some(odo0),
        end_odometer_km: Some(c.odo),
        distance_km: Some(km),
        speed_max_kmh: Some(vmax),
        power_max_kw: Some(80.0),
        power_min_kw: Some(-40.0),
        energy_used_kwh: Some(kwh),
        start_soc: Some(soc0),
        end_soc: Some(c.soc),
        start_range_km: Some(range0),
        end_range_km: Some(c.range()),
        outside_temp_avg_c: Some(c.outside(start)),
        inside_temp_avg_c: Some(21.0),
        ascent_m: Some(km * 3.0),
        descent_m: Some(km * 2.8),
        source: Source::Teslamate,
        inputs: Inputs::NONE,
        rules_version: 0,
    });
    out.states.push(span(start - 2 * MIN, end + 5 * MIN, State::Online));
}

fn charge_of(c: &mut Car0, start: u64, mins: u64, dc: bool, target: f64, step: u64, out: &mut Day) {
    let spot = SPOTS[c.at];
    let (soc0, range0) = (c.soc, c.range());
    let target = target.max(soc0);
    let end = start + mins * MIN;
    let n = ((end - start) / step).max(2);
    let kw = if dc { 120.0 } else { 7.6 };
    let mut pmax: f64 = 0.0;
    for k in 0..=n {
        let f = k as f64 / n as f64;
        let t = start + k * step;
        c.soc = soc0 + (target - soc0) * f;
        let p = if k == n { 0.0 } else { kw * (1.0 - 0.5 * f * f64::from(dc)) };
        pmax = pmax.max(p);
        let mut s = c.sample(t, spot);
        s.gear = Some(Gear::P);
        s.speed_kmh = Some(0.0);
        s.charging = Some(k < n);
        s.charge_kind = Some(if dc { ChargeKind::Dc } else { ChargeKind::Ac });
        s.charge_power_kw = Some(p);
        s.battery_power_kw = Some(-p);
        out.samples.push(s);
        out.points.push(ChargePoint {
            t_ms: t,
            power_kw: Some(p),
            voltage_v: Some(if dc { 390.0 } else { 240.0 }),
            current_a: Some(if dc { p * 1000.0 / 390.0 } else { 32.0 }),
            soc: Some(c.soc),
            battery_temp_c: Some(25.0 + 10.0 * f),
            range_km: Some(c.range()),
        });
    }
    let added = (target - soc0) / 100.0 * PACK_KWH;
    out.charges.push(Charge {
        id: 0,
        car: 0,
        start_ms: start,
        end_ms: end,
        position: Some(c.position(start, spot)),
        place: None,
        address: None,
        kind: Some(if dc { ChargeKind::Dc } else { ChargeKind::Ac }),
        energy_added_kwh: Some(added),
        energy_used_kwh: Some(added * if dc { 1.04 } else { 1.1 }),
        energy_spans: None,
        start_soc: Some(soc0),
        end_soc: Some(c.soc),
        start_range_km: Some(range0),
        end_range_km: Some(c.range()),
        power_max_kw: Some(pmax),
        outside_temp_avg_c: Some(c.outside(start)),
        battery_temp_avg_c: Some(30.0),
        cost: None,
        source: Source::Teslamate,
        inputs: Inputs::NONE,
        rules_version: 0,
    });
    out.states.push(span(start - MIN, end + 5 * MIN, State::Online));
}

fn span(a: u64, b: u64, state: State) -> StateSpan {
    StateSpan {
        car: 0,
        state,
        start_ms: a,
        end_ms: b,
        buses: Vec::new(),
        source: Source::Teslamate,
        inputs: Inputs::NONE,
    }
}

/// `v` to `k` decimals, as a decimal column of a backup gives it.
fn decimals(v: &mut Option<f64>, k: i32) {
    let p = 10f64.powi(k);
    *v = v.map(|x| (x * p).round() / p);
}

/// A sample with the precision a TeslaMate backup's columns have (as the importer's made-up fixtures show them:
/// `12.301000`, a whole speed, power, battery level and elevation, `399.00` km, `12.5` degrees), so what is stored
/// is the size an owner's import is. The odometer stays as worked out: a backup's is a float with any digits.
fn as_stored(s: &mut Sample) {
    for v in [&mut s.lat, &mut s.lon] {
        decimals(v, 6);
    }
    for v in [&mut s.speed_kmh, &mut s.battery_power_kw, &mut s.soc, &mut s.elevation_m, &mut s.charge_power_kw] {
        decimals(v, 0);
    }
    for v in [&mut s.range_km, &mut s.ideal_range_km] {
        decimals(v, 2);
    }
    for v in [&mut s.outside_temp_c, &mut s.inside_temp_c] {
        decimals(v, 1);
    }
}

/// A charge point with a backup's precision (whole kW, volts, amperes and battery level, `377.15` km, `12.5` degrees).
fn point_as_stored(p: &mut ChargePoint) {
    for v in [&mut p.power_kw, &mut p.voltage_v, &mut p.current_a, &mut p.soc] {
        decimals(v, 0);
    }
    decimals(&mut p.range_km, 2);
    decimals(&mut p.battery_temp_c, 1);
}

/// As `tesla::convert` has it.
const KM_PER_MILE: f64 = 1.609_344;

/// A distance as the Tesla API gives it and `tesla::convert` reads it: miles written with `k` decimals, read as a
/// number, times the kilometres in a mile.
fn from_miles(km: Option<f64>, k: usize) -> Option<f64> {
    let text = format!("{:.k$}", km? / KM_PER_MILE);
    text.parse::<f64>().ok().map(|mi| mi * KM_PER_MILE)
}

/// A value written with `k` decimals and read back, as a JSON number is.
fn written(v: Option<f64>, k: usize) -> Option<f64> {
    format!("{:.k$}", v?).parse().ok()
}

/// One `vehicle_data` answer for the car as `s` has it, at `t`: what `tesla::convert::reading` makes of one. Miles
/// with the API's digits (the odometer 6, the ranges 2, a whole mph), whole kW, V, A and battery level, 0.1 °C,
/// the tyres in bar at the 0.025 bar the car measures in.
fn tesla_reading(rng: &mut Rng, s: &Sample, t: u64) -> Sample {
    let driving = matches!(s.gear, Some(Gear::D | Gear::R | Gear::N));
    let charging = s.charging == Some(true);
    let tyre = |rng: &mut Rng, base: f64| written(Some(base + (rng.next() % 3) as f64 * 0.025), 3);
    let tpms_bar = [tyre(rng, 2.875), tyre(rng, 2.875), tyre(rng, 2.85), tyre(rng, 2.85)];
    Sample {
        t_ms: t,
        lat: s.lat,
        lon: s.lon,
        // parked: the API's speed and gear are null
        speed_kmh: if driving { from_miles(s.speed_kmh, 0) } else { None },
        gear: if driving { s.gear } else { None },
        odometer_km: from_miles(s.odometer_km, 6),
        soc: written(s.soc, 0),
        range_km: from_miles(s.range_km, 2),
        ideal_range_km: from_miles(s.ideal_range_km, 2),
        battery_power_kw: written(s.battery_power_kw.or(Some(0.0)), 0),
        outside_temp_c: written(s.outside_temp_c, 1),
        inside_temp_c: written(s.inside_temp_c.map(|c| c + (rng.next() % 5) as f64 * 0.1), 1),
        charging: Some(charging),
        charge_kind: if charging { s.charge_kind } else { None },
        charge_power_kw: if charging { written(s.charge_power_kw, 0) } else { None },
        charger_voltage_v: if charging {
            Some(if s.charge_kind == Some(ChargeKind::Dc) { 390.0 } else { 238.0 + (rng.next() % 5) as f64 })
        } else {
            None
        },
        charger_current_a: if charging {
            Some(if s.charge_kind == Some(ChargeKind::Dc) { 0.0 } else { 32.0 })
        } else {
            None
        },
        tpms_bar,
        extra: model::SampleExtra { tires_mapped: Some(true), ..model::SampleExtra::default() },
        ..Sample::default()
    }
}

/// One record of the Tesla API's stream for the car as `s` has it: what `tesla::stream::parse_record` makes of one
/// (a whole mph, the odometer in miles with one decimal, whole battery level, metres, kW and miles of range).
fn tesla_record(s: &Sample) -> Sample {
    Sample {
        t_ms: s.t_ms,
        lat: s.lat,
        lon: s.lon,
        speed_kmh: from_miles(s.speed_kmh, 0),
        odometer_km: from_miles(s.odometer_km, 1),
        soc: written(s.soc, 0),
        elevation_m: written(s.elevation_m, 0),
        battery_power_kw: written(s.battery_power_kw, 0),
        gear: s.gear,
        range_km: from_miles(s.range_km, 0),
        ..Sample::default()
    }
}

/// A day as the Tesla API input gets it, from the day's samples made every second while driving and every 30 s
/// while charging: the stream's records while driving (a record missing now and then) with a `vehicle_data` answer
/// about every 60 s joined in, an answer about every 30 s while charging, and one every 60 s while the car is awake
/// and parked (the minutes before and after a drive or a charge); nothing while it sleeps. In sentry mode (`sentry`,
/// the day's start) the car is awake and parked all day outside its drives and charges, an answer every 15 to 30 s.
fn tesla_day(rng: &mut Rng, d: &Day, sentry: Option<u64>) -> Vec<Sample> {
    let mut out: std::collections::BTreeMap<u64, Sample> = std::collections::BTreeMap::new();
    let mut next_poll = 0u64;
    for s in &d.samples {
        if s.charging.is_some() {
            // a charge: answers only, a second early or late
            let t = s.t_ms + (rng.next() % 3) * S;
            out.insert(t, tesla_reading(rng, s, t));
            continue;
        }
        if s.t_ms >= next_poll {
            // the answer's values win over the record's of the same second; the record's elevation stays
            let mut r = tesla_reading(rng, s, s.t_ms);
            r.elevation_m = written(s.elevation_m, 0);
            out.insert(s.t_ms, r);
            next_poll = s.t_ms + 58 * S + (rng.next() % 5) * S;
        } else if !rng.next().is_multiple_of(20) {
            out.insert(s.t_ms, tesla_record(s));
        }
    }
    // awake and parked: the car as the nearest sample has it
    let spans: Vec<(u64, u64)> = match sentry {
        Some(day) => vec![(day, day + DAY)],
        None => d.states.iter().filter(|s| s.state == State::Online).map(|s| (s.start_ms, s.end_ms)).collect(),
    };
    let (near_ms, every) = if sentry.is_some() { (10 * S, 15) } else { (45 * S, 59) };
    for (a, b) in spans {
        let mut t = a - a % S;
        while t < b {
            let near = out.range(t.saturating_sub(near_ms)..t + near_ms).next().is_some();
            let i = d.samples.partition_point(|s| s.t_ms <= t);
            if let (false, Some(s)) = (near, d.samples.get(i.saturating_sub(1))) {
                let mut parked = s.clone();
                (parked.gear, parked.speed_kmh, parked.charging) = (None, None, None);
                parked.battery_power_kw = Some(0.0);
                out.insert(t, tesla_reading(rng, &parked, t));
            }
            t += if sentry.is_some() { every * S + (rng.next() % 16) * S } else { every * S + (rng.next() % 3) * S };
        }
    }
    out.into_values().collect()
}

/// Walks the events of `[from, to)` a day at a time (the car's state carried from day to day).
fn each_day(
    c: &mut Car0,
    events: &[Event],
    from: u64,
    to: u64,
    drive_step: u64,
    charge_step: u64,
    mut f: impl FnMut(u64, Day) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut i = 0;
    let mut day = from;
    while day < to {
        let mut d = Day::default();
        while let Some(e) = events.get(i) {
            match *e {
                Event::Drive { start, mins, to } if start < day + DAY => {
                    drive_of(c, start, mins, to, drive_step, &mut d)
                }
                Event::Charge { start, mins, dc, target } if start < day + DAY => {
                    charge_of(c, start, mins, dc, target, charge_step, &mut d)
                }
                _ => break,
            }
            i += 1;
        }
        // asleep between the spans the car was awake in
        let mut sleep = Vec::new();
        let mut at = day;
        for s in &d.states {
            if s.start_ms > at {
                sleep.push(span(at, s.start_ms, State::Asleep));
            }
            at = at.max(s.end_ms);
        }
        if at < day + DAY {
            sleep.push(span(at, day + DAY, State::Asleep));
        }
        d.states.extend(sleep);
        d.states.sort_by_key(|s| s.start_ms);
        d.samples.iter_mut().for_each(as_stored);
        d.points.iter_mut().for_each(point_as_stored);
        f(day, d)?;
        day += DAY;
    }
    Ok(())
}

fn sink_err(e: import::rows::SinkError) -> Box<dyn std::error::Error> {
    e.to_string().into()
}

/// One import of the events of `[from, to)` through the importer's sink (TeslaMate's or TeslaFi's), a day's rows
/// per batch, ids counting from 1.
#[allow(clippy::too_many_arguments)]
fn import_into(
    sink: &mut DbSink<'_>,
    car: &Car,
    c: &mut Car0,
    events: &[Event],
    from: u64,
    to: u64,
    drive_step: u64,
    charge_step: u64,
    with_places: bool,
    counts: &mut Counts,
    teslafi: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    sink.matches.insert(1, Match::Car(car.id));
    let tm = TmCar {
        tm_id: 1,
        vin: car.vin.clone(),
        name: Some(car.name.clone()),
        model: Some("Model Y".into()),
        trim_badging: Some("74D".into()),
        efficiency_wh_km: Some(150.0),
        ..TmCar::default()
    };
    let Some(target) = sink.car(&tm).map_err(sink_err)? else { return Err("the import left the car out".into()) };
    let id = target.car;
    if with_places {
        let place = |tm_id, name: &str, kind, (lat, lon): (f64, f64)| PlaceRow {
            tm_id,
            place: model::Place { id: 0, name: name.into(), kind, lat, lon, radius_m: 150.0, price: None },
            per_minute_price: None,
            session_fee: None,
            tm_radius_m: Some(150.0),
        };
        sink.places(vec![
            place(1, "Home", model::PlaceKind::Home, SPOTS[0]),
            place(2, "Work", model::PlaceKind::Work, SPOTS[1]),
        ])
        .map_err(sink_err)?;
    }
    let (mut pos_id, mut drive_id, mut charge_id, mut point_id, mut state_id) = (1i64, 1i64, 1i64, 1i64, 1i64);
    let mut updates = Vec::new();
    each_day(c, events, from, to, drive_step, charge_step, |day, d| {
        let n = d.samples.len() as u64;
        if teslafi {
            counts.teslafi_positions += n;
        } else {
            counts.positions += n;
        }
        // positions in batches of the importer's size
        for chunk in d.samples.chunks(20_000) {
            let rows = chunk
                .iter()
                .map(|s| {
                    pos_id += 1;
                    PositionRow { tm_id: pos_id, tm_drive_id: None, sample: s.clone(), usable_soc: s.soc }
                })
                .collect();
            sink.batch(id, Batch::Positions(rows)).map_err(sink_err)?;
        }
        if !d.drives.is_empty() {
            counts.drives += d.drives.len() as u64;
            let rows = d
                .drives
                .into_iter()
                .map(|drive| {
                    drive_id += 1;
                    DriveRow { tm_id: drive_id, drive, tm: DriveRefs::default() }
                })
                .collect();
            sink.batch(id, Batch::Drives(rows)).map_err(sink_err)?;
        }
        if !d.charges.is_empty() {
            counts.charges += d.charges.len() as u64;
            let rows = d
                .charges
                .into_iter()
                .map(|charge| {
                    charge_id += 1;
                    ChargeRow { tm_id: charge_id, charge, tm: ChargeRefs::default() }
                })
                .collect();
            sink.batch(id, Batch::Charges(rows)).map_err(sink_err)?;
            counts.charge_points += d.points.len() as u64;
            let rows = d
                .points
                .into_iter()
                .map(|point| {
                    point_id += 1;
                    ChargePointRow {
                        tm_id: point_id,
                        tm_charge_id: charge_id,
                        point,
                        fast_charger: None,
                        phases: None,
                        energy_added_kwh: None,
                        outside_temp_c: None,
                        ideal_range_km: None,
                        usable_soc: None,
                    }
                })
                .collect();
            sink.batch(id, Batch::ChargePoints(rows)).map_err(sink_err)?;
        }
        counts.states += d.states.len() as u64;
        let rows = d
            .states
            .into_iter()
            .map(|span| {
                state_id += 1;
                StateRow { tm_id: state_id, span, offline: false }
            })
            .collect();
        sink.batch(id, Batch::States(rows)).map_err(sink_err)?;
        // a software update every six weeks, at 2 in the night
        let n_day = (day - from) / DAY;
        if n_day.is_multiple_of(42) {
            let k = updates.len() as i64;
            updates.push(UpdateRow {
                tm_id: k + 1,
                update: Update {
                    car: id,
                    start_ms: day + 2 * H,
                    end_ms: Some(day + 2 * H + 25 * MIN),
                    version: format!("2026.{}.{}", 2 + k * 4, 3 + k % 3),
                    source: Source::Teslamate,
                    inputs: Inputs::NONE,
                },
            });
        }
        Ok(())
    })?;
    counts.updates += updates.len() as u64;
    if !updates.is_empty() {
        sink.batch(id, Batch::Updates(updates)).map_err(sink_err)?;
    }
    sink.finish();
    Ok(())
}

/// The made-up car to give the history to: the one `dev seed` made (the first car), else a new one.
fn the_car(e: &Engine) -> Result<Car, Box<dyn std::error::Error>> {
    let page = e.db().fleet(&db::FleetQuery { search: None, limit: 1, after: None })?;
    if let Some(c) = page.cars.into_iter().next() {
        return Ok(c.car);
    }
    Ok(e.db().create_car(Car {
        id: 0,
        name: "Made-up Model Y".into(),
        vehicle_keys: Vec::new(),
        preferred_key: None,
        vin: Some(model::VinInfo {
            vin_prefix: "7SAYGDEE1PF".into(),
            model: Some('Y'),
            year: Some(2023),
            plant: Some('F'),
            via: 0x405,
        }),
        profile_setting: None,
        time_zone: "America/Los_Angeles".into(),
        time_zone_typed: Some(true),
        efficiency_wh_km: None,
        created_ms: 1_767_225_600_000,
        inputs: Default::default(),
    })?)
}

/// Writes the history (the engine started; waits until it built what the live inputs brought).
pub async fn run(e: &Engine, o: &Options) -> Result<Counts, Box<dyn std::error::Error>> {
    // the SMO logs a `dev seed` put in the data folder are decoded first: the import goes to the car they made
    e.idle().await;
    let car = the_car(e)?;
    let from = o.end_ms - u64::from(o.days) * DAY;
    let mut counts = Counts::default();
    let mut rng = Rng(o.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let events = plan(&mut rng, from, o.end_ms, o.drives, o.charges);
    let start = |seed: u64| Car0 { rng: Rng(seed | 1), at: 0, odo: 12_000.0, soc: 70.0 };
    {
        let mut sink = DbSink::new(e);
        let mut c = start(o.seed);
        import_into(
            &mut sink,
            &car,
            &mut c,
            &events,
            from,
            o.end_ms,
            o.drive_step_s * S,
            15 * S,
            true,
            &mut counts,
            false,
        )?;
    }
    if o.teslafi_days > 0 {
        let mut sink = DbSink::teslafi(e);
        let mut c = start(o.seed + 1);
        let to = (from + u64::from(o.teslafi_days) * DAY).min(o.end_ms);
        import_into(&mut sink, &car, &mut c, &events, from, to, MIN, MIN, false, &mut counts, true)?;
    }
    if o.api_days > 0 {
        let a = o.end_ms.saturating_sub(u64::from(o.api_days) * DAY).max(from);
        for (input, step) in [(Input::TeslafiApi, MIN), (Input::TeslamateLocal, 5 * S)] {
            let mut c = start(o.seed + 2);
            each_day(&mut c, &events, a, o.end_ms, step, step, |_, d| {
                counts.api_samples += d.samples.len() as u64;
                let facts = d
                    .states
                    .iter()
                    .filter(|s| s.state == State::Online)
                    .map(|s| ApiSpan {
                        start_ms: s.start_ms,
                        end_ms: s.end_ms,
                        state: ApiState::Online,
                        data: true,
                        inputs: Inputs::NONE,
                    })
                    .collect();
                e.feed_points(input, car.id, d.samples, facts);
                Ok(())
            })?;
            e.idle().await;
        }
    }
    if o.tesla_days > 0 {
        let a = o.end_ms.saturating_sub(u64::from(o.tesla_days) * DAY).max(from);
        let mut c = start(o.seed + 2);
        let mut rng = Rng(o.seed.wrapping_mul(0xD1B5_4A32_D192_ED03) | 1);
        each_day(&mut c, &events, a, o.end_ms, S, 30 * S, |day, d| {
            let samples: Vec<Sample> = tesla_day(&mut rng, &d, o.tesla_sentry.then_some(day))
                .into_iter()
                .filter(|s| (day..day + DAY).contains(&s.t_ms))
                .collect();
            counts.tesla_samples += samples.len() as u64;
            let online: Vec<(u64, u64)> = if o.tesla_sentry {
                vec![(day, day + DAY)]
            } else {
                d.states.iter().filter(|s| s.state == State::Online).map(|s| (s.start_ms, s.end_ms)).collect()
            };
            let facts = online
                .into_iter()
                .map(|(start_ms, end_ms)| ApiSpan {
                    start_ms,
                    end_ms,
                    state: ApiState::Online,
                    data: true,
                    inputs: Inputs::NONE,
                })
                .collect();
            e.feed_points(Input::TeslaApi, car.id, samples, facts);
            Ok(())
        })?;
        e.idle().await;
    }
    e.idle().await;
    let _: CarId = car.id;
    Ok(counts)
}
