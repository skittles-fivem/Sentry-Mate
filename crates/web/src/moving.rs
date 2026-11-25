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
//! | `GET`/`DELETE /setup/job` | the claim | the move or restore under way here |
//! | `POST /system/recovery-key/show`, `.../new`, `.../saved` | admin | the recovery key file ([`crate::recovery`]) |
//! | `POST`/`PATCH /setup/restore` | the claim | a restore from an export file, uploaded in chunks ([`crate::restore`]) |
//!
//! The service [`MoveSite`] (the data and keys folders, the server's version) is handed over by the program; a
//! server without it answers `503 not_ready`.

use crate::ctx::target;
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
        .route("/api/v1/setup/restore", post(crate::restore::start).patch(crate::restore::chunk))
}

/// The claim cookie (`__Host-`: the real server is HTTPS only).
pub const CLAIM_COOKIE: &str = "__Host-sentry-mate-setup";
/// A claim lasts this long from its last use.
pub const CLAIM_MS: u64 = 3_600_000;
/// Wrong set-up codes from one address in a minute before `429`.
pub const MAX_WRONG_CODES: usize = 10;
/// An export file is kept this long after it was made or last downloaded ...
pub const EXPORT_KEPT_MS: u64 = 24 * 3_600_000;
/// ... and never longer than this after it was made.
pub const EXPORT_KEPT_MOST_MS: u64 = 7 * EXPORT_KEPT_MS;
/// The pause of an export's snapshot waits at most this long for the writes under way.
pub const EXPORT_WAIT: Duration = Duration::from_secs(30);
/// An export (or its checksum) that did nothing for this long is `stalled` (the page says so).
pub const EXPORT_STALL_MS: u64 = 60_000;

/// What the move side needs of the program: the data and keys folders and the server's version.
pub struct MoveSite {
    pub data: PathBuf,
    pub keys: PathBuf,
    pub app_version: String,
    /// The Argon2id settings a new recovery key is made with ([`vault::Kdf::STRONG`]; tests use a fast one).
    pub kdf: vault::Kdf,
    /// A recovery key is being made or tried (one Argon2id at a time: each takes up to 1 GiB for a few seconds).
    pub(crate) making_key: std::sync::atomic::AtomicBool,
    /// The claim, kept by the program across the web side's restarts (a restore's switch starts it again).
    claim: ClaimKeep,
    /// The restore being uploaded ([`crate::restore`]: one at a time).
    pub(crate) restore: crate::restore::Slot,
    /// The options a restore runs with (`None`: the usual ones for this version).
    import_opts: Option<Arc<moving::Options>>,
    state: Mutex<SiteState>,
    /// The export file kept across a restart is looked for once ([`MoveSite::recover_export`]).
    recovered: std::sync::Once,
    /// Held while the first admin is made and while a move or a restore is begun ([`MoveSite::setup_gate`]).
    setup_gate: Mutex<()>,
    /// Tests: called at named points (see [`MoveSite::with_pause`]).
    pause: Option<Pause>,
}

/// A test's hook ([`MoveSite::with_pause`]).
type Pause = Arc<dyn Fn(&str) + Send + Sync>;

/// The claim: its token's SHA-256, its last use, and the set-up code it was made with (for the first admin).
type Claim = (String, u64, Zeroizing<String>);

/// Where the claim is held. The program makes one when it starts and hands it to each new web side
/// ([`MoveSite::keeping_claim`]), so the browser that restored an export or moved the data here still holds its claim
/// after the switch and can read the job's `done` and `result`. The move's speed is kept here too.
#[derive(Clone, Default)]
pub struct ClaimKeep(Arc<Mutex<Option<Claim>>>, Arc<Mutex<Option<Rate>>>);

/// The move's speed: the last look at its bytes (job id, when, bytes done) and the speed it gave.
struct Rate {
    job: String,
    at_ms: u64,
    bytes: u64,
    per_s: Option<u64>,
}

/// A speed is worked out from two looks at least this far apart.
const RATE_MS: u64 = 2_000;

impl ClaimKeep {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Claim>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The bytes per second of the move `job` at `bytes` done (`None` until two looks were far enough apart).
    fn speed(&self, job: &str, now: u64, bytes: u64) -> Option<u64> {
        let mut r = self.1.lock().unwrap_or_else(|e| e.into_inner());
        match r.as_mut() {
            Some(x) if x.job == job && bytes >= x.bytes => {
                let dt = now.saturating_sub(x.at_ms);
                if dt >= RATE_MS {
                    x.per_s = Some((bytes - x.bytes).saturating_mul(1000) / dt);
                    x.at_ms = now;
                    x.bytes = bytes;
                }
                x.per_s
            }
            _ => {
                *r = Some(Rate { job: job.to_string(), at_ms: now, bytes, per_s: None });
                None
            }
        }
    }
}

#[derive(Default)]
struct SiteState {
    wrong: HashMap<std::net::IpAddr, Vec<u64>>,
    /// Wrong recovery keys from each address (a restore's), the same way.
    wrong_keys: HashMap<std::net::IpAddr, Vec<u64>>,
    export: Option<ExportSlot>,
}

struct ExportSlot {
    id: String,
    made_ms: u64,
    /// `None` while preparing.
    snapshot: Option<Arc<moving::Snapshot>>,
    error: Option<String>,
    /// Why it failed: a code and what to do.
    problem: Option<(&'static str, &'static str)>,
    /// How far the making, then the checksum, got; cancelled when the slot goes.
    watch: Arc<moving::Watch>,
    /// The file's SHA-256, once worked out (after it is ready).
    sha256: Option<String>,
    /// Why the checksum could not be worked out.
    checksum_error: Option<String>,
    sent_bytes: u64,
    /// When a download of it last started.
    last_download_ms: Option<u64>,
}

impl ExportSlot {
    /// When the file goes: a day after it was made or last downloaded, a week after it was made at most.
    fn expires_ms(&self) -> u64 {
        let from = self.last_download_ms.unwrap_or(0).max(self.made_ms);
        (from + EXPORT_KEPT_MS).min(self.made_ms + EXPORT_KEPT_MOST_MS)
    }
}

impl MoveSite {
    pub fn new(data: PathBuf, keys: PathBuf, app_version: &str) -> MoveSite {
        MoveSite {
            data,
            keys,
            app_version: app_version.to_string(),
            kdf: vault::Kdf::STRONG,
            making_key: std::sync::atomic::AtomicBool::new(false),
            claim: ClaimKeep::default(),
            restore: crate::restore::Slot::default(),
            import_opts: None,
            state: Mutex::new(SiteState::default()),
            recovered: std::sync::Once::new(),
            setup_gate: Mutex::new(()),
            pause: None,
        }
    }

    /// Tests only: `f` is called (on a blocking thread, so it may wait) at these points: `restore-key-opened` (a
    /// restore's recovery key opened its file, nothing begun yet), `export-start` (an export file's job, before it
    /// counts) and `setup-ready` (`POST /setup`: the code is right and nothing runs, the password not yet hashed).
    #[doc(hidden)]
    pub fn with_pause(mut self, f: impl Fn(&str) + Send + Sync + 'static) -> MoveSite {
        self.pause = Some(Arc::new(f));
        self
    }

    pub(crate) fn paused(&self, at: &str) {
        if let Some(f) = &self.pause {
            f(at);
        }
    }

    /// One at a time: the first admin made (from its busy check to its account), or a move or a restore begun (from
    /// its `needs_setup` check to its files on disk). Without it a restore could begin while the first admin is made,
    /// and be left half done on a server that is set up, where the web pages can't carry it on or give it up.
    pub(crate) fn setup_gate(&self) -> std::sync::MutexGuard<'_, ()> {
        self.setup_gate.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The same, holding the claim in `keep` (the program's, kept across restarts of the web side).
    pub fn keeping_claim(mut self, keep: ClaimKeep) -> MoveSite {
        self.claim = keep;
        self
    }

    /// The same, restoring export files with `opts` (tests: no free space kept, small checkpoints).
    pub fn with_import_options(mut self, opts: moving::Options) -> MoveSite {
        self.import_opts = Some(Arc::new(opts));
        self
    }

    /// The options a restore from the web pages runs with: the web side's own first-start files (no account yet)
    /// don't count as the owner's data, and are set aside at the switch.
    pub(crate) fn import_options(&self) -> Arc<moving::Options> {
        if let Some(o) = &self.import_opts {
            return o.clone();
        }
        let mut o = moving::Options::new(&self.app_version);
        o.first_start_also = moving::import::WEB_FIRST_START.iter().map(|s| s.to_string()).collect();
        Arc::new(o)
    }

    /// A recovery key try from `ip`, counted at once as a wrong one (checked and counted under one lock, so tries
    /// sent at the same moment can't all pass): `429 throttled` once [`MAX_WRONG_CODES`] came in the last minute.
    /// [`MoveSite::right_key`] takes the count back when the key opens.
    pub(crate) fn key_try(&self, ip: std::net::IpAddr, now: u64) -> ApiResult<()> {
        let mut st = self.lock();
        let w = st.wrong_keys.entry(ip).or_default();
        w.retain(|t| now.saturating_sub(*t) < 60_000);
        if w.len() >= MAX_WRONG_CODES {
            let wait = 60_000u64.saturating_sub(now.saturating_sub(w[0])).div_ceil(1000).max(1);
            return Err(ApiError::throttled(wait));
        }
        w.push(now);
        Ok(())
    }

    /// The try [`MoveSite::key_try`] counted at `now` was not a wrong key.
    pub(crate) fn right_key(&self, ip: std::net::IpAddr, now: u64) {
        let mut st = self.lock();
        let w = st.wrong_keys.entry(ip).or_default();
        if let Some(i) = w.iter().rposition(|t| *t == now) {
            w.remove(i);
        }
    }

    /// Holds the one Argon2id run (a recovery key made or tried) until dropped: `None` while another runs.
    pub fn key_work(&self) -> Option<KeyWork<'_>> {
        if self.making_key.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return None;
        }
        Some(KeyWork(self))
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

/// The one Argon2id run ([`MoveSite::key_work`]).
pub struct KeyWork<'a>(&'a MoveSite);

impl Drop for KeyWork<'_> {
    fn drop(&mut self) {
        self.0.making_key.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Whether this server's data moved away (its write gate held for good, from the moment of the move's commit): what
/// runs here on its own writes nothing then (the upkeep, the invoices' audit, the MQTT publisher, an import).
pub fn data_moved(app: &AppState) -> bool {
    match app.service::<moving::old::Offer>() {
        Ok(o) => o.is_moved(),
        Err(_) => site(app).is_ok_and(|s| moving::old::is_moved(&s.data)),
    }
}

pub(crate) fn site(app: &AppState) -> ApiResult<Arc<MoveSite>> {
    let s = app.service::<MoveSite>()?;
    s.recovered.call_once(|| s.recover_export(app));
    Ok(s)
}

/// Takes up the export file a restart left (done once; the first use of the move side does it too). The program
/// calls it at its start, so a file that expired while the server was stopped is removed then.
pub fn recover(app: &AppState) {
    let _ = site(app);
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
        (None, Some(c)) if c.claimed.is_some() && paused(app) => "paused",
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
        "last_cancelled": cancelled, "last_undone_ms": st.last_undone_ms, "skipped": [], "problem": null,
        "log": log_view(&moving::old::log(&s.data, &app.vault)),
    }))
}

/// The move's log as the pages get it: `[{ at_ms, text }]`, oldest first (nothing secret in it: [`moving::log`]).
pub(crate) fn log_view(log: &[moving::log::Line]) -> Value {
    Value::Array(log.iter().map(|l| json!({ "at_ms": l.at_ms, "text": l.text })).collect())
}

/// The uploads are paused for the move's last round (the running server's [`moving::old::Offer`], when it has one).
fn paused(app: &AppState) -> bool {
    app.service::<moving::old::Offer>().is_ok_and(|o| o.is_paused())
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
            let (what, id) = match c {
                moving::old::Cancelled::Nothing => ("nothing", String::new()),
                moving::old::Cancelled::Revoked { id } => ("revoked", id),
                moving::old::Cancelled::Ended { id, .. } => ("ended", id),
            };
            let mut out = old_view(&s, app)?;
            if let Value::Object(m) = &mut out {
                m.insert("cancelled".into(), json!(what));
            }
            Ok((out, (what, id)))
        })
        .await?;
    let (what, id) = what;
    if what == "nothing" {
        // there was nothing to cancel: nothing changed
        v.mark_audited();
    } else {
        let summary = if what == "revoked" { "Revoked the move code." } else { "Ended the move under way." };
        v.audit(&app, "move-cancelled", Some(&target("move", None, &id)), summary).await?;
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
            let Some(moved) = moving::old::undo(&s.data, &app.vault).map_err(moving_error)? else {
                return Err(ApiError::conflict("not_moved", "This server has not moved."));
            };
            Ok((old_view(&s, app)?, moved.code_id))
        })
        .await?;
    let (out, id) = out;
    v.audit(&app, "move-undone", Some(&target("move", None, &id)), "Undid the move: this server takes uploads again.")
        .await?;
    Ok(Json(out))
}

// ---------------------------------------------------------------- export files

/// The step of making an export (1 to 3) of a watch's phase.
fn export_step(p: moving::Phase) -> u64 {
    match p {
        moving::Phase::Counting => 1,
        moving::Phase::Waiting => 2,
        _ => 3,
    }
}

/// A watch's progress as the API gives it: done and total, the speed, the time left, and whether it stalled.
fn progress_view(seen: &moving::Seen, now: u64) -> Value {
    let known = seen.bytes_total > 0 || seen.files_total > 0;
    let percent = known.then(|| {
        if seen.bytes_total > 0 {
            (seen.bytes_done as f64 * 1000.0 / seen.bytes_total as f64).round() / 10.0
        } else {
            (seen.files_done as f64 * 1000.0 / seen.files_total.max(1) as f64).round() / 10.0
        }
    });
    let per_s = seen.bytes_per_s;
    let eta = match per_s {
        Some(r) if r > 0 && known => Some(seen.bytes_total.saturating_sub(seen.bytes_done).div_ceil(r)),
        _ => None,
    };
    let quiet = now.saturating_sub(seen.progress_ms);
    // the moment between the snapshot's end and the file shown ready is still the snapshot's step
    let phase = if seen.phase == moving::Phase::Done { "collecting" } else { seen.phase.name() };
    json!({
        "phase": phase,
        "step": export_step(seen.phase),
        "steps": 3,
        "files_done": seen.files_done,
        "files_total": known.then_some(seen.files_total),
        "bytes_done": seen.bytes_done,
        "bytes_total": known.then_some(seen.bytes_total),
        "percent": percent,
        "bytes_per_s": per_s,
        "eta_s": eta,
        "started_ms": seen.started_ms,
        "phase_started_ms": seen.phase_ms,
        "progress_ms": seen.progress_ms,
        "quiet_ms": quiet,
        "stalled": quiet >= EXPORT_STALL_MS,
    })
}

/// A problem's code and what the owner can do (a saved export file names its problem by the code).
fn problem(code: &str) -> (&'static str, &'static str) {
    match code {
        "no_space" => ("no_space", "Free some space on the data disk, then make the export again."),
        "no_hard_links" => (
            "no_hard_links",
            "Use a move code instead (it needs no hard links), or keep the data folder on a disk that has them.",
        ),
        "writes_busy" => (
            "writes_busy",
            "Something kept writing for 30 seconds (an import, a big upload): try again in a few minutes.",
        ),
        "server_stopping" => ("server_stopping", "The server is stopping or moved: make the export once it runs."),
        "changed" => ("changed", "Make the export again."),
        "cancelled" => ("cancelled", "Make a new one when you want it."),
        "disk" => {
            ("disk", "The disk could not be read or written: look at the server's log, then make the export again.")
        }
        "server_restarted" => {
            ("server_restarted", "The server restarted while the export file was being made: make it again.")
        }
        _ => ("server_error", "Look at the server's log, then make the export again."),
    }
}

/// What a failed export's error is: a code, and what the owner can do.
fn export_problem(e: &moving::Error) -> (&'static str, &'static str) {
    problem(match e {
        moving::Error::NoSpace { .. } => "no_space",
        moving::Error::Io(io) if io.kind() == std::io::ErrorKind::StorageFull => "no_space",
        moving::Error::NoHardLinks(_) => "no_hard_links",
        moving::Error::Gate(gate::Refused::Busy) => "writes_busy",
        moving::Error::Gate(_) => "server_stopping",
        moving::Error::Changed(_) => "changed",
        moving::Error::Cancelled => "cancelled",
        moving::Error::Io(_) => "disk",
        _ => "server_error",
    })
}

fn export_view(e: &ExportSlot, now: u64) -> Value {
    let expires = e.expires_ms();
    let state = match (&e.snapshot, &e.error) {
        (_, Some(err)) if err == EXPIRED => "expired",
        (_, Some(_)) => "failed",
        (None, None) => "preparing",
        (Some(_), None) if now >= expires => "expired",
        (Some(_), None) => "ready",
    };
    let snap = e.snapshot.as_ref().filter(|_| state == "ready");
    let seen = e.watch.seen();
    let progress = (state == "preparing").then(|| progress_view(&seen, now));
    let checksum = snap.map(|_| {
        let working = e.sha256.is_none() && e.checksum_error.is_none();
        let mut c = if working && seen.phase == moving::Phase::Checking {
            progress_view(&seen, now)
        } else {
            json!({ "bytes_done": null, "bytes_total": null, "percent": null, "bytes_per_s": null, "eta_s": null,
                    "quiet_ms": null, "stalled": false })
        };
        c["state"] = json!(if e.sha256.is_some() {
            "done"
        } else if e.checksum_error.is_some() {
            "failed"
        } else {
            "working"
        });
        c["error"] = json!(e.checksum_error);
        c
    });
    let phase = match &progress {
        Some(p) => p["phase"].as_str().unwrap_or("counting"),
        None => state,
    };
    json!({
        "id": e.id,
        "state": state,
        "phase": phase,
        "progress": progress,
        "checksum": checksum,
        "sha256": snap.and(e.sha256.as_ref()),
        "problem": e.problem.filter(|_| state == "failed").map(|(code, what)| json!({
            "code": code, "message": e.error, "what_to_do": what,
        })),
        "now_ms": now,
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

/// The `error` of an export file that was kept its time.
const EXPIRED: &str = "expired";

/// Drops an expired snapshot (its folder goes with it, once no download holds it) and saves that.
fn tidy_export(s: &MoveSite, vault: &vault::Vault, st: &mut SiteState, now: u64) {
    if let Some(e) = &mut st.export
        && e.snapshot.is_some()
        && now >= e.expires_ms()
    {
        e.watch.cancel();
        if let Some(snap) = e.snapshot.take() {
            snap.keep_on_drop(false);
        }
        e.error = Some(EXPIRED.into());
        s.save_best_effort(vault, e);
    }
}

// ---------------------------------------------------------------- the export file across a restart

/// Where the state of the export file is kept (sealed, beside its snapshot's folder).
const SLOT_FILE: &str = "slot.sealed";
const SLOT_LABEL: &str = ".export/slot.sealed";

/// What `data/.export/slot.sealed` holds: enough to take the export file up again after a restart.
#[derive(serde::Serialize, serde::Deserialize)]
struct SlotFile {
    v: u32,
    id: String,
    made_ms: u64,
    /// The snapshot's folder; `None` while preparing, failed or expired.
    snapshot: Option<String>,
    /// `preparing`, `ready`, `failed` or `expired`.
    state: String,
    last_download_ms: Option<u64>,
    sent_bytes: u64,
    sha256: Option<String>,
    checksum_error: Option<String>,
    problem_code: Option<String>,
    error: Option<String>,
}

/// [`moving::Snapshot::reopen`], waiting a few seconds while the folder is still in use: a web side that stopped in
/// this process (a restart without leaving the program) may hold it until its last download or job lets go.
fn reopen_waiting(data: &std::path::Path, vault: &vault::Vault, hid: &str) -> Result<moving::Snapshot, String> {
    let mut tries = 0;
    loop {
        match moving::Snapshot::reopen(data, vault, hid) {
            Err(e) if e.to_string().contains("in use") && tries < 40 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            r => return r.map_err(|e| e.to_string()),
        }
    }
}

impl MoveSite {
    fn slot_path(&self) -> PathBuf {
        self.data.join(moving::export::SNAPSHOTS).join(SLOT_FILE)
    }

    /// Writes the export file's state whole or not at all.
    fn save_slot(&self, vault: &vault::Vault, e: &ExportSlot) -> Result<(), String> {
        let state = match (&e.error, &e.snapshot) {
            (Some(err), _) if err == EXPIRED => "expired",
            (Some(_), _) => "failed",
            (None, Some(_)) => "ready",
            (None, None) => "preparing",
        };
        let f = SlotFile {
            v: 1,
            id: e.id.clone(),
            made_ms: e.made_ms,
            snapshot: e.snapshot.as_ref().map(|s| s.header().id.clone()),
            state: state.into(),
            last_download_ms: e.last_download_ms,
            sent_bytes: e.sent_bytes,
            sha256: e.sha256.clone(),
            checksum_error: e.checksum_error.clone(),
            problem_code: e.problem.map(|(code, _)| code.to_string()),
            error: e.error.clone(),
        };
        let json = serde_json::to_vec(&f).map_err(|e| e.to_string())?;
        vault.write_file(&self.slot_path(), SLOT_LABEL, &json).map_err(|e| e.to_string())
    }

    /// [`MoveSite::save_slot`] where a failure only goes to the log.
    fn save_best_effort(&self, vault: &vault::Vault, e: &ExportSlot) {
        if let Err(err) = self.save_slot(vault, e) {
            tracing::warn!("the export file's state could not be saved: {err}");
        }
    }

    /// The export `id`'s state saved, if it is still the one there is.
    fn persist(&self, vault: &vault::Vault, id: &str) {
        let st = self.lock();
        if let Some(e) = st.export.as_ref().filter(|e| e.id == id) {
            self.save_best_effort(vault, e);
        }
    }

    fn load_slot(&self, vault: &vault::Vault) -> Result<Option<SlotFile>, String> {
        let Some(b) = vault.read_file(&self.slot_path(), SLOT_LABEL).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let f: SlotFile = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
        if f.v != 1 {
            return Err(format!("a state file of version {}", f.v));
        }
        Ok(Some(f))
    }

    /// The export gone for good: its snapshot's folder goes when the last download of it ends, and its state file now.
    fn retire(&self, e: &ExportSlot) {
        e.watch.cancel();
        if let Some(snap) = &e.snapshot {
            snap.keep_on_drop(false);
        }
    }

    fn remove_slot_file(&self) {
        if let Err(err) = std::fs::remove_file(self.slot_path())
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("the export file's state could not be removed: {err}");
        }
    }

    /// At the start (once): takes up what the last run left of the export file. A finished one that has not expired
    /// is opened again; one that was being made is reported stopped (its snapshot was one moment taken under a pause
    /// of the writes that died with the process, and made again it would pause them without the owner's password);
    /// one that expired meanwhile is removed. What no state file names is removed.
    fn recover_export(self: &Arc<Self>, app: &AppState) {
        let _ = std::fs::remove_file(self.data.join(moving::export::SNAPSHOTS).join("slot.tmp"));
        let now = app.now();
        match self.load_slot(&app.vault) {
            Ok(None) => {}
            Err(err) => tracing::warn!("the export file's state could not be read, it is dropped: {err}"),
            Ok(Some(f)) => {
                let slot = self.take_up(app, f, now);
                if let Some(e) = &slot {
                    self.save_best_effort(&app.vault, e);
                }
                self.lock().export = slot;
            }
        }
        // the one kept holds its lock: only the rest go
        moving::export::remove_unused_snapshots(&self.data);
    }

    fn take_up(self: &Arc<Self>, app: &AppState, f: SlotFile, now: u64) -> Option<ExportSlot> {
        let mut e = ExportSlot {
            id: f.id,
            made_ms: f.made_ms,
            snapshot: None,
            error: f.error,
            problem: f.problem_code.as_deref().map(problem),
            watch: Arc::new(moving::Watch::new(app.clock.clone())),
            sha256: f.sha256,
            checksum_error: f.checksum_error,
            sent_bytes: f.sent_bytes,
            last_download_ms: f.last_download_ms,
        };
        match f.state.as_str() {
            "expired" => e.error = Some(EXPIRED.into()),
            "failed" => {
                e.error.get_or_insert_with(|| "The export file failed.".into());
                e.problem.get_or_insert(problem("server_error"));
            }
            "preparing" => {
                e.error = Some("The server restarted while the export file was being made.".into());
                e.problem = Some(problem("server_restarted"));
            }
            "ready" => {
                if now >= e.expires_ms() {
                    e.error = Some(EXPIRED.into());
                } else {
                    let opened = match f.snapshot.as_deref() {
                        Some(hid) => reopen_waiting(&self.data, &app.vault, hid),
                        None => Err("no snapshot is named".to_string()),
                    };
                    match opened {
                        Ok(snap) => {
                            snap.keep_on_drop(true);
                            let snap = Arc::new(snap);
                            e.error = None;
                            e.snapshot = Some(snap.clone());
                            if e.sha256.is_none() && e.checksum_error.is_none() {
                                self.spawn_checksum(app, snap, e.watch.clone(), e.id.clone());
                            }
                        }
                        Err(err) => {
                            tracing::warn!("the export file could not be opened again after the restart: {err}");
                            e.error = Some(format!("The export file could not be opened again: {err}"));
                            e.problem = Some(problem("disk"));
                        }
                    }
                }
            }
            other => {
                tracing::warn!("the export file's state `{other}` is not known, it is dropped");
                return None;
            }
        }
        Some(e)
    }

    /// Works out the SHA-256 on a thread of its own (the export file is ready to download meanwhile).
    fn spawn_checksum(
        self: &Arc<Self>,
        app: &AppState,
        snap: Arc<moving::Snapshot>,
        watch: Arc<moving::Watch>,
        id: String,
    ) {
        let (s, vault) = (self.clone(), app.vault.clone());
        std::thread::spawn(move || s.finish_checksum(&vault, snap, &watch, &id));
    }

    /// Reads the export file once for its SHA-256 and keeps it with the export `id`, if that is still the one there is.
    fn finish_checksum(&self, vault: &vault::Vault, snap: Arc<moving::Snapshot>, watch: &moving::Watch, id: &str) {
        let sha = snap.sha256(vault, watch);
        drop(snap);
        let mut st = self.lock();
        if let Some(e) = st.export.as_mut().filter(|e| e.id == id) {
            match sha {
                Ok(sha) => e.sha256 = Some(sha),
                Err(moving::Error::Cancelled) => return,
                Err(err) => e.checksum_error = Some(err.to_string()),
            }
            self.save_best_effort(vault, e);
        }
    }
}

async fn exports(State(app): State<AppState>, Admin(_): Admin, q: crate::Params) -> ApiResult<Response> {
    q.wants_csv()?;
    let s = site(&app)?;
    let now = app.now();
    let mut st = s.lock();
    tidy_export(&s, &app.vault, &mut st, now);
    let items: Vec<Value> = st.export.iter().map(|e| export_view(e, now)).collect();
    drop(st);
    crate::csv::answer(&q, "exports", json!({ "items": items, "next": null }))
}

async fn make_export(State(app): State<AppState>, Admin(v): Admin, body: JsonBody) -> ApiResult<Response> {
    body.only(&["password", "two_step"])?;
    confirm(&app, &v, &body).await?;
    let s = site(&app)?;
    let now = app.now();
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).map_err(|e| ApiError::server(format!("an id: {e}")))?;
    let id = format!("ex{}", hex::encode(b));
    let watch = Arc::new(moving::Watch::new(app.clock.clone()));
    {
        let mut st = s.lock();
        if st.export.as_ref().is_some_and(|e| e.snapshot.is_none() && e.error.is_none()) {
            return Err(ApiError::conflict("export_preparing", "An export file is being made; wait for it."));
        }
        let new = ExportSlot {
            id: id.clone(),
            made_ms: now,
            snapshot: None,
            error: None,
            problem: None,
            watch: watch.clone(),
            sha256: None,
            checksum_error: None,
            sent_bytes: 0,
            last_download_ms: None,
        };
        // written before anything runs: a server that stops now reports it stopped, never forgets it. A disk that
        // can't be written fails the job itself with its own problem, which the page shows
        s.save_best_effort(&app.vault, &new);
        // the one there was goes (its snapshot is removed when the last download of it ends; its checksum stops)
        if let Some(old) = &st.export {
            s.retire(old);
        }
        st.export = Some(new);
    }
    let (s2, app2, id2) = (s.clone(), app.clone(), id.clone());
    tokio::task::spawn_blocking(move || {
        s2.paused("export-start");
        let gate = gate::Gate::open(&s2.data);
        let made = gate.map_err(moving::Error::from).and_then(|g| {
            moving::Snapshot::make_watched(&s2.data, &app2.vault, &g, &s2.app_version, EXPORT_WAIT, &watch)
        });
        let snap = {
            let mut st = s2.lock();
            let Some(e) = st.export.as_mut().filter(|e| e.id == id2) else { return };
            match made {
                Ok(snap) => {
                    let snap = Arc::new(snap);
                    e.snapshot = Some(snap.clone());
                    // kept across a restart only once its state says so
                    match s2.save_slot(&app2.vault, e) {
                        Ok(()) => snap.keep_on_drop(true),
                        Err(err) => tracing::warn!("the export file's state could not be saved: {err}"),
                    }
                    snap
                }
                Err(err) => {
                    tracing::warn!("an export file could not be made: {err}");
                    e.problem = Some(export_problem(&err));
                    e.error = Some(err.to_string());
                    s2.save_best_effort(&app2.vault, e);
                    return;
                }
            }
        };
        // ready to download; its SHA-256 comes after, read once from the snapshot (nothing kept in memory)
        s2.finish_checksum(&app2.vault, snap, &watch, &id2);
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
    tidy_export(&s, &app.vault, &mut st, now);
    Ok(Json(export_view(the_export(&mut st, &id)?, now)))
}

/// Removes the export file; one being made is cancelled (it stops at its next file or MiB, and the writes it paused
/// go on).
async fn remove_export(State(app): State<AppState>, Admin(v): Admin, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let s = site(&app)?;
    let cancelled = {
        let mut st = s.lock();
        let e = the_export(&mut st, &id)?;
        let preparing = e.snapshot.is_none() && e.error.is_none();
        s.retire(e);
        st.export = None;
        s.remove_slot_file();
        preparing
    };
    let d = s.data.clone();
    tokio::task::spawn_blocking(move || moving::export::remove_unused_snapshots(&d));
    let words = if cancelled { "Cancelled the export file being made." } else { "Removed the export file." };
    v.audit(&app, "export-removed", Some(&target("export", None, &id)), words).await?;
    Ok(Json(json!({ "deleted": true, "id": id, "cancelled": cancelled })))
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
    method: axum::http::Method,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let s = site(&app)?;
    let now = app.now();
    let snap = {
        let mut st = s.lock();
        tidy_export(&s, &app.vault, &mut st, now);
        let e = st.export.as_mut().filter(|e| e.id == id).ok_or_else(|| ApiError::not_found("export_gone"))?;
        match (&e.snapshot, &e.error) {
            (Some(snap), None) => {
                e.last_download_ms = Some(now);
                snap.clone()
            }
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
        // the download's start is saved (it keeps the file a day more), and its end, before the body ends
        s2.persist(&vault, &id2);
        let send = || {
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
        };
        send();
        s2.persist(&vault, &id2);
        drop(tx);
    });
    // every download is recorded, a resumed one too (from any byte); a HEAD request sends nothing
    if method != axum::http::Method::HEAD {
        let said = if from == 0 {
            "Downloaded the export file.".to_string()
        } else {
            format!("Downloaded the export file from byte {from} on (a download resumed).")
        };
        v.audit(&app, "export-downloaded", Some(&target("export", None, &id)), &said).await?;
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

/// The set-up code as typed: case, spaces, dashes and the `SETUP-` in front don't matter (the accounts crate's rule).
fn normal_code(text: &str) -> String {
    accounts::normal_setup_code(text)
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
pub(crate) fn claimed(app: &AppState, s: &MoveSite, headers: &HeaderMap) -> ApiResult<Zeroizing<String>> {
    if !app.accounts.needs_setup() {
        return Err(ApiError::conflict("setup_done", "This server is set up: sign in."));
    }
    holds_claim(app, s, headers)
        .ok_or_else(|| ApiError::new(401, "not_claimed", "Type the set-up code from the server's log first."))
}

/// The claim this browser holds (renewed), set up or not.
fn holds_claim(app: &AppState, s: &MoveSite, headers: &HeaderMap) -> Option<Zeroizing<String>> {
    let token = cookie_of(headers, CLAIM_COOKIE)?;
    let now = app.now();
    let mut held = s.claim.lock();
    match held.as_mut() {
        Some((h, last, code)) if same(h, &sha(&token)) && now < *last + CLAIM_MS => {
            *last = now;
            Some(code.clone())
        }
        _ => None,
    }
}

fn claim_cookie(token: &str) -> String {
    format!("{CLAIM_COOKIE}={token}; Max-Age={}; Path=/; Secure; HttpOnly; SameSite=Strict", CLAIM_MS / 1000)
}

/// Whether this browser holds the claim, without renewing it (`GET /setup` only looks).
pub(crate) fn holds_claim_now(app: &AppState, s: &MoveSite, headers: &HeaderMap) -> bool {
    let Some(token) = cookie_of(headers, CLAIM_COOKIE) else { return false };
    let now = app.now();
    matches!(s.claim.lock().as_ref(), Some((h, last, _)) if same(h, &sha(&token)) && now < *last + CLAIM_MS)
}

/// The `/setup` answer of a browser (`holds`: it holds the claim).
pub(crate) fn setup_answer(app: &AppState, s: &MoveSite, holds: bool) -> Value {
    let needs_setup = app.accounts.needs_setup();
    let moving_here = matches!(moving::pull::pending(&s.data, &s.keys), moving::pull::Pending::Moving { .. });
    // a restore staged on disk, or one the web page runs (uploading, checking, switching)
    let restoring = !matches!(moving::import::pending(&s.data, &s.keys), moving::Pending::None)
        || crate::restore::job(app, s)
            .ok()
            .flatten()
            .is_some_and(|j| matches!(j["state"].as_str(), Some("uploading" | "checking" | "switching")));
    let state = if !needs_setup {
        "set_up"
    } else if moving_here {
        "moving_here"
    } else if restoring {
        "restoring"
    } else {
        "fresh"
    };
    let claim_expires = s.claim.lock().as_ref().map(|(_, last, _)| last + CLAIM_MS);
    let job = if holds { job_view(app, s).ok().flatten() } else { None };
    json!({ "needs_setup": needs_setup, "state": state, "claimed": holds,
            "claim_expires_ms": if holds { claim_expires } else { None }, "job": job })
}

/// The `/setup` answer of a server with no [`MoveSite`] (no move or restore can run): only fresh or set up.
pub(crate) fn plain_setup_answer(app: &AppState) -> Value {
    let needs_setup = app.accounts.needs_setup();
    json!({ "needs_setup": needs_setup, "state": if needs_setup { "fresh" } else { "set_up" }, "claimed": false,
            "claim_expires_ms": null, "job": null })
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
        *s.claim.lock() = Some((sha(&token), now, Zeroizing::new(on_file)));
        let mut res = Json(setup_answer(app, &s, true)).into_response();
        if let Ok(v) = HeaderValue::from_str(&claim_cookie(&token)) {
            res.headers_mut().append(header::SET_COOKIE, v);
        }
        Ok(res)
    })
    .await
}

/// `POST /setup/fresh` `{ name, password, time_zone? }`: the first admin, signed in, and the recovery key shown this
/// once. `time_zone` (the browser's IANA zone) becomes the zone new cars start in, unless one is saved already; a
/// name that is no zone is left out, never a reason to refuse the first admin.
async fn fresh(
    State(app): State<AppState>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Response> {
    body.only(&["name", "password", "time_zone"])?;
    let (name, password) = (body.req_str("name")?.to_string(), body.req_str("password")?.to_string());
    let zone = body.str("time_zone")?.map(str::to_string);
    let s = site(&app)?;
    let recovery_file = s.recovery_file();
    app.blocking(move |app| {
        let _gate = s.setup_gate();
        let code = claimed(app, &s, &headers)?;
        refuse_if_busy(&s)?;
        let client = accounts::Client::new(conn.ip, &crate::policy::device(&headers));
        let local = client.local;
        let ns = match app.accounts.setup(&code, &name, &password, &client) {
            Ok(ns) => ns,
            // the code the claim was made with is gone (a restore's switch set it aside): claim again
            Err(accounts::Error::WrongSetupCode | accounts::Error::SetupCodeExpired) => {
                *s.claim.lock() = None;
                return Err(ApiError::new(401, "not_claimed", "Type the set-up code from the server's log again."));
            }
            Err(e) => return Err(e.into()),
        };
        *s.claim.lock() = None;
        crate::resources::imports::first_admin_made(app);
        if let Some(z) = zone.as_deref()
            && let Err(e) = crate::resources::settings::first_time_zone(app, z)
        {
            tracing::warn!("set-up: the browser's time zone was not saved as the default for new cars: {e}");
        }
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

/// `409 setup_busy` while a move or a restore into this server is under way (the first admin waits for it).
pub(crate) fn refuse_if_busy(s: &MoveSite) -> ApiResult<()> {
    if !matches!(moving::pull::pending(&s.data, &s.keys), moving::pull::Pending::None) {
        return Err(ApiError::conflict("setup_busy", "A move here runs: give it up first."));
    }
    if !matches!(moving::import::pending(&s.data, &s.keys), moving::Pending::None) {
        return Err(ApiError::conflict("setup_busy", "A restore runs: give it up first."));
    }
    Ok(())
}

/// The move or restore under way here as a SetupJob (`None`: there is none).
pub(crate) fn job_view(app: &AppState, s: &MoveSite) -> ApiResult<Option<Value>> {
    if let Some(job) = move_view(app, s)? {
        return Ok(Some(job));
    }
    // a restore under way before a move that is done (a move from a server with no account leaves set-up open)
    let restore = crate::restore::job(app, s)?;
    if restore.as_ref().is_some_and(|j| j["state"] != "done") {
        return Ok(restore);
    }
    if let Some(job) = moved_in_view(app) {
        return Ok(Some(job));
    }
    Ok(restore)
}

/// The cars the data has (`None` without an engine).
fn car_count(app: &AppState) -> Option<usize> {
    app.service::<engine::Engine>().ok().and_then(|e| engine::query::all_cars(&e).ok()).map(|c| c.len())
}

/// The move under way here as a SetupJob (`None`: there is none).
fn move_view(app: &AppState, s: &MoveSite) -> ApiResult<Option<Value>> {
    let Some(st) = moving::pull::status(&s.data, &s.keys).map_err(moving_error)? else { return Ok(None) };
    use moving::pull::Phase;
    let state = match (st.phase, &st.problem) {
        (Phase::Switching, _) => "switching",
        (Phase::Copying, _) if st.forced => "switching",
        (_, Some(p)) if p.retry => "waiting",
        (_, Some(_)) => "stopped",
        (Phase::Claiming, None) => "connecting",
        (Phase::Copying, None) if st.finishing => "finishing",
        (Phase::Copying, None) if st.progress.round <= 1 => "copying",
        (Phase::Copying, None) => "catching_up",
    };
    let p = &st.progress;
    let problem = st.problem.as_ref().map(
        |p| json!({ "code": p.code, "message": p.message, "retry_ms": if p.retry { Some(60_000) } else { None } }),
    );
    let skipped: Vec<Value> = st.skipped.iter().map(|k| json!({ "path": k.path, "why": k.why })).collect();
    let per_s = if matches!(state, "copying" | "catching_up") {
        s.claim.speed(&st.job_id, app.now(), p.bytes_done).filter(|r| *r > 0)
    } else {
        None
    };
    let eta = per_s.map(|r| p.bytes_total.saturating_sub(p.bytes_done) / r);
    let total = (st.phase != Phase::Claiming).then_some(p.files_total);
    Ok(Some(json!({
        "id": st.job_id,
        "kind": "move_here",
        "state": state,
        "started_ms": st.started_ms,
        "from": st.from,
        "file_name": null,
        "size": null,
        "received": null,
        "progress": { "round": p.round, "files_done": p.files_done, "files_total": total,
                      "bytes_done": p.bytes_done, "bytes_total": total.map(|_| p.bytes_total), "cars_done": null,
                      "cars_total": null, "bytes_per_s": per_s, "eta_s": eta },
        "problem": problem,
        "result": null,
        "skipped": skipped,
        "log": log_view(&st.log),
    })))
}

/// The move that brought this server's data, as a SetupJob that is `done` (`None`: the data didn't come by a move).
fn moved_in_view(app: &AppState) -> Option<Value> {
    let s = site(app).ok()?;
    let n = moving::pull::moved_in(&s.data, &app.vault)?;
    let result = json!({ "at_ms": n.at_ms, "from": n.from, "file_name": null, "forced": n.forced,
                         "same_machine": n.same_machine, "accounts": app.accounts.count(), "cars": car_count(app),
                         "cert_sha256": crate::restore::cert_sha256(&s.data) });
    Some(json!({
        "id": n.job_id,
        "kind": "move_here",
        "state": "done",
        "started_ms": (n.started_ms > 0).then_some(n.started_ms),
        "from": n.from,
        "file_name": null,
        "size": null,
        "received": null,
        "progress": null,
        "problem": null,
        "result": result,
        "skipped": [],
        "log": log_view(&n.log),
    }))
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
async fn move_here(
    State(app): State<AppState>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Response> {
    body.only(&["from", "code"])?;
    let (from, code) = (body.req_str("from")?.trim().to_string(), Zeroizing::new(body.req_str("code")?.to_string()));
    if from.starts_with("http://") {
        return Err(ApiError::bad_input("from", "Use the old server's address (https), not http://."));
    }
    let addr = match moving::client::Address::parse(&from) {
        Ok(a) => a.to_string(),
        Err(e) => return Err(ApiError::bad_input("from", &e)),
    };
    if let Err(e) = moving::code::MoveCode::parse(&code) {
        return Err(ApiError::bad_input("code", &e.to_string()));
    }
    let s = site(&app)?;
    app.blocking(move |app| {
        // held until the move's files are on disk: no first admin is made in between
        let gate = s.setup_gate();
        claimed(app, &s, &headers)?;
        let had = moving::pull::status(&s.data, &s.keys).map_err(moving_error)?;
        if let Some(h) = &had {
            // a move that stopped carries on with a new code from the same old server only
            if h.from != addr {
                return Err(ApiError::conflict(
                    "setup_busy",
                    format!(
                        "The move from {} stopped here and carries on with a new code from that server. To move from \
                         another server, give that move up first.",
                        h.from
                    ),
                ));
            }
        } else {
            refuse_if_busy(&s)?;
        }
        let mut opts = moving::pull::Options::new(&s.app_version);
        // the web side's own first-start files (no account yet) don't count as the owner's data
        opts.first_start_also = moving::import::WEB_FIRST_START.iter().map(|n| n.to_string()).collect();
        moving::pull::begin(&s.data, &s.keys, &from, &code, &opts).map_err(begin_problem)?;
        drop(gate);
        // the running server carries the move on from here (`pull::run`, with this web side up)
        let job = job_view(app, &s)?.unwrap_or(Value::Null);
        let (status, what, summary) = if had.is_some() {
            (
                StatusCode::OK,
                "setup-move-resumed",
                format!("Carried on moving the data here from {addr} with a new move code."),
            )
        } else {
            (StatusCode::CREATED, "setup-move-started", format!("Started moving the data here from {addr}."))
        };
        let details = crate::ctx::audit_details(Some(&job_target(&job)), &summary);
        app.accounts.audit().record(None, Some(conn.ip), what, &details)?;
        Ok((status, Json(job)).into_response())
    })
    .await
}

/// `POST /setup/move-here/finish` `{ recovery_key, confirm: true }`.
async fn move_finish(
    State(app): State<AppState>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["recovery_key", "confirm"])?;
    if body.bool("confirm")? != Some(true) {
        return Err(ApiError::bad_input("confirm", "Required: true (the last changes may be missing)."));
    }
    let key = Zeroizing::new(body.req_str("recovery_key")?.trim().to_string());
    let s = site(&app)?;
    app.blocking(move |app| {
        claimed(app, &s, &headers)?;
        let Some(st) = moving::pull::status(&s.data, &s.keys).map_err(moving_error)? else {
            return Err(ApiError::new(404, "no_such_job", "No move into this server is under way."));
        };
        // only a copy that stopped part way (`waiting` or `stopped`), with something copied
        let stopped = st.problem.is_some() && !st.forced && st.phase != moving::pull::Phase::Switching;
        let copied = st.cert_sha256.is_some() && (st.progress.bytes_done > 0 || st.progress.files_done > 0);
        if !stopped || !copied {
            return Err(ApiError::conflict(
                "not_ready",
                "Finish with the recovery key only when the old server is gone for good and the copy has stopped \
                 part way with something copied.",
            ));
        }
        if !vault::RecoveryKey::reads(&key) {
            return Err(ApiError::bad_input(
                "recovery_key",
                "This doesn't read as a recovery key: SMRK- and 13 groups of 4. Check for a missing group.",
            ));
        }
        // one try at a time (each takes up to 1 GiB), counted before it runs, like a restore's
        let Some(work) = s.key_work() else {
            return Err(ApiError::new(503, "busy", "A recovery key is being tried; try again in a few seconds.")
                .header(header::RETRY_AFTER, "5"));
        };
        let tried = app.now();
        s.key_try(conn.ip, tried)?;
        let r = moving::pull::force_finish(&s.data, &s.keys, &key);
        if !matches!(r, Err(moving::Error::WrongRecoveryKey)) {
            s.right_key(conn.ip, tried);
        }
        drop(work);
        r.map_err(|e| match e {
            moving::Error::WrongRecoveryKey => ApiError::new(
                422,
                "wrong_recovery_key",
                "That recovery key doesn't open the old server's data. Use the one the old server showed when it was set up.",
            )
            .with_fields(serde_json::Map::from_iter([("recovery_key".into(), json!("Doesn't open this data."))])),
            e => moving_error(e),
        })?;
        let job = job_view(app, &s)?.unwrap_or(Value::Null);
        let details = crate::ctx::audit_details(
            Some(&job_target(&job)),
            "Finished the move with the recovery key (old server gone).",
        );
        app.accounts.audit().record(None, Some(conn.ip), "setup-move-finished-forced", &details)?;
        Ok(Json(job))
    })
    .await
}

/// A move or restore under way here, as its audit entries name it (`move:<its id>`).
pub(crate) fn job_target(job: &Value) -> String {
    target("move", None, job["id"].as_str().unwrap_or("here"))
}

async fn setup_job(State(app): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let s = site(&app)?;
    app.blocking(move |app| {
        // once set up, still for the browser that holds the claim: the page shows the restore's `done` and `result`
        if app.accounts.needs_setup() || holds_claim(app, &s, &headers).is_none() {
            claimed(app, &s, &headers)?;
        }
        job_view(app, &s)?.map(Json).ok_or_else(|| ApiError::not_found("no_such_job"))
    })
    .await
}

async fn give_up_job(
    State(app): State<AppState>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let s = site(&app)?;
    app.blocking(move |app| {
        claimed(app, &s, &headers)?;
        let job = job_view(app, &s)?.ok_or_else(|| ApiError::not_found("no_such_job"))?;
        if job["kind"] == "restore" {
            crate::restore::give_up(app, &s, &job, conn.ip)?;
        } else {
            if job["state"] == "done" {
                return Err(ApiError::conflict("finished", "The move is done; this server holds the data."));
            }
            if job["state"] == "switching" {
                return Err(ApiError::conflict(
                    "finished",
                    "It is switching to the copied data; it finishes by itself.",
                ));
            }
            moving::pull::cancel(&s.data, &s.keys).map_err(moving_error)?;
            let from = job["from"].as_str().unwrap_or("the old server");
            let details = crate::ctx::audit_details(
                Some(&job_target(&job)),
                &format!("Gave up moving the data here from {from}."),
            );
            app.accounts.audit().record(None, Some(conn.ip), "setup-move-cancelled", &details)?;
        }
        Ok(Json(json!({ "deleted": true, "id": job["id"] })))
    })
    .await
}

/// The export file as `/system/exports/{export}` gives it now, if there is one (the live stream's `job` of kind
/// `export`).
pub(crate) fn export_now(app: &AppState) -> Option<Value> {
    let s = site(app).ok()?;
    let now = app.now();
    let mut st = s.lock();
    tidy_export(&s, &app.vault, &mut st, now);
    st.export.as_ref().map(|e| export_view(e, now))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_key_try_is_counted_before_it_runs() {
        let s = MoveSite::new(PathBuf::from("/nowhere/data"), PathBuf::from("/nowhere/keys"), "1.0.0");
        let ip: std::net::IpAddr = [192, 0, 2, 7].into();
        // tries sent at the same moment, none of them answered yet: the next one is throttled already
        for _ in 0..MAX_WRONG_CODES {
            s.key_try(ip, 1_000).unwrap();
        }
        assert_eq!(s.key_try(ip, 1_000).unwrap_err().code, "throttled");
        // a key that opened is taken back
        s.right_key(ip, 1_000);
        s.key_try(ip, 1_000).unwrap();
        assert!(s.key_try(ip, 61_000).is_ok(), "a minute later");
        // one Argon2id at a time
        let held = s.key_work().unwrap();
        assert!(s.key_work().is_none());
        drop(held);
        assert!(s.key_work().is_some());
    }
}
