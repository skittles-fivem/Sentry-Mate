//! Sentry Mate's web side: the HTTPS server for the UI packs and the `/api/v1` JSON API (docs/ui-api.md), on its own
//! port ([`PORT`], 10101, beside the upload port 10100). TLS 1.3 only with the server's own certificate (the same
//! one the upload port uses), HTTP/1.1.
//!
//! # The frame every endpoint shares
//!
//! - **One guard for every API route** ([`policy`]): TLS only, a request rate per address, the CSRF header
//!   (`X-Sentry-Mate: 1`) on every change, the session from the `__Host-sentry-mate` cookie ([`accounts`]), the
//!   role, and every car a request names (the `{car}` in its path, `cars=` and `car=` in its query) checked against
//!   the account's car list, all before a handler runs. Changes are admin-only unless the route is on the short list
//!   a watch account may use, so a new route can't forget it. A hidden car and an unknown car give the same
//!   `404 no_such_car`. A car in a JSON body is read with [`JsonBody::car`] / [`JsonBody::cars_checked`], which
//!   check it the same way (`422 invalid` on the field, the same for a hidden and an unknown car); never read a
//!   car id from a body any other way.
//! - **Extractors** ([`ctx`]): [`Viewer`] (the signed-in account), [`Admin`], [`CarParam`] (the checked car of the
//!   path), [`CarsParam`] (`cars=`), [`Params`] (the query, with typed getters that answer `400 bad_input` with
//!   `fields`), [`JsonBody`] (a JSON object of at most 1 MB).
//! - **Answers**: [`ApiError`] is the one error shape; lists page with opaque, signed cursors ([`query`]); CSV with
//!   [`csv`]; the audit log with [`Viewer::audit`] (every change a handler does not record itself is recorded as a
//!   plain entry); security headers on every answer; `X-Sentry-Mate-Now-Ms` on every API answer.
//! - **Live stream** ([`events`]): `/api/v1/events`, server-sent events from [`Events`], filtered per account.
//! - **UI packs** ([`packs`]): the built-in packs embedded at build time, uploaded packs from a checked zip (sealed),
//!   the pack chosen for everyone and each person's own, `/original`, a strict CSP.
//!
//! # Adding endpoints
//!
//! Each resource group has its own module in [`resources`] with a `routes()` that returns a `Router<AppState>`; its
//! routes are written with their full path (`/api/v1/cars/{car}/drives`):
//!
//! ```ignore
//! pub fn routes() -> Router<AppState> {
//!     Router::new().route("/api/v1/cars/{car}/drives", get(list))
//! }
//!
//! async fn list(State(app): State<AppState>, v: Viewer, CarParam(car): CarParam, q: Params) -> ApiResult<Json<Value>> {
//!     let period = q.period(app.now(), None)?;
//!     let paging = q.paging()?;
//!     let after: Option<u64> = app.cursor(&q)?;
//!     // ... read rows from the service the program gave: app.service::<MyEngine>()?
//!     Ok(Json(json!({ "car": ids::car(car), "items": [], "next": null })))
//! }
//! ```
//!
//! Engine parts (the database, the timeline) are handed to the frame as services ([`Builder::service`]) and read
//! back with [`AppState::service`], so resource modules don't change this file.

// An error answer is built once per failed request; its size doesn't matter, and every handler returns one.
#![allow(clippy::result_large_err)]

pub mod accounts_admin;
pub mod auth;
pub mod csv;
pub mod ctx;
pub mod error;
pub mod events;
pub mod heavy;
pub mod ids;
pub mod moving;
pub mod packs;
pub mod policy;
pub mod prefs;
pub mod query;
pub mod recovery;
pub mod resources;
pub mod restore;
pub mod server;
#[cfg(feature = "testkit")]
pub mod testkit;
pub mod units;

pub use ctx::{Admin, CarParam, CarsParam, JsonBody, Viewer};
pub use error::{ApiError, ApiResult, Fields};
pub use events::{Event, Events, FleetChange};
pub use query::{Page, Params};
pub use server::{Conn, serve};

use accounts::{Accounts, Clock};
use axum::Router;
use axum::routing::get;
use model::CarId;
use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use vault::Vault;

/// The web port's default (the upload port is 10100).
pub const PORT: u16 = 10101;
/// The API version the server speaks (the `api` a UI pack's `ui.json` names).
pub const API_VERSION: u32 = 1;
/// Request bodies at most (JSON); a pack upload has its own limit ([`packs::MAX_ZIP_BYTES`]).
pub const MAX_BODY: usize = 1 << 20;

/// Which cars exist, for the access check of every car a request names. The engine provides it; until then
/// [`NoCars`] (no car exists) keeps every car-scoped route at `404 no_such_car`.
pub trait CarDirectory: Send + Sync + 'static {
    fn exists(&self, car: CarId) -> bool;
}

/// No cars at all.
pub struct NoCars;

impl CarDirectory for NoCars {
    fn exists(&self, _car: CarId) -> bool {
        false
    }
}

/// A fixed set of cars (tests, and a server before its engine is wired in).
pub struct FixedCars(pub std::collections::HashSet<CarId>);

impl CarDirectory for FixedCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.contains(&car)
    }
}

/// The server's state, shared by every request (cheap to clone).
#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    pub accounts: Arc<Accounts>,
    pub vault: Arc<Vault>,
    /// The web side's own folder (preferences, uploaded packs, the pack chosen for everyone).
    pub dir: PathBuf,
    pub clock: Clock,
    pub events: Events,
    pub cars: Arc<dyn CarDirectory>,
    pub packs: packs::Packs,
    pub prefs: prefs::Prefs,
    /// Server-wide defaults of everyone's preferences (the settings module keeps them in step with `/settings`).
    pub defaults: RwLock<units::Defaults>,
    /// Origins (`https://tile.example.org`) the pages may load map tiles from, besides the server itself; part of
    /// the packs' CSP. The settings module sets it from the chosen tile server.
    pub tile_origins: RwLock<Vec<String>>,
    /// Where the one-time setup code is written for the owner (`/keys/SETUP-CODE.txt`).
    pub setup_code_file: Option<PathBuf>,
    pub(crate) cursor_key: [u8; 32],
    pub(crate) rate: policy::RateLimit,
    /// How long a request body may take to arrive ([`ctx::read_body`]).
    pub(crate) body_timeout: std::time::Duration,
    /// The answers that decode stored logs: a few at a time ([`heavy`]).
    pub heavy: heavy::Heavy,
    services: HashMap<std::any::TypeId, Arc<dyn Any + Send + Sync>>,
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

/// Builds an [`AppState`].
pub struct Builder {
    accounts: Arc<Accounts>,
    vault: Arc<Vault>,
    dir: PathBuf,
    clock: Clock,
    cars: Arc<dyn CarDirectory>,
    events: Events,
    defaults: units::Defaults,
    tile_origins: Vec<String>,
    setup_code_file: Option<PathBuf>,
    requests_per_minute: u32,
    body_timeout: std::time::Duration,
    heavy: (usize, std::time::Duration),
    services: HashMap<std::any::TypeId, Arc<dyn Any + Send + Sync>>,
}

impl Builder {
    pub fn cars(mut self, cars: Arc<dyn CarDirectory>) -> Builder {
        self.cars = cars;
        self
    }

    pub fn clock(mut self, clock: Clock) -> Builder {
        self.clock = clock;
        self
    }

    /// The broadcast the engine publishes live values into (else a new one; [`AppState::events`] gives it).
    pub fn events(mut self, events: Events) -> Builder {
        self.events = events;
        self
    }

    pub fn defaults(mut self, d: units::Defaults) -> Builder {
        self.defaults = d;
        self
    }

    pub fn tile_origins(mut self, origins: Vec<String>) -> Builder {
        self.tile_origins = origins;
        self
    }

    /// Where [`AppState::announce_setup`] writes the setup code.
    pub fn setup_code_file(mut self, file: PathBuf) -> Builder {
        self.setup_code_file = Some(file);
        self
    }

    /// API requests one address may make per minute (bursts up to a fifth of it at once).
    pub fn requests_per_minute(mut self, n: u32) -> Builder {
        self.requests_per_minute = n.max(10);
        self
    }

    /// How long a JSON request body may take to arrive (default [`server::BODY_TIMEOUT`]; `408 timeout` and the
    /// connection closed beyond). A pack upload has its own, longer limit ([`packs::UPLOAD_TIMEOUT`]).
    pub fn body_timeout(mut self, d: std::time::Duration) -> Builder {
        self.body_timeout = d;
        self
    }

    /// Answers that decode stored logs running at once, and how long one more waits for its turn (default
    /// [`heavy::RUNNING`], [`heavy::WAIT`]).
    pub fn log_queries(mut self, running: usize, wait: std::time::Duration) -> Builder {
        self.heavy = (running, wait);
        self
    }

    /// Hands the frame an engine part (one per type) that handlers read back with [`AppState::service`].
    pub fn service<T: Send + Sync + 'static>(mut self, s: Arc<T>) -> Builder {
        self.services.insert(std::any::TypeId::of::<T>(), s);
        self
    }

    pub fn build(self) -> std::io::Result<AppState> {
        std::fs::create_dir_all(&self.dir)?;
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(std::io::Error::other)?;
        let packs = packs::Packs::open(self.dir.join("packs"), self.vault.clone())?;
        let prefs = prefs::Prefs::open(self.dir.join("prefs"), self.vault.clone())?;
        Ok(AppState(Arc::new(Inner {
            accounts: self.accounts,
            vault: self.vault,
            dir: self.dir,
            clock: self.clock,
            events: self.events,
            cars: self.cars,
            packs,
            prefs,
            defaults: RwLock::new(self.defaults),
            tile_origins: RwLock::new(self.tile_origins),
            setup_code_file: self.setup_code_file,
            cursor_key: key,
            rate: policy::RateLimit::new(self.requests_per_minute),
            body_timeout: self.body_timeout,
            heavy: heavy::Heavy::new(self.heavy.0, self.heavy.1),
            services: self.services,
        })))
    }
}

impl AppState {
    /// A state for the accounts in `accounts`, keeping its own files in `dir` (sealed with `vault`).
    pub fn builder(accounts: Arc<Accounts>, vault: Arc<Vault>, dir: impl Into<PathBuf>) -> Builder {
        Builder {
            accounts,
            vault,
            dir: dir.into(),
            clock: accounts::system_clock(),
            cars: Arc::new(NoCars),
            events: Events::new(),
            defaults: units::Defaults::default(),
            // no outside tile server until the settings turn one on
            tile_origins: Vec::new(),
            setup_code_file: None,
            requests_per_minute: 1200,
            body_timeout: server::BODY_TIMEOUT,
            heavy: (heavy::RUNNING, heavy::WAIT),
            services: HashMap::new(),
        }
    }

    /// The time now, ms since 1970 UTC.
    pub fn now(&self) -> u64 {
        (self.clock)()
    }

    /// An engine part handed over with [`Builder::service`]; `503 not_ready` when there is none.
    pub fn service<T: Send + Sync + 'static>(&self) -> ApiResult<Arc<T>> {
        self.services
            .get(&std::any::TypeId::of::<T>())
            .and_then(|s| s.clone().downcast::<T>().ok())
            .ok_or_else(|| ApiError::unavailable(std::any::type_name::<T>()))
    }

    /// Whether a car exists (not whether someone may see it: [`Viewer::sees`]).
    pub fn car_exists(&self, car: CarId) -> bool {
        self.cars.exists(car)
    }

    /// Runs account work (password hashing takes a few hundred ms on purpose) off the async threads.
    pub async fn blocking<R: Send + 'static>(
        &self,
        f: impl FnOnce(&AppState) -> ApiResult<R> + Send + 'static,
    ) -> ApiResult<R> {
        let me = self.clone();
        tokio::task::spawn_blocking(move || f(&me))
            .await
            .map_err(|e| ApiError::server(format!("a task failed: {e}")))?
    }

    /// The cursor `after` of a list request, as the key of the last row it was given (`None`: the first page). A
    /// cursor made for other filters or another sort, or not made by this server, is `400` on `after`.
    pub fn cursor<K: serde::de::DeserializeOwned>(&self, q: &Params) -> ApiResult<Option<K>> {
        query::read_cursor(&self.cursor_key, q)
    }

    /// The `next` cursor after the row whose key is `key`.
    pub fn next_cursor<K: serde::Serialize>(&self, q: &Params, key: &K) -> ApiResult<String> {
        query::make_cursor(&self.cursor_key, q, key)
    }

    /// Pages through rows already in the list's order, by a key that grows along it ([`Page`]).
    pub fn page<T, K>(&self, q: &Params, rows: impl IntoIterator<Item = T>, key: impl Fn(&T) -> K) -> ApiResult<Page<T>>
    where
        K: serde::Serialize + serde::de::DeserializeOwned + PartialOrd,
    {
        query::page(self, q, rows, key)
    }

    /// If no account exists yet: makes the one-time setup code, writes it to the setup code file, and prints it in
    /// the log, for the first admin to type on the setup page. `None` when setup is done.
    pub fn announce_setup(&self) -> ApiResult<Option<String>> {
        if !self.accounts.needs_setup() {
            return Ok(None);
        }
        let file = self.setup_code_file.clone().unwrap_or_else(|| self.dir.join("SETUP-CODE.txt"));
        let code = self.accounts.start_setup(&file)?;
        tracing::warn!(
            "No account yet. Open the web page and set up the first admin with this one-time code (valid 24 hours, \
             also in {}): {}",
            file.display(),
            code.code.as_str()
        );
        Ok(Some(code.code.to_string()))
    }

    /// The pack CSP's tile origins.
    pub fn tiles(&self) -> Vec<String> {
        self.tile_origins.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Sets the tile origins (from the tile server setting); only `https://host[:port]` origins are kept.
    pub fn set_tiles(&self, origins: Vec<String>) {
        let ok: Vec<String> = origins.into_iter().filter(|o| packs::valid_origin(o)).collect();
        *self.tile_origins.write().unwrap_or_else(|e| e.into_inner()) = ok;
    }

    /// The defaults of everyone's preferences.
    pub fn defaults(&self) -> units::Defaults {
        self.defaults.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Sets the defaults (from `/settings`) and tells every open stream to send its person's preferences again.
    pub fn set_defaults(&self, d: units::Defaults) {
        *self.defaults.write().unwrap_or_else(|e| e.into_inner()) = d;
        self.events.publish(Event::DefaultsChanged);
    }
}

/// The whole web side: the API under `/api/v1` (behind the guard), `/health`, `/original`, and the UI pack for
/// every other path. Serve it with [`serve`].
pub fn router(app: AppState) -> Router {
    router_with(app, Router::new())
}

/// [`router`] with more API routes behind the same guard (the program's own, or a probe in a test).
pub fn router_with(app: AppState, extra: Router<AppState>) -> Router {
    let api = Router::new()
        .merge(extra)
        .merge(auth::routes())
        .merge(accounts_admin::routes())
        .merge(prefs::routes())
        .merge(events::routes())
        .merge(packs::routes())
        .merge(moving::routes())
        .merge(recovery::routes())
        .merge(resources::routes())
        .route_layer(axum::middleware::from_fn_with_state(app.clone(), policy::guard));
    Router::new()
        .merge(api)
        .route("/health", get(|| async { axum::http::StatusCode::OK }))
        .route("/original", get(packs::original_on))
        .route("/original/off", get(packs::original_off))
        .fallback(packs::fallback)
        .method_not_allowed_fallback(policy::method_not_allowed)
        .layer(axum::middleware::from_fn_with_state(app.clone(), policy::headers))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
        .with_state(app)
}
