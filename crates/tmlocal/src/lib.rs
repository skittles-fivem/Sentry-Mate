//! The owner's own TeslaMate server as a live input (`model::Input::TeslamateLocal`, "Data from local TeslaMate"):
//! its MQTT broker read only ([`mqtt`]), and optionally its PostgreSQL database read in a read-only transaction for
//! catch-up ([`catchup`]), whose rows are stored as a TeslaMate import. Off until an admin adds it.
//!
//! With no settings file, [`Service::start`] starts nothing, so the server never connects to anything for it. The
//! rules an admin's settings must follow ([`validate`]) are kept here, so every caller gets them: a password never
//! goes over plain TCP unless the owner allowed it for the local network (`plaintext_password`), and a kept password
//! goes only to the host it was typed for (`password_resend`). Passwords are sealed in the vault ([`store`]) and are
//! never logged or returned ([`Secret`]).

pub mod catchup;
pub mod map;
pub mod mqtt;
pub mod store;
pub mod tls;

pub use mqtt::Failure;
pub use store::{Config, DbConfig, LocalCar, Match, MqttConfig, Password, Secret, Store, StoreError};

use model::{CarId, Sample};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
pub use tesla::{ApiFact, ApiMeta, ErrorInfo, NewCar, SinkError};
use vault::Vault;

/// The engine as this input sees it.
pub trait Sink: Send + Sync + 'static {
    /// Makes a car for a TeslaMate car the admin chose to add as new.
    fn new_car(&self, car: &NewCar) -> Result<CarId, SinkError>;
    /// Samples (by time) and what TeslaMate said about the car's state, from MQTT.
    fn points(&self, car: CarId, samples: &[Sample], facts: &[ApiFact]) -> Result<(), SinkError>;
    /// A catch-up dump (pg_dump-style `COPY ... FROM stdin;` blocks, see [`catchup`]) to store as a TeslaMate
    /// import, for the matched cars (`(TeslaMate car id, car here)`).
    fn catch_up(&self, dump: &std::path::Path, cars: &[(i64, CarId)]) -> Result<catchup::Counts, SinkError>;
    /// What TeslaMate said that a `Sample` has no field for (locked, the software version and update, the scheduled
    /// charge start, ...), from MQTT, at most every [`map::META_EVERY_MS`] per car.
    fn meta(&self, _car: CarId, _meta: &ApiMeta) {}
    /// The settings or status changed (for an audit entry or a notice): `status` is the status now. Told on every
    /// change of the settings, and when a [`Trouble`] begins, changes or ends (not again while it lasts).
    fn changed(&self, _status: &Status) {}
}

/// TeslaMate cars kept at most (a wrong broker can't fill the settings with made-up ids).
pub const MAX_CARS: usize = 32;

// ---------------------------------------------------------------- the database side

/// The catch-up's settings from the saved ones and the sealed password.
pub fn catchup_config(d: &DbConfig, password: Option<Secret>) -> catchup::DbConfig {
    catchup::DbConfig {
        host: d.host.clone(),
        port: d.port,
        database: d.database.clone(),
        username: d.username.clone(),
        password,
        tls: d.tls,
        pin_sha256: d.pin_sha256.clone(),
        allow_unencrypted: d.allow_unencrypted,
    }
}

fn db_failure(e: &catchup::CatchupError) -> Failure {
    Failure { code: e.code(), message: e.to_string() }
}

// ---------------------------------------------------------------- the rules for settings

/// A setting that breaks a rule: the field (`mqtt.password`...), the API code (`plaintext_password`,
/// `password_resend`, `invalid`) and plain words.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {message}")]
pub struct Invalid {
    pub field: String,
    pub code: &'static str,
    pub message: String,
}

/// New settings as an admin sends them.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    pub mqtt: MqttConfig,
    pub mqtt_password: Password,
    pub database: DbConfig,
    pub db_password: Password,
}

fn invalid(field: &str, message: &str) -> Invalid {
    Invalid { field: field.into(), code: "invalid", message: message.into() }
}

/// A host name or an IP address: no scheme, no path, no port.
pub fn host_ok(h: &str) -> bool {
    if h.is_empty() || h.len() > 253 {
        return false;
    }
    if h.contains(':') {
        return h.parse::<std::net::Ipv6Addr>().is_ok();
    }
    h.split('.').all(|l| {
        !l.is_empty() && l.len() <= 63 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }) && !h.starts_with('-')
}

/// An MQTT topic prefix to subscribe under: no wildcard, no `$` at the start, no empty level.
pub fn prefix_ok(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 128
        && !p.starts_with('$')
        && !p.contains(['+', '#'])
        && !p.chars().any(char::is_control)
        && p.split('/').all(|l| !l.is_empty())
}

fn text_ok(s: &str, max: usize) -> bool {
    s.chars().count() <= max && !s.chars().any(char::is_control)
}

/// Tidies a draft as it will be stored: trimmed host, the pin as plain lowercase hex, an empty user name as none.
pub fn normalize(d: &mut Draft) {
    d.mqtt.host = d.mqtt.host.trim().trim_start_matches('[').trim_end_matches(']').to_string();
    d.database.host = d.database.host.trim().trim_start_matches('[').trim_end_matches(']').to_string();
    d.mqtt.prefix = d.mqtt.prefix.trim().to_string();
    d.mqtt.username = d.mqtt.username.take().map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
    d.database.username = d.database.username.trim().to_string();
    d.database.database = d.database.database.trim().to_string();
    for pin in [&mut d.mqtt.pin_sha256, &mut d.database.pin_sha256] {
        let p = pin.take().filter(|p| !p.trim().is_empty());
        *pin = p.map(|p| tls::parse_pin(&p).map_or(p, hex::encode));
    }
    d.mqtt.password_set = false;
    d.database.password_set = false;
}

struct Side<'a> {
    name: &'static str,
    host: &'a str,
    port: u16,
    tls: bool,
    pin: Option<&'a str>,
    allow_unencrypted: bool,
    password: &'a Password,
    /// The saved host, port and TLS and whether a password is saved.
    saved: Option<(&'a str, u16, bool, bool)>,
}

fn check_side(s: &Side<'_>, out: &mut Vec<Invalid>) {
    let f = |k: &str| format!("{}.{k}", s.name);
    if !host_ok(s.host) {
        out.push(invalid(&f("host"), "A host name or IP address (no scheme, no port), e.g. 192.0.2.10."));
    }
    if s.port == 0 {
        out.push(invalid(&f("port"), "A port from 1 to 65535."));
    }
    if let Some(p) = s.pin {
        if tls::parse_pin(p).is_none() {
            out.push(invalid(&f("pin_sha256"), "The certificate's SHA-256: 64 hexadecimal digits (colons allowed)."));
        } else if !s.tls {
            out.push(invalid(&f("pin_sha256"), "A pinned certificate needs TLS on."));
        }
    }
    let saved_pw = s.saved.is_some_and(|(_, _, _, set)| set);
    if let Password::Set(p) = s.password
        && (p.is_empty() || p.expose().len() > 1024 || p.expose().chars().any(char::is_control))
    {
        out.push(invalid(&f("password"), "1 to 1024 characters (null removes the saved one)."));
    }
    let has_pw = match s.password {
        Password::Set(_) => true,
        Password::Remove => false,
        Password::Keep => saved_pw,
    };
    check_resend(s, out);
    if has_pw && !s.tls && !s.allow_unencrypted {
        out.push(Invalid {
            field: f("password"),
            code: "plaintext_password",
            message: "A password over plain TCP would cross the network readable: turn TLS on, or tick \"Allow \
                      unencrypted (local network only)\"."
                .into(),
        });
    }
}

/// A saved password that is kept goes only to the host, port and TLS setting it was typed for.
fn check_resend(s: &Side<'_>, out: &mut Vec<Invalid>) {
    if matches!(s.password, Password::Keep)
        && let Some((host, port, tls, true)) = s.saved
        && (host != s.host || port != s.port || tls != s.tls)
    {
        out.push(Invalid {
            field: format!("{}.password", s.name),
            code: "password_resend",
            message: "The host, port or TLS changed: type the password again (a saved password goes only to the host \
                      it was typed for), or send null to remove it."
                .into(),
        });
    }
}

/// The rules of new settings, against the saved ones (`None`: not added yet). All the problems, not only the first.
pub fn validate(d: &Draft, saved: Option<&Config>) -> Result<(), Vec<Invalid>> {
    let mut out = Vec::new();
    let (m, db) = (&d.mqtt, &d.database);
    check_side(
        &Side {
            name: "mqtt",
            host: &m.host,
            port: m.port,
            tls: m.tls,
            pin: m.pin_sha256.as_deref(),
            allow_unencrypted: m.allow_unencrypted,
            password: &d.mqtt_password,
            saved: saved.map(|c| (c.mqtt.host.as_str(), c.mqtt.port, c.mqtt.tls, c.mqtt.password_set)),
        },
        &mut out,
    );
    if !prefix_ok(&m.prefix) {
        out.push(invalid("mqtt.prefix", "TeslaMate's MQTT prefix (teslamate): no +, #, leading $ or empty level."));
    }
    if m.username.as_deref().is_some_and(|u| !text_ok(u, 256)) {
        out.push(invalid("mqtt.username", "At most 256 characters."));
    }
    if matches!(d.mqtt_password, Password::Set(_)) && m.username.is_none() {
        out.push(invalid("mqtt.username", "A password needs a user name."));
    }
    // the database: checked only when it is used (or a password is given for it); a kept password stays with its
    // host all the same (else a host saved while the database is off would get it once the database is on again)
    let db_used = db.enabled || matches!(d.db_password, Password::Set(_));
    let side = Side {
        name: "database",
        host: &db.host,
        port: db.port,
        tls: db.tls,
        pin: db.pin_sha256.as_deref(),
        allow_unencrypted: db.allow_unencrypted,
        password: &d.db_password,
        saved: saved.map(|c| (c.database.host.as_str(), c.database.port, c.database.tls, c.database.password_set)),
    };
    if db_used {
        check_side(&side, &mut out);
        if db.database.is_empty() || !text_ok(&db.database, 63) {
            out.push(invalid("database.database", "The database's name (teslamate), at most 63 characters."));
        }
        if db.username.is_empty() || !text_ok(&db.username, 63) {
            out.push(invalid("database.username", "The database user (teslamate), at most 63 characters."));
        }
        if !(60..=86_400).contains(&db.interval_s) {
            out.push(invalid("database.interval_s", "From 60 to 86400 seconds."));
        }
    } else {
        check_resend(&side, &mut out);
        if db.pin_sha256.as_deref().is_some_and(|p| tls::parse_pin(p).is_none()) {
            out.push(invalid(
                "database.pin_sha256",
                "The certificate's SHA-256: 64 hexadecimal digits (colons allowed).",
            ));
        }
    }
    if out.is_empty() { Ok(()) } else { Err(out) }
}

// ---------------------------------------------------------------- car matching

/// A car here, as a suggestion looks at it.
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub car: CarId,
    pub name: &'a str,
    pub vin_prefix: Option<&'a str>,
}

fn same_prefix(a: &str, b: &str) -> bool {
    let n = a.len().min(b.len());
    n >= 10 && a[..n].eq_ignore_ascii_case(&b[..n])
}

/// The car a TeslaMate car most likely is: the one car with its VIN prefix (when known), else the one car named as
/// TeslaMate names it. Only a suggestion: an admin confirms it.
pub fn suggest(c: &LocalCar, cars: &[Candidate<'_>]) -> Option<CarId> {
    let one = |found: Vec<CarId>| if found.len() == 1 { found.first().copied() } else { None };
    if let Some(p) = c.vin_prefix.as_deref().filter(|p| !p.is_empty())
        && let Some(car) =
            one(cars.iter().filter(|k| k.vin_prefix.is_some_and(|v| same_prefix(v, p))).map(|k| k.car).collect())
    {
        return Some(car);
    }
    let name = c.display_name.as_deref()?.trim();
    if name.is_empty() {
        return None;
    }
    one(cars.iter().filter(|k| k.name.trim().eq_ignore_ascii_case(name)).map(|k| k.car).collect())
}

// ---------------------------------------------------------------- status

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MqttState {
    #[default]
    Off,
    Connecting,
    Connected,
    Error,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DbState {
    #[default]
    Off,
    Syncing,
    Ok,
    Error,
    Paused,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct MqttStatus {
    pub state: MqttState,
    pub connected_since_ms: Option<u64>,
    pub last_message_ms: Option<u64>,
    pub messages_today: u64,
    pub last_error: Option<ErrorInfo>,
    /// Connected, but the broker has sent nothing for [`Options::quiet_after_ms`]: since when (its newest message,
    /// else the connection). Cleared by the next message, and whenever the connection ends.
    #[serde(skip)]
    pub quiet_since_ms: Option<u64>,
    #[serde(skip)]
    day: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct DbStatus {
    pub state: DbState,
    pub last_sync_ms: Option<u64>,
    pub next_sync_ms: Option<u64>,
    pub drives: u64,
    pub charges: u64,
    pub positions: u64,
    pub last_error: Option<ErrorInfo>,
}

/// What the input is doing.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Status {
    pub mqtt: MqttStatus,
    pub database: DbStatus,
    /// The newest message of each TeslaMate car (by its id).
    #[serde(skip)]
    pub cars_last_message_ms: HashMap<String, u64>,
}

/// Something wrong with the input an admin should hear about (a notice), each with its own fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Trouble {
    /// The broker could not be reached: it refused the connection, did not answer, or its TLS handshake failed.
    ConnectionRefused,
    /// The broker refused the user name or password (or the subscription).
    AuthFailed,
    /// The broker's certificate does not have the pinned SHA-256.
    PinMismatch,
    /// Connected, but the broker sent nothing for [`Options::quiet_after_ms`].
    Quiet,
    /// The last database catch-up failed (whatever the cause).
    CatchupFailed,
}

impl Trouble {
    pub const ALL: [Trouble; 5] =
        [Trouble::ConnectionRefused, Trouble::AuthFailed, Trouble::PinMismatch, Trouble::Quiet, Trouble::CatchupFailed];

    /// The trouble's word: `connection_refused`, `auth_failed`, `pin_mismatch`, `quiet` or `catchup_failed`.
    pub fn word(self) -> &'static str {
        match self {
            Trouble::ConnectionRefused => "connection_refused",
            Trouble::AuthFailed => "auth_failed",
            Trouble::PinMismatch => "pin_mismatch",
            Trouble::Quiet => "quiet",
            Trouble::CatchupFailed => "catchup_failed",
        }
    }

    /// Which side it is on.
    pub fn target(self) -> Target {
        match self {
            Trouble::ConnectionRefused | Trouble::AuthFailed | Trouble::PinMismatch | Trouble::Quiet => Target::Mqtt,
            Trouble::CatchupFailed => Target::Database,
        }
    }

    /// The MQTT trouble of a `last_error` code (`connection_refused`, `timeout`, `tls_failed`, `auth_failed`,
    /// `pin_mismatch`); a code not known is taken as the broker not reached.
    pub fn of_mqtt_code(code: &str) -> Trouble {
        match code {
            "auth_failed" => Trouble::AuthFailed,
            "pin_mismatch" => Trouble::PinMismatch,
            "connection_refused" | "timeout" | "tls_failed" => Trouble::ConnectionRefused,
            other => {
                tracing::debug!(code = other, "local TeslaMate: an MQTT error code not known");
                Trouble::ConnectionRefused
            }
        }
    }
}

/// A [`Trouble`] now, with its error (none for [`Trouble::Quiet`]) and since when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub trouble: Trouble,
    pub error: Option<ErrorInfo>,
    pub since_ms: Option<u64>,
}

impl Status {
    /// The MQTT side's trouble now: its connection failing (by its error), or connected and quiet.
    pub fn mqtt_problem(&self) -> Option<Problem> {
        let m = &self.mqtt;
        match m.state {
            MqttState::Error => m.last_error.as_ref().map(|e| Problem {
                trouble: Trouble::of_mqtt_code(&e.code),
                error: Some(e.clone()),
                since_ms: Some(e.at_ms),
            }),
            MqttState::Connected => {
                m.quiet_since_ms.map(|t| Problem { trouble: Trouble::Quiet, error: None, since_ms: Some(t) })
            }
            MqttState::Off | MqttState::Connecting | MqttState::Paused => None,
        }
    }

    /// The database side's trouble now: the last catch-up failed (also while the next one runs).
    pub fn db_problem(&self) -> Option<Problem> {
        let d = &self.database;
        match d.state {
            DbState::Error | DbState::Syncing => d.last_error.as_ref().map(|e| Problem {
                trouble: Trouble::CatchupFailed,
                error: Some(e.clone()),
                since_ms: Some(e.at_ms),
            }),
            DbState::Off | DbState::Ok | DbState::Paused => None,
        }
    }

    /// Every trouble now, MQTT's first.
    pub fn problems(&self) -> Vec<Problem> {
        self.mqtt_problem().into_iter().chain(self.db_problem()).collect()
    }
}

/// The trouble last told to the sink (another error of the same trouble, a refusal after a timeout, is not told
/// again).
type Told = Option<Trouble>;

fn told_of(p: Option<&Problem>) -> Told {
    p.map(|p| p.trouble)
}

fn err(f: &Failure, at_ms: u64) -> ErrorInfo {
    ErrorInfo { code: f.code.to_string(), message: f.message.clone(), at_ms }
}

// ---------------------------------------------------------------- the service

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("the local TeslaMate is not added")]
    NotAdded,
    #[error("the settings were changed meanwhile; read them again")]
    Stale,
    #[error("{}", .0.first().map_or(String::new(), ToString::to_string))]
    Invalid(Vec<Invalid>),
    #[error("no such TeslaMate car")]
    NoSuchCar,
    #[error("that car is already matched to another TeslaMate car")]
    CarTaken,
    #[error("{0}")]
    Store(StoreError),
}

impl ServiceError {
    /// The API error code.
    pub fn code(&self) -> &'static str {
        match self {
            ServiceError::NotAdded => "teslamate_not_added",
            ServiceError::Stale => "stale",
            ServiceError::Invalid(v) => v.first().map_or("invalid", |i| i.code),
            ServiceError::NoSuchCar => "no_such_teslamate_car",
            ServiceError::CarTaken => "car_taken",
            ServiceError::Store(_) => "storage",
        }
    }
}

impl From<StoreError> for ServiceError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => ServiceError::NotAdded,
            StoreError::Stale => ServiceError::Stale,
            e => ServiceError::Store(e),
        }
    }
}

/// How the service connects and how often it hands samples on.
#[derive(Clone)]
pub struct Options {
    pub connect_timeout: Duration,
    /// The first and the longest wait before connecting again.
    pub backoff: (Duration, Duration),
    /// At most one sample per car this often.
    pub sample_every_ms: u64,
    /// How long a state TeslaMate said is taken to last after it was last said.
    pub state_fresh_ms: u64,
    /// How long a test listens for TeslaMate's cars.
    pub test_listen: Duration,
    /// Connected and nothing from the broker this long: it has gone quiet ([`Trouble::Quiet`]). TeslaMate sends
    /// little while its cars sleep, so this is long.
    pub quiet_after_ms: u64,
}

/// [`Options::quiet_after_ms`] by default: a day.
pub const QUIET_AFTER_MS: u64 = 86_400_000;

impl Default for Options {
    fn default() -> Self {
        Options {
            connect_timeout: Duration::from_secs(10),
            backoff: (mqtt::RECONNECT_MIN, mqtt::RECONNECT_MAX),
            sample_every_ms: 1_000,
            state_fresh_ms: 3_600_000,
            test_listen: Duration::from_secs(3),
            quiet_after_ms: QUIET_AFTER_MS,
        }
    }
}

/// What a Test button tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Mqtt,
    Database,
}

/// How a test went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub error: Option<&'static str>,
    pub message: String,
    pub latency_ms: Option<u64>,
    pub at_ms: u64,
    pub cars_seen: Vec<String>,
}

struct Inner<S: Sink> {
    store: Store,
    sink: Arc<S>,
    opts: Options,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    status: Arc<Mutex<Status>>,
}

/// The local TeslaMate input of one server.
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

fn match_car(m: Option<Match>) -> Option<CarId> {
    match m {
        Some(Match::Car { car }) => Some(car),
        _ => None,
    }
}

impl<S: Sink> Service<S> {
    /// Builds nothing that talks to the network.
    pub fn new(data: &Path, vault: Arc<Vault>, sink: Arc<S>, opts: Options) -> Service<S> {
        Service {
            inner: Arc::new(Inner {
                store: Store::new(data, vault),
                sink,
                opts,
                tasks: Mutex::new(Vec::new()),
                status: Arc::new(Mutex::new(Status::default())),
            }),
        }
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    /// The settings (`None`: not added).
    pub fn config(&self) -> Option<Config> {
        match self.inner.store.load() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "the local TeslaMate settings could not be read");
                None
            }
        }
    }

    pub fn status(&self) -> Status {
        lock(&self.inner.status).clone()
    }

    /// Whether a connection task runs.
    pub fn running(&self) -> bool {
        lock(&self.inner.tasks).iter().any(|t| !t.is_finished())
    }

    /// `host:port` of the broker (and of the database while it is on), as saved; empty when not added.
    pub fn hosts(&self) -> Vec<String> {
        let Some(c) = self.config() else { return Vec::new() };
        let mut out = vec![mqtt::Broker::new(&c.mqtt, None).address()];
        if c.database.enabled {
            out.push(db_address(&c.database));
        }
        out
    }

    /// Starts reading (call inside the server's runtime). Not added, or paused: nothing connects.
    pub fn start(&self) {
        self.stop();
        let Some(c) = self.config() else {
            *lock(&self.inner.status) = Status::default();
            return;
        };
        {
            let mut st = lock(&self.inner.status);
            st.mqtt.connected_since_ms = None;
            st.mqtt.quiet_since_ms = None;
            if c.paused {
                st.mqtt.state = MqttState::Paused;
                st.database.state = if c.database.enabled { DbState::Paused } else { DbState::Off };
                st.database.next_sync_ms = None;
                return;
            }
            st.mqtt.state = MqttState::Connecting;
            st.database.state = DbState::Off;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("local TeslaMate: not started (no runtime)");
            return;
        };
        let mut tasks = lock(&self.inner.tasks);
        let password = self.inner.store.mqtt_password().ok().flatten();
        let broker = mqtt::Broker::new(&c.mqtt, password);
        tracing::info!(broker = %broker.address(), tls = broker.tls, "local TeslaMate: reading its MQTT broker");
        let live = Live {
            store: self.inner.store.clone(),
            sink: self.inner.sink.clone(),
            status: self.inner.status.clone(),
            generation: self.inner.store.generation(),
            config: c.clone(),
            cars: HashMap::new(),
            sent: HashMap::new(),
            every_ms: self.inner.opts.sample_every_ms,
            fresh_ms: self.inner.opts.state_fresh_ms,
            quiet_after_ms: self.inner.opts.quiet_after_ms,
            told: None,
        };
        tasks.push(rt.spawn(mqtt::run(broker, self.inner.opts.connect_timeout, self.inner.opts.backoff, live)));
        if c.database.enabled {
            tasks.push(rt.spawn(db_loop(self.inner.store.clone(), self.inner.sink.clone(), self.inner.status.clone())));
        }
    }

    /// Stops reading at once (shutdown, a change of settings, before a move switches servers).
    pub fn stop(&self) {
        for t in lock(&self.inner.tasks).drain(..) {
            t.abort();
        }
        let mut st = lock(&self.inner.status);
        st.mqtt.state = MqttState::Off;
        st.mqtt.connected_since_ms = None;
        st.mqtt.quiet_since_ms = None;
        st.database.state = DbState::Off;
        st.database.next_sync_ms = None;
    }

    /// Adds (`version` 0) or replaces the settings, after [`validate`]. Starts reading with them.
    pub fn put(&self, version: u64, mut draft: Draft, now_ms: u64) -> Result<Config, ServiceError> {
        normalize(&mut draft);
        let saved = self.inner.store.load()?;
        validate(&draft, saved.as_ref()).map_err(ServiceError::Invalid)?;
        let mut config = saved.clone().unwrap_or(Config {
            version: 0,
            added_ms: now_ms,
            paused: false,
            mqtt: MqttConfig::default(),
            database: DbConfig::default(),
            cars: Vec::new(),
        });
        config.mqtt = draft.mqtt;
        config.database = draft.database;
        let c = self.inner.store.put(version, config, &draft.mqtt_password, &draft.db_password, now_ms)?;
        tracing::info!(version = c.version, "local TeslaMate settings saved");
        self.start();
        self.inner.sink.changed(&self.status());
        Ok(c)
    }

    /// Pauses or resumes reading.
    pub fn set_paused(&self, version: u64, paused: bool) -> Result<Config, ServiceError> {
        let (c, ()) = self.inner.store.update(Some(version), |c| c.paused = paused)?;
        self.start();
        self.inner.sink.changed(&self.status());
        Ok(c)
    }

    /// Says which car each TeslaMate car is. A car here may be matched to one TeslaMate car only.
    pub fn set_matches(&self, version: u64, matches: &[(String, Option<Match>)]) -> Result<Config, ServiceError> {
        let c = self.inner.store.get()?;
        if c.version != version {
            return Err(ServiceError::Stale);
        }
        let changing: Vec<&str> = matches.iter().map(|(t, _)| t.as_str()).collect();
        let before: Vec<(String, Option<Match>)> =
            c.cars.iter().map(|k| (k.teslamate_car.clone(), k.matched)).collect();
        let mut taken: Vec<CarId> = c
            .cars
            .iter()
            .filter(|k| !changing.contains(&k.teslamate_car.as_str()))
            .filter_map(|k| match_car(k.matched))
            .collect();
        for (t, m) in matches {
            if c.car(t).is_none() {
                return Err(ServiceError::NoSuchCar);
            }
            if let Some(car) = match_car(*m) {
                if taken.contains(&car) {
                    return Err(ServiceError::CarTaken);
                }
                taken.push(car);
            }
        }
        let (c, ()) = self.inner.store.update(Some(version), |c| {
            for (t, m) in matches {
                if let Some(k) = c.cars.iter_mut().find(|k| &k.teslamate_car == t) {
                    k.matched = *m;
                }
            }
        })?;
        // a TeslaMate car matched to another car here is caught up again from its start: its catch-up mark was of
        // the car it was matched to before, and the newly matched car has none of its history yet (the import skips
        // what is stored already)
        let moved: Vec<i64> = matches
            .iter()
            .filter(|(t, m)| before.iter().any(|(bt, bm)| bt == t && bm != m))
            .filter_map(|(t, _)| t.parse().ok())
            .collect();
        if !moved.is_empty() {
            let mut cursor = self.inner.store.cursor()?;
            let had = cursor.cars.len();
            cursor.cars.retain(|id, _| !moved.contains(id));
            if cursor.cars.len() != had {
                self.inner.store.set_cursor(&cursor)?;
            }
        }
        self.inner.sink.changed(&self.status());
        Ok(c)
    }

    /// Stops at once and deletes the sealed file (its passwords). History already stored stays.
    pub fn remove(&self) -> Result<(), ServiceError> {
        self.inner.store.remove()?;
        self.stop();
        *lock(&self.inner.status) = Status::default();
        tracing::info!("local TeslaMate removed");
        self.inner.sink.changed(&self.status());
        Ok(())
    }

    /// Tries a draft (or, with none, the saved settings): connects, subscribes (or lists the database's cars) and
    /// says how it went. Publishes nothing, stores nothing. A draft without a password and with the saved host,
    /// port and TLS uses the saved password.
    pub async fn test(&self, target: Target, draft: Option<Draft>, now_ms: u64) -> Result<TestResult, ServiceError> {
        let saved = self.inner.store.load()?;
        let (cfg, mqtt_pw, db_pw) = match draft {
            Some(mut d) => {
                normalize(&mut d);
                // only the side under test is checked
                let mut errs = validate(&d, saved.as_ref()).err().unwrap_or_default();
                let side = match target {
                    Target::Mqtt => "mqtt.",
                    Target::Database => "database.",
                };
                errs.retain(|e| e.field.starts_with(side));
                if target == Target::Database && !d.database.enabled {
                    let mut on = d.clone();
                    on.database.enabled = true;
                    errs = validate(&on, saved.as_ref()).err().unwrap_or_default();
                    errs.retain(|e| e.field.starts_with(side));
                }
                if !errs.is_empty() {
                    return Err(ServiceError::Invalid(errs));
                }
                let resolve = |p: &Password, saved_pw: Option<Secret>| match p {
                    Password::Set(s) => Some(s.clone()),
                    Password::Remove => None,
                    Password::Keep => saved_pw,
                };
                let same_m = saved.as_ref().is_some_and(|s| {
                    (s.mqtt.host.as_str(), s.mqtt.port, s.mqtt.tls) == (d.mqtt.host.as_str(), d.mqtt.port, d.mqtt.tls)
                });
                let same_d = saved.as_ref().is_some_and(|s| {
                    (s.database.host.as_str(), s.database.port, s.database.tls)
                        == (d.database.host.as_str(), d.database.port, d.database.tls)
                });
                let mp = if same_m { self.inner.store.mqtt_password().ok().flatten() } else { None };
                let dp = if same_d { self.inner.store.db_password().ok().flatten() } else { None };
                let cfg = Config {
                    version: 0,
                    added_ms: 0,
                    paused: false,
                    mqtt: d.mqtt.clone(),
                    database: d.database.clone(),
                    cars: Vec::new(),
                };
                (cfg, resolve(&d.mqtt_password, mp), resolve(&d.db_password, dp))
            }
            None => {
                let c = saved.ok_or(ServiceError::NotAdded)?;
                (c, self.inner.store.mqtt_password().ok().flatten(), self.inner.store.db_password().ok().flatten())
            }
        };
        let started = std::time::Instant::now();
        Ok(match target {
            Target::Mqtt => {
                let broker = mqtt::Broker::new(&cfg.mqtt, mqtt_pw);
                let p = mqtt::probe(&broker, self.inner.opts.connect_timeout, self.inner.opts.test_listen).await;
                match p.failure {
                    None => TestResult {
                        ok: true,
                        error: None,
                        message: if p.cars_seen.is_empty() {
                            format!(
                                "Connected to {} and subscribed; no TeslaMate car sent a value yet.",
                                broker.address()
                            )
                        } else {
                            format!(
                                "Connected to {} and subscribed; TeslaMate cars seen: {}.",
                                broker.address(),
                                p.cars_seen.join(", ")
                            )
                        },
                        latency_ms: p.latency_ms,
                        at_ms: now_ms,
                        cars_seen: p.cars_seen,
                    },
                    Some(f) => TestResult {
                        ok: false,
                        error: Some(f.code),
                        message: format!("{}: {}", broker.address(), f.message),
                        latency_ms: None,
                        at_ms: now_ms,
                        cars_seen: Vec::new(),
                    },
                }
            }
            Target::Database => {
                let address = db_address(&cfg.database);
                match catchup::test(&catchup_config(&cfg.database, db_pw)).await {
                    Ok(cars) => {
                        let seen: Vec<String> = cars.iter().map(|c| c.id.to_string()).collect();
                        TestResult {
                            ok: true,
                            error: None,
                            message: format!("Connected to {address}; TeslaMate cars: {}.", seen.len()),
                            latency_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
                            at_ms: now_ms,
                            cars_seen: seen,
                        }
                    }
                    Err(e) => TestResult {
                        ok: false,
                        error: Some(e.code()),
                        message: format!("{address}: {e}"),
                        latency_ms: None,
                        at_ms: now_ms,
                        cars_seen: Vec::new(),
                    },
                }
            }
        })
    }
}

/// `host:port` of the database.
pub fn db_address(d: &DbConfig) -> String {
    if d.host.contains(':') { format!("[{}]:{}", d.host, d.port) } else { format!("{}:{}", d.host, d.port) }
}

// ---------------------------------------------------------------- the MQTT side's handler

/// What the MQTT task keeps: the settings it reads with, the newest values of each car.
struct Live<S: Sink> {
    store: Store,
    sink: Arc<S>,
    status: Arc<Mutex<Status>>,
    generation: u64,
    config: Config,
    cars: HashMap<String, map::Car>,
    /// Per car: when a state fact was last handed on, and its state.
    sent: HashMap<String, (u64, Option<String>)>,
    every_ms: u64,
    fresh_ms: u64,
    quiet_after_ms: u64,
    /// The MQTT trouble last told to the sink.
    told: Told,
}

impl<S: Sink> Live<S> {
    /// Tells the sink when the MQTT side's trouble began, changed or ended.
    fn tell_trouble(&mut self) {
        let st = lock(&self.status).clone();
        let now = told_of(st.mqtt_problem().as_ref());
        if now != self.told {
            self.told = now;
            self.sink.changed(&st);
        }
    }

    /// Connected and nothing came for [`Options::quiet_after_ms`]: the broker has gone quiet.
    fn check_quiet(&mut self, now_ms: u64) {
        {
            let mut st = lock(&self.status);
            if st.mqtt.state != MqttState::Connected || st.mqtt.quiet_since_ms.is_some() {
                return;
            }
            let Some(last) = st.mqtt.last_message_ms.max(st.mqtt.connected_since_ms) else { return };
            if now_ms.saturating_sub(last) < self.quiet_after_ms {
                return;
            }
            st.mqtt.quiet_since_ms = Some(last);
        }
        tracing::warn!("local TeslaMate: its MQTT broker has sent nothing for a while");
        self.tell_trouble();
    }

    fn sync(&mut self) {
        let g = self.store.generation();
        if g == self.generation {
            return;
        }
        self.generation = g;
        if let Ok(c) = self.store.get() {
            self.config = c;
        }
    }

    fn save(&mut self, change: impl FnOnce(&mut Config)) {
        match self.store.update(None, change) {
            Ok((c, ())) => {
                self.config = c;
                self.generation = self.store.generation();
            }
            Err(e) => tracing::warn!(error = %e, "the local TeslaMate settings could not be saved"),
        }
    }

    fn make_new_cars(&mut self) {
        let todo: Vec<LocalCar> = self.config.cars.iter().filter(|c| c.matched == Some(Match::New)).cloned().collect();
        for c in todo {
            let new = NewCar {
                vin_prefix: c.vin_prefix.clone(),
                model: None,
                year: None,
                display_name: c.display_name.clone(),
            };
            match self.sink.new_car(&new) {
                Ok(car) => self.save(|cfg| {
                    if let Some(k) = cfg.cars.iter_mut().find(|k| k.teslamate_car == c.teslamate_car) {
                        k.matched = Some(Match::Car { car });
                    }
                }),
                Err(e) => tracing::warn!(error = %e, "a car for a TeslaMate car could not be made"),
            }
        }
    }
}

impl<S: Sink> mqtt::Handler for Live<S> {
    fn connected(&mut self, now_ms: u64) {
        let mut st = lock(&self.status);
        st.mqtt.state = MqttState::Connected;
        st.mqtt.connected_since_ms = Some(now_ms);
        st.mqtt.last_error = None;
        st.mqtt.quiet_since_ms = None;
        drop(st);
        tracing::info!("local TeslaMate: connected to its MQTT broker");
        self.tell_trouble();
    }

    fn message(&mut self, topic: &str, payload: &[u8], now_ms: u64) {
        let Some((id, name)) = map::split_topic(&self.config.mqtt.prefix, topic) else { return };
        let was_quiet = {
            let mut st = lock(&self.status);
            let day = now_ms / 86_400_000;
            if st.mqtt.day != day {
                st.mqtt.day = day;
                st.mqtt.messages_today = 0;
            }
            st.mqtt.messages_today += 1;
            st.mqtt.last_message_ms = Some(now_ms);
            if st.cars_last_message_ms.len() < MAX_CARS || st.cars_last_message_ms.contains_key(id) {
                st.cars_last_message_ms.insert(id.to_string(), now_ms);
            }
            st.mqtt.quiet_since_ms.take().is_some()
        };
        if was_quiet {
            self.tell_trouble();
        }
        if self.config.car(id).is_none() {
            if self.config.cars.len() >= MAX_CARS {
                return;
            }
            tracing::info!(teslamate_car = %id, "local TeslaMate sent values of a car not matched yet");
            let car = LocalCar { teslamate_car: id.to_string(), display_name: None, vin_prefix: None, matched: None };
            self.save(|c| {
                if c.car(&car.teslamate_car).is_none() {
                    c.cars.push(car);
                }
            });
            let st = lock(&self.status).clone();
            self.sink.changed(&st);
        }
        let payload = String::from_utf8_lossy(payload);
        let car = self.cars.entry(id.to_string()).or_default();
        if let map::Applied::Name(n) = car.apply(name, &payload, now_ms)
            && self.config.car(id).is_some_and(|c| c.display_name.as_deref() != Some(n.as_str()))
        {
            let id = id.to_string();
            self.save(|c| {
                if let Some(k) = c.cars.iter_mut().find(|k| k.teslamate_car == id) {
                    k.display_name = Some(n);
                }
            });
        }
    }

    fn tick(&mut self, now_ms: u64) {
        self.sync();
        if self.config.paused {
            return;
        }
        self.check_quiet(now_ms);
        self.make_new_cars();
        for k in &self.config.cars {
            let Some(car) = match_car(k.matched) else { continue };
            let Some(acc) = self.cars.get_mut(&k.teslamate_car) else { continue };
            if let Some(m) = acc.take_meta(now_ms) {
                self.sink.meta(car, &m);
            }
            let samples: Vec<Sample> = acc.take(now_ms, self.every_ms).into_iter().collect();
            let (last_ms, last_state) = self.sent.get(&k.teslamate_car).cloned().unwrap_or((0, None));
            let state = acc.state().map(str::to_string);
            // a state fact when the state changes, with a sample, and once a minute
            let due = state != last_state || !samples.is_empty() || now_ms >= last_ms + 60_000;
            let facts: Vec<ApiFact> =
                if due { acc.fact(now_ms, self.fresh_ms).into_iter().collect() } else { Vec::new() };
            if !facts.is_empty() {
                self.sent.insert(k.teslamate_car.clone(), (now_ms, state));
            }
            if (!samples.is_empty() || !facts.is_empty())
                && let Err(e) = self.sink.points(car, &samples, &facts)
            {
                tracing::warn!(error = %e, "local TeslaMate data could not be stored");
            }
        }
    }

    fn failed(&mut self, failure: &Failure, _retry_ms: u64) {
        {
            let mut st = lock(&self.status);
            // a broker that keeps failing the same way: since its first failure (not a new problem each try)
            let at = st
                .mqtt
                .last_error
                .as_ref()
                .filter(|e| e.code == failure.code && st.mqtt.state == MqttState::Error)
                .map_or_else(mqtt::now_ms, |e| e.at_ms);
            st.mqtt.state = MqttState::Error;
            st.mqtt.connected_since_ms = None;
            st.mqtt.quiet_since_ms = None;
            st.mqtt.last_error = Some(err(failure, at));
        }
        self.tell_trouble();
    }
}

// ---------------------------------------------------------------- the database side's timer

/// Adds the database's cars not known yet, and their VIN prefixes and names where missing. Whether it changed any.
fn merge_cars(store: &Store, seen: &[catchup::DbCar]) -> bool {
    let Ok(c) = store.get() else { return false };
    let new_info = seen.iter().any(|s| match c.car(&s.id.to_string()) {
        None => c.cars.len() < MAX_CARS,
        Some(k) => {
            (s.vin_prefix.is_some() && k.vin_prefix != s.vin_prefix) || (s.name.is_some() && k.display_name.is_none())
        }
    });
    if !new_info {
        return false;
    }
    let r = store.update(None, |cfg| {
        for s in seen {
            let id = s.id.to_string();
            let prefix = s.vin_prefix.as_ref().map(|p| p.chars().take(11).collect::<String>());
            let full = cfg.cars.len() >= MAX_CARS;
            match cfg.cars.iter_mut().find(|k| k.teslamate_car == id) {
                Some(k) => {
                    if prefix.is_some() {
                        k.vin_prefix = prefix;
                    }
                    if k.display_name.is_none() {
                        k.display_name = s.name.clone();
                    }
                }
                None if !full => cfg.cars.push(LocalCar {
                    teslamate_car: id,
                    display_name: s.name.clone(),
                    vin_prefix: prefix,
                    matched: None,
                }),
                None => {}
            }
        }
    });
    match r {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(error = %e, "the local TeslaMate settings could not be saved");
            false
        }
    }
}

/// One catch-up: the database's cars (for the match table), then, for the matched cars, a dump from each car's
/// mark, stored as a TeslaMate import; the marks move on only once the sink stored it. The dump file is deleted
/// whatever happened.
async fn sync_once<S: Sink>(
    store: &Store,
    sink: &Arc<S>,
    status: &Mutex<Status>,
    c: &Config,
) -> Result<catchup::Counts, Failure> {
    let cfg = catchup_config(&c.database, store.db_password().ok().flatten());
    let seen = catchup::test(&cfg).await.map_err(|e| db_failure(&e))?;
    if merge_cars(store, &seen) {
        let st = lock(status).clone();
        sink.changed(&st);
    }
    let c = store.get().map_err(|e| Failure { code: "query_failed", message: e.to_string() })?;
    let pairs: Vec<(i64, CarId)> =
        c.cars.iter().filter_map(|k| Some((k.teslamate_car.parse::<i64>().ok()?, match_car(k.matched)?))).collect();
    if pairs.is_empty() {
        return Ok(catchup::Counts::default());
    }
    let ids: Vec<i64> = pairs.iter().map(|(i, _)| *i).collect();
    let cursor = store.cursor().unwrap_or_default();
    // a dump file of its own: a catch-up whose task was stopped (the settings saved meanwhile) may still be storing
    // its dump, and a new dump on the same path would cut that file under it
    static DUMPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = DUMPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base = store.dump_path();
    let tmp = base.with_file_name(format!(
        "{}.{n}",
        base.file_name().map_or_else(|| "catchup.dump".into(), |f| f.to_string_lossy().into_owned())
    ));
    let r = async {
        let d = catchup::dump(&cfg, &cursor, &ids, &tmp).await.map_err(|e| db_failure(&e))?;
        let (sink2, tmp2, pairs2) = (sink.clone(), tmp.clone(), pairs.clone());
        let counts = tokio::task::spawn_blocking(move || {
            // one import at a time (stopping the task does not stop the import it waits for); the file goes with
            // the import, as the task that waited for it may be gone
            static IMPORTING: Mutex<()> = Mutex::new(());
            let _one = lock(&IMPORTING);
            let r = sink2.catch_up(&tmp2, &pairs2);
            let _ = std::fs::remove_file(&tmp2);
            r
        })
        .await
        .map_err(|e| Failure { code: "query_failed", message: e.to_string() })?
        .map_err(|e| Failure { code: "query_failed", message: format!("storing the catch-up: {e}") })?;
        store.set_cursor(&d.cursor).map_err(|e| Failure { code: "query_failed", message: e.to_string() })?;
        Ok(counts)
    }
    .await;
    let _ = std::fs::remove_file(&tmp);
    r
}

async fn db_loop<S: Sink>(store: Store, sink: Arc<S>, status: Arc<Mutex<Status>>) {
    // the database trouble last told to the sink
    let mut told: Told = None;
    loop {
        let Ok(c) = store.get() else { return };
        if c.paused || !c.database.enabled {
            return;
        }
        lock(&status).database.state = DbState::Syncing;
        let r = sync_once(&store, &sink, &status, &c).await;
        let now = mqtt::now_ms();
        let next = now + c.database.interval_s.max(60) * 1000;
        {
            let mut st = lock(&status);
            st.database.next_sync_ms = Some(next);
            match &r {
                Ok(counts) => {
                    st.database.state = DbState::Ok;
                    st.database.last_sync_ms = Some(now);
                    st.database.drives += counts.drives;
                    st.database.charges += counts.charges;
                    st.database.positions += counts.positions;
                    st.database.last_error = None;
                }
                Err(f) => {
                    tracing::warn!(code = f.code, "local TeslaMate: the database catch-up failed: {}", f.message);
                    // failing the same way again: since its first failure
                    let at = st.database.last_error.as_ref().filter(|e| e.code == f.code).map_or(now, |e| e.at_ms);
                    st.database.state = DbState::Error;
                    st.database.last_error = Some(err(f, at));
                }
            }
        }
        let st = lock(&status).clone();
        let now_told = told_of(st.db_problem().as_ref());
        if now_told != told {
            told = now_told;
            sink.changed(&st);
        }
        tokio::time::sleep(Duration::from_millis(next.saturating_sub(mqtt::now_ms()))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> Draft {
        Draft { mqtt: MqttConfig { host: "192.0.2.10".into(), ..MqttConfig::default() }, ..Draft::default() }
    }

    fn codes(r: Result<(), Vec<Invalid>>) -> Vec<(String, &'static str)> {
        r.err().unwrap_or_default().into_iter().map(|i| (i.field, i.code)).collect()
    }

    #[test]
    fn a_password_over_plain_tcp_needs_the_tick() {
        assert!(validate(&draft(), None).is_ok(), "plain TCP without a password is fine");
        let mut d = draft();
        d.mqtt.username = Some("reader".into());
        d.mqtt_password = Password::Set(Secret::new("made-up-password"));
        assert_eq!(codes(validate(&d, None)), vec![("mqtt.password".to_string(), "plaintext_password")]);
        d.mqtt.allow_unencrypted = true;
        assert!(validate(&d, None).is_ok());
        d.mqtt.allow_unencrypted = false;
        d.mqtt.tls = true;
        assert!(validate(&d, None).is_ok());
        // a kept password counts too
        let saved = Config {
            version: 1,
            added_ms: 0,
            paused: false,
            mqtt: MqttConfig { host: "192.0.2.10".into(), password_set: true, tls: true, ..MqttConfig::default() },
            database: DbConfig::default(),
            cars: Vec::new(),
        };
        let mut k = draft();
        k.mqtt.tls = true;
        assert!(validate(&k, Some(&saved)).is_ok());
        k.mqtt.tls = false;
        let c = codes(validate(&k, Some(&saved)));
        assert!(c.contains(&("mqtt.password".to_string(), "password_resend")), "{c:?}");
        assert!(c.contains(&("mqtt.password".to_string(), "plaintext_password")), "{c:?}");
        // the database the same way, when on
        let mut d = draft();
        d.database = DbConfig { enabled: true, host: "192.0.2.10".into(), ..DbConfig::default() };
        d.db_password = Password::Set(Secret::new("made-up-password"));
        assert_eq!(codes(validate(&d, None)), vec![("database.password".to_string(), "plaintext_password")]);
    }

    #[test]
    fn a_kept_database_password_never_follows_a_host_changed_while_the_database_is_off() {
        let db = |enabled: bool, host: &str, port: u16, tls: bool| DbConfig {
            enabled,
            host: host.into(),
            port,
            tls,
            ..DbConfig::default()
        };
        let port = DbConfig::default().port;
        let saved = Config {
            version: 1,
            added_ms: 0,
            paused: false,
            mqtt: MqttConfig { host: "192.0.2.10".into(), ..MqttConfig::default() },
            database: DbConfig { password_set: true, ..db(true, "192.0.2.10", port, true) },
            cars: Vec::new(),
        };
        let with = |database: DbConfig, db_password: Password| Draft { database, db_password, ..draft() };
        let resend = vec![("database.password".to_string(), "password_resend")];
        // on or off: another host, port or TLS with the password kept is refused (else it would be saved as the
        // host the password belongs to, and go there once the database is on again)
        for enabled in [true, false] {
            for moved in [db(enabled, "192.0.2.66", port, true), db(enabled, "192.0.2.10", port + 1, true)] {
                assert_eq!(codes(validate(&with(moved, Password::Keep), Some(&saved))), resend, "{enabled}");
            }
            let c = codes(validate(&with(db(enabled, "192.0.2.10", port, false), Password::Keep), Some(&saved)));
            assert!(c.contains(&resend[0]), "{enabled}: {c:?}");
            // the same place, the password removed, or typed again: fine
            assert!(validate(&with(db(enabled, "192.0.2.10", port, true), Password::Keep), Some(&saved)).is_ok());
            assert!(validate(&with(db(enabled, "192.0.2.66", port, true), Password::Remove), Some(&saved)).is_ok());
            let typed = Password::Set(Secret::new("made-up-password"));
            assert!(validate(&with(db(enabled, "192.0.2.66", port, true), typed), Some(&saved)).is_ok());
        }
        // no password saved: an unused database's host is anything
        let mut bare = saved.clone();
        bare.database.password_set = false;
        assert!(validate(&with(db(false, "192.0.2.66", port, true), Password::Keep), Some(&bare)).is_ok());
    }

    #[test]
    fn hosts_prefixes_pins() {
        for h in ["192.0.2.10", "teslamate.lan", "2001:db8::1", "pi"] {
            assert!(host_ok(h), "{h}");
        }
        for h in ["", "mqtt://192.0.2.10", "192.0.2.10:1883", "a b", "x/y"] {
            assert!(!host_ok(h), "{h}");
        }
        assert!(prefix_ok("teslamate") && prefix_ok("home/teslamate"));
        for p in ["", "a/+", "#", "$SYS", "a//b", "/a", "a/"] {
            assert!(!prefix_ok(p), "{p}");
        }
        let mut d = draft();
        d.mqtt.pin_sha256 = Some(vec!["AB"; 32].join(":"));
        normalize(&mut d);
        assert_eq!(d.mqtt.pin_sha256.as_deref(), Some("ab".repeat(32).as_str()));
        assert_eq!(codes(validate(&d, None)), vec![("mqtt.pin_sha256".to_string(), "invalid")], "a pin needs TLS");
    }

    #[test]
    fn suggestions() {
        let cars = [
            Candidate { car: 3, name: "Made-up Car", vin_prefix: Some("5YJ3E1EA0PF") },
            Candidate { car: 4, name: "Other", vin_prefix: None },
        ];
        let mut c = LocalCar { teslamate_car: "1".into(), display_name: None, vin_prefix: None, matched: None };
        assert_eq!(suggest(&c, &cars), None);
        c.display_name = Some("made-up car".into());
        assert_eq!(suggest(&c, &cars), Some(3));
        c.display_name = Some("Other".into());
        c.vin_prefix = Some("5YJ3E1EA0P".into());
        assert_eq!(suggest(&c, &cars), Some(3), "the VIN prefix first");
    }
}
