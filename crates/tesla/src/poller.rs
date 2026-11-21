//! One task per account that reads its cars without ever waking one, and lets them fall asleep.
//!
//! The cadence (all in [`Cadence`]):
//!
//! | Mode | When | Calls |
//! |---|---|---|
//! | `off` | account paused, sign-in needed, car not matched | none |
//! | `watching_list` | the list says asleep or offline (or a 408) | the product list every 60 s (one call for all cars; it never wakes a car) |
//! | `online_idle` | online, parked, not charging | `vehicle_data` every 60 s |
//! | `letting_sleep` | online and idle for 10 min with nothing keeping it awake | no `vehicle_data` for 21 min, only the list; then one look, and back to waiting if still idle |
//! | `charging` | charging | `vehicle_data` every 30 s |
//! | `driving` | gear D, R or N, or moving | the streaming socket, and `vehicle_data` every 60 s (30 s without a stream) |
//! | `backoff` | errors | 429: Tesla's wait (at least 60 s, else 5 min); 5xx and network: 30 s doubling to 15 min |
//!
//! When SMO sends live CAN for the car, `vehicle_data` drops to every 5 min while online and no stream is opened.
//! At most one request a second and 3,000 a day per account (then nothing until midnight UTC). The access token
//! is refreshed when under 30 min are left, and once on a 401; a refused refresh stops the account until an admin
//! signs in again. The new refresh token is stored before the new access token is used.

use crate::api::{ApiError, CarState, Tesla, Vehicle};
use crate::auth::AuthError;
use crate::convert::{self, ApiMeta, Reading};
use crate::store::{Account, AccountCar, AccountState, Credentials, ErrorInfo, Location, Match, Store, StoreError};
use crate::stream::{StreamHandle, Streamer};
use model::{CarId, Sample};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Where the poller's data goes (the engine implements it).
pub trait Sink: Send + Sync + 'static {
    /// Makes a car for a vehicle the admin chose to add as new.
    fn new_car(&self, car: &NewCar) -> Result<CarId, SinkError>;
    /// Samples (by time) and what the API said about the car's state over spans.
    fn points(&self, car: CarId, samples: &[Sample], facts: &[ApiFact]) -> Result<(), SinkError>;
    /// Values beside the samples (version, settings, charger), each time they change.
    fn meta(&self, car: CarId, meta: &ApiMeta) -> Result<(), SinkError>;
    /// SMO sends live CAN for this car (its newest log arrived a few minutes ago): poll less.
    fn smo_live(&self, car: CarId) -> bool;
    /// The account's state changed (for an audit entry or an event).
    fn account_changed(&self, _account: &Account) {}
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SinkError(pub String);

/// A vehicle to add as a new car.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCar {
    pub vin_prefix: Option<String>,
    pub model: Option<char>,
    pub year: Option<u16>,
    pub display_name: Option<String>,
}

/// What the API said about the car's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiState {
    Online,
    Asleep,
    Offline,
}

/// A span over which the API saw the car in one state (`start_ms == end_ms` for one look).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiFact {
    pub start_ms: u64,
    pub end_ms: u64,
    pub state: ApiState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Off,
    WatchingList,
    OnlineIdle,
    LettingSleep,
    Charging,
    Driving,
    Backoff,
}

/// The timing, ms (and the daily cap).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cadence {
    pub list_ms: u64,
    pub idle_ms: u64,
    pub charging_ms: u64,
    pub driving_ms: u64,
    pub driving_streaming_ms: u64,
    pub smo_live_ms: u64,
    pub idle_before_sleep_ms: u64,
    pub quiet_ms: u64,
    pub min_gap_ms: u64,
    pub daily_cap: u64,
    pub refresh_before_ms: u64,
    pub backoff_first_ms: u64,
    pub backoff_max_ms: u64,
    pub rate_limit_min_ms: u64,
    pub rate_limit_default_ms: u64,
    pub refused_retry_ms: u64,
    pub stream_drain_ms: u64,
    /// Two looks at the same state this close together make one span.
    pub fact_join_ms: u64,
}

impl Default for Cadence {
    fn default() -> Self {
        Cadence {
            list_ms: 60_000,
            idle_ms: 60_000,
            charging_ms: 30_000,
            driving_ms: 30_000,
            driving_streaming_ms: 60_000,
            smo_live_ms: 5 * 60_000,
            idle_before_sleep_ms: 10 * 60_000,
            quiet_ms: 21 * 60_000,
            min_gap_ms: 1_000,
            daily_cap: 3_000,
            refresh_before_ms: 30 * 60_000,
            backoff_first_ms: 30_000,
            backoff_max_ms: 15 * 60_000,
            rate_limit_min_ms: 60_000,
            rate_limit_default_ms: 5 * 60_000,
            refused_retry_ms: 60 * 60_000,
            stream_drain_ms: 5_000,
            fact_join_ms: 5 * 60_000,
        }
    }
}

/// How often the server asks, as an admin chooses it (`/integrations/tesla` `poll`). Only the reads of a car that is
/// awake change; letting the car sleep, the caps and the back-off stay as they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Pace {
    /// Half the waits of `normal` (more requests: still at most 3,000 a day).
    Often,
    #[default]
    Normal,
    /// Fewer requests: parked every 5 min, charging every 2 min, driving every minute, the list every 2 min.
    Gentle,
}

impl Pace {
    pub const ALL: [Pace; 3] = [Pace::Often, Pace::Normal, Pace::Gentle];

    pub fn word(self) -> &'static str {
        match self {
            Pace::Often => "often",
            Pace::Normal => "normal",
            Pace::Gentle => "gentle",
        }
    }

    pub fn parse(s: &str) -> Option<Pace> {
        Pace::ALL.into_iter().find(|p| p.word() == s)
    }

    /// The cadence of this pace.
    pub fn cadence(self) -> Cadence {
        let n = Cadence::default();
        match self {
            Pace::Normal => n,
            Pace::Often => Cadence {
                list_ms: 30_000,
                idle_ms: 30_000,
                charging_ms: 15_000,
                driving_ms: 15_000,
                driving_streaming_ms: 30_000,
                ..n
            },
            Pace::Gentle => Cadence {
                list_ms: 120_000,
                idle_ms: 5 * 60_000,
                charging_ms: 120_000,
                driving_ms: 60_000,
                driving_streaming_ms: 120_000,
                ..n
            },
        }
    }
}

/// One car's polling, for the status page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CarStatus {
    pub vehicle: String,
    pub car: Option<CarId>,
    pub mode: Mode,
    pub car_state: CarState,
    pub last_poll_ms: Option<u64>,
    pub next_poll_ms: Option<u64>,
    pub last_data_ms: Option<u64>,
    pub streaming: bool,
    pub last_error: Option<ErrorInfo>,
}

/// One account's polling, for the status page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountStatus {
    pub account: String,
    pub state: AccountState,
    pub polls_today: u64,
    pub last_ok_ms: Option<u64>,
    pub last_error: Option<ErrorInfo>,
    pub cars: Vec<CarStatus>,
}

struct CarPoll {
    vehicle: String,
    tesla_id: String,
    tag: Option<String>,
    car: Option<CarId>,
    mode: Mode,
    state: CarState,
    next_data_ms: u64,
    idle_since_ms: Option<u64>,
    quiet_until_ms: u64,
    just_woke: bool,
    last_poll_ms: Option<u64>,
    last_data_ms: Option<u64>,
    last_seen: Option<(u64, ApiState)>,
    last_meta: Option<ApiMeta>,
    stream: Option<StreamHandle>,
    stream_refusals: u8,
    last_error: Option<ErrorInfo>,
}

impl CarPoll {
    fn new(c: &AccountCar, car: Option<CarId>) -> CarPoll {
        CarPoll {
            vehicle: c.vehicle.clone(),
            tesla_id: c.tesla_id.clone(),
            tag: c.tesla_vehicle_id.clone(),
            car,
            mode: Mode::WatchingList,
            state: CarState::Unknown,
            next_data_ms: 0,
            idle_since_ms: None,
            quiet_until_ms: 0,
            just_woke: false,
            last_poll_ms: None,
            last_data_ms: None,
            last_seen: None,
            last_meta: None,
            stream: None,
            stream_refusals: 0,
            last_error: None,
        }
    }

    fn data_due(&self) -> Option<u64> {
        match self.mode {
            Mode::OnlineIdle | Mode::Charging | Mode::Driving => Some(self.next_data_ms),
            Mode::LettingSleep => Some(self.quiet_until_ms),
            _ => None,
        }
    }

    fn sleeping(&mut self) {
        self.mode = Mode::WatchingList;
        self.idle_since_ms = None;
        self.stream = None;
        self.stream_refusals = 0;
    }
}

fn err(code: &str, message: impl Into<String>, at_ms: u64) -> ErrorInfo {
    ErrorInfo { code: code.to_string(), message: message.into(), at_ms }
}

/// The poller of one account.
pub struct Poller<T: Tesla, S: Sink, St: Streamer> {
    id: String,
    store: Store,
    tesla: T,
    sink: Arc<S>,
    streamer: St,
    cadence: Cadence,
    account: Account,
    creds: Credentials,
    generation: u64,
    cars: Vec<CarPoll>,
    list_next_ms: u64,
    backoff_until_ms: u64,
    backoff_step: u32,
    day: u64,
    polls_today: u64,
    last_request_ms: Option<u64>,
    force_refresh: bool,
    unauthorized_once: bool,
    gone: bool,
    last_ok_ms: Option<u64>,
    last_error: Option<ErrorInfo>,
    last_ok_written_ms: u64,
}

impl<T: Tesla, S: Sink, St: Streamer> Poller<T, S, St> {
    pub fn new(
        store: Store,
        id: &str,
        tesla: T,
        sink: Arc<S>,
        streamer: St,
        cadence: Cadence,
    ) -> Result<Self, StoreError> {
        let generation = store.generation();
        let account = store.get(id)?;
        let creds = store.credentials(id)?;
        let mut p = Poller {
            id: id.to_string(),
            store,
            tesla,
            sink,
            streamer,
            cadence,
            last_ok_ms: account.last_ok_ms,
            last_error: account.last_error.clone(),
            account,
            creds,
            generation,
            cars: Vec::new(),
            list_next_ms: 0,
            backoff_until_ms: 0,
            backoff_step: 0,
            day: 0,
            polls_today: 0,
            last_request_ms: None,
            force_refresh: false,
            unauthorized_once: false,
            gone: false,
            last_ok_written_ms: 0,
        };
        p.rebuild_cars();
        Ok(p)
    }

    pub fn status(&self) -> AccountStatus {
        AccountStatus {
            account: self.id.clone(),
            state: self.account.state,
            polls_today: self.polls_today,
            last_ok_ms: self.last_ok_ms,
            last_error: self.last_error.clone(),
            cars: self
                .cars
                .iter()
                .map(|c| CarStatus {
                    vehicle: c.vehicle.clone(),
                    car: c.car,
                    mode: if self.idle() { Mode::Off } else { c.mode },
                    car_state: c.state,
                    last_poll_ms: c.last_poll_ms,
                    next_poll_ms: if self.idle() { None } else { c.data_due().or(Some(self.list_next_ms)) },
                    last_data_ms: c.last_data_ms,
                    streaming: c.stream.is_some(),
                    last_error: c.last_error.clone(),
                })
                .collect(),
        }
    }

    fn idle(&self) -> bool {
        self.account.paused || matches!(self.account.state, AccountState::SignInNeeded) || self.gone
    }

    fn rebuild_cars(&mut self) {
        let mut old: Vec<CarPoll> = std::mem::take(&mut self.cars);
        for c in &self.account.cars {
            let car = match c.matched {
                Some(Match::Car { car }) => Some(car),
                Some(Match::New) => None,
                _ => continue,
            };
            let poll = match old.iter().position(|o| o.vehicle == c.vehicle && o.tesla_id == c.tesla_id) {
                Some(i) => {
                    let mut p = old.swap_remove(i);
                    p.car = car;
                    p.tag = c.tesla_vehicle_id.clone();
                    p
                }
                None => {
                    let mut p = CarPoll::new(c, car);
                    // A car added while the account runs is looked up in the next list.
                    self.list_next_ms = 0;
                    p.mode = Mode::WatchingList;
                    p
                }
            };
            self.cars.push(poll);
        }
    }

    /// Reads the account again when someone changed it. `false`: it is gone.
    fn sync(&mut self) -> bool {
        let g = self.store.generation();
        if g == self.generation {
            return true;
        }
        self.generation = g;
        match (self.store.get(&self.id), self.store.credentials(&self.id)) {
            (Ok(a), Ok(c)) => {
                let new_token = c.refresh != self.creds.refresh;
                self.account = a;
                self.creds = c;
                if new_token && self.account.state != AccountState::SignInNeeded {
                    // The admin pasted a new token: start over.
                    self.backoff_until_ms = 0;
                    self.unauthorized_once = false;
                    if self.last_error.as_ref().is_some_and(|e| e.code == "endpoint_gone") {
                        self.gone = false;
                    }
                }
                self.rebuild_cars();
                true
            }
            (Err(StoreError::NotFound), _) | (_, Err(StoreError::NotFound)) => {
                self.gone = true;
                self.cars.clear();
                false
            }
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!(account = %self.id, error = %e, "the Tesla account could not be read again");
                true
            }
        }
    }

    fn set_state(&mut self, state: AccountState, error: Option<ErrorInfo>, now_ms: u64) {
        let changed = self.account.state != state || self.last_error != error;
        self.last_error = error.clone();
        if state == AccountState::Ok {
            self.last_ok_ms = Some(now_ms);
        }
        let write_ok = state == AccountState::Ok && now_ms >= self.last_ok_written_ms + 3_600_000;
        if !changed && !write_ok {
            return;
        }
        let last_ok = self.last_ok_ms;
        match self.store.update(&self.id, None, |a, _| {
            a.state = state;
            a.last_error = error;
            a.last_ok_ms = last_ok;
        }) {
            Ok((a, ())) => {
                self.last_ok_written_ms = now_ms;
                self.account = a;
                self.generation = self.store.generation();
                if changed {
                    tracing::info!(account = %self.id, state = ?state, "tesla account state");
                    self.sink.account_changed(&self.account);
                }
            }
            Err(e) => tracing::warn!(account = %self.id, error = %e, "the Tesla account could not be saved"),
        }
    }

    fn back_off(&mut self, now_ms: u64, wait_ms: Option<u64>) -> u64 {
        let wait = wait_ms.unwrap_or_else(|| {
            let w = self.cadence.backoff_first_ms.saturating_mul(1u64 << self.backoff_step.min(16));
            self.backoff_step += 1;
            w.min(self.cadence.backoff_max_ms)
        });
        self.backoff_until_ms = now_ms + wait;
        self.backoff_until_ms
    }

    /// May a request go out now? `Err(t)`: not before `t`.
    fn gate(&mut self, now_ms: u64) -> Result<(), u64> {
        if let Some(last) = self.last_request_ms
            && now_ms < last + self.cadence.min_gap_ms
        {
            return Err(last + self.cadence.min_gap_ms);
        }
        if self.polls_today >= self.cadence.daily_cap {
            let midnight = (now_ms / 86_400_000 + 1) * 86_400_000;
            self.set_state(
                AccountState::Ok,
                Some(err("daily_cap", "the daily number of requests to Tesla was reached", now_ms)),
                now_ms,
            );
            for c in &mut self.cars {
                c.sleeping();
            }
            self.backoff_until_ms = midnight;
            return Err(midnight);
        }
        self.last_request_ms = Some(now_ms);
        self.polls_today += 1;
        Ok(())
    }

    fn fact(&self, car: usize, at_ms: u64, state: ApiState) -> ApiFact {
        match self.cars[car].last_seen {
            Some((t, s)) if s == state && at_ms >= t && at_ms - t <= self.cadence.fact_join_ms => {
                ApiFact { start_ms: t, end_ms: at_ms, state }
            }
            _ => ApiFact { start_ms: at_ms, end_ms: at_ms, state },
        }
    }

    fn send(&mut self, i: usize, samples: &[Sample], fact: Option<ApiFact>) {
        let Some(car) = self.cars[i].car else { return };
        if let Some(f) = fact {
            self.cars[i].last_seen = Some((f.end_ms, f.state));
        }
        let facts: Vec<ApiFact> = fact.into_iter().collect();
        if samples.is_empty() && facts.is_empty() {
            return;
        }
        if let Err(e) = self.sink.points(car, samples, &facts) {
            tracing::warn!(account = %self.id, error = %e, "Tesla data could not be stored");
        }
    }

    fn drain_streams(&mut self) {
        for i in 0..self.cars.len() {
            let Some(h) = self.cars[i].stream.as_mut() else { continue };
            let d = h.drain();
            if d.auth_failed {
                // Refused: refresh the token once and try again; refused twice in one drive, no stream until the
                // next drive.
                self.cars[i].stream_refusals = self.cars[i].stream_refusals.saturating_add(1);
                if self.cars[i].stream_refusals == 1 {
                    self.force_refresh = true;
                }
            }
            if d.ended {
                self.cars[i].stream = None;
            }
            if !d.samples.is_empty() {
                self.cars[i].last_data_ms = d.samples.last().map(|s| s.t_ms);
                self.send(i, &d.samples, None);
            }
        }
    }

    fn make_new_cars(&mut self) {
        let todo: Vec<AccountCar> =
            self.account.cars.iter().filter(|c| c.matched == Some(Match::New)).cloned().collect();
        for c in todo {
            let new = NewCar {
                vin_prefix: c.vin_prefix.clone(),
                model: c.model,
                year: c.year,
                display_name: c.display_name.clone(),
            };
            match self.sink.new_car(&new) {
                Ok(car) => {
                    let r = self.store.update(&self.id, None, |a, _| {
                        if let Some(x) = a.cars.iter_mut().find(|x| x.vehicle == c.vehicle) {
                            x.matched = Some(Match::Car { car });
                        }
                    });
                    if let Ok((a, ())) = r {
                        self.account = a;
                        self.generation = self.store.generation();
                        self.rebuild_cars();
                    }
                }
                Err(e) => tracing::warn!(account = %self.id, error = %e, "a car for a Tesla vehicle could not be made"),
            }
        }
    }

    /// Does what is due at `now_ms`; returns when to come back. `None`: the account is gone, stop.
    pub async fn tick(&mut self, now_ms: u64) -> Option<u64> {
        if !self.sync() {
            return None;
        }
        if now_ms / 86_400_000 != self.day {
            self.day = now_ms / 86_400_000;
            self.polls_today = 0;
        }
        let later = now_ms + self.cadence.list_ms;
        if self.idle() {
            for c in &mut self.cars {
                c.sleeping();
                c.mode = Mode::Off;
            }
            return Some(later);
        }
        self.make_new_cars();
        self.drain_streams();
        let drain = self.cars.iter().any(|c| c.stream.is_some()).then_some(now_ms + self.cadence.stream_drain_ms);
        let soonest = |t: u64| Some(drain.map_or(t, |d| d.min(t)));
        if now_ms < self.backoff_until_ms {
            return soonest(self.backoff_until_ms);
        }
        if self.cars.iter().all(|c| c.car.is_none()) {
            // Nothing matched yet: nothing to poll.
            return Some(later);
        }
        // The access token first.
        let need_token = self.force_refresh
            || self.creds.access.is_none()
            || self.creds.access_expires_ms <= now_ms + self.cadence.refresh_before_ms;
        if need_token {
            if let Err(t) = self.gate(now_ms) {
                return soonest(t);
            }
            return match self.refresh(now_ms).await {
                Ok(()) => Some(now_ms + self.cadence.min_gap_ms),
                Err(t) => soonest(t),
            };
        }
        if self.list_next_ms <= now_ms
            && self.cars.iter().any(|c| c.data_due().is_none() || c.mode == Mode::LettingSleep)
        {
            if let Err(t) = self.gate(now_ms) {
                return soonest(t);
            }
            return match self.list(now_ms).await {
                Ok(()) => Some(now_ms + self.cadence.min_gap_ms),
                Err(t) => soonest(t),
            };
        }
        let due = (0..self.cars.len())
            .filter(|i| self.cars[*i].car.is_some())
            .filter_map(|i| self.cars[i].data_due().map(|t| (t, i)))
            .min();
        if let Some((t, i)) = due
            && t <= now_ms
        {
            if let Err(t) = self.gate(now_ms) {
                return soonest(t);
            }
            return match self.data(i, now_ms).await {
                Ok(()) => Some(now_ms + self.cadence.min_gap_ms),
                Err(t) => soonest(t),
            };
        }
        let needs_list = self.cars.iter().any(|c| c.data_due().is_none() || c.mode == Mode::LettingSleep);
        let mut next = due.map_or(u64::MAX, |(t, _)| t);
        if needs_list {
            next = next.min(self.list_next_ms);
        }
        if next == u64::MAX {
            next = later;
        }
        soonest(next.max(now_ms + 1))
    }

    async fn refresh(&mut self, now_ms: u64) -> Result<(), u64> {
        match self.tesla.refresh(&self.creds.refresh).await {
            Ok(t) => {
                let new = Credentials {
                    refresh: t.refresh.unwrap_or_else(|| self.creds.refresh.clone()),
                    access: Some(t.access),
                    access_expires_ms: now_ms + t.expires_in_s * 1000,
                };
                // Stored before it is used: the old refresh token is spent now.
                let saved = self.store.update(&self.id, None, |_, c| *c = new.clone());
                match saved {
                    Ok((a, ())) => {
                        self.account = a;
                        self.generation = self.store.generation();
                        self.creds = new;
                        self.force_refresh = false;
                        self.backoff_step = 0;
                        Ok(())
                    }
                    Err(e) => {
                        tracing::error!(account = %self.id, error = %e, "new Tesla tokens could not be stored");
                        self.set_state(
                            AccountState::Error,
                            Some(err("store_failed", "the new tokens could not be stored", now_ms)),
                            now_ms,
                        );
                        Err(self.back_off(now_ms, None))
                    }
                }
            }
            Err(AuthError::Refused) => {
                self.set_state(
                    AccountState::SignInNeeded,
                    Some(err("sign_in_needed", "Tesla refused the refresh token; sign in again", now_ms)),
                    now_ms,
                );
                for c in &mut self.cars {
                    c.sleeping();
                    c.mode = Mode::Off;
                }
                Err(now_ms + self.cadence.list_ms)
            }
            Err(AuthError::RateLimited { retry_after_s }) => {
                let wait = self.rate_wait(retry_after_s);
                self.note_error(err("rate_limited", "Tesla asked us to wait", now_ms));
                Err(self.back_off(now_ms, Some(wait)))
            }
            Err(e) => {
                self.note_error(err(e.code(), e.to_string(), now_ms));
                Err(self.back_off(now_ms, None))
            }
        }
    }

    fn rate_wait(&self, retry_after_s: Option<u64>) -> u64 {
        retry_after_s
            .map_or(self.cadence.rate_limit_default_ms, |s| s.saturating_mul(1000))
            .max(self.cadence.rate_limit_min_ms)
    }

    fn note_error(&mut self, e: ErrorInfo) {
        tracing::warn!(account = %self.id, code = %e.code, "tesla request failed");
        self.last_error = Some(e);
    }

    /// Handles an API error common to both calls. Returns when to come back.
    fn api_error(&mut self, e: &ApiError, now_ms: u64) -> u64 {
        match e {
            ApiError::Unauthorized if !self.unauthorized_once => {
                self.unauthorized_once = true;
                self.force_refresh = true;
                now_ms + self.cadence.min_gap_ms
            }
            ApiError::Unauthorized | ApiError::Forbidden => {
                self.set_state(
                    AccountState::Error,
                    Some(err("forbidden", "Tesla refuses this account's requests; trying again in an hour", now_ms)),
                    now_ms,
                );
                self.back_off(now_ms, Some(self.cadence.refused_retry_ms))
            }
            ApiError::EndpointGone => {
                self.set_state(
                    AccountState::Error,
                    Some(err("endpoint_gone", "Tesla closed the Owner API endpoint this server uses", now_ms)),
                    now_ms,
                );
                self.gone = true;
                now_ms + self.cadence.list_ms
            }
            ApiError::RateLimited { retry_after_s } => {
                let wait = self.rate_wait(*retry_after_s);
                self.note_error(err("rate_limited", "Tesla asked us to wait", now_ms));
                self.back_off(now_ms, Some(wait))
            }
            other => {
                self.note_error(err(other.code(), other.to_string(), now_ms));
                self.back_off(now_ms, None)
            }
        }
    }

    async fn list(&mut self, now_ms: u64) -> Result<(), u64> {
        let Some(access) = self.creds.access.clone() else { return Err(now_ms + self.cadence.min_gap_ms) };
        let list = match self.tesla.products(&access).await {
            Ok(l) => l,
            Err(e) => return Err(self.api_error(&e, now_ms)),
        };
        self.unauthorized_once = false;
        self.backoff_step = 0;
        self.list_next_ms = now_ms + self.cadence.list_ms;
        self.set_state(AccountState::Ok, None, now_ms);
        self.note_vehicles(&list);
        for i in 0..self.cars.len() {
            let Some(v) = list.iter().find(|v| v.id == self.cars[i].tesla_id) else {
                self.cars[i].last_error = Some(err("not_on_account", "Tesla no longer lists this car", now_ms));
                continue;
            };
            self.cars[i].last_poll_ms = Some(now_ms);
            self.cars[i].last_error = None;
            self.cars[i].state = v.state;
            let fact = match v.state {
                CarState::Online => Some(ApiState::Online),
                CarState::Asleep => Some(ApiState::Asleep),
                CarState::Offline => Some(ApiState::Offline),
                CarState::Unknown => None,
            };
            let c = &mut self.cars[i];
            match v.state {
                CarState::Online => {
                    if matches!(c.mode, Mode::WatchingList | Mode::Off | Mode::Backoff) {
                        // Awake on its own: look now.
                        c.mode = Mode::OnlineIdle;
                        c.next_data_ms = now_ms;
                        c.idle_since_ms = None;
                        c.just_woke = true;
                    }
                }
                CarState::Asleep | CarState::Offline => c.sleeping(),
                CarState::Unknown => {}
            }
            let f = fact.map(|s| self.fact(i, now_ms, s));
            self.send(i, &[], f);
        }
        Ok(())
    }

    /// Keeps the account's vehicle list up to date (new vehicles wait for an admin to match them).
    fn note_vehicles(&mut self, list: &[Vehicle]) {
        let known = &self.account.cars;
        let changed = list.iter().any(|v| match known.iter().find(|c| c.tesla_id == v.id) {
            None => true,
            Some(c) => c.display_name != v.display_name || (c.tesla_vehicle_id.is_none() && v.vehicle_id.is_some()),
        });
        if !changed {
            return;
        }
        let tags: Vec<Option<String>> = list.iter().map(|v| v.vin.as_ref().map(|x| self.store.vin_tag(x))).collect();
        let r = self.store.update(&self.id, None, |a, _| merge_vehicles(&mut a.cars, list, &tags));
        if let Ok((a, ())) = r {
            self.account = a;
            self.generation = self.store.generation();
            self.rebuild_cars();
        }
    }

    async fn data(&mut self, i: usize, now_ms: u64) -> Result<(), u64> {
        let Some(access) = self.creds.access.clone() else { return Err(now_ms + self.cadence.min_gap_ms) };
        let c = &self.cars[i];
        let location = match self.account.location {
            Location::Always => true,
            Location::Never => false,
            Location::DrivingOrCharging => c.just_woke || matches!(c.mode, Mode::Driving | Mode::Charging),
        };
        let id = c.tesla_id.clone();
        self.cars[i].last_poll_ms = Some(now_ms);
        let v = match self.tesla.vehicle_data(&access, &id, location).await {
            Ok(v) => v,
            Err(ApiError::Unavailable) => {
                // Asleep, offline or a weak signal: never woken; the list tells when it is back.
                self.cars[i].sleeping();
                self.cars[i].state = CarState::Unknown;
                self.list_next_ms = self.list_next_ms.min(now_ms + self.cadence.list_ms);
                return Ok(());
            }
            Err(ApiError::NotFound) => {
                self.cars[i].sleeping();
                self.cars[i].last_error = Some(err("not_found", "Tesla does not know this car", now_ms));
                self.list_next_ms = now_ms;
                return Ok(());
            }
            Err(e) => return Err(self.api_error(&e, now_ms)),
        };
        self.unauthorized_once = false;
        self.backoff_step = 0;
        self.set_state(AccountState::Ok, None, now_ms);
        let r = convert::reading(&v, now_ms);
        self.take_reading(i, r, now_ms);
        Ok(())
    }

    fn take_reading(&mut self, i: usize, r: Reading, now_ms: u64) {
        let fact = self.fact(i, now_ms, ApiState::Online);
        self.send(i, std::slice::from_ref(&r.sample), Some(fact));
        if let Some(car) = self.cars[i].car {
            let same =
                self.cars[i].last_meta.as_ref().is_some_and(|m| ApiMeta { at_ms: m.at_ms, ..r.meta.clone() } == *m);
            if !same {
                if let Err(e) = self.sink.meta(car, &r.meta) {
                    tracing::warn!(account = %self.id, error = %e, "Tesla values could not be stored");
                }
                self.cars[i].last_meta = Some(r.meta.clone());
            }
        }
        let smo = self.cars[i].car.is_some_and(|car| self.sink.smo_live(car));
        let cad = self.cadence.clone();
        let c = &mut self.cars[i];
        c.state = CarState::Online;
        c.last_data_ms = Some(now_ms);
        c.last_error = None;
        c.just_woke = false;
        let a = r.activity;
        if a.driving {
            c.mode = Mode::Driving;
            c.idle_since_ms = None;
        } else {
            c.stream = None;
            c.stream_refusals = 0;
            if a.charging {
                c.mode = Mode::Charging;
                c.idle_since_ms = None;
            } else if a.busy {
                c.mode = Mode::OnlineIdle;
                c.idle_since_ms = Some(now_ms);
            } else {
                let since = *c.idle_since_ms.get_or_insert(now_ms);
                // Still idle after a quiet window: straight back to waiting, no more polling meanwhile.
                if c.mode == Mode::LettingSleep || now_ms >= since + cad.idle_before_sleep_ms {
                    c.mode = Mode::LettingSleep;
                    c.quiet_until_ms = now_ms + cad.quiet_ms;
                } else {
                    c.mode = Mode::OnlineIdle;
                }
            }
        }
        if c.mode == Mode::Driving
            && !smo
            && c.stream.is_none()
            && c.stream_refusals < 2
            && let (Some(tag), Some(access)) = (c.tag.clone(), self.creds.access.as_ref())
        {
            c.stream = self.streamer.open(&tag, access);
        }
        if smo {
            c.stream = None;
        }
        let gap = match c.mode {
            Mode::Driving if c.stream.is_some() => cad.driving_streaming_ms,
            Mode::Driving => cad.driving_ms,
            Mode::Charging => cad.charging_ms,
            _ => cad.idle_ms,
        };
        c.next_data_ms = now_ms + if smo { gap.max(cad.smo_live_ms) } else { gap };
    }
}

/// Adds vehicles the account did not have, and keeps names and streaming ids current.
fn merge_vehicles(cars: &mut Vec<AccountCar>, list: &[Vehicle], tags: &[Option<String>]) {
    for (v, tag) in list.iter().zip(tags) {
        if let Some(c) = cars.iter_mut().find(|c| c.tesla_id == v.id) {
            c.display_name.clone_from(&v.display_name);
            if v.vehicle_id.is_some() {
                c.tesla_vehicle_id.clone_from(&v.vehicle_id);
            }
            continue;
        }
        let next = cars.iter().filter_map(|c| c.vehicle.strip_prefix("tv")?.parse::<u64>().ok()).max().unwrap_or(0) + 1;
        let info = v.vin.as_ref().and_then(|x| timeline::decode_vin(x.expose(), 0));
        cars.push(AccountCar {
            vehicle: format!("tv{next}"),
            tesla_id: v.id.clone(),
            tesla_vehicle_id: v.vehicle_id.clone(),
            vin_prefix: info.as_ref().map(|i| i.vin_prefix.clone()),
            vin_tag: tag.clone(),
            model: info.as_ref().and_then(|i| i.model),
            year: info.as_ref().and_then(|i| i.year),
            display_name: v.display_name.clone(),
            matched: None,
        });
    }
}

pub(crate) fn account_cars(list: &[Vehicle], store: &Store) -> Vec<AccountCar> {
    let mut cars = Vec::new();
    let tags: Vec<Option<String>> = list.iter().map(|v| v.vin.as_ref().map(|x| store.vin_tag(x))).collect();
    merge_vehicles(&mut cars, list, &tags);
    cars
}

/// Where "now" comes from (tests use their own).
pub trait Clock: Send + Sync + 'static {
    fn now_ms(&self) -> u64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }
}

impl<T: Tesla, S: Sink, St: Streamer> Poller<T, S, St> {
    /// Runs until the account is removed: ticks, publishes the status, sleeps until the next tick or until `wake`.
    pub async fn run(
        mut self,
        clock: impl Clock,
        wake: Arc<tokio::sync::Notify>,
        status: Arc<std::sync::Mutex<Option<AccountStatus>>>,
    ) {
        loop {
            let now = clock.now_ms();
            let Some(next) = self.tick(now).await else { break };
            if let Ok(mut s) = status.lock() {
                *s = Some(self.status());
            }
            let wait = next.saturating_sub(clock.now_ms());
            tokio::select! {
                () = tokio::time::sleep(std::time::Duration::from_millis(wait)) => {}
                () = wake.notified() => {}
            }
        }
        if let Ok(mut s) = status.lock() {
            *s = None;
        }
    }
}
