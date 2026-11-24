//! One task per account that reads TeslaFi's polls of the car, politely, and never asks for anything else.
//!
//! | Mode | When | Reads |
//! |---|---|---|
//! | `off` | account paused, the token refused, no car matched yet (after the first look) | none |
//! | `driving` / `charging` | TeslaFi's last poll says so | the latest poll every 60 s (`often`: 15 s; `gentle`: 2 min) |
//! | `idle` | awake, parked | every 3 min (`often`: 1 min; `gentle`: 5 min) |
//! | `asleep` | TeslaFi says the car sleeps (or is offline) | every 10 min (`often`: 3 min; `gentle`: 20 min) |
//! | `backoff` | errors, TeslaFi's wait, the daily cap | 429: TeslaFi's wait (at least 60 s, else 5 min); others: 60 s doubling to 15 min |
//!
//! The waits are the [`Pace`] an admin chooses (`normal` above). `often` reads about as often as TeslaFi itself polls
//! the car while it drives or charges (TeslaFi's data changes no faster, so reading faster would only repeat it).
//! The pace never slows because SMO or the Tesla API is live for the car: every input keeps reading. At most one request every 10 s and 1,500 a day per account (`often`: 6,000) (then nothing until midnight UTC), the checks of
//! a token included (their requests are written into the pacing). TeslaFi's own poll time is the time of the data, and
//! a poll already handed on (the same `data_id`) is not handed on again.
//! A refused token stops the account until an admin gives a new one. The back-off, TeslaFi's wait and the day's
//! count are saved with the account, so a restart does not start asking again early.

use crate::api::{Feed, FeedError, Read};
use crate::store::{
    Account, AccountCar, AccountState, ErrorInfo, Match, Pacing, Store, StoreError, car_of, next_vehicle,
};
use import::teslafi::CarState as FiState;
use import::teslafi::feed::{self, FeedPoll};
use model::{CarId, Sample};
use serde::Serialize;
use std::sync::Arc;
pub use tesla::{ApiFact, ApiState, Clock, NewCar, SinkError, SystemClock};
use tesla::{FullVin, Secret};

/// The longest wait taken from TeslaFi's `retry-after`.
pub const MAX_WAIT_MS: u64 = 3_600_000;

/// Where the poller's data goes (the engine implements it).
pub trait Sink: Send + Sync + 'static {
    /// Makes a car for a TeslaFi car the admin chose to add as new.
    fn new_car(&self, car: &NewCar) -> Result<CarId, SinkError>;
    /// Samples (by time) and what TeslaFi said about the car's state.
    fn points(&self, car: CarId, samples: &[Sample], facts: &[ApiFact]) -> Result<(), SinkError>;
    /// The values no CAN message carries, as a new poll (TeslaFi's time `at_ms`) gave them; only when it gave some.
    fn off_can(&self, car: CarId, at_ms: u64, values: &model::OffCan) -> Result<(), SinkError>;
    /// The account's state changed (for an audit entry or a notice).
    fn account_changed(&self, _account: &Account) {}
    /// Charges with a cost from TeslaFi's log ([`crate::costs`]): kept, one per key. Returns how many were new.
    fn invoices(&self, _found: &[tesla::invoices::Found]) -> Result<u64, SinkError> {
        Ok(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Off,
    Asleep,
    Idle,
    Charging,
    Driving,
    Backoff,
}

/// The timing, ms (and the daily cap).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cadence {
    pub driving_ms: u64,
    pub charging_ms: u64,
    pub idle_ms: u64,
    pub asleep_ms: u64,
    pub min_gap_ms: u64,
    pub daily_cap: u64,
    pub backoff_first_ms: u64,
    pub backoff_max_ms: u64,
    pub rate_limit_min_ms: u64,
    pub rate_limit_default_ms: u64,
    /// Two looks at the same state this close together make one span.
    pub fact_join_ms: u64,
    /// Read the costs TeslaFi lists for its charges ([`crate::costs`]): off until an admin turns them on.
    pub invoices: bool,
    /// Requests for them at most a day, and the requests of the daily cap they always leave to the polls.
    pub invoice_daily_cap: u64,
    pub invoice_reserve: u64,
    pub invoice_check_ms: u64,
    pub invoice_after_charge_ms: u64,
    pub invoice_error_first_ms: u64,
    pub invoice_error_max_ms: u64,
}

impl Cadence {
    /// How far apart two looks at the same state may be and still make one span: [`Cadence::fact_join_ms`], or one
    /// and a half of the slowest read when that is slower (looks further apart than the join would all be
    /// zero-length, and the car would never be recorded asleep or online between them).
    pub fn join_ms(&self) -> u64 {
        let slowest = [self.asleep_ms, self.idle_ms, self.charging_ms, self.driving_ms].into_iter().max().unwrap_or(0);
        self.fact_join_ms.max(slowest.saturating_add(slowest / 2))
    }
}

impl Default for Cadence {
    fn default() -> Self {
        Cadence {
            driving_ms: 60_000,
            charging_ms: 60_000,
            idle_ms: 3 * 60_000,
            asleep_ms: 10 * 60_000,
            min_gap_ms: 10_000,
            daily_cap: 1_500,
            backoff_first_ms: 60_000,
            backoff_max_ms: 15 * 60_000,
            rate_limit_min_ms: 60_000,
            rate_limit_default_ms: 5 * 60_000,
            fact_join_ms: 15 * 60_000,
            invoices: false,
            invoice_daily_cap: 60,
            invoice_reserve: 200,
            invoice_check_ms: 24 * 3_600_000,
            invoice_after_charge_ms: 30 * 60_000,
            invoice_error_first_ms: 30 * 60_000,
            invoice_error_max_ms: 24 * 3_600_000,
        }
    }
}

/// How often TeslaFi is read, as an admin chooses it (`/integrations/teslafi` `poll`). The gap between requests,
/// TeslaFi's `retry-after` and the back-off on errors stay as they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Pace {
    /// As often as TeslaFi refreshes its data: driving or charging every 15 s, awake 1 min, asleep 3 min (at most
    /// 6,000 requests a day).
    Often,
    #[default]
    Normal,
    /// Fewer requests: driving or charging every 2 min, awake 5 min, asleep 20 min.
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
                driving_ms: 15_000,
                charging_ms: 15_000,
                idle_ms: 60_000,
                asleep_ms: 3 * 60_000,
                daily_cap: 6_000,
                ..n
            },
            Pace::Gentle => {
                Cadence { driving_ms: 120_000, charging_ms: 120_000, idle_ms: 5 * 60_000, asleep_ms: 20 * 60_000, ..n }
            }
        }
    }
}

/// One account's polling, for the status page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountStatus {
    pub account: String,
    pub state: AccountState,
    pub mode: Mode,
    /// The car TeslaFi's polls are of now (`fv1`), when known.
    pub vehicle: Option<String>,
    pub car: Option<CarId>,
    /// TeslaFi's word for what the car does (`Driving`, `Sleeping`...).
    pub car_state: Option<String>,
    pub polls_today: u64,
    pub last_request_ms: Option<u64>,
    pub next_request_ms: Option<u64>,
    /// TeslaFi's time of the newest poll handed on.
    pub last_data_ms: Option<u64>,
    pub last_ok_ms: Option<u64>,
    pub last_error: Option<ErrorInfo>,
    /// The TLS version of the last connection (`1.3`, or `1.2` when TeslaFi's host offered no 1.3).
    pub tls: Option<&'static str>,
}

fn err(code: &str, message: impl Into<String>, at_ms: u64) -> ErrorInfo {
    ErrorInfo { code: code.to_string(), message: message.into(), at_ms }
}

/// The poller of one account.
pub struct Poller<F: Feed, S: Sink> {
    id: String,
    store: Store,
    feed: F,
    sink: Arc<S>,
    cadence: Cadence,
    default_tz: chrono_tz::Tz,
    tls: Arc<dyn Fn() -> Option<&'static str> + Send + Sync>,
    account: Account,
    token: Secret,
    generation: u64,
    pacing: Pacing,
    saved_pacing: Pacing,
    mode: Mode,
    next_ms: u64,
    /// The car the polls are of now (index into the account's cars).
    current: Option<String>,
    car_state: Option<String>,
    last_seen: Option<(u64, ApiState)>,
    last_ok_ms: Option<u64>,
    last_error: Option<ErrorInfo>,
    last_ok_written_ms: u64,
    now_ms: u64,
    /// The account has no car yet and its latest poll named none: the next read asks for the last poll awake.
    last_good_due: bool,
    /// The last poll awake was asked for once by this poller: not again (the latest polls name the car once TeslaFi
    /// knows it).
    last_good_asked: bool,
}

impl<F: Feed, S: Sink> Poller<F, S> {
    pub fn new(
        store: Store,
        id: &str,
        feed: F,
        sink: Arc<S>,
        cadence: Cadence,
        default_tz: chrono_tz::Tz,
    ) -> Result<Self, StoreError> {
        let generation = store.generation();
        let account = store.get(id)?;
        let token = store.token(id)?;
        let pacing = match store.pacing(id) {
            Ok(p) => p,
            Err(StoreError::NotFound) => return Err(StoreError::NotFound),
            Err(_) => Pacing::default(),
        };
        let current = (account.cars.len() == 1).then(|| account.cars[0].vehicle.clone());
        Ok(Poller {
            id: id.to_string(),
            store,
            feed,
            sink,
            cadence,
            default_tz,
            tls: Arc::new(|| None),
            last_ok_ms: account.last_ok_ms,
            last_error: account.last_error.clone(),
            account,
            token,
            generation,
            saved_pacing: pacing.clone(),
            pacing,
            mode: Mode::Asleep,
            next_ms: 0,
            current,
            car_state: None,
            last_seen: None,
            last_ok_written_ms: 0,
            now_ms: 0,
            last_good_due: false,
            last_good_asked: false,
        })
    }

    /// Where the status's TLS version comes from (the real client's last connection).
    pub fn with_tls(mut self, tls: Arc<dyn Fn() -> Option<&'static str> + Send + Sync>) -> Self {
        self.tls = tls;
        self
    }

    fn car_of(&self, vehicle: &str) -> Option<&AccountCar> {
        self.account.cars.iter().find(|c| c.vehicle == vehicle)
    }

    fn matched(&self) -> Option<CarId> {
        let v = self.current.as_deref()?;
        match self.car_of(v)?.matched {
            Some(Match::Car { car }) => Some(car),
            _ => None,
        }
    }

    fn idle(&self) -> bool {
        self.account.paused || self.account.state == AccountState::Refused
    }

    pub fn status(&self) -> AccountStatus {
        let mode = if self.idle() {
            Mode::Off
        } else if self.now_ms < self.pacing.backoff_until_ms {
            Mode::Backoff
        } else {
            self.mode
        };
        AccountStatus {
            account: self.id.clone(),
            state: self.account.state,
            mode,
            vehicle: self.current.clone(),
            car: self.matched(),
            car_state: self.car_state.clone(),
            polls_today: self.pacing.polls_today,
            last_request_ms: self.pacing.last_request_ms,
            next_request_ms: match mode {
                Mode::Off => None,
                Mode::Backoff => Some(self.pacing.backoff_until_ms),
                _ => Some(self.next_ms.max(self.pacing.backoff_until_ms)),
            },
            last_data_ms: self.pacing.last_poll_ms,
            last_ok_ms: self.last_ok_ms,
            last_error: self.last_error.clone(),
            tls: (self.tls)(),
        }
    }

    /// Reads the account again when someone changed it. `false`: it is gone.
    fn sync(&mut self) -> bool {
        let g = self.store.generation();
        if g == self.generation {
            return true;
        }
        self.generation = g;
        match (self.store.get(&self.id), self.store.token(&self.id)) {
            (Ok(a), Ok(t)) => {
                let new_token = t != self.token;
                self.account = a;
                self.token = t;
                if let Ok(p) = self.store.pacing(&self.id) {
                    // requests a token's check sent meanwhile: the gap after them, and the day's count
                    self.pacing.merge_requests(&p);
                    if new_token {
                        // the admin gave a new token: start over (the store dropped the old waits)
                        self.pacing.backoff_until_ms = p.backoff_until_ms;
                        self.pacing.backoff_step = p.backoff_step;
                    }
                }
                if new_token {
                    self.next_ms = 0;
                }
                if self.current.as_deref().is_some_and(|v| self.car_of(v).is_none()) {
                    self.current = None;
                }
                if self.current.is_none() && self.account.cars.len() == 1 {
                    self.current = Some(self.account.cars[0].vehicle.clone());
                }
                true
            }
            (Err(StoreError::NotFound), _) | (_, Err(StoreError::NotFound)) => false,
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!(account = %self.id, error = %e, "the TeslaFi account could not be read again");
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
        match self.store.update(&self.id, None, None, |a| {
            a.state = state;
            a.last_error = error;
            a.last_ok_ms = last_ok;
        }) {
            Ok((a, ())) => {
                self.last_ok_written_ms = now_ms;
                // the generation is left behind on purpose: an admin's change made while a request was out is read
                // again by the next tick
                self.account = a;
                if changed {
                    tracing::info!(account = %self.id, state = ?state, "teslafi account state");
                    self.sink.account_changed(&self.account);
                }
            }
            Err(e) => tracing::warn!(account = %self.id, error = %e, "the TeslaFi account could not be saved"),
        }
    }

    fn back_off(&mut self, now_ms: u64, wait_ms: Option<u64>) -> u64 {
        let wait = wait_ms.unwrap_or_else(|| {
            let w = self.cadence.backoff_first_ms.saturating_mul(1u64 << self.pacing.backoff_step.min(16));
            self.pacing.backoff_step += 1;
            w.min(self.cadence.backoff_max_ms)
        });
        self.pacing.backoff_until_ms = now_ms.saturating_add(wait.min(MAX_WAIT_MS));
        self.pacing.backoff_until_ms
    }

    /// May a request go out now? `Err(t)`: not before `t`.
    fn gate(&mut self, now_ms: u64) -> Result<(), u64> {
        if let Some(last) = self.pacing.last_request_ms
            && now_ms < last + self.cadence.min_gap_ms
        {
            return Err(last + self.cadence.min_gap_ms);
        }
        if self.pacing.polls_today >= self.cadence.daily_cap {
            let midnight = (now_ms / 86_400_000 + 1) * 86_400_000;
            self.set_state(
                AccountState::Ok,
                Some(err("daily_cap", "the daily number of requests to TeslaFi was reached", now_ms)),
                now_ms,
            );
            self.pacing.backoff_until_ms = midnight;
            return Err(midnight);
        }
        self.pacing.last_request_ms = Some(now_ms);
        self.pacing.polls_today += 1;
        Ok(())
    }

    fn save_pacing(&mut self) {
        if self.pacing == self.saved_pacing {
            return;
        }
        match self.store.set_pacing(&self.id, &self.pacing) {
            Ok(()) => self.saved_pacing = self.pacing.clone(),
            Err(StoreError::NotFound) => {}
            Err(e) => tracing::warn!(account = %self.id, error = %e, "the TeslaFi poller's pacing could not be saved"),
        }
    }

    /// Does what is due at `now_ms`; returns when to come back. `None`: the account is gone, stop.
    pub async fn tick(&mut self, now_ms: u64) -> Option<u64> {
        // the server's clock was set back (a wrong clock corrected, a machine resumed): every due time moves back as
        // far, else nothing would be read until the clock had caught up with the old times
        if self.now_ms > now_ms.saturating_add(60_000) {
            let back = self.now_ms - now_ms;
            self.next_ms = self.next_ms.saturating_sub(back);
            self.pacing.backoff_until_ms = self.pacing.backoff_until_ms.saturating_sub(back);
            if let Some(t) = &mut self.pacing.last_request_ms {
                *t = t.saturating_sub(back);
            }
            self.last_seen = None;
            if let Some(t) = &mut self.last_ok_ms {
                *t = t.saturating_sub(back);
            }
            self.last_ok_written_ms = self.last_ok_written_ms.saturating_sub(back);
            // the stored copy too (the next read of the account merges its last request in), with the requests a
            // token's check wrote meanwhile, their time moved back as well
            if let Ok(mut stored) = self.store.pacing(&self.id) {
                if let Some(t) = &mut stored.last_request_ms {
                    *t = t.saturating_sub(back);
                }
                self.pacing.merge_requests(&stored);
            }
            match self.store.set_pacing_exact(&self.id, &self.pacing) {
                Ok(()) => self.saved_pacing = self.pacing.clone(),
                Err(e) => {
                    tracing::warn!(account = %self.id, error = %e, "the TeslaFi poller's pacing could not be saved")
                }
            }
        }
        self.now_ms = now_ms;
        let next = self.tick_inner(now_ms).await;
        if next.is_some() {
            self.save_pacing();
        }
        next
    }

    async fn tick_inner(&mut self, now_ms: u64) -> Option<u64> {
        if !self.sync() {
            return None;
        }
        if now_ms / 86_400_000 != self.pacing.day {
            self.pacing.day = now_ms / 86_400_000;
            self.pacing.polls_today = 0;
        }
        if now_ms / 86_400_000 != self.pacing.costs.day {
            self.pacing.costs.day = now_ms / 86_400_000;
            self.pacing.costs.requests_today = 0;
        }
        let later = now_ms + self.cadence.idle_ms;
        if self.idle() {
            self.mode = Mode::Off;
            return Some(later);
        }
        self.make_new_cars();
        if now_ms < self.pacing.backoff_until_ms {
            return Some(self.pacing.backoff_until_ms);
        }
        // a car TeslaFi sent polls of, none of them matched yet: nothing is read until an admin matches one
        if !self.account.cars.is_empty()
            && !self.account.cars.iter().any(|c| matches!(c.matched, Some(Match::Car { .. })))
        {
            self.mode = Mode::Off;
            return Some(later);
        }
        if now_ms < self.next_ms {
            // the charge costs only between the reads of the car, never instead of one
            if let Some(t) = self.costs_due(now_ms) {
                if t <= now_ms && now_ms + self.cadence.min_gap_ms <= self.next_ms {
                    if let Err(t) = self.gate(now_ms) {
                        return Some(t);
                    }
                    self.costs_step(now_ms).await;
                    return Some((now_ms + self.cadence.min_gap_ms).min(self.next_ms).max(now_ms + 1));
                }
                return Some(self.next_ms.min(t.max(now_ms + 1)));
            }
            return Some(self.next_ms);
        }
        if let Err(t) = self.gate(now_ms) {
            return Some(t);
        }
        // an account whose car is not known yet, its latest poll naming none: once, the last poll of the car awake
        let what = if self.account.cars.is_empty() && self.last_good_due { Read::LastGood } else { Read::Latest };
        match self.feed.read(&self.token, what).await {
            Ok(poll) => {
                self.pacing.backoff_step = 0;
                self.take(&poll, what, now_ms);
                let gap = now_ms + self.cadence.min_gap_ms;
                // the charge costs in the gap before the next read, when due
                let costs = self.costs_due(now_ms).map_or(u64::MAX, |t| t.max(gap));
                Some(self.next_ms.min(costs).max(gap))
            }
            Err(e) => Some(self.failed(&e, now_ms)),
        }
    }

    fn failed(&mut self, e: &FeedError, now_ms: u64) -> u64 {
        tracing::warn!(account = %self.id, code = %e.code(), "teslafi request failed");
        match e {
            FeedError::Refused => {
                self.set_state(AccountState::Refused, Some(err("token_refused", e.to_string(), now_ms)), now_ms);
                self.mode = Mode::Off;
                now_ms + self.cadence.idle_ms
            }
            FeedError::RateLimited { retry_after_s } => {
                let wait = retry_after_s
                    .map_or(self.cadence.rate_limit_default_ms, |s| s.saturating_mul(1000))
                    .clamp(self.cadence.rate_limit_min_ms, MAX_WAIT_MS.max(self.cadence.rate_limit_min_ms));
                self.last_error = Some(err("rate_limited", e.to_string(), now_ms));
                self.back_off(now_ms, Some(wait))
            }
            other => {
                let error = err(other.code(), other.to_string(), now_ms);
                if self.pacing.backoff_step >= 3 {
                    self.set_state(AccountState::Error, Some(error), now_ms);
                } else {
                    self.last_error = Some(error);
                }
                self.back_off(now_ms, None)
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
                    let r = self.store.update(&self.id, None, None, |a| {
                        if let Some(x) = a.cars.iter_mut().find(|x| x.vehicle == c.vehicle) {
                            x.matched = Some(Match::Car { car });
                        }
                    });
                    if let Ok((a, ())) = r {
                        self.account = a;
                        self.generation = self.store.generation();
                    }
                }
                Err(e) => tracing::warn!(account = %self.id, error = %e, "a car for a TeslaFi car could not be made"),
            }
        }
    }

    /// The account's car for a VIN TeslaFi named: known, or added (not matched: an admin says which car it is).
    fn note_vin(&mut self, vin: &str, name: Option<&str>) -> Option<String> {
        let tag = self.store.vin_tag(&FullVin::new(vin));
        if let Some(c) = self.account.cars.iter().find(|c| c.vin_tag.as_deref() == Some(tag.as_str())) {
            let v = c.vehicle.clone();
            if c.display_name.as_deref() != name && name.is_some() {
                let n = name.map(str::to_string);
                if let Ok((a, ())) = self.store.update(&self.id, None, None, |a| {
                    if let Some(x) = a.cars.iter_mut().find(|x| x.vehicle == v) {
                        x.display_name = n;
                    }
                }) {
                    self.account = a;
                    self.generation = self.store.generation();
                }
            }
            return Some(v);
        }
        let car = car_of(&self.store, vin, name.map(str::to_string), next_vehicle(&self.account.cars));
        let v = car.vehicle.clone();
        match self.store.update(&self.id, None, None, |a| a.cars.push(car)) {
            Ok((a, ())) => {
                tracing::info!(account = %self.id, vehicle = %v, "teslafi sent polls of a car not matched yet");
                self.account = a;
                self.generation = self.store.generation();
                Some(v)
            }
            Err(e) => {
                tracing::warn!(account = %self.id, error = %e, "the TeslaFi account could not be saved");
                None
            }
        }
    }

    fn fact(&mut self, at_ms: u64, state: ApiState) -> ApiFact {
        let f = match self.last_seen {
            Some((t, s)) if s == state && at_ms >= t && at_ms - t <= self.cadence.join_ms() => {
                ApiFact { start_ms: t, end_ms: at_ms, state }
            }
            _ => ApiFact { start_ms: at_ms, end_ms: at_ms, state },
        };
        self.last_seen = Some((at_ms, state));
        f
    }

    fn tz(&self) -> chrono_tz::Tz {
        self.account.time_zone.as_deref().and_then(|z| z.parse().ok()).unwrap_or(self.default_tz)
    }

    fn mode_of(&self, p: &FeedPoll) -> Mode {
        if p.driving || p.car_state.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("driving")) {
            Mode::Driving
        } else if p.charging {
            Mode::Charging
        } else {
            match p.state {
                Some(FiState::Asleep | FiState::Offline) => Mode::Asleep,
                _ if p.car_state.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("sleeping")) => Mode::Asleep,
                _ => Mode::Idle,
            }
        }
    }

    fn take(&mut self, raw: &crate::api::Poll, what: Read, now_ms: u64) {
        if what == Read::LastGood {
            self.last_good_due = false;
            self.last_good_asked = true;
        }
        // TeslaFi's times are local times of the account's zone: after a change of zone the time of the last poll
        // handed on is not comparable any more (a zone east of the old one makes every new poll look older), so
        // until a poll is read in the new zone only its id tells a poll handed on already
        let tz = self.tz();
        if self.pacing.poll_zone.as_deref() != Some(tz.name()) {
            if self.pacing.poll_zone.is_some() {
                self.pacing.last_poll_ms = None;
                self.last_seen = None;
            }
            self.pacing.poll_zone = Some(tz.name().to_string());
        }
        let fields = feed::fields_of(raw);
        let p: FeedPoll = match feed::read(&fields, tz, self.pacing.last_poll_ms) {
            Ok(p) => p,
            Err(e) => {
                self.last_error = Some(err("bad_answer", e.to_string(), now_ms));
                self.next_ms = now_ms + self.cadence.idle_ms;
                return;
            }
        };
        self.set_state(AccountState::Ok, None, now_ms);
        if let Some(vin) = &p.vin {
            self.current = self.note_vin(vin, p.display_name.as_deref());
        }
        // the last poll awake is an old one: it says which car, not what the car does now
        if what == Read::Latest {
            self.car_state = p.car_state.clone();
            let mode = self.mode_of(&p);
            if self.mode == Mode::Charging && mode != Mode::Charging && self.cadence.invoices {
                // a charge ended: TeslaFi lists its cost once Tesla billed it
                let c = &mut self.pacing.costs;
                c.next_check_ms = c.next_check_ms.min(now_ms + self.cadence.invoice_after_charge_ms);
            }
            self.mode = mode;
        }
        // a poll already handed on (the same id), or one older than it (the last awake poll asked for later): not
        // again
        let new = match p.data_id {
            Some(id) => Some(id) != self.pacing.last_data_id,
            None => true,
        } && self.pacing.last_poll_ms.is_none_or(|t| p.t_ms > t);
        let car = self.matched();
        if new {
            self.pacing.last_data_id = p.data_id.or(self.pacing.last_data_id);
            self.pacing.last_poll_ms = Some(p.t_ms.max(self.pacing.last_poll_ms.unwrap_or(0)));
            if let Some(car) = car {
                let state = match (p.state, self.mode) {
                    (Some(FiState::Offline), _) => Some(ApiState::Offline),
                    (_, Mode::Asleep) => Some(ApiState::Asleep),
                    (Some(FiState::Online), _) | (_, Mode::Driving | Mode::Charging | Mode::Idle) => {
                        Some(ApiState::Online)
                    }
                    _ => None,
                };
                let facts: Vec<ApiFact> = state.map(|s| self.fact(p.t_ms, s)).into_iter().collect();
                let samples: Vec<Sample> = if p.has_values() { vec![p.sample.clone()] } else { Vec::new() };
                if (!samples.is_empty() || !facts.is_empty())
                    && let Err(e) = self.sink.points(car, &samples, &facts)
                {
                    tracing::warn!(account = %self.id, error = %e, "TeslaFi data could not be stored");
                }
                if !p.off_can.is_empty()
                    && let Err(e) = self.sink.off_can(car, p.t_ms, &p.off_can)
                {
                    tracing::warn!(account = %self.id, error = %e, "TeslaFi's values could not be stored");
                }
            }
        }
        let gap = match self.mode {
            Mode::Driving => self.cadence.driving_ms,
            Mode::Charging => self.cadence.charging_ms,
            Mode::Asleep => self.cadence.asleep_ms,
            _ => self.cadence.idle_ms,
        };
        // the first look at an account whose car is not known yet, the latest poll naming none: once more soon, for
        // the last poll of the car awake; after that the normal pace (an awake car whose polls name no car is not
        // asked every 10 s)
        let gap = if self.account.cars.is_empty() && what == Read::Latest && !self.last_good_asked {
            self.last_good_due = true;
            self.cadence.min_gap_ms
        } else {
            gap
        };
        self.next_ms = now_ms + gap;
    }
}

/// The charge costs ([`crate::costs`]): one request per tick, between the reads of the car.
impl<F: Feed, S: Sink> Poller<F, S> {
    fn costs_due(&self, now_ms: u64) -> Option<u64> {
        let cad = &self.cadence;
        if !cad.invoices || self.account.state != AccountState::Ok || self.matched().is_none() {
            return None;
        }
        let c = &self.pacing.costs;
        let midnight = (now_ms / 86_400_000 + 1) * 86_400_000;
        if c.requests_today >= cad.invoice_daily_cap || self.pacing.polls_today + cad.invoice_reserve >= cad.daily_cap {
            return Some(midnight);
        }
        let want = if c.backfill_done { c.next_check_ms } else { now_ms };
        Some(want.max(c.wait_until_ms))
    }

    async fn costs_step(&mut self, now_ms: u64) {
        let today = (now_ms / 86_400_000) as i64;
        self.pacing.costs.requests_today += 1;
        let (from, to) = if self.pacing.costs.backfill_done {
            (today - crate::costs::CHECK_DAYS, today)
        } else {
            let before = self.pacing.costs.backfill_before.unwrap_or(today + 1);
            ((before - crate::costs::CHUNK_DAYS).max(crate::costs::OLDEST_DAY), before - 1)
        };
        let answer = self.feed.charges(&self.token, from, to).await;
        let records = match answer {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(account = %self.id, code = %e.code(), "teslafi charge costs request failed");
                let message = match &e {
                    FeedError::NotEnabled | FeedError::NotData => {
                        "TeslaFi gave no charge log for this token".to_string()
                    }
                    other => other.to_string(),
                };
                let (first, max) = (self.cadence.invoice_error_first_ms, self.cadence.invoice_error_max_ms);
                self.pacing.costs.note_error(err(e.code(), message, now_ms), first, max);
                if let FeedError::RateLimited { retry_after_s } = e {
                    let wait = retry_after_s.map_or(self.cadence.rate_limit_default_ms, |s| s.saturating_mul(1000));
                    self.back_off(now_ms, Some(wait.max(self.cadence.rate_limit_min_ms)));
                }
                return;
            }
        };
        let tz = self.tz();
        let car = self.matched();
        let vehicle = self.current.clone();
        let found: Vec<tesla::invoices::Found> = records
            .iter()
            .filter_map(|r| crate::costs::session(r, tz))
            .filter_map(|s| {
                let id: String =
                    s.id.chars().filter(|c| c.is_ascii_alphanumeric() || "_.-".contains(*c)).take(120).collect();
                (!id.is_empty()).then(|| tesla::invoices::Found {
                    key: format!("teslafi-{id}"),
                    account: self.id.clone(),
                    vehicle: vehicle.clone(),
                    car,
                    session: s,
                })
            })
            .collect();
        match self.sink.invoices(&found) {
            Ok(n) => {
                let c = &mut self.pacing.costs;
                c.found += n;
                c.errors = 0;
                c.last_error = None;
            }
            Err(e) => {
                tracing::warn!(account = %self.id, error = %e, "TeslaFi's charge costs could not be stored");
                let (first, max) = (self.cadence.invoice_error_first_ms, self.cadence.invoice_error_max_ms);
                self.pacing.costs.note_error(err("store_failed", "the costs could not be stored", now_ms), first, max);
                return;
            }
        }
        let c = &mut self.pacing.costs;
        if c.backfill_done {
            c.last_check_ms = Some(now_ms);
            c.next_check_ms = now_ms + self.cadence.invoice_check_ms;
        } else {
            c.backfill_before = Some(from);
            c.empty_chunks = if records.is_empty() { c.empty_chunks + 1 } else { 0 };
            if c.empty_chunks >= 2 || from <= crate::costs::OLDEST_DAY {
                c.backfill_done = true;
                c.last_check_ms = Some(now_ms);
                c.next_check_ms = now_ms + self.cadence.invoice_check_ms;
            }
        }
    }
}

impl<F: Feed, S: Sink> Poller<F, S> {
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
