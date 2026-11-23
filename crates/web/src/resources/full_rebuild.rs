//! The full rebuild (docs/ui-api.md, "Server, settings and health"), over [`engine::full`]: everything the server
//! worked out for a car (or every car) made again from the raw inputs it keeps, beside the history in use.
//!
//! | Route | |
//! |---|---|
//! | `GET /cars/{car}/full-rebuild` | admin: the status, with `this_car` and `car_stale` |
//! | `POST /cars/{car}/full-rebuild` | admin, `{ confirm_password, code }` -> `202` the status |
//! | `GET /system/full-rebuild` | admin: the status |
//! | `POST /system/full-rebuild` | admin, `{ confirm_password, code }`: every car -> `202` |
//! | `DELETE /system/full-rebuild` | admin: cancels the one under way |
//!
//! A start asks for the admin's own password again (as an account change does: `403 password_needed`, `403
//! wrong_password`, `401 wrong_code`; every request is over TLS, the policy refuses anything else). The audit log has
//! its start (`car-full-rebuild`, by the admin) here; its end, its failure or its cancel is written by [`spawn_watch`]
//! once the engine's record says it ended (after a restart too). The watch also starts the full rebuild a new
//! server version calls for (the cars whose history was made with another decoding) while the server setting
//! `full_rebuild.auto` is on, once per version: one that failed is offered again by the notice, not retried.

use crate::accounts_admin::{confirm_code, confirm_password};
use crate::ctx::{Admin, target};
use crate::resources::notices::Notice;
use crate::{ApiError, ApiResult, AppState, CarParam, JsonBody, ids};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use engine::Engine;
use engine::full::{FullRecord, FullStatus, Phase, Reason, StartError};
use model::CarId;
use serde_json::{Value, json};
use std::time::Duration;

/// How often the watch looks at the engine's record.
pub const WATCH_EVERY: Duration = Duration::from_secs(2);
/// Where the notices send the admin.
const LINK: &str = "/settings/server#full-rebuild";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/cars/{car}/full-rebuild", get(car_status).post(car_start))
        .route("/api/v1/system/full-rebuild", get(system_status).post(system_start).delete(cancel))
}

fn phase_word(p: Phase) -> &'static str {
    match p {
        Phase::Queued => "queued",
        Phase::Copying => "copying",
        Phase::Decoding => "decoding",
        Phase::Days => "days",
        Phase::Switching => "switching",
    }
}

fn state_word(s: FullStatus) -> &'static str {
    match s {
        FullStatus::Running => "running",
        FullStatus::Done => "done",
        FullStatus::Failed => "failed",
        FullStatus::Cancelled => "cancelled",
    }
}

fn reason_word(r: Reason) -> &'static str {
    match r {
        Reason::Owner => "owner",
        Reason::Version => "version",
    }
}

/// The status as the API answers it (`None`: none ever ran).
pub fn status_json(e: &Engine, r: Option<&FullRecord>, now_ms: u64) -> Value {
    let stale: Vec<String> = e.version_stale().into_iter().map(ids::car).collect();
    let version =
        json!({ "server": engine::full::derived_version(), "stale_cars": stale, "auto": e.full_rebuild_auto() });
    let Some(r) = r else {
        return json!({
            "state": "idle", "id": null, "scope": null, "cars": [], "car": null, "cars_done": 0, "cars_total": 0,
            "reason": null, "phase": null, "logs": { "done": 0, "total": 0 }, "days": { "done": 0, "total": 0 },
            "bytes": { "done": 0, "total": 0, "copied": 0 }, "percent": null, "rate_bytes_per_s": null,
            "rate_logs_per_s": null, "rate_days_per_s": null, "eta_ms": null,
            "started_ms": null, "finished_ms": null, "last_progress_ms": null, "stalled": false, "errors": [],
            "error": null, "by": null, "version": version,
        });
    };
    let running = r.running();
    let rates = if running { r.rates(now_ms) } else { engine::full::Rates::default() };
    let errors: Vec<Value> = r
        .errors
        .iter()
        .map(|x| json!({ "car": x.car.map(ids::car), "log": x.log, "message": x.message, "at_ms": x.at_ms }))
        .collect();
    json!({
        "state": state_word(r.status),
        "id": r.id.to_string(),
        "scope": if r.all { "all" } else { "car" },
        "cars": r.cars.iter().map(|c| ids::car(*c)).collect::<Vec<_>>(),
        "car": if running || r.status == FullStatus::Failed { r.car().map(ids::car) } else { None },
        "cars_done": r.index.min(r.cars.len()),
        "cars_total": r.cars.len(),
        "reason": reason_word(r.reason),
        "phase": if running { Some(phase_word(r.phase)) } else { None },
        "logs": { "done": r.logs_done, "total": r.logs_total },
        "days": { "done": r.days_done, "total": r.days_total },
        "bytes": { "done": r.bytes_done, "total": r.bytes_total, "copied": r.bytes_copied },
        "percent": percent(r),
        "rate_bytes_per_s": rates.bytes_per_s,
        "rate_logs_per_s": rates.logs_per_s,
        "rate_days_per_s": rates.days_per_s,
        "eta_ms": rates.eta_ms,
        "started_ms": r.started_ms,
        "finished_ms": r.finished_ms,
        "last_progress_ms": r.last_progress_ms,
        "stalled": r.stalled(now_ms),
        "errors": errors,
        "error": r.error,
        "by": r.by,
        "version": version,
    })
}

/// How far the phase under way is, 0..=100: the logs decoded of all while decoding, then the days worked out of
/// all; 100 once done or while switching; `None` before the first log (and when none ran).
pub fn percent(r: &FullRecord) -> Option<u64> {
    let of = |done: u64, total: u64| (total > 0).then(|| (100 * done.min(total)) / total);
    match (r.status, r.phase) {
        (FullStatus::Done, _) => Some(100),
        (FullStatus::Running, Phase::Decoding) => of(r.logs_done, r.logs_total),
        (FullStatus::Running, Phase::Days) => of(r.days_done, r.days_total),
        (FullStatus::Running, Phase::Switching) => Some(100),
        (FullStatus::Running, Phase::Queued | Phase::Copying) => Some(0),
        _ => None,
    }
}

async fn read(app: &AppState, car: Option<CarId>) -> ApiResult<Value> {
    let e = app.service::<Engine>()?;
    let now = app.now();
    app.blocking(move |_| {
        let r = e.full_rebuild_status().map_err(|x| ApiError::server(format!("the full rebuild's record: {x}")))?;
        let mut v = status_json(&e, r.as_ref(), now);
        if let Some(c) = car {
            v["this_car"] = json!(r.as_ref().is_some_and(|r| r.cars.contains(&c)));
            v["car_stale"] = json!(e.version_stale().contains(&c));
        }
        Ok(v)
    })
    .await
}

async fn car_status(State(app): State<AppState>, Admin(_): Admin, CarParam(car): CarParam) -> ApiResult<Json<Value>> {
    Ok(Json(read(&app, Some(car)).await?))
}

async fn system_status(State(app): State<AppState>, Admin(_): Admin) -> ApiResult<Json<Value>> {
    Ok(Json(read(&app, None).await?))
}

fn running_error(r: &FullRecord) -> ApiError {
    let what = if r.all { "every car".to_string() } else { format!("{} car", r.cars.len()) };
    ApiError::conflict(
        "full_rebuild_running",
        format!("A full rebuild of {what} is under way already: wait for it to end, or cancel it."),
    )
}

/// What a full rebuild is of, in words ("every car", "Comet").
fn of_words(app: &AppState, r: &FullRecord) -> String {
    match (r.all, r.cars.as_slice()) {
        (true, _) => "every car".into(),
        (false, [c]) => app.car_name(*c),
        (false, cars) => cars.iter().map(|c| app.car_name(*c)).collect::<Vec<_>>().join(", "),
    }
}

fn audit_target(r: &FullRecord) -> Option<String> {
    match (r.all, r.cars.as_slice()) {
        (false, [c]) => Some(target("car", Some(*c), "")),
        _ => None,
    }
}

async fn start(
    app: &AppState,
    v: &crate::Viewer,
    car: Option<CarId>,
    headers: &HeaderMap,
    body: &JsonBody,
) -> ApiResult<Response> {
    body.only(&["confirm_password", "code"])?;
    let e = app.service::<Engine>()?;
    // one at a time: said before the password is asked (nothing is changed either way)
    let e2 = e.clone();
    if let Some(r) =
        app.blocking(move |_| e2.full_rebuild_status().map_err(|x| ApiError::server(x.to_string()))).await?
        && r.running()
    {
        return Err(running_error(&r));
    }
    let password = confirm_password(Some(body), headers)?;
    let code = confirm_code(Some(body), headers)?;
    let v2 = v.clone();
    app.blocking(move |app| Ok(app.accounts.confirm(v2.signed_in(), &password, code.as_deref())?)).await?;
    let by = v.name().to_string();
    let e2 = e.clone();
    let r = app
        .blocking(move |_| match e2.full_rebuild(car, Reason::Owner, Some(by)) {
            Ok(r) => Ok(r),
            Err(StartError::Running(r)) => Err(running_error(&r)),
            Err(StartError::NoSuchCar) => Err(ApiError::not_found("no_such_car")),
            Err(StartError::Engine(x)) => Err(ApiError::server(format!("starting the full rebuild: {x}"))),
        })
        .await?;
    let summary = format!(
        "Started a full rebuild of {}: everything worked out made again from the stored data, beside the history in use.",
        of_words(app, &r)
    );
    match audit_target(&r) {
        Some(t) => v.audit(app, "car-full-rebuild", Some(&t), &summary).await?,
        // every car: the entry names no one car
        None => {
            v.mark_audited();
            let (who, ip) = (v.name().to_string(), v.client().address);
            let details = crate::ctx::audit_details(None, &summary);
            app.blocking(move |app| {
                Ok(app.accounts.audit().record(Some(&who), Some(ip), "car-full-rebuild", &details)?)
            })
            .await?;
        }
    }
    let mut out = read(app, car).await?;
    out["accepted"] = json!(true);
    Ok((StatusCode::ACCEPTED, Json(out)).into_response())
}

async fn car_start(
    State(app): State<AppState>,
    Admin(v): Admin,
    CarParam(car): CarParam,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Response> {
    start(&app, &v, Some(car), &headers, &body).await
}

async fn system_start(
    State(app): State<AppState>,
    Admin(v): Admin,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Response> {
    start(&app, &v, None, &headers, &body).await
}

async fn cancel(State(app): State<AppState>, Admin(v): Admin, body: JsonBody) -> ApiResult<Json<Value>> {
    body.only(&[])?;
    let e = app.service::<Engine>()?;
    let e2 = e.clone();
    let was = app.blocking(move |_| e2.cancel_full_rebuild().map_err(|x| ApiError::server(x.to_string()))).await?;
    if !was {
        return Err(ApiError::conflict("full_rebuild_not_running", "No full rebuild is under way."));
    }
    // (its end, cancelled, is written to the audit log by the watch once it stopped)
    v.mark_audited();
    Ok(Json(read(&app, None).await?))
}

/// `POST /cars/{car}/rebuild` while a full rebuild runs: refused.
pub(crate) fn refuse_while_running(e: &Engine) -> ApiResult<()> {
    match e.full_rebuild_status() {
        Ok(Some(r)) if r.running() => Err(ApiError::conflict(
            "full_rebuild_running",
            "A full rebuild is under way: it makes the whole history again. Wait for it to end.",
        )),
        _ => Ok(()),
    }
}

/// The admins' notices: one under way, one that failed (until the next one ends well), one a new version offers.
pub fn notices(app: &AppState) -> Vec<Notice> {
    let Ok(e) = app.service::<Engine>() else { return Vec::new() };
    let Ok(r) = e.full_rebuild_status() else { return Vec::new() };
    let stale = e.version_stale();
    match &r {
        Some(r) if r.running() => {
            let why = if r.reason == Reason::Version {
                " (this server decodes the stored logs differently from the one that made it)"
            } else {
                ""
            };
            return vec![Notice {
                code: "full_rebuild_running",
                level: "info",
                message: format!(
                    "A full rebuild of {} is under way{why}. Pages show the old history until it is done.",
                    of_words(app, r)
                ),
                since_ms: Some(r.started_ms),
                fix_link: Some(LINK),
                detail: None,
            }];
        }
        Some(r) if r.status == FullStatus::Failed => {
            return vec![Notice {
                code: "full_rebuild_failed",
                level: "warning",
                message: format!(
                    "The full rebuild of {} failed; the old history was kept. {}",
                    of_words(app, r),
                    r.error.as_deref().unwrap_or("")
                )
                .trim_end()
                .to_string(),
                since_ms: r.finished_ms,
                fix_link: Some(LINK),
                detail: None,
            }];
        }
        _ => {}
    }
    if stale.is_empty() {
        return Vec::new();
    }
    let names: Vec<String> = stale.iter().map(|c| app.car_name(*c)).collect();
    vec![Notice {
        code: "full_rebuild_offered",
        level: "warning",
        message: format!(
            "This server decodes the stored logs differently from the one that made the history of {}: a full rebuild \
             makes it again.",
            names.join(", ")
        ),
        since_ms: None,
        fix_link: Some(LINK),
        detail: Some(json!({ "cars": stale.iter().map(|c| ids::car(*c)).collect::<Vec<_>>() })),
    }]
}

/// One look: the end of a finished full rebuild into the audit log, and the one a new version calls for started.
pub fn watch_once(app: &AppState) {
    let Ok(e) = app.service::<Engine>() else { return };
    let Ok(r) = e.full_rebuild_status() else { return };
    if let Some(r) = &r
        && !r.running()
        && !r.audited_end
    {
        let took = r.finished_ms.map_or(0, |f| f.saturating_sub(r.started_ms));
        let mins = took.div_ceil(60_000);
        let of = of_words(app, r);
        let summary = match r.status {
            FullStatus::Done => format!(
                "Full rebuild of {of} done in {mins} min: {} logs decoded again{}.",
                r.logs_done,
                match r.errors.len() {
                    0 => String::new(),
                    1 => ", 1 problem".into(),
                    n => format!(", {n} problems"),
                }
            ),
            FullStatus::Failed => format!(
                "Full rebuild of {of} failed (the old history was kept): {}.",
                r.error.as_deref().unwrap_or("no reason given").trim_end_matches('.')
            ),
            FullStatus::Cancelled => {
                format!("Full rebuild of {of} cancelled (the cars not switched yet kept their history).")
            }
            FullStatus::Running => return,
        };
        let details = crate::ctx::audit_details(audit_target(r).as_deref(), &summary);
        match app.accounts.audit().record(None, None, "car-full-rebuild", &details) {
            Ok(()) => {
                let _ = e.full_rebuild_audited(r.id);
            }
            Err(err) => tracing::warn!("audit: {err}"),
        }
    }
    // a new version's: once per version (one that failed or was cancelled is offered, not run again)
    let stale = e.version_stale();
    let tried = r
        .as_ref()
        .is_some_and(|r| r.running() || (r.reason == Reason::Version && r.version == engine::full::derived_version()));
    if stale.is_empty() || tried || !e.full_rebuild_auto() {
        return;
    }
    let all = engine::query::all_cars(&e).unwrap_or_default();
    let every = stale.len() == all.len();
    let res = if every || stale.len() > 1 {
        e.full_rebuild_of(stale.clone(), every, Reason::Version, None)
    } else {
        e.full_rebuild(Some(stale[0]), Reason::Version, None)
    };
    match res {
        Ok(r) => {
            let summary = format!(
                "Started a full rebuild of {}: this server decodes the stored logs differently from the one that \
                 made their history.",
                of_words(app, &r)
            );
            let details = crate::ctx::audit_details(audit_target(&r).as_deref(), &summary);
            if let Err(err) = app.accounts.audit().record(None, None, "car-full-rebuild", &details) {
                tracing::warn!("audit: {err}");
            }
            tracing::info!("full rebuild of {} started: the server's decoding changed", of_words(app, &r));
        }
        Err(StartError::Running(_)) => {}
        Err(err) => tracing::warn!("the full rebuild a new version calls for did not start: {err}"),
    }
}

/// Looks at the full rebuild every [`WATCH_EVERY`] (see [`watch_once`]), off the async threads.
pub fn spawn_watch(app: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let a = app.clone();
            let _ = tokio::task::spawn_blocking(move || watch_once(&a)).await;
            tokio::time::sleep(WATCH_EVERY).await;
        }
    })
}
