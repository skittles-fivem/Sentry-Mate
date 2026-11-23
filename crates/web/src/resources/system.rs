//! Server health and storage (docs/ui-api.md, "Server, settings and health"): `/about`, `/cars/{car}/health`,
//! `/cars/{car}/rebuild`, `/cars/{car}/overlaps`, `/cars/{car}/storage`, `/system/storage` and `/system/health`, over
//! the engine's [`engine::query::system`]. (`/health`, with no sign-in and no body, is the frame's own.)
//!
//! Reads of a car are for every account that sees it (the guard checks the `{car}`; a hidden car answers as an
//! unknown one); `/system/*` is admin-only (the guard's list). A rebuild and an overlap flip are an admin's changes,
//! recorded in the audit log, and tell the live stream which rows changed.

use super::now::{answer, list_answer, period, run};
use crate::ctx::target;
use crate::query::MAX_MS;
use crate::{ApiError, ApiResult, AppState, CarParam, JsonBody, Params, Viewer, ids};
use axum::extract::State;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use engine::Engine;
use engine::query as q;
use serde_json::{Value, json};
use std::sync::OnceLock;
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
    let mut found = 0;
    for key in keys {
        // low priority: every other task goes first between keys
        tokio::task::yield_now().await;
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
        let (n, k) = (scan.new.len(), key.clone());
        let _ = app
            .blocking(move |app| {
                let e = app.service::<Engine>()?;
                let car = q::all_cars(&e)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|&c| e.db().car(c).ok().flatten().is_some_and(|c| c.vehicle_keys.contains(&k)));
                let logs = if n == 1 { "1 CAN log".to_string() } else { format!("{n} CAN logs") };
                let (t, whose) = match car {
                    Some(c) => (Some(target("car", Some(c), "")), app.car_name(c)),
                    None => (None, "a CAN Bridge key not given to a car".to_string()),
                };
                let summary =
                    format!("Found {logs} of {whose} missing on the server; asked the car to send them again");
                let details = crate::ctx::audit_details(t.as_deref(), &summary);
                if let Err(err) = app.accounts.audit().record(None, None, "can-logs-missing", &details) {
                    tracing::warn!("audit: {err}");
                }
                Ok(())
            })
            .await;
    }
    if found > 0 {
        tracing::info!("missing CAN logs: {found} found missing on the server; the cars are asked to send them again");
    }
    found
}

pub fn routes() -> Router<AppState> {
    STARTED.get_or_init(Instant::now);
    Router::new()
        .route("/api/v1/about", get(about))
        .route("/api/v1/cars/{car}/health", get(health))
        .route("/api/v1/cars/{car}/rebuild", post(rebuild))
        .route("/api/v1/cars/{car}/overlaps", get(overlaps).patch(flip_overlap))
        .route("/api/v1/cars/{car}/storage", get(car_storage))
        .route("/api/v1/system/storage", get(system_storage))
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
/// (at most [`REBUILD_WAIT`]; beyond, it answers `status: "running"` and the job carries on).
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
    let job = app
        .blocking(move |_| {
            super::full_rebuild::refuse_while_running(&e2)?;
            e2.rebuild(engine::Scope { car: Some(car), from_ms: from, to_ms: to })
                .map_err(|x| ApiError::server(format!("starting the rebuild: {x}")))
        })
        .await?;
    let waited = Instant::now();
    let mut now = job.clone();
    while now.status == engine::JobStatus::Running && waited.elapsed() < REBUILD_WAIT {
        tokio::time::sleep(Duration::from_millis(50)).await;
        now = e.job(job.id).unwrap_or(now);
    }
    let status = match now.status {
        engine::JobStatus::Running => "running",
        engine::JobStatus::Done => "done",
        engine::JobStatus::Cancelled => "cancelled",
        engine::JobStatus::Failed => "failed",
    };
    if now.status == engine::JobStatus::Failed {
        return Err(ApiError::server(format!(
            "the rebuild failed: {}",
            now.error.clone().unwrap_or_else(|| "no reason given".into())
        )));
    }
    let kinds = ["drives", "charges", "states", "visits"];
    let (a, b) = (now.from_ms, now.to_ms.min(app.now().max(now.from_ms.saturating_add(1))));
    let days = b.saturating_sub(a).div_ceil(86_400_000).max(1);
    let name = app.car_name_now(car).await;
    let summary = match (from, to) {
        (None, None) => {
            format!("Rebuilt {name}'s drives, charges, states and visits from the stored logs (the whole history).")
        }
        _ => format!("Rebuilt {name}'s drives, charges, states and visits from the stored logs ({days} days)."),
    };
    // (the engine's rebuild tells the live stream of each window it made again: no second `rows` event here)
    v.audit(&app, "car-rebuild", Some(&target("car", Some(car), "")), &summary).await?;
    Ok(Json(json!({
        "car": ids::car(car),
        "job": now.id.to_string(),
        "status": status,
        "from_ms": a,
        "to_ms": b,
        "kinds": kinds,
        "logs_read": now.done,
        "logs_total": now.total,
        "took_ms": now.took_ms,
    })))
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
