//! The scenario API: a made-up car, its town and its days, turned into the CAN Bridge logs it would upload.

use crate::bridge::{Bridge, BridgeConfig, Formats, LogFile};
use crate::car::{CarModel, CarState};
use crate::geo::LatLon;
use crate::plan::{self, Plan, Trouble};
use crate::rng::Rng;
use crate::signals::{Rates, RawFrame, Schedule};
use crate::signer::Signer;
use crate::sim::{Sim, Step, Truth};
use sha2::{Digest, Sha256};

/// 2026-03-02T00:00:00Z, a Monday.
pub const DEFAULT_START_MS: u64 = 1_772_409_600_000;

/// The made-up town's centre: open sea in the Gulf of Guinea (0.30° N, 0.20° E), where no one lives.
pub const TOWN: LatLon = LatLon::new(0.30, 0.20);

/// The most cars [`fleet`] makes: their homes fill a grid of 15 by 14 spots 2 km apart, all of it (and every car's
/// work, shop and supercharger) in the open sea north east of [`TOWN`], inside 0° to 1.5° N and 0° to 1.5° E.
pub const MAX_FLEET: u32 = 200;

/// The highest car number: its VIN's six digits are 100 + the number.
pub const MAX_CAR: u32 = 999_899;

/// A made-up car and what it does. Every choice has a default; the same scenario (seed included) gives the same logs,
/// byte for byte.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub seed: u64,
    /// Which car of a fleet (its key, VIN and town spot differ).
    pub car: u32,
    pub model: CarModel,
    pub start_ms: u64,
    pub days: u32,
    /// Local time = UTC + this (minutes); the plan's clock times are local.
    pub utc_offset_min: i32,
    pub home: LatLon,
    pub work: LatLon,
    pub shop: LatLon,
    pub supercharger: LatLon,
    /// Weekdays home → work → home, leaving at these local minutes after midnight (± a little).
    pub commute: bool,
    pub leave_min: u32,
    pub back_min: u32,
    pub errands: bool,
    /// A trip to the supercharger and back (DC charge) on the third day, or the first Saturday before it.
    pub supercharger_trip: bool,
    /// Plugged in at home: an AC charge from `charge_start_min` (local) when below `charge_below` %, to `charge_limit`.
    pub home_charging: bool,
    pub home_charger_kw: f64,
    pub charge_start_min: u32,
    pub charge_below: f64,
    pub charge_limit: f64,
    pub software_update: bool,
    pub start_soc: f64,
    pub start_odometer_km: f64,
    /// Average outside temperature, °C.
    pub outside_c: f64,
    pub formats: Formats,
    pub rates: Rates,
    pub xz_preset: u32,
    /// The bridge's trouble set by hand (times in ms).
    pub trouble: Trouble,
    /// Adds the usual trouble, each where the car is parked and awake (see [`Scenario::bridge_trouble`]): a lost log
    /// (a chain gap), dropped frames, a pause in recording and an app update.
    pub with_trouble: bool,
    /// Made-up firmware fingerprints in the modules' info frames (off: none sent).
    pub firmware: Option<crate::firmware::Firmware>,
}

impl Scenario {
    pub fn new(seed: u64) -> Scenario {
        Scenario {
            seed,
            car: 0,
            model: CarModel::Y,
            start_ms: DEFAULT_START_MS,
            days: 3,
            utc_offset_min: 0,
            home: TOWN,
            work: TOWN.offset(5.2, 6.8),
            shop: TOWN.offset(-1.3, -2.4),
            supercharger: TOWN.offset(48.0, 61.0),
            commute: true,
            leave_min: 7 * 60 + 40,
            back_min: 17 * 60 + 15,
            errands: true,
            supercharger_trip: true,
            home_charging: true,
            home_charger_kw: 7.4,
            charge_start_min: 23 * 60 + 30,
            charge_below: 70.0,
            charge_limit: 80.0,
            software_update: true,
            start_soc: 58.0,
            start_odometer_km: 23_456.7,
            outside_c: 14.0,
            formats: Formats::Columns,
            rates: Rates::Realistic,
            xz_preset: 6,
            trouble: Trouble::default(),
            with_trouble: false,
            firmware: None,
        }
    }

    /// Which car of a fleet, at most [`MAX_CAR`] (a higher number is taken as that).
    pub fn car(mut self, n: u32) -> Self {
        self.car = n.min(MAX_CAR);
        self
    }
    pub fn model(mut self, m: CarModel) -> Self {
        self.model = m;
        self
    }
    pub fn start(mut self, ms: u64) -> Self {
        self.start_ms = ms;
        self
    }
    pub fn days(mut self, n: u32) -> Self {
        self.days = n;
        self
    }
    pub fn utc_offset_min(mut self, m: i32) -> Self {
        self.utc_offset_min = m;
        self
    }
    /// Home, and the rest of the town around it (work, shop, supercharger keep their places relative to home).
    pub fn home(mut self, lat: f64, lon: f64) -> Self {
        let (dn, de) = (lat - self.home.lat, lon - self.home.lon);
        let shift = |p: LatLon| LatLon::new(p.lat + dn, p.lon + de);
        self.home = LatLon::new(lat, lon);
        self.work = shift(self.work);
        self.shop = shift(self.shop);
        self.supercharger = shift(self.supercharger);
        self
    }
    /// Weekday commute to `work`, leaving and coming back at these local times (hours, e.g. 7.5 and 17.25).
    pub fn commute(mut self, lat: f64, lon: f64, leave_h: f64, back_h: f64) -> Self {
        self.commute = true;
        self.work = LatLon::new(lat, lon);
        self.leave_min = (leave_h * 60.0) as u32;
        self.back_min = (back_h * 60.0) as u32;
        self
    }
    pub fn no_commute(mut self) -> Self {
        self.commute = false;
        self
    }
    pub fn shop(mut self, lat: f64, lon: f64) -> Self {
        self.shop = LatLon::new(lat, lon);
        self
    }
    pub fn errands(mut self, on: bool) -> Self {
        self.errands = on;
        self
    }
    /// A supercharger, and a trip to it (DC charge) on the third day, or the first Saturday before it.
    pub fn supercharger(mut self, lat: f64, lon: f64) -> Self {
        self.supercharger = LatLon::new(lat, lon);
        self.supercharger_trip = true;
        self
    }
    pub fn supercharger_trip(mut self, on: bool) -> Self {
        self.supercharger_trip = on;
        self
    }
    /// AC charging at home: `kw` from the wall, from `start_h` local, when below `below` %, up to `limit` %.
    pub fn charge_at_home(mut self, kw: f64, start_h: f64, below: f64, limit: f64) -> Self {
        self.home_charging = true;
        self.home_charger_kw = kw;
        self.charge_start_min = (start_h * 60.0) as u32;
        self.charge_below = below;
        self.charge_limit = limit;
        self
    }
    pub fn no_home_charging(mut self) -> Self {
        self.home_charging = false;
        self
    }
    pub fn software_update(mut self, on: bool) -> Self {
        self.software_update = on;
        self
    }
    pub fn battery(mut self, soc: f64) -> Self {
        self.start_soc = soc.clamp(1.0, 100.0);
        self
    }
    pub fn odometer(mut self, km: f64) -> Self {
        self.start_odometer_km = km.max(0.0);
        self
    }
    pub fn outside(mut self, c: f64) -> Self {
        self.outside_c = c;
        self
    }
    pub fn formats(mut self, f: Formats) -> Self {
        self.formats = f;
        self
    }
    pub fn rates(mut self, r: Rates) -> Self {
        self.rates = r;
        self
    }
    pub fn xz_preset(mut self, p: u32) -> Self {
        self.xz_preset = p.min(9);
        self
    }
    pub fn trouble(mut self, t: Trouble) -> Self {
        self.trouble = t;
        self
    }
    pub fn with_trouble(mut self, on: bool) -> Self {
        self.with_trouble = on;
        self
    }
    /// The modules' info frames with these made-up fingerprints (see [`crate::Firmware::made_up`]).
    pub fn firmware(mut self, f: crate::firmware::Firmware) -> Self {
        self.firmware = Some(f);
        self
    }

    /// The seed of this car (the scenario's seed and the car's number).
    fn car_seed(&self) -> u64 {
        let h = Sha256::digest(format!("synth car {} {}", self.seed, self.car));
        u64::from_le_bytes([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]])
    }

    /// The CAN Bridge install's key: 12 hex characters, as the app makes from its install id.
    pub fn vehicle_key(&self) -> String {
        let h = Sha256::digest(format!("synth install {} {}", self.seed, self.car));
        hex::encode(&h[..6])
    }

    /// A VIN that is clearly made up (`FAKE` where the body and motor codes go), yet reads as a model, a year and a
    /// plant: maker `5YJ` (every model's), the model letter, `FAKE0`, `R` (2024), `X`, then six digits.
    pub fn vin(&self) -> String {
        format!("5YJ{}FAKE0RX{:06}", self.model.letter(), 100 + u64::from(self.car.min(MAX_CAR)))
    }

    /// The bridge's signing key.
    pub fn signer(&self) -> Signer {
        Signer::from_seed(format!("synth signer {} {}", self.seed, self.car).as_bytes())
    }

    pub fn end_ms(&self) -> u64 {
        self.start_ms + u64::from(self.days) * plan::DAY_MS
    }

    pub fn plan(&self) -> Plan {
        let mut s = self.clone();
        s.seed = self.car_seed();
        plan::make(&s)
    }

    fn sim(&self) -> Sim {
        let state = CarState::new(self.model, self.start_ms, self.home, self.start_soc, self.start_odometer_km);
        Sim::new(self.model, self.plan(), state, self.end_ms(), self.car_seed(), self.outside_c)
    }

    /// What happens, without making any log (quick): the drives, charges, sleeps and updates.
    pub fn truth(&self) -> Truth {
        let mut sim = self.sim();
        while sim.step().is_some() {}
        sim.truth
    }

    /// The trouble the bridge has: the trouble set by hand, and with [`Scenario::with_trouble`] the usual four, each
    /// in a span of its own where the car is parked and awake (so each really happens): a lost log in the first span
    /// of 5 min or more, dropped frames in the next of 4 min or more, a 6 min pause in recording in the next of 9 min
    /// or more, and an app update in the next of 4 min or more. The spans are found in a run one day longer, so a
    /// scenario's first days have the same trouble whatever its length.
    pub fn bridge_trouble(&self) -> Trouble {
        let mut t = self.trouble.clone();
        if !self.with_trouble {
            return t;
        }
        let mut longer = self.clone();
        longer.days = self.days.saturating_add(1);
        longer.with_trouble = false;
        let truth = longer.truth();
        let end = longer.end_ms();
        // awake: between sleeps (the car is asleep at the start)
        let mut awake = Vec::new();
        let mut woke: Option<u64> = None;
        for &(a, b) in &truth.sleeps {
            if let Some(w) = woke {
                awake.push((w, a));
            }
            woke = Some(b);
        }
        if let Some(w) = woke.filter(|&w| w < end) {
            awake.push((w, end));
        }
        // parked: awake but not driving (half a minute either side of a drive)
        let mut parked = Vec::new();
        for (a, b) in awake {
            let mut from = a;
            for d in truth.drives.iter().filter(|d| d.end_ms > a && d.start_ms < b) {
                let to = d.start_ms.saturating_sub(30_000);
                if to > from {
                    parked.push((from, to));
                }
                from = from.max(d.end_ms + 30_000);
            }
            if b > from {
                parked.push((from, b));
            }
        }
        const MIN: u64 = 60_000;
        let mut kind = 0;
        for (a, b) in parked {
            let len = b - a;
            match kind {
                0 if len >= 5 * MIN => t.lose_log_at.push(a + 2 * MIN),
                1 if len >= 4 * MIN => t.drop_at.push((a + 2 * MIN + 20_000, 400)),
                2 if len >= 9 * MIN => t.not_recorded.push((a + 90_000, a + 90_000 + 6 * MIN)),
                3 if len >= 4 * MIN => t.app_update_at.push((a + 2 * MIN, 25_000)),
                _ => continue,
            }
            kind += 1;
            if kind == 4 {
                break;
            }
        }
        t
    }

    /// The logs, one at a time, in the order the bridge finishes them. The log still being written at the end is not
    /// given: the logs of a scenario are always the first logs of a longer run of it, byte for byte.
    pub fn logs(&self) -> Logs {
        let sim = self.sim();
        let trouble = self.bridge_trouble();
        let rng = Rng::new(self.car_seed()).fork(3);
        let cfg = BridgeConfig {
            vehicle_key: self.vehicle_key(),
            formats: self.formats,
            xz_preset: self.xz_preset,
            app_build: 150,
            trouble,
        };
        let uptime = 3 * 86_400 + rng.clone().below(86_400);
        Logs {
            trouble: cfg.trouble.clone(),
            bridge: Bridge::new(cfg, self.signer(), self.start_ms * 1000, uptime),
            schedule: Schedule::new(&self.vin(), self.rates, rng),
            firmware: self.firmware.clone().map(crate::firmware::Sender::new),
            sim,
            buf: Vec::new(),
            done: false,
        }
    }
}

/// The logs of a scenario ([`Scenario::logs`]).
pub struct Logs {
    trouble: Trouble,
    bridge: Bridge,
    schedule: Schedule,
    firmware: Option<crate::firmware::Sender>,
    sim: Sim,
    buf: Vec<RawFrame>,
    done: bool,
}

/// What a scenario's run made, beyond its logs.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Report {
    pub truth: Truth,
    /// Logs signed but never uploaded (the chain shows them missing).
    pub lost: Vec<String>,
    /// Logs with a `# dropped` line.
    pub dropped: Vec<String>,
    /// Spans recording was paused, `[from, to)` ms.
    pub not_recorded: Vec<(u64, u64)>,
    /// When the app updated itself (ms).
    pub app_updates: Vec<u64>,
    /// The trouble the bridge was given ([`Scenario::bridge_trouble`]).
    pub trouble: Trouble,
    pub fingerprint: String,
}

impl Logs {
    /// What happened so far (everything, once the iterator is done).
    pub fn report(&self) -> Report {
        Report {
            truth: self.sim.truth.clone(),
            lost: self.bridge.lost.clone(),
            dropped: self.bridge.dropped.clone(),
            not_recorded: self.bridge.not_recorded.clone(),
            app_updates: self.bridge.app_updates.clone(),
            trouble: self.trouble.clone(),
            fingerprint: self.bridge.signer().fingerprint(),
        }
    }
}

impl Iterator for Logs {
    type Item = LogFile;

    fn next(&mut self) -> Option<LogFile> {
        loop {
            if let Some(l) = self.bridge.take() {
                return Some(l);
            }
            if self.done {
                return None;
            }
            match self.sim.step() {
                Some(Step::Awake { t_ms, buses }) => {
                    self.buf.clear();
                    let from = t_ms * 1000;
                    self.schedule.frames(
                        from,
                        from + crate::sim::STEP_MS * 1000,
                        buses,
                        self.sim.state(),
                        &mut self.buf,
                    );
                    if let Some(fw) = &mut self.firmware {
                        let updated = !self.sim.truth.updates.is_empty();
                        fw.frames(from, from + crate::sim::STEP_MS * 1000, buses, updated, &mut self.buf);
                        self.buf.sort_by_key(|f| f.t_us);
                    }
                    if self.buf.is_empty() {
                        self.bridge.tick(from);
                    } else {
                        self.bridge.frames(&self.buf);
                    }
                }
                Some(Step::Asleep { from_ms, to_ms }) => {
                    self.schedule.frames(0, 0, 0, self.sim.state(), &mut self.buf);
                    // the bridge's trouble happens on time while the car sleeps
                    for us in self.bridge.boundaries(from_ms * 1000, to_ms * 1000) {
                        self.bridge.tick(us);
                    }
                    self.bridge.tick(to_ms * 1000);
                }
                None => {
                    self.bridge.finish(self.sim.now() * 1000);
                    self.done = true;
                }
            }
        }
    }
}

/// Several cars of one made-up fleet (at most [`MAX_FLEET`]): each its own key, VIN, signing key and home in the
/// town; Model Y and Model 3 in turn.
pub fn fleet(seed: u64, cars: u32, days: u32) -> Vec<Scenario> {
    (0..cars.min(MAX_FLEET))
        .map(|i| {
            let f = f64::from(i);
            let model = if i % 2 == 0 { CarModel::Y } else { CarModel::Three };
            let base = Scenario::new(seed).car(i).model(model).days(days);
            let home = TOWN.offset(2.0 * f64::from(i / 15), 2.0 * f64::from(i % 15));
            base.home(home.lat, home.lon).battery(50.0 + (f * 7.0) % 20.0).odometer(12_000.0 + f * 9_731.0)
        })
        .collect()
}
