//! The TeslaFi input, for admins: `/api/v1/integrations/teslafi` (docs/ui-api.md, "Integrations: TeslaFi"). For
//! owners who pay for TeslaFi: TeslaFi's own polls of the car, read from its API with the owner's TeslaFi token.
//!
//! | Route | |
//! |---|---|
//! | `GET`/`PATCH /integrations/teslafi` | `{ enabled, accounts, cars, hosts, poll, polls, polls_today, rate_limited_until_ms, last_error, clients_built, tls, version }`; PATCH `{ version, poll }` |
//! | `GET`/`POST /integrations/teslafi/accounts` | POST `{ label, token, time_zone? }` -> 201 Account |
//! | `GET`/`PATCH`/`DELETE /integrations/teslafi/accounts/{account}` | PATCH `{ version, label?, paused?, time_zone?, token? }` |
//! | `GET`/`PUT /integrations/teslafi/accounts/{account}/cars` | PUT `{ version, items: [{ vehicle, match }] }` |
//! | `GET /integrations/teslafi/status` | what each account's poller is doing |
//!
//! All admin only, reads included (`/integrations` is in `policy::ADMIN_READS`). Every change but the car matches
//! asks for the admin's own password again (`confirm_password` or the `X-Sentry-Mate-Password` header) and, when the
//! admin has two-step sign-in on, a code; the car matches take the admin session alone (they store and reveal no
//! token). Every change writes its own audit entry. No answer, audit entry or log line ever holds the TeslaFi
//! token or a VIN serial: the [`teslafi`] crate keeps the token sealed and never hands it out, and the answers here
//! are built from its token-free `Account`.
//!
//! Nothing connects to TeslaFi until an admin adds a token: the service builds no client before that
//! (`clients_built` says how many it built), and a server that starts with no account starts no poller.

use crate::accounts_admin::{confirm_code, confirm_password};
use crate::ctx::{Admin, JsonBody, audit_details, target};
use crate::events::{Event, Events};
use crate::resources::notices::Notice;
use crate::{ApiError, ApiResult, AppState, Fields, Params, Viewer, ids};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use engine::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use teslafi::{Account, AccountState, AccountStatus, Match, Pace, ServiceError};

/// The TeslaFi input as the web side holds it (handed over with `Builder::service`).
pub type TeslafiService = teslafi::Service<engine::TeslafiSink>;

/// Labels at most (characters).
const MAX_LABEL: usize = 64;

/// The database setting the input's own settings are kept under.
pub const SETTING: &str = "teslafi";

/// The input's settings: how often TeslaFi is read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TeslafiSettings {
    pub poll: Pace,
    /// Supercharger invoices (`/integrations/invoices`): off until an admin turns them on.
    pub invoices: bool,
    pub version: u64,
}

impl TeslafiSettings {
    /// The pollers' cadence: the pace, and whether they read invoices.
    pub fn cadence(&self) -> teslafi::Cadence {
        let mut c = self.poll.cadence();
        c.invoices = self.invoices;
        c
    }
}

impl Default for TeslafiSettings {
    fn default() -> TeslafiSettings {
        TeslafiSettings { poll: Pace::Normal, invoices: false, version: 1 }
    }
}

/// The saved settings (the defaults when none are saved or they don't read).
pub fn load_settings(e: &Engine) -> TeslafiSettings {
    e.db().setting(SETTING).ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// The options the service starts with: the saved pace, the owner's proxy and the server's default time zone.
pub fn options(e: &Engine) -> teslafi::Options {
    let mut o = teslafi::Options::default();
    o.proxy = super::tesla::proxy(e);
    o.cadence = load_settings(e).cadence();
    o.default_time_zone = e.options().default_time_zone.clone();
    o
}

/// The TeslaFi input of a server over `engine`, keeping its accounts sealed with `vault` in `data/teslafi`. Builds
/// nothing that talks to the network; start its pollers with [`TeslafiService::start`] (none without an account).
/// When TeslaFi refuses an account's token, an audit entry is written in the server's name and the admins' pages get
/// a notice.
pub fn service(
    data: &std::path::Path,
    vault: Arc<vault::Vault>,
    engine: &Engine,
    audit: Arc<accounts::Audit>,
    events: Events,
) -> TeslafiService {
    let (audit2, events2) = (audit.clone(), events.clone());
    let hook: engine::tesla_in::TeslafiHook = Arc::new(move |a: &Account| {
        let Some(n) = notice(a) else { return };
        let (what, summary) = match a.state {
            AccountState::Refused => (
                "teslafi-token-refused",
                format!(
                    "TeslaFi refused the token of the TeslaFi account {} (a wrong token, or the TeslaFi subscription \
                     ended): its car is not read.",
                    a.label
                ),
            ),
            _ => ("teslafi-account-error", format!("The TeslaFi account {} is not read: {}", a.label, n.message)),
        };
        let details = audit_details(Some(&target("teslafi", None, &a.id)), &summary);
        let audit = audit.clone();
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
    });
    let limited = rate_limit_hook(audit2, events2);
    let sink = engine::TeslafiSink { engine: engine.clone(), on_account: Some(hook), on_rate_limit: Some(limited) };
    teslafi::Service::new(data, vault, Arc::new(sink), options(engine))
}

/// What is done when TeslaFi starts limiting an account's requests (a 429 after none): an audit entry in the server's
/// name and a notice for the admins' pages, as for the Tesla API (`tesla-rate-limited`).
pub fn rate_limit_hook(audit: Arc<accounts::Audit>, events: Events) -> engine::tesla_in::TeslafiRateLimitHook {
    Arc::new(move |a: &Account, until_ms: u64| {
        let n = rate_notice(a, until_ms, None);
        let details = audit_details(Some(&target("teslafi", None, &a.id)), &n.message);
        let audit = audit.clone();
        let write = move || {
            if let Err(e) = audit.record(None, None, "teslafi-rate-limited", &details) {
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
    })
}

/// The settings changed: the pollers follow a new proxy (they start again with it).
pub fn follow_proxy(app: &AppState, proxy: Option<&str>) {
    if let Ok(svc) = app.service::<TeslafiService>() {
        svc.reconfigure(proxy.and_then(tesla::Proxy::parse));
    }
}

/// A time in words, UTC to the minute: `2026-11-24 14:05 UTC`.
fn utc_minute(ms: u64) -> String {
    let day = engine::tz::date_str(ms, engine::tz::zone("UTC"));
    format!("{day} {:02}:{:02} UTC", (ms / 3_600_000) % 24, (ms / 60_000) % 60)
}

/// The notice of TeslaFi limiting an account's requests (it answered 429): nothing is asked before `until_ms`.
pub fn rate_notice(a: &Account, until_ms: u64, since_ms: Option<u64>) -> Notice {
    Notice {
        code: "teslafi_rate_limited",
        level: "warning",
        message: format!(
            "TeslaFi is limiting requests for the TeslaFi account \"{}\", next try at {}. This server sets no limit \
             of its own; it waits as long as TeslaFi asks.",
            a.label,
            utc_minute(until_ms)
        ),
        since_ms,
        fix_link: Some("/settings/integrations#teslafi"),
        detail: Some(json!({ "account": a.id, "until_ms": until_ms })),
    }
}

/// The notice an account in trouble gives (none while it is fine or paused).
pub fn notice(a: &Account) -> Option<Notice> {
    let detail = Some(json!({ "account": a.id }));
    let since_ms = a.last_error.as_ref().map(|e| e.at_ms);
    if a.paused {
        return None;
    }
    match a.state {
        AccountState::Refused => Some(Notice {
            code: "teslafi_token_refused",
            level: "warning",
            message: format!(
                "TeslaFi refused the token of the TeslaFi account \"{}\": a wrong token, or the TeslaFi subscription \
                 ended. Its car is not read until you give a new token (Settings, TeslaFi).",
                a.label
            ),
            since_ms,
            fix_link: Some("/settings/integrations#teslafi"),
            detail,
        }),
        AccountState::Error => Some(Notice {
            code: "teslafi_account_error",
            level: "warning",
            message: format!(
                "The TeslaFi account \"{}\" is not read: {}.",
                a.label,
                a.last_error.as_ref().map_or("TeslaFi answers with errors", |e| e.message.as_str())
            ),
            since_ms,
            fix_link: Some("/settings/integrations#teslafi"),
            detail,
        }),
        _ => None,
    }
}

/// The notices of every TeslaFi account in trouble (for admins).
pub fn notices(app: &AppState) -> Vec<Notice> {
    let Ok(s) = app.service::<TeslafiService>() else { return Vec::new() };
    let status = s.status();
    let mut out: Vec<Notice> = Vec::new();
    for a in s.accounts() {
        out.extend(notice(&a));
        let st = status.iter().find(|x| x.account == a.id);
        if a.state == AccountState::Ok
            && !a.paused
            && let Some(until) = st.and_then(|x| x.rate_limited_until_ms)
        {
            let since = st.and_then(|x| x.last_error.as_ref()).filter(|e| e.code == "rate_limited").map(|e| e.at_ms);
            out.push(rate_notice(&a, until, since));
        }
    }
    out
}

/// Whether a vehicle of an account is matched to `car` (it is read for that car), for the car's data sources.
pub fn feeds_car(app: &AppState, car: model::CarId) -> bool {
    app.service::<TeslafiService>()
        .is_ok_and(|s| s.accounts().iter().flat_map(|a| &a.cars).any(|c| c.matched == Some(Match::Car { car })))
}

/// The TeslaFi part of `/system/health`: accounts, matched cars, requests today, the last problem.
pub fn health(app: &AppState) -> Value {
    let Ok(svc) = app.service::<TeslafiService>() else {
        return json!({ "accounts": 0, "cars": 0, "requests_today": 0, "last_error": null });
    };
    let accounts = svc.accounts();
    let status = svc.status();
    json!({
        "accounts": accounts.len(),
        "cars": matched_cars(&accounts),
        "requests_today": status.iter().map(|s| s.polls_today).sum::<u64>(),
        "last_error": last_error(&accounts),
    })
}

/// The TeslaFi row of `/network`: TeslaFi's host (`enabled` while an account is read), off when there is none.
pub fn network_rows(app: &AppState) -> Vec<Value> {
    let Ok(svc) = app.service::<TeslafiService>() else { return Vec::new() };
    let accounts = svc.accounts();
    if accounts.is_empty() {
        return vec![json!({ "purpose": "teslafi", "host": teslafi::HOST, "enabled": false, "last_used_ms": null })];
    }
    let reading = accounts.iter().any(|a| !a.paused && a.state != AccountState::Refused && svc.running(&a.id));
    let last = accounts.iter().filter_map(|a| a.last_ok_ms).max();
    svc.hosts()
        .into_iter()
        .map(|h| json!({ "purpose": "teslafi", "host": h, "enabled": reading, "last_used_ms": last }))
        .collect()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/integrations/teslafi", get(overview).patch(change_settings))
        .route("/api/v1/integrations/teslafi/accounts", get(list).post(add))
        .route("/api/v1/integrations/teslafi/accounts/{teslafi_account}", get(one).patch(change).delete(remove))
        .route("/api/v1/integrations/teslafi/accounts/{teslafi_account}/cars", get(cars).put(put_cars))
        .route("/api/v1/integrations/teslafi/status", get(status))
}

fn svc(app: &AppState) -> ApiResult<Arc<TeslafiService>> {
    app.service::<TeslafiService>()
}

/// A [`ServiceError`] as the API says it. Never the token or what TeslaFi answered: the codes and plain words only.
fn failed(svc: &TeslafiService, id: Option<&str>, e: ServiceError) -> ApiError {
    match e.code() {
        "no_such_teslafi_account" => ApiError::not_found("no_such_teslafi_account"),
        "no_such_vehicle" => ApiError::not_found("no_such_vehicle"),
        "invalid_token" => ApiError::invalid("token", "Not a TeslaFi token (TeslaFi, Settings, TeslaFi API)."),
        "invalid_time_zone" => ApiError::invalid("time_zone", "Not a time zone (e.g. America/Chicago)."),
        "stale" => ApiError::stale_version(id.and_then(|i| svc.account(i).ok()).map_or(0, |a| a.version)),
        "account_exists" => ApiError::conflict(
            "account_exists",
            "This TeslaFi token's car is already on another TeslaFi account here (or the token is another car's).",
        ),
        "car_taken" => ApiError::conflict("car_taken", "That car is already matched to another TeslaFi car."),
        "teslafi_refused" => ApiError::new(
            422,
            "teslafi_refused",
            "TeslaFi refused the token: check it on TeslaFi's API page (Settings, TeslaFi API), and that the TeslaFi \
             subscription is active.",
        ),
        "teslafi_unreachable" => ApiError::new(
            502,
            "teslafi_unreachable",
            "TeslaFi could not be reached, or answered with an error. Try again in a few minutes.",
        ),
        _ => ApiError::server(format!("teslafi: {e}")),
    }
}

/// The admin's password again (and two-step code), off the async threads.
async fn reconfirm(app: &AppState, v: &Viewer, body: &JsonBody, headers: &HeaderMap) -> ApiResult<()> {
    let password = confirm_password(Some(body), headers)?;
    let code = confirm_code(Some(body), headers)?;
    let v2 = v.clone();
    app.blocking(move |app| Ok(app.accounts.confirm(v2.signed_in(), &password, code.as_deref())?)).await
}

fn word<T: Serialize>(t: &T) -> Value {
    serde_json::to_value(t).unwrap_or(Value::Null)
}

fn status_word(a: &Account) -> &'static str {
    if a.paused {
        return "paused";
    }
    match a.state {
        AccountState::Checking => "checking",
        AccountState::Ok => "ok",
        AccountState::Refused => "token_refused",
        AccountState::Error => "error",
    }
}

fn match_json(m: Option<Match>) -> Value {
    match m {
        Some(Match::Car { car }) => json!(ids::car(car)),
        Some(Match::New) => json!("new"),
        Some(Match::LeaveOut) => json!("leave_out"),
        None => Value::Null,
    }
}

fn matched_cars(accounts: &[Account]) -> usize {
    accounts.iter().flat_map(|a| &a.cars).filter(|c| matches!(c.matched, Some(Match::Car { .. }))).count()
}

fn error_json(e: &teslafi::store::ErrorInfo) -> Value {
    json!({ "code": e.code, "message": e.message, "at_ms": e.at_ms })
}

/// The newest problem of any account.
fn last_error(accounts: &[Account]) -> Value {
    accounts.iter().filter_map(|a| a.last_error.as_ref().map(|e| (a, e))).max_by_key(|(_, e)| e.at_ms).map_or(
        Value::Null,
        |(a, e)| json!({ "account": a.id, "code": e.code, "message": e.message, "at_ms": e.at_ms }),
    )
}

/// One car of an account, and the car it most likely is (when not matched: the one car with its VIN prefix).
fn car_json(e: &Engine, c: &teslafi::AccountCar, st: Option<&AccountStatus>) -> Value {
    let suggested = if c.matched.is_none() {
        c.vin_prefix.as_deref().and_then(|p| e.suggest_car(p).ok().flatten()).map(ids::car)
    } else {
        None
    };
    let current = st.is_some_and(|s| s.vehicle.as_deref() == Some(c.vehicle.as_str()));
    json!({
        "vehicle": c.vehicle,
        "vin_prefix": c.vin_prefix,
        "model": c.model.map(|m| format!("Model {m}")),
        "year": c.year,
        "display_name": c.display_name,
        "match": match_json(c.matched),
        "suggested": suggested,
        "current": current,
        "car_state": if current { st.and_then(|s| s.car_state.clone()) } else { None },
    })
}

/// An account as the API writes it (no token: `Account` holds none).
fn account_json(e: &Engine, svc: &TeslafiService, a: &Account) -> Value {
    let st = svc.status().into_iter().find(|s| s.account == a.id);
    let cars: Vec<Value> = a.cars.iter().map(|c| car_json(e, c, st.as_ref())).collect();
    json!({
        "id": a.id,
        "label": a.label,
        "hosts": [teslafi::HOST],
        "added_ms": a.added_ms,
        "status": status_word(a),
        "paused": a.paused,
        "time_zone": a.time_zone,
        "running": svc.running(&a.id),
        "mode": st.as_ref().map_or(json!("off"), |s| word(&s.mode)),
        "last_ok_ms": a.last_ok_ms,
        "last_error": a.last_error.as_ref().map(error_json),
        "polls_today": st.as_ref().map_or(0, |s| s.polls_today),
        "rate_limited_until_ms": st.as_ref().and_then(|s| s.rate_limited_until_ms),
        "tls": st.as_ref().and_then(|s| s.tls),
        "cars": cars,
        "version": a.version,
    })
}

async fn account_answer(app: &AppState, id: String) -> ApiResult<Value> {
    let s = svc(app)?;
    let e = app.service::<Engine>()?;
    app.blocking(move |_| {
        let a = s.account(&id).map_err(|err| failed(&s, Some(&id), err))?;
        Ok(account_json(&e, &s, &a))
    })
    .await
}

fn label_of(body: &JsonBody) -> ApiResult<Option<String>> {
    let Some(l) = body.str("label")? else { return Ok(None) };
    if l.trim().is_empty() {
        return Err(ApiError::invalid("label", "Must not be empty."));
    }
    if l.chars().count() > MAX_LABEL {
        return Err(ApiError::invalid("label", "At most 64 characters."));
    }
    Ok(Some(l.trim().to_string()))
}

fn token_of(body: &JsonBody) -> ApiResult<Option<teslafi::Secret>> {
    let Some(t) = body.str("token")? else { return Ok(None) };
    let t = t.trim();
    if !teslafi::token_ok(t) {
        return Err(ApiError::invalid("token", "Not a TeslaFi token (TeslaFi, Settings, TeslaFi API)."));
    }
    Ok(Some(teslafi::Secret::new(t)))
}

/// `time_zone`: absent (no change), `null` (the server's default) or an IANA zone.
fn time_zone_of(body: &JsonBody) -> ApiResult<Option<Option<String>>> {
    match body.get("time_zone") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(z)) if teslafi::time_zone_ok(z.trim()) => Ok(Some(Some(z.trim().to_string()))),
        Some(_) => Err(ApiError::invalid("time_zone", "Must be a time zone (e.g. America/Chicago) or null.")),
    }
}

// ---------------------------------------------------------------- the input as a whole

async fn overview(State(app): State<AppState>, Admin(_): Admin) -> ApiResult<Json<Value>> {
    let s = svc(&app)?;
    let e = app.service::<Engine>()?;
    app.blocking(move |_| {
        let accounts = s.accounts();
        let status = s.status();
        let c = s.cadence();
        let settings = load_settings(&e);
        Ok(Json(json!({
            "enabled": !accounts.is_empty(),
            "accounts": accounts.len(),
            "cars": matched_cars(&accounts),
            "hosts": s.hosts(),
            "poll": settings.poll.word(),
            "polls": {
                "driving_s": c.driving_ms / 1000,
                "charging_s": c.charging_ms / 1000,
                "parked_s": c.idle_ms / 1000,
                "asleep_s": c.asleep_ms / 1000,
            },
            "polls_today": status.iter().map(|s| s.polls_today).sum::<u64>(),
            "rate_limited_until_ms": status.iter().filter_map(|s| s.rate_limited_until_ms).max(),
            "last_error": last_error(&accounts),
            "clients_built": s.clients_built(),
            "tls": status.iter().find_map(|s| s.tls),
            "invoices": super::invoices::teslafi_block(&e, &s, settings.invoices),
            "version": settings.version,
        })))
    })
    .await
}

/// `PATCH /integrations/teslafi` `{ version, poll }`: how often TeslaFi is read.
async fn change_settings(
    State(app): State<AppState>,
    Admin(v): Admin,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["version", "poll", "invoices", "confirm_password", "code"])?;
    let version = body.version()?;
    let invoices = body.bool("invoices")?;
    let pace = match body.str("poll")? {
        None => None,
        Some(p) => Some(Pace::parse(p).ok_or_else(|| ApiError::invalid("poll", "Must be often, normal or gentle."))?),
    };
    reconfirm(&app, &v, &body, &headers).await?;
    let s = svc(&app)?;
    let e = app.service::<Engine>()?;
    let (old, new) = app
        .blocking(move |_| {
            let _one = super::settings::CHANGES.lock().unwrap_or_else(|p| p.into_inner());
            let old = load_settings(&e);
            if old.version != version {
                return Err(ApiError::stale_version(old.version));
            }
            let mut new = old.clone();
            if let Some(p) = pace {
                new.poll = p;
            }
            if let Some(i) = invoices {
                new.invoices = i;
            }
            if new != old {
                new.version = old.version + 1;
                let value = serde_json::to_value(&new).map_err(ApiError::server)?;
                e.db()
                    .set_setting(SETTING, value)
                    .map_err(|err| ApiError::server(format!("teslafi settings: {err}")))?;
                s.set_cadence(new.cadence());
            }
            Ok((old, new))
        })
        .await?;
    if new.invoices != old.invoices {
        let summary = if new.invoices {
            "Turned the charge costs from TeslaFi on."
        } else {
            "Turned the charge costs from TeslaFi off."
        };
        v.audit(&app, "teslafi-invoices-changed", Some(&target("settings", None, "teslafi-invoices")), summary).await?;
    }
    if new.poll != old.poll {
        let summary = format!("Set how often TeslaFi is read to {}.", new.poll.word());
        v.audit(&app, "teslafi-poll-changed", Some(&target("settings", None, "teslafi")), &summary).await?;
    }
    if new == old {
        v.mark_audited();
    }
    overview(State(app), Admin(v)).await
}

async fn status(State(app): State<AppState>, Admin(_): Admin, q: Params) -> ApiResult<Response> {
    q.wants_csv()?;
    let s = svc(&app)?;
    let items: Vec<Value> = s
        .status()
        .into_iter()
        .map(|a| {
            json!({
                "account": a.account,
                "car": a.car.map(ids::car),
                "vehicle": a.vehicle,
                "mode": word(&a.mode),
                "car_state": a.car_state,
                "last_request_ms": a.last_request_ms,
                "next_request_ms": a.next_request_ms,
                "last_data_ms": a.last_data_ms,
                "polls_today": a.polls_today,
                "rate_limited_until_ms": a.rate_limited_until_ms,
                "tls": a.tls,
                "last_error": a.last_error.as_ref().map(error_json),
            })
        })
        .collect();
    crate::csv::answer(&q, "teslafi-status", json!({ "items": items, "next": null }))
}

// ---------------------------------------------------------------- accounts

async fn list(State(app): State<AppState>, Admin(_): Admin, q: Params) -> ApiResult<Response> {
    q.wants_csv()?;
    let s = svc(&app)?;
    let e = app.service::<Engine>()?;
    app.blocking(move |_| {
        let items: Vec<Value> = s.accounts().iter().map(|a| account_json(&e, &s, a)).collect();
        crate::csv::answer(&q, "teslafi-accounts", json!({ "items": items, "next": null }))
    })
    .await
}

async fn one(State(app): State<AppState>, Admin(_): Admin, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    account_answer(&app, id).await.map(Json)
}

/// `POST /integrations/teslafi/accounts` `{ label, token, time_zone? }`: checked with TeslaFi (one or two reads)
/// before anything is kept.
async fn add(State(app): State<AppState>, Admin(v): Admin, headers: HeaderMap, body: JsonBody) -> ApiResult<Response> {
    body.only(&["label", "token", "time_zone", "confirm_password", "code"])?;
    let label = label_of(&body)?.unwrap_or_else(|| "TeslaFi account".into());
    let token = token_of(&body)?.ok_or_else(|| ApiError::bad_input("token", "Required."))?;
    let time_zone = time_zone_of(&body)?.flatten();
    reconfirm(&app, &v, &body, &headers).await?;
    let s = svc(&app)?;
    let a = s.add(&label, token, time_zone, app.now()).await.map_err(|e| failed(&s, None, e))?;
    let n = a.cars.len();
    let summary = format!(
        "Added the TeslaFi account {} ({}; checked with TeslaFi).",
        a.label,
        if n == 0 { "its car not named yet".to_string() } else { format!("{n} car{}", if n == 1 { "" } else { "s" }) }
    );
    v.audit(&app, "teslafi-account-added", Some(&target("teslafi", None, &a.id)), &summary).await?;
    Ok((StatusCode::CREATED, Json(account_answer(&app, a.id.clone()).await?)).into_response())
}

/// `PATCH /integrations/teslafi/accounts/{account}` `{ version, label?, paused?, time_zone?, token? }`.
async fn change(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["version", "label", "paused", "time_zone", "token", "confirm_password", "code"])?;
    let version = body.version()?;
    let label = label_of(&body)?;
    let paused = body.bool("paused")?;
    let time_zone = time_zone_of(&body)?;
    let token = token_of(&body)?;
    let s = svc(&app)?;
    let before = {
        let (s, id) = (s.clone(), id.clone());
        app.blocking(move |_| s.account(&id).map_err(|e| failed(&s, Some(&id), e))).await?
    };
    reconfirm(&app, &v, &body, &headers).await?;
    let change = teslafi::Change { label: label.clone(), paused, time_zone: time_zone.clone() };
    // all or nothing: a new token is checked with TeslaFi first, and when TeslaFi refuses it or cannot be reached
    // nothing at all is changed
    let new_token = token.is_some();
    let after = match token {
        Some(t) => s.change(&id, version, change, Some(t)).await.map_err(|e| failed(&s, Some(&id), e))?,
        None => {
            let (s, id) = (s.clone(), id.clone());
            app.blocking(move |_| s.update(&id, version, change).map_err(|e| failed(&s, Some(&id), e))).await?
        }
    };
    let target = target("teslafi", None, &id);
    let mut audited = false;
    if let Some(p) = paused
        && p != before.paused
    {
        let (what, summary) = if p {
            ("teslafi-account-paused", format!("Paused the TeslaFi account {}: its car is not read.", after.label))
        } else {
            ("teslafi-account-resumed", format!("Resumed the TeslaFi account {}.", after.label))
        };
        v.audit(&app, what, Some(&target), &summary).await?;
        audited = true;
    }
    let mut what = Vec::new();
    if label.as_deref().is_some_and(|l| l != before.label) {
        what.push(format!("renamed it from {}", before.label));
    }
    if let Some(z) = &time_zone
        && *z != before.time_zone
    {
        what.push(format!("set its time zone to {}", z.as_deref().unwrap_or("the server's default")));
    }
    if !what.is_empty() {
        let summary = format!("Changed the TeslaFi account {}: {}.", after.label, what.join(", "));
        v.audit(&app, "teslafi-account-changed", Some(&target), &summary).await?;
        audited = true;
    }
    if new_token {
        let summary = format!("Gave the TeslaFi account {} a new token (checked with TeslaFi).", after.label);
        v.audit(&app, "teslafi-token-replaced", Some(&target), &summary).await?;
        audited = true;
    }
    if !audited {
        v.mark_audited();
    }
    account_answer(&app, id).await.map(Json)
}

/// `DELETE /integrations/teslafi/accounts/{account}`: polling stops at once and the sealed file (with the token) is
/// deleted. The history already stored stays.
async fn remove(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["confirm_password", "code"])?;
    let s = svc(&app)?;
    let a = {
        let (s, id) = (s.clone(), id.clone());
        app.blocking(move |_| s.account(&id).map_err(|e| failed(&s, Some(&id), e))).await?
    };
    reconfirm(&app, &v, &body, &headers).await?;
    {
        let (s, id) = (s.clone(), id.clone());
        app.blocking(move |_| s.remove(&id).map_err(|e| failed(&s, Some(&id), e))).await?;
    }
    let summary = format!(
        "Removed the TeslaFi account {}: its token is deleted here; the history already stored stays.",
        a.label
    );
    v.audit(&app, "teslafi-account-removed", Some(&target("teslafi", None, &id)), &summary).await?;
    Ok(Json(json!({ "deleted": true, "id": id })))
}

// ---------------------------------------------------------------- the cars of an account

async fn cars(State(app): State<AppState>, Admin(_): Admin, Path(id): Path<String>, q: Params) -> ApiResult<Response> {
    q.wants_csv()?;
    crate::csv::answer(&q, "teslafi-account-cars", cars_answer(&app, id).await?)
}

/// An account's cars as the list `GET .../cars` answers.
async fn cars_answer(app: &AppState, id: String) -> ApiResult<Value> {
    let a = account_answer(app, id).await?;
    Ok(json!({ "items": a["cars"].clone(), "next": null, "version": a["version"].clone() }))
}

/// `PUT /integrations/teslafi/accounts/{account}/cars` `{ version, items: [{ vehicle, match }] }`: `match` is a car
/// (`c3`), `"new"` (a new car is made for it), `"leave_out"` (never read) or `null` (not decided: not read). Only a
/// matched car gets TeslaFi's data.
async fn put_cars(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path(id): Path<String>,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["version", "items"])?;
    let version = body.version()?;
    let Some(Value::Array(items)) = body.get("items") else {
        return Err(ApiError::bad_input("items", "Required: a list of { vehicle, match }."));
    };
    let s = svc(&app)?;
    let before = {
        let (s, id) = (s.clone(), id.clone());
        app.blocking(move |_| s.account(&id).map_err(|e| failed(&s, Some(&id), e))).await?
    };
    let mut changes: Vec<(String, Option<Match>)> = Vec::new();
    let mut bad = Fields::new();
    let shape = "Must be a car id, \"new\", \"leave_out\" or null.";
    for (i, item) in items.iter().enumerate() {
        let Value::Object(m) = item else {
            return Err(ApiError::bad_input(&format!("items.{i}"), "Must be { vehicle, match }."));
        };
        if let Some(k) = m.keys().find(|k| *k != "vehicle" && *k != "match") {
            return Err(ApiError::bad_input(&format!("items.{i}.{k}"), "Unknown field."));
        }
        let Some(vehicle) = m.get("vehicle").and_then(Value::as_str) else {
            return Err(ApiError::bad_input(&format!("items.{i}.vehicle"), "Required: the car's id here (fv1)."));
        };
        if !before.cars.iter().any(|c| c.vehicle == vehicle) {
            bad.add(&format!("items.{i}.vehicle"), "No such car on this account.");
            continue;
        }
        if changes.iter().any(|(v, _)| v == vehicle) {
            bad.add(&format!("items.{i}.vehicle"), "Named twice.");
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
        changes.push((vehicle.to_string(), m));
    }
    bad.invalid()?;
    let after = {
        let (s, id, ch) = (s.clone(), id.clone(), changes.clone());
        app.blocking(move |_| s.set_matches(&id, version, &ch).map_err(|e| failed(&s, Some(&id), e))).await?
    };
    let mut audited = false;
    for (vehicle, m) in &changes {
        let old = before.cars.iter().find(|c| &c.vehicle == vehicle).and_then(|c| c.matched);
        if old == *m {
            continue;
        }
        let name = before
            .cars
            .iter()
            .find(|c| &c.vehicle == vehicle)
            .and_then(|c| c.display_name.clone())
            .unwrap_or_else(|| vehicle.clone());
        let car = match m {
            Some(Match::Car { car }) => Some(*car),
            _ => None,
        };
        let of = format!("of the TeslaFi account {}", after.label);
        let summary = match (m, car) {
            (_, Some(c)) => {
                let car_name = app.blocking(move |app| Ok(app.car_name(c))).await?;
                format!("Matched TeslaFi car {name} {of} to {car_name}: it is read from now on.")
            }
            (Some(Match::New), _) => {
                format!("Matched TeslaFi car {name} {of} to a new car of its own: it is read from now on.")
            }
            (Some(Match::LeaveOut), _) => format!("Left TeslaFi car {name} {of} out: it is not read."),
            _ => format!("Unmatched TeslaFi car {name} {of}: it is not read."),
        };
        let t = crate::ctx::target_of_car(&target("teslafi", None, &format!("{}:{vehicle}", after.id)), car);
        v.audit(&app, "teslafi-car-matched", Some(&t), &summary).await?;
        audited = true;
    }
    if !audited {
        v.mark_audited();
    }
    Ok(Json(cars_answer(&app, id).await?))
}

/// The charge costs block as `/integrations/teslafi` gives it now (`None` without the TeslaFi input).
pub(crate) fn invoices_now(app: &AppState) -> Option<Value> {
    let s = svc(app).ok()?;
    let e = app.service::<Engine>().ok()?;
    let settings = load_settings(&e);
    Some(super::invoices::teslafi_block(&e, &s, settings.invoices))
}
