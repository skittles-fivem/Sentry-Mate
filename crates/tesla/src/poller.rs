//! One task per account that reads its cars without ever waking one, and lets them fall asleep.
//!
//! The cadence (all in [`Cadence`]; the four reads are the admin's [`Intervals`], each at its default below or slower,
//! never faster):
//!
//! | Mode | When | Calls |
//! |---|---|---|
//! | `off` | account paused, sign-in needed, car not matched | none |
//! | `watching_list` | the list says asleep or offline (or a 408) | the product list every 30 s (at least 30 s; one call for all cars; it never wakes a car); `vehicle_data` again only once the list says online |
//! | `online_idle` | online, parked, not charging | `vehicle_data` every 60 s (at least 60 s) |
//! | `letting_sleep` | online and idle for 10 min with nothing keeping it awake | no `vehicle_data` for 21 min, only the list; then one look, and back to waiting if still idle |
//! | `charging` | charging | `vehicle_data` every 5 s (at least 5 s) |
//! | `driving` | gear D, R or N, or moving | the streaming socket, and `vehicle_data` every 2.5 s (at least 2.5 s) beside it |
//! | `backoff` | errors | 429: Tesla's wait (`Retry-After`, else `RateLimit-Reset`; at least 60 s, 5 min without one, at most 24 h); 5xx and network: 30 s doubling to 15 min |
//!
//! The pace never slows because SMO (or another input) is live for the car: every input keeps reading.
//! This server sets no limit of its own on how many requests it makes: no daily cap, no gap between requests beyond
//! the intervals above. Tesla does the limiting, and its 429 is waited out as it asks; each new such wait is handed to
//! the [`Sink`] ([`Sink::rate_limited`]) so the admins are told, and the status says until when
//! ([`AccountStatus::rate_limited_until_ms`]). Requests go one at a time per account, and the day's count
//! (`polls_today`, from midnight UTC) is kept only to be shown. The access token is refreshed when under 30 min (or
//! a quarter of a short token's life) are left, and once on a 401; a refused refresh stops the account until an
//! admin signs in again. The new refresh token is stored before the new access token is used. The back-off, Tesla's
//! wait, a closed endpoint and the day's count are saved with the account, so a restart or a settings change does not
//! start asking again early.

use crate::api::{ApiError, CarState, Tesla, Vehicle};
use crate::auth::AuthError;
use crate::convert::{self, ApiMeta, Reading};
use crate::store::{
    Account, AccountCar, AccountState, Credentials, ErrorInfo, Location, Match, Pacing, Store, StoreError,
};
use crate::stream::{StreamHandle, Streamer};
use model::{CarId, Sample};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The longest wait taken from Tesla's `retry-after` (a larger one is read as this).
pub const MAX_WAIT_MS: u64 = 24 * 3_600_000;

/// After a request, the next tick comes this soon (ms): requests go one at a time, with no gap of their own between
/// them; what is due waits for its interval.
const NEXT_MS: u64 = 1;

/// Where the poller's data goes (the engine implements it).
pub trait Sink: Send + Sync + 'static {
    /// Makes a car for a vehicle the admin chose to add as new.
    fn new_car(&self, car: &NewCar) -> Result<CarId, SinkError>;
    /// Samples (by time) and what the API said about the car's state over spans.
    fn points(&self, car: CarId, samples: &[Sample], facts: &[ApiFact]) -> Result<(), SinkError>;
    /// Values beside the samples (version, settings, charger), each time they change.
    fn meta(&self, car: CarId, meta: &ApiMeta) -> Result<(), SinkError>;
    /// The account's state changed (for an audit entry or an event).
    fn account_changed(&self, _account: &Account) {}
    /// Tesla started limiting the account's requests (a 429 after none): nothing is asked before `until_ms`. Once per
    /// episode; a 429 during one only moves the end (in the status).
    fn rate_limited(&self, _account: &Account, _until_ms: u64) {}
    /// Supercharger sessions read from the charging history: kept (one per key). Returns how many were new.
    fn invoices(&self, _found: &[crate::invoices::Found]) -> Result<u64, SinkError> {
        Ok(0)
    }
    /// Whether a session is kept already.
    fn invoice_known(&self, _key: &str) -> bool {
        false
    }
    /// The sessions of the account whose PDF is not kept yet: `(key, invoice id, vehicle)`, oldest first.
    fn invoice_pdfs_wanted(&self, _account: &str) -> Vec<(String, String, Option<String>)> {
        Vec::new()
    }
    /// Keeps a session's PDF; `false`: not taken (no such session, or not a PDF).
    fn invoice_pdf(&self, _key: &str, _pdf: &[u8]) -> Result<bool, SinkError> {
        Ok(false)
    }
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

/// The timing, ms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cadence {
    pub list_ms: u64,
    pub idle_ms: u64,
    pub charging_ms: u64,
    pub driving_ms: u64,
    pub idle_before_sleep_ms: u64,
    pub quiet_ms: u64,
    pub refresh_before_ms: u64,
    pub backoff_first_ms: u64,
    pub backoff_max_ms: u64,
    pub rate_limit_min_ms: u64,
    pub rate_limit_default_ms: u64,
    pub refused_retry_ms: u64,
    pub stream_drain_ms: u64,
    /// Two looks at the same state this close together make one span.
    pub fact_join_ms: u64,
    /// Read Supercharger invoices ([`crate::invoices`]): off until an admin turns them on.
    pub invoices: bool,
    /// The daily check for new sessions, and the look soon after a DC charge ends.
    pub invoice_check_ms: u64,
    pub invoice_after_dc_ms: u64,
    /// After an invoice error: the first wait, doubling up to the longest.
    pub invoice_error_first_ms: u64,
    pub invoice_error_max_ms: u64,
}

impl Cadence {
    /// How far apart two looks at the same state may be and still make one span: [`Cadence::fact_join_ms`], or one
    /// and a half of the slowest read when that is slower (looks further apart than the join would all be
    /// zero-length, and the car would never be recorded asleep or online between them).
    pub fn join_ms(&self) -> u64 {
        let slowest = [self.list_ms, self.idle_ms, self.charging_ms, self.driving_ms].into_iter().max().unwrap_or(0);
        self.fact_join_ms.max(slowest.saturating_add(slowest / 2))
    }
}

impl Default for Cadence {
    fn default() -> Self {
        Cadence {
            list_ms: Intervals::DEFAULT.list_ms,
            idle_ms: Intervals::DEFAULT.idle_ms,
            charging_ms: Intervals::DEFAULT.charging_ms,
            driving_ms: Intervals::DEFAULT.driving_ms,
            idle_before_sleep_ms: 10 * 60_000,
            quiet_ms: 21 * 60_000,
            refresh_before_ms: 30 * 60_000,
            backoff_first_ms: 30_000,
            backoff_max_ms: 15 * 60_000,
            rate_limit_min_ms: 60_000,
            rate_limit_default_ms: 5 * 60_000,
            refused_retry_ms: 60 * 60_000,
            stream_drain_ms: 5_000,
            fact_join_ms: 5 * 60_000,
            invoices: false,
            invoice_check_ms: 24 * 3_600_000,
            invoice_after_dc_ms: 20 * 60_000,
            invoice_error_first_ms: 30 * 60_000,
            invoice_error_max_ms: 24 * 3_600_000,
        }
    }
}

/// How often the server asks, as an admin chooses it (`/integrations/tesla` `poll`): the four reads, ms. Each read
/// can be made slower than its default but never faster than its floor ([`Intervals::FLOOR`], the default itself);
/// letting the car sleep and the back-off stay as they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Intervals {
    /// `vehicle_data` while driving (the stream runs beside it).
    pub driving_ms: u64,
    /// `vehicle_data` while charging.
    pub charging_ms: u64,
    /// `vehicle_data` while online and idle.
    pub idle_ms: u64,
    /// The product list while asleep or offline (and while letting the car sleep).
    pub list_ms: u64,
}

impl Default for Intervals {
    fn default() -> Self {
        Intervals::DEFAULT
    }
}

impl Intervals {
    pub const DEFAULT: Intervals =
        Intervals { driving_ms: 2_500, charging_ms: 5_000, idle_ms: 60_000, list_ms: 30_000 };
    /// The fastest each read may be: the defaults.
    pub const FLOOR: Intervals = Intervals::DEFAULT;
    /// The slowest each read may be.
    pub const CEILING: Intervals =
        Intervals { driving_ms: 10 * 60_000, charging_ms: 30 * 60_000, idle_ms: 60 * 60_000, list_ms: 60 * 60_000 };

    /// The fields by name, with their value, floor and ceiling.
    pub fn fields(&self) -> [(&'static str, u64, u64, u64); 4] {
        let (f, c) = (Intervals::FLOOR, Intervals::CEILING);
        [
            ("driving_ms", self.driving_ms, f.driving_ms, c.driving_ms),
            ("charging_ms", self.charging_ms, f.charging_ms, c.charging_ms),
            ("idle_ms", self.idle_ms, f.idle_ms, c.idle_ms),
            ("list_ms", self.list_ms, f.list_ms, c.list_ms),
        ]
    }

    /// Within the floors and ceilings (a value saved out of them is read as the nearest one allowed).
    pub fn clamped(self) -> Intervals {
        let (f, c) = (Intervals::FLOOR, Intervals::CEILING);
        Intervals {
            driving_ms: self.driving_ms.clamp(f.driving_ms, c.driving_ms),
            charging_ms: self.charging_ms.clamp(f.charging_ms, c.charging_ms),
            idle_ms: self.idle_ms.clamp(f.idle_ms, c.idle_ms),
            list_ms: self.list_ms.clamp(f.list_ms, c.list_ms),
        }
    }

    /// The cadence of these intervals (the rest as in [`Cadence::default`]), within the floors.
    pub fn cadence(self) -> Cadence {
        let i = self.clamped();
        Cadence {
            list_ms: i.list_ms,
            idle_ms: i.idle_ms,
            charging_ms: i.charging_ms,
            driving_ms: i.driving_ms,
            ..Cadence::default()
        }
    }

    /// The intervals a cadence reads at.
    pub fn of(c: &Cadence) -> Intervals {
        Intervals { driving_ms: c.driving_ms, charging_ms: c.charging_ms, idle_ms: c.idle_ms, list_ms: c.list_ms }
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
    /// Requests made since midnight UTC: information only (no cap).
    pub polls_today: u64,
    /// Tesla is limiting the account's requests (it answered 429): nothing is asked before this.
    pub rate_limited_until_ms: Option<u64>,
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
    /// Charging at a DC charger (its end is when Tesla bills the session).
    dc: bool,
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
            dc: false,
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
    /// The end of Tesla's current 429 wait (0 or past: none).
    rate_limited_until_ms: u64,
    force_refresh: bool,
    unauthorized_once: bool,
    gone: bool,
    last_ok_ms: Option<u64>,
    last_error: Option<ErrorInfo>,
    last_ok_written_ms: u64,
    /// How long the last access token lasted (to refresh a short-lived one only near its end).
    token_lifetime_ms: Option<u64>,
    /// The pacing as last saved.
    saved_pacing: Pacing,
    /// The time of the last tick, for the status.
    now_ms: u64,
    /// The full VINs the product list gave (by Tesla's id of the vehicle), in memory only: the charging history is
    /// asked by VIN.
    vins: std::collections::HashMap<String, crate::FullVin>,
    /// How far the invoices are read, and as last saved.
    inv: crate::invoices::Progress,
    inv_saved: crate::invoices::Progress,
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
        let pacing = match store.pacing(id) {
            Ok(p) => p,
            Err(StoreError::NotFound) => return Err(StoreError::NotFound),
            Err(_) => Pacing::default(),
        };
        let inv = store.invoice_progress(id).unwrap_or_default();
        // the wait until midnight of the daily cap older servers kept: not a wait any more
        let old_cap = account.last_error.as_ref().is_some_and(|e| e.code == "daily_cap");
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
            backoff_until_ms: if old_cap { 0 } else { pacing.backoff_until_ms },
            backoff_step: pacing.backoff_step,
            day: pacing.day,
            polls_today: pacing.polls_today,
            last_request_ms: pacing.last_request_ms,
            rate_limited_until_ms: pacing.rate_limited_until_ms,
            force_refresh: false,
            unauthorized_once: false,
            gone: pacing.gone,
            last_ok_written_ms: 0,
            token_lifetime_ms: pacing.token_lifetime_ms,
            saved_pacing: pacing,
            now_ms: 0,
            vins: std::collections::HashMap::new(),
            inv: inv.clone(),
            inv_saved: inv,
        };
        p.rebuild_cars();
        Ok(p)
    }

    pub fn status(&self) -> AccountStatus {
        AccountStatus {
            account: self.id.clone(),
            state: self.account.state,
            polls_today: self.polls_today,
            rate_limited_until_ms: (self.rate_limited_until_ms > self.now_ms).then_some(self.rate_limited_until_ms),
            last_ok_ms: self.last_ok_ms,
            last_error: self.last_error.clone(),
            cars: self
                .cars
                .iter()
                .map(|c| CarStatus {
                    vehicle: c.vehicle.clone(),
                    car: c.car,
                    mode: self.mode_of(c),
                    car_state: c.state,
                    last_poll_ms: c.last_poll_ms,
                    next_poll_ms: match self.mode_of(c) {
                        Mode::Off => None,
                        Mode::Backoff => Some(self.backoff_until_ms),
                        _ => c.data_due().or(Some(self.list_next_ms)),
                    },
                    last_data_ms: c.last_data_ms,
                    streaming: c.stream.is_some(),
                    last_error: c.last_error.clone(),
                })
                .collect(),
        }
    }

    /// What is done for the car now: while the account waits after errors (or Tesla's 429), `backoff`.
    fn mode_of(&self, c: &CarPoll) -> Mode {
        if self.idle() {
            Mode::Off
        } else if c.car.is_some() && self.now_ms < self.backoff_until_ms {
            Mode::Backoff
        } else {
            c.mode
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
                    self.backoff_step = 0;
                    // a 429 wait ends with it: the status no longer says Tesla is limiting requests, and the next 429
                    // is announced again
                    self.rate_limited_until_ms = 0;
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
                // The generation is left behind on purpose: an admin's change (a new match, a new token) made while
                // a request was out is read again, and the cars rebuilt, by the next tick's sync.
                self.account = a;
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
        self.backoff_until_ms = now_ms.saturating_add(wait.min(MAX_WAIT_MS));
        self.backoff_until_ms
    }

    /// A request goes out now: counted for the day (shown, never a limit).
    fn count(&mut self, now_ms: u64) {
        self.last_request_ms = Some(now_ms);
        self.polls_today += 1;
    }

    /// Tesla answered 429: wait as it asks. A new episode (none running) is handed to the sink once.
    fn rate_limited(&mut self, now_ms: u64, retry_after_s: Option<u64>) -> u64 {
        let wait = self.rate_wait(retry_after_s);
        self.note_error(err("rate_limited", "Tesla is limiting requests", now_ms));
        let until = self.back_off(now_ms, Some(wait));
        let new = self.rate_limited_until_ms <= now_ms;
        self.rate_limited_until_ms = until;
        if new {
            tracing::warn!(account = %self.id, until_ms = until, "Tesla is limiting requests");
            self.sink.rate_limited(&self.account, until);
        }
        until
    }

    fn fact(&self, car: usize, at_ms: u64, state: ApiState) -> ApiFact {
        match self.cars[car].last_seen {
            Some((t, s)) if s == state && at_ms >= t && at_ms - t <= self.cadence.join_ms() => {
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

    fn pacing(&self) -> Pacing {
        Pacing {
            backoff_until_ms: self.backoff_until_ms,
            backoff_step: self.backoff_step,
            gone: self.gone,
            day: self.day,
            polls_today: self.polls_today,
            last_request_ms: self.last_request_ms,
            token_lifetime_ms: self.token_lifetime_ms,
            rate_limited_until_ms: self.rate_limited_until_ms,
        }
    }

    /// Saves the pacing when it changed, so a restart or a settings change keeps Tesla's waits and the caps.
    fn save_pacing(&mut self) {
        let p = self.pacing();
        if p == self.saved_pacing {
            return;
        }
        match self.store.set_pacing(&self.id, &p) {
            Ok(()) => self.saved_pacing = p,
            Err(StoreError::NotFound) => {}
            Err(e) => tracing::warn!(account = %self.id, error = %e, "the Tesla poller's pacing could not be saved"),
        }
    }

    /// When the access token counts as expiring: 30 min before its end, or a quarter of its life for a short one
    /// (else a token that lasts under 30 min would be refreshed again at once, over and over).
    fn token_margin_ms(&self) -> u64 {
        self.token_lifetime_ms.map_or(self.cadence.refresh_before_ms, |l| self.cadence.refresh_before_ms.min(l / 4))
    }

    /// Does what is due at `now_ms`; returns when to come back. `None`: the account is gone, stop.
    pub async fn tick(&mut self, now_ms: u64) -> Option<u64> {
        // the server's clock was set back (a wrong clock corrected, a machine resumed): every due time moves back as
        // far, else nothing would be read until the clock had caught up with the old times
        if self.now_ms > now_ms.saturating_add(60_000) {
            let back = self.now_ms - now_ms;
            let earlier = |t: &mut u64| *t = t.saturating_sub(back);
            earlier(&mut self.list_next_ms);
            earlier(&mut self.backoff_until_ms);
            earlier(&mut self.rate_limited_until_ms);
            for c in &mut self.cars {
                earlier(&mut c.next_data_ms);
                earlier(&mut c.quiet_until_ms);
                if let Some(t) = &mut c.idle_since_ms {
                    earlier(t);
                }
                c.last_seen = None;
            }
        }
        self.now_ms = now_ms;
        let next = self.tick_inner(now_ms).await;
        if next.is_some() {
            self.save_pacing();
            self.save_invoices();
        }
        next
    }

    fn save_invoices(&mut self) {
        if self.inv == self.inv_saved {
            return;
        }
        match self.store.set_invoice_progress(&self.id, &self.inv) {
            Ok(()) => self.inv_saved = self.inv.clone(),
            Err(StoreError::NotFound) => {}
            Err(e) => tracing::warn!(account = %self.id, error = %e, "the Tesla invoices' progress could not be saved"),
        }
    }

    /// How far the invoices are read.
    pub fn invoice_progress(&self) -> &crate::invoices::Progress {
        &self.inv
    }

    async fn tick_inner(&mut self, now_ms: u64) -> Option<u64> {
        if !self.sync() {
            return None;
        }
        if now_ms / 86_400_000 != self.day {
            self.day = now_ms / 86_400_000;
            self.polls_today = 0;
        }
        if now_ms / 86_400_000 != self.inv.day {
            self.inv.day = now_ms / 86_400_000;
            self.inv.requests_today = 0;
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
            || self.creds.access_expires_ms <= now_ms.saturating_add(self.token_margin_ms());
        if need_token {
            self.count(now_ms);
            return match self.refresh(now_ms).await {
                Ok(()) => Some(now_ms + NEXT_MS),
                Err(t) => soonest(t),
            };
        }
        if self.list_next_ms <= now_ms
            && self.cars.iter().any(|c| c.data_due().is_none() || c.mode == Mode::LettingSleep)
        {
            self.count(now_ms);
            return match self.list(now_ms).await {
                Ok(()) => Some(now_ms + NEXT_MS),
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
            self.count(now_ms);
            return match self.data(i, now_ms).await {
                Ok(()) => Some(now_ms + NEXT_MS),
                Err(t) => soonest(t),
            };
        }
        // the invoices last: only when no car is due
        let inv_due = self.invoice_due(now_ms);
        if inv_due.is_some_and(|t| t <= now_ms) {
            self.count(now_ms);
            self.inv.requests_today += 1;
            return match self.invoice_step(now_ms).await {
                Ok(()) => Some(now_ms + NEXT_MS),
                Err(t) => soonest(t),
            };
        }
        let needs_list = self.cars.iter().any(|c| c.data_due().is_none() || c.mode == Mode::LettingSleep);
        let mut next = due.map_or(u64::MAX, |(t, _)| t);
        if needs_list {
            next = next.min(self.list_next_ms);
        }
        if let Some(t) = inv_due {
            next = next.min(t);
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
                    access_expires_ms: now_ms.saturating_add(t.expires_in_s.saturating_mul(1000)),
                };
                // Stored before it is used: the old refresh token is spent now.
                let saved = self.store.update(&self.id, None, |_, c| *c = new.clone());
                match saved {
                    Ok((a, ())) => {
                        // Generation left behind: the next sync reads again what an admin changed meanwhile.
                        self.account = a;
                        self.creds = new;
                        self.token_lifetime_ms = Some(t.expires_in_s.saturating_mul(1000));
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
            Err(AuthError::RateLimited { retry_after_s }) => Err(self.rate_limited(now_ms, retry_after_s)),
            Err(e) => {
                self.note_error(err(e.code(), e.to_string(), now_ms));
                Err(self.back_off(now_ms, None))
            }
        }
    }

    fn rate_wait(&self, retry_after_s: Option<u64>) -> u64 {
        retry_after_s
            .map_or(self.cadence.rate_limit_default_ms, |s| s.saturating_mul(1000))
            .clamp(self.cadence.rate_limit_min_ms, MAX_WAIT_MS.max(self.cadence.rate_limit_min_ms))
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
                now_ms + NEXT_MS
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
            ApiError::RateLimited { retry_after_s } => self.rate_limited(now_ms, *retry_after_s),
            other => {
                self.note_error(err(other.code(), other.to_string(), now_ms));
                self.back_off(now_ms, None)
            }
        }
    }

    async fn list(&mut self, now_ms: u64) -> Result<(), u64> {
        let Some(access) = self.creds.access.clone() else { return Err(now_ms + NEXT_MS) };
        let answer = self.tesla.products(&access).await;
        self.drain_streams();
        let list = match answer {
            Ok(l) => l,
            Err(e) => return Err(self.api_error(&e, now_ms)),
        };
        self.unauthorized_once = false;
        self.backoff_step = 0;
        self.list_next_ms = now_ms + self.cadence.list_ms;
        self.set_state(AccountState::Ok, None, now_ms);
        for v in &list {
            if let Some(vin) = &v.vin {
                self.vins.insert(v.id.clone(), vin.clone());
            }
        }
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
        let Some(access) = self.creds.access.clone() else { return Err(now_ms + NEXT_MS) };
        let c = &self.cars[i];
        let location = match self.account.location {
            Location::Always => true,
            Location::Never => false,
            Location::DrivingOrCharging => c.just_woke || matches!(c.mode, Mode::Driving | Mode::Charging),
        };
        let id = c.tesla_id.clone();
        self.cars[i].last_poll_ms = Some(now_ms);
        let answer = self.tesla.vehicle_data(&access, &id, location).await;
        // What the stream sent while the request was out goes in before the stream can be closed.
        self.drain_streams();
        let v = match answer {
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
        let cad = self.cadence.clone();
        let c = &mut self.cars[i];
        if r.activity.charging && r.meta.fast_charger_type.is_some() {
            c.dc = true;
        } else if !r.activity.charging && c.dc {
            // a DC charge ended: Tesla bills it shortly
            c.dc = false;
            if cad.invoices {
                self.inv.soon(now_ms, cad.invoice_after_dc_ms);
            }
        }
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
            && c.stream.is_none()
            && c.stream_refusals < 2
            && let (Some(tag), Some(access)) = (c.tag.clone(), self.creds.access.as_ref())
        {
            c.stream = self.streamer.open(&tag, access);
        }
        let gap = match c.mode {
            Mode::Driving => cad.driving_ms,
            Mode::Charging => cad.charging_ms,
            _ => cad.idle_ms,
        };
        c.next_data_ms = now_ms + gap;
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

/// A PDF to download: the session's key, Tesla's id of the invoice, the vehicle.
type PdfWanted = (String, String, Option<String>);

/// The invoice reads ([`crate::invoices`]): one request per tick, after the cars, within the day's caps.
impl<T: Tesla, S: Sink, St: Streamer> Poller<T, S, St> {
    /// The vehicles whose history can be read now: (Tesla's id, this server's id, the car it is matched to).
    fn invoice_vehicles(&self) -> Vec<(String, String, Option<CarId>)> {
        self.account
            .cars
            .iter()
            .filter(|c| c.matched != Some(Match::LeaveOut) && self.vins.contains_key(&c.tesla_id))
            .map(|c| {
                let car = match c.matched {
                    Some(Match::Car { car }) => Some(car),
                    _ => None,
                };
                (c.tesla_id.clone(), c.vehicle.clone(), car)
            })
            .collect()
    }

    fn invoice_pdf_next(&self, now_ms: u64) -> Option<(u64, PdfWanted)> {
        self.sink
            .invoice_pdfs_wanted(&self.id)
            .into_iter()
            .map(|w| {
                let at = self.inv.pdf_failed.get(&w.0).map_or(now_ms, |(t, _)| *t);
                (at, w)
            })
            .filter(|(at, _)| *at != u64::MAX)
            .min_by_key(|(at, _)| *at)
    }

    /// When the next invoice request is due (none: invoices off, nothing to read, or no VIN known yet).
    fn invoice_due(&self, now_ms: u64) -> Option<u64> {
        let cad = &self.cadence;
        if !cad.invoices || self.account.state != AccountState::Ok {
            return None;
        }
        self.creds.access.as_ref()?;
        self.http_ownership()?;
        let vehicles = self.invoice_vehicles();
        if vehicles.is_empty() {
            return None;
        }
        let ids: Vec<String> = vehicles.iter().map(|v| v.0.clone()).collect();
        let want = if self.inv.backfilling(&ids) || !self.inv.checking.is_empty() {
            now_ms
        } else {
            let pdf = self.invoice_pdf_next(now_ms).map_or(u64::MAX, |(t, _)| t.max(now_ms));
            self.inv.next_check_ms.min(pdf)
        };
        Some(want.max(self.inv.wait_until_ms))
    }

    fn http_ownership(&self) -> Option<()> {
        // a China account has no host for the charging history: never asked
        (self.account.region == crate::Region::Global).then_some(())
    }

    /// The sessions of a page as the sink takes them.
    fn found(&self, page: &crate::invoices::Page, asked: &str) -> Vec<crate::invoices::Found> {
        page.sessions
            .iter()
            .filter_map(|s| {
                let key = crate::invoices::key_of(&s.id)?;
                let tid = s
                    .vin
                    .as_ref()
                    .and_then(|v| self.vins.iter().find(|(_, x)| *x == v).map(|(t, _)| t.clone()))
                    .unwrap_or_else(|| asked.to_string());
                let c = self.account.cars.iter().find(|c| c.tesla_id == tid);
                let car = c.and_then(|c| match c.matched {
                    Some(Match::Car { car }) => Some(car),
                    _ => None,
                });
                Some(crate::invoices::Found {
                    key,
                    account: self.id.clone(),
                    vehicle: c.map(|c| c.vehicle.clone()),
                    car,
                    session: s.clone(),
                })
            })
            .collect()
    }

    /// A history or PDF request failed. Never changes the account's state: the cars go on being read.
    fn invoice_error(&mut self, e: &ApiError, now_ms: u64) -> Result<(), u64> {
        match e {
            ApiError::Unauthorized if !self.unauthorized_once => {
                self.unauthorized_once = true;
                self.force_refresh = true;
                Ok(())
            }
            ApiError::RateLimited { retry_after_s } => {
                let until = self.rate_limited(now_ms, *retry_after_s);
                let wait = until.saturating_sub(now_ms);
                self.inv.note_error(err("rate_limited", "Tesla is limiting requests", now_ms), wait, wait);
                Err(until)
            }
            other => {
                let message = match other {
                    ApiError::Forbidden | ApiError::Unauthorized => {
                        "Tesla refuses this account's charging history".to_string()
                    }
                    ApiError::EndpointGone | ApiError::NotFound => {
                        "Tesla's charging history is not there for this account".to_string()
                    }
                    o => o.to_string(),
                };
                tracing::warn!(account = %self.id, code = other.code(), "tesla invoice request failed");
                let (first, max) = (self.cadence.invoice_error_first_ms, self.cadence.invoice_error_max_ms);
                self.inv.note_error(err(other.code(), message, now_ms), first, max);
                Ok(())
            }
        }
    }

    /// Reads one page, keeps its sessions: `(records on the page, keys, new sessions)`.
    async fn invoice_page(
        &mut self,
        access: &crate::Secret,
        tid: &str,
        page: u32,
        now_ms: u64,
    ) -> Result<Option<(usize, Vec<String>, u64)>, u64> {
        let Some(vin) = self.vins.get(tid).cloned() else { return Ok(None) };
        let answer = self.tesla.charging_history(access, &vin, page).await;
        self.drain_streams();
        let p = match answer {
            Ok(p) => p,
            Err(e) => {
                // pages may count from 1: a refused page 0 says so
                let refused = matches!(e, ApiError::NotFound | ApiError::Tesla { status: 400..=499 });
                if page == 0 && self.inv.first_page.is_none() && refused {
                    self.inv.first_page = Some(1);
                    for b in self.inv.backfill.values_mut() {
                        b.next_page = b.next_page.max(1);
                    }
                    for p in self.inv.checking.values_mut() {
                        *p = (*p).max(1);
                    }
                    return Ok(None);
                }
                self.invoice_error(&e, now_ms)?;
                return Ok(None);
            }
        };
        self.unauthorized_once = false;
        if self.inv.first_page.is_none() {
            self.inv.first_page = Some(page);
        }
        let found = self.found(&p, tid);
        let keys: Vec<String> = found.iter().map(|f| f.key.clone()).collect();
        let fresh = found.iter().filter(|f| !self.sink.invoice_known(&f.key)).count() as u64;
        match self.sink.invoices(&found) {
            Ok(_) => self.inv.note_ok(),
            Err(e) => {
                tracing::warn!(account = %self.id, error = %e, "Tesla invoices could not be stored");
                let (first, max) = (self.cadence.invoice_error_first_ms, self.cadence.invoice_error_max_ms);
                self.inv.note_error(err("store_failed", "the sessions could not be stored", now_ms), first, max);
                return Ok(None);
            }
        }
        Ok(Some((p.records, keys, fresh)))
    }

    async fn invoice_step(&mut self, now_ms: u64) -> Result<(), u64> {
        let Some(access) = self.creds.access.clone() else { return Ok(()) };
        let vehicles = self.invoice_vehicles();
        let first = self.inv.first_page.unwrap_or(0);
        let page_size = crate::invoices::PAGE_SIZE as usize;
        // 1. the backfill: every page of each vehicle's history, once
        if let Some((tid, _, _)) = vehicles.iter().find(|v| !self.inv.backfill.get(&v.0).is_some_and(|b| b.done)) {
            let tid = tid.clone();
            let page = self
                .inv
                .backfill
                .entry(tid.clone())
                .or_insert_with(|| crate::invoices::Backfill { next_page: first, ..Default::default() })
                .next_page;
            let Some((records, keys, fresh)) = self.invoice_page(&access, &tid, page, now_ms).await? else {
                return Ok(());
            };
            let first = self.inv.first_page.unwrap_or(first);
            let b = self.inv.backfill.entry(tid).or_default();
            let again = !keys.is_empty() && keys == b.last_page;
            b.found += fresh;
            b.done = records < page_size || again || page.saturating_sub(first) + 1 >= crate::invoices::MAX_PAGES;
            b.last_page = keys;
            b.next_page = page + 1;
            if self.inv.backfill.values().all(|b| b.done) && self.inv.last_check_ms.is_none() {
                self.inv.last_check_ms = Some(now_ms);
                self.inv.next_check_ms = now_ms + self.cadence.invoice_check_ms;
            }
            return Ok(());
        }
        // 2. the daily check (or the look after a DC charge): the first pages, until one brings nothing new
        if !self.inv.checking.is_empty() || now_ms >= self.inv.next_check_ms {
            if self.inv.checking.is_empty() {
                self.inv.checking = vehicles.iter().map(|v| (v.0.clone(), first)).collect();
            }
            let Some((tid, page)) = self.inv.checking.iter().next().map(|(t, p)| (t.clone(), *p)) else {
                return Ok(());
            };
            if !self.vins.contains_key(&tid) {
                self.inv.checking.remove(&tid);
                return Ok(());
            }
            let Some((records, _, fresh)) = self.invoice_page(&access, &tid, page, now_ms).await? else {
                return Ok(());
            };
            let first = self.inv.first_page.unwrap_or(first);
            if fresh == 0 || records < page_size || page.saturating_sub(first) + 1 >= crate::invoices::CHECK_PAGES {
                self.inv.checking.remove(&tid);
            } else {
                self.inv.checking.insert(tid, page + 1);
            }
            if self.inv.checking.is_empty() {
                self.inv.last_check_ms = Some(now_ms);
                self.inv.next_check_ms = now_ms + self.cadence.invoice_check_ms;
            }
            return Ok(());
        }
        // 3. the PDFs not kept yet, oldest first
        let Some((_, (key, id, vehicle))) = self.invoice_pdf_next(now_ms) else { return Ok(()) };
        let vin = vehicle
            .and_then(|v| self.account.cars.iter().find(|c| c.vehicle == v))
            .and_then(|c| self.vins.get(&c.tesla_id).cloned());
        let answer = self.tesla.invoice_pdf(&access, &id, vin.as_ref()).await;
        self.drain_streams();
        match answer {
            Ok(pdf) => match self.sink.invoice_pdf(&key, &pdf) {
                Ok(true) => {
                    self.inv.pdf_failed.remove(&key);
                    self.inv.note_ok();
                }
                Ok(false) | Err(_) => self.pdf_failed(&key, now_ms),
            },
            Err(e @ (ApiError::Unauthorized | ApiError::RateLimited { .. })) => self.invoice_error(&e, now_ms)?,
            Err(ApiError::Http(e)) => self.invoice_error(&ApiError::Http(e), now_ms)?,
            Err(_) => self.pdf_failed(&key, now_ms),
        }
        Ok(())
    }

    /// An invoice Tesla would not give (or that is not a PDF): tried again a day later, at most a few times.
    fn pdf_failed(&mut self, key: &str, now_ms: u64) {
        let tries = self.inv.pdf_failed.get(key).map_or(0, |(_, n)| *n) + 1;
        let at = if tries >= crate::invoices::PDF_TRIES { u64::MAX } else { now_ms + crate::invoices::PDF_RETRY_MS };
        self.inv.pdf_failed.insert(key.to_string(), (at, tries));
    }
}
