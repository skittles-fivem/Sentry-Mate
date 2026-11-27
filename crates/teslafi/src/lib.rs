//! TeslaFi's API as an input, for owners who pay for TeslaFi: TeslaFi's own polls of the car (about one a minute
//! while it is awake), read by this server and handed to a [`Sink`] (the engine) tagged as coming from TeslaFi.
//!
//! Off until an admin adds a TeslaFi token: with no account file, [`Service::start`] starts no task and builds no
//! client, so the server never looks up or connects to TeslaFi. It talks only to TeslaFi's API host ([`hosts`]),
//! over TLS 1.3 (1.2 only if that host stops offering 1.3, and the status says so) with rustls, sends only the two
//! reads of [`api::Read`] (never a wake, never a command: there is no way to build one), keeps the token sealed in
//! the vault ([`store`]) and never logs or returns it ([`Secret`]).

pub mod api;
pub mod costs;
pub mod health;
pub mod hosts;
pub mod http;
pub mod poller;
pub mod store;

pub use api::{Client, Feed, FeedError, Read};
pub use health::{DataHealth, data_health};
pub use hosts::{Endpoint, HOST};
pub use http::{Http, HttpError};
pub use poller::{AccountStatus, Cadence, Mode, Pace, Poller, Sink};
pub use store::{Account, AccountCar, AccountState, Match, Store, StoreError};
pub use tesla::{Proxy, Secret};

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tesla::Clock;
use tokio::sync::Notify;
use vault::Vault;

/// The longest token taken (TeslaFi's are 32 hexadecimal characters).
pub const MAX_TOKEN: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("no such TeslaFi account")]
    NoSuchAccount,
    #[error("no such car on this TeslaFi account")]
    NoSuchVehicle,
    #[error("not a TeslaFi token")]
    BadToken,
    #[error("not a time zone")]
    BadTimeZone,
    #[error("the account was changed meanwhile; read it again")]
    Stale,
    #[error("this TeslaFi account's car is already on another TeslaFi account here")]
    AccountExists,
    #[error("that car is already matched to another TeslaFi car")]
    CarTaken,
    #[error("{0}")]
    Feed(FeedError),
    #[error("{0}")]
    Http(HttpError),
    #[error("{0}")]
    Store(StoreError),
}

impl ServiceError {
    /// The API error code: `teslafi_refused` (422), `teslafi_unreachable` (502), `account_exists` / `car_taken`
    /// (409) ...
    pub fn code(&self) -> &'static str {
        match self {
            ServiceError::NoSuchAccount | ServiceError::Store(StoreError::NotFound) => "no_such_teslafi_account",
            ServiceError::NoSuchVehicle => "no_such_vehicle",
            ServiceError::BadToken => "invalid_token",
            ServiceError::BadTimeZone => "invalid_time_zone",
            ServiceError::Stale | ServiceError::Store(StoreError::Stale) => "stale",
            ServiceError::AccountExists => "account_exists",
            ServiceError::CarTaken => "car_taken",
            ServiceError::Feed(FeedError::Refused) => "teslafi_refused",
            ServiceError::Feed(_) | ServiceError::Http(_) => "teslafi_unreachable",
            ServiceError::Store(_) => "storage",
        }
    }
}

impl From<StoreError> for ServiceError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => ServiceError::NoSuchAccount,
            StoreError::Stale => ServiceError::Stale,
            e => ServiceError::Store(e),
        }
    }
}

type EndpointFor = Arc<dyn Fn() -> Endpoint + Send + Sync>;

/// How the service connects and how often it reads.
#[derive(Clone)]
pub struct Options {
    /// The owner's HTTP proxy (`/settings` `outbound.proxy`), else direct.
    pub proxy: Option<Proxy>,
    pub cadence: Cadence,
    /// The time zone of TeslaFi's times when an account names none (the server's default zone).
    pub default_time_zone: String,
    endpoint: EndpointFor,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            proxy: None,
            cadence: Cadence::default(),
            default_time_zone: "UTC".into(),
            endpoint: Arc::new(Endpoint::teslafi),
        }
    }
}

impl Options {
    /// Every account talks to `e` (a fake TeslaFi). Not in a normal build.
    #[cfg(feature = "test-endpoints")]
    pub fn with_endpoint(mut self, e: Endpoint) -> Options {
        self.endpoint = Arc::new(move || e.clone());
        self
    }

    fn tz(&self) -> chrono_tz::Tz {
        self.default_time_zone.parse().unwrap_or(chrono_tz::Tz::UTC)
    }
}

/// Changes to an account other than its token and its cars.
#[derive(Debug, Clone, Default)]
pub struct Change {
    pub label: Option<String>,
    pub paused: Option<bool>,
    /// `Some(None)`: the server's default zone.
    pub time_zone: Option<Option<String>>,
}

/// The requests a token's check sent: written into the account's pacing, so the poller keeps its gap after them
/// and counts them for the day.
#[derive(Default)]
struct Sent {
    n: u64,
    at_ms: u64,
}

struct Task {
    wake: Arc<Notify>,
    status: Arc<Mutex<Option<AccountStatus>>>,
    join: tokio::task::JoinHandle<()>,
}

struct Inner<S: Sink> {
    store: Store,
    sink: Arc<S>,
    opts: std::sync::RwLock<Options>,
    tasks: Mutex<HashMap<String, Task>>,
    clients: AtomicUsize,
}

/// The TeslaFi input of one server.
pub struct Service<S: Sink> {
    inner: Arc<Inner<S>>,
}

impl<S: Sink> Clone for Service<S> {
    fn clone(&self) -> Self {
        Service { inner: self.inner.clone() }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn clean_label(label: &str) -> String {
    let l: String = label.trim().chars().filter(|c| !c.is_control()).take(64).collect();
    if l.is_empty() { "TeslaFi account".to_string() } else { l }
}

fn apply(a: &mut Account, change: &Change) {
    if let Some(l) = &change.label {
        a.label = clean_label(l);
    }
    if let Some(p) = change.paused {
        a.paused = p;
    }
    if let Some(z) = change.time_zone.clone() {
        a.time_zone = z;
    }
}

/// A token as TeslaFi gives it: printable, no spaces, not too long.
pub fn token_ok(t: &str) -> bool {
    !t.is_empty() && t.len() <= MAX_TOKEN && t.bytes().all(|b| b.is_ascii_graphic())
}

/// An IANA time zone the server knows.
pub fn time_zone_ok(z: &str) -> bool {
    z.parse::<chrono_tz::Tz>().is_ok()
}

impl<S: Sink> Service<S> {
    /// Builds nothing that talks to the network.
    pub fn new(data: &Path, vault: Arc<Vault>, sink: Arc<S>, opts: Options) -> Service<S> {
        Service {
            inner: Arc::new(Inner {
                store: Store::new(data, vault),
                sink,
                opts: std::sync::RwLock::new(opts),
                tasks: Mutex::new(HashMap::new()),
                clients: AtomicUsize::new(0),
            }),
        }
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    fn opts(&self) -> Options {
        self.inner.opts.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn cadence(&self) -> Cadence {
        self.opts().cadence
    }

    /// The zone TeslaFi's times are read in for an account with none of its own.
    pub fn default_time_zone(&self) -> String {
        self.opts().default_time_zone
    }

    /// A new proxy (a settings change): every running poller starts again with it, on a new client.
    pub fn reconfigure(&self, proxy: Option<Proxy>) {
        {
            let mut o = self.inner.opts.write().unwrap_or_else(|p| p.into_inner());
            if o.proxy == proxy {
                return;
            }
            o.proxy = proxy;
        }
        self.restart();
    }

    /// The server's default time zone changed: the accounts with no zone of their own read TeslaFi's times in the new
    /// one (their pollers start again with it).
    pub fn set_default_time_zone(&self, zone: &str) {
        {
            let mut o = self.inner.opts.write().unwrap_or_else(|p| p.into_inner());
            if o.default_time_zone == zone {
                return;
            }
            o.default_time_zone = zone.to_string();
        }
        self.restart();
    }

    /// A new pace (an admin chose how often TeslaFi is read): every running poller starts again with it. Their
    /// saved pacing (the day's count, TeslaFi's wait, the back-off) is kept, so this never asks early.
    pub fn set_cadence(&self, cadence: Cadence) {
        {
            let mut o = self.inner.opts.write().unwrap_or_else(|p| p.into_inner());
            if o.cadence == cadence {
                return;
            }
            o.cadence = cadence;
        }
        self.restart();
    }

    /// Reads the account's charge costs from the start again (their times were read in another zone): its poller is
    /// stopped, the backfill starts over (the count read so far kept), and the poller starts again when it ran.
    /// TeslaFi's costs are kept one per charge, so those read again take the place of the old ones.
    pub fn restart_costs(&self, id: &str) -> Result<(), ServiceError> {
        let was = lock(&self.inner.tasks).remove(id).map(|t| t.join.abort()).is_some();
        let store = &self.inner.store;
        let mut p = store.pacing(id)?;
        p.costs = costs::Progress { found: p.costs.found, ..costs::Progress::default() };
        store.set_pacing_exact(id, &p)?;
        if was {
            self.spawn(id, None)?;
        }
        Ok(())
    }

    fn restart(&self) {
        let running: Vec<String> = lock(&self.inner.tasks).keys().cloned().collect();
        for id in running {
            if let Err(e) = self.spawn(&id, None) {
                tracing::warn!(account = %id, error = %e, "a TeslaFi account could not be started again");
            }
        }
    }

    pub fn running(&self, id: &str) -> bool {
        lock(&self.inner.tasks).contains_key(id)
    }

    /// Starts a poller for every stored account (call inside the server's runtime). With no account: nothing.
    pub fn start(&self) {
        for id in self.inner.store.ids() {
            if let Err(e) = self.spawn(&id, None) {
                tracing::warn!(account = %id, error = %e, "a TeslaFi account could not be started");
            }
        }
    }

    /// Stops every poller (shutdown, or before a move switches servers).
    pub fn stop(&self) {
        for (_, t) in lock(&self.inner.tasks).drain() {
            t.join.abort();
        }
    }

    /// Clients built so far: 0 means this server has not made a single connection to TeslaFi.
    pub fn clients_built(&self) -> usize {
        self.inner.clients.load(Ordering::Relaxed)
    }

    fn client(&self) -> Result<Arc<Http>, ServiceError> {
        let opts = self.opts();
        let http = Http::new((opts.endpoint)(), opts.proxy).map_err(ServiceError::Http)?;
        self.inner.clients.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(http))
    }

    fn spawn(&self, id: &str, http: Option<Arc<Http>>) -> Result<(), ServiceError> {
        let http = match http {
            Some(h) => h,
            None => self.client()?,
        };
        let opts = self.opts();
        let (h2, tz) = (http.clone(), opts.tz());
        let poller =
            Poller::new(self.inner.store.clone(), id, Client::new(http), self.inner.sink.clone(), opts.cadence, tz)?
                .with_tls(Arc::new(move || h2.tls_version()));
        let wake = Arc::new(Notify::new());
        let status = Arc::new(Mutex::new(Some(poller.status())));
        let join = tokio::spawn(poller.run(tesla::SystemClock, wake.clone(), status.clone()));
        if let Some(old) = lock(&self.inner.tasks).insert(id.to_string(), Task { wake, status, join }) {
            old.join.abort();
        }
        Ok(())
    }

    fn wake(&self, id: &str) {
        if let Some(t) = lock(&self.inner.tasks).get(id) {
            t.wake.notify_one();
        }
    }

    /// The host the accounts connect to (empty when there is no account).
    pub fn hosts(&self) -> Vec<String> {
        if self.inner.store.ids().is_empty() { Vec::new() } else { vec![(self.opts().endpoint)().host().to_string()] }
    }

    pub fn accounts(&self) -> Vec<Account> {
        self.inner.store.list()
    }

    pub fn account(&self, id: &str) -> Result<Account, ServiceError> {
        Ok(self.inner.store.get(id)?)
    }

    /// What each running poller is doing.
    pub fn status(&self) -> Vec<AccountStatus> {
        let tasks = lock(&self.inner.tasks);
        let mut out: Vec<AccountStatus> = tasks.values().filter_map(|t| lock(&t.status).clone()).collect();
        out.sort_by(|a, b| a.account.cmp(&b.account));
        out
    }

    /// One look with a token (the latest poll, and, when it names no car, the last poll of the car awake, asked the
    /// cadence's gap later so even a check never sends two requests closer than that): the VIN it names, if any.
    /// Refused: the token is not taken. `sent` counts the requests and keeps the time of the last one.
    async fn check(
        &self,
        http: &Arc<Http>,
        token: &Secret,
        sent: &mut Sent,
    ) -> Result<Option<(String, Option<String>)>, ServiceError> {
        let c = Client::new(http.clone());
        let opts = self.opts();
        let tz = opts.tz();
        for (i, what) in [Read::Latest, Read::LastGood].into_iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(opts.cadence.min_gap_ms)).await;
            }
            sent.n += 1;
            sent.at_ms = tesla::SystemClock.now_ms();
            let poll = c.read(token, what).await.map_err(ServiceError::Feed)?;
            let fields = import::teslafi::feed::fields_of(&poll);
            if let Ok(p) = import::teslafi::feed::read(&fields, tz, None)
                && let Some(vin) = p.vin
            {
                return Ok(Some((vin, p.display_name)));
            }
        }
        Ok(None)
    }

    /// Adds an account from a TeslaFi token (TeslaFi, Settings, TeslaFi API): checked with one or two reads before
    /// anything is kept. Its car (when TeslaFi named one) waits for an admin to say which car it is.
    pub async fn add(
        &self,
        label: &str,
        token: Secret,
        time_zone: Option<String>,
        now_ms: u64,
    ) -> Result<Account, ServiceError> {
        if !token_ok(token.expose()) {
            return Err(ServiceError::BadToken);
        }
        if time_zone.as_deref().is_some_and(|z| !time_zone_ok(z)) {
            return Err(ServiceError::BadTimeZone);
        }
        let http = self.client()?;
        let mut sent = Sent::default();
        let seen = self.check(&http, &token, &mut sent).await?;
        let store = &self.inner.store;
        let mut cars = Vec::new();
        if let Some((vin, name)) = seen {
            let car = store::car_of(store, &vin, name, "fv1".into());
            let taken = store.list().into_iter().flat_map(|a| a.cars).any(|c| c.vin_tag == car.vin_tag);
            if taken {
                return Err(ServiceError::AccountExists);
            }
            cars.push(car);
        }
        let mut pacing = store::Pacing::default();
        pacing.note_requests(sent.n, sent.at_ms);
        let account = store.add_paced(
            Account {
                id: String::new(),
                label: clean_label(label),
                added_ms: now_ms,
                version: 0,
                paused: false,
                time_zone,
                state: AccountState::Ok,
                last_ok_ms: Some(now_ms),
                last_error: None,
                cars,
            },
            &token,
            pacing,
        )?;
        tracing::info!(account = %account.id, cars = account.cars.len(), "teslafi account added");
        self.spawn(&account.id, Some(http))?;
        Ok(account)
    }

    /// A new token for an account (after TeslaFi refused the old one): checked with one or two reads before it is
    /// stored.
    pub async fn replace_token(&self, id: &str, version: u64, token: Secret) -> Result<Account, ServiceError> {
        self.change(id, version, Change::default(), Some(token)).await
    }

    /// Changes an account, and with `token` gives it a new one. All or nothing: the token is checked with TeslaFi
    /// first, and when TeslaFi refuses it or cannot be reached nothing at all is changed.
    pub async fn change(
        &self,
        id: &str,
        version: u64,
        change: Change,
        token: Option<Secret>,
    ) -> Result<Account, ServiceError> {
        if let Some(Some(z)) = &change.time_zone
            && !time_zone_ok(z)
        {
            return Err(ServiceError::BadTimeZone);
        }
        let Some(token) = token else { return self.update(id, version, change) };
        if !token_ok(token.expose()) {
            return Err(ServiceError::BadToken);
        }
        let account = self.inner.store.get(id)?;
        if account.version != version {
            return Err(ServiceError::Stale);
        }
        let http = self.client()?;
        let mut sent = Sent::default();
        let seen = self.check(&http, &token, &mut sent).await?;
        let tag = seen.map(|(vin, _)| self.inner.store.vin_tag(&tesla::FullVin::new(vin)));
        // a token of another TeslaFi account (another car) is not this account's
        if let Some(t) = &tag
            && !account.cars.is_empty()
            && !account.cars.iter().any(|c| c.vin_tag.as_ref() == Some(t))
        {
            return Err(ServiceError::AccountExists);
        }
        let (a, ()) = self.inner.store.update_paced(
            id,
            Some(version),
            Some(&token),
            |a| {
                apply(a, &change);
                a.state = AccountState::Ok;
                a.last_error = None;
            },
            |p| p.note_requests(sent.n, sent.at_ms),
        )?;
        if lock(&self.inner.tasks).contains_key(id) {
            self.wake(id);
        } else {
            self.spawn(id, Some(http))?;
        }
        Ok(a)
    }

    pub fn update(&self, id: &str, version: u64, change: Change) -> Result<Account, ServiceError> {
        if let Some(Some(z)) = &change.time_zone
            && !time_zone_ok(z)
        {
            return Err(ServiceError::BadTimeZone);
        }
        let (a, ()) = self.inner.store.update(id, Some(version), None, |a| apply(a, &change))?;
        self.wake(id);
        Ok(a)
    }

    /// Says which car each TeslaFi car is. A car may be matched to one TeslaFi car only.
    pub fn set_matches(
        &self,
        id: &str,
        version: u64,
        matches: &[(String, Option<Match>)],
    ) -> Result<Account, ServiceError> {
        let store = &self.inner.store;
        let account = store.get(id)?;
        if account.version != version {
            return Err(ServiceError::Stale);
        }
        let car_of = |m: Option<Match>| match m {
            Some(Match::Car { car }) => Some(car),
            _ => None,
        };
        let changing: Vec<&str> = matches.iter().map(|(v, _)| v.as_str()).collect();
        let mut taken: Vec<model::CarId> = store
            .list()
            .into_iter()
            .filter(|a| a.id != id)
            .flat_map(|a| a.cars.into_iter())
            .filter_map(|c| car_of(c.matched))
            .collect();
        taken.extend(
            account.cars.iter().filter(|c| !changing.contains(&c.vehicle.as_str())).filter_map(|c| car_of(c.matched)),
        );
        for (v, m) in matches {
            if !account.cars.iter().any(|c| &c.vehicle == v) {
                return Err(ServiceError::NoSuchVehicle);
            }
            if let Some(car) = car_of(*m) {
                if taken.contains(&car) {
                    return Err(ServiceError::CarTaken);
                }
                taken.push(car);
            }
        }
        let (a, ()) = store.update(id, Some(version), None, |a| {
            for (v, m) in matches {
                if let Some(c) = a.cars.iter_mut().find(|c| &c.vehicle == v) {
                    c.matched = *m;
                }
            }
        })?;
        self.wake(id);
        Ok(a)
    }

    /// Forgets the account at once (its sealed file and token). History already stored stays.
    pub fn remove(&self, id: &str) -> Result<(), ServiceError> {
        self.inner.store.remove(id)?;
        if let Some(t) = lock(&self.inner.tasks).remove(id) {
            t.join.abort();
        }
        tracing::info!(account = %id, "teslafi account removed");
        Ok(())
    }
}
