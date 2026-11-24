//! The Tesla API input, for admins: `/api/v1/integrations/tesla` (docs/ui-api.md, "Integrations: Tesla").
//!
//! | Route | |
//! |---|---|
//! | `GET`/`PATCH /integrations/tesla` | `{ enabled, accounts, cars, hosts, poll, poll_limits, polls_today, rate_limited_until_ms, last_error, clients_built, version }`; PATCH `{ version, poll: { driving_ms?, charging_ms?, idle_ms?, list_ms? } }` |
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
use crate::{ApiError, ApiResult, AppState, Fields, Params, Viewer, ids};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use engine::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tesla::{Account, AccountState, AccountStatus, Intervals, Match, Region, ServiceError};

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
    /// The four reads and the daily cap. `None` in settings saved before they were kept: read from `poll`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intervals: Option<Intervals>,
    /// The pace word settings saved before the intervals held (`often`, `normal`, `gentle`); read, never written.
    #[serde(skip_serializing)]
    pub poll: Option<String>,
    /// Supercharger invoices (`/integrations/invoices`): off until an admin turns them on.
    pub invoices: bool,
    pub version: u64,
}

impl TeslaSettings {
    /// How often it asks: the saved intervals, else those of an older saved pace (`gentle` kept slower, any other
    /// the defaults), within the floors.
    pub fn intervals(&self) -> Intervals {
        if let Some(i) = self.intervals {
            return i.clamped();
        }
        match self.poll.as_deref() {
            Some("gentle") => {
                Intervals { driving_ms: 60_000, charging_ms: 120_000, idle_ms: 5 * 60_000, list_ms: 120_000 }
            }
            _ => Intervals::DEFAULT,
        }
    }

    /// The pollers' cadence: the intervals, and whether they read invoices.
    pub fn cadence(&self) -> tesla::Cadence {
        let mut c = self.intervals().cadence();
        c.invoices = self.invoices;
        c
    }
}

impl Default for TeslaSettings {
    fn default() -> TeslaSettings {
        TeslaSettings { intervals: None, poll: None, invoices: false, version: 1 }
    }
}

/// The saved settings (the defaults when none are saved or they don't read).
pub fn load_settings(e: &Engine) -> TeslaSettings {
    e.db().setting(SETTING).ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// A time in words, UTC to the minute: `2026-11-24 14:05 UTC`.
fn utc_minute(ms: u64) -> String {
    let day = engine::tz::date_str(ms, engine::tz::zone("UTC"));
    format!("{day} {:02}:{:02} UTC", (ms / 3_600_000) % 24, (ms / 60_000) % 60)
}

/// An interval in words, for the audit: `2.5 s`, `60 s`.
fn secs(ms: u64) -> String {
    if ms.is_multiple_of(1000) { format!("{} s", ms / 1000) } else { format!("{}.{} s", ms / 1000, (ms % 1000) / 100) }
}

/// The intervals in words, for the audit.
fn intervals_words(i: &Intervals) -> String {
    format!(
        "driving every {}, charging every {}, idle every {}, asleep (the list) every {}",
        secs(i.driving_ms),
        secs(i.charging_ms),
        secs(i.idle_ms),
        secs(i.list_ms)
    )
}

/// The `poll` of a PATCH over `old`: each field given replaces its value; `400 bad_input` for a wrong type or an
/// unknown field, `422 invalid` for a value under its floor or over its ceiling (named `poll.<field>`).
fn intervals_of(v: &Value, old: Intervals) -> ApiResult<Intervals> {
    let Value::Object(m) = v else { return Err(ApiError::bad_input("poll", "Must be an object.")) };
    let mut f = Fields::new();
    for k in m.keys() {
        if !matches!(k.as_str(), "driving_ms" | "charging_ms" | "idle_ms" | "list_ms") {
            f.add(&format!("poll.{k}"), "Unknown field.");
        }
    }
    f.bad_input()?;
    let get = |k: &str, was: u64| -> ApiResult<u64> {
        match m.get(k) {
            None | Some(Value::Null) => Ok(was),
            Some(x) => x.as_u64().ok_or_else(|| ApiError::bad_input(&format!("poll.{k}"), "Must be a whole number.")),
        }
    };
    let new = Intervals {
        driving_ms: get("driving_ms", old.driving_ms)?,
        charging_ms: get("charging_ms", old.charging_ms)?,
        idle_ms: get("idle_ms", old.idle_ms)?,
        list_ms: get("list_ms", old.list_ms)?,
    };
    let mut f = Fields::new();
    for (field, v, lo, hi) in new.fields() {
        if v < lo || v > hi {
            let why = format!("Must be from {} to {} (never faster than {}).", secs(lo), secs(hi), secs(lo));
            f.add(&format!("poll.{field}"), &why);
        }
    }
    f.invalid()?;
    Ok(new)
}

/// The floors, defaults and ceilings of `poll`, for the page.
fn poll_limits() -> Value {
    let mut m = serde_json::Map::new();
    for (k, default, min, max) in Intervals::DEFAULT.fields() {
        m.insert(k.to_string(), json!({ "min": min, "default": default, "max": max }));
    }
    Value::Object(m)
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
    o.cadence = load_settings(e).cadence();
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
    let (audit2, events2) = (audit.clone(), events.clone());
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
    let limited: engine::tesla_in::RateLimitHook = Arc::new(move |a: &Account, until_ms: u64| {
        let n = rate_notice(a, until_ms, None);
        let details = audit_details(Some(&target("tesla", None, &a.id)), &n.message);
        let audit = audit2.clone();
        let write = move || {
            if let Err(e) = audit.record(None, None, "tesla-rate-limited", &details) {
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
            events2.publish(Event::Notice { notice: Arc::new(v), admins_only: true });
        }
    });
    let sink = engine::TeslaSink { engine: engine.clone(), on_account: Some(hook), on_rate_limit: Some(limited) };
    tesla::Service::new(data, vault, Arc::new(sink), options(engine))
}

/// The settings changed: the pollers follow a new proxy (they start again with it).
pub fn follow_proxy(app: &AppState, proxy: Option<&str>) {
    if let Ok(svc) = app.service::<TeslaService>() {
        svc.reconfigure(proxy.and_then(tesla::Proxy::parse), svc.cadence());
    }
}

/// The notice of Tesla limiting an account's requests (it answered 429): nothing is asked before `until_ms`.
pub fn rate_notice(a: &Account, until_ms: u64, since_ms: Option<u64>) -> Notice {
    Notice {
        code: "tesla_rate_limited",
        level: "warning",
        message: format!(
            "Tesla is limiting requests for the Tesla account \"{}\", next try at {}. This server sets no limit of its \
             own; it waits as long as Tesla asks.",
            a.label,
            utc_minute(until_ms)
        ),
        since_ms,
        fix_link: Some("/settings/integrations#tesla"),
        detail: Some(json!({ "account": a.id, "until_ms": until_ms })),
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
    let Ok(s) = app.service::<TeslaService>() else { return Vec::new() };
    let status = s.status();
    let mut out: Vec<Notice> = Vec::new();
    for a in s.accounts() {
        out.extend(notice(&a));
        let st = status.iter().find(|x| x.account == a.id);
        if a.state == AccountState::Ok
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
        "rate_limited_until_ms": st.as_ref().and_then(|s| s.rate_limited_until_ms),
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
            "poll": Intervals::of(&c),
            "poll_limits": poll_limits(),
            "polls_today": status.iter().map(|s| s.polls_today).sum::<u64>(),
            "rate_limited_until_ms": status.iter().filter_map(|s| s.rate_limited_until_ms).max(),
            "last_error": last_error(&accounts),
            "clients_built": s.clients_built(),
            "invoices": super::invoices::tesla_block(&e, &s, settings.invoices),
            "version": settings.version,
        })))
    })
    .await
}

/// `PATCH /integrations/tesla` `{ version, poll: { driving_ms?, charging_ms?, idle_ms?, list_ms? } }`:
/// how often the server asks (each read never faster than its floor), and `invoices`.
async fn change_settings(
    State(app): State<AppState>,
    Admin(v): Admin,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["version", "poll", "invoices", "confirm_password", "code"])?;
    let version = body.version()?;
    let invoices = body.bool("invoices")?;
    let poll = match body.get("poll") {
        None | Some(Value::Null) => None,
        Some(p) => {
            // checked before the password is asked again (a wrong value changes nothing either way)
            intervals_of(p, Intervals::DEFAULT)?;
            Some(p.clone())
        }
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
            if let Some(p) = &poll {
                let i = intervals_of(p, old.intervals())?;
                if i != old.intervals() {
                    new.intervals = Some(i);
                    new.poll = None;
                }
            }
            if let Some(i) = invoices {
                new.invoices = i;
            }
            if new != old {
                new.version = old.version + 1;
                let value = serde_json::to_value(&new).map_err(ApiError::server)?;
                e.db().set_setting(SETTING, value).map_err(|err| ApiError::server(format!("tesla settings: {err}")))?;
                s.reconfigure(proxy(&e), new.cadence());
            }
            Ok((old, new))
        })
        .await?;
    if new.invoices != old.invoices {
        let summary = if new.invoices {
            "Turned Supercharger invoices from the Tesla API on."
        } else {
            "Turned Supercharger invoices from the Tesla API off."
        };
        v.audit(&app, "tesla-invoices-changed", Some(&target("settings", None, "tesla-invoices")), summary).await?;
    }
    if new.intervals() != old.intervals() {
        let summary = format!("Set how often the Tesla API is asked: {}.", intervals_words(&new.intervals()));
        v.audit(&app, "tesla-poll-changed", Some(&target("settings", None, "tesla")), &summary).await?;
    }
    if new == old {
        v.mark_audited();
    }
    overview(State(app), Admin(v)).await
}

async fn status(State(app): State<AppState>, Admin(_): Admin, q: Params) -> ApiResult<Response> {
    q.wants_csv()?;
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
                "rate_limited_until_ms": a.rate_limited_until_ms,
                "streaming": c.streaming,
                "last_error": c.last_error.as_ref().map(|e| json!({ "code": e.code, "message": e.message, "at_ms": e.at_ms })),
            }));
        }
    }
    crate::csv::answer(&q, "tesla-status", json!({ "items": items, "next": null }))
}

// ---------------------------------------------------------------- accounts

async fn list(State(app): State<AppState>, Admin(_): Admin, q: Params) -> ApiResult<Response> {
    q.wants_csv()?;
    let s = svc(&app)?;
    let e = app.service::<Engine>()?;
    app.blocking(move |_| {
        let items: Vec<Value> = s.accounts().iter().map(|a| account_json(&e, &s, a)).collect();
        crate::csv::answer(&q, "tesla-accounts", json!({ "items": items, "next": null }))
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

async fn cars(State(app): State<AppState>, Admin(_): Admin, Path(id): Path<String>, q: Params) -> ApiResult<Response> {
    q.wants_csv()?;
    crate::csv::answer(&q, "tesla-account-cars", cars_answer(&app, id).await?)
}

/// An account's cars as the list `GET .../cars` answers.
async fn cars_answer(app: &AppState, id: String) -> ApiResult<Value> {
    let a = account_answer(app, id).await?;
    Ok(json!({ "items": a["cars"].clone(), "next": null, "version": a["version"].clone() }))
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
        let car = match m {
            Some(Match::Car { car }) => Some(*car),
            _ => None,
        };
        let of = format!("of the Tesla account {}", after.label);
        let summary = match (m, car) {
            (_, Some(c)) => {
                let car_name = app.blocking(move |app| Ok(app.car_name(c))).await?;
                format!("Matched Tesla car {name} {of} to {car_name}: it is read from now on.")
            }
            (Some(Match::New), _) => {
                format!("Matched Tesla car {name} {of} to a new car of its own: it is read from now on.")
            }
            (Some(Match::LeaveOut), _) => format!("Left Tesla car {name} {of} out: it is not read."),
            _ => format!("Unmatched Tesla car {name} {of}: it is not read."),
        };
        let t = crate::ctx::target_of_car(&target("tesla", None, &format!("{}:{vehicle}", after.id)), car);
        v.audit(&app, "tesla-car-matched", Some(&t), &summary).await?;
        audited = true;
    }
    if !audited {
        v.mark_audited();
    }
    Ok(Json(cars_answer(&app, id).await?))
}

/// The Supercharger invoices block as `/integrations/tesla` gives it now (`None` without the Tesla API input).
pub(crate) fn invoices_now(app: &AppState) -> Option<Value> {
    let s = svc(app).ok()?;
    let e = app.service::<Engine>().ok()?;
    let settings = load_settings(&e);
    Some(super::invoices::tesla_block(&e, &s, settings.invoices))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_limit_notice_says_the_next_try() {
        let a = Account {
            id: "ta1".into(),
            label: "Family".into(),
            region: Region::Global,
            redirect: tesla::Redirect::App,
            added_ms: 0,
            version: 1,
            paused: false,
            location: tesla::Location::default(),
            state: AccountState::Ok,
            last_ok_ms: None,
            last_error: None,
            cars: Vec::new(),
        };
        // 2027-01-15 14:05:30 UTC
        let until = 1_800_021_930_000;
        let n = rate_notice(&a, until, Some(5));
        assert_eq!(n.code, "tesla_rate_limited");
        assert_eq!(n.level, "warning");
        assert!(
            n.message.starts_with("Tesla is limiting requests for the Tesla account \"Family\", next try at "),
            "{}",
            n.message
        );
        assert!(n.message.contains(&utc_minute(until)), "{}", n.message);
        assert_eq!(utc_minute(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc_minute(90_061_000), "1970-01-02 01:01 UTC");
        assert_eq!(n.detail, Some(json!({ "account": "ta1", "until_ms": until })));
        assert_eq!(n.since_ms, Some(5));
    }
}
