//! Restoring from an export file from the first page (docs/ui-api.md, `/setup/restore` and "Restore uploads"). The
//! browser sends the file's first 64 KiB with the recovery key (`POST`): the header and the key are checked before
//! a byte of the body comes. Then the chunks (`PATCH`), each opened and read into the staging folder as it arrives
//! ([`moving::Import::feed`]), so nothing is stored twice. After the last byte the import is ready, and the program
//! switches to it and starts again (`import_ready` in the binary).
//!
//! | Route | Who | What |
//! |---|---|---|
//! | `POST /setup/restore` | the claim | `{ name, size, recovery_key, header }`: the SetupJob (201), or the same file's (200) |
//! | `PATCH /setup/restore` | the claim | the next chunk (`Upload-Offset`, `Upload-Length`, `Upload-SHA256` optional) |
//!
//! `GET`/`DELETE /setup/job` show and give up a restore too ([`job`], [`give_up`]). One restore at a time: the open
//! import lives in [`crate::moving::MoveSite`]'s [`Slot`].

use crate::ctx::read_body;
use crate::error::{ApiError, ApiResult, Fields};
use crate::moving::{claimed, site};
use crate::resources::imports::{CHUNK_MAX, header_u64, sha_header_opt};
use crate::server::Conn;
use crate::{AppState, JsonBody};
use axum::Json;
use axum::extract::{Extension, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use zeroize::Zeroizing;

/// The largest export file a restore takes (64 TB).
pub const MAX_EXPORT: u64 = 64 << 40;
/// The first bytes of the file the `POST` sends (or all of it, when smaller).
pub const HEAD_BYTES: u64 = 65_536;
const UPLOAD_URL: &str = "/api/v1/setup/restore";

/// The restore being uploaded, if any (one at a time).
#[derive(Default)]
pub struct Slot {
    open: Mutex<Option<Restore>>,
    /// A chunk is being stored: another one meanwhile is `409 wrong_offset`.
    storing: AtomicBool,
    /// Where the upload is, readable while a chunk is being stored.
    received: AtomicU64,
}

impl Slot {
    fn lock(&self) -> MutexGuard<'_, Option<Restore>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One restore: the export file, the import reading it, and how it went.
pub(crate) struct Restore {
    id: String,
    name: String,
    size: u64,
    prefix_sha256: String,
    started_ms: u64,
    files_total: u64,
    /// `None` once it failed, or after a fault of this server (a `POST` of the same file opens it again).
    import: Option<moving::Import<'static>>,
    /// The problem it failed with (the file is at fault, or no space): nothing of it is kept.
    failed: Option<Value>,
    received: u64,
    files_done: u64,
    /// When this server began taking its chunks, and from which byte (for the speed).
    since: Option<(u64, u64)>,
    bytes_per_s: Option<u64>,
}

/// Holds [`Slot::storing`] while a chunk is stored.
struct Storing<'a>(&'a AtomicBool);

impl Drop for Storing<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn wrong_offset(at: u64) -> ApiError {
    ApiError::new(409, "wrong_offset", format!("The server holds the file up to byte {at}; send from there."))
        .with("offset", at)
        .header(http::HeaderName::from_static("upload-offset"), &at.to_string())
}

fn problem(code: &str, message: &str) -> Value {
    json!({ "code": code, "message": message, "retry_ms": null })
}

/// The problem of a restore the file (or the disk) ended: `None` for a fault of this server.
fn failure(e: &moving::Error) -> Option<Value> {
    let code = match e {
        moving::Error::CutShort { .. } => "export_cut_short",
        moving::Error::NoSpace { .. } => "no_space",
        e if e.is_the_file() => "export_damaged",
        _ => return None,
    };
    Some(problem(code, &e.to_string()))
}

/// The SetupJob of a restore.
#[allow(clippy::too_many_arguments)]
fn view(
    id: &str,
    state: &str,
    started_ms: Option<u64>,
    name: &str,
    size: u64,
    received: u64,
    files: (u64, u64),
    speed: Option<u64>,
    problem: Option<Value>,
    result: Option<Value>,
) -> Value {
    let eta = speed.filter(|&b| b > 0 && state == "uploading").map(|b| size.saturating_sub(received).div_ceil(b));
    json!({
        "id": id,
        "kind": "restore",
        "state": state,
        "started_ms": started_ms,
        "from": null,
        "file_name": name,
        "size": size,
        "received": received,
        "progress": { "round": 1, "files_done": files.0, "files_total": files.1, "bytes_done": received,
                      "bytes_total": size, "cars_done": null, "cars_total": null,
                      "bytes_per_s": speed.filter(|_| state == "uploading"), "eta_s": eta },
        "problem": problem,
        "result": result,
        "skipped": [],
        "log": [],
    })
}

/// A restore held here, as a SetupJob (`None`: it is gone, cancelled outside the web pages).
fn view_of(app: &AppState, s: &crate::moving::MoveSite, r: &Restore) -> Option<Value> {
    let (state, problem, result) = if let Some(p) = &r.failed {
        ("failed", Some(p.clone()), None)
    } else {
        match moving::import::pending(&s.data, &s.keys) {
            moving::Pending::Reading { .. } if r.received >= r.size => ("checking", None, None),
            moving::Pending::Reading { .. } => ("uploading", None, None),
            moving::Pending::Ready { .. } => ("checking", None, None),
            moving::Pending::Switching => ("switching", None, None),
            moving::Pending::Unreadable => {
                ("failed", Some(problem("unreadable", "The restore's state on this server can't be read.")), None)
            }
            moving::Pending::None => {
                let done = moving::import::imported_note(&s.data, &app.vault).filter(|n| n.export_id == r.id)?;
                ("done", None, Some(result_of(app, s, &done)))
            }
        }
    };
    Some(view(
        &r.id,
        state,
        Some(r.started_ms),
        &r.name,
        r.size,
        r.received,
        (r.files_done, r.files_total),
        r.bytes_per_s,
        problem,
        result,
    ))
}

/// The `result` of a restore that is done.
fn result_of(app: &AppState, s: &crate::moving::MoveSite, n: &moving::import::Imported) -> Value {
    let cars = app.service::<engine::Engine>().ok().and_then(|e| engine::query::all_cars(&e).ok()).map(|c| c.len());
    json!({ "at_ms": n.at_ms, "from": null, "file_name": n.name, "forced": false, "same_machine": false,
            "accounts": app.accounts.count(), "cars": cars, "cert_sha256": cert_sha256(&s.data) })
}

/// SHA-256 of the upload port's certificate (`tls/cert.pem`, the first one in it), lowercase hex.
pub(crate) fn cert_sha256(data: &std::path::Path) -> Option<String> {
    use base64::Engine as _;
    let pem = std::fs::read_to_string(data.join("tls").join("cert.pem")).ok()?;
    let body = pem.split("-----BEGIN CERTIFICATE-----").nth(1)?.split("-----END CERTIFICATE-----").next()?;
    let b64: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    let der = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    Some(hex::encode(Sha256::digest(der)))
}

/// The restore under way here as a SetupJob: the one held here, else one staged before a restart, else one this
/// data folder took (`done`). `None`: there is none.
pub(crate) fn job(app: &AppState, s: &crate::moving::MoveSite) -> ApiResult<Option<Value>> {
    let held = s.restore.lock();
    if let Some(r) = held.as_ref()
        && let Some(v) = view_of(app, s, r)
    {
        return Ok(Some(v));
    }
    drop(held);
    if let Some(st) = moving::import::staged(&s.data, &s.keys) {
        let state = match st.phase {
            moving::import::Phase::Reading => "uploading",
            moving::import::Phase::Ready => "checking",
            moving::import::Phase::Switching => "switching",
        };
        let started = (st.started_ms > 0).then_some(st.started_ms);
        let files = (st.files_done, st.files);
        return Ok(Some(view(&st.export_id, state, started, &st.name, st.size, st.received, files, None, None, None)));
    }
    let Some(n) = moving::import::imported_note(&s.data, &app.vault) else { return Ok(None) };
    let files = (n.files, n.files);
    let result = Some(result_of(app, s, &n));
    Ok(Some(view(&n.export_id, "done", None, &n.name, n.size, n.size, files, None, None, result)))
}

/// `DELETE /setup/job` of a restore (`job`: its SetupJob): the staging removed. `409 finished` from `switching` on.
pub(crate) fn give_up(app: &AppState, s: &crate::moving::MoveSite, job: &Value, ip: std::net::IpAddr) -> ApiResult<()> {
    if matches!(job["state"].as_str(), Some("switching" | "done")) {
        return Err(ApiError::conflict("finished", "It is switching to the restored data; it finishes by itself."));
    }
    let taken = s.restore.lock().take();
    drop(taken);
    s.restore.received.store(0, Ordering::SeqCst);
    moving::import::cancel(&s.data, &s.keys).map_err(|e| match e {
        moving::Error::Switching => {
            ApiError::conflict("finished", "It is switching to the restored data; it finishes by itself.")
        }
        e => ApiError::server(format!("cancelling the restore: {e}")),
    })?;
    let name = job["file_name"].as_str().unwrap_or("the export file");
    let details =
        crate::ctx::audit_details(Some(&crate::moving::job_target(job)), &format!("Gave up restoring {name}."));
    app.accounts.audit().record(None, Some(ip), "setup-restore-cancelled", &details)?;
    Ok(())
}

fn header_error(e: moving::Error) -> ApiError {
    let m = e.to_string();
    match e {
        moving::Error::NotAnExport | moving::Error::Malformed(_) => ApiError::new(
            422,
            "not_an_export",
            "This is not a Sentry Mate export file. Choose the .smexport file the old server made.",
        )
        .with_fields(Map::from_iter([("header".into(), json!("Not an export file."))])),
        moving::Error::NewerFormat(_) | moving::Error::NewerApp { .. } => ApiError::new(422, "export_too_new", m),
        moving::Error::CutShort { .. } => ApiError::new(422, "export_cut_short", m),
        e => ApiError::server(format!("reading an export's header: {e}")),
    }
}

/// `POST /setup/restore` `{ name, size, recovery_key, header }`.
pub(crate) async fn start(
    State(app): State<AppState>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Response> {
    body.only(&["name", "size", "recovery_key", "header"])?;
    let mut bad = Fields::new();
    let name: String = match body.get("name") {
        Some(Value::String(n)) if !n.trim().is_empty() => n.trim().chars().take(200).collect(),
        _ => {
            bad.add("name", "Required: the file's name.");
            String::new()
        }
    };
    let size = match body.get("size") {
        Some(Value::Number(n)) if n.is_u64() || n.is_i64() => n.as_u64(),
        _ => {
            bad.add("size", "Required: the file's size in bytes.");
            None
        }
    };
    let key = match body.get("recovery_key") {
        Some(Value::String(k)) => Zeroizing::new(k.clone()),
        _ => {
            bad.add("recovery_key", "Required: the old server's recovery key.");
            Zeroizing::new(String::new())
        }
    };
    let head = match body.get("header") {
        Some(Value::String(h)) => h.clone(),
        _ => {
            bad.add("header", "Required: the file's first 64 KiB (or all of it when smaller), base64.");
            String::new()
        }
    };
    if !bad.is_empty() {
        return Err(bad.into_bad_input());
    }
    let s = site(&app)?;
    let s2 = s.clone();
    let h2 = headers.clone();
    app.blocking(move |app| claimed(app, &s2, &h2).map(|_| ())).await?;
    let size = size
        .filter(|n| (1..=MAX_EXPORT).contains(n))
        .ok_or_else(|| ApiError::invalid("size", "Between 1 byte and 64 TB."))?;
    let head = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(head.trim())
            .map_err(|_| ApiError::bad_input("header", "Not base64."))?
    };
    let want = size.min(HEAD_BYTES);
    if head.len() as u64 != want {
        return Err(ApiError::bad_input("header", &format!("Send the first {want} bytes of the file.")));
    }
    if !vault::RecoveryKey::reads(&key) {
        return Err(ApiError::bad_input(
            "recovery_key",
            "This doesn't read as a recovery key: SMRK- and 13 groups of 4. Check for a missing group.",
        ));
    }
    let (out, fresh) = app
        .blocking(move |app| {
            claimed(app, &s, &headers)?;
            let ex = moving::ExportFile::from_head(&head, size, &s.app_version).map_err(header_error)?.named(&name);
            // the Argon2id settings come from the file, not yet proven: no more than this server's own
            if ex.header.recovery.kdf().m > vault::Kdf::STRONG.m.max(s.kdf.m) {
                return Err(header_error(moving::Error::NotAnExport));
            }
            // one try at a time (each takes up to 1 GiB), counted before it runs
            let Some(work) = s.key_work() else {
                return Err(ApiError::new(503, "busy", "A recovery key is being tried; try again in a few seconds.")
                    .header(http::header::RETRY_AFTER, "5"));
            };
            let tried = app.now();
            s.key_try(conn.ip, tried)?;
            let opened = ex.open_key(&key);
            if !matches!(opened, Err(moving::Error::WrongRecoveryKey)) {
                s.right_key(conn.ip, tried);
            }
            drop(work);
            let opened = match opened {
                Ok(v) => v,
                Err(moving::Error::WrongRecoveryKey) => {
                    return Err(ApiError::new(
                        422,
                        "wrong_recovery_key",
                        "That recovery key doesn't open this export. An export opens with the recovery key the old \
                         server had when it was made.",
                    )
                    .with_fields(Map::from_iter([("recovery_key".into(), json!("Doesn't open this file."))])));
                }
                Err(moving::Error::Vault(_)) => return Err(header_error(moving::Error::NotAnExport)),
                Err(e) => return Err(ApiError::server(format!("opening an export's key: {e}"))),
            };
            s.paused("restore-key-opened");
            // the key check takes long: from here to the restore's files on disk, no first admin is made, and one
            // made meanwhile is seen (the claim is checked again)
            let _gate = s.setup_gate();
            claimed(app, &s, &headers)?;
            let busy = || {
                ApiError::conflict("setup_busy", "A move or a restore into this server is under way. Cancel it first.")
            };
            if !matches!(moving::pull::pending(&s.data, &s.keys), moving::pull::Pending::None) {
                return Err(busy());
            }
            let mut held = s.restore.lock();
            let same = |id: &str, sha: &str, n: &str, sz: u64| {
                id == ex.header.id && sha == ex.prefix_sha256() && n == name && sz == size
            };
            if let Some(r) = held.as_ref()
                && r.failed.is_none()
                && r.import.as_ref().is_some_and(|i| i.phase() == moving::import::Phase::Reading)
                && same(&r.id, &r.prefix_sha256, &r.name, r.size)
            {
                // the same file again while it uploads: carry on from where it is
                let v = view_of(app, &s, r).unwrap_or(Value::Null);
                return Ok((v, false));
            }
            let opts = s.import_options();
            let pending = moving::import::pending(&s.data, &s.keys);
            let (imp, started, fresh) = match pending {
                moving::Pending::None => {
                    let imp =
                        moving::import::begin_streamed(&s.data, &s.keys, &ex, opened, opts).map_err(|e| match e {
                            moving::Error::InUse(_) | moving::Error::KeysInUse => {
                                ApiError::conflict("not_empty", e.to_string())
                            }
                            moving::Error::OtherImport | moving::Error::Switching | moving::Error::Unreadable => busy(),
                            e => ApiError::server(format!("beginning a restore: {e}")),
                        })?;
                    (imp, None, true)
                }
                moving::Pending::Reading { .. } => {
                    let st = moving::import::staged(&s.data, &s.keys);
                    match moving::import::resume(&s.data, &s.keys, &ex, opts) {
                        Ok(moving::Resume::Same(imp)) if st.as_ref().is_some_and(|st| st.name == name) => {
                            (*imp, st.map(|st| st.started_ms).filter(|&t| t > 0), false)
                        }
                        Ok(_) => return Err(busy()),
                        Err(e) => return Err(ApiError::server(format!("carrying on a restore: {e}"))),
                    }
                }
                _ => return Err(busy()),
            };
            let received = imp.received();
            let r = Restore {
                id: ex.header.id.clone(),
                name: name.clone(),
                size,
                prefix_sha256: ex.prefix_sha256().to_string(),
                started_ms: started.unwrap_or_else(|| app.now()),
                files_total: ex.header.files,
                files_done: imp.progress().files_done,
                import: Some(imp),
                failed: None,
                received,
                since: None,
                bytes_per_s: None,
            };
            s.restore.received.store(received, Ordering::SeqCst);
            let v = view_of(app, &s, &r).unwrap_or(Value::Null);
            *held = Some(r);
            drop(held);
            if fresh {
                let details = crate::ctx::audit_details(
                    Some(&crate::moving::job_target(&v)),
                    &format!("Started restoring {name} ({size} bytes)."),
                );
                app.accounts.audit().record(None, Some(conn.ip), "setup-restore-started", &details)?;
            }
            Ok((v, fresh))
        })
        .await?;
    let mut out = out;
    if let Value::Object(m) = &mut out {
        m.insert("upload_url".into(), json!(UPLOAD_URL));
        m.insert("chunk_max_bytes".into(), json!(CHUNK_MAX));
    }
    let status = if fresh { StatusCode::CREATED } else { StatusCode::OK };
    Ok((status, Json(out)).into_response())
}

/// `PATCH /setup/restore`: the next chunk of the file.
pub(crate) async fn chunk(State(app): State<AppState>, req: Request) -> ApiResult<Json<Value>> {
    let headers = req.headers().clone();
    // the claim before anything of the request is read
    let s = site(&app)?;
    let (s2, h2) = (s.clone(), headers.clone());
    app.blocking(move |app| claimed(app, &s2, &h2).map(|_| ())).await?;
    let ct = headers.get(http::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !ct.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/offset+octet-stream") {
        return Err(ApiError::bad_input("Content-Type", "Send the chunk as application/offset+octet-stream."));
    }
    let offset = header_u64(&headers, "Upload-Offset")?
        .ok_or_else(|| ApiError::bad_input("Upload-Offset", "Required: where this chunk starts, in bytes."))?;
    let length = header_u64(&headers, "Upload-Length")?;
    let want = sha_header_opt(&headers)?;
    let bytes = read_body(req, CHUNK_MAX).await?;
    if bytes.is_empty() {
        return Err(ApiError::bad_input("body", "Send the chunk's bytes."));
    }
    app.blocking(move |app| {
        claimed(app, &s, &headers)?;
        let slot = &s.restore;
        if slot.storing.swap(true, Ordering::SeqCst) {
            return Err(wrong_offset(slot.received.load(Ordering::SeqCst)));
        }
        let _storing = Storing(&slot.storing);
        let mut held = slot.lock();
        let Some(r) = held.as_mut() else {
            if moving::import::staged(&s.data, &s.keys).is_some() {
                return Err(ApiError::conflict(
                    "not_uploading",
                    "This server started again: choose the same file again to carry on.",
                ));
            }
            return Err(ApiError::new(404, "no_such_job", "No restore is under way. Choose the file again."));
        };
        if r.failed.is_none() && r.import.is_none() {
            // a fault of this server while a chunk was stored (not the file's): the import was let go
            return Err(ApiError::conflict(
                "not_uploading",
                "Storing the last chunk went wrong on this server: choose the same file again to carry on.",
            ));
        }
        let uploading =
            r.failed.is_none() && r.import.as_ref().is_some_and(|i| i.phase() == moving::import::Phase::Reading);
        if !uploading {
            let state = view_of(app, &s, r).and_then(|v| v["state"].as_str().map(str::to_string));
            let state = state.unwrap_or_else(|| "gone".into());
            return Err(ApiError::conflict(
                "not_uploading",
                format!("This restore is {state}; it takes no more bytes."),
            ));
        }
        if length.is_some_and(|l| l != r.size) {
            return Err(ApiError::invalid("Upload-Length", &format!("This file is {} bytes.", r.size)));
        }
        if offset != r.received {
            return Err(wrong_offset(r.received));
        }
        if want.is_some_and(|w| Sha256::digest(&bytes).as_slice() != w) {
            return Err(ApiError::new(
                422,
                "checksum_mismatch",
                "The chunk does not match its SHA-256. Send it again.",
            )
            .with_fields(Map::from_iter([("Upload-SHA256".into(), json!("Does not match the bytes sent."))])));
        }
        if offset + bytes.len() as u64 > r.size {
            return Err(ApiError::invalid("body", "More bytes than the file's size."));
        }
        let now = app.now();
        let Some(imp) = r.import.as_mut() else { return Err(ApiError::server("a restore without its import")) };
        match imp.feed(&bytes) {
            Ok(p) => {
                r.received = imp.received();
                r.files_done = p.files_done;
                let (t0, b0) = *r.since.get_or_insert((now, offset));
                if now > t0 && r.received > b0 {
                    r.bytes_per_s = Some((r.received - b0) * 1000 / (now - t0));
                }
            }
            Err(e) => {
                r.import = None;
                match failure(&e) {
                    Some(p) => {
                        tracing::warn!("restore: {e}");
                        r.failed = Some(p);
                    }
                    None => return Err(ApiError::server(format!("restore: storing a chunk: {e}"))),
                }
            }
        }
        slot.received.store(r.received, Ordering::SeqCst);
        Ok(Json(view_of(app, &s, r).unwrap_or(Value::Null)))
    })
    .await
}
