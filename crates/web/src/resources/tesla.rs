//! The Tesla API input, for admins: `/api/v1/integrations/tesla` (docs/ui-api.md, "Integrations: Tesla").
//!
//! | Route | |
//! |---|---|
//! | `GET`/`PATCH /integrations/tesla` | `{ enabled, accounts, cars, hosts, poll, polls, polls_today, last_error, clients_built, version }`; PATCH `{ version, poll }` |
//! | `GET`/`POST /integrations/tesla/accounts` | POST `{ label, region, refresh_token }` -> 201 Account |
//! | `POST /integrations/tesla/sign-in` | `{ region }` -> `{ sign_in, url, expires_ms }` (changes nothing stored) |
//! | `POST /integrations/tesla/sign-in/{sign_in}` | `{ pasted_url, label }` -> 201 Account |
//! | `GET`/`PATCH`/`DELETE /integrations/tesla/accounts/{account}` | PATCH `{ version, label?, paused?, location?, refresh_token? }` |
//! | `GET`/`PUT /integrations/tesla/accounts/{account}/cars` | PUT `{ version, items: [{ vehicle, match }] }` |
//! | `GET /integrations/tesla/status` | what each car's poller is doing |
//!
//! All admin only, reads included (the guard: `/integrations` is in `policy::ADMIN_READS`). Every change but the
//! sign-in start and the car matches asks for the admin's own password again (`confirm_password` or the
//! `X-Sentry-Mate-Password` header) and, when the admin has two-step sign-in on, a code (`code` or
//! `X-Sentry-Mate-Code`); the car matches take the admin session alone (they store and reveal no token). Every change
//! writes its own audit entry. No answer ever holds a Tesla token, a VIN serial or the account's e-mail: the
//! [`tesla`] crate keeps the tokens sealed and never hands them out, and the answers here are built from its
//! token-free `Account`.
//!
//! Nothing connects to Tesla until an admin adds an account: the service builds no client before that
//! (`clients_built` says how many it built), and a server that starts with no account starts no poller.

use crate::accounts_admin::{confirm_code, confirm_password};
use crate::ctx::{Admin, JsonBody, audit_details, target};
use crate::events::{Event, Events};
use crate::resources::notices::Notice;
use crate::{ApiError, ApiResult, AppState, Fields, Viewer, ids};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use engine::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tesla::{Account, AccountState, AccountStatus, Match, Pace, Region, ServiceError};

/// The Tesla API input as the web side holds it (handed over with `Builder::service`).
pub type TeslaService = tesla::Service<engine::TeslaSink>;

/// The database setting the input's own settings are kept under.
pub const SETTING: &str = "tesla";

/// Labels at most (characters).
const MAX_LABEL: usize = 64;

/// The input's settings: how often it asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TeslaSettings {
    pub poll: Pace,
    pub version: u64,
}

impl Default for TeslaSettings {
    fn default() -> TeslaSettings {
        TeslaSettings { poll: Pace::Normal, version: 1 }
    }
}

/// The saved settings (the defaults when none are saved or they don't read).
pub fn load_settings(e: &Engine) -> TeslaSettings {
    e.db().setting(SETTING).ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// The owner's proxy for what the server fetches itself (`/settings` `outbound.proxy`), when it is one the Tesla
/// client can use (`http://host:port`).
pub fn proxy(e: &Engine) -> Option<tesla::Proxy> {
    super::settings::load(e, &[]).ok()?.outbound.proxy.as_deref().and_then(tesla::Proxy::parse)
}

/// The options the service starts with: the saved pace and the owner's proxy.
pub fn options(e: &Engine) -> tesla::Options {
    let mut o = tesla::Options::default();
    o.proxy = proxy(e);
    o.cadence = load_settings(e).poll.cadence();
    o
}

/// The Tesla API input of a server over `engine`, keeping its accounts sealed with `vault` in `data/tesla`. Builds
/// nothing that talks to the network; start its pollers with [`TeslaService::start`] (none without an account).
/// When an account's state changes for the worse (a new sign-in is needed, Tesla refuses it), an audit entry is
/// written in the server's name and the admins' pages get a notice.
pub fn service(
    data: &std::path::Path,
    vault: Arc<vault::Vault>,
    engine: &Engine,
    audit: Arc<accounts::Audit>,
    events: Events,
) -> TeslaService {
    let hook: engine::tesla_in::AccountHook = Arc::new(move |a: &Account| {
        let Some(n) = notice(a) else { return };
        let (what, summary) = match a.state {
            AccountState::SignInNeeded => (
                "tesla-sign-in-needed",
                format!("Tesla stopped taking the sign-in of the Tesla account {}: its cars are not read.", a.label),
            ),
            _ => ("tesla-account-error", format!("Tesla refuses the Tesla account {}: {}", a.label, n.message)),
        };
        let details = audit_details(Some(&target("tesla", None, &a.id)), &summary);
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
    let sink = engine::TeslaSink { engine: engine.clone(), on_account: Some(hook) };
    tesla::Service::new(data, vault, Arc::new(sink), options(engine))
}

/// The settings changed: the pollers follow a new proxy (they start again with it).
pub fn follow_proxy(app: &AppState, proxy: Option<&str>) {
    if let Ok(svc) = app.service::<TeslaService>() {
        svc.reconfigure(proxy.and_then(tesla::Proxy::parse), svc.cadence());
    }
}

/// The notice an account in trouble gives (none while it is fine or paused).
pub fn notice(a: &Account) -> Option<Notice> {
    let detail = Some(json!({ "account": a.id }));
    let since_ms = a.last_error.as_ref().map(|e| e.at_ms);
    match a.state {
        AccountState::SignInNeeded => Some(Notice {
            code: "tesla_sign_in_needed",
            level: "warning",
            message: format!(
                "Tesla stopped taking the sign-in of the Tesla account \"{}\". Its cars are not read until you sign \
                 in again (Settings, Tesla account).",
                a.label
            ),
            since_ms,
            fix_link: Some("/settings/integrations#tesla"),
            detail,
        }),
        AccountState::Error => Some(Notice {
            code: "tesla_account_error",
            level: "warning",
            message: format!(
                "The Tesla account \"{}\" is not read: {}.",
                a.label,
                a.last_error.as_ref().map_or("Tesla refuses it", |e| e.message.as_str())
            ),
            since_ms,
            fix_link: Some("/settings/integrations#tesla"),
            detail,
        }),
        _ => None,
    }
}

/// The notices of every Tesla account in trouble (for admins).
pub fn notices(app: &AppState) -> Vec<Notice> {
    app.service::<TeslaService>().map(|s| s.accounts().iter().filter_map(notice).collect()).unwrap_or_default()
}

/// Whether a vehicle of an account is matched to `car` (it is read for that car), for the car's data sources.
pub fn feeds_car(app: &AppState, car: model::CarId) -> bool {
    app.service::<TeslaService>()
        .is_ok_and(|s| s.accounts().iter().flat_map(|a| &a.cars).any(|c| c.matched == Some(Match::Car { car })))
}

/// How many Tesla accounts there are (`/setup/status`), 0 without the input.
pub fn account_count(app: &AppState) -> usize {
    app.service::<TeslaService>().map(|s| s.accounts().len()).unwrap_or(0)
}

/// The Tesla part of `/system/health`: accounts, matched cars, requests today, the last problem.
pub fn health(app: &AppState) -> Value {
    let Ok(svc) = app.service::<TeslaService>() else {
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

/// The Tesla rows of `/network`: one per Tesla host the accounts use (`enabled` while one is read), or one row for
/// the global API host, off, when there is no account.
pub fn network_rows(app: &AppState) -> Vec<Value> {
    let Ok(svc) = app.service::<TeslaService>() else { return Vec::new() };
    let accounts = svc.accounts();
    if accounts.is_empty() {
        return vec![
            json!({ "purpose": "tesla", "host": tesla::hosts::GLOBAL_HOSTS[1], "enabled": false, "last_used_ms": null }),
        ];
    }
    let reading = accounts.iter().any(|a| !a.paused && svc.running(&a.id));
    let last = accounts.iter().filter_map(|a| a.last_ok_ms).max();
    svc.hosts()
        .into_iter()
        .map(|h| json!({ "purpose": "tesla", "host": h, "enabled": reading, "last_used_ms": last }))
        .collect()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/integrations/tesla", get(overview).patch(change_settings))
        .route("/api/v1/integrations/tesla/accounts", get(list).post(add))
        .route("/api/v1/integrations/tesla/sign-in", post(sign_in_start))
        .route("/api/v1/integrations/tesla/sign-in/{sign_in}", post(sign_in_finish))
        .route("/api/v1/integrations/tesla/accounts/{tesla_account}", get(one).patch(change).delete(remove))
        .route("/api/v1/integrations/tesla/accounts/{tesla_account}/cars", get(cars).put(put_cars))
        .route("/api/v1/integrations/tesla/status", get(status))
}

fn svc(app: &AppState) -> ApiResult<Arc<TeslaService>> {
    app.service::<TeslaService>()
}

/// A [`ServiceError`] as the API says it. Never a token or what Tesla answered: the codes and plain words only.
fn failed(svc: &TeslaService, id: Option<&str>, e: ServiceError) -> ApiError {
    match e.code() {
        "no_such_tesla_account" => ApiError::not_found("no_such_tesla_account"),
        "no_such_sign_in" => {
            ApiError::new(404, "no_such_sign_in", "That sign-in ran out or was used: start a new one.")
        }
        "no_such_vehicle" => ApiError::not_found("no_such_vehicle"),
        "too_many_sign_ins" => ApiError::conflict(
            "too_many_sign_ins",
            "Too many sign-ins are open: finish one, or wait ten minutes for them to run out.",
        ),
        "stale" => ApiError::stale_version(id.and_then(|i| svc.account(i).ok()).map_or(0, |a| a.version)),
        "account_exists" => {
            ApiError::conflict("account_exists", "The cars of this Tesla account are already on another one here.")
        }
        "car_taken" => ApiError::conflict("car_taken", "That car is already matched to another Tesla vehicle."),
        "invalid" => ApiError::invalid("pasted_url", &capitalized(&e.to_string())),
        "tesla_refused" => ApiError::new(
            422,
            "tesla_refused",
            "Tesla refused the sign-in or the token (it may have run out, been used, or the password changed). Sign \
             in again.",
        ),
        "tesla_unreachable" => ApiError::new(
            502,
            "tesla_unreachable",
            "Tesla could not be reached, or answered with an error. Try again in a few minutes.",
        ),
        _ => ApiError::server(format!("tesla: {e}")),
    }
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => format!("{}{}.", f.to_uppercase(), c.as_str().trim_end_matches('.')),
        None => String::new(),
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
    if a.paused && a.state != AccountState::SignInNeeded {
        return "paused";
    }
    match a.state {
        AccountState::Checking => "checking",
        AccountState::Ok => "ok",
        AccountState::Paused => "paused",
        AccountState::SignInNeeded => "sign_in_needed",
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

/// The newest problem of any account.
fn last_error(accounts: &[Account]) -> Value {
    accounts.iter().filter_map(|a| a.last_error.as_ref().map(|e| (a, e))).max_by_key(|(_, e)| e.at_ms).map_or(
        Value::Null,
        |(a, e)| json!({ "account": a.id, "code": e.code, "message": e.message, "at_ms": e.at_ms }),
    )
}

/// One vehicle of an account, with what the poller does for it and the car it most likely is (when not matched).
fn car_json(e: &Engine, c: &tesla::AccountCar, st: Option<&AccountStatus>) -> Value {
    let poll = st.and_then(|s| s.cars.iter().find(|p| p.vehicle == c.vehicle));
    let suggested = if c.matched.is_none() {
        c.vin_prefix.as_deref().and_then(|p| e.suggest_car(p).ok().flatten()).map(ids::car)
    } else {
        None
    };
    json!({
        "vehicle": c.vehicle,
        "vin_prefix": c.vin_prefix,
        "model": c.model.map(|m| format!("Model {m}")),
        "year": c.year,
        "display_name": c.display_name,
        "match": match_json(c.matched),
        "suggested": suggested,
        "state": poll.map_or(json!("unknown"), |p| word(&p.car_state)),
        "mode": poll.map_or(json!("off"), |p| word(&p.mode)),
    })
}

/// An account as the API writes it (no token: `Account` holds none).
fn account_json(e: &Engine, svc: &TeslaService, a: &Account) -> Value {
    let st = svc.status().into_iter().find(|s| s.account == a.id);
    let cars: Vec<Value> = a.cars.iter().map(|c| car_json(e, c, st.as_ref())).collect();
    json!({
        "id": a.id,
        "label": a.label,
        "region": a.region.word(),
        "redirect": word(&a.redirect),
        "hosts": tesla::Endpoints::for_region(a.region).hosts(),
        "added_ms": a.added_ms,
        "status": status_word(a),
        "paused": a.paused,
        "location": word(&a.location),
        "running": svc.running(&a.id),
        "last_ok_ms": a.last_ok_ms,
        "last_error": a.last_error.as_ref().map(|e| json!({ "code": e.code, "message": e.message, "at_ms": e.at_ms })),
        "polls_today": st.as_ref().map_or(0, |s| s.polls_today),
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

fn region_of(body: &JsonBody) -> ApiResult<Region> {
    match body.str("region")? {
        None => Ok(Region::Global),
        Some(r) => Region::parse(r).ok_or_else(|| ApiError::invalid("region", "Must be global or china.")),
    }
}

fn token_of(body: &JsonBody, key: &str) -> ApiResult<Option<tesla::Secret>> {
    let Some(t) = body.str(key)? else { return Ok(None) };
    let t = t.trim();
    if t.is_empty() || t.len() > 8192 || !t.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ApiError::invalid(key, "Not a Tesla refresh token."));
    }
    Ok(Some(tesla::Secret::new(t)))
}

// ---------------------------------------------------------------- the input as a whole

async fn overview(State(app): State<AppState>, Admin(_): Admin) -> ApiResult<Json<Value>> {
    let s = svc(&app)?;
    let e = app.service::<Engine>()?;
    app.blocking(move |_| {
        let accounts = s.accounts();
        let status = s.status();
        let settings = load_settings(&e);
        let c = s.cadence();
        Ok(Json(json!({
            "enabled": !accounts.is_empty(),
            "accounts": accounts.len(),
            "cars": matched_cars(&accounts),
            "hosts": s.hosts(),
            "poll": settings.poll.word(),
            "polls": {
                "list_s": c.list_ms / 1000,
                "parked_s": c.idle_ms / 1000,
                "charging_s": c.charging_ms / 1000,
                "driving_s": c.driving_ms / 1000,
                "driving_streaming_s": c.driving_streaming_ms / 1000,
                "smo_live_s": c.smo_live_ms / 1000,
                "daily_cap": c.daily_cap,
            },
            "polls_today": status.iter().map(|s| s.polls_today).sum::<u64>(),
            "last_error": last_error(&accounts),
            "clients_built": s.clients_built(),
            "version": settings.version,
        })))
    })
    .await
}

/// `PATCH /integrations/tesla` `{ version, poll }`: how often the server asks.
async fn change_settings(
    State(app): State<AppState>,
    Admin(v): Admin,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["version", "poll", "confirm_password", "code"])?;
    let version = body.version()?;
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
            if new != old {
                new.version = old.version + 1;
                let value = serde_json::to_value(&new).map_err(ApiError::server)?;
                e.db().set_setting(SETTING, value).map_err(|err| ApiError::server(format!("tesla settings: {err}")))?;
                s.reconfigure(proxy(&e), new.poll.cadence());
            }
            Ok((old, new))
        })
        .await?;
    if new != old {
        let summary = format!("Set how often the Tesla API is asked to {}.", new.poll.word());
        v.audit(&app, "tesla-poll-changed", Some(&target("settings", None, "tesla")), &summary).await?;
    } else {
        v.mark_audited();
    }
    overview(State(app), Admin(v)).await
}

async fn status(State(app): State<AppState>, Admin(_): Admin) -> ApiResult<Json<Value>> {
    let s = svc(&app)?;
    let mut items = Vec::new();
    for a in s.status() {
        for c in &a.cars {
            items.push(json!({
                "car": c.car.map(ids::car),
                "account": a.account,
                "vehicle": c.vehicle,
                "mode": word(&c.mode),
                "car_state": word(&c.car_state),
                "last_poll_ms": c.last_poll_ms,
                "next_poll_ms": c.next_poll_ms,
                "last_data_ms": c.last_data_ms,
                "polls_today": a.polls_today,
                "streaming": c.streaming,
                "last_error": c.last_error.as_ref().map(|e| json!({ "code": e.code, "message": e.message, "at_ms": e.at_ms })),
            }));
        }
    }
    Ok(Json(json!({ "items": items, "next": null })))
}

// ---------------------------------------------------------------- accounts

async fn list(State(app): State<AppState>, Admin(_): Admin) -> ApiResult<Json<Value>> {
    let s = svc(&app)?;
    let e = app.service::<Engine>()?;
    app.blocking(move |_| {
        let items: Vec<Value> = s.accounts().iter().map(|a| account_json(&e, &s, a)).collect();
        Ok(Json(json!({ "items": items, "next": null })))
    })
    .await
}

async fn one(State(app): State<AppState>, Admin(_): Admin, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    account_answer(&app, id).await.map(Json)
}

fn created(v: Value) -> Response {
    (StatusCode::CREATED, Json(v)).into_response()
}

async fn added(app: &AppState, v: &Viewer, a: &Account, how: &str) -> ApiResult<Response> {
    let n = a.cars.len();
    let summary = format!("Added the Tesla account {} {how} ({n} car{}).", a.label, if n == 1 { "" } else { "s" });
    v.audit(app, "tesla-account-added", Some(&target("tesla", None, &a.id)), &summary).await?;
    Ok(created(account_answer(app, a.id.clone()).await?))
}

/// `POST /integrations/tesla/accounts` `{ label, region, refresh_token }`: checked with Tesla (one refresh, one
/// vehicle list) before anything is kept.
async fn add(State(app): State<AppState>, Admin(v): Admin, headers: HeaderMap, body: JsonBody) -> ApiResult<Response> {
    body.only(&["label", "region", "refresh_token", "confirm_password", "code"])?;
    let label = label_of(&body)?.unwrap_or_else(|| "Tesla account".into());
    let region = region_of(&body)?;
    let token = token_of(&body, "refresh_token")?.ok_or_else(|| ApiError::bad_input("refresh_token", "Required."))?;
    reconfirm(&app, &v, &body, &headers).await?;
    let s = svc(&app)?;
    let a = s.add_with_refresh_token(&label, region, token, app.now()).await.map_err(|e| failed(&s, None, e))?;
    added(&app, &v, &a, "with a refresh token").await
}

/// `POST /integrations/tesla/sign-in` `{ region, redirect? }`: the address of Tesla's sign-in page. Nothing is
/// stored and nothing is sent anywhere: the address is only built.
async fn sign_in_start(State(app): State<AppState>, Admin(v): Admin, body: JsonBody) -> ApiResult<Response> {
    body.only(&["region", "redirect"])?;
    let region = region_of(&body)?;
    let redirect = match body.str("redirect")? {
        None | Some("app") => tesla::Redirect::App,
        Some("void") => tesla::Redirect::Void,
        Some(_) => return Err(ApiError::invalid("redirect", "Must be app or void.")),
    };
    let s = svc(&app)?;
    let started = s.sign_in_start(region, redirect, app.now()).map_err(|e| failed(&s, None, e))?;
    // changes nothing kept: no audit entry
    v.mark_audited();
    Ok(created(json!({ "sign_in": started.id, "url": started.url, "expires_ms": started.expires_ms })))
}

/// `POST /integrations/tesla/sign-in/{sign_in}` `{ pasted_url, label }`: the address Tesla's page ended on.
async fn sign_in_finish(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Response> {
    body.only(&["pasted_url", "label", "confirm_password", "code"])?;
    let pasted = body.req_str("pasted_url")?.to_string();
    if pasted.len() > 8192 {
        return Err(ApiError::invalid("pasted_url", "Too long for Tesla's callback address."));
    }
    let label = label_of(&body)?.unwrap_or_else(|| "Tesla account".into());
    reconfirm(&app, &v, &body, &headers).await?;
    let s = svc(&app)?;
    let pasted = zeroize::Zeroizing::new(pasted);
    let a = s.sign_in_finish(&id, &pasted, &label, app.now()).await.map_err(|e| failed(&s, None, e))?;
    added(&app, &v, &a, "by signing in at Tesla").await
}

/// `PATCH /integrations/tesla/accounts/{account}` `{ version, label?, paused?, location?, refresh_token? }`.
async fn change(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["version", "label", "paused", "location", "refresh_token", "confirm_password", "code"])?;
    let version = body.version()?;
    let label = label_of(&body)?;
    let paused = body.bool("paused")?;
    let location = match body.str("location")? {
        None => None,
        Some(w) => Some(
            serde_json::from_value::<tesla::Location>(json!(w))
                .map_err(|_| ApiError::invalid("location", "Must be driving_or_charging, always or never."))?,
        ),
    };
    let token = token_of(&body, "refresh_token")?;
    let s = svc(&app)?;
    let before = {
        let s = s.clone();
        let id = id.clone();
        app.blocking(move |_| s.account(&id).map_err(|e| failed(&s, Some(&id), e))).await?
    };
    reconfirm(&app, &v, &body, &headers).await?;
    let change = tesla::Change { label: label.clone(), paused, location };
    let after = {
        let (s, id) = (s.clone(), id.clone());
        app.blocking(move |_| s.update(&id, version, change).map_err(|e| failed(&s, Some(&id), e))).await?
    };
    let target = target("tesla", None, &id);
    let mut audited = false;
    if let Some(p) = paused
        && p != before.paused
    {
        let (what, summary) = if p {
            ("tesla-account-paused", format!("Paused the Tesla account {}: its cars are not read.", after.label))
        } else {
            ("tesla-account-resumed", format!("Resumed the Tesla account {}.", after.label))
        };
        v.audit(&app, what, Some(&target), &summary).await?;
        audited = true;
    }
    let mut what = Vec::new();
    if label.as_deref().is_some_and(|l| l != before.label) {
        what.push(format!("renamed it from {}", before.label));
    }
    if location.is_some_and(|l| l != before.location) {
        what.push(format!("set when its cars' location is read to {}", word(&after.location).as_str().unwrap_or("")));
    }
    if !what.is_empty() {
        let summary = format!("Changed the Tesla account {}: {}.", after.label, what.join(", "));
        v.audit(&app, "tesla-account-changed", Some(&target), &summary).await?;
        audited = true;
    }
    if let Some(t) = token {
        let a = s.replace_token(&id, after.version, t, app.now()).await.map_err(|e| failed(&s, Some(&id), e))?;
        let summary = format!("Gave the Tesla account {} a new refresh token (checked with Tesla).", a.label);
        v.audit(&app, "tesla-token-replaced", Some(&target), &summary).await?;
        audited = true;
    }
    if !audited {
        v.mark_audited();
    }
    account_answer(&app, id).await.map(Json)
}

/// `DELETE /integrations/tesla/accounts/{account}`: polling stops at once and the sealed file (with the tokens) is
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
        "Removed the Tesla account {}: its tokens are deleted here; the history already stored stays.",
        a.label
    );
    v.audit(&app, "tesla-account-removed", Some(&target("tesla", None, &id)), &summary).await?;
    Ok(Json(json!({ "deleted": true, "id": id })))
}

// ---------------------------------------------------------------- the cars of an account

async fn cars(State(app): State<AppState>, Admin(_): Admin, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let a = account_answer(&app, id).await?;
    Ok(Json(json!({ "items": a["cars"].clone(), "next": null, "version": a["version"].clone() })))
}

/// `PUT /integrations/tesla/accounts/{account}/cars` `{ version, items: [{ vehicle, match }] }`: `match` is a car
/// (`c3`), `"new"` (a new car is made for it), `"leave_out"` (never read) or `null` (not decided: not read). Only a
/// matched car is read.
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
    for (i, item) in items.iter().enumerate() {
        let Value::Object(m) = item else {
            return Err(ApiError::bad_input(&format!("items.{i}"), "Must be { vehicle, match }."));
        };
        if let Some(k) = m.keys().find(|k| *k != "vehicle" && *k != "match") {
            return Err(ApiError::bad_input(&format!("items.{i}.{k}"), "Unknown field."));
        }
        let Some(vehicle) = m.get("vehicle").and_then(Value::as_str) else {
            return Err(ApiError::bad_input(&format!("items.{i}.vehicle"), "Required: the vehicle id (tv1)."));
        };
        if !before.cars.iter().any(|c| c.vehicle == vehicle) {
            bad.add(&format!("items.{i}.vehicle"), "No such vehicle on this account.");
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
                    return Err(ApiError::bad_input(
                        &format!("items.{i}.match"),
                        "Must be a car id, \"new\", \"leave_out\" or null.",
                    ));
                };
                // the same check as a car read with `JsonBody::car`: a hidden car and an unknown one alike
                if !v.sees_existing(&app, car) {
                    bad.add(&format!("items.{i}.match"), "No such car.");
                    continue;
                }
                Some(Match::Car { car })
            }
            Some(_) => {
                return Err(ApiError::bad_input(
                    &format!("items.{i}.match"),
                    "Must be a car id, \"new\", \"leave_out\" or null.",
                ));
            }
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
        let (car, what) = match m {
            Some(Match::Car { car }) => (Some(*car), format!("is car {}: it is read from now on", ids::car(*car))),
            Some(Match::New) => (None, "gets a new car of its own: it is read from now on".to_string()),
            Some(Match::LeaveOut) => (None, "is left out: it is not read".to_string()),
            None => (None, "is not matched to a car: it is not read".to_string()),
        };
        let t = target("tesla", car, &format!("{}:{vehicle}", after.id));
        let summary = format!("Tesla vehicle {name} of the Tesla account {} {what}.", after.label);
        v.audit(&app, "tesla-car-matched", Some(&t), &summary).await?;
        audited = true;
    }
    if !audited {
        v.mark_audited();
    }
    cars(State(app), Admin(v), Path(id)).await
}
