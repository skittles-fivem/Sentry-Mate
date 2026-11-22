//! The local TeslaMate input, for admins: `/api/v1/integrations/teslamate` (docs/plans/teslamate-local-contract.md).
//! The owner's own TeslaMate server: its MQTT broker read only (live values), and optionally its database (catch-up).
//!
//! | Route | |
//! |---|---|
//! | `GET`/`PUT`/`PATCH`/`DELETE /integrations/teslamate` | the settings, status and cars; PUT `{ version, mqtt, database? }` adds (201) or replaces; PATCH `{ version, paused }`; DELETE removes |
//! | `POST /integrations/teslamate/test` | `{ target, mqtt?, database? }`: tries a draft or the saved settings |
//! | `GET`/`PUT /integrations/teslamate/cars` | PUT `{ version, items: [{ teslamate_car, match }] }` |
//!
//! All admin only, reads included (`/integrations` is in `policy::ADMIN_READS`). Every change asks for the admin's
//! own password again (`confirm_password` or the `X-Sentry-Mate-Password` header) and, with two-step sign-in on, a
//! code, and writes its own audit entry. No answer, audit entry or log line holds a password: the [`tmlocal`] crate
//! keeps them sealed and its `Config` says only `password_set`. The crate's rules ([`tmlocal::validate`]) answer
//! `422 plaintext_password` (a password over plain TCP without "Allow unencrypted") and `422 password_resend` (a
//! kept password and a new host, port or TLS).
//!
//! Nothing connects anywhere until an admin adds it: the service starts no task without its settings file.
//!
//! When it is in trouble (its broker refuses the connection, the user name or password, or has another certificate
//! than the pinned one, goes quiet, or a database catch-up fails), the admins' pages get a notice ([`notice`]) and
//! an audit entry is written in the server's name, as the TeslaFi input does, told by the engine's
//! `TeslamateLocalSink::on_change`.

use crate::accounts_admin::{confirm_code, confirm_password};
use crate::ctx::{Admin, JsonBody, audit_details, target};
use crate::events::{Event, Events};
use crate::resources::notices::Notice;
use crate::{ApiError, ApiResult, AppState, Fields, Viewer, ids};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use engine::Engine;
use model::CarId;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tmlocal::{
    Config, DbConfig, Draft, Invalid, LocalCar, Match, MqttConfig, Password, Problem, ServiceError, Status, Trouble,
};

/// The local TeslaMate input as the web side holds it (handed over with `Builder::service`).
pub type TeslamateLocalService = tmlocal::Service<engine::TeslamateLocalSink>;

/// The local TeslaMate input of a server over `engine`, keeping its settings sealed with `vault` in
/// `data/teslamate-local`. Builds nothing that talks to the network; [`TeslamateLocalService::start`] starts reading
/// (nothing without the settings file). When it gets into trouble an audit entry is written in the server's name and
/// the admins' pages get a notice ([`notice`]), once per trouble while it lasts.
pub fn service(
    data: &std::path::Path,
    vault: Arc<vault::Vault>,
    engine: &Engine,
    audit: Arc<accounts::Audit>,
    events: Events,
) -> TeslamateLocalService {
    service_with(data, vault, engine, audit, events, tmlocal::Options::default())
}

/// [`service`] with its own connection options (tests: a short quiet time, short waits).
pub fn service_with(
    data: &std::path::Path,
    vault: Arc<vault::Vault>,
    engine: &Engine,
    audit: Arc<accounts::Audit>,
    events: Events,
    opts: tmlocal::Options,
) -> TeslamateLocalService {
    // the troubles told already (a trouble that lasts is told once)
    let told: Mutex<HashSet<Trouble>> = Mutex::new(HashSet::new());
    let hook: engine::tesla_in::TeslamateLocalHook = Arc::new(move |st: &Status| {
        let problems = st.problems();
        let new: Vec<&Problem> = {
            let mut t = told.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let new = problems.iter().filter(|p| !t.contains(&p.trouble)).collect();
            *t = problems.iter().map(|p| p.trouble).collect();
            new
        };
        for p in new {
            let n = notice(p, now_ms());
            let details = audit_details(Some(&target("settings", None, "teslamate_local")), &n.message);
            let (audit, what) = (audit.clone(), audit_action(p.trouble));
            let write = move || {
                if let Err(e) = audit.record(None, None, what, &details) {
                    tracing::warn!("audit: {e}");
                }
            };
            match tokio::runtime::Handle::try_current() {
                Ok(h) => {
                    h.spawn_blocking(write);
                }
                Err(_) => write(),
            }
            if let Ok(v) = serde_json::to_value(&n) {
                events.publish(Event::Notice { notice: Arc::new(v), admins_only: true });
            }
        }
    });
    let sink = engine::TeslamateLocalSink { engine: engine.clone(), on_change: Some(hook) };
    tmlocal::Service::new(data, vault, Arc::new(sink), opts)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Where every local TeslaMate notice sends the admin: its section of Settings: Integrations.
pub const FIX_LINK: &str = "/settings/integrations#teslamate";

/// The audit action written when a trouble begins.
pub fn audit_action(t: Trouble) -> &'static str {
    match t {
        Trouble::ConnectionRefused => "teslamate-local-connection-refused",
        Trouble::AuthFailed => "teslamate-local-auth-failed",
        Trouble::PinMismatch => "teslamate-local-pin-mismatch",
        Trouble::Quiet => "teslamate-local-quiet",
        Trouble::CatchupFailed => "teslamate-local-catchup-failed",
    }
}

/// The notice code of a trouble.
pub fn notice_code(t: Trouble) -> &'static str {
    match t {
        Trouble::ConnectionRefused => "teslamate_connection_refused",
        Trouble::AuthFailed => "teslamate_auth_failed",
        Trouble::PinMismatch => "teslamate_pin_mismatch",
        Trouble::Quiet => "teslamate_quiet",
        Trouble::CatchupFailed => "teslamate_catchup_failed",
    }
}

/// The notice of a trouble at `now_ms`: `detail: { part: "mqtt" | "database", error }` (`error` the status's
/// `last_error` code, null for a quiet broker), linking to Settings: Integrations (#teslamate).
pub fn notice(p: &Problem, now_ms: u64) -> Notice {
    let why = p.error.as_ref().map(|e| e.message.trim_end_matches('.').to_string());
    let why = |default: &str| why.clone().filter(|w| !w.is_empty()).unwrap_or_else(|| default.to_string());
    let message = match p.trouble {
        Trouble::ConnectionRefused => format!(
            "The server cannot reach local TeslaMate's MQTT broker ({}): its live values are not read. Check the host \
             and port, and that the broker runs; the server tries again by itself.",
            why("the connection was refused")
        ),
        Trouble::AuthFailed => format!(
            "Local TeslaMate's MQTT broker refused the user name or password ({}): nothing is read from it until you \
             give the right ones.",
            why("not authorized")
        ),
        Trouble::PinMismatch => "The certificate of local TeslaMate's MQTT broker does not have the pinned SHA-256 \
                                 fingerprint, so the server does not connect. If the broker's certificate was renewed, \
                                 pin the new one."
            .to_string(),
        Trouble::Quiet => {
            let hours = p.since_ms.map_or(0, |t| now_ms.saturating_sub(t) / 3_600_000);
            format!(
                "Local TeslaMate's MQTT broker is connected but has sent nothing for {hours} hour{}: check that \
                 TeslaMate runs and publishes to this broker under the topic prefix set (a car asleep for days sends \
                 nothing either).",
                if hours == 1 { "" } else { "s" }
            )
        }
        Trouble::CatchupFailed => format!(
            "The catch-up from local TeslaMate's database failed ({}). It is tried again at the next interval; live \
             values from MQTT still come.",
            why("a query failed")
        ),
    };
    let part = match p.trouble.target() {
        tmlocal::Target::Mqtt => "mqtt",
        tmlocal::Target::Database => "database",
    };
    Notice {
        code: notice_code(p.trouble),
        level: "warning",
        message,
        since_ms: p.since_ms,
        fix_link: Some(FIX_LINK),
        detail: Some(json!({ "part": part, "error": p.error.as_ref().map(|e| e.code.clone()) })),
    }
}

/// The notices of the local TeslaMate input in trouble now (for admins).
pub fn notices(app: &AppState) -> Vec<Notice> {
    let now = app.now();
    svc(app).map(|s| s.status().problems().iter().map(|p| notice(p, now)).collect()).unwrap_or_default()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/integrations/teslamate", get(overview).put(put).patch(patch).delete(remove))
        .route("/api/v1/integrations/teslamate/test", post(test))
        .route("/api/v1/integrations/teslamate/cars", get(cars).put(put_cars))
}

fn svc(app: &AppState) -> ApiResult<Arc<TeslamateLocalService>> {
    app.service::<TeslamateLocalService>()
}

fn word<T: serde::Serialize>(t: &T) -> Value {
    serde_json::to_value(t).unwrap_or(Value::Null)
}

/// The `/system/health` part: `{ enabled, mqtt_state, database_state, cars, last_message_ms, last_sync_ms }`.
pub fn health(app: &AppState) -> Value {
    let Ok(s) = svc(app) else {
        return json!({ "enabled": false, "mqtt_state": "off", "database_state": "off", "cars": 0,
                       "last_message_ms": null, "last_sync_ms": null });
    };
    let c = s.config();
    let st = s.status();
    json!({
        "enabled": c.is_some(),
        "mqtt_state": word(&st.mqtt.state),
        "database_state": word(&st.database.state),
        "cars": c.as_ref().map_or(0, matched),
        "last_message_ms": st.mqtt.last_message_ms,
        "last_sync_ms": st.database.last_sync_ms,
    })
}

/// The `/network` rows: `teslamate_mqtt` and `teslamate_db`, `host:port` as saved, `enabled` while used.
pub fn network_rows(app: &AppState) -> Vec<Value> {
    let Ok(s) = svc(app) else { return Vec::new() };
    let Some((c, st)) = s.config().map(|c| (c, s.status())) else {
        return vec![
            json!({ "purpose": "teslamate_mqtt", "host": null, "enabled": false, "last_used_ms": null }),
            json!({ "purpose": "teslamate_db", "host": null, "enabled": false, "last_used_ms": null }),
        ];
    };
    let broker = tmlocal::mqtt::Broker::new(&c.mqtt, None).address();
    let db_on = c.database.enabled && !c.paused;
    vec![
        json!({ "purpose": "teslamate_mqtt", "host": broker, "enabled": !c.paused, "last_used_ms": st.mqtt.last_message_ms }),
        json!({
            "purpose": "teslamate_db",
            "host": c.database.enabled.then(|| tmlocal::db_address(&c.database)),
            "enabled": db_on,
            "last_used_ms": st.database.last_sync_ms,
        }),
    ]
}

fn matched(c: &Config) -> usize {
    c.cars.iter().filter(|k| matches!(k.matched, Some(Match::Car { .. }))).count()
}

fn match_json(m: Option<Match>) -> Value {
    match m {
        Some(Match::Car { car }) => json!(ids::car(car)),
        Some(Match::New) => json!("new"),
        Some(Match::LeaveOut) => json!("leave_out"),
        None => Value::Null,
    }
}

/// The cars here a TeslaMate car may be: name and VIN prefix of each (sold cars left out).
fn candidates(e: &Engine) -> Vec<(CarId, String, Option<String>)> {
    let Ok(all) = engine::query::all_cars(e) else { return Vec::new() };
    all.into_iter()
        .filter(|c| !engine::query::fleet::is_sold(e, *c))
        .filter_map(|id| e.db().car(id).ok().flatten())
        .map(|c| (c.id, c.name, c.vin.map(|v| v.vin_prefix)))
        .collect()
}

fn cars_json(e: &Engine, c: &Config, st: &Status) -> Vec<Value> {
    let all = candidates(e);
    let taken: Vec<CarId> = c
        .cars
        .iter()
        .filter_map(|k| match k.matched {
            Some(Match::Car { car }) => Some(car),
            _ => None,
        })
        .collect();
    let free: Vec<tmlocal::Candidate<'_>> = all
        .iter()
        .filter(|(id, _, _)| !taken.contains(id))
        .map(|(id, name, vin)| tmlocal::Candidate { car: *id, name, vin_prefix: vin.as_deref() })
        .collect();
    c.cars
        .iter()
        .map(|k: &LocalCar| {
            let suggested = if k.matched.is_none() { tmlocal::suggest(k, &free).map(ids::car) } else { None };
            json!({
                "teslamate_car": k.teslamate_car,
                "display_name": k.display_name,
                "vin_prefix": k.vin_prefix,
                "match": match_json(k.matched),
                "suggested": suggested,
                "last_message_ms": st.cars_last_message_ms.get(&k.teslamate_car),
            })
        })
        .collect()
}

fn answer(e: &Engine, s: &TeslamateLocalService) -> Value {
    let c = s.config();
    let st = s.status();
    let enabled = c.is_some();
    let cfg = c.clone().unwrap_or(Config {
        version: 0,
        added_ms: 0,
        paused: false,
        mqtt: MqttConfig::default(),
        database: DbConfig::default(),
        cars: Vec::new(),
    });
    let mut mqtt = word(&cfg.mqtt);
    let mut database = word(&cfg.database);
    for v in [&mut mqtt, &mut database] {
        if let Value::Object(m) = v {
            // never a password (Config holds none); the pin as saved
            m.remove("password");
        }
    }
    json!({
        "enabled": enabled,
        "paused": cfg.paused,
        "mqtt": mqtt,
        "database": database,
        "status": { "mqtt": word(&st.mqtt), "database": word(&st.database) },
        "cars": if enabled { cars_json(e, &cfg, &st) } else { Vec::new() },
        "hosts": s.hosts(),
        "version": cfg.version,
    })
}

async fn overview(State(app): State<AppState>, Admin(_): Admin) -> ApiResult<Json<Value>> {
    let s = svc(&app)?;
    let e = app.service::<Engine>()?;
    app.blocking(move |_| Ok(Json(answer(&e, &s)))).await
}

/// The admin's password again (and two-step code), off the async threads.
async fn reconfirm(app: &AppState, v: &Viewer, body: &JsonBody, headers: &HeaderMap) -> ApiResult<()> {
    let password = confirm_password(Some(body), headers)?;
    let code = confirm_code(Some(body), headers)?;
    let v2 = v.clone();
    app.blocking(move |app| Ok(app.accounts.confirm(v2.signed_in(), &password, code.as_deref())?)).await
}

/// The crate's rule breaks as the API says them: `plaintext_password` or `password_resend` (422, on the password
/// field) when one of those, else `invalid` (422) with every field.
fn invalid_answer(errs: &[Invalid]) -> ApiError {
    let mut fields = Map::new();
    for code in ["plaintext_password", "password_resend"] {
        if let Some(i) = errs.iter().find(|i| i.code == code) {
            for i in errs.iter().filter(|i| i.code == code) {
                fields.entry(i.field.clone()).or_insert_with(|| json!(i.message));
            }
            return ApiError::new(
                422,
                if code == "plaintext_password" { "plaintext_password" } else { "password_resend" },
                i.message.clone(),
            )
            .with_fields(fields);
        }
    }
    let mut f = Fields::new();
    for i in errs {
        f.add(&i.field, &i.message);
    }
    f.into_invalid()
}

fn failed(s: &TeslamateLocalService, e: ServiceError) -> ApiError {
    match e {
        ServiceError::Invalid(v) => invalid_answer(&v),
        ServiceError::Stale => ApiError::stale_version(s.config().map_or(0, |c| c.version)),
        ServiceError::NotAdded => ApiError::not_found("teslamate_not_added"),
        ServiceError::NoSuchCar => ApiError::invalid("items", "No such TeslaMate car here."),
        ServiceError::CarTaken => {
            ApiError::conflict("car_taken", "That car is already matched to another TeslaMate car.")
        }
        ServiceError::Store(e) => ApiError::server(format!("teslamate local: {e}")),
    }
}

// ---------------------------------------------------------------- reading a body's settings

fn obj<'a>(v: &'a Value, name: &str) -> ApiResult<&'a Map<String, Value>> {
    v.as_object().ok_or_else(|| ApiError::bad_input(name, "Must be an object."))
}

fn only(m: &Map<String, Value>, name: &str, allowed: &[&str]) -> ApiResult<()> {
    match m.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(ApiError::bad_input(&format!("{name}.{k}"), "Unknown field.")),
        None => Ok(()),
    }
}

fn s_of(m: &Map<String, Value>, name: &str, k: &str) -> ApiResult<Option<String>> {
    match m.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(ApiError::bad_input(&format!("{name}.{k}"), "Must be a string or null.")),
    }
}

fn b_of(m: &Map<String, Value>, name: &str, k: &str, default: bool) -> ApiResult<bool> {
    match m.get(k) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(ApiError::bad_input(&format!("{name}.{k}"), "Must be true or false.")),
    }
}

fn port_of(m: &Map<String, Value>, name: &str, default: u16) -> ApiResult<u16> {
    match m.get("port") {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p > 0)
            .ok_or_else(|| ApiError::bad_input(&format!("{name}.port"), "A port from 1 to 65535.")),
    }
}

/// `password`: a string sets it, `null` removes it, left out keeps it.
fn password_of(m: &Map<String, Value>, name: &str) -> ApiResult<Password> {
    match m.get("password") {
        None => Ok(Password::Keep),
        Some(Value::Null) => Ok(Password::Remove),
        Some(Value::String(p)) => Ok(Password::Set(tmlocal::Secret::new(p.as_str()))),
        Some(_) => Err(ApiError::bad_input(&format!("{name}.password"), "Must be a string or null.")),
    }
}

fn mqtt_of(v: &Value) -> ApiResult<(MqttConfig, Password)> {
    let n = "mqtt";
    let m = obj(v, n)?;
    only(
        m,
        n,
        &["host", "port", "tls", "pin_sha256", "username", "password", "password_set", "prefix", "allow_unencrypted"],
    )?;
    let d = MqttConfig::default();
    let c = MqttConfig {
        host: s_of(m, n, "host")?.unwrap_or_default(),
        port: port_of(m, n, d.port)?,
        tls: b_of(m, n, "tls", false)?,
        pin_sha256: s_of(m, n, "pin_sha256")?,
        username: s_of(m, n, "username")?,
        password_set: false,
        prefix: s_of(m, n, "prefix")?.unwrap_or(d.prefix),
        allow_unencrypted: b_of(m, n, "allow_unencrypted", false)?,
    };
    Ok((c, password_of(m, n)?))
}

fn database_of(v: &Value) -> ApiResult<(DbConfig, Password)> {
    let n = "database";
    let m = obj(v, n)?;
    only(
        m,
        n,
        &[
            "enabled",
            "host",
            "port",
            "database",
            "username",
            "password",
            "password_set",
            "tls",
            "pin_sha256",
            "allow_unencrypted",
            "interval_s",
        ],
    )?;
    let d = DbConfig::default();
    let interval_s = match m.get("interval_s") {
        None | Some(Value::Null) => d.interval_s,
        Some(v) => v.as_u64().ok_or_else(|| ApiError::bad_input("database.interval_s", "Seconds, a whole number."))?,
    };
    let c = DbConfig {
        enabled: b_of(m, n, "enabled", false)?,
        host: s_of(m, n, "host")?.unwrap_or_default(),
        port: port_of(m, n, d.port)?,
        database: s_of(m, n, "database")?.unwrap_or(d.database),
        username: s_of(m, n, "username")?.unwrap_or(d.username),
        password_set: false,
        tls: b_of(m, n, "tls", false)?,
        pin_sha256: s_of(m, n, "pin_sha256")?,
        allow_unencrypted: b_of(m, n, "allow_unencrypted", false)?,
        interval_s,
    };
    Ok((c, password_of(m, n)?))
}

/// The draft of a body: each side given, else as saved (else the defaults), its password kept.
fn draft_of(body: &JsonBody, saved: Option<&Config>, need_mqtt: bool) -> ApiResult<Draft> {
    let (mqtt, mqtt_password) = match body.get("mqtt") {
        Some(v) if !v.is_null() => mqtt_of(v)?,
        _ if need_mqtt => return Err(ApiError::bad_input("mqtt", "Required: the MQTT broker's settings.")),
        _ => (saved.map(|c| c.mqtt.clone()).unwrap_or_default(), Password::Keep),
    };
    let (database, db_password) = match body.get("database") {
        Some(v) if !v.is_null() => database_of(v)?,
        _ => (saved.map(|c| c.database.clone()).unwrap_or_default(), Password::Keep),
    };
    Ok(Draft { mqtt, mqtt_password, database, db_password })
}

fn describe(c: &Config) -> String {
    let m = &c.mqtt;
    let mut s = format!(
        "MQTT {} ({}{}, prefix {})",
        tmlocal::mqtt::Broker::new(m, None).address(),
        if m.tls { "TLS" } else { "plain TCP" },
        if m.pin_sha256.is_some() { ", pinned certificate" } else { "" },
        m.prefix
    );
    if m.password_set {
        s.push_str(if m.tls { ", with a password" } else { ", with a password over plain TCP (allowed)" });
    }
    if c.database.enabled {
        s.push_str(&format!(
            "; database {} ({}, every {} s)",
            tmlocal::db_address(&c.database),
            if c.database.tls { "TLS" } else { "plain TCP" },
            c.database.interval_s
        ));
    } else {
        s.push_str("; database off");
    }
    s
}

// ---------------------------------------------------------------- changes

/// `PUT /integrations/teslamate` `{ version, mqtt, database?, confirm_password, code? }`.
async fn put(State(app): State<AppState>, Admin(v): Admin, headers: HeaderMap, body: JsonBody) -> ApiResult<Response> {
    body.only(&["version", "mqtt", "database", "confirm_password", "code"])?;
    let version = body.version()?;
    let s = svc(&app)?;
    let saved = {
        let s = s.clone();
        app.blocking(move |_| s.store().load().map_err(|e| ApiError::server(format!("teslamate local: {e}")))).await?
    };
    let mut draft = draft_of(&body, saved.as_ref(), true)?;
    // the rules first (no password asked for a draft that can't be saved)
    tmlocal::normalize(&mut draft);
    tmlocal::validate(&draft, saved.as_ref()).map_err(|e| invalid_answer(&e))?;
    if saved.as_ref().map_or(0, |c| c.version) != version {
        return Err(ApiError::stale_version(saved.as_ref().map_or(0, |c| c.version)));
    }
    reconfirm(&app, &v, &body, &headers).await?;
    let now = app.now();
    let c = {
        let s = s.clone();
        app.blocking(move |_| s.put(version, draft, now).map_err(|e| failed(&s, e))).await?
    };
    let added = saved.is_none();
    let (what, summary) = if added {
        ("teslamate-local-added", format!("Added the local TeslaMate: {}.", describe(&c)))
    } else {
        ("teslamate-local-changed", format!("Changed the local TeslaMate: {}.", describe(&c)))
    };
    v.audit(&app, what, Some(&target("settings", None, "teslamate_local")), &summary).await?;
    let e = app.service::<Engine>()?;
    let out = app.blocking(move |_| Ok(answer(&e, &s))).await?;
    let status = if added { StatusCode::CREATED } else { StatusCode::OK };
    Ok((status, Json(out)).into_response())
}

/// `PATCH /integrations/teslamate` `{ version, paused, confirm_password, code? }`.
async fn patch(
    State(app): State<AppState>,
    Admin(v): Admin,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["version", "paused", "confirm_password", "code"])?;
    let version = body.version()?;
    let paused = body.bool("paused")?;
    let s = svc(&app)?;
    let before = {
        let s = s.clone();
        app.blocking(move |_| s.store().get().map_err(|e| failed(&s, e.into()))).await?
    };
    reconfirm(&app, &v, &body, &headers).await?;
    match paused {
        Some(p) if p != before.paused || version != before.version => {
            let s2 = s.clone();
            app.blocking(move |_| s2.set_paused(version, p).map_err(|e| failed(&s2, e))).await?;
            let (what, summary) = if p {
                ("teslamate-local-paused", "Paused the local TeslaMate: nothing is read from it.")
            } else {
                ("teslamate-local-resumed", "Resumed the local TeslaMate.")
            };
            v.audit(&app, what, Some(&target("settings", None, "teslamate_local")), summary).await?;
        }
        _ => v.mark_audited(),
    }
    let e = app.service::<Engine>()?;
    app.blocking(move |_| Ok(Json(answer(&e, &s)))).await
}

/// `DELETE /integrations/teslamate` `{ confirm_password, code? }`: stops at once and deletes the sealed file (its
/// passwords). The history already stored stays.
async fn remove(
    State(app): State<AppState>,
    Admin(v): Admin,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["confirm_password", "code"])?;
    let s = svc(&app)?;
    {
        let s = s.clone();
        app.blocking(move |_| s.store().get().map_err(|e| failed(&s, e.into()))).await?;
    }
    reconfirm(&app, &v, &body, &headers).await?;
    {
        let s = s.clone();
        app.blocking(move |_| s.remove().map_err(|e| failed(&s, e))).await?;
    }
    let summary = "Removed the local TeslaMate: its settings and passwords are deleted here; the history already \
                   stored stays.";
    v.audit(&app, "teslamate-local-removed", Some(&target("settings", None, "teslamate_local")), summary).await?;
    Ok(Json(json!({ "deleted": true })))
}

/// `POST /integrations/teslamate/test` `{ target: "mqtt" | "database", mqtt?, database?, confirm_password, code? }`.
async fn test(
    State(app): State<AppState>,
    Admin(v): Admin,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["target", "mqtt", "database", "confirm_password", "code"])?;
    let target = match body.req_str("target")? {
        "mqtt" => tmlocal::Target::Mqtt,
        "database" => tmlocal::Target::Database,
        _ => return Err(ApiError::bad_input("target", "Must be mqtt or database.")),
    };
    let s = svc(&app)?;
    let saved = {
        let s = s.clone();
        app.blocking(move |_| s.store().load().map_err(|e| ApiError::server(format!("teslamate local: {e}")))).await?
    };
    let has_draft =
        body.get("mqtt").is_some_and(|v| !v.is_null()) || body.get("database").is_some_and(|v| !v.is_null());
    let draft = if has_draft { Some(draft_of(&body, saved.as_ref(), false)?) } else { None };
    if draft.is_none() && saved.is_none() {
        let field = if target == tmlocal::Target::Mqtt { "mqtt" } else { "database" };
        return Err(ApiError::bad_input(field, "Required: nothing is saved yet to test."));
    }
    reconfirm(&app, &v, &body, &headers).await?;
    let r = s.test(target, draft, app.now()).await.map_err(|e| failed(&s, e))?;
    v.mark_audited();
    Ok(Json(word(&r)))
}

// ---------------------------------------------------------------- cars

async fn cars(State(app): State<AppState>, Admin(_): Admin) -> ApiResult<Json<Value>> {
    let s = svc(&app)?;
    let e = app.service::<Engine>()?;
    app.blocking(move |_| {
        let c = s.store().get().map_err(|err| failed(&s, err.into()))?;
        Ok(Json(json!({ "items": cars_json(&e, &c, &s.status()), "version": c.version })))
    })
    .await
}

/// `PUT /integrations/teslamate/cars` `{ version, items: [{ teslamate_car, match }] }`: `match` is a car (`c3`),
/// `"new"`, `"leave_out"` or `null`. Only a matched car gets TeslaMate's data.
async fn put_cars(
    State(app): State<AppState>,
    Admin(v): Admin,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["version", "items", "confirm_password", "code"])?;
    let version = body.version()?;
    let Some(Value::Array(items)) = body.get("items") else {
        return Err(ApiError::bad_input("items", "Required: a list of { teslamate_car, match }."));
    };
    let s = svc(&app)?;
    let before = {
        let s = s.clone();
        app.blocking(move |_| s.store().get().map_err(|e| failed(&s, e.into()))).await?
    };
    let mut changes: Vec<(String, Option<Match>)> = Vec::new();
    let mut bad = Fields::new();
    let shape = "Must be a car id, \"new\", \"leave_out\" or null.";
    for (i, item) in items.iter().enumerate() {
        let Value::Object(m) = item else {
            return Err(ApiError::bad_input(&format!("items.{i}"), "Must be { teslamate_car, match }."));
        };
        if let Some(k) = m.keys().find(|k| *k != "teslamate_car" && *k != "match") {
            return Err(ApiError::bad_input(&format!("items.{i}.{k}"), "Unknown field."));
        }
        let tc = match m.get("teslamate_car") {
            Some(Value::String(t)) => t.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => return Err(ApiError::bad_input(&format!("items.{i}.teslamate_car"), "Required: TeslaMate's car id.")),
        };
        if before.car(&tc).is_none() {
            bad.add(&format!("items.{i}.teslamate_car"), "No such TeslaMate car here.");
            continue;
        }
        if changes.iter().any(|(t, _)| *t == tc) {
            bad.add(&format!("items.{i}.teslamate_car"), "Named twice.");
            continue;
        }
        let m = match m.get("match") {
            None | Some(Value::Null) => None,
            Some(Value::String(w)) if w == "new" => Some(Match::New),
            Some(Value::String(w)) if w == "leave_out" => Some(Match::LeaveOut),
            Some(Value::String(w)) => {
                let Some(car) = ids::parse_car(w) else {
                    return Err(ApiError::bad_input(&format!("items.{i}.match"), shape));
                };
                if !v.sees_existing(&app, car) {
                    bad.add(&format!("items.{i}.match"), "No such car.");
                    continue;
                }
                Some(Match::Car { car })
            }
            Some(_) => return Err(ApiError::bad_input(&format!("items.{i}.match"), shape)),
        };
        changes.push((tc, m));
    }
    bad.invalid()?;
    reconfirm(&app, &v, &body, &headers).await?;
    {
        let (s, ch) = (s.clone(), changes.clone());
        app.blocking(move |_| s.set_matches(version, &ch).map_err(|e| failed(&s, e))).await?;
    }
    let mut audited = false;
    for (tc, m) in &changes {
        let k = before.car(tc);
        if k.and_then(|k| k.matched) == *m {
            continue;
        }
        let name = k.and_then(|k| k.display_name.clone()).unwrap_or_else(|| format!("car {tc}"));
        let (car, what) = match m {
            Some(Match::Car { car }) => (Some(*car), format!("is car {}: it is read from now on", ids::car(*car))),
            Some(Match::New) => (None, "gets a new car of its own: it is read from now on".to_string()),
            Some(Match::LeaveOut) => (None, "is left out: it is not read".to_string()),
            None => (None, "is not matched to a car: it is not read".to_string()),
        };
        let t = target("teslamate_local", car, tc);
        let summary = format!("TeslaMate car {name} (id {tc}) of the local TeslaMate {what}.");
        v.audit(&app, "teslamate-local-car-matched", Some(&t), &summary).await?;
        audited = true;
    }
    if !audited {
        v.mark_audited();
    }
    cars(State(app), Admin(v)).await
}
