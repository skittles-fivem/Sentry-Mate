//! Integrations: `/settings/mqtt` (GET, PUT), `/settings/mqtt/status`, `/settings/mqtt/preview`,
//! `/settings/mqtt/test` and `/network` (docs/ui-api.md, "Integrations"), and the publisher that sends the cars'
//! newest values to the broker ([`spawn`]).
//!
//! - **Stored** in the history's database (sealed with the vault) under the `mqtt` setting, as the mqtt crate's
//!   [`mqtt::Settings`] plus what only the pages use (Home Assistant's address, the place text for "at no saved
//!   place", the version). `/cars/{car}/events` renders with the same setting, so its payloads are what the broker
//!   gets. The password is write-only: GET answers `password: null` and `password_set`.
//! - **The rules** are the mqtt crate's, SMO's (`MqttSecurity.kt`): TLS unless the owner turns it off (`tls: false`
//!   is the owner allowing plain MQTT), a pin only with TLS, and never a password without TLS. A PUT that breaks one
//!   is `422 invalid` on the field to change.
//! - **Publishing** ([`spawn`], started by `serve`): while MQTT is on, one connection publishes every car's newest
//!   values as the engine records them (and once for every car at start), and each drive and charge starting and
//!   ending on the event topic, once, as the engine records it ([`engine::query::published`]: those of the last
//!   [`RECENT_MS`], so a late upload still gives its events, a restart or a rebuild of old days none again). A saved
//!   change restarts it with the new settings; when that turns Home Assistant discovery off or changes a prefix, what
//!   the old layout left on the broker (discovery configs, retained values) is cleared first ([`mqtt::cleanup`]). The
//!   engine reports no car going away yet (no car can be deleted), so nothing is cleared for one. Its counters answer
//!   `/status`: `last_error` is always a code (`connection_refused`, `auth_failed`, `tls_failed`, `pin_mismatch`,
//!   `timeout`, `invalid_settings`, `no_host`), `last_error_message` the words.
//! - **The test** connects with the saved settings, publishes one message on `<prefix>/test` and disconnects; it
//!   never touches the running connection.

use super::settings::{self as server, CHANGES};
use crate::ctx::target;
use crate::units::{self, Units};
use crate::{ApiError, ApiResult, AppState, Fields, JsonBody, Params, Viewer, ids};
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use engine::Engine;
use model::CarId;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

/// The database setting the MQTT settings are kept under (the engine's `/cars/{car}/events` reads it too).
pub const KEY: &str = "mqtt";
/// The longest text published for a car at no saved place.
pub const MAX_NO_PLACE_TEXT: usize = 40;
/// How often the publisher's counters are looked at (for `last_used_ms` and `last_error_ms`).
const TICK: Duration = Duration::from_secs(5);
/// How far back drive and charge events are published: a day and a half covers a car away from the server's network
/// all day (its logs upload when it is back). Older ones are never sent, so a rebuild of old days sends nothing.
pub const RECENT_MS: u64 = 36 * 3_600_000;
/// How long changed cars wait for the engine to be idle before their events go out anyway (an engine kept busy by
/// many cars' uploads).
const EVENTS_WAIT: Duration = Duration::from_secs(60);
/// Event keys remembered per car (twice the most one look gives), so none is sent twice.
const SENT_PER_CAR: usize = 2 * engine::query::published::MAX_EVENTS;

/// The MQTT setting as stored: the crate's settings, and what only the pages use.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Stored {
    #[serde(flatten)]
    pub settings: mqtt::Settings,
    /// Home Assistant's own address (`https://`), for links from the pages.
    pub ha_url: Option<String>,
    /// Published as the place of a car at no saved place (empty: an empty place).
    pub no_place_text: String,
    pub version: u64,
}

impl Default for Stored {
    fn default() -> Stored {
        Stored { settings: mqtt::Settings::default(), ha_url: None, no_place_text: String::new(), version: 1 }
    }
}

/// The publisher's side the handlers see (handed over with `Builder::service`; without it, settings are saved but
/// nothing publishes).
#[derive(Default)]
pub struct Publisher {
    /// The running publisher (the task in [`spawn`] owns it: when that task ends, it goes).
    live: Mutex<Weak<mqtt::Mqtt>>,
    restart: tokio::sync::Notify,
    log: Mutex<Log>,
}

#[derive(Default, Clone)]
struct Log {
    /// A code (as `/test` answers them), with the words in `last_error_message`.
    last_error: Option<String>,
    last_error_message: Option<String>,
    last_error_ms: Option<u64>,
    last_used_ms: Option<u64>,
    delivered: u64,
    seen_error: Option<String>,
}

impl Publisher {
    pub fn new() -> Publisher {
        Publisher::default()
    }

    fn running(&self) -> Option<Arc<mqtt::Mqtt>> {
        self.live.lock().unwrap_or_else(|p| p.into_inner()).upgrade()
    }

    fn log(&self) -> std::sync::MutexGuard<'_, Log> {
        self.log.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn error(&self, code: &str, message: String, at_ms: u64) {
        let mut l = self.log();
        l.last_error = Some(code.to_string());
        l.last_error_message = Some(message);
        l.last_error_ms = Some(at_ms);
    }

    /// Looks at the running publisher's counters: a new delivery is a use of the connection, a new error is said
    /// with the time it was first seen.
    fn observe(&self, now_ms: u64) {
        let Some(m) = self.running() else { return };
        let s = m.stats();
        let mut l = self.log();
        if s.delivered != l.delivered {
            l.delivered = s.delivered;
            l.last_used_ms = Some(now_ms);
        }
        if s.last_error != l.seen_error {
            if let Some(e) = &s.last_error {
                l.last_error = Some(s.last_error_code.unwrap_or("connection_refused").to_string());
                l.last_error_message = Some(e.clone());
                l.last_error_ms = Some(now_ms);
            }
            l.seen_error = s.last_error.clone();
        }
    }
}

/// What the publisher is doing, for the connections overview ([`super::overview`]).
pub(crate) struct PublisherView {
    /// A publisher runs (none: MQTT off, or its settings could not start one).
    pub running: bool,
    pub connected: bool,
    /// Messages the broker acknowledged since this publisher started.
    pub delivered: u64,
    /// The last error's code, words and when it was first seen.
    pub last_error: Option<(String, String, u64)>,
    pub last_used_ms: Option<u64>,
}

impl Publisher {
    /// Looks at the counters (as `/status` does) and says what the publisher is doing.
    pub(crate) fn view(&self, now_ms: u64) -> PublisherView {
        self.observe(now_ms);
        let stats = self.running().map(|m| m.stats());
        let l = self.log().clone();
        let last_error = match (l.last_error, l.last_error_ms) {
            (Some(code), Some(at)) => Some((code, l.last_error_message.unwrap_or_default(), at)),
            _ => None,
        };
        PublisherView {
            running: stats.is_some(),
            connected: stats.as_ref().is_some_and(|x| x.connected),
            delivered: stats.as_ref().map_or(0, |x| x.delivered),
            last_error,
            last_used_ms: l.last_used_ms,
        }
    }
}

/// The ports the server listens on, for `/network` (handed over with `Builder::service`).
#[derive(Debug, Clone, Copy)]
pub struct Network {
    pub web_port: u16,
    pub upload_port: u16,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/settings/mqtt", get(read).put(replace))
        .route("/api/v1/settings/mqtt/status", get(status))
        .route("/api/v1/settings/mqtt/preview", get(preview))
        .route("/api/v1/settings/mqtt/test", post(test))
        .route("/api/v1/network", get(network))
}

// ---- stored

fn db_err<E: std::fmt::Display>(what: &str) -> impl Fn(E) -> ApiError + '_ {
    move |e| ApiError::server(format!("{what}: {e}"))
}

/// The saved MQTT setting (the defaults until a first save).
pub fn load(e: &Engine) -> ApiResult<Stored> {
    let saved = e.db().setting(KEY).map_err(db_err("reading the MQTT settings"))?;
    Ok(match saved {
        Some(v) => serde_json::from_value(v).unwrap_or_else(|err| {
            tracing::error!("the MQTT settings can't be read ({err}); MQTT stays off until they are saved again");
            Stored::default()
        }),
        None => Stored::default(),
    })
}

fn save(e: &Engine, s: &Stored) -> ApiResult<()> {
    e.db().set_setting(KEY, serde_json::to_value(s)?).map_err(db_err("saving the MQTT settings"))
}

/// The server's units as MQTT's.
pub fn mqtt_units(u: Units) -> mqtt::Units {
    mqtt::Units {
        distance: match u.distance {
            units::Distance::Km => mqtt::DistanceUnit::Km,
            units::Distance::Mi => mqtt::DistanceUnit::Mi,
        },
        temperature: match u.temperature {
            units::Temperature::C => mqtt::TemperatureUnit::C,
            units::Temperature::F => mqtt::TemperatureUnit::F,
        },
        pressure: match u.pressure {
            units::Pressure::Bar => mqtt::PressureUnit::Bar,
            units::Pressure::Psi => mqtt::PressureUnit::Psi,
        },
    }
}

/// The server's units changed (`/settings`): MQTT follows them (called under [`CHANGES`]).
pub(crate) fn follow_units(app: &AppState, e: &Engine, u: Units) -> ApiResult<()> {
    let mut s = load(e)?;
    let mu = mqtt_units(u);
    if s.settings.units != mu {
        s.settings.units = mu;
        save(e, &s)?;
        if let Ok(p) = app.service::<Publisher>() {
            p.restart.notify_one();
        }
    }
    Ok(())
}

/// The broker of the setting as the pages show it: host, port, TLS.
fn broker_parts(s: &mqtt::Settings) -> (String, u16, bool) {
    match mqtt::settings::parse_broker(&s.broker, true) {
        Ok(b) => (b.host, b.port, b.tls),
        Err(_) => (String::new(), if s.allow_unencrypted { 1883 } else { 8883 }, !s.allow_unencrypted),
    }
}

fn broker_url(host: &str, port: u16, tls: bool) -> String {
    if host.is_empty() {
        return String::new();
    }
    let scheme = if tls { "mqtts" } else { "mqtt" };
    if host.contains(':') { format!("{scheme}://[{host}]:{port}") } else { format!("{scheme}://{host}:{port}") }
}

fn view(s: &Stored) -> Value {
    let m = &s.settings;
    let (host, port, tls) = broker_parts(m);
    json!({
        "enabled": m.enabled,
        "host": host,
        "port": port,
        "username": m.username,
        "password": null,
        "password_set": m.password.as_deref().is_some_and(|p| !p.is_empty()),
        "tls": tls,
        "pin_sha256": m.cert_sha256,
        "namespace": m.prefix,
        "publish_location": m.publish_location,
        "ha_discovery": { "enabled": m.discovery, "prefix": m.discovery_prefix, "url": s.ha_url },
        "no_place_text": s.no_place_text,
        "version": s.version,
    })
}

async fn read(State(app): State<AppState>, _v: Viewer) -> ApiResult<Json<Value>> {
    let e = app.service::<Engine>()?;
    let s = app.blocking(move |_| load(&e)).await?;
    Ok(Json(view(&s)))
}

// ---- PUT

const FIELDS: &[&str] = &[
    "enabled",
    "host",
    "port",
    "username",
    "password",
    "tls",
    "pin_sha256",
    "namespace",
    "publish_location",
    "ha_discovery",
    "no_place_text",
];

/// `400` on the field when it is missing or not of its type.
fn req<'a>(body: &'a JsonBody, k: &str, bad: &mut Fields) -> Option<&'a Value> {
    let v = body.get(k);
    if v.is_none() {
        bad.add(k, "Required (PUT sends the whole setting; leave out only password to keep it).");
    }
    v
}

fn req_bool(body: &JsonBody, k: &str, bad: &mut Fields) -> bool {
    match req(body, k, bad) {
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            bad.add(k, "Must be true or false.");
            false
        }
        None => false,
    }
}

fn req_text(body: &JsonBody, k: &str, bad: &mut Fields) -> String {
    match req(body, k, bad) {
        Some(Value::String(s)) => s.clone(),
        Some(_) => {
            bad.add(k, "Must be text.");
            String::new()
        }
        None => String::new(),
    }
}

/// Text or null; empty text counts as null.
fn req_opt_text(body: &JsonBody, k: &str, bad: &mut Fields) -> Option<String> {
    match req(body, k, bad) {
        Some(Value::String(s)) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Some(Value::Null) | None => None,
        Some(_) => {
            bad.add(k, "Must be text or null.");
            None
        }
    }
}

/// A broker host: a DNS name, an IPv4 address or an IPv6 address (without brackets).
fn host_ok(h: &str) -> bool {
    h.parse::<std::net::Ipv6Addr>().is_ok() || server::is_host_name(h)
}

/// Which field an mqtt crate rule is about.
fn field_of(e: &mqtt::Error) -> &'static str {
    use mqtt::Error as E;
    match e {
        E::NoScheme | E::UnsupportedScheme(_) | E::CredentialsInUrl | E::BadAddress(_) => "host",
        E::Unencrypted(_) | E::PasswordUnencrypted => "tls",
        E::PasswordWithoutUser | E::BadPassword => "password",
        E::BadUsername => "username",
        E::BadPin | E::PinWithoutTls => "pin_sha256",
        E::BadPrefix(_) => "namespace",
        E::BadClientId | E::BadLimit(_) | E::Tls(_) | E::NoRuntime => "body",
    }
}

/// Whether `host`, `port`, `tls` and `pin` name the broker the saved setting connects to (the same certificate pin, or
/// none on both).
fn same_broker(saved: &mqtt::Settings, host: &str, port: u16, tls: bool, pin: Option<&str>) -> bool {
    let Ok(old) = mqtt::settings::parse_broker(&saved.broker, true) else { return false };
    let norm = |p: Option<&str>| p.map(mqtt::security::normalize_fingerprint);
    old.host.eq_ignore_ascii_case(host)
        && old.port == port
        && old.tls == tls
        && norm(saved.cert_sha256.as_deref()) == norm(pin)
}

/// The new setting a PUT gives, every rule checked.
fn replaced(cur: &Stored, body: &JsonBody) -> ApiResult<Stored> {
    let mut known: Vec<&str> = FIELDS.to_vec();
    known.push("version");
    body.only(&known)?;
    let mut bad = Fields::new();
    let enabled = req_bool(body, "enabled", &mut bad);
    let tls = req_bool(body, "tls", &mut bad);
    let publish_location = req_bool(body, "publish_location", &mut bad);
    let host = req_text(body, "host", &mut bad).trim().to_string();
    let namespace = req_text(body, "namespace", &mut bad).trim().to_string();
    let no_place_text = req_text(body, "no_place_text", &mut bad);
    let username = req_opt_text(body, "username", &mut bad);
    let pin = req_opt_text(body, "pin_sha256", &mut bad);
    let port = match req(body, "port", &mut bad) {
        Some(v) => match v.as_u64() {
            Some(n) => Some(n),
            None => {
                bad.add("port", "Must be a whole number.");
                None
            }
        },
        None => None,
    };
    // absent: kept; null or empty: removed
    let password: Option<Option<String>> = match body.get("password") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(s)) => Some(Some(s.clone()).filter(|s| !s.is_empty())),
        Some(_) => {
            bad.add("password", "Must be text or null.");
            None
        }
    };
    let (mut ha_enabled, mut ha_prefix, mut ha_url) = (false, String::new(), None);
    match req(body, "ha_discovery", &mut bad) {
        Some(Value::Object(h)) => {
            for k in h.keys() {
                if !["enabled", "prefix", "url"].contains(&k.as_str()) {
                    bad.add(&format!("ha_discovery.{k}"), "Unknown field.");
                }
            }
            match h.get("enabled") {
                Some(Value::Bool(b)) => ha_enabled = *b,
                _ => bad.add("ha_discovery.enabled", "Must be true or false."),
            }
            match h.get("prefix") {
                Some(Value::String(s)) => ha_prefix = s.trim().to_string(),
                _ => bad.add("ha_discovery.prefix", "Must be text."),
            }
            match h.get("url") {
                None | Some(Value::Null) => {}
                Some(Value::String(s)) => ha_url = Some(s.trim().to_string()).filter(|s| !s.is_empty()),
                Some(_) => bad.add("ha_discovery.url", "Must be text or null."),
            }
        }
        Some(_) => bad.add("ha_discovery", "Must be { enabled, prefix, url }."),
        None => {}
    }
    bad.bad_input()?;
    let mut rule = Fields::new();
    if enabled && host.is_empty() {
        rule.add("host", "Publishing is on, so the broker needs a host, such as broker.local.");
    } else if !host.is_empty() && !host_ok(&host) {
        rule.add("host", "A host name such as broker.local, or an IP address.");
    }
    let port = match port {
        Some(p @ 1..=65535) => u16::try_from(p).unwrap_or(8883),
        _ => {
            rule.add("port", "Must be between 1 and 65535.");
            8883
        }
    };
    if let Some(p) = &pin {
        let digits: String = p.chars().filter(|c| *c != ':').collect();
        if digits.len() != 64 || mqtt::security::normalize_fingerprint(p).is_none() {
            rule.add("pin_sha256", "The broker certificate's SHA-256: 64 hex digits, with or without colons.");
        } else if !tls {
            rule.add("pin_sha256", "A certificate pin needs TLS.");
        }
    }
    for (k, p) in [("namespace", &namespace), ("ha_discovery.prefix", &ha_prefix)] {
        if let Err(e) = mqtt::topics::check_prefix(p) {
            let why = if p.is_empty() {
                "Must not be empty.".to_string()
            } else {
                format!("No + or #, not starting with $, no empty level ({e}).")
            };
            rule.add(k, &why);
        }
    }
    if let Some(u) = &ha_url
        && server::origin_of(u).is_none()
    {
        rule.add("ha_discovery.url", "An https:// address, or null.");
    }
    if no_place_text.chars().count() > MAX_NO_PLACE_TEXT || no_place_text.chars().any(char::is_control) {
        rule.add("no_place_text", &format!("At most {MAX_NO_PLACE_TEXT} characters, no control characters."));
    }
    if let Some(u) = &username
        && (u.chars().count() > 128 || u.chars().any(char::is_control))
    {
        rule.add("username", "1 to 128 characters without control characters, or null.");
    }
    let password = match password {
        Some(p) => p,
        None => {
            let kept = cur.settings.password.clone().filter(|p| !p.is_empty());
            // the saved password goes only to the broker it was typed for: a PUT that points it at another host,
            // port, TLS setting or certificate pin must send it again (else an admin session could carry the owner's
            // broker login, which no answer shows, to a broker of its own with the next test or publish)
            if kept.is_some() && !same_broker(&cur.settings, &host, port, tls, pin.as_deref()) {
                rule.add("password", "The broker changed: send its password again (or null for none).");
            }
            kept
        }
    };
    if let Some(p) = &password {
        if !tls {
            rule.add("tls", "A password is only ever sent over TLS. Turn TLS on, or remove the password.");
        } else if username.is_none() {
            rule.add("password", "A password needs a user name.");
        } else if p.len() > usize::from(u16::MAX) {
            rule.add("password", "Longer than MQTT allows (65,535 bytes).");
        }
    }
    rule.invalid()?;
    let mut settings = cur.settings.clone();
    settings.enabled = enabled;
    settings.broker = broker_url(&host, port, tls);
    settings.allow_unencrypted = !tls;
    settings.cert_sha256 = pin;
    settings.username = username;
    settings.password = password;
    settings.prefix = namespace;
    settings.publish_location = publish_location;
    settings.discovery = ha_enabled;
    settings.discovery_prefix = ha_prefix;
    // the crate's own check of the whole, so a saved setting always starts (only when there is a broker to check)
    if !host.is_empty()
        && let Err(e) = settings.check()
    {
        return Err(ApiError::invalid(field_of(&e), &e.to_string()));
    }
    Ok(Stored { settings, ha_url, no_place_text, version: cur.version })
}

async fn replace(State(app): State<AppState>, v: Viewer, body: JsonBody) -> ApiResult<Json<Value>> {
    let e = app.service::<Engine>()?;
    let s = app
        .blocking(move |app| {
            let _one = CHANGES.lock().unwrap_or_else(|p| p.into_inner());
            let cur = load(&e)?;
            if body.version()? != cur.version {
                return Err(ApiError::stale_version(cur.version));
            }
            let mut new = replaced(&cur, &body)?;
            // values go out in the server's units
            let units = server::load(&e, &[])?.units;
            new.settings.units = mqtt_units(units);
            new.version = cur.version + 1;
            save(&e, &new)?;
            if let Ok(p) = app.service::<Publisher>() {
                p.restart.notify_one();
            }
            Ok(new)
        })
        .await?;
    let (host, port, _) = broker_parts(&s.settings);
    let what = if s.settings.enabled { "on" } else { "off" };
    let broker = if host.is_empty() { "none".to_string() } else { format!("{host}:{port}") };
    let summary = format!("Changed the MQTT settings (publishing {what}, broker {broker}).");
    v.audit(&app, "mqtt-changed", Some(&target("settings", None, "mqtt")), &summary).await?;
    Ok(Json(view(&s)))
}

// ---- status

async fn status(State(app): State<AppState>, _v: Viewer) -> ApiResult<Json<Value>> {
    let e = app.service::<Engine>()?;
    let s = app.blocking(move |_| load(&e)).await?;
    let (host, port, tls) = broker_parts(&s.settings);
    let p = app.service::<Publisher>().ok();
    if let Some(p) = &p {
        p.observe(app.now());
    }
    let stats = p.as_ref().and_then(|p| p.running()).map(|m| m.stats());
    let log = p.as_ref().map(|p| p.log().clone()).unwrap_or_default();
    Ok(Json(json!({
        "enabled": s.settings.enabled,
        "connected": stats.as_ref().is_some_and(|x| x.connected),
        "delivered": stats.as_ref().map_or(0, |x| x.delivered),
        "waiting": stats.as_ref().map_or(0, |x| x.states_waiting + x.events_waiting),
        "dropped": stats.as_ref().map_or(0, |x| x.states_dropped + x.events_dropped),
        "last_error": log.last_error,
        "last_error_message": log.last_error_message,
        "last_error_ms": log.last_error_ms,
        "broker": (!host.is_empty()).then(|| if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") }),
        "tls": tls,
    })))
}

// ---- preview

/// The first car of the account in the server's order (then by id).
fn first_car(app: &AppState, e: &Engine, v: &Viewer) -> ApiResult<Option<CarId>> {
    let mut cars: Vec<CarId> =
        engine::query::all_cars(e).map_err(super::now::failed)?.into_iter().filter(|c| v.sees(*c)).collect();
    let order = app.defaults().car_order;
    let rank = |c: &CarId| order.iter().position(|o| *o == ids::car(*c)).unwrap_or(usize::MAX);
    cars.sort_by_key(|c| (rank(c), *c));
    Ok(cars.first().copied())
}

/// The place a car is published at: its saved place's name, else the setting's text.
fn place_name(e: &Engine, now: &model::Now, no_place: &str) -> Option<String> {
    let at = match (now.sample.lat, now.sample.lon) {
        (Some(la), Some(lo)) => e.places().place_at(la, lo).map(|p| p.name.clone()),
        _ => None,
    };
    at.or_else(|| (!no_place.is_empty()).then(|| no_place.to_string()))
}

/// The car's newest values as MQTT publishes them: with Sentry Mate's range estimate and the Data health verdict.
fn car_now(e: &Engine, car: CarId) -> ApiResult<Option<model::Now>> {
    match engine::query::now::published_now(e, car) {
        Err(engine::query::QueryError::NoSuchCar) => Ok(None),
        r => r.map_err(db_err("reading a car")),
    }
}

fn car_info(e: &Engine, car: CarId) -> ApiResult<Option<mqtt::CarInfo>> {
    Ok(e.db().car(car).map_err(db_err("reading a car"))?.map(|c| mqtt::CarInfo::of(&c)))
}

fn units_json(u: mqtt::Units) -> Value {
    json!({
        "distance": u.distance_unit(),
        "temperature": if u.temperature == mqtt::TemperatureUnit::F { "f" } else { "c" },
        "pressure": u.pressure_unit(),
    })
}

fn messages_json(msgs: &[mqtt::Message]) -> Vec<Value> {
    msgs.iter().map(|m| json!({ "topic": m.topic, "payload": m.payload, "retained": m.retain })).collect()
}

async fn preview(State(app): State<AppState>, v: Viewer, p: Params) -> ApiResult<Json<Value>> {
    let e = app.service::<Engine>()?;
    // `car=` was checked by the guard (an unknown or hidden car is `404 no_such_car`)
    let named = match p.get("car") {
        Some(s) => Some(ids::parse_car(s).ok_or_else(ApiError::no_such_car)?),
        None => None,
    };
    let value = app
        .blocking(move |app| {
            let s = load(&e)?;
            let mut settings = s.settings.clone();
            settings.units = mqtt_units(app.defaults().units);
            let units = units_json(settings.units);
            let car = match named {
                Some(c) => Some(c),
                None => first_car(app, &e, &v)?,
            };
            let Some(car) = car else {
                return Ok(json!({ "car": null, "name": null, "units": units, "device": null, "messages": [], "ha_entities": [], "inputs": [], "sources": {} }));
            };
            let info = car_info(&e, car)?.ok_or_else(ApiError::no_such_car)?;
            let layout = mqtt::Layout::new(&settings).map_err(|err| ApiError::server(format!("the MQTT layout: {err}")))?;
            let mut msgs = vec![mqtt::Message { topic: layout.topics.status(), payload: "online".into(), retain: true }];
            // where each value no CAN message carries came from
            let mut sources = Map::new();
            if let Some(now) = car_now(&e, car)? {
                let place = place_name(&e, &now, &s.no_place_text);
                // (the stored Now names its own car; topics are this car's)
                let now = model::Now { car, ..now };
                msgs.extend(mqtt::render::now_messages(&layout, &now, place.as_deref()));
                for f in mqtt::render::Field::ALL {
                    if let Some((input, at_ms, stale)) = mqtt::render::field_source(f, &now) {
                        let r = engine::query::SourceRef::from(model::Origin::from(input));
                        sources.insert(
                            f.key().into(),
                            json!({ "kind": r.kind, "label": r.label, "at_ms": at_ms, "stale": stale }),
                        );
                    }
                }
            }
            // the live inputs the car has data from (an older car that names none: SMO, when it has CAN keys)
            let inputs: Vec<Value> = e
                .db()
                .car(car)
                .map_err(db_err("reading a car"))?
                .map(|c| {
                    let mut i = c.inputs;
                    if i.is_empty() && !c.vehicle_keys.is_empty() {
                        i.insert(model::Input::Smo);
                    }
                    i.iter()
                        .map(|i| {
                            let r = engine::query::SourceRef::from(model::Origin::from(i));
                            json!({ "kind": r.kind, "label": r.label })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut entities = Vec::new();
            let mut device = Value::Null;
            if settings.discovery {
                for m in mqtt::discovery::configs(&layout, car, &info) {
                    let Ok(Value::Object(c)) = serde_json::from_str::<Value>(&m.payload) else { continue };
                    if device.is_null() {
                        device = c.get("device").cloned().unwrap_or(Value::Null);
                    }
                    let mut ent = Map::new();
                    ent.insert("id".into(), c.get("unique_id").cloned().unwrap_or(Value::Null));
                    ent.insert("name".into(), c.get("name").cloned().unwrap_or(Value::Null));
                    ent.insert("unit".into(), c.get("unit_of_measurement").cloned().unwrap_or(Value::Null));
                    ent.insert("device_class".into(), c.get("device_class").cloned().unwrap_or(Value::Null));
                    let state = c.get("state_topic").or_else(|| c.get("json_attributes_topic")).cloned();
                    ent.insert("state_topic".into(), state.unwrap_or(Value::Null));
                    ent.insert("config_topic".into(), m.topic.clone().into());
                    if let Some(t) = c.get("event_types") {
                        ent.insert("event_types".into(), t.clone());
                    }
                    entities.push(Value::Object(ent));
                }
            }
            if device.is_null() {
                device = json!({ "name": info.name, "model": info.model, "identifiers": [layout.topics.node_id(car)] });
            }
            Ok(json!({
                "car": ids::car(car),
                "name": info.name,
                "units": units,
                "device": device,
                "messages": messages_json(&msgs),
                "ha_entities": entities,
                "inputs": inputs,
                "sources": sources,
            }))
        })
        .await?;
    Ok(Json(value))
}

// ---- test

async fn test(State(app): State<AppState>, v: Viewer, body: JsonBody) -> ApiResult<Json<Value>> {
    if !body.0.is_empty() {
        let mut f = Fields::new();
        for k in body.0.keys() {
            f.add(k, "Unknown field (the test uses the saved settings).");
        }
        f.bad_input()?;
    }
    let e = app.service::<Engine>()?;
    let s = app.blocking(move |_| load(&e)).await?;
    let at_ms = app.now();
    let r = mqtt::probe::probe(&s.settings).await;
    if let (Some(code), Ok(p)) = (r.error, app.service::<Publisher>()) {
        p.error(code, r.message.clone(), at_ms);
    }
    let summary = match r.error {
        None => "Tested the MQTT broker: connected and published one test message.".to_string(),
        Some(code) => format!("Tested the MQTT broker: {code}."),
    };
    v.audit(&app, "mqtt-tested", Some(&target("settings", None, "mqtt")), &summary).await?;
    Ok(Json(json!({
        "ok": r.ok,
        "error": r.error,
        "message": r.message,
        "latency_ms": r.latency_ms,
        "topic": r.topic,
        "tls": r.tls,
        "at_ms": at_ms,
    })))
}

// ---- network

async fn network(State(app): State<AppState>, _v: Viewer) -> ApiResult<Json<Value>> {
    let e = app.service::<Engine>()?;
    let (ss, m, names, sha256, own) = app
        .blocking(move |app| {
            let (names, sha256) = match super::certificate::running(app) {
                Some((n, s)) => (n, Some(s)),
                None => (Vec::new(), None),
            };
            let own = super::certificate::own(app);
            Ok((server::load(&e, &names)?, load(&e)?, names, sha256, own))
        })
        .await?;
    let upload_address = super::certificate::upload_address(&app, &names);
    let p = app.service::<Publisher>().ok();
    if let Some(p) = &p {
        p.observe(app.now());
    }
    let mqtt_used = p.as_ref().and_then(|p| p.log().last_used_ms);
    let (mqtt_host, _, _) = broker_parts(&m.settings);
    let geo_host = ss.geocode.url.as_deref().and_then(server::host_of);
    let tiles_online = ss.tiles.mode == "online";
    let tile_hosts = ss.tiles.hosts();
    let tile_host = tile_hosts.first().cloned();
    let geonames = server::host_of(places::geocode::download::DEFAULT_GEONAMES_URL);
    let mut inbound = Vec::new();
    if let Ok(n) = app.service::<Network>() {
        inbound.push(json!({ "purpose": "web", "port": n.web_port, "tls": true }));
        inbound.push(json!({ "purpose": "upload", "port": n.upload_port, "tls": true }));
    }
    let tesla = super::tesla::network_rows(&app);
    let teslafi = super::teslafi::network_rows(&app);
    let mut outbound = vec![
        // no address lookup runs online in this server yet: listed, never used
        json!({ "purpose": "geocoding", "host": geo_host, "enabled": ss.geocode.mode == "online" && geo_host.is_some(), "last_used_ms": null }),
        // the viewers' browsers load the tiles, not the server
        // (every host of the chosen preset and the styles viewers may switch to, in `hosts`)
        json!({
            "purpose": "tiles", "host": if tiles_online { tile_host } else { None },
            "hosts": if tiles_online { tile_hosts } else { Vec::new() },
            "preset": ss.tiles.chosen_id(), "enabled": tiles_online, "last_used_ms": null,
        }),
        json!({ "purpose": "geonames_download", "host": geonames, "enabled": false, "last_used_ms": null }),
        json!({ "purpose": "mqtt", "host": (!mqtt_host.is_empty()).then_some(mqtt_host), "enabled": m.settings.enabled, "last_used_ms": mqtt_used }),
        super::release::network_row(&app),
    ];
    outbound.extend(tesla);
    outbound.extend(teslafi);
    outbound.extend(super::teslamate_local::network_rows(&app));
    Ok(Json(json!({
        "outbound": outbound,
        "inbound": inbound,
        "certificate": { "names": names, "sha256": sha256, "own": own },
        "upload_address": upload_address,
    })))
}

// ---- publishing

/// Publishes one car's newest values (read off the async threads).
async fn publish_car(e: &Engine, m: &Arc<mqtt::Mqtt>, car: CarId, no_place: &Arc<str>) {
    let (e, m, no_place) = (e.clone(), m.clone(), no_place.clone());
    let r = tokio::task::spawn_blocking(move || -> ApiResult<()> {
        let (Some(now), Some(info)) = (car_now(&e, car)?, car_info(&e, car)?) else { return Ok(()) };
        let place = place_name(&e, &now, &no_place);
        m.publish_now(&info, &model::Now { car, ..now }, place.as_deref());
        Ok(())
    })
    .await;
    if let Ok(Err(err)) = r {
        tracing::debug!("MQTT: car {car} not published: {}", err.message);
    }
}

async fn publish_all(e: &Engine, m: &Arc<mqtt::Mqtt>, no_place: &Arc<str>) {
    let e2 = e.clone();
    let cars =
        tokio::task::spawn_blocking(move || engine::query::all_cars(&e2).unwrap_or_default()).await.unwrap_or_default();
    for car in cars {
        publish_car(e, m, car, no_place).await;
    }
}

/// The drive and charge events already sent, per car (the newest [`SENT_PER_CAR`] keys of each).
#[derive(Default)]
struct SentEvents(std::collections::HashMap<CarId, std::collections::VecDeque<String>>);

impl SentEvents {
    fn has(&self, car: CarId, key: &str) -> bool {
        self.0.get(&car).is_some_and(|k| k.iter().any(|x| x == key))
    }

    fn add(&mut self, car: CarId, key: String) {
        let keys = self.0.entry(car).or_default();
        if keys.len() >= SENT_PER_CAR {
            keys.pop_front();
        }
        keys.push_back(key);
    }
}

/// A car's drive and charge events of the last [`RECENT_MS`], oldest first (read off the async threads).
async fn recent_events(e: &Engine, car: CarId, now_ms: u64) -> Vec<engine::query::published::Published> {
    let e = e.clone();
    let from = now_ms.saturating_sub(RECENT_MS);
    let r = tokio::task::spawn_blocking(move || {
        engine::query::published::drive_charge_events(&e, car, from, u64::MAX, engine::query::published::MAX_EVENTS)
    })
    .await;
    match r {
        Ok(Ok(v)) => v,
        Ok(Err(err)) => {
            tracing::debug!("MQTT: the events of car {car} not read: {err}");
            Vec::new()
        }
        Err(_) => Vec::new(),
    }
}

/// Publishes the car's events not sent yet; with `quiet`, only notes them as sent (at start: those of the last
/// [`RECENT_MS`] were sent before, or are no news any more).
async fn publish_events(e: &Engine, m: &mqtt::Mqtt, car: CarId, sent: &mut SentEvents, now_ms: u64, quiet: bool) {
    for p in recent_events(e, car, now_ms).await {
        if sent.has(car, &p.key) {
            continue;
        }
        if !quiet {
            m.publish_event(p.event);
        }
        sent.add(car, p.key);
    }
}

async fn cars_of(e: &Engine) -> Vec<CarId> {
    let e = e.clone();
    tokio::task::spawn_blocking(move || engine::query::all_cars(&e).unwrap_or_default()).await.unwrap_or_default()
}

/// Clears what publishing with `old` left on the broker that `new` doesn't take over (Home Assistant discovery
/// turned off, a prefix changed), before the new publisher starts.
async fn clean_up(app: &AppState, e: &Engine, p: &Publisher, old: &mqtt::Settings, new: &mqtt::Settings) {
    let Some(what) = mqtt::cleanup::leftovers(old, new) else { return };
    let cars = cars_of(e).await;
    let msgs = match mqtt::cleanup::messages(old, &cars, what) {
        Ok(m) => m,
        Err(err) => {
            tracing::warn!("MQTT: the old topics can't be cleared: {err}");
            return;
        }
    };
    match mqtt::cleanup::clear(old, msgs).await {
        Ok(n) => tracing::info!("MQTT: cleared {n} retained topics the earlier settings left"),
        Err((code, why)) => {
            tracing::warn!("MQTT: the old topics were not cleared: {why}");
            p.error(code, format!("Clearing what the earlier settings left: {why}"), app.now());
        }
    }
}

/// Clears what this server published under `settings` on the broker (the data moved to another server, which
/// publishes the same topics now): an empty retained message on every car's value topics, its Home Assistant
/// discovery configs and the connection's status topic. How many were cleared, or why not.
pub async fn clear_owned(e: &Engine, settings: &mqtt::Settings) -> Result<usize, String> {
    if !settings.enabled {
        return Ok(0);
    }
    let cars = cars_of(e).await;
    let what = mqtt::cleanup::Leftovers { values: true, discovery: settings.discovery };
    let msgs = mqtt::cleanup::messages(settings, &cars, what).map_err(|err| err.to_string())?;
    mqtt::cleanup::clear(settings, msgs).await.map_err(|(_, why)| why)
}

/// Resolves when the data moved away (never without the move's side, or while it hasn't).
async fn moved_away(rx: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    match rx.as_mut() {
        Some(r) => {
            if r.wait_for(|m| *m).await.is_err() {
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending::<()>().await,
    }
}

/// The publisher, for `serve`: while MQTT is on, every car's newest values go to the broker as the engine records
/// them, and its drive and charge events once each; a saved change of the settings starts it again with them (after
/// clearing what the old layout left). Needs the engine and a [`Publisher`] among the services (else it ends at
/// once). Aborting the task closes the connection.
///
/// Once the data moved to another server ([`moving::old::Offer`]'s moved state) it publishes nothing more: it closes
/// its connection, clears the retained values it published ([`clear_owned`]: the new server publishes the same
/// topics) and ends. On a server whose data moved away it ends at once.
pub fn spawn(app: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (Ok(e), Ok(p)) = (app.service::<Engine>(), app.service::<Publisher>()) else { return };
        let mut moved = app.service::<moving::old::Offer>().ok().map(|o| o.watch());
        if crate::moving::data_moved(&app) {
            return;
        }
        let mut rx = e.subscribe();
        // the settings the last publisher ran with
        let mut prev: Option<mqtt::Settings> = None;
        loop {
            let e2 = e.clone();
            let loaded = tokio::task::spawn_blocking(move || -> ApiResult<Stored> {
                let mut s = load(&e2)?;
                s.settings.units = mqtt_units(server::load(&e2, &[])?.units);
                Ok(s)
            })
            .await;
            let stored = match loaded {
                Ok(Ok(s)) => s,
                _ => Stored::default(),
            };
            if let Some(old) = prev.take() {
                clean_up(&app, &e, &p, &old, &stored.settings).await;
            }
            let no_place: Arc<str> = Arc::from(stored.no_place_text.as_str());
            let running = match mqtt::Mqtt::start(&stored.settings) {
                Ok(m) => m.map(Arc::new),
                Err(err) => {
                    tracing::warn!("MQTT not started: {err}");
                    p.error("invalid_settings", err.to_string(), app.now());
                    None
                }
            };
            *p.live.lock().unwrap_or_else(|x| x.into_inner()) = running.as_ref().map_or_else(Weak::new, Arc::downgrade);
            let mut sent = SentEvents::default();
            // cars whose drives or charges changed, published once the engine has worked through what it was given
            // (a late upload goes log by log: only its final rows go out)
            let mut dirty: std::collections::BTreeSet<CarId> = std::collections::BTreeSet::new();
            let mut dirty_since = std::time::Instant::now();
            if let Some(m) = &running {
                prev = Some(stored.settings.clone());
                tracing::info!("MQTT publishing to {}", m.stats().broker);
                publish_all(&e, m, &no_place).await;
                let now = app.now();
                for car in cars_of(&e).await {
                    publish_events(&e, m, car, &mut sent, now, true).await;
                }
            }
            let mut tick = tokio::time::interval(TICK);
            let mut gone = false;
            loop {
                tokio::select! {
                    () = p.restart.notified() => break,
                    () = moved_away(&mut moved) => {
                        gone = true;
                        break;
                    }
                    _ = tick.tick() => {
                        p.observe(app.now());
                        if let Some(m) = &running
                            && !dirty.is_empty()
                            && (e.queued() == 0 || dirty_since.elapsed() >= EVENTS_WAIT)
                        {
                            let now = app.now();
                            for car in std::mem::take(&mut dirty) {
                                publish_events(&e, m, car, &mut sent, now, false).await;
                            }
                        }
                    }
                    ev = rx.recv() => {
                        let Some(m) = &running else {
                            if matches!(ev, Err(tokio::sync::broadcast::error::RecvError::Closed)) { return }
                            continue;
                        };
                        match ev {
                            Ok(engine::Event::Now { car, .. }) => publish_car(&e, m, car, &no_place).await,
                            Ok(engine::Event::Rows { car, kinds, .. }) if kinds.iter().any(|k| *k == "drives" || *k == "charges") => {
                                if dirty.is_empty() {
                                    dirty_since = std::time::Instant::now();
                                }
                                dirty.insert(car);
                                if e.queued() == 0 {
                                    let now = app.now();
                                    for car in std::mem::take(&mut dirty) {
                                        publish_events(&e, m, car, &mut sent, now, false).await;
                                    }
                                }
                            }
                            // (the engine sends no `removed` yet: no car can be deleted)
                            Ok(engine::Event::Fleet { car, change: "removed", .. }) => m.forget_car(car),
                            Ok(engine::Event::Fleet { car, .. }) => publish_car(&e, m, car, &no_place).await,
                            Ok(_) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                publish_all(&e, m, &no_place).await;
                                if dirty.is_empty() {
                                    dirty_since = std::time::Instant::now();
                                }
                                dirty.extend(cars_of(&e).await);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                        }
                    }
                }
            }
            *p.live.lock().unwrap_or_else(|x| x.into_inner()) = Weak::new();
            let was_on = running.is_some();
            if let Some(m) = running {
                match Arc::try_unwrap(m) {
                    Ok(m) => m.stop().await,
                    Err(m) => drop(m),
                }
            }
            if gone {
                if was_on {
                    match clear_owned(&e, &stored.settings).await {
                        Ok(n) => tracing::info!(
                            "MQTT: the data moved to another server: publishing stopped, {n} retained topics cleared"
                        ),
                        Err(why) => tracing::warn!(
                            "MQTT: the data moved to another server: publishing stopped; the retained topics were not cleared: {why}"
                        ),
                    }
                }
                return;
            }
        }
    })
}
