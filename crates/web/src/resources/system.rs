//! Server health and storage (docs/ui-api.md, "Server, settings and health"): `/about`, `/cars/{car}/health`,
//! `/cars/{car}/rebuild`, `/cars/{car}/overlaps`, `/cars/{car}/storage`, `/system/storage` (with its `detail` and
//! `compact`) and `/system/health`, over
//! the engine's [`engine::query::system`]. (`/health`, with no sign-in and no body, is the frame's own.)
//!
//! Reads of a car are for every account that sees it (the guard checks the `{car}`; a hidden car answers as an
//! unknown one); `/system/*` is admin-only (the guard's list). A rebuild and an overlap flip are an admin's changes,
//! recorded in the audit log, and tell the live stream which rows changed.

use super::now::{answer, list_answer, period, run};
use crate::ctx::{Admin, target};
use crate::query::MAX_MS;
use crate::{ApiError, ApiResult, AppState, CarParam, JsonBody, Params, Viewer, ids};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use engine::Engine;
use engine::query as q;
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// When this server's web side started (its routes were made), for `uptime_ms`.
static STARTED: OnceLock<Instant> = OnceLock::new();

/// How long a rebuild is waited for before the answer says it runs on (`status: "running"`).
pub const REBUILD_WAIT: Duration = Duration::from_secs(60);
/// A rebuild covers at most this much at a time.
pub const REBUILD_MAX_MS: u64 = 3 * 366 * 86_400_000;

/// The data folder's write gate, waited for while it pauses (an export's snapshot); `None` when it is closed (the
/// data moved away) or still paused after a minute.
async fn through_gate(gate: &gate::Gate) -> Option<gate::Pass> {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        match gate.try_enter() {
            Ok(Some(pass)) => return Some(pass),
            Ok(None) | Err(gate::Refused::Busy) => {}
            Err(gate::Refused::Closed(_) | gate::Refused::Io(_)) => return None,
        }
        if Instant::now() >= until {
            return None;
        }
        tokio::time::sleep(gate::STEP * 4).await;
    }
}

/// How long after the start the first look for missing CAN logs waits (the server's start has its own work first).
pub const MISSING_SCAN_DELAY: Duration = Duration::from_secs(120);

/// One look for the CAN logs the server should have but no longer does, over every car key with records (see
/// `ingest::missing`), each key through the data folder's write gate. A key with logs newly found missing gets an
/// audit entry (`can-logs-missing`) on its car. Returns how many were newly found.
pub async fn scan_missing_logs(app: &AppState, gate: &gate::Gate) -> usize {
    let Ok(e) = app.service::<Engine>() else { return 0 };
    let keys = match e.judge().keys().await {
        Ok(k) => k,
        Err(err) => {
            tracing::warn!("missing CAN logs: the car keys could not be listed: {err}");
            return 0;
        }
    };
    let now = app.now();
    scan_edit(|s| {
        *s = MissingScan {
            state: "running",
            keys_done: 0,
            keys_total: keys.len(),
            found: 0,
            started_ms: Some(now),
            finished_ms: None,
        }
    });
    let found = scan_keys(app, gate, &e, keys).await;
    let now = app.now();
    scan_edit(|s| {
        s.state = "done";
        s.finished_ms = Some(now);
        s.found = found;
    });
    found
}

/// The look for missing CAN logs as it is now (the live stream's `job` of kind `missing_scan`).
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct MissingScan {
    /// `idle` (none since the start), `running`, `done`.
    pub state: &'static str,
    pub keys_done: usize,
    pub keys_total: usize,
    /// Logs newly found missing (in this look so far).
    pub found: usize,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
}

static MISSING_SCAN: std::sync::Mutex<MissingScan> = std::sync::Mutex::new(MissingScan {
    state: "idle",
    keys_done: 0,
    keys_total: 0,
    found: 0,
    started_ms: None,
    finished_ms: None,
});

fn scan_edit(f: impl FnOnce(&mut MissingScan)) {
    f(&mut MISSING_SCAN.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
}

/// The look for missing CAN logs under way or the last one.
pub fn missing_scan() -> MissingScan {
    *MISSING_SCAN.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn scan_keys(app: &AppState, gate: &gate::Gate, e: &Arc<Engine>, keys: Vec<String>) -> usize {
    let mut found = 0;
    for key in keys {
        scan_edit(|s| s.found = found);
        // low priority: every other task goes first between keys
        tokio::task::yield_now().await;
        let _count = KeyDone;
        let Some(pass) = through_gate(gate).await else { continue };
        let uploading = e.store().uploading(&key).await;
        let r = e.judge().scan_missing(&key, uploading).await;
        drop(pass);
        let scan = match r {
            Ok(s) => s,
            // not the key: the server's log is kept in the clear
            Err(err) => {
                tracing::warn!("missing CAN logs: a car key could not be looked at: {err}");
                continue;
            }
        };
        if scan.new.is_empty() {
            continue;
        }
        found += scan.new.len();
        let n = scan.new.len();
        let logs = if n == 1 { "1 CAN log".to_string() } else { format!("{n} CAN logs") };
        audit_key(app, &key, "can-logs-missing", move |whose| {
            format!("Found {logs} of {whose} missing on the server; asked the car to send them again")
        })
        .await;
    }
    if found > 0 {
        tracing::info!("missing CAN logs: {found} found missing on the server; the cars are asked to send them again");
    }
    found
}

/// An audit entry by the server itself on the car a car key belongs to (`summary` is told the car's name, or what the
/// key is when no car has it).
async fn audit_key(
    app: &AppState,
    key: &str,
    code: &'static str,
    summary: impl FnOnce(String) -> String + Send + 'static,
) {
    let k = key.to_string();
    let _ = app
        .blocking(move |app| {
            let e = app.service::<Engine>()?;
            let car = q::all_cars(&e)
                .unwrap_or_default()
                .into_iter()
                .find(|&c| e.db().car(c).ok().flatten().is_some_and(|c| c.vehicle_keys.contains(&k)));
            let (t, whose) = match car {
                Some(c) => (Some(target("car", Some(c), "")), app.car_name(c)),
                None => (None, "a CAN Bridge key not given to a car".to_string()),
            };
            let details = crate::ctx::audit_details(t.as_deref(), &summary(whose));
            if let Err(err) = app.accounts.audit().record(None, None, code, &details) {
                tracing::warn!("audit: {err}");
            }
            Ok(())
        })
        .await;
}

/// Once at the start, before the first look for missing logs: each car key whose records were judged by older rules
/// has its failed logs judged again by the rules of this version (`ingest::judge::Judge::rejudge_old`), each key
/// through the data folder's write gate; nothing is sent again. A key with logs judged again gets an audit entry
/// (`can-logs-rechecked`) on its car, saying how many verify now. Returns how many verify now.
pub async fn rejudge_old_logs(app: &AppState, gate: &gate::Gate) -> u64 {
    let Ok(e) = app.service::<Engine>() else { return 0 };
    let keys = match e.judge().keys().await {
        Ok(k) => k,
        Err(err) => {
            tracing::warn!("CAN log checks: the car keys could not be listed: {err}");
            return 0;
        }
    };
    let mut verified = 0;
    for key in keys {
        tokio::task::yield_now().await;
        let Some(pass) = through_gate(gate).await else { continue };
        let r = e.judge().rejudge_old(&key).await;
        drop(pass);
        let done = match r {
            Ok(Some(d)) if d.judged > 0 => d,
            Ok(_) => continue,
            // not the key: the server's log is kept in the clear
            Err(err) => {
                tracing::warn!("CAN log checks: a car key's failed logs could not be judged again: {err}");
                continue;
            }
        };
        verified += done.verified;
        let logs = |n: u64| if n == 1 { "1 CAN log".to_string() } else { format!("{n} CAN logs") };
        let (judged, now) = (logs(done.judged), done.verified);
        audit_key(app, &key, "can-logs-rechecked", move |whose| {
            format!("Checked {judged} of {whose} that had failed their check again, by this version's rules: {now} verified now")
        })
        .await;
    }
    if verified > 0 {
        tracing::info!("CAN log checks: {verified} logs that had failed their check verify by this version's rules");
    }
    verified
}

/// Counts a key of the look as done however its turn ends.
struct KeyDone;

impl Drop for KeyDone {
    fn drop(&mut self) {
        scan_edit(|s| s.keys_done += 1);
    }
}

pub fn routes() -> Router<AppState> {
    STARTED.get_or_init(Instant::now);
    Router::new()
        .route("/api/v1/about", get(about))
        .route("/api/v1/cars/{car}/health", get(health))
        .route("/api/v1/cars/{car}/rebuild", post(rebuild))
        .route("/api/v1/cars/{car}/repair", get(repair_report).post(start_repair))
        .route("/api/v1/cars/{car}/gps-layout", get(gps_layout).put(put_gps_layout))
        .route("/api/v1/cars/{car}/gps-layout/fit", get(gps_fit).post(start_gps_fit))
        .route("/api/v1/cars/{car}/gps-layout/fits", get(gps_fits))
        .route("/api/v1/cars/{car}/gps-layout/fits/{fit}", get(gps_fit_one).delete(delete_gps_fit))
        .route("/api/v1/cars/{car}/gps-layout/fits/{fit}/apply", post(apply_gps_fit))
        .route("/api/v1/cars/{car}/gps-layout/changes", get(gps_layout_changes))
        .route("/api/v1/cars/{car}/overlaps", get(overlaps).patch(flip_overlap))
        .route("/api/v1/cars/{car}/storage", get(car_storage))
        .route("/api/v1/system/storage", get(system_storage))
        .route("/api/v1/system/storage/detail", get(storage_detail))
        .route("/api/v1/system/storage/compact", post(storage_compact))
        .route("/api/v1/system/health", get(system_health))
}

/// The server's own version (the workspace's).
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The signal lists the server decodes with, and their licences (crates/dbc/files).
const LICENCES: &[(&str, &str)] = &[
    ("model3dbc (Tesla Model 3 and Y signal list)", "MIT"),
    ("opendbc (Tesla signal lists)", "MIT"),
    ("Open Vehicle Monitoring System (Model S signal list)", "MIT"),
];

async fn about(State(app): State<AppState>, v: Viewer, headers: axum::http::HeaderMap) -> ApiResult<Json<Value>> {
    let original = crate::packs::original(&headers);
    let pack = app.blocking(move |app| Ok(crate::packs::about_ui_pack(app, Some(&v), original))).await?;
    let licences: Vec<Value> = LICENCES.iter().map(|(n, l)| json!({ "name": n, "licence": l })).collect();
    Ok(Json(json!({
        "version": SERVER_VERSION,
        "build": super::release::about_build(&app),
        "api_version": crate::API_VERSION,
        "ui_pack": pack,
        "links": { "source": null, "docs": "/help" },
        "licence": { "name": null, "url": null },
        "licences": licences,
        "data_credits": [
            { "name": "OpenStreetMap", "text": format!("Map data {}", places::tiles::OSM_TILE_CREDIT), "licence": "ODbL" },
            { "name": "GeoNames", "text": places::geocode::GEONAMES_CREDIT, "licence": "CC BY 4.0" },
        ],
    })))
}

/// Whether the car has an upload token that is not revoked.
pub(crate) fn uploads_enabled(e: &Engine, car: model::CarId) -> q::QResult<bool> {
    let mut paging = q::Paging { limit: Some(q::MAX_LIMIT), after: None };
    for _ in 0..100 {
        let page = q::keys::car_tokens(e, car, &paging)?;
        if page.body.items.iter().any(|t| !t.revoked) {
            return Ok(true);
        }
        match page.body.next {
            Some(n) => paging.after = Some(n),
            None => break,
        }
    }
    Ok(false)
}

async fn health(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    answer(&app, &p, "health", move |e| {
        let mut h = q::system::health(e, car, &per)?;
        h.body.uploads_enabled = Some(uploads_enabled(e, car)?);
        Ok(h)
    })
    .await
}

/// `POST /cars/{car}/rebuild` `{ from_ms?, to_ms? }`: drives, charges, states and visits made again from the stored
/// logs (the whole history when no period is given; at most three years at a time when one is). Waits for the job
/// (at most [`REBUILD_WAIT`]; beyond, it answers `status: "running"` and the job carries on). A job that failed is
/// `500 rebuild_failed` with why. The audit entry is written when the job ends, whatever became of the request.
async fn rebuild(
    State(app): State<AppState>,
    v: Viewer,
    CarParam(car): CarParam,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["from_ms", "to_ms"])?;
    let (from, to) = (body.u64("from_ms")?, body.u64("to_ms")?);
    for (field, t) in [("from_ms", from), ("to_ms", to)] {
        if t.is_some_and(|t| t > MAX_MS) {
            return Err(ApiError::invalid(field, "A time in ms since 1970, before the year 2100."));
        }
    }
    if let (Some(a), Some(b)) = (from, to) {
        if a >= b {
            return Err(ApiError::invalid("to_ms", "Must be after from_ms."));
        }
        if b - a > REBUILD_MAX_MS {
            return Err(ApiError::invalid("from_ms", "At most three years at a time."));
        }
    }
    let e = app.service::<Engine>()?;
    let e2 = e.clone();
    let by = v.name().to_string();
    let asked = Asked { car, from, to, who: by.clone(), ip: v.client().address };
    // The job is followed by a task of its own, which writes the audit entry when it ends, by how it ended. It is
    // set to follow the job in the same step that starts it: a request that goes away meanwhile (a closed page, a
    // timeout), even while the job is being started, or that gives up waiting changes neither.
    let (job, mut end) = app
        .blocking(move |app| {
            super::full_rebuild::refuse_while_running(&e2)?;
            let job = e2
                .rebuild_because(
                    engine::Scope { car: Some(car), from_ms: from, to_ms: to },
                    engine::jobs::What::WHOLE,
                    engine::versions::Cause::owner(Some(&by)),
                    false,
                )
                .map_err(|x| ApiError::server(format!("starting the rebuild: {x}")))?;
            let end = follow_rebuild(app, &e2, job.id, asked);
            Ok((job, end))
        })
        .await?;
    v.mark_audited();
    let now = match tokio::time::timeout(REBUILD_WAIT, end.wait_for(Option::is_some)).await {
        Ok(Ok(j)) => j.clone().unwrap_or_else(|| job.clone()),
        _ => e.job(job.id).unwrap_or_else(|| job.clone()),
    };
    let status = match now.status {
        engine::JobStatus::Running => "running",
        engine::JobStatus::Done => "done",
        engine::JobStatus::Cancelled => "cancelled",
        engine::JobStatus::Failed => {
            return Err(ApiError::new(500, "rebuild_failed", format!("The rebuild failed: {}.", why_failed(&now))));
        }
    };
    let kinds = ["drives", "charges", "states", "visits"];
    let (a, b) = (now.from_ms, now.to_ms.min(app.now().max(now.from_ms.saturating_add(1))));
    // (the engine's rebuild tells the live stream of each window it made again: no second `rows` event here)
    Ok(Json(json!({
        "car": ids::car(car),
        "job": now.id.to_string(),
        "status": status,
        "from_ms": a,
        "to_ms": b,
        "kinds": kinds,
        "logs_read": now.done.saturating_sub(now.failed),
        "logs_total": now.total,
        "took_ms": now.took_ms,
    })))
}

/// Whether a file of a car's db is its drives table.
fn is_drives_table(file: &str) -> bool {
    file.ends_with("/drives.snap") || file.ends_with("/drives.log")
}

/// A time span in words: `from 2026-01-02 UTC to 2026-03-04 UTC`, an open end as one.
fn span_words(from: Option<u64>, to: Option<u64>) -> String {
    let day = |ms: u64| format!("{} UTC", moving::export::day_of(ms));
    match (from, to) {
        (Some(a), Some(b)) => format!("from {} to {}", day(a), day(b)),
        (Some(a), None) => format!("from {} on", day(a)),
        (None, Some(b)) => format!("up to {}", day(b)),
        (None, None) => "at a time that is not known".into(),
    }
}

/// What a repair could not give back, in one sentence.
fn lost_words(l: &engine::repair::Lost) -> String {
    let n = match (l.expected, l.found) {
        (Some(e), Some(f)) => format!("{} of {e}", e.saturating_sub(f)),
        (Some(e), None) => format!("{e}"),
        _ => "some".into(),
    };
    format!("{n} {} ({}, {}): {}", l.what, l.source, span_words(l.from_ms, l.to_ms), l.why)
}

/// A repair's report as the API gives it.
fn repair_json(car: model::CarId, r: &engine::repair::RepairReport) -> Value {
    let mut v = serde_json::to_value(r).unwrap_or(Value::Null);
    v["car"] = json!(ids::car(car));
    v["state"] = json!(if r.running() { "running" } else { "done" });
    v["lost"] = json!(
        r.lost
            .iter()
            .map(|l| {
                let mut j = serde_json::to_value(l).unwrap_or(Value::Null);
                j["said"] = json!(lost_words(l));
                j
            })
            .collect::<Vec<_>>()
    );
    if let Some(o) = v.as_object_mut() {
        o.remove("audited");
        o.remove("live_expected");
        o.remove("live_salvaged");
    }
    v
}

/// `GET /cars/{car}/repair` (admin): the report of the car's last repair of its drives table (what was kept, put
/// back and made again, and `lost`: what could not be recovered, each with the time it covered), or `404 no_repair`.
async fn repair_report(
    State(app): State<AppState>,
    Admin(_): Admin,
    CarParam(car): CarParam,
) -> ApiResult<Json<Value>> {
    let e = app.service::<Engine>()?;
    let r = app.blocking(move |_| e.repair_report(car).map_err(|x| ApiError::server(x.to_string()))).await?;
    match r {
        Some(r) => Ok(Json(repair_json(car, &r))),
        None => Err(ApiError::not_found("no_repair")),
    }
}

/// `POST /cars/{car}/repair` `{ table: "drives" }` (admin): repairs the car's damaged drives table in the
/// background (`202`): what still reads is kept, the files are put aside, imported rows come back from the imports'
/// kept uploads, and the rest is made again from the stored logs by a rebuild from where the damage begins. A table
/// that reads is left alone. `409 repair_running` while one is under way. Read the outcome at `GET .../repair`.
async fn start_repair(
    State(app): State<AppState>,
    Admin(v): Admin,
    CarParam(car): CarParam,
    body: JsonBody,
) -> ApiResult<Response> {
    body.only(&["table"])?;
    if body.str("table")? != Some("drives") {
        return Err(ApiError::invalid("table", "Only the drives table is repaired: \"drives\"."));
    }
    let e = app.service::<Engine>()?;
    super::full_rebuild::refuse_while_running(&e)?;
    // an import writes the car's rows meanwhile: wait for it
    if app.service::<super::imports::Imports>().is_ok_and(|im| im.running() > 0) {
        return Err(ApiError::conflict(
            "import_running",
            "An import is running and writes the car's rows: repair the table once it is over.",
        ));
    }
    // (the repair claims the car before this answers, so a second ask meets it)
    if !e.repair_begin(car) {
        return Err(ApiError::conflict("repair_running", "A repair of this car's drives table is under way."));
    }
    let name = app.car_name_now(car).await;
    let e2 = e.clone();
    drop(tokio::task::spawn_blocking(move || {
        if let Err(err) = e2.repair_drives_table(car) {
            tracing::warn!("repairing the drives table of car {car}: {err}");
        }
    }));
    v.audit(
        &app,
        "table-repaired",
        Some(&target("car", Some(car), "")),
        &format!("Asked for a repair of {name}'s drives table."),
    )
    .await?;
    let out = json!({ "car": ids::car(car), "table": "drives", "state": "running" });
    Ok((axum::http::StatusCode::ACCEPTED, Json(out)).into_response())
}

/// The end of each repair into the audit log, once (in the server's name): what was kept, put back and made again,
/// and what could not be recovered.
pub fn audit_repairs(app: &AppState, e: &Engine) -> usize {
    let done = e.repairs_to_audit();
    for (car, r) in &done {
        let (car, r) = (*car, r);
        let name = app.car_name(car);
        let lost = match r.lost.len() {
            0 => "nothing was lost".to_string(),
            1 => "1 thing could not be recovered".to_string(),
            n => format!("{n} things could not be recovered"),
        };
        let summary = format!(
            "Repaired {name}'s drives table: {} rows kept, {} imported rows put back from the kept uploads, {} \
             made again from the logs, {} purposes put back on their drives; {lost}.",
            r.salvaged,
            r.reimported,
            r.rebuilt.unwrap_or(0),
            r.relinked,
        );
        let details = crate::ctx::audit_details(Some(&target("car", Some(car), "")), &summary);
        match app.accounts.audit().record(None, None, "table-repaired", &details) {
            Ok(()) => {
                let _ = e.repair_audited(car, r.started_ms);
            }
            Err(err) => tracing::warn!("audit: {err}"),
        }
    }
    done.len()
}

/// Why a rebuild job failed, as a sentence's end.
fn why_failed(j: &engine::Job) -> String {
    j.error.as_deref().unwrap_or("no reason given").trim_end_matches('.').to_string()
}

/// Who asked for a rebuild, of what.
struct Asked {
    car: model::CarId,
    from: Option<u64>,
    to: Option<u64>,
    who: String,
    ip: std::net::IpAddr,
}

/// How much of the history a rebuild was asked for, for its audit entry: the whole of it, or the period in UTC
/// days as it was given (an open end is said as one, never counted from 1970 or to the year 2100).
fn rebuild_span(from: Option<u64>, to: Option<u64>) -> String {
    let day = |ms: u64| format!("{} UTC", moving::export::day_of(ms));
    match (from, to) {
        (None, None) => "the whole history".into(),
        (Some(a), None) => format!("everything from {} on", day(a)),
        (None, Some(b)) => format!("everything up to {}", day(b)),
        (Some(a), Some(b)) => match b.saturating_sub(a).div_ceil(86_400_000).max(1) {
            1 => format!("1 day from {}", day(a)),
            n => format!("{n} days from {}", day(a)),
        },
    }
}

/// Where the notices below send an admin: the server's settings, with the rebuilds.
const NOTICE_LINK: &str = "/settings/server";

/// For admins: a car's rebuild that failed (`rebuild_failed`, until a later rebuild of the car ends well or the
/// server starts again), and files of a car's history that can't be read (`data_damaged`, while they are there),
/// each with what can be done about it.
pub fn notices(app: &AppState) -> Vec<super::notices::Notice> {
    use super::notices::Notice;
    let Ok(e) = app.service::<Engine>() else { return Vec::new() };
    let mut out = Vec::new();
    let damaged = e.db().damaged();
    if let Some(first) = damaged.first() {
        let name = app.car_name(first.car);
        let more = match damaged.len() - 1 {
            0 => String::new(),
            1 => " (and 1 more file)".into(),
            n => format!(" (and {n} more files)"),
        };
        let fix = if damaged.iter().all(|d| d.from_logs) {
            "It holds only what was decoded from the stored logs: a rebuild of the car makes it again.".to_string()
        } else if damaged.iter().all(|d| d.from_logs || is_drives_table(&d.file)) {
            "A rebuild of the car, or Repair, mends it: what still reads is kept, the imported rows come back from \
             the kept uploads of the imports, the rest is made again from the stored logs, and the damaged file is \
             set aside (never deleted). What could not be recovered is said afterwards."
                .to_string()
        } else {
            format!(
                "It also holds what no stored log gives (imported rows, what was typed in), so no rebuild makes it \
                 again: stop the server, put the folder db/cars/{} back from a backup and start it again; the logs \
                 that came since are decoded again by themselves.",
                first.car
            )
        };
        out.push(Notice {
            code: "data_damaged",
            level: "error",
            message: format!(
                "A file of {name}'s history is damaged and can't be read: {}{more}. Pages that need it answer with \
                 an error. {fix}",
                first.file
            ),
            since_ms: damaged.iter().map(|d| d.since_ms).min(),
            fix_link: Some(NOTICE_LINK),
            detail: Some(json!({
                "files": damaged.iter().map(|d| json!({
                    "car": ids::car(d.car), "file": d.file, "why": d.why, "since_ms": d.since_ms,
                    "mend": if d.from_logs { "rebuild" } else if is_drives_table(&d.file) { "repair" } else { "backup" },
                })).collect::<Vec<_>>(),
            })),
        });
    }
    // a drives table that was repaired and could not be given back whole
    for car in engine::rehome::car_ids(&e).unwrap_or_default() {
        let Ok(Some(r)) = e.repair_report(car) else { continue };
        if r.running() || r.lost.is_empty() {
            continue;
        }
        let said: Vec<String> = r.lost.iter().map(lost_words).collect();
        out.push(Notice {
            code: "data_repaired",
            level: "warning",
            message: format!(
                "The drives table of {} was damaged and has been repaired, but some of it could not be recovered: \
                 {}. The damaged file was kept aside. Restore from a backup only for what is listed here.",
                app.car_name(car),
                said.join("; ")
            ),
            since_ms: r.finished_ms,
            fix_link: Some(NOTICE_LINK),
            detail: Some(repair_json(car, &r)),
        });
    }
    // the newest ended job of each car (or of every car): failed, and none after it ended well
    let mut seen = std::collections::HashSet::new();
    for j in e.jobs() {
        if j.status == engine::JobStatus::Running || !seen.insert(j.car) {
            continue;
        }
        if j.status == engine::JobStatus::Failed {
            let of = j.car.map_or_else(|| "every car".to_string(), |c| app.car_name(c));
            out.push(Notice {
                code: "rebuild_failed",
                level: "warning",
                message: format!("The rebuild of {of} from the stored logs failed: {}.", why_failed(&j)),
                since_ms: j.finished_ms,
                fix_link: Some(NOTICE_LINK),
                detail: Some(json!({ "car": j.car.map(ids::car), "job": j.id.to_string(), "kind": j.kind })),
            });
        }
    }
    out
}

type RebuildEnd = tokio::sync::watch::Receiver<Option<engine::Job>>;

/// A rebuild job is followed to its end for at most this long.
const FOLLOW_MAX: Duration = Duration::from_secs(7 * 86_400);

/// The rebuild jobs being followed, by engine and job: a second ask that joined a job waits for the same end, and
/// the job gets one audit entry.
static FOLLOWED: std::sync::Mutex<Vec<(usize, u64, RebuildEnd)>> = std::sync::Mutex::new(Vec::new());

/// Follows a rebuild job to its end in a task of its own (one per job) and writes its audit entry then: rebuilt,
/// failed (with why) or cancelled. Returns where the end is told.
fn follow_rebuild(app: &AppState, e: &Arc<Engine>, id: u64, asked: Asked) -> RebuildEnd {
    let key = (Arc::as_ptr(e) as usize, id);
    let mut followed = FOLLOWED.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((_, _, rx)) = followed.iter().find(|(p, i, _)| (*p, *i) == key) {
        return rx.clone();
    }
    let (tx, rx) = tokio::sync::watch::channel(None);
    followed.push((key.0, key.1, rx.clone()));
    drop(followed);
    let (app, e) = (app.clone(), e.clone());
    tokio::spawn(async move {
        let began = Instant::now();
        let job = loop {
            match e.job(id) {
                // (a job that never ends, its engine stopped under it, is not followed for good)
                Some(j) if j.status == engine::JobStatus::Running && began.elapsed() < FOLLOW_MAX => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                j => break j,
            }
        };
        if let Some(j) = job.as_ref().filter(|j| j.status != engine::JobStatus::Running) {
            let name = app.car_name_now(asked.car).await;
            let what = format!("{name}'s drives, charges, states and visits from the stored logs");
            let span = rebuild_span(asked.from, asked.to);
            let summary = match j.status {
                engine::JobStatus::Failed => format!("The rebuild of {what} ({span}) failed: {}.", why_failed(j)),
                engine::JobStatus::Cancelled => format!("The rebuild of {what} ({span}) was cancelled."),
                _ => format!("Rebuilt {what} ({span})."),
            };
            let details = crate::ctx::audit_details(Some(&target("car", Some(asked.car), "")), &summary);
            let (who, ip) = (asked.who, asked.ip);
            let r = app
                .blocking(move |app| Ok(app.accounts.audit().record(Some(&who), Some(ip), "car-rebuild", &details)?))
                .await;
            if let Err(err) = r {
                tracing::warn!("audit: {}", err.message);
            }
        }
        FOLLOWED.lock().unwrap_or_else(|p| p.into_inner()).retain(|(p, i, _)| (*p, *i) != key);
        // (the entry is written before the end is told: an answer that says done has it)
        let _ = tx.send(job.or_else(|| e.job(id)));
    });
    rx
}

/// `GET /cars/{car}/gps-layout`: the car's own layout of the Screenmate's GPS frame (0x04F on its internal bus) as
/// `sentry-mate gps-layout` prints it, or null while the DBC's is used; `set_ms` when it was last changed (null:
/// never).
async fn gps_layout(State(app): State<AppState>, CarParam(car): CarParam) -> ApiResult<Json<Value>> {
    let e = app.service::<Engine>()?;
    let (l, set) = app
        .blocking(move |_| {
            let read = |x: engine::Error| ApiError::server(format!("reading the layout: {x}"));
            Ok::<_, ApiError>((e.gps_layout(car).map_err(read)?, e.gps_layout_set_ms(car).map_err(read)?))
        })
        .await?;
    Ok(Json(json!({ "car": ids::car(car), "layout": l.map(|l| l.to_string()), "set_ms": set })))
}

/// `PUT /cars/{car}/gps-layout` `{ layout }`: the layout `sentry-mate gps-layout` printed (`lat 0|28@1- (1e-6,0);
/// lon 28|28@1- (1e-6,0)`), or null for the DBC's. A change reads the car's positions again (only its stored logs whose
/// positions came from that frame are decoded again, a few saves in a row are one rebuild); the answer names its
/// `job`, null when nothing changed.
async fn put_gps_layout(
    State(app): State<AppState>,
    v: Viewer,
    CarParam(car): CarParam,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["layout"])?;
    let layout: Option<model::gps::GpsLayout> = match body.get("layout") {
        None => return Err(ApiError::invalid("layout", "Required: the layout, or null for the DBC's.")),
        Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.parse().map_err(|why: String| ApiError::invalid("layout", &why))?),
        Some(_) => return Err(ApiError::invalid("layout", "Must be text or null.")),
    };
    let e = app.service::<Engine>()?;
    let by = v.name().to_string();
    let job = app
        .blocking(move |_| {
            e.set_gps_layout_by(car, layout, Some(by), None).map_err(|x| ApiError::server(format!("the layout: {x}")))
        })
        .await?;
    if job.is_some() {
        let name = app.car_name_now(car).await;
        let summary = match layout {
            Some(l) => {
                format!("Set how {name}'s Screenmate GPS frames are read ({l}); its positions are read again.")
            }
            None => {
                format!("Set {name}'s Screenmate GPS frames to be read as the DBC says; its positions are read again.")
            }
        };
        v.audit(&app, "car-gps-layout", Some(&target("car", Some(car), "")), &summary).await?;
    }
    let e = app.service::<Engine>()?;
    let set = app
        .blocking(move |_| e.gps_layout_set_ms(car).map_err(|x| ApiError::server(format!("reading the layout: {x}"))))
        .await?;
    Ok(Json(json!({
        "car": ids::car(car),
        "layout": layout.map(|l| l.to_string()),
        "set_ms": set,
        "job": job.map(|j| j.id.to_string()),
    })))
}

/// A GPS layout fit as the API answers it (`None`: none since the server started): its state and progress, what
/// it found (layouts, counts and errors in metres only), the car's layout now, and what else the server is busy
/// with.
pub fn gps_fit_json(
    e: &Engine,
    car: model::CarId,
    s: Option<&engine::gps_fit::FitStatus>,
    now: u64,
) -> ApiResult<Value> {
    use engine::gps_fit::FitState;
    let current = e.gps_layout(car).map_err(|x| ApiError::server(format!("reading the layout: {x}")))?;
    let current = current.map(|l| l.to_string());
    let busy = e.busy_words();
    let Some(s) = s else {
        return Ok(json!({
            "car": ids::car(car), "state": "idle", "id": null, "days": null, "phase": null, "done": 0, "total": 0,
            "percent": 0, "positions_total": 0, "logs_total": 0, "eta_ms": null, "started_ms": null,
            "finished_ms": null, "error": null, "result": null, "waiting": null, "current": current, "applied": false,
            "applied_ms": null, "applied_by": null, "server_busy": busy,
        }));
    };
    let result = match &s.outcome {
        Some(o) => {
            let mut v = serde_json::to_value(o).map_err(ApiError::server)?;
            v["car"] = json!(ids::car(car));
            v
        }
        None => Value::Null,
    };
    // the layout found is the one the car's frames are read with now
    let applied = s
        .outcome
        .as_ref()
        .is_some_and(|o| o.layout.is_some() && if o.is_dbc { current.is_none() } else { o.layout == current });
    Ok(json!({
        "car": ids::car(car),
        "state": match s.state { FitState::Running => "running", FitState::Done => "done", FitState::Failed => "failed" },
        "id": s.id.to_string(),
        "days": s.days,
        "phase": s.phase,
        "done": s.done,
        "total": s.total,
        "percent": s.percent(),
        "positions_total": s.positions_total,
        "logs_total": s.logs_total,
        "eta_ms": s.eta_ms(now),
        "started_ms": s.started_ms,
        "finished_ms": s.finished_ms,
        "error": s.error,
        "waiting": s.waiting,
        "result": result,
        "current": current,
        "applied": applied,
        "applied_ms": s.applied_ms,
        "applied_by": s.applied_by,
        "server_busy": busy,
    }))
}

/// A run as a row of the list of past fits: its counts, the best layout with its errors in metres, and whether it
/// was applied. No position, time of day or place.
pub fn gps_fit_row(e: &Engine, car: model::CarId, s: &engine::gps_fit::FitStatus) -> ApiResult<Value> {
    use engine::gps_fit::FitState;
    let current = e.gps_layout(car).map_err(|x| ApiError::server(format!("reading the layout: {x}")))?;
    let current = current.map(|l| l.to_string());
    let o = s.outcome.as_ref();
    let applied = s.applied_ms.is_some()
        || o.is_some_and(|o| o.layout.is_some() && if o.is_dbc { current.is_none() } else { o.layout == current });
    Ok(json!({
        "id": s.id.to_string(),
        "car": ids::car(car),
        "state": match s.state { FitState::Running => "running", FitState::Done => "done", FitState::Failed => "failed" },
        "days": s.days,
        "percent": s.percent(),
        "started_ms": s.started_ms,
        "finished_ms": s.finished_ms,
        "error": s.error,
        "logs": o.map(|o| o.logs),
        "pairs": o.map(|o| o.pairs),
        "layout": o.and_then(|o| o.layout.clone()),
        "median_m": o.and_then(|o| o.median_m),
        "p95_m": o.and_then(|o| o.p95_m),
        "is_dbc": o.map(|o| o.is_dbc),
        "enough": o.map(|o| o.enough),
        "weak": o.map(|o| o.weak),
        "good": o.map(|o| o.good),
        "applied": applied,
        "applied_ms": s.applied_ms,
        "applied_by": s.applied_by,
    }))
}

fn fit_id(text: &str) -> ApiResult<u64> {
    ids::number(text).ok_or_else(|| ApiError::not_found("no_such_fit"))
}

/// `GET /cars/{car}/gps-layout/fits` (admin): every fit run of the car, newest first, paged: one row each
/// ([`gps_fit_row`]). The one under way is the first row.
async fn gps_fits(
    State(app): State<AppState>,
    Admin(_): Admin,
    CarParam(car): CarParam,
    p: Params,
) -> ApiResult<Json<Value>> {
    let e = app.service::<Engine>()?;
    let pg = super::now::paging(&p)?;
    let v = app
        .blocking(move |_| {
            let page = e.gps_fit_page(car, &pg).map_err(|x| ApiError::server(format!("reading the fits: {x}")))?;
            let items = page.items.iter().map(|s| gps_fit_row(&e, car, s)).collect::<ApiResult<Vec<_>>>()?;
            Ok(json!({ "items": items, "next": page.next, "total": page.total }))
        })
        .await?;
    Ok(Json(v))
}

/// `GET /cars/{car}/gps-layout/changes` (admin): the layouts the car was read with over time, newest first,
/// paged: `{ at_ms, by, layout, before, fit }` (`layout`, `before` null: the DBC's; `fit` the run it was applied
/// from, when one).
async fn gps_layout_changes(
    State(app): State<AppState>,
    Admin(_): Admin,
    CarParam(car): CarParam,
    p: Params,
) -> ApiResult<Response> {
    list_answer(&app, &p, "gps-layout-changes", move |e, pg| e.gps_layout_page(car, pg)).await
}

/// `GET /cars/{car}/gps-layout/fits/{fit}` (admin): one run in full (the answer of `GET .../fit`, with its result).
async fn gps_fit_one(
    State(app): State<AppState>,
    Admin(_): Admin,
    CarParam(car): CarParam,
    axum::extract::Path(path): axum::extract::Path<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let id = fit_id(path.get("fit").map_or("", String::as_str))?;
    let e = app.service::<Engine>()?;
    let now = app.now();
    let v = app
        .blocking(move |_| {
            let s = e
                .gps_fit_run(car, id)
                .map_err(|x| ApiError::server(format!("reading the fit: {x}")))?
                .ok_or_else(|| ApiError::not_found("no_such_fit"))?;
            gps_fit_json(&e, car, Some(&s), now)
        })
        .await?;
    Ok(Json(v))
}

/// `DELETE /cars/{car}/gps-layout/fits/{fit}` (admin): removes a finished run (`409 fit_running` while it runs). It
/// changes no layout in use.
async fn delete_gps_fit(
    State(app): State<AppState>,
    Admin(v): Admin,
    CarParam(car): CarParam,
    axum::extract::Path(path): axum::extract::Path<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    use engine::gps_fit_runs::Deleted;
    let id = fit_id(path.get("fit").map_or("", String::as_str))?;
    let e = app.service::<Engine>()?;
    let gone = app
        .blocking(move |_| e.gps_fit_delete(car, id).map_err(|x| ApiError::server(format!("removing the fit: {x}"))))
        .await?;
    match gone {
        Deleted::NotFound => return Err(ApiError::not_found("no_such_fit")),
        Deleted::Running => {
            return Err(ApiError::conflict("fit_running", "This fit is still under way: wait for it to end."));
        }
        Deleted::Gone => {}
    }
    let name = app.car_name_now(car).await;
    let summary = format!("Removed a past GPS layout fit of {name}.");
    v.audit(&app, "car-gps-fit-removed", Some(&target("car", Some(car), "")), &summary).await?;
    Ok(Json(json!({ "car": ids::car(car), "id": id.to_string(), "removed": true })))
}

/// `POST /cars/{car}/gps-layout/fits/{fit}/apply` `{ confirm_password, code? }` (admin): sets the car's layout to the
/// one a finished run found (the DBC's when that was best), as `PUT /cars/{car}/gps-layout` does, with the password
/// asked again. Names the run in the list of layouts over time. `409 fit_not_done`, `409 fit_found_no_layout`.
async fn apply_gps_fit(
    State(app): State<AppState>,
    Admin(v): Admin,
    CarParam(car): CarParam,
    axum::extract::Path(path): axum::extract::Path<std::collections::HashMap<String, String>>,
    headers: axum::http::HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    use crate::accounts_admin::{confirm_code, confirm_password};
    body.only(&["confirm_password", "code"])?;
    let id = fit_id(path.get("fit").map_or("", String::as_str))?;
    let e = app.service::<Engine>()?;
    let e2 = e.clone();
    let run = app
        .blocking(move |_| e2.gps_fit_run(car, id).map_err(|x| ApiError::server(format!("reading the fit: {x}"))))
        .await?
        .ok_or_else(|| ApiError::not_found("no_such_fit"))?;
    let Some(o) = run.outcome.as_ref().filter(|_| run.state == engine::gps_fit::FitState::Done) else {
        return Err(ApiError::conflict("fit_not_done", "This fit has no result to apply."));
    };
    if o.layout.is_none() {
        return Err(ApiError::conflict("fit_found_no_layout", "This fit found no layout within 5 km to apply."));
    }
    let layout: Option<model::gps::GpsLayout> = if o.is_dbc {
        None
    } else {
        o.layout
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(|why: String| ApiError::server(format!("the fit's layout: {why}")))?
    };
    let password = confirm_password(Some(&body), &headers)?;
    let code = confirm_code(Some(&body), &headers)?;
    let v2 = v.clone();
    app.blocking(move |app| Ok(app.accounts.confirm(v2.signed_in(), &password, code.as_deref())?)).await?;
    let by = v.name().to_string();
    let e2 = e.clone();
    let job = app
        .blocking(move |_| {
            let job = e2
                .set_gps_layout_by(car, layout, Some(by.clone()), Some(id))
                .map_err(|x| ApiError::server(format!("the layout: {x}")))?;
            e2.gps_fit_applied(car, id, Some(by)).map_err(|x| ApiError::server(format!("noting the fit: {x}")))?;
            Ok(job)
        })
        .await?;
    if job.is_some() {
        let name = app.car_name_now(car).await;
        let summary = match layout {
            Some(l) => format!(
                "Set how {name}'s Screenmate GPS frames are read ({l}), from a past fit; its positions are read again."
            ),
            None => format!(
                "Set {name}'s Screenmate GPS frames to be read as the DBC says, from a past fit; its positions are read \
                 again."
            ),
        };
        v.audit(&app, "car-gps-layout", Some(&target("car", Some(car), "")), &summary).await?;
    } else {
        v.mark_audited();
    }
    let e2 = e.clone();
    let set = app
        .blocking(move |_| e2.gps_layout_set_ms(car).map_err(|x| ApiError::server(format!("reading the layout: {x}"))))
        .await?;
    Ok(Json(json!({
        "car": ids::car(car),
        "layout": layout.map(|l| l.to_string()),
        "set_ms": set,
        "job": job.map(|j| j.id.to_string()),
        "fit": id.to_string(),
    })))
}

/// `GET /cars/{car}/gps-layout/fit` (admin): the car's newest GPS layout fit, `state: "idle"` when none ran since
/// the server started.
async fn gps_fit(State(app): State<AppState>, Admin(_): Admin, CarParam(car): CarParam) -> ApiResult<Json<Value>> {
    let e = app.service::<Engine>()?;
    let now = app.now();
    let v = app.blocking(move |_| gps_fit_json(&e, car, e.gps_fit_status(car).as_ref(), now)).await?;
    Ok(Json(v))
}

/// `POST /cars/{car}/gps-layout/fit` `{ days? }` (admin): finds the layout of the car's Screenmate GPS frame from
/// the last `days` days (1 to 366, default 30) in the background, as `sentry-mate gps-layout` does; `202` with the
/// fit's status (one under way already is answered instead). It only reads: Apply is `PUT /cars/{car}/gps-layout`.
async fn start_gps_fit(
    State(app): State<AppState>,
    Admin(_): Admin,
    CarParam(car): CarParam,
    body: JsonBody,
) -> ApiResult<Response> {
    body.only(&["days"])?;
    let days = match body.get("days") {
        None | Some(Value::Null) => 30,
        Some(v) => match v.as_u64() {
            Some(d @ 1..=366) => d as u32,
            _ => return Err(ApiError::invalid("days", "A whole number of days, 1 to 366.")),
        },
    };
    let e = app.service::<Engine>()?;
    let now = app.now();
    let v = app
        .blocking(move |_| {
            let s = e.gps_fit_start(car, days).map_err(|x| match x {
                engine::gps_fit::FitError::NoSuchCar => ApiError::no_such_car(),
                engine::gps_fit::FitError::Other(why) => ApiError::server(format!("starting the fit: {why}")),
            })?;
            gps_fit_json(&e, car, Some(&s), now)
        })
        .await?;
    Ok((axum::http::StatusCode::ACCEPTED, Json(v)).into_response())
}

async fn overlaps(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    list_answer(&app, &p, "overlaps", move |e, pg| q::system::overlaps(e, car, pg)).await
}

/// `PATCH /cars/{car}/overlaps` `{ id, shown, version? }` (or `start_ms` in place of `id`, a `live` span): which side
/// a span shows, or of an `imports` span which site. A stale `version` is `409 stale_version`; one is needed for an
/// `imports` span (the old pages that flip `live` spans send none). Answers the first page of the list plus `changed`.
async fn flip_overlap(
    State(app): State<AppState>,
    v: Viewer,
    CarParam(car): CarParam,
    p: Params,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["id", "start_ms", "shown", "version"])?;
    let shown = body.req_str("shown")?.to_string();
    let version = body.u64("version")?;
    let id = match (body.str("id")?, body.u64("start_ms")?) {
        (Some(id), _) => id.to_string(),
        (None, Some(t)) => format!("overlap:{t}"),
        (None, None) => return Err(ApiError::bad_input("id", "Name the overlap by its id (or start_ms).")),
    };
    let pair = id.starts_with("imports:");
    if pair && version.is_none() {
        return Err(ApiError::bad_input("version", "Required: the version you loaded."));
    }
    let words: &[&str] = if pair {
        &["import:teslamate", "import:teslafi", "teslamate", "teslafi"]
    } else {
        &["live", "can", "import", "import:teslamate", "import:teslafi", "teslamate"]
    };
    if !words.contains(&shown.as_str()) {
        return Err(ApiError::invalid(
            "shown",
            if pair {
                "Must be import:teslamate or import:teslafi: the import this span shows."
            } else {
                "Must be live or import (import:teslamate, import:teslafi; or the old words can, teslamate)."
            },
        ));
    }
    let paging = super::now::paging(&p)?;
    let (s2, id2) = (shown.clone(), id.clone());
    let out = run(&app, move |e| {
        Ok(match q::system::set_overlap(e, car, &id2, &s2, version)? {
            q::fleet::Patched::Done(changed) => (Some(changed), None, Some(q::system::overlaps(e, car, &paging)?)),
            q::fleet::Patched::Stale { version } => (None, Some(version), None),
        })
    })
    .await?;
    if let Some(have) = out.get(1).and_then(Value::as_u64) {
        return Err(ApiError::stale_version(have));
    }
    let changed = out.get(0).cloned().unwrap_or(Value::Null);
    let mut list = out.get(2).cloned().unwrap_or_else(|| json!({}));
    let site = |w: &str| if w.ends_with("teslafi") { "TeslaFi" } else { "TeslaMate" };
    let summary = if pair {
        format!("Showing the {} import for a span both TeslaMate and TeslaFi imports cover.", site(&shown))
    } else if shown == "can" || shown == "live" {
        "Showing live history for an overlap of live and imported history.".to_string()
    } else {
        "Showing imported history for an overlap of live and imported history.".to_string()
    };
    // (the engine's flip tells the live stream which rows changed: no second `rows` event here)
    let summary = format!("{} ({})", summary.trim_end_matches('.'), app.car_name_now(car).await) + ".";
    v.audit(&app, "car-overlap", Some(&target("car", Some(car), &id)), &summary).await?;
    if let Value::Object(m) = &mut list {
        m.insert("changed".into(), changed);
    }
    Ok(Json(list))
}

async fn car_storage(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let kinds = p.list("kinds", Some(&q::system::STORAGE_KINDS[..]), 4)?.map(|v| v.join(","));
    let per = period(&p)?;
    list_answer(&app, &p, "storage", move |e, pg| q::system::storage(e, car, kinds.as_deref(), &per, pg)).await
}

/// Free bytes on the disk that holds `dir` (the nearest folder of it that exists), or `None` when it can't be read.
fn free_bytes(dir: &std::path::Path) -> Option<u64> {
    let mut d = dir.to_path_buf();
    loop {
        if let Ok(s) = rustix::fs::statvfs(&d) {
            return Some(s.f_bavail.saturating_mul(s.f_frsize));
        }
        if !d.pop() {
            return None;
        }
    }
}

/// A car's health as `/cars/{car}/health` gives it (its default period), for the live stream's `health` event.
pub(crate) fn health_now(e: &Engine, car: model::CarId) -> q::QResult<Value> {
    let mut h = q::system::health(e, car, &q::Period::new(None, None))?;
    h.body.uploads_enabled = Some(uploads_enabled(e, car)?);
    Ok(serde_json::to_value(&h).unwrap_or(Value::Null))
}

/// The server's storage figures as `/system/storage` gives them, with compaction as `/system/storage/detail` has
/// it (no walk over the data folder), for the live stream's `storage` event.
pub(crate) fn storage_now(e: &Engine, now_ms: u64) -> q::QResult<Value> {
    let s = q::system::server_storage(e, now_ms)?;
    let free = e.store().dir().parent().and_then(free_bytes);
    let full_in_days = match (free, s.growth_per_day_bytes) {
        (Some(f), Some(g)) if g > 0 => Some(f / g),
        _ => None,
    };
    let mut v = serde_json::to_value(Storage { body: s, free_bytes: free, full_in_days }).unwrap_or(Value::Null);
    v["compaction"] = serde_json::to_value(e.compaction()).unwrap_or(Value::Null);
    Ok(v)
}

/// `/system/storage`: the engine's figures with the disk's free space.
#[derive(serde::Serialize)]
struct Storage {
    #[serde(flatten)]
    body: q::system::ServerStorage,
    free_bytes: Option<u64>,
    full_in_days: Option<u64>,
}

async fn system_storage(State(app): State<AppState>, p: Params) -> ApiResult<Response> {
    let now_ms = app.now();
    let e = app.service::<Engine>()?;
    let data = e.store().dir().parent().map(std::path::Path::to_path_buf);
    answer(&app, &p, "storage", move |e| {
        let s = q::system::server_storage(e, now_ms)?;
        let free = data.as_deref().and_then(free_bytes);
        let full_in_days = match (free, s.growth_per_day_bytes) {
            (Some(f), Some(g)) if g > 0 => Some(f / g),
            _ => None,
        };
        Ok(Storage { body: s, free_bytes: free, full_in_days })
    })
    .await
}

/// `/system/storage/detail`: what the data folder takes by category (one walk over it), the disk's free space, and
/// compaction as it is now. For the Storage panel of Settings: Server and access.
async fn storage_detail(State(app): State<AppState>, p: Params) -> ApiResult<Json<Value>> {
    p.wants_csv()?;
    let e = app.service::<Engine>()?;
    let body = app
        .blocking(move |_| {
            let categories = q::storage::categories(&e).map_err(super::now::failed)?;
            let free = e.store().dir().parent().and_then(free_bytes);
            Ok(json!({
                "total_bytes": categories.iter().map(|c| c.bytes).sum::<u64>(),
                "total_disk_bytes": categories.iter().map(|c| c.disk_bytes).sum::<u64>(),
                "categories": categories,
                "free_bytes": free,
                "compaction": e.compaction(),
            }))
        })
        .await?;
    Ok(Json(body))
}

/// `POST /system/storage/compact`: starts a compaction pass in the background unless one runs. Nothing is lost by
/// it (docs/plans/storage.md); the answer is the compaction as it is right after.
async fn storage_compact(State(app): State<AppState>, v: Viewer) -> ApiResult<Response> {
    let e = app.service::<Engine>()?;
    let was = e.compaction();
    let now = app
        .blocking(move |_| e.compact_start(false).map_err(|x| ApiError::server(format!("starting compaction: {x}"))))
        .await?;
    if was.status != engine::compact::Status::Running {
        v.audit(&app, "storage-compacted", Some("server:storage"), "Started packing the stored data (Compact now).")
            .await?;
    }
    Ok((axum::http::StatusCode::ACCEPTED, Json(json!({ "compaction": now }))).into_response())
}

async fn system_health(State(app): State<AppState>, p: Params) -> ApiResult<Response> {
    p.wants_csv()?;
    let t0 = Instant::now();
    let e = app.service::<Engine>()?;
    let uptime = STARTED.get().map(|s| s.elapsed().as_millis() as u64);
    let geocode = app.blocking({
        let e = e.clone();
        move |_| Ok(super::settings::load(&e, &[]).ok().map(|s| s.geocode.mode.clone()))
    });
    let geocode = geocode.await?;
    let (sh, cached) = app
        .blocking({
            let e = e.clone();
            move |_| Ok((q::system::system_health(&e), e.store().cached_cars()))
        })
        .await?;
    let mut versions = sh.versions.clone();
    if let Value::Object(m) = &mut versions {
        m.insert("server".into(), json!(SERVER_VERSION));
        m.insert("api".into(), json!(crate::API_VERSION));
    }
    let mut timings = sh.timings_ms.clone();
    if let Value::Object(m) = &mut timings {
        m.insert("api_p50".into(), Value::Null);
        m.insert("api_p95".into(), Value::Null);
    }
    let read_ms = (t0.elapsed().as_secs_f64() * 1000.0 * 100.0).round() / 100.0;
    let v = json!({
        "read_ms": read_ms,
        "uptime_ms": uptime,
        "versions": versions,
        "db": sh.db,
        "ingest": { "memory": { "used_bytes": null, "limit_bytes": null }, "cached_cars": cached },
        "engine": {
            "logs_decoded": sh.engine.logs_decoded,
            "logs_left_out": sh.engine.logs_left_out,
            "windows_rebuilt": sh.engine.windows_rebuilt,
            "queued": sh.engine.queued,
            "failures": sh.engine.failures,
            "last_error": sh.engine.last_error,
        },
        "geocoder": geocode.map(|m| json!({ "mode": m, "cache_size": null, "hits_pct": null })),
        "tesla": super::tesla::health(&app),
        "teslafi": super::teslafi::health(&app),
        "teslamate_local": super::teslamate_local::health(&app),
        "housekeeping_last_ms": null,
        "timings_ms": timings,
    });
    Ok(axum::response::IntoResponse::into_response(Json(v)))
}
