//! Synthetic but realistic car days for tests and measurements: 1 Hz samples shaped like a real car's (speed curves
//! with stops, slowly moving SOC, noisy pack current, values quantized the way the car's DBC factors make them).
//! Parked values come from a decoded parked minute when `SENTRY_MATE_PARKED_CSV` names one (a `sentry-mate log decode`
//! file kept outside the repo: real car data is never committed), else from built-in stand-ins. Positions are made
//! up around an arbitrary point.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use model::{ChargeKind, ChargePoint, Gear, Sample};
use std::collections::BTreeMap;

pub const DAY_MS: u64 = 86_400_000;
/// 2026-11-16 00:00 UTC.
pub const DAY0: u64 = 1_794_787_200_000;
/// The variable naming a decoded parked minute to take parked values from (none set: the stand-ins).
pub const PARKED_CSV_VAR: &str = "SENTRY_MATE_PARKED_CSV";

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn u64(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn f64(&mut self) -> f64 {
        (self.u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Roughly normal, mean 0, sd 1.
    pub fn normal(&mut self) -> f64 {
        (0..12).map(|_| self.f64()).sum::<f64>() - 6.0
    }
    pub fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[(self.u64() % v.len() as u64) as usize]
    }
}

/// A value as a DBC signal gives it: raw integer × factor + offset, in f64.
pub fn dbc(v: f64, factor: f64, offset: f64) -> f64 {
    ((v - offset) / factor).round() * factor + offset
}

/// One real parked minute, per signal: the value at each second (last in the second, carried forward) and how often
/// it changed from one second to the next.
pub struct RealParked {
    pub per_second: BTreeMap<String, Vec<f64>>,
}

impl RealParked {
    pub fn load() -> Option<RealParked> {
        let text = std::fs::read_to_string(std::env::var_os(PARKED_CSV_VAR)?).ok()?;
        let mut by: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
        for line in text.lines().skip(1) {
            let mut f = line.split(',');
            let (Some(t), Some(name), Some(v)) = (f.next(), f.next(), f.next()) else { continue };
            let (Ok(t), Ok(v)) = (t.parse::<f64>(), v.parse::<f64>()) else { continue };
            by.entry(name.to_string()).or_default().push((t, v));
        }
        let t0 = by.values().flat_map(|v| v.iter().map(|p| p.0)).fold(f64::MAX, f64::min).floor();
        let mut per_second = BTreeMap::new();
        for (name, vals) in by {
            let mut secs = vec![f64::NAN; 62];
            for (t, v) in vals {
                let s = ((t - t0) as usize).min(61);
                secs[s] = v;
            }
            let mut last = secs.iter().copied().find(|v| !v.is_nan()).unwrap_or(0.0);
            for s in secs.iter_mut() {
                if s.is_nan() {
                    *s = last;
                }
                last = *s;
            }
            per_second.insert(name, secs);
        }
        Some(RealParked { per_second })
    }

    /// A noisy signal: the real per-second values and change rate.
    fn pool(&self, name: &str) -> Option<(Vec<f64>, f64)> {
        let s = self.per_second.get(name)?;
        let changes = s.windows(2).filter(|w| w[0].to_bits() != w[1].to_bits()).count();
        Some((s.clone(), changes as f64 / (s.len() - 1) as f64))
    }
}

/// Noise drawn from a pool of values at a change rate.
struct Noisy {
    pool: Vec<f64>,
    rate: f64,
    cur: f64,
}

impl Noisy {
    fn new(pool: Vec<f64>, rate: f64) -> Noisy {
        let cur = pool[0];
        Noisy { pool, rate, cur }
    }
    fn next(&mut self, rng: &mut Rng) -> f64 {
        if rng.f64() < self.rate {
            self.cur = *rng.pick(&self.pool);
        }
        self.cur
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Drive { minutes: u32, highway: bool },
    ParkedAwake,
    Charging { kw: f64 },
}

/// The car's slow state, carried from one second to the next.
struct Car {
    rng: Rng,
    soc: f64,
    odo: f64,
    lat: f64,
    lon: f64,
    heading: f64,
    elev: f64,
    speed: f64,
    acc: f64,
    batt_temp: f64,
    out_temp: f64,
    in_temp: f64,
    total_in: f64,
    total_out: f64,
    tpms: [f64; 4],
    parked: Vec<(&'static str, Noisy)>,
}

const PACK_KWH: f64 = 75.0;

impl Car {
    fn new(seed: u64, real: Option<&RealParked>) -> Car {
        let mut rng = Rng::new(seed);
        let mut parked = Vec::new();
        let real_names = [
            ("current", "SmoothBattCurrent132"),
            ("volt_noise", "BattVoltage132"),
            ("line_v", "ChargeLineVoltage264"),
            ("line_kw", "ChargeLinePower264"),
            ("line_a", "ChargeLineCurrent264"),
            ("lv", "v12vBattVoltage261"),
        ];
        for (k, name) in real_names {
            let n = match real.and_then(|r| r.pool(name)) {
                Some((pool, rate)) => Noisy::new(pool, rate.max(0.02)),
                None => {
                    // made-up stand-ins: round values, with the DBC steps and change rates of a parked car
                    let (base, step, factor, rate): (f64, f64, f64, f64) = match k {
                        "current" => (0.0, 0.1, 0.1, 0.6),
                        "volt_noise" => (380.0, 0.01, 0.01, 0.5),
                        "line_v" => (240.0, 0.0333, 0.0333, 0.9),
                        "line_kw" => (0.7, 0.1, 0.1, 0.3),
                        "line_a" => (3.0, 1.0, 1.0, 0.1),
                        _ => (13.8, 0.000_544, 0.000_544, 0.9),
                    };
                    let pool = (0..9).map(|i| dbc(base + (i as f64 - 4.0) * step, factor, 0.0)).collect();
                    Noisy::new(pool, rate)
                }
            };
            parked.push((k, n));
        }
        let soc = 55.0 + rng.f64() * 30.0;
        Car {
            soc,
            odo: 20_000.0 + rng.f64() * 30_000.0,
            lat: 10.0 + rng.f64() * 0.1,
            lon: 20.0 + rng.f64() * 0.1,
            heading: rng.f64() * std::f64::consts::TAU,
            elev: 200.0 + rng.f64() * 100.0,
            speed: 0.0,
            acc: 3.0,
            batt_temp: 15.0,
            out_temp: 5.0 + rng.f64() * 10.0,
            in_temp: 12.0,
            total_in: 25_000.0 + rng.f64() * 1000.0,
            total_out: 24_000.0 + rng.f64() * 1000.0,
            tpms: [2.9, 2.9, 2.875, 2.9],
            parked,
            rng,
        }
    }

    fn parked(&mut self, k: &str) -> f64 {
        let rng = &mut self.rng;
        self.parked.iter_mut().find(|(n, _)| *n == k).map(|(_, n)| n.next(rng)).unwrap_or(0.0)
    }

    /// A parked value less the first one its pool has: how far it wandered.
    fn parked_wander(&mut self, k: &str) -> f64 {
        let rng = &mut self.rng;
        self.parked.iter_mut().find(|(n, _)| *n == k).map(|(_, n)| n.next(rng) - n.pool[0]).unwrap_or(0.0)
    }

    fn sample(&mut self, t_ms: u64, mode: Mode, sec_in_mode: u32) -> (Sample, Option<ChargePoint>) {
        let current;
        let mut charge_kw = None;
        let gear;
        match mode {
            Mode::Drive { minutes, highway } => {
                gear = Gear::D;
                let left = minutes * 60 - sec_in_mode;
                let cruise = if highway { 108.0 } else { 52.0 };
                // stops at lights every few minutes in town, slowing at the end
                let stop = !highway && (sec_in_mode % 240) > 215;
                let target = if left < 40 || stop { 0.0 } else { cruise + 6.0 * (sec_in_mode as f64 / 90.0).sin() };
                let dv = (target - self.speed).clamp(-9.0, 7.0) + self.rng.normal() * 0.6;
                self.speed = (self.speed + dv).max(0.0);
                let ms = self.speed / 3.6;
                self.heading += self.rng.normal() * 0.02 + if sec_in_mode.is_multiple_of(300) { 1.2 } else { 0.0 };
                self.lat += ms * self.heading.cos() / 111_320.0;
                self.lon += ms * self.heading.sin() / (111_320.0 * self.lat.to_radians().cos());
                self.elev += self.rng.normal() * 0.3 + (sec_in_mode as f64 / 200.0).sin() * 0.2;
                self.odo += ms / 1000.0;
                let kw = 0.03 * self.speed + 0.0008 * self.speed.powi(2) + dv.max(-6.0) * 4.0 + self.rng.normal() * 1.5;
                let v = 340.0 + self.soc * 0.75;
                current = dbc(kw * 1000.0 / v, 0.1, 0.0);
                self.soc -= kw / 3600.0 / PACK_KWH * 100.0;
                self.total_out += kw.max(0.0) / 3600.0;
                self.batt_temp += 0.002;
                if sec_in_mode % 600 == 599 {
                    self.tpms[(sec_in_mode / 600 % 4) as usize] += 0.025;
                }
                self.acc = (self.acc + self.rng.normal() * 0.1).clamp(1.0, 9.0);
            }
            Mode::ParkedAwake => {
                gear = Gear::P;
                self.speed = 0.0;
                current = self.parked("current");
            }
            Mode::Charging { kw } => {
                gear = Gear::P;
                self.speed = 0.0;
                let taper = if self.soc > 80.0 { (100.0 - self.soc) / 20.0 } else { 1.0 };
                let p = dbc(kw * taper + self.rng.normal() * 0.05, 0.1, 0.0);
                let v = 340.0 + self.soc * 0.75;
                current = dbc(-p * 1000.0 / v, 0.1, 0.0);
                self.soc += p * 0.92 / 3600.0 / PACK_KWH * 100.0;
                self.total_in += p * 0.92 / 3600.0;
                charge_kw = Some(p);
            }
        }
        if !matches!(mode, Mode::Drive { .. }) {
            self.out_temp += self.rng.normal() * 0.002;
        }
        self.in_temp += (21.0 - self.in_temp) * 0.001;
        // the pack's voltage follows SOC and sags with current; parked, it wanders around its pool's first value
        let wander = match mode {
            Mode::ParkedAwake => self.parked_wander("volt_noise"),
            _ => self.rng.normal() * 0.02,
        };
        let volt = dbc(340.0 + self.soc * 0.75 - current * 0.04 + wander, 0.01, 0.0);
        let charging = charge_kw.is_some();
        let s = Sample {
            t_ms,
            lat: Some(dbc(self.lat, 1e-6, 0.0)),
            lon: Some(dbc(self.lon, 1e-6, 0.0)),
            gps_accuracy_m: Some(dbc(self.acc, 0.2, 0.0)),
            elevation_m: Some(self.elev.round()),
            speed_kmh: Some(dbc(self.speed, 0.08, -40.0)),
            gear: Some(gear),
            odometer_km: Some(dbc(self.odo, 0.001, 0.0)),
            soc: Some(self.soc.round()),
            soc_bms: Some(dbc(self.soc, 0.1, 0.0)),
            range_km: Some((self.soc * 3.3).round() * 1.609_344),
            ideal_range_km: Some((self.soc * 3.6).round() * 1.609_344),
            energy_remaining_kwh: Some(dbc(self.soc / 100.0 * PACK_KWH, 0.02, 0.0)),
            battery_voltage_v: Some(volt),
            battery_current_a: Some(current),
            battery_power_kw: Some(volt * current / 1000.0),
            battery_temp_c: Some(dbc(self.batt_temp, 0.5, -40.0)),
            outside_temp_c: Some(dbc(self.out_temp, 0.5, -40.0)),
            inside_temp_c: Some(dbc(self.in_temp, 0.1, -40.0)),
            charging: Some(charging),
            charge_kind: charging.then_some(ChargeKind::Ac),
            charge_power_kw: charge_kw,
            charger_voltage_v: charging.then(|| self.parked("line_v")),
            charger_current_a: charging.then(|| dbc(charge_kw.unwrap_or(0.0) * 1000.0 / 240.0, 1.0, 0.0)),
            total_charge_kwh: Some(dbc(self.total_in, 0.001, 0.0)),
            total_discharge_kwh: Some(dbc(self.total_out, 0.001, 0.0)),
            lv_voltage_v: Some(self.parked("lv")),
            tpms_bar: self.tpms.map(|p| Some(dbc(p, 0.025, 0.0))),
        };
        let cp = charge_kw.map(|p| ChargePoint {
            t_ms,
            power_kw: Some(p),
            voltage_v: s.battery_voltage_v,
            current_a: s.battery_current_a,
            soc: s.soc_bms,
            battery_temp_c: s.battery_temp_c,
            range_km: s.range_km,
        });
        (s, cp)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A commute day: an overnight AC charge, three drives, some time parked awake; asleep otherwise (~5.5 h of samples).
    Driving,
    /// Parked and mostly asleep: six short wake-ups (1 h of samples).
    ParkedAsleep,
    /// Parked and awake all day (Sentry mode, keeping the pack warm): 86,400 samples, the most a day can hold.
    ParkedAwake,
}

/// One day of a car: (1 Hz samples, charge curve points).
pub fn day(shape: Shape, day_start_ms: u64, seed: u64, real: Option<&RealParked>) -> (Vec<Sample>, Vec<ChargePoint>) {
    let mut car = Car::new(seed, real);
    let h = 3600u32;
    let plan: Vec<(u32, u32, Mode)> = match shape {
        Shape::Driving => vec![
            (h, 4 * h, Mode::Charging { kw: 7.6 }),
            (7 * h + 1800, 7 * h + 1800 + 35 * 60, Mode::Drive { minutes: 35, highway: true }),
            (8 * h + 300, 8 * h + 900, Mode::ParkedAwake),
            (12 * h + 600, 12 * h + 600 + 15 * 60, Mode::Drive { minutes: 15, highway: false }),
            (12 * h + 1500, 12 * h + 3300, Mode::ParkedAwake),
            (17 * h + 1200, 17 * h + 1200 + 40 * 60, Mode::Drive { minutes: 40, highway: false }),
            (18 * h, 18 * h + 1200, Mode::ParkedAwake),
        ],
        Shape::ParkedAsleep => (0..6).map(|i| (i * 4 * h + 1000, i * 4 * h + 1600, Mode::ParkedAwake)).collect(),
        Shape::ParkedAwake => vec![(0, 24 * h, Mode::ParkedAwake)],
    };
    let (mut samples, mut points) = (Vec::new(), Vec::new());
    for (from, to, mode) in plan {
        for s in from..to {
            let (sample, cp) = car.sample(day_start_ms + u64::from(s) * 1000, mode, s - from);
            samples.push(sample);
            points.extend(cp);
        }
    }
    (samples, points)
}

// ---- db helpers ----

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct TestDb {
    pub dir: tempfile::TempDir,
    pub data: PathBuf,
    pub keys: PathBuf,
    pub vault: Arc<vault::Vault>,
    pub db: db::Db,
}

pub fn vault_in(t: &Path) -> (PathBuf, PathBuf, Arc<vault::Vault>) {
    let (data, keys) = (t.join("data"), t.join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    (data, keys, Arc::new(v))
}

pub fn test_db(opts: db::Options) -> TestDb {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys, vault) = vault_in(dir.path());
    let db = db::Db::open(&data, vault.clone(), opts).unwrap();
    TestDb { dir, data, keys, vault, db }
}

impl TestDb {
    /// Opens the same data folder again (as after a restart).
    pub fn reopen(&mut self, opts: db::Options) {
        self.db = db::Db::open(&self.data, self.vault.clone(), opts).unwrap();
    }

    pub fn try_reopen(&self, opts: db::Options) -> db::Result<db::Db> {
        db::Db::open(&self.data, self.vault.clone(), opts)
    }
}

pub fn new_car(name: &str, keys: &[&str]) -> model::Car {
    model::Car {
        id: 0,
        name: name.to_string(),
        vehicle_keys: keys.iter().map(|k| k.to_string()).collect(),
        preferred_key: None,
        vin: None,
        profile_setting: None,
        time_zone: "UTC".into(),
        efficiency_wh_km: None,
        created_ms: DAY0,
    }
}

pub fn drive(car: model::CarId, id: u64, start_ms: u64, minutes: u64, km: f64, source: model::Source) -> model::Drive {
    model::Drive {
        id,
        car,
        start_ms,
        end_ms: start_ms + minutes * 60_000,
        start: None,
        end: None,
        start_place: None,
        end_place: None,
        start_address: None,
        end_address: None,
        start_odometer_km: Some(1000.0),
        end_odometer_km: Some(1000.0 + km),
        distance_km: Some(km),
        speed_max_kmh: Some(110.0),
        power_max_kw: Some(120.0),
        power_min_kw: Some(-40.0),
        energy_used_kwh: Some(km * 0.16),
        start_soc: Some(80.0),
        end_soc: Some(70.0),
        start_range_km: None,
        end_range_km: None,
        outside_temp_avg_c: Some(5.5),
        inside_temp_avg_c: Some(21.0),
        ascent_m: None,
        descent_m: None,
        source,
        inputs: model::Inputs::NONE,
        rules_version: 1,
    }
}

pub fn charge(
    car: model::CarId,
    id: u64,
    start_ms: u64,
    minutes: u64,
    kwh: f64,
    source: model::Source,
) -> model::Charge {
    model::Charge {
        id,
        car,
        start_ms,
        end_ms: start_ms + minutes * 60_000,
        position: None,
        place: None,
        address: None,
        kind: Some(model::ChargeKind::Ac),
        energy_added_kwh: Some(kwh),
        energy_used_kwh: Some(kwh * 1.1),
        start_soc: Some(40.0),
        end_soc: Some(80.0),
        start_range_km: None,
        end_range_km: None,
        power_max_kw: Some(11.0),
        outside_temp_avg_c: None,
        battery_temp_avg_c: None,
        cost: None,
        source,
        inputs: model::Inputs::NONE,
        rules_version: 1,
    }
}

pub fn state(
    car: model::CarId,
    id: u64,
    start_ms: u64,
    end_ms: u64,
    s: model::State,
    source: model::Source,
) -> db::StateRow {
    db::StateRow {
        id,
        span: model::StateSpan {
            car,
            state: s,
            start_ms,
            end_ms,
            buses: vec!["can0".into()],
            source,
            inputs: model::Inputs::NONE,
        },
    }
}

/// Every file under a folder, temporary ones included.
pub fn all_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// A few 1 Hz samples starting at `t0`.
pub fn samples(t0: u64, n: u64) -> Vec<Sample> {
    (0..n)
        .map(|i| Sample {
            t_ms: t0 + i * 1000,
            lat: Some(10.0 + i as f64 * 1e-5),
            lon: Some(20.0),
            speed_kmh: Some((i % 90) as f64),
            gear: Some(Gear::D),
            soc: Some(70.0),
            battery_voltage_v: Some(dbc(400.0 + (i % 13) as f64 * 0.01, 0.01, 0.0)),
            battery_current_a: Some(dbc((i % 40) as f64, 0.1, 0.0)),
            ..Sample::default()
        })
        .map(|mut s| {
            s.battery_power_kw = Some(s.battery_voltage_v.unwrap() * s.battery_current_a.unwrap() / 1000.0);
            s
        })
        .collect()
}
