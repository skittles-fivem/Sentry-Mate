//! Runs the plan: the car's state every 100 ms while it is awake, and the spans it sleeps. What really happened (the
//! drives, the charges, the sleeps) is kept as the [`Truth`] the tests compare Sentry Mate's timeline with.

use crate::car::{CarModel, CarState, ChargeStatus, dc_curve_kw, elevation_at};
use crate::geo::Route;
use crate::plan::{Activity, DAY_MS, Plan, When};
use crate::rng::Rng;
use model::{ChargeKind, Gear};

/// The step while awake, ms.
pub const STEP_MS: u64 = 100;
/// The car stays awake this long after it was last used, then sleeps.
pub const IDLE_AWAKE_MS: u64 = 10 * 60_000;
/// It wakes this long before a drive (the driver walks up, the phone key wakes it).
pub const WAKE_LEAD_MS: u64 = 90_000;

pub const CAN0: u8 = 1;
pub const CAN1: u8 = 2;
pub const CAN3: u8 = 8;
pub const ALL_BUSES: u8 = CAN0 | CAN1 | CAN3;

/// A drive as it happened.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrueDrive {
    /// Gear out of P, and back in P.
    pub start_ms: u64,
    pub end_ms: u64,
    pub from: String,
    pub to: String,
    pub distance_km: f64,
    pub start_soc: f64,
    pub end_soc: f64,
    pub max_speed_kmh: f64,
    pub start_odometer_km: f64,
    pub end_odometer_km: f64,
}

/// A charge as it happened (pauses inside it included).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrueCharge {
    pub start_ms: u64,
    pub end_ms: u64,
    pub kind: ChargeKind,
    pub place: String,
    pub start_soc: f64,
    pub end_soc: f64,
    /// Into the pack, kWh.
    pub energy_added_kwh: f64,
    pub max_power_kw: f64,
    pub pauses: u32,
}

/// What the scenario made happen.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Truth {
    pub drives: Vec<TrueDrive>,
    pub charges: Vec<TrueCharge>,
    /// The car asleep (no frames): `[from, to)`.
    pub sleeps: Vec<(u64, u64)>,
    pub updates: Vec<(u64, u64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// `[t_ms, t_ms + STEP_MS)` awake, sending on these buses (none: modules restarting).
    Awake { t_ms: u64, buses: u8 },
    /// Asleep over `[from_ms, to_ms)`.
    Asleep { from_ms: u64, to_ms: u64 },
}

enum DrivePhase {
    Reverse { until: u64 },
    Go,
    Wait { until: u64 },
}

enum Running {
    Drive {
        route: Route,
        km: f64,
        phase: DrivePhase,
        /// Stops already made, km along.
        passed: f64,
        truth: TrueDrive,
        noise: f64,
    },
    Charge {
        kind: ChargeKind,
        kw: f64,
        limit: f64,
        pause: Option<(u64, u64)>,
        began: u64,
        deadline: u64,
        truth: TrueCharge,
        charging: bool,
    },
    Update {
        start: u64,
        len: u64,
        silences: Vec<(u64, u64)>,
    },
}

pub struct Sim {
    model: CarModel,
    plan: Plan,
    next: usize,
    state: CarState,
    t_ms: u64,
    end_ms: u64,
    rng: Rng,
    running: Option<Running>,
    last_end: u64,
    at_place: usize,
    outside_base: f64,
    seed: u64,
    idle_awake_ms: u64,
    pub truth: Truth,
    /// Sentry mode: never asleep ([`crate::Scenario::stays_awake`]).
    pub stays_awake: bool,
}

impl Sim {
    pub fn new(model: CarModel, plan: Plan, state: CarState, end_ms: u64, seed: u64, outside_base: f64) -> Sim {
        let t = state.t_ms;
        let mut s = Sim {
            model,
            plan,
            next: 0,
            state,
            t_ms: t,
            end_ms,
            rng: Rng::new(seed).fork(2),
            running: None,
            // asleep at the start
            last_end: t.saturating_sub(IDLE_AWAKE_MS + 1),
            at_place: crate::plan::HOME,
            outside_base,
            seed,
            idle_awake_ms: IDLE_AWAKE_MS,
            truth: Truth::default(),
            stays_awake: false,
        };
        s.state.outside_c = s.outside_at(t);
        s.state.batt_temp_c = s.state.outside_c + 2.0;
        s.state.cabin_c = s.state.outside_c;
        s
    }

    /// The car stays awake this long after it was last used (ms) instead of [`IDLE_AWAKE_MS`]; still asleep at the
    /// start.
    pub fn awake_for(mut self, ms: u64) -> Sim {
        self.idle_awake_ms = ms;
        self.last_end = self.t_ms.saturating_sub(ms + 1);
        self
    }

    pub fn state(&self) -> &CarState {
        &self.state
    }

    pub fn now(&self) -> u64 {
        self.t_ms
    }

    /// A made-up climate: a daily swing around a base that wanders from day to day.
    fn outside_at(&self, t: u64) -> f64 {
        let day = (t / DAY_MS) as f64;
        let hour = (t % DAY_MS) as f64 / 3_600_000.0;
        let base = self.outside_base + 3.0 * libm::sin(day * 1.7 + (self.seed % 97) as f64);
        base + 6.0 * libm::sin((hour - 9.0) / 24.0 * std::f64::consts::TAU)
    }

    /// When the next activity is due, after skipping charges not needed.
    fn next_start(&mut self) -> Option<u64> {
        loop {
            let a = self.plan.activities.get(self.next)?;
            let t = match a.when() {
                When::At(t) => t,
                When::After(d) => self.last_end + d,
            };
            if let Activity::Charge { only_below: Some(b), .. } = a
                && self.t_ms + STEP_MS >= t
                && self.state.soc() >= *b
            {
                self.next += 1;
                continue;
            }
            return Some(t);
        }
    }

    /// When the car wakes for the next activity.
    fn wake_for(&mut self) -> Option<u64> {
        let t = self.next_start()?;
        Some(match self.plan.activities.get(self.next) {
            Some(Activity::Drive { .. }) => t.saturating_sub(WAKE_LEAD_MS),
            _ => t,
        })
    }

    /// The start of the first activity after the current one that has a fixed time (none: nothing more planned).
    fn next_fixed(&self) -> Option<u64> {
        self.plan.activities[self.next..].iter().find_map(|a| match a.when() {
            When::At(t) => Some(t),
            When::After(_) => None,
        })
    }

    fn begin(&mut self) {
        let Some(a) = self.plan.activities.get(self.next).cloned() else { return };
        self.next += 1;
        let t = self.t_ms;
        // a charge stops 15 min before the next thing planned; the scenario's end only stops the run (a longer run of
        // the same scenario does the same until then)
        let deadline = self.next_fixed().map_or(u64::MAX, |n| n.saturating_sub(15 * 60_000).max(t + 60_000));
        let s = &mut self.state;
        self.running = Some(match a {
            Activity::Drive { from, to, route, .. } => {
                // unplugged on the way out
                s.plug = None;
                s.status = ChargeStatus::Disconnected;
                s.no_charger_current();
                s.gear = Gear::R;
                let truth = TrueDrive {
                    start_ms: t,
                    end_ms: t,
                    from: self.plan.places[from].name.clone(),
                    to: self.plan.places[to].name.clone(),
                    distance_km: 0.0,
                    start_soc: s.soc(),
                    end_soc: s.soc(),
                    max_speed_kmh: 0.0,
                    start_odometer_km: s.odometer_km,
                    end_odometer_km: s.odometer_km,
                };
                self.at_place = to;
                Running::Drive {
                    route,
                    km: 0.0,
                    phase: DrivePhase::Reverse { until: t + 4_000 },
                    passed: -1.0,
                    truth,
                    noise: self.rng.range(0.0, 100.0),
                }
            }
            Activity::Charge { kind, kw, limit, pause, .. } => {
                s.plug = Some(kind);
                s.status = ChargeStatus::Waiting;
                let truth = TrueCharge {
                    start_ms: t,
                    end_ms: t,
                    kind,
                    place: self.plan.places[self.at_place].name.clone(),
                    start_soc: s.soc(),
                    end_soc: s.soc(),
                    energy_added_kwh: 0.0,
                    max_power_kw: 0.0,
                    pauses: 0,
                };
                Running::Charge { kind, kw, limit, pause, began: t, deadline, truth, charging: false }
            }
            Activity::Update { len_ms, silences, .. } => Running::Update { start: t, len: len_ms, silences },
        });
    }

    /// The next step, `None` at the scenario's end.
    pub fn step(&mut self) -> Option<Step> {
        if self.t_ms >= self.end_ms {
            if let Some(r) = self.running.take() {
                self.finish(r);
            }
            return None;
        }
        if self.running.is_none()
            && let Some(start) = self.next_start()
            && start <= self.t_ms
        {
            self.begin();
        }
        let t = self.t_ms;
        let dt = STEP_MS as f64 / 1000.0;
        self.state.t_ms = t;
        self.state.outside_c = self.outside_at(t);
        let step = match self.running.take() {
            Some(r) => {
                let (r, buses) = self.run(r, dt);
                self.running = r;
                Step::Awake { t_ms: t, buses }
            }
            None => {
                let awake_until = self.last_end + self.idle_awake_ms;
                let wake = self.wake_for();
                if self.stays_awake || t < awake_until || wake.is_some_and(|w| w <= t) {
                    self.state.gear = Gear::P;
                    self.state.no_charger_current();
                    self.state.idle(0.3 + (self.state.cabin_c - 20.0).abs() * 0.02, dt);
                    self.state.temps(dt, None);
                    Step::Awake { t_ms: t, buses: ALL_BUSES }
                } else {
                    let to = wake.unwrap_or(self.end_ms).min(self.end_ms).min(t + 3_600_000).max(t + STEP_MS);
                    // whole steps, so the awake steps stay on the grid
                    let to = t + (to - t).div_ceil(STEP_MS) * STEP_MS;
                    self.state.sleep((to - t) as f64 / 1000.0);
                    match self.truth.sleeps.last_mut() {
                        Some(last) if last.1 == t => last.1 = to,
                        _ => self.truth.sleeps.push((t, to)),
                    }
                    self.t_ms = to;
                    return Some(Step::Asleep { from_ms: t, to_ms: to });
                }
            }
        };
        self.t_ms += STEP_MS;
        Some(step)
    }

    fn finish(&mut self, r: Running) {
        self.last_end = self.t_ms;
        match r {
            Running::Drive { mut truth, .. } => {
                truth.end_ms = self.t_ms;
                truth.end_soc = self.state.soc();
                truth.end_odometer_km = self.state.odometer_km;
                truth.distance_km = truth.end_odometer_km - truth.start_odometer_km;
                self.truth.drives.push(truth);
            }
            Running::Charge { mut truth, .. } => {
                if truth.energy_added_kwh > 0.0 {
                    truth.end_soc = self.state.soc();
                    self.truth.charges.push(truth);
                }
            }
            Running::Update { start, .. } => self.truth.updates.push((start, self.t_ms)),
        }
    }

    /// One step of the activity under way; gives it back unless it ended, with the buses that send.
    fn run(&mut self, r: Running, dt: f64) -> (Option<Running>, u8) {
        let t = self.t_ms;
        match r {
            Running::Drive { route, mut km, mut phase, mut passed, mut truth, noise } => {
                let s = &mut self.state;
                let climate = 0.4 + (s.cabin_c - 21.0).abs() * 0.05;
                let mut done = false;
                match phase {
                    DrivePhase::Reverse { until } => {
                        let left = until.saturating_sub(t) as f64 / 1000.0;
                        let kmh = if left > 2.0 { (4.0 - left) * 2.5 } else { left * 2.5 };
                        s.gear = Gear::R;
                        s.drive(self.model, kmh.max(0.0), 0.0, climate, dt);
                        if t + STEP_MS >= until {
                            phase = DrivePhase::Go;
                            s.gear = Gear::D;
                        }
                    }
                    DrivePhase::Wait { until } => {
                        s.drive(self.model, 0.0, 0.0, climate, dt);
                        if t >= until {
                            phase = DrivePhase::Go;
                        }
                    }
                    DrivePhase::Go => {
                        let leg = route.leg_at(km);
                        let wobble = 2.0 * libm::sin((t as f64 / 9_000.0) + noise);
                        let target = (route.limit_kmh[leg] + wobble).max(5.0) / 3.6;
                        let stop_km = (leg..route.limit_kmh.len())
                            .find(|&l| route.stop_after[l] && route.leg_end_km(l) > passed + 1e-9)
                            .map_or(route.km(), |l| route.leg_end_km(l));
                        let dist_m = ((stop_km - km) * 1000.0).max(0.0);
                        let allow =
                            (2.0 * 1.6 * (dist_m - 1.0).max(0.0)).sqrt().max(if dist_m > 1.5 { 1.0 } else { 0.0 });
                        let v = s.speed_kmh / 3.6;
                        let want = target.min(allow);
                        let a = ((want - v) / 1.2).clamp(-3.5, 2.2);
                        let v2 = (v + a * dt).max(0.0);
                        let step_km = ((v + v2) / 2.0 * dt / 1000.0).min(dist_m / 1000.0 + 0.0005);
                        km = (km + step_km).min(route.km());
                        s.gear = Gear::D;
                        s.drive(self.model, v2 * 3.6, a, climate, dt);
                        s.pos = route.at(km);
                        s.elevation_m = elevation_at(s.pos);
                        s.gps_accuracy_m = 1.6 + 1.2 * libm::sin((t as f64 / 17_000.0) + noise).abs();
                        truth.max_speed_kmh = truth.max_speed_kmh.max(s.speed_kmh);
                        if (stop_km - km) * 1000.0 < 1.5 && v2 < 0.4 {
                            s.speed_kmh = 0.0;
                            if stop_km >= route.km() - 1e-9 {
                                done = true;
                            } else {
                                passed = stop_km;
                                phase = DrivePhase::Wait { until: t + self.rng.below(27_000) + 8_000 };
                            }
                        }
                    }
                }
                s.temps(dt, Some(21.0));
                if done {
                    s.gear = Gear::P;
                    s.speed_kmh = 0.0;
                    self.t_ms += STEP_MS;
                    self.finish(Running::Drive { route, km, phase, passed, truth, noise });
                    self.t_ms -= STEP_MS;
                    return (None, ALL_BUSES);
                }
                (Some(Running::Drive { route, km, phase, passed, truth, noise }), ALL_BUSES)
            }
            Running::Charge { kind, kw, limit, pause, began, deadline, mut truth, mut charging } => {
                let s = &mut self.state;
                let paused = pause.is_some_and(|(at, len)| t >= began + at && t < began + at + len);
                let full = s.soc() >= limit || t >= deadline;
                if full {
                    s.status = if s.soc() >= limit { ChargeStatus::Complete } else { ChargeStatus::Stopped };
                    s.no_charger_current();
                    s.idle(0.25, dt);
                    s.temps(dt, None);
                    self.t_ms += STEP_MS;
                    self.finish(Running::Charge { kind, kw, limit, pause, began, deadline, truth, charging });
                    self.t_ms -= STEP_MS;
                    return (None, CAN0 | CAN3);
                }
                if paused {
                    if charging {
                        truth.pauses += 1;
                    }
                    charging = false;
                    s.status = ChargeStatus::Stopped;
                    s.no_charger_current();
                    s.idle(0.25, dt);
                } else {
                    if !charging && truth.energy_added_kwh == 0.0 {
                        truth.start_ms = t;
                        truth.start_soc = s.soc();
                    }
                    charging = true;
                    s.status = ChargeStatus::Charging;
                    let ramp = ((t - began) as f64 / 30_000.0).clamp(0.05, 1.0);
                    let p = match kind {
                        ChargeKind::Ac => kw * if s.soc() > 92.0 { ((100.0 - s.soc()) / 8.0).max(0.1) } else { 1.0 },
                        ChargeKind::Dc => kw.min(dc_curve_kw(self.model, s.soc())),
                    } * ramp;
                    let before = s.energy_kwh;
                    s.charge(kind, p, dt);
                    truth.energy_added_kwh += s.energy_kwh - before;
                    truth.max_power_kw = truth.max_power_kw.max(-s.power_kw());
                    truth.end_ms = t + STEP_MS;
                }
                s.temps(dt, None);
                (Some(Running::Charge { kind, kw, limit, pause, began, deadline, truth, charging }), CAN0 | CAN3)
            }
            Running::Update { start, len, silences } => {
                let s = &mut self.state;
                s.idle(0.6, dt);
                s.temps(dt, None);
                let quiet = silences.iter().any(|&(at, l)| t >= start + at && t < start + at + l);
                if t + STEP_MS >= start + len {
                    self.t_ms += STEP_MS;
                    self.finish(Running::Update { start, len, silences });
                    self.t_ms -= STEP_MS;
                    return (None, ALL_BUSES);
                }
                (Some(Running::Update { start, len, silences }), if quiet { 0 } else { ALL_BUSES })
            }
        }
    }
}
