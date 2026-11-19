//! Moving a server and first-run set-up from the web pages (docs/ui-api.md, "Moving and first-run setup"; how each
//! call maps to the `moving` crate: docs/plans/move-server.md, "From the web UI"). The `moving` crate does the work;
//! this module gives it its routes, in blocking tasks.
//!
//! | Route | Who | What |
//! |---|---|---|
//! | `GET`/`POST`/`DELETE /system/move` | admin | this server's move to another: its state, a move code (password again), cancel |
//! | `POST /system/move/undo` | admin | after a move: take uploads here again (password again) |
//! | `GET`/`POST /system/exports`, `GET`/`DELETE /system/exports/{export}`, `GET .../file` | admin | an export file |
//! | `POST /setup/claim` | public, no account yet | the one-time set-up code: the claim cookie |
//! | `POST /setup/fresh` | the claim | start fresh: the first admin, the recovery key once |
//! | `POST /setup/move-here`, `POST /setup/move-here/finish` | the claim | a move into this server |
//! | `GET`/`DELETE /setup/job` | the claim | the move under way here |
//! | `POST /system/recovery-key/show`, `.../new`, `.../saved` | admin | the recovery key file ([`crate::recovery`]) |
//! | `POST`/`PATCH /setup/restore` | public | `501 not_built` (an export file read from chunks needs the moving crate's header check over bytes) |
//!
//! The service [`MoveSite`] (the data and keys folders, the server's version) is handed over by the program; a
//! server without it answers `503 not_ready`.

use crate::ctx::{MaybeViewer, target};
use crate::error::{ApiError, ApiResult};
use crate::server::Conn;
use crate::{Admin, AppState, JsonBody};
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zeroize::Zeroizing;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/system/move", get(system_move).post(make_code).delete(cancel_move))
        .route("/api/v1/system/move/undo", post(undo_move))
        .route("/api/v1/system/exports", get(exports).post(make_export))
        .route("/api/v1/system/exports/{export}", get(export_one).delete(remove_export))
        .route("/api/v1/system/exports/{export}/file", get(export_file))
        .route("/api/v1/setup/claim", post(claim))
        .route("/api/v1/setup/fresh", post(fresh))
        .route("/api/v1/setup/move-here", post(move_here))
        .route("/api/v1/setup/move-here/finish", post(move_finish))
        .route("/api/v1/setup/job", get(setup_job).delete(give_up_job))
        .route("/api/v1/setup/restore", post(setup_restore).patch(setup_restore))
}

/// The claim cookie (`__Host-`: the real server is HTTPS only).
pub const CLAIM_COOKIE: &str = "__Host-sentry-mate-setup";
/// A claim lasts this long from its last use.
pub const CLAIM_MS: u64 = 3_600_000;
/// Wrong set-up codes from one address in a minute before `429`.
pub const MAX_WRONG_CODES: usize = 10;
/// An export file is kept this long after it was made.
pub const EXPORT_KEPT_MS: u64 = 24 * 3_600_000;
/// The pause of an export's snapshot waits at most this long for the writes under way.
pub const EXPORT_WAIT: Duration = Duration::from_secs(30);

/// What the move side needs of the program: the data and keys folders and the server's version.
pub struct MoveSite {
    pub data: PathBuf,
    pub keys: PathBuf,
    pub app_version: String,
    /// The Argon2id settings a new recovery key is made with ([`vault::Kdf::STRONG`]; tests use a fast one).
    pub kdf: vault::Kdf,
    /// A new recovery key is being made (one at a time: each takes 1 GiB for a few seconds).
    pub(crate) making_key: std::sync::atomic::AtomicBool,
    state: Mutex<SiteState>,
}

#[derive(Default)]
struct SiteState {
    /// The claim: its token's SHA-256, its last use, and the set-up code it was made with (for the first admin).
    claim: Option<(String, u64, Zeroizing<String>)>,
    wrong: HashMap<std::net::IpAddr, Vec<u64>>,
    export: Option<ExportSlot>,
}

struct ExportSlot {
    id: String,
    made_ms: u64,
    /// `None` while preparing.
    snapshot: Option<Arc<moving::Snapshot>>,
    error: Option<String>,
    sent_bytes: u64,
}

impl MoveSite {
    pub fn new(data: PathBuf, keys: PathBuf, app_version: &str) -> MoveSite {
        MoveSite {
            data,
            keys,
            app_version: app_version.to_string(),
            kdf: vault::Kdf::STRONG,
            making_key: std::sync::atomic::AtomicBool::new(false),
            state: Mutex::new(SiteState::default()),
        }
    }

    /// The same, making new recovery keys with `kdf` (tests: [`vault::Kdf::FAST_FOR_TESTS`]).
    pub fn with_kdf(mut self, kdf: vault::Kdf) -> MoveSite {
        self.kdf = kdf;
        self
    }

    /// The recovery key file in the keys folder (`RECOVERY-KEY.txt`).
    pub fn recovery_file(&self) -> PathBuf {
        self.keys.join(moving::import::RECOVERY_FILE)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SiteState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub(crate) fn site(app: &AppState) -> ApiResult<Arc<MoveSite>> {
    app.service::<MoveSite>()
}

pub(crate) fn moving_error(e: moving::Error) -> ApiError {
    match e {
        moving::Error::MoveUnderWay(m) => ApiError::conflict("move_under_way", m),
        moving::Error::Finished(m) => ApiError::conflict("finished", m),
        moving::Error::Move(m) if m.contains("undo") => ApiError::conflict("moved", m),
        moving::Error::Move(m) => ApiError::conflict("not_ready", m),
        moving::Error::WrongRecoveryKey => ApiError::new(422, "wrong_recovery_key", e.to_string()),
        moving::Error::Gate(_) => ApiError::new(503, "busy", "The writes didn't pause in time; try again in a minute.")
            .header(header::RETRY_AFTER, "30"),
        e => ApiError::server(format!("moving: {e}")),
    }
}

/// The account's password again (and its two-step code when it has one on): `403 wrong_password` /
/// `403 wrong_two_step` with `fields`.
pub(crate) async fn confirm(app: &AppState, v: &crate::Viewer, body: &JsonBody) -> ApiResult<()> {
    let password = body.req_str("password")?.to_string();
    let code = body.str("two_step")?.map(str::to_string);
    let v = v.clone();
    app.blocking(move |app| match app.accounts.confirm(v.signed_in(), &password, code.as_deref()) {
        Ok(()) => Ok(()),
        Err(accounts::Error::WrongPassword | accounts::Error::WrongCredentials) => {
            Err(ApiError::new(403, "wrong_password", "That password is not right.")
                .with_fields(serde_json::Map::from_iter([("password".into(), json!("Not right."))])))
        }
        Err(accounts::Error::WrongCode) => {
            Err(ApiError::new(403, "wrong_two_step", "That two-step code is not right.")
                .with_fields(serde_json::Map::from_iter([("two_step".into(), json!("Not right."))])))
        }
        Err(e) => Err(e.into()),
    })
    .await
}

// ---------------------------------------------------------------- this server moving away

fn old_view(s: &MoveSite, app: &AppState) -> ApiResult<Value> {
    let st = moving::old::status(&s.data, &app.vault).map_err(moving_error)?;
    let now = app.now();
    let state = match (&st.moved, &st.code) {
        (Some(m), _) if m.switched_ms.is_some() => "done",
        (Some(_), _) => "moved",
        (None, None) => "none",
        (None, Some(c)) if c.claimed.is_some() => "copying",
        (None, Some(c)) if c.expires_ms <= now => "expired",
        (None, Some(_)) => "offered",
    };
    let code = st.code.as_ref().map(|c| {
        json!({ "id": c.id, "made_ms": c.expires_ms.saturating_sub(moving::old::CODE_VALID_MS),
                "expires_ms": c.expires_ms, "anywhere": c.anywhere, "wrong_tries": c.wrong })
    });
    let claim = st.code.as_ref().and_then(|c| c.claimed.as_ref());
    let peer = claim.map(
        |cl| json!({ "address": cl.from, "app": cl.app, "claimed_ms": cl.at_ms, "last_seen_ms": cl.last_seen_ms }),
    );
    let progress = claim.map(|cl| {
        json!({ "round": null, "files_done": null, "files_total": cl.files, "bytes_done": cl.sent_bytes,
                "bytes_total": cl.bytes, "cars_done": null, "cars_total": null, "bytes_per_s": null, "eta_s": null })
    });
    let moved = st.moved.as_ref().map(|m| json!({ "at_ms": m.at_ms, "to": m.to, "switched_ms": m.switched_ms }));
    let cancelled = st.last_cancelled.as_ref().map(|(at, why)| json!({ "at_ms": at, "why": why }));
    Ok(json!({
        "state": state, "code": code, "peer": peer, "progress": progress, "moved": moved,
        "last_cancelled": cancelled, "last_undone_ms": null, "skipped": [], "problem": null,
    }))
}

async fn system_move(State(app): State<AppState>, Admin(_): Admin) -> ApiResult<Json<Value>> {
    let s = site(&app)?;
    app.blocking(move |app| old_view(&s, app).map(Json)).await
}

/// `POST /system/move` `{ password, two_step?, anywhere?, replace? }`: a move code, shown this once.
async fn make_code(State(app): State<AppState>, Admin(v): Admin, body: JsonBody) -> ApiResult<Response> {
    body.only(&["password", "two_step", "anywhere", "replace"])?;
    let (anywhere, replace) = (body.bool("anywhere")?.unwrap_or(false), body.bool("replace")?.unwrap_or(false));
    confirm(&app, &v, &body).await?;
    let s = site(&app)?;
    let (mut out, id) = app
        .blocking(move |app| {
            let made = moving::old::make_code(&s.data, &app.vault, anywhere, replace).map_err(moving_error)?;
            let mut out = old_view(&s, app)?;
            if let Value::Object(m) = &mut out {
                m.insert("move_code".into(), json!(made.code.as_str()));
            }
            Ok((out, made.id))
        })
        .await?;
    let where_ = if anywhere { "from anywhere" } else { "on the home network" };
    v.audit(&app, "move-code-made", Some(&target("move", None, &id)), &format!("Made a move code, usable {where_}."))
        .await?;
    if let Value::Object(m) = &mut out {
        m.entry("move_code").or_insert(Value::Null);
    }
    Ok((StatusCode::CREATED, Json(out)).into_response())
}

async fn cancel_move(State(app): State<AppState>, Admin(v): Admin) -> ApiResult<Json<Value>> {
    let s = site(&app)?;
    let (out, what) = app
        .blocking(move |app| {
            let c = moving::old::cancel(&s.data, &app.vault).map_err(moving_error)?;
            let what = match c {
                moving::old::Cancelled::Nothing => "nothing",
                moving::old::Cancelled::Revoked { .. } => "revoked",
                moving::old::Cancelled::Ended { .. } => "ended",
            };
            let mut out = old_view(&s, app)?;
            if let Value::Object(m) = &mut out {
                m.insert("cancelled".into(), json!(what));
            }
            Ok((out, what))
        })
        .await?;
    if what != "nothing" {
        let summary = if what == "revoked" { "Revoked the move code." } else { "Ended the move under way." };
        v.audit(&app, "move-cancelled", None, summary).await?;
    }
    Ok(Json(out))
}

async fn undo_move(State(app): State<AppState>, Admin(v): Admin, body: JsonBody) -> ApiResult<Json<Value>> {
    body.only(&["password", "two_step", "confirm"])?;
    if body.bool("confirm")? != Some(true) {
        return Err(ApiError::bad_input("confirm", "Required: true (stop the new server first)."));
    }
    confirm(&app, &v, &body).await?;
    let s = site(&app)?;
    let out = app
        .blocking(move |app| {
            if moving::old::undo(&s.data, &app.vault).map_err(moving_error)?.is_none() {
                return Err(ApiError::conflict("not_moved", "This server has not moved."));
            }
            old_view(&s, app)
        })
        .await?;
    v.audit(&app, "move-undone", None, "Undid the move: this server takes uploads again.").await?;
    Ok(Json(out))
}

// ---------------------------------------------------------------- export files

fn export_view(e: &ExportSlot, now: u64) -> Value {
    let expires = e.made_ms + EXPORT_KEPT_MS;
    let state = match (&e.snapshot, &e.error) {
        (_, Some(_)) => "failed",
        (None, None) => "preparing",
        (Some(_), None) if now >= expires => "expired",
        (Some(_), None) => "ready",
    };
    let snap = e.snapshot.as_ref().filter(|_| state == "ready");
    json!({
        "id": e.id,
        "state": state,
        "file_name": snap.map(|s| s.file_name()),
        "files": snap.map(|s| s.header().files),
        "bytes": snap.map(|s| s.header().bytes),
        "size": snap.map(|s| s.size()),
        "sent_bytes": e.sent_bytes,
        "made_ms": e.made_ms,
        "expires_ms": snap.map(|_| expires),
        "url": snap.map(|_| format!("/api/v1/system/exports/{}/file", e.id)),
        "skipped": snap.map_or_else(Vec::new, |s| s.skips().named.iter()
            .map(|k| json!({ "path": k.path, "why": k.why })).collect::<Vec<_>>()),
        "error": e.error,
    })
}

/// Drops an expired snapshot (its folder goes with it).
fn tidy_export(st: &mut SiteState, now: u64) {
    if let Some(e) = &mut st.export
        && e.snapshot.is_some()
        && now >= e.made_ms + EXPORT_KEPT_MS
    {
        e.snapshot = None;
        e.error = Some("expired".into());
    }
}

async fn exports(State(app): State<AppState>, Admin(_): Admin) -> ApiResult<Json<Value>> {
    let s = site(&app)?;
    let now = app.now();
    let mut st = s.lock();
    tidy_export(&mut st, now);
    let items: Vec<Value> = st.export.iter().map(|e| export_view(e, now)).collect();
    Ok(Json(json!({ "items": items, "next": null })))
}

async fn make_export(State(app): State<AppState>, Admin(v): Admin, body: JsonBody) -> ApiResult<Response> {
    body.only(&["password", "two_step"])?;
    confirm(&app, &v, &body).await?;
    let s = site(&app)?;
    let now = app.now();
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).map_err(|e| ApiError::server(format!("an id: {e}")))?;
    let id = format!("ex{}", hex::encode(b));
    {
        let mut st = s.lock();
        if st.export.as_ref().is_some_and(|e| e.snapshot.is_none() && e.error.is_none()) {
            return Err(ApiError::conflict("export_preparing", "An export file is being made; wait for it."));
        }
        // the one there was goes (its snapshot is removed when the last download of it ends)
        st.export = Some(ExportSlot { id: id.clone(), made_ms: now, snapshot: None, error: None, sent_bytes: 0 });
    }
    let (s2, app2, id2) = (s.clone(), app.clone(), id.clone());
    tokio::task::spawn_blocking(move || {
        let gate = gate::Gate::open(&s2.data);
        let made = gate
            .map_err(moving::Error::from)
            .and_then(|g| moving::Snapshot::make(&s2.data, &app2.vault, &g, &s2.app_version, EXPORT_WAIT));
        let mut st = s2.lock();
        if let Some(e) = st.export.as_mut().filter(|e| e.id == id2) {
            match made {
                Ok(snap) => e.snapshot = Some(Arc::new(snap)),
                Err(err) => e.error = Some(err.to_string()),
            }
        }
    });
    v.audit(&app, "export-made", Some(&target("export", None, &id)), "Made an export file of the whole server.")
        .await?;
    let view = s.lock().export.as_ref().map(|e| export_view(e, now)).unwrap_or(Value::Null);
    Ok((StatusCode::CREATED, Json(view)).into_response())
}

fn the_export<'a>(st: &'a mut SiteState, id: &str) -> ApiResult<&'a mut ExportSlot> {
    st.export.as_mut().filter(|e| e.id == id).ok_or_else(|| ApiError::not_found("no_such_export"))
}

async fn export_one(State(app): State<AppState>, Admin(_): Admin, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let s = site(&app)?;
    let now = app.now();
    let mut st = s.lock();
    tidy_export(&mut st, now);
    Ok(Json(export_view(the_export(&mut st, &id)?, now)))
}

async fn remove_export(State(app): State<AppState>, Admin(v): Admin, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let s = site(&app)?;
    {
        let mut st = s.lock();
        the_export(&mut st, &id)?;
        st.export = None;
    }
    let d = s.data.clone();
    tokio::task::spawn_blocking(move || moving::export::remove_unused_snapshots(&d));
    v.audit(&app, "export-removed", Some(&target("export", None, &id)), "Removed the export file.").await?;
    Ok(Json(json!({ "deleted": true, "id": id })))
}

/// `bytes=a-` / `bytes=a-b` (one range).
fn range_of(h: &HeaderMap, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(v) = h.get(header::RANGE).and_then(|v| v.to_str().ok()) else { return Ok(None) };
    let Some(r) = v.trim().strip_prefix("bytes=") else { return Err(()) };
    let (a, b) = r.split_once('-').ok_or(())?;
    let (a, b) = (a.trim(), b.trim());
    let (from, to) = match (a.is_empty(), b.is_empty()) {
        (false, true) => (a.parse::<u64>().map_err(|_| ())?, size.saturating_sub(1)),
        (false, false) => {
            (a.parse::<u64>().map_err(|_| ())?, b.parse::<u64>().map_err(|_| ())?.min(size.saturating_sub(1)))
        }
        (true, false) => {
            let n = b.parse::<u64>().map_err(|_| ())?.min(size);
            (size - n, size.saturating_sub(1))
        }
        (true, true) => return Err(()),
    };
    if from >= size || from > to {
        return Err(());
    }
    Ok(Some((from, to)))
}

/// A body read from the export by a blocking thread, a piece at a time.
struct ExportBody(tokio::sync::mpsc::Receiver<Result<bytes::Bytes, std::io::Error>>);

impl futures_core::Stream for ExportBody {
    type Item = Result<bytes::Bytes, std::io::Error>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

async fn export_file(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let s = site(&app)?;
    let now = app.now();
    let snap = {
        let mut st = s.lock();
        tidy_export(&mut st, now);
        let e = st.export.as_mut().filter(|e| e.id == id).ok_or_else(|| ApiError::not_found("export_gone"))?;
        match (&e.snapshot, &e.error) {
            (Some(snap), None) => snap.clone(),
            (None, None) => return Err(ApiError::conflict("not_ready", "The export file is still being made.")),
            _ => return Err(ApiError::not_found("export_gone")),
        }
    };
    let size = snap.size();
    let etag = format!("\"{}\"", snap.header().id);
    let if_range_ok = headers.get(header::IF_RANGE).and_then(|v| v.to_str().ok()).is_none_or(|v| v.trim() == etag);
    let range = if if_range_ok {
        range_of(&headers, size).map_err(|()| {
            ApiError::new(416, "bad_range", "That range is not in the file.")
                .header(header::CONTENT_RANGE, &format!("bytes */{size}"))
        })?
    } else {
        None
    };
    let (from, to) = range.unwrap_or((0, size.saturating_sub(1)));
    let len = if size == 0 { 0 } else { to - from + 1 };
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let (vault, s2, id2) = (app.vault.clone(), s.clone(), id.clone());
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut r = match snap.reader(&vault, from) {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.blocking_send(Err(std::io::Error::other(e.to_string())));
                return;
            }
        };
        let mut left = len;
        let mut buf = vec![0u8; 64 * 1024];
        while left > 0 {
            let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            match r.read(&mut buf[..want]) {
                Ok(0) => break,
                Ok(n) => {
                    left -= n as u64;
                    if tx.blocking_send(Ok(bytes::Bytes::copy_from_slice(&buf[..n]))).is_err() {
                        return;
                    }
                    if let Some(e) = s2.lock().export.as_mut().filter(|e| e.id == id2) {
                        e.sent_bytes = e.sent_bytes.max(from + (len - left));
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            }
        }
    });
    if from == 0 {
        v.audit(&app, "export-downloaded", Some(&target("export", None, &id)), "Downloaded the export file.").await?;
    }
    let mut res = axum::body::Body::from_stream(ExportBody(rx)).into_response();
    *res.status_mut() = if range.is_some() { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK };
    let h = res.headers_mut();
    let name = snap_name(&s, &id).unwrap_or_else(|| "sentry-mate.smexport".into());
    let put = |h: &mut HeaderMap, k: header::HeaderName, v: String| {
        if let Ok(v) = HeaderValue::from_str(&v) {
            h.insert(k, v);
        }
    };
    put(h, header::CONTENT_TYPE, "application/octet-stream".into());
    put(h, header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""));
    put(h, header::CONTENT_LENGTH, len.to_string());
    put(h, header::ETAG, etag);
    put(h, header::ACCEPT_RANGES, "bytes".into());
    if range.is_some() {
        put(h, header::CONTENT_RANGE, format!("bytes {from}-{to}/{size}"));
    }
    Ok(res)
}

fn snap_name(s: &MoveSite, id: &str) -> Option<String> {
    let st = s.lock();
    st.export.as_ref().filter(|e| e.id == id).and_then(|e| e.snapshot.as_ref()).map(|s| s.file_name())
}

// ---------------------------------------------------------------- first-run set-up

fn sha(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// The set-up code as typed: case, spaces and dashes don't matter.
fn normal_code(text: &str) -> String {
    text.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

/// Equal in constant time (both are SHA-256 hex digests of the same length).
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn cookie_of(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|p| {
        let (k, v) = p.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

/// The claim this browser holds (renewed), with its set-up code: `401 not_claimed` without one, `409 setup_done`
/// once there is an account.
fn claimed(app: &AppState, s: &MoveSite, headers: &HeaderMap) -> ApiResult<Zeroizing<String>> {
    if !app.accounts.needs_setup() {
        return Err(ApiError::conflict("setup_done", "This server is set up: sign in."));
    }
    let token = cookie_of(headers, CLAIM_COOKIE);
    let now = app.now();
    let mut st = s.lock();
    match (&mut st.claim, token) {
        (Some((h, last, code)), Some(t)) if same(h, &sha(&t)) && now < *last + CLAIM_MS => {
            *last = now;
            Ok(code.clone())
        }
        _ => Err(ApiError::new(401, "not_claimed", "Type the set-up code from the server's log first.")),
    }
}

fn claim_cookie(token: &str) -> String {
    format!("{CLAIM_COOKIE}={token}; Max-Age={}; Path=/; Secure; HttpOnly; SameSite=Strict", CLAIM_MS / 1000)
}

/// The `/setup` answer of a browser (the claim's).
fn setup_answer(app: &AppState, s: &MoveSite, holds: bool) -> Value {
    let moving_here = matches!(moving::pull::pending(&s.data, &s.keys), moving::pull::Pending::Moving { .. });
    let restoring = !matches!(moving::import::pending(&s.data, &s.keys), moving::Pending::None);
    let state = if !app.accounts.needs_setup() {
        "set_up"
    } else if moving_here {
        "moving_here"
    } else if restoring {
        "restoring"
    } else {
        "fresh"
    };
    let st = s.lock();
    let claim_expires = st.claim.as_ref().map(|(_, last, _)| last + CLAIM_MS);
    drop(st);
    let job = if holds { job_view(app, s).ok().flatten() } else { None };
    json!({ "needs_setup": app.accounts.needs_setup(), "state": state, "claimed": holds,
            "claim_expires_ms": if holds { claim_expires } else { None }, "job": job })
}

/// `POST /setup/claim` `{ setup_code }`.
async fn claim(State(app): State<AppState>, Extension(conn): Extension<Conn>, body: JsonBody) -> ApiResult<Response> {
    body.only(&["setup_code"])?;
    let typed = normal_code(body.req_str("setup_code")?);
    let s = site(&app)?;
    let file = app.setup_code_file.clone().unwrap_or_else(|| app.dir.join("SETUP-CODE.txt"));
    app.blocking(move |app| {
        if !app.accounts.needs_setup() {
            return Err(ApiError::conflict("setup_done", "This server is set up: sign in."));
        }
        let now = app.now();
        {
            let mut st = s.lock();
            let w = st.wrong.entry(conn.ip).or_default();
            w.retain(|t| now.saturating_sub(*t) < 60_000);
            if w.len() >= MAX_WRONG_CODES {
                let wait = 60_000u64.saturating_sub(now.saturating_sub(w[0])).div_ceil(1000).max(1);
                return Err(ApiError::throttled(wait));
            }
        }
        let on_file = std::fs::read_to_string(&file).map(|t| normal_code(&t)).unwrap_or_default();
        if on_file.is_empty() || !same(&sha(&typed), &sha(&on_file)) {
            s.lock().wrong.entry(conn.ip).or_default().push(now);
            return Err(ApiError::new(403, "wrong_setup_code", "That is not the set-up code in the server's log."));
        }
        let mut t = [0u8; 32];
        getrandom::fill(&mut t).map_err(|e| ApiError::server(format!("a claim: {e}")))?;
        let token = hex::encode(t);
        s.lock().claim = Some((sha(&token), now, Zeroizing::new(on_file)));
        let mut res = Json(setup_answer(app, &s, true)).into_response();
        if let Ok(v) = HeaderValue::from_str(&claim_cookie(&token)) {
            res.headers_mut().append(header::SET_COOKIE, v);
        }
        Ok(res)
    })
    .await
}

/// `POST /setup/fresh` `{ name, password }`: the first admin, signed in, and the recovery key shown this once.
async fn fresh(
    State(app): State<AppState>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Response> {
    body.only(&["name", "password"])?;
    let (name, password) = (body.req_str("name")?.to_string(), body.req_str("password")?.to_string());
    let s = site(&app)?;
    let recovery_file = s.recovery_file();
    app.blocking(move |app| {
        let code = claimed(app, &s, &headers)?;
        if !matches!(moving::pull::pending(&s.data, &s.keys), moving::pull::Pending::None) {
            return Err(ApiError::conflict("setup_busy", "A move here runs: give it up first."));
        }
        let client = accounts::Client::new(conn.ip, &crate::policy::device(&headers));
        let local = client.local;
        let ns = app.accounts.setup(&code, &name, &password, &client)?;
        s.lock().claim = None;
        // the key itself, from the file's text (it says what the key is for around it)
        let recovery = moving::import::recovery_from_file(&recovery_file);
        let cars = app.accounts.car_access(ns.account.id)?;
        let body = json!({ "status": "done", "me": crate::auth::me_json(app, &ns.account, &cars, local),
                           "recovery_key": recovery.as_ref().map(|k| k.as_str()) });
        let mut res = (StatusCode::CREATED, Json(body)).into_response();
        if let Ok(v) = HeaderValue::from_str(&ns.set_cookie()) {
            res.headers_mut().append(header::SET_COOKIE, v);
        }
        Ok(res)
    })
    .await
}

/// The move under way here as a SetupJob (`None`: there is none).
fn job_view(_app: &AppState, s: &MoveSite) -> ApiResult<Option<Value>> {
    let Some(st) = moving::pull::status(&s.data, &s.keys).map_err(moving_error)? else { return Ok(None) };
    let state = match (st.phase, &st.problem) {
        (moving::pull::Phase::Switching, _) => "switching",
        (_, Some(p)) if p.retry => "waiting",
        (_, Some(_)) => "stopped",
        (moving::pull::Phase::Claiming, None) => "connecting",
        (moving::pull::Phase::Copying, None) if st.progress.round <= 1 => "copying",
        (moving::pull::Phase::Copying, None) => "catching_up",
    };
    let p = &st.progress;
    let problem = st.problem.as_ref().map(
        |p| json!({ "code": p.code, "message": p.message, "retry_ms": if p.retry { Some(60_000) } else { None } }),
    );
    let skipped: Vec<Value> = st.skipped.iter().map(|k| json!({ "path": k.path, "why": k.why })).collect();
    Ok(Some(json!({
        "id": st.code_id,
        "kind": "move_here",
        "state": state,
        "started_ms": st.started_ms,
        "from": st.from,
        "file_name": null,
        "size": null,
        "received": null,
        "progress": { "round": p.round, "files_done": p.files_done, "files_total": p.files_total,
                      "bytes_done": p.bytes_done, "bytes_total": p.bytes_total, "cars_done": null,
                      "cars_total": null, "bytes_per_s": null, "eta_s": null },
        "problem": problem,
        "result": null,
        "skipped": skipped,
    })))
}

/// The answers of `pull::begin`'s problems before anything runs.
fn begin_problem(p: moving::pull::Problem) -> ApiError {
    let m = p.message.clone();
    match p.code.as_str() {
        "address" => ApiError::bad_input("from", &m),
        "unreachable" | "not_proven" => ApiError::new(502, "old_server_unreachable", m),
        "wrong_code" => ApiError::new(422, "move_code_wrong", m),
        "code_expired" => ApiError::new(422, "move_code_expired", m),
        "code_used" => ApiError::conflict("move_code_used", m),
        "too_old" => ApiError::new(422, "move_too_old", m),
        "not_home_network" => ApiError::new(422, "move_not_home_network", m),
        "no_move" => ApiError::new(422, "move_not_offered", m),
        "not_empty" => ApiError::conflict("not_empty", m),
        "move_under_way" | "finished" => ApiError::conflict("setup_busy", m),
        "disk_times" | "no_space" | "disk" => ApiError::new(507, "no_space", m),
        _ => ApiError::new(502, "old_server_unreachable", m),
    }
}

/// `POST /setup/move-here` `{ from, code }`.
async fn move_here(State(app): State<AppState>, headers: HeaderMap, body: JsonBody) -> ApiResult<Response> {
    body.only(&["from", "code"])?;
    let (from, code) = (body.req_str("from")?.trim().to_string(), Zeroizing::new(body.req_str("code")?.to_string()));
    if from.starts_with("http://") {
        return Err(ApiError::bad_input("from", "Use the old server's address (https), not http://."));
    }
    if let Err(e) = moving::client::Address::parse(&from) {
        return Err(ApiError::bad_input("from", &e));
    }
    if let Err(e) = moving::code::MoveCode::parse(&code) {
        return Err(ApiError::bad_input("code", &e.to_string()));
    }
    let s = site(&app)?;
    app.blocking(move |app| {
        claimed(app, &s, &headers)?;
        let had = job_view(app, &s)?;
        let opts = moving::pull::Options::new(&s.app_version);
        moving::pull::begin(&s.data, &s.keys, &from, &code, &opts).map_err(begin_problem)?;
        // the server's own loop runs the move from here (`pull::run`), as for `move from`
        let job = job_view(app, &s)?.unwrap_or(Value::Null);
        let status = if had.is_some() { StatusCode::OK } else { StatusCode::CREATED };
        Ok((status, Json(job)).into_response())
    })
    .await
}

/// `POST /setup/move-here/finish` `{ recovery_key, confirm: true }`.
async fn move_finish(State(app): State<AppState>, headers: HeaderMap, body: JsonBody) -> ApiResult<Json<Value>> {
    body.only(&["recovery_key", "confirm"])?;
    if body.bool("confirm")? != Some(true) {
        return Err(ApiError::bad_input("confirm", "Required: true (the last changes may be missing)."));
    }
    let key = Zeroizing::new(body.req_str("recovery_key")?.trim().to_string());
    let s = site(&app)?;
    app.blocking(move |app| {
        claimed(app, &s, &headers)?;
        moving::pull::force_finish(&s.data, &s.keys, &key).map_err(|e| match e {
            moving::Error::Vault(_) => {
                ApiError::new(422, "wrong_recovery_key", "That recovery key doesn't open the copy.")
            }
            e => moving_error(e),
        })?;
        Ok(Json(job_view(app, &s)?.unwrap_or(Value::Null)))
    })
    .await
}

async fn setup_job(State(app): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let s = site(&app)?;
    app.blocking(move |app| {
        claimed(app, &s, &headers)?;
        job_view(app, &s)?.map(Json).ok_or_else(|| ApiError::not_found("no_such_job"))
    })
    .await
}

async fn give_up_job(State(app): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let s = site(&app)?;
    app.blocking(move |app| {
        claimed(app, &s, &headers)?;
        let job = job_view(app, &s)?.ok_or_else(|| ApiError::not_found("no_such_job"))?;
        moving::pull::cancel(&s.data, &s.keys).map_err(moving_error)?;
        Ok(Json(json!({ "deleted": true, "id": job["id"] })))
    })
    .await
}

// STUB: an export file read from its chunks as they arrive needs the moving crate's header checks over bytes
// (`import::open_file` reads a file today).
async fn setup_restore(MaybeViewer(_): MaybeViewer) -> ApiResult<()> {
    Err(ApiError::not_built("Restoring from an export file"))
}
