//! The full rebuild (docs/ui-api.md, "Server, settings and health"), over [`engine::full`]: everything the server
//! worked out for a car (or every car) made again from the raw inputs it keeps, beside the history in use.
//!
//! | Route | |
//! |---|---|
//! | `GET /cars/{car}/full-rebuild` | admin: the status, with `this_car` and `car_stale` |
//! | `POST /cars/{car}/full-rebuild` | admin, `{ confirm_password, code }` -> `202` the status |
//! | `GET /system/full-rebuild` | admin: the status |
//! | `POST /system/full-rebuild` | admin, `{ confirm_password, code }`: every car -> `202` |
//! | `PATCH /system/full-rebuild` | admin, `{ when: "now" \| "tonight" \| "pause" }`: when the one under way goes on |
//! | `DELETE /system/full-rebuild` | admin: cancels the one under way |
//!
//! A start asks for the admin's own password again (as an account change does: `403 password_needed`, `403
//! wrong_password`, `401 wrong_code`; every request is over TLS, the policy refuses anything else). The audit log has
//! its start (`car-full-rebuild`, by the admin) here; its end, its failure or its cancel is written by [`spawn_watch`]
//! once the engine's record says it ended (after a restart too). The watch also plans the full rebuild a new
//! server version calls for (the cars whose history was made with another decoding) while the server setting
//! `full_rebuild.auto` is on, once per version ([`engine::Engine::full_rebuild_auto_plan`]): it starts after a grace
//! of [`engine::full::AUTO_GRACE_MS`] unless an admin starts it now, puts it off until tonight or pauses it; one that
//! failed is offered again by the notice, not retried. A start while one runs joins it (its cars added): one run.
//!
//! Every status says why it runs (`causes`: each with its words and a `link` to the change explained, for a server
//! update), whether it waits (`hold`), and the last rebuilds of any kind with their causes and times (`history`).

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
use engine::full::{FullRecord, FullStatus, HoldKind, Phase, Reason, StartError, When};
use engine::versions::Cause;
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
        .route("/api/v1/system/full-rebuild", get(system_status).post(system_start).patch(when).delete(cancel))
}

/// The rebuilds' history answered with the status: at most this many.
pub const HISTORY_SHOWN: usize = 5;

/// A cause as the API answers it: its link a page can open (the change explained in the repository's docs).
pub fn cause_json(c: &Cause) -> Value {
    json!({
        "code": c.code,
        "text": c.text,
        "link": c.link.as_ref().map(|l| format!("https://github.com/{}/blob/main/{l}", release::REPO)),
    })
}

fn causes_json(v: &[Cause]) -> Vec<Value> {
    v.iter().map(cause_json).collect()
}

fn hold_word(k: HoldKind) -> &'static str {
    match k {
        HoldKind::Grace => "grace",
        HoldKind::Tonight => "tonight",
        HoldKind::Paused => "paused",
    }
}

/// The last rebuilds, newest first ([`engine::history`]).
pub fn history_json(e: &Engine) -> Vec<Value> {
    e.rebuild_history()
        .unwrap_or_default()
        .iter()
        .take(HISTORY_SHOWN)
        .map(|r| {
            json!({
                "kind": r.kind,
                "scope": if r.all { "all" } else { "car" },
                "cars": r.cars.iter().map(|c| ids::car(*c)).collect::<Vec<_>>(),
                "causes": causes_json(&r.causes),
                "state": r.state,
                "started_ms": r.started_ms,
                "finished_ms": r.finished_ms,
                "took_ms": r.took_ms(),
                "logs": r.logs,
            })
        })
        .collect()
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
            "reason": null, "phase": null, "logs": { "done": 0, "total": 0, "failed": 0 }, "days": { "done": 0, "total": 0 },
            "bytes": { "done": 0, "total": 0, "copied": 0 }, "percent": null, "rate_bytes_per_s": null,
            "rate_logs_per_s": null, "rate_days_per_s": null, "eta_ms": null, "speed": null,
            "started_ms": null, "finished_ms": null, "last_progress_ms": null, "stalled": false, "errors": [],
            "error": null, "by": null, "version": version, "causes": [], "hold": null, "cancelling": false, "history": history_json(e),
        });
    };
    // (one asked to stop is stopping, whatever held it: it says that and nothing else about waiting)
    let cancelling = r.running() && r.cancel;
    let hold = r
        .hold
        .filter(|_| r.held(now_ms) && !cancelling)
        .map(|h| json!({ "kind": hold_word(h.kind), "until_ms": h.until_ms }));
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
        "logs": { "done": r.logs_done, "total": r.logs_total, "failed": r.logs_failed },
        "days": { "done": r.days_done, "total": r.days_total },
        "bytes": { "done": r.bytes_done, "total": r.bytes_total, "copied": r.bytes_copied },
        "percent": percent(r),
        "rate_bytes_per_s": rates.bytes_per_s,
        "rate_logs_per_s": rates.logs_per_s,
        "rate_days_per_s": rates.days_per_s,
        "eta_ms": rates.eta_ms,
        "speed": if running && r.phase == Phase::Decoding { e.rebuild_speed() } else { None },
        "started_ms": r.started_ms,
        "finished_ms": r.finished_ms,
        "last_progress_ms": r.last_progress_ms,
        "stalled": r.stalled(now_ms),
        "errors": errors,
        "error": r.error,
        "by": r.by,
        "version": version,
        "causes": causes_json(&r.causes),
        "hold": hold,
        "cancelling": cancelling,
        "history": history_json(e),
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
    // (one under way takes the ask in: its cars added, one run)
    let e2 = e.clone();
    let was = app.blocking(move |_| e2.full_rebuild_status().map_err(|x| ApiError::server(x.to_string()))).await?;
    let joined = was.as_ref().is_some_and(FullRecord::running);
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
            Err(e @ StartError::Rezoning(_)) => Err(ApiError::conflict("redate_running", e.to_string())),
            Err(StartError::Engine(x)) => Err(ApiError::server(format!("starting the full rebuild: {x}"))),
        })
        .await?;
    let summary = if joined {
        format!("Asked for a full rebuild while one ran: it does {} in the same run.", of_words(app, &r))
    } else {
        format!(
            "Started a full rebuild of {}: everything worked out made again from the stored data, beside the history \
             in use.",
            of_words(app, &r)
        )
    };
    audit_run(app, v, &r, &summary).await?;
    // the open pages hear of it at once, even when it is over before the watcher looks again
    tell_pages(app, json!({ "code": "full_rebuild_running" }));
    let mut out = read(app, car).await?;
    out["accepted"] = json!(true);
    out["joined"] = json!(joined);
    Ok((StatusCode::ACCEPTED, Json(out)).into_response())
}

/// `PATCH /system/full-rebuild` `{ when }`: the one under way goes on now, waits until tonight
/// ([`engine::full::TONIGHT_HOUR`] o'clock in the server's time zone), or is paused until started again (its
/// progress kept). `409 full_rebuild_not_running` when none is under way.
async fn when(State(app): State<AppState>, Admin(v): Admin, body: JsonBody) -> ApiResult<Json<Value>> {
    body.only(&["when"])?;
    let word = body.get("when").and_then(Value::as_str).unwrap_or_default().to_string();
    let e = app.service::<Engine>()?;
    let now = app.now();
    let w = match word.as_str() {
        "now" => When::Now,
        "tonight" => When::Tonight(engine::full::tonight_ms(now, &e.default_time_zone())),
        "pause" => When::Pause,
        _ => return Err(ApiError::invalid("when", "One of now, tonight, pause.")),
    };
    let e2 = e.clone();
    let r = app.blocking(move |_| e2.full_rebuild_when(w).map_err(|x| ApiError::server(x.to_string()))).await?;
    let Some(r) = r else {
        return Err(ApiError::conflict("full_rebuild_not_running", "No full rebuild is under way."));
    };
    let of = of_words(&app, &r);
    let summary = match w {
        When::Now => format!("Started the full rebuild of {of} now."),
        When::Tonight(_) => format!("Put the full rebuild of {of} off until tonight."),
        When::Pause => format!("Paused the full rebuild of {of} (its progress is kept)."),
    };
    audit_run(&app, &v, &r, &summary).await?;
    Ok(Json(read(&app, None).await?))
}

/// The audit entry of an admin's change to a full rebuild (`car-full-rebuild`): on its car, or on none for every car.
async fn audit_run(app: &AppState, v: &crate::Viewer, r: &FullRecord, summary: &str) -> ApiResult<()> {
    match audit_target(r) {
        Some(t) => v.audit(app, "car-full-rebuild", Some(&t), summary).await,
        // every car: the entry names no one car
        None => {
            v.mark_audited();
            let (who, ip) = (v.name().to_string(), v.client().address);
            let details = crate::ctx::audit_details(None, summary);
            app.blocking(move |app| {
                Ok(app.accounts.audit().record(Some(&who), Some(ip), "car-full-rebuild", &details)?)
            })
            .await
        }
    }
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
    let now = app.now();
    match &r {
        Some(r) if r.held(now) => {
            let why = why_words(r);
            let when = match r.hold.map(|h| (h.kind, h.until_ms)) {
                Some((HoldKind::Paused, _)) => "It is paused until you start it".to_string(),
                Some((HoldKind::Tonight, _)) => "It waits for tonight".to_string(),
                Some((_, Some(u))) => format!("It starts in about {} min", u.saturating_sub(now).div_ceil(60_000)),
                _ => "It starts soon".to_string(),
            };
            return vec![Notice {
                code: "full_rebuild_planned",
                level: "info",
                message: format!(
                    "A full rebuild of {} is planned{why}. {when}: start it now, tonight, or pause it.",
                    of_words(app, r)
                ),
                since_ms: Some(r.started_ms),
                fix_link: Some(LINK),
                detail: Some(json!({ "causes": causes_json(&r.causes) })),
            }];
        }
        Some(r) if r.running() => {
            let why = why_words(r);
            return vec![Notice {
                code: "full_rebuild_running",
                level: "info",
                message: format!(
                    "A full rebuild of {} is under way{why}. Pages show the old history until it is done.",
                    of_words(app, r)
                ),
                since_ms: Some(r.started_ms),
                fix_link: Some(LINK),
                detail: Some(json!({ "causes": causes_json(&r.causes) })),
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
    let causes = e.stale_causes(&stale);
    let why = causes.first().map(|c| format!(" ({})", c.text)).unwrap_or_default();
    vec![Notice {
        code: "full_rebuild_offered",
        level: "warning",
        message: format!(
            "This server decodes the stored logs differently from the one that made the history of {}{why}: a full \
             rebuild makes it again.",
            names.join(", ")
        ),
        since_ms: None,
        fix_link: Some(LINK),
        detail: Some(json!({
            "cars": stale.iter().map(|c| ids::car(*c)).collect::<Vec<_>>(),
            "causes": causes_json(&causes),
        })),
    }]
}

/// Why a full rebuild runs, for a notice: " (server updated: positions are worked out differently, and 1 more)".
fn why_words(r: &FullRecord) -> String {
    match r.causes.as_slice() {
        [] => String::new(),
        [one] => format!(" ({})", lower_first(&one.text)),
        [one, rest @ ..] => format!(" ({}, and {} more)", lower_first(&one.text), rest.len()),
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_lowercase().chain(c).collect()).unwrap_or_default()
}

/// One look: the end of a finished full rebuild into the audit log, and the one a new version calls for started.
pub fn watch_once(app: &AppState) {
    let Ok(e) = app.service::<Engine>() else { return };
    // a repaired drives table's end (a rebuild of any kind, or an admin's repair, did it)
    if super::system::audit_repairs(app, &e) > 0 {
        // (the notice of the damage goes, and one of what could not be recovered may come)
        tell_pages(app, json!({ "code": "data_repaired" }));
    }
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
                r.logs_done.saturating_sub(r.logs_failed),
                // every log that could not be worked on (the list keeps the first ones only), and what else went
                // wrong
                match r.logs_failed + r.errors.iter().filter(|e| e.log.is_none()).count() as u64 {
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
                // its notice goes (or the failed one comes): the open pages read the notices again
                tell_pages(app, json!({ "code": "full_rebuild_ended" }));
            }
            Err(err) => tracing::warn!("audit: {err}"),
        }
    }
    // a wait that ended: the worker goes on
    e.full_rebuild_tick();
    // a new version's: planned once per version (one that failed or was cancelled is offered, not run again)
    match e.full_rebuild_auto_plan(e.full_rebuild_grace_ms()) {
        Ok(Some(r)) => {
            let why = r.causes.iter().map(|c| c.text.trim_end_matches('.')).collect::<Vec<_>>().join("; ");
            let when = match r.hold {
                Some(_) => format!(
                    "it starts in {} min unless an admin chooses otherwise",
                    e.full_rebuild_grace_ms().div_ceil(60_000)
                ),
                None => "it started at once".to_string(),
            };
            let summary = format!("Planned a full rebuild of {} ({why}): {when}.", of_words(app, &r));
            let details = crate::ctx::audit_details(audit_target(&r).as_deref(), &summary);
            if let Err(err) = app.accounts.audit().record(None, None, "car-full-rebuild", &details) {
                tracing::warn!("audit: {err}");
            }
            tracing::info!("full rebuild of {} planned: {why}", of_words(app, &r));
        }
        Ok(None) | Err(StartError::Running(_)) => {}
        // planned at the next look, once the re-dating is done
        Err(StartError::Rezoning(car)) => {
            tracing::info!("the full rebuild a new version calls for waits for the re-dating of car {car}'s data")
        }
        Err(err) => tracing::warn!("the full rebuild a new version calls for was not planned: {err}"),
    }
}

/// A `notice` event for the admins' open pages (they read the notices again).
fn tell_pages(app: &AppState, notice: Value) {
    app.events.publish(crate::events::Event::Notice { notice: std::sync::Arc::new(notice), admins_only: true });
}

/// Tells the open pages when the full rebuild's notices change (planned, under way, paused, failed, gone): they read
/// the notices again on a `notice` event, as for the other inputs' notices. Admins only, as the notices are.
pub fn announce_notices(app: &AppState, last: &mut Option<String>) {
    let now = notices(app);
    let key = serde_json::to_string(&now).unwrap_or_default();
    if last.as_deref() == Some(key.as_str()) {
        return;
    }
    let first = last.is_none();
    *last = Some(key);
    // at start nothing has changed for a page yet (it reads the notices when it opens)
    if first && now.is_empty() {
        return;
    }
    let notice = match now.first() {
        Some(n) => serde_json::to_value(n).unwrap_or(Value::Null),
        None => json!({ "code": "full_rebuild_ended" }),
    };
    tell_pages(app, notice);
}

/// Looks at the full rebuild every [`WATCH_EVERY`] (see [`watch_once`]), off the async threads.
pub fn spawn_watch(app: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut shown: Option<String> = None;
        loop {
            // a server whose data moved away starts no rebuild and writes no audit entry
            if crate::moving::data_moved(&app) {
                return;
            }
            let a = app.clone();
            if let Ok(s) = tokio::task::spawn_blocking(move || {
                watch_once(&a);
                announce_notices(&a, &mut shown);
                shown
            })
            .await
            {
                shown = s;
            } else {
                shown = None;
            }
            tokio::time::sleep(WATCH_EVERY).await;
        }
    })
}
