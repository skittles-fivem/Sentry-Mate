//! The Tesla Owner API as an input: a Tesla account signed in by its owner, its cars read without ever waking one,
//! and the readings handed to a [`Sink`] (the engine) tagged as coming from the Tesla API.
//!
//! Off until an admin adds an account: with no account file, [`Service::start`] starts no task and builds no
//! client, so the server never looks up or connects to a Tesla host. It talks only to Tesla's own hosts
//! ([`hosts`]), over TLS 1.3 with rustls, sends only the token requests, the two reads and the streaming
//! subscription, keeps the tokens sealed in the vault ([`store`]) and never logs or returns them ([`Secret`]).
//! It never wakes a car and never sends one a command.

pub mod api;
pub mod auth;
pub mod convert;
pub mod hosts;
pub mod http;
pub mod invoices;
pub mod poller;
pub mod secret;
pub mod store;
pub mod stream;

pub use api::{ApiError, CarState, Client, Tesla, Vehicle};
pub use auth::{AuthError, Redirect, SignIn, Tokens};
pub use convert::{Activity, ApiMeta, Reading};
pub use hosts::{Endpoints, Region};
pub use http::{Http, HttpError, Proxy};
pub use poller::{
    AccountStatus, ApiFact, ApiState, Cadence, CarStatus, Clock, Mode, NewCar, Pace, Poller, Sink, SinkError,
    SystemClock,
};
pub use secret::{FullVin, Secret};
pub use store::{Account, AccountCar, AccountState, Credentials, ErrorInfo, Location, Match, Store, StoreError};
pub use stream::{StreamHandle, StreamTiming, Streamer, WsStreamer};

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use vault::Vault;

/// Open sign-ins at most (each waits [`auth::SIGN_IN_TTL_MS`]).
pub const MAX_SIGN_INS: usize = 20;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("no such Tesla account")]
    NoSuchAccount,
    #[error("no such sign-in (it expired or was used)")]
    NoSuchSignIn,
    #[error("no such vehicle on this account")]
    NoSuchVehicle,
    #[error("too many sign-ins are open; finish or wait for one")]
    TooManySignIns,
    #[error("the account was changed meanwhile; read it again")]
    Stale,
    #[error("these cars are already on another Tesla account here")]
    AccountExists,
    #[error("that car is already matched to another Tesla vehicle")]
    CarTaken,
    #[error("{0}")]
    Auth(AuthError),
    #[error("{0}")]
    Api(ApiError),
    #[error("{0}")]
    Http(HttpError),
    #[error("{0}")]
    Store(StoreError),
}

impl ServiceError {
    /// The API error code: `tesla_refused` (422), `tesla_unreachable` (502), `account_exists` / `car_taken` (409)
    /// ...
    pub fn code(&self) -> &'static str {
        match self {
            ServiceError::NoSuchAccount => "no_such_tesla_account",
            ServiceError::NoSuchSignIn => "no_such_sign_in",
            ServiceError::NoSuchVehicle => "no_such_vehicle",
            ServiceError::TooManySignIns => "too_many_sign_ins",
            ServiceError::Stale => "stale",
            ServiceError::AccountExists => "account_exists",
            ServiceError::CarTaken => "car_taken",
            ServiceError::Auth(AuthError::BadPaste(_) | AuthError::WrongState) => "invalid",
            ServiceError::Auth(AuthError::Expired) => "no_such_sign_in",
            ServiceError::Auth(AuthError::Refused) | ServiceError::Api(ApiError::Unauthorized) => "tesla_refused",
            ServiceError::Api(ApiError::Forbidden) => "tesla_refused",
            ServiceError::Auth(_) | ServiceError::Api(_) | ServiceError::Http(_) => "tesla_unreachable",
            ServiceError::Store(StoreError::NotFound) => "no_such_tesla_account",
            ServiceError::Store(StoreError::Stale) => "stale",
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

type EndpointsFor = Arc<dyn Fn(Region) -> Endpoints + Send + Sync>;

/// How the service connects and how often it polls.
#[derive(Clone)]
pub struct Options {
    /// The owner's HTTP proxy (`/settings` `outbound.proxy`), else direct.
    pub proxy: Option<Proxy>,
    pub cadence: Cadence,
    pub stream: StreamTiming,
    endpoints: EndpointsFor,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            proxy: None,
            cadence: Cadence::default(),
            stream: StreamTiming::default(),
            endpoints: Arc::new(Endpoints::for_region),
        }
    }
}

impl Options {
    /// Every account talks to `e` (a fake Tesla). Not in a normal build.
    #[cfg(feature = "test-endpoints")]
    pub fn with_endpoints(mut self, e: Endpoints) -> Options {
        self.endpoints = Arc::new(move |_| e.clone());
        self
    }
}

/// A sign-in that was started: the address to open, and until when it waits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInStarted {
    pub id: String,
    pub url: String,
    pub expires_ms: u64,
}

/// Changes to an account other than its token and its cars.
#[derive(Debug, Clone, Default)]
pub struct Change {
    pub label: Option<String>,
    pub paused: Option<bool>,
    pub location: Option<Location>,
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
    sign_ins: Mutex<HashMap<String, (Region, SignIn)>>,
    clients: AtomicUsize,
}

/// The Tesla API input of one server.
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
    if l.is_empty() { "Tesla account".to_string() } else { l }
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
                sign_ins: Mutex::new(HashMap::new()),
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

    /// The cadence the pollers use now.
    pub fn cadence(&self) -> Cadence {
        self.opts().cadence
    }

    /// A new proxy or cadence (a settings change): every running poller starts again with it, on a new client (the
    /// proxy is part of the connection). Accounts that are not running stay so.
    pub fn reconfigure(&self, proxy: Option<Proxy>, cadence: Cadence) {
        {
            let mut o = self.inner.opts.write().unwrap_or_else(|p| p.into_inner());
            if o.proxy == proxy && o.cadence == cadence {
                return;
            }
            o.proxy = proxy;
            o.cadence = cadence;
        }
        let running: Vec<String> = lock(&self.inner.tasks).keys().cloned().collect();
        for id in running {
            if let Err(e) = self.spawn(&id, None) {
                tracing::warn!(account = %id, error = %e, "a Tesla account could not be started again");
            }
        }
    }

    /// Sign-ins started and not finished or run out yet.
    pub fn open_sign_ins(&self, now_ms: u64) -> usize {
        let mut open = lock(&self.inner.sign_ins);
        open.retain(|_, (_, s)| s.expires_ms() >= now_ms);
        open.len()
    }

    /// Whether a poller runs for the account.
    pub fn running(&self, id: &str) -> bool {
        lock(&self.inner.tasks).contains_key(id)
    }

    /// Starts a poller for every stored account (call inside the server's runtime). With no account: nothing.
    pub fn start(&self) {
        for id in self.inner.store.ids() {
            if let Err(e) = self.spawn(&id, None) {
                tracing::warn!(account = %id, error = %e, "a Tesla account could not be started");
            }
        }
    }

    /// Stops every poller (shutdown, or before a move switches servers).
    pub fn stop(&self) {
        for (_, t) in lock(&self.inner.tasks).drain() {
            t.join.abort();
        }
    }

    /// Clients built so far: 0 means this server has not made a single connection to Tesla.
    pub fn clients_built(&self) -> usize {
        self.inner.clients.load(Ordering::Relaxed)
    }

    fn client(&self, region: Region) -> Result<Arc<Http>, ServiceError> {
        let opts = self.opts();
        let http = Http::new((opts.endpoints)(region), opts.proxy).map_err(ServiceError::Http)?;
        self.inner.clients.fetch_add(1, Ordering::Relaxed);
        Ok(Arc::new(http))
    }

    fn spawn(&self, id: &str, http: Option<Arc<Http>>) -> Result<(), ServiceError> {
        let account = self.inner.store.get(id)?;
        let http = match http {
            Some(h) => h,
            None => self.client(account.region)?,
        };
        let opts = self.opts();
        let streamer = WsStreamer { http: http.clone(), timing: opts.stream };
        let poller = Poller::new(
            self.inner.store.clone(),
            id,
            Client::new(http),
            self.inner.sink.clone(),
            streamer,
            opts.cadence,
        )?;
        let wake = Arc::new(Notify::new());
        let status = Arc::new(Mutex::new(Some(poller.status())));
        let join = tokio::spawn(poller.run(SystemClock, wake.clone(), status.clone()));
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

    /// Every host the stored accounts may connect to (empty when there is no account).
    pub fn hosts(&self) -> Vec<String> {
        let opts = self.opts();
        let mut h: Vec<String> = self
            .accounts()
            .iter()
            .flat_map(|a| {
                let e = (opts.endpoints)(a.region);
                let mut h = e.hosts();
                if opts.cadence.invoices {
                    h.extend(e.invoice_host());
                }
                h
            })
            .collect();
        h.sort();
        h.dedup();
        h
    }

    pub fn accounts(&self) -> Vec<Account> {
        self.inner.store.list()
    }

    /// How far an account's Supercharger invoices are read (saved by its poller after each request).
    pub fn invoice_progress(&self, id: &str) -> Result<invoices::Progress, ServiceError> {
        Ok(self.inner.store.invoice_progress(id)?)
    }

    /// Whether an account's region has a host for the charging history (China has none).
    pub fn invoices_available(&self, region: Region) -> bool {
        (self.opts().endpoints)(region).ownership.is_some()
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

    /// Adds an account from a refresh token made elsewhere: refreshed once at once (which checks it and rotates
    /// it; the other app's copy stops working within a day), then the vehicle list is read.
    pub async fn add_with_refresh_token(
        &self,
        label: &str,
        region: Region,
        refresh_token: Secret,
        now_ms: u64,
    ) -> Result<Account, ServiceError> {
        if refresh_token.is_empty() || refresh_token.expose().len() > 8192 {
            return Err(ServiceError::Auth(AuthError::Refused));
        }
        let http = self.client(region)?;
        let tokens = auth::refresh(&http, &refresh_token).await.map_err(ServiceError::Auth)?;
        let refresh = tokens.refresh.clone().unwrap_or(refresh_token);
        self.add(label, region, Redirect::default(), tokens, refresh, http, now_ms).await
    }

    /// Starts a sign-in: the address of Tesla's page to open in the browser.
    pub fn sign_in_start(
        &self,
        region: Region,
        redirect: Redirect,
        now_ms: u64,
    ) -> Result<SignInStarted, ServiceError> {
        let mut open = lock(&self.inner.sign_ins);
        open.retain(|_, (_, s)| s.expires_ms() >= now_ms);
        if open.len() >= MAX_SIGN_INS {
            return Err(ServiceError::TooManySignIns);
        }
        let e = (self.opts().endpoints)(region);
        let (s, url) = SignIn::start(&e, redirect, now_ms).map_err(ServiceError::Auth)?;
        let mut idb = [0u8; 12];
        getrandom::fill(&mut idb).map_err(|_| ServiceError::Auth(AuthError::BadAnswer))?;
        let id = format!("si_{}", hex::encode(idb));
        let expires_ms = s.expires_ms();
        open.insert(id.clone(), (region, s));
        Ok(SignInStarted { id, url, expires_ms })
    }

    /// Finishes a sign-in with the pasted callback address. A sign-in is used once.
    pub async fn sign_in_finish(
        &self,
        id: &str,
        pasted: &str,
        label: &str,
        now_ms: u64,
    ) -> Result<Account, ServiceError> {
        let (region, mut s) = {
            let mut open = lock(&self.inner.sign_ins);
            let Some((region, s)) = open.get(id) else { return Err(ServiceError::NoSuchSignIn) };
            let e = (self.opts().endpoints)(*region);
            // A bad paste leaves the sign-in open for another try; a good one takes it.
            s.code_from(&e, pasted, now_ms).map_err(ServiceError::Auth)?;
            open.remove(id).ok_or(ServiceError::NoSuchSignIn)?
        };
        let http = self.client(region)?;
        let code = s.code_from(http.endpoints(), pasted, now_ms).map_err(ServiceError::Auth)?;
        let tokens = s.finish(&http, &code).await.map_err(ServiceError::Auth)?;
        let Some(refresh) = tokens.refresh.clone() else {
            return Err(ServiceError::Auth(AuthError::BadAnswer));
        };
        self.add(label, region, s.redirect, tokens, refresh, http, now_ms).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn add(
        &self,
        label: &str,
        region: Region,
        redirect: Redirect,
        tokens: Tokens,
        refresh: Secret,
        http: Arc<Http>,
        now_ms: u64,
    ) -> Result<Account, ServiceError> {
        let store = &self.inner.store;
        let creds = Credentials {
            refresh,
            access: Some(tokens.access.clone()),
            access_expires_ms: now_ms + tokens.expires_in_s * 1000,
        };
        // Stored first: the refresh token it replaced is spent.
        let account = store.add(
            Account {
                id: String::new(),
                label: clean_label(label),
                region,
                redirect,
                added_ms: now_ms,
                version: 0,
                paused: false,
                location: Location::default(),
                state: AccountState::Checking,
                last_ok_ms: None,
                last_error: None,
                cars: Vec::new(),
            },
            &creds,
        )?;
        let client = Client::new(http.clone());
        let list = match client.products(&tokens.access).await {
            Ok(l) => l,
            Err(e) => {
                let _ = store.remove(&account.id);
                return Err(ServiceError::Api(e));
            }
        };
        let cars = poller::account_cars(&list, store);
        let taken: Vec<String> = store
            .list()
            .into_iter()
            .filter(|a| a.id != account.id)
            .flat_map(|a| a.cars.into_iter().filter_map(|c| c.vin_tag))
            .collect();
        if !cars.is_empty() && cars.iter().all(|c| c.vin_tag.as_ref().is_some_and(|t| taken.contains(t))) {
            let _ = store.remove(&account.id);
            return Err(ServiceError::AccountExists);
        }
        let (account, ()) = store.update(&account.id, None, |a, _| {
            a.cars = cars;
            a.state = AccountState::Ok;
            a.last_ok_ms = Some(now_ms);
        })?;
        tracing::info!(account = %account.id, cars = account.cars.len(), "tesla account added");
        self.spawn(&account.id, Some(http))?;
        Ok(account)
    }

    /// A new refresh token for an account (after `sign_in_needed`): checked by one refresh before it is stored.
    pub async fn replace_token(
        &self,
        id: &str,
        version: u64,
        refresh_token: Secret,
        now_ms: u64,
    ) -> Result<Account, ServiceError> {
        let account = self.inner.store.get(id)?;
        if account.version != version {
            return Err(ServiceError::Stale);
        }
        let http = self.client(account.region)?;
        let tokens = auth::refresh(&http, &refresh_token).await.map_err(ServiceError::Auth)?;
        let creds = Credentials {
            refresh: tokens.refresh.clone().unwrap_or(refresh_token),
            access: Some(tokens.access),
            access_expires_ms: now_ms + tokens.expires_in_s * 1000,
        };
        // Stored whatever the version is now: the old token is spent.
        let (a, ()) = self.inner.store.update(id, None, |a, c| {
            *c = creds;
            a.state = AccountState::Ok;
            a.last_error = None;
        })?;
        if lock(&self.inner.tasks).contains_key(id) {
            self.wake(id);
        } else {
            self.spawn(id, Some(http))?;
        }
        Ok(a)
    }

    pub fn update(&self, id: &str, version: u64, change: Change) -> Result<Account, ServiceError> {
        let (a, ()) = self.inner.store.update(id, Some(version), |a, _| {
            if let Some(l) = &change.label {
                a.label = clean_label(l);
            }
            if let Some(p) = change.paused {
                a.paused = p;
                if !p && a.state == AccountState::Paused {
                    a.state = AccountState::Ok;
                }
            }
            if let Some(l) = change.location {
                a.location = l;
            }
        })?;
        self.wake(id);
        Ok(a)
    }

    /// Says which car each vehicle is. A car may be matched to one Tesla vehicle only.
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
        let mut taken: Vec<model::CarId> = store
            .list()
            .into_iter()
            .filter(|a| a.id != id)
            .flat_map(|a| a.cars.into_iter())
            .filter_map(|c| match c.matched {
                Some(Match::Car { car }) => Some(car),
                _ => None,
            })
            .collect();
        let changing: Vec<&str> = matches.iter().map(|(v, _)| v.as_str()).collect();
        taken.extend(account.cars.iter().filter(|c| !changing.contains(&c.vehicle.as_str())).filter_map(|c| {
            match c.matched {
                Some(Match::Car { car }) => Some(car),
                _ => None,
            }
        }));
        for (v, m) in matches {
            if !account.cars.iter().any(|c| &c.vehicle == v) {
                return Err(ServiceError::NoSuchVehicle);
            }
            if let Some(Match::Car { car }) = m {
                if taken.contains(car) {
                    return Err(ServiceError::CarTaken);
                }
                taken.push(*car);
            }
        }
        let (a, ()) = store.update(id, Some(version), |a, _| {
            for (v, m) in matches {
                if let Some(c) = a.cars.iter_mut().find(|c| &c.vehicle == v) {
                    c.matched = *m;
                }
            }
        })?;
        self.wake(id);
        Ok(a)
    }

    /// Forgets the account at once (its sealed file and tokens). History already stored stays.
    pub fn remove(&self, id: &str) -> Result<(), ServiceError> {
        self.inner.store.remove(id)?;
        if let Some(t) = lock(&self.inner.tasks).remove(id) {
            t.join.abort();
        }
        tracing::info!(account = %id, "tesla account removed");
        Ok(())
    }
}
