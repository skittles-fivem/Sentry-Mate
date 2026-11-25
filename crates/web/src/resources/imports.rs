//! Imports (docs/ui-api.md, "Import"): `/imports[/{import}[/cars|/start]]`, admin only (the guard's list). Three
//! kinds, told apart by the file itself once it is in: a TeslaMate backup (`kind: "teslamate"`, SQL, plain, gzip or
//! xz); TeslaFi's data download (`kind: "teslafi"`, its monthly CSVs: one, several in a `.zip`, gzip or xz; known by
//! the CSV's header, [`import::teslafi::sniff`]); and Sentry Six Manager's CAN export (`kind: "sentry_manager_can"`,
//! the `.zip` of its `GET /api/can/export/bundle`): the car's own signed CAN Bridge logs, each checked, stored and
//! judged as an upload is ([`engine::query::can_import`]); its cars follow from the logs' car keys, so it has no cars
//! to match.
//!
//! 1. **Upload**: `POST /imports` `{ name, bytes_total, compression? }` (or an `Upload-Length` header) answers 201
//!    with the job, `upload_url` and `chunk_max_bytes`; each `PATCH /imports/{import}` is the next chunk
//!    (`application/offset+octet-stream`, `Upload-Offset`, `Upload-Length`, `Upload-SHA256` in hex or base64). The
//!    file is sealed with the vault as it arrives (`web/imports/<id>/backup`, sealed records), never stored in the
//!    clear. A wrong offset is `409 wrong_offset` with the offset to send from; a chunk that is not its SHA-256 is
//!    `422 checksum_mismatch`. One import at a time (`409 import_running`).
//! 2. **Check** (pass 1, `checking`): once the last byte is in, the whole file is read as the import would read it,
//!    storing nothing ([`engine::query::imports::check`]): its cars, what each brings, the report, and the car here
//!    with the same VIN prefix (suggested, and matched at first). Then `ready`, or `failed` with `error`.
//! 3. **Cars**: `GET /imports/{import}/cars`, `PUT` `{ items: [{ tm_car, match: car | "new" | "leave_out" }] }`.
//! 4. **Start** (pass 2, `importing`): `POST /imports/{import}/start` `{ currency? }` (what TeslaMate's charge costs
//!    are in; TeslaMate keeps none, so costs are taken only with it). A TeslaFi import also takes `time_zone` (TeslaFi
//!    writes local time without a zone: the check read it in the matched car's, which the report names),
//!    `distance_unit` (`mi`, `km`) and `temperature_unit` (`c`, `f`) for its base columns when what the check
//!    worked out from the file is wrong. Then `done` with the report, or `failed`.
//!
//! A job is kept sealed (`web/imports/<id>/job`); a restart picks up a check again, and an import cut off by one
//! goes back to `ready` with the reason in `error` (running it again replaces what it stored, never doubles it).
//! `DELETE` discards a job at any step (a running pass stops at its next read); history already imported stays.
//!
//! The uploaded file is kept until the import is discarded. Once the import is over (`done` or `failed`),
//! compaction packs it ([`Imports::kept_step`], the `kept` crate): `backup` becomes `backup.pack`, a few percent of
//! a plain dump's size, and gives back the same bytes. Whatever reads an upload reads either form.

use crate::ctx::{read_body, target};
use crate::{Admin, ApiError, ApiResult, AppState, JsonBody, Params};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use engine::Engine;
use engine::query::can_import as can;
use engine::query::imports::{self as im, BackupCar, TeslafiSettings};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The largest chunk a PATCH may carry (just under the 1 MiB body limit).
pub const CHUNK_MAX: usize = (1 << 20) - 1024;
/// The largest backup taken.
pub const MAX_FILE: u64 = 64 << 30;
const COMPRESSIONS: [&str; 4] = ["plain", "gzip", "xz", "zip"];
/// The kinds of import, known once the file is checked.
pub const KIND_TESLAMATE: &str = "teslamate";
pub const KIND_CAN: &str = can::KIND;
pub const KIND_TESLAFI: &str = import::teslafi::KIND;
const UNFINISHED: [&str; 4] = ["uploading", "checking", "ready", "importing"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/imports", get(list).post(create))
        .route("/api/v1/imports/{import}", get(one).patch(chunk).delete(discard))
        .route("/api/v1/imports/{import}/files/{index}", axum::routing::patch(chunk_file))
        .route("/api/v1/imports/{import}/cars", get(cars).put(match_cars))
        .route("/api/v1/imports/{import}/start", post(start))
        .route("/api/v1/import-holds", get(holds))
        .route("/api/v1/import-holds/{hold}", post(assign_hold))
}

/// The imports of this server (a service the program hands the frame; one per server).
#[derive(Default)]
pub struct Imports {
    state: Mutex<Jobs>,
    /// One chunk of an upload at a time, per import: held from the offset check to the job's save, so a chunk sent
    /// again while the first try is still being stored gets `409 wrong_offset`, never a half-written job. It holds
    /// the upload's file, opened to add to it once and kept open between chunks: opening it walks every record
    /// already there, so opening it per chunk made each chunk cost more than the one before (an upload that started
    /// fast got slower and slower as the file grew).
    chunks: Mutex<HashMap<String, Arc<Mutex<Option<vault::Records>>>>>,
    /// Times an upload's file was opened to add to it (tests: once per upload, not once per chunk).
    upload_opens: AtomicU64,
    /// The data folder's write gate, which a CAN export's logs are stored through (as uploads are).
    gate: std::sync::OnceLock<gate::Gate>,
    /// One per import whose kept files compaction packs or a discard removes: whoever holds its lock works in the
    /// import's folder, and its flag asks a pack under way to stop at its next block (see [`KeptStep`]).
    packing: Mutex<HashMap<String, Arc<Packing>>>,
    /// Uploads found packed already (gzip, xz, zip): left as they are, not looked at again until a restart.
    as_uploaded: Mutex<std::collections::HashSet<String>>,
    /// For tests of what a crash leaves: where the next pack stops.
    kept_stop: Mutex<Option<kept::Stop>>,
}

#[derive(Default)]
struct Packing {
    lock: Mutex<()>,
    discarded: AtomicBool,
}

#[derive(Default)]
struct Jobs {
    loaded: bool,
    jobs: BTreeMap<String, Job>,
    live: HashMap<String, Live>,
}

/// A pass running now: the bytes it has read (a TeslaMate pass reads the file twice, a CAN export's once) and its
/// stop.
#[derive(Clone)]
struct Live {
    read: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    reads: u64,
}

impl Live {
    fn new(reads: u64) -> Live {
        Live { read: Arc::new(AtomicU64::new(0)), stop: Arc::new(AtomicBool::new(false)), reads: reads.max(1) }
    }
}

impl Imports {
    pub fn new() -> Imports {
        Imports::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Jobs> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The lock of one import's chunks, with its file once opened (a discarded import's goes with it).
    fn chunk_lock(&self, id: &str) -> Arc<Mutex<Option<vault::Records>>> {
        let mut m = self.chunks.lock().unwrap_or_else(|e| e.into_inner());
        m.entry(id.to_string()).or_default().clone()
    }

    fn forget_chunks(&self, id: &str) {
        let part = format!("{id}/");
        self.chunks.lock().unwrap_or_else(|e| e.into_inner()).retain(|k, _| k != id && !k.starts_with(&part));
    }

    /// Times an upload's file has been opened to add to it since the server started (once per upload, again only
    /// after a restart or a failed write).
    pub fn upload_opens(&self) -> u64 {
        self.upload_opens.load(Ordering::Relaxed)
    }

    fn packing_of(&self, id: &str) -> Arc<Packing> {
        self.packing.lock().unwrap_or_else(|e| e.into_inner()).entry(id.to_string()).or_default().clone()
    }

    /// The step of compaction that packs the kept uploads of finished imports (the engine runs it in every pass:
    /// [`Engine::compact_step`]). `dir` is the web side's folder, `vault` what its files are sealed with.
    pub fn kept_step(self: &Arc<Self>, dir: &std::path::Path, vault: Arc<vault::Vault>) -> Arc<KeptStep> {
        Arc::new(KeptStep { all: self.clone(), dir: dir.join("imports"), vault })
    }

    /// For tests: the next pack stops there, as if the server died.
    #[doc(hidden)]
    pub fn kept_stop_at(&self, at: Option<kept::Stop>) {
        *self.kept_stop.lock().unwrap_or_else(|e| e.into_inner()) = at;
    }

    /// For tests: forgets the jobs in memory, as a restart does (the next request reads them from the disk).
    #[doc(hidden)]
    pub fn forget_jobs(&self) {
        let mut st = self.lock();
        st.loaded = false;
        st.jobs.clear();
        st.live.clear();
    }

    /// The data folder's write gate (the server gives it once; without one, as in tests, nothing is paused for).
    pub fn set_gate(&self, g: gate::Gate) {
        let _ = self.gate.set(g);
    }

    /// The data moved to another server: every check and import under way stops at its next read and writes
    /// nothing more (its job stays as it was saved; after `move undo` a cut import goes back to `ready`, and on the
    /// new server too). How many were under way.
    pub fn stop_for_move(&self) -> usize {
        let st = self.lock();
        for l in st.live.values() {
            l.stop.store(true, Ordering::Relaxed);
        }
        st.live.len()
    }

    /// The checks and imports running now.
    pub fn running(&self) -> usize {
        self.lock().live.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JobError {
    code: String,
    message: String,
}

/// The match of a car of the file whose rows go with another car of the file (it takes that car's match).
const WITH: &str = "with";
/// The match of rows that wait for a car (held, then given one in Settings: Import).
const NEEDS_CAR: &str = "needs_car";

/// Where a backup car goes at first: with the car of the file its rows carry on from; for rows that need a car, the
/// car of their name their odometer and time fit, else held; else the car here with its whole VIN or VIN prefix,
/// else left out when it holds no drive and no charge (nothing would be imported for it), else not chosen yet.
fn first_match(car: &engine::query::imports::BackupCar) -> Option<String> {
    if car.joins.is_some() {
        return Some(WITH.into());
    }
    if car.needs_car {
        return Some(car.suggested.map_or_else(|| NEEDS_CAR.into(), crate::ids::car));
    }
    match car.suggested {
        Some(c) => Some(crate::ids::car(c)),
        None if car.drives == 0 && car.charges == 0 => Some("leave_out".into()),
        None => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CarRow {
    car: BackupCar,
    /// `c<n>`, `new`, `leave_out`, or `None` while not matched.
    matched: Option<String>,
}

/// One uploaded file of an import of several (TeslaFi's monthly CSVs, uploaded side by side).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Part {
    name: String,
    bytes_total: u64,
    uploaded_bytes: u64,
}

/// The sealed file of upload `k` (the first is `backup`, as for an import of one file).
fn backup_file(k: usize) -> String {
    if k == 0 { "backup".into() } else { format!("backup.{k}") }
}

/// The most files one import takes.
const MAX_FILES: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Job {
    id: String,
    name: String,
    compression: String,
    status: String,
    pass: u8,
    bytes_total: u64,
    uploaded_bytes: u64,
    /// `bytes_read` of the last pass that ended.
    bytes_read: u64,
    started_ms: u64,
    checked_ms: Option<u64>,
    finished_ms: Option<u64>,
    error: Option<JobError>,
    cars: Vec<CarRow>,
    report: Option<Value>,
    /// `teslamate`, `teslafi` or `sentry_manager_can`, once checked (`None` before).
    #[serde(default)]
    kind: Option<String>,
    /// A TeslaFi import's time zone and units, as the start said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    teslafi: Option<TeslafiSettings>,
    /// The files, when several were uploaded as one import (empty: one file, the job's own name and sizes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    files: Vec<Part>,
    /// Rows of this import moved from one car to another or to wait for a car, newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    moves: Vec<Value>,
    /// The lots of its rows that wait for a car (or waited), as [`lot_view`] writes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    holds: Vec<Value>,
    /// The version of the matching its rows were last checked against ([`REHOME_VERSION`]).
    #[serde(default)]
    rehomed: u32,
    /// The car each car of the file went to when it ran (by its id in the file): a car matched as `new` is only
    /// known by this. Not in the job's view; what reading the kept upload again for a car needs
    /// ([`replay`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    went: BTreeMap<i64, model::CarId>,
}

/// The version of TeslaFi's matching (whole VINs; rows without a VIN only where they plainly carry on): a done import
/// matched by an older one has its rows checked again once, from its kept files.
const REHOME_VERSION: u32 = 1;

impl Job {
    fn is_can(&self) -> bool {
        self.kind.as_deref() == Some(KIND_CAN)
    }

    fn is_teslafi(&self) -> bool {
        self.kind.as_deref() == Some(KIND_TESLAFI)
    }

    /// Its files: several, or the one of an import of one file.
    fn parts(&self) -> Vec<Part> {
        if self.files.is_empty() {
            vec![Part { name: self.name.clone(), bytes_total: self.bytes_total, uploaded_bytes: self.uploaded_bytes }]
        } else {
            self.files.clone()
        }
    }

    /// The report without the cars left out (not in its cars, not in its counts).
    fn report_view(&self) -> Option<Value> {
        let mut r = self.report.clone()?;
        let left: Vec<String> = self
            .cars
            .iter()
            .filter(|c| c.matched.as_deref() == Some("leave_out"))
            .map(|c| c.car.tm_car.to_string())
            .collect();
        im::without_left_out(&mut r, &left);
        Some(r)
    }

    fn view(&self, live: Option<&Live>) -> Value {
        let read = live.map_or(self.bytes_read, |l| (l.read.load(Ordering::Relaxed) / l.reads).min(self.bytes_total));
        let cars_to_match: Value = if self.cars.is_empty() && matches!(self.status.as_str(), "uploading" | "checking") {
            Value::Null
        } else {
            json!(self.cars.iter().filter(|c| c.matched.is_none()).count())
        };
        json!({
            "id": self.id,
            "name": self.name,
            "compression": self.compression,
            "kind": self.kind,
            "status": self.status,
            "pass": self.pass,
            "bytes_read": read,
            "bytes_total": self.bytes_total,
            "uploaded_bytes": self.uploaded_bytes,
            "started_ms": self.started_ms,
            "checked_ms": self.checked_ms,
            "finished_ms": self.finished_ms,
            "error": self.error,
            "report": self.report_view(),
            "cars_to_match": cars_to_match,
            "files": self.parts().iter().enumerate().map(|(k, p)| json!({
                "index": k, "name": p.name, "bytes_total": p.bytes_total, "uploaded_bytes": p.uploaded_bytes,
                "upload_url": format!("/api/v1/imports/{}/files/{k}", self.id),
            })).collect::<Vec<_>>(),
            "moves": self.moves,
            "needs_car": self.holds,
        })
    }

    fn unfinished(&self) -> bool {
        UNFINISHED.contains(&self.status.as_str())
    }
}

fn folder(app: &AppState, id: &str) -> PathBuf {
    app.dir.join("imports").join(id)
}

/// Labels are the files' paths under the data folder (the web side's folder is `<data>/web`).
fn label(id: &str, file: &str) -> String {
    format!("web/imports/{id}/{file}")
}

fn save(app: &AppState, j: &Job) -> ApiResult<()> {
    let dir = folder(app, &j.id);
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::server(format!("the import's folder: {e}")))?;
    let bytes = serde_json::to_vec(j).map_err(ApiError::server)?;
    app.vault
        .write_file(&dir.join("job"), &label(&j.id, "job"), &bytes)
        .map_err(|e| ApiError::server(format!("saving the import: {e}")))
}

fn imports(app: &AppState) -> ApiResult<Arc<Imports>> {
    app.service::<Imports>()
}

/// Reads the jobs from disk once (blocking): a check a restart cut off starts again, an import cut off goes back to
/// `ready` with why.
fn load(app: &AppState, all: &Imports) -> ApiResult<()> {
    let mut again = Vec::new();
    {
        let mut st = all.lock();
        if st.loaded {
            return Ok(());
        }
        st.loaded = true;
        let Ok(rd) = std::fs::read_dir(app.dir.join("imports")) else { return Ok(()) };
        for d in rd.flatten() {
            let id = d.file_name().to_string_lossy().into_owned();
            let Ok(Some(raw)) = app.vault.read_file(&d.path().join("job"), &label(&id, "job")) else { continue };
            let Ok(mut j) = serde_json::from_slice::<Job>(&raw) else { continue };
            match j.status.as_str() {
                "checking" => again.push(j.id.clone()),
                "importing" => {
                    j.status = "ready".into();
                    j.pass = 1;
                    let message = if j.is_can() {
                        "The server stopped during the import. Start it again: the logs it stored are skipped, the \
                         rest are imported."
                    } else {
                        "The server stopped during the import. Start it again: what it stored is replaced, not doubled."
                    };
                    j.error = Some(JobError { code: "interrupted".into(), message: message.into() });
                    save(app, &j)?;
                }
                _ => {}
            }
            st.jobs.insert(j.id.clone(), j);
        }
    }
    for id in again {
        spawn_check(app, &id)?;
    }
    Ok(())
}

/// A job by id (`404 no_such_import`).
fn job(all: &Imports, id: &str) -> ApiResult<Job> {
    all.lock().jobs.get(id).cloned().ok_or_else(|| ApiError::not_found("no_such_import"))
}

fn view_of(all: &Imports, j: &Job) -> Value {
    let st = all.lock();
    j.view(st.live.get(&j.id))
}

/// Runs account-free work off the async threads with the jobs loaded.
async fn with_jobs<R: Send + 'static>(
    app: &AppState,
    f: impl FnOnce(&AppState, &Imports) -> ApiResult<R> + Send + 'static,
) -> ApiResult<R> {
    let all = imports(app)?;
    app.blocking(move |app| {
        load(app, &all)?;
        f(app, &all)
    })
    .await
}

/// The id of the import that is not finished yet (uploading, checked or importing), if any: `/setup/status` says
/// so. `None` also where there are no imports to ask.
pub async fn unfinished(app: &AppState) -> Option<String> {
    with_jobs(app, |_, all| Ok(all.lock().jobs.values().find(|j| j.unfinished()).map(|j| j.id.clone())))
        .await
        .ok()
        .flatten()
}

async fn list(State(app): State<AppState>, p: Params) -> ApiResult<Response> {
    let rows = with_jobs(&app, |_, all| {
        let st = all.lock();
        let mut v: Vec<(u64, String, Value)> =
            st.jobs.values().map(|j| (j.started_ms, j.id.clone(), j.view(st.live.get(&j.id)))).collect();
        v.sort_by(|a, b| (b.0, &b.1).cmp(&(a.0, &a.1)));
        Ok(v)
    })
    .await?;
    if p.wants_csv()? {
        let items: Vec<Value> = rows.into_iter().map(|r| r.2).collect();
        return Ok(crate::csv::response("imports", crate::csv::rows(&items)));
    }
    let page = app.page(&p, rows, |r| (u64::MAX - r.0, r.1.clone()))?;
    let items: Vec<Value> = page.items.into_iter().map(|r| r.2).collect();
    Ok(Json(json!({ "items": items, "next": page.next })).into_response())
}

fn random_id() -> ApiResult<String> {
    let mut b = [0u8; 6];
    getrandom::fill(&mut b).map_err(|e| ApiError::server(format!("an id: {e}")))?;
    Ok(format!("im{}", hex::encode(b)))
}

pub(crate) fn header_u64(h: &HeaderMap, name: &str) -> ApiResult<Option<u64>> {
    match h.get(name) {
        None => Ok(None),
        Some(v) => v
            .to_str()
            .ok()
            .and_then(|s| crate::ids::number(s.trim()))
            .map(Some)
            .ok_or_else(|| ApiError::bad_input(name, "Must be a whole number of bytes.")),
    }
}

async fn create(
    State(app): State<AppState>,
    Admin(v): Admin,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Response> {
    body.only(&["name", "bytes_total", "compression", "files"])?;
    let mut parts: Vec<Part> = Vec::new();
    if let Some(f) = body.get("files") {
        let Value::Array(items) = f else { return Err(ApiError::bad_input("files", "Send [{ name, bytes_total }].")) };
        if items.is_empty() || items.len() > MAX_FILES {
            return Err(ApiError::invalid("files", "Between 1 and 500 files."));
        }
        for (i, x) in items.iter().enumerate() {
            let size = x.get("bytes_total").and_then(Value::as_u64).filter(|n| (1..=MAX_FILE).contains(n));
            let size =
                size.ok_or_else(|| ApiError::invalid(&format!("files.{i}.bytes_total"), "Between 1 byte and 64 GB."))?;
            let name: String = x.get("name").and_then(Value::as_str).unwrap_or("").trim().chars().take(200).collect();
            if engine::query::not_plain_text(&name) {
                return Err(ApiError::invalid(&format!("files.{i}.name"), "No control characters."));
            }
            let name = if name.is_empty() { format!("file-{}", i + 1) } else { name };
            parts.push(Part { name, bytes_total: size, uploaded_bytes: 0 });
        }
    }
    let total = match body.u64("bytes_total")? {
        _ if !parts.is_empty() => parts.iter().map(|p| p.bytes_total).sum(),
        Some(n) => n,
        None => header_u64(&headers, "Upload-Length")?.ok_or_else(|| {
            ApiError::bad_input("bytes_total", "Required: the size of the whole file in bytes (or Upload-Length).")
        })?,
    };
    if !(1..=MAX_FILE).contains(&total) {
        return Err(ApiError::invalid("bytes_total", "Between 1 byte and 64 GB."));
    }
    let name: String = body.str("name")?.unwrap_or("teslamate-backup.sql").trim().chars().take(200).collect();
    if engine::query::not_plain_text(&name) {
        return Err(ApiError::invalid("name", "No control characters."));
    }
    let name = if name.is_empty() { "teslamate-backup.sql".to_string() } else { name };
    let name = match parts.as_slice() {
        [] => name,
        [one] => one.name.clone(),
        [first, more @ ..] => format!("{} and {} more files", first.name, more.len()),
    };
    // one file of several is one import of one file
    if parts.len() == 1 {
        parts.clear();
    }
    let compression = match body.str("compression")? {
        Some(c) => c.to_string(),
        None if name.to_ascii_lowercase().ends_with(".zip") => "zip".into(),
        None if name.ends_with(".xz") => "xz".into(),
        None if name.ends_with(".gz") => "gzip".into(),
        None => "plain".into(),
    };
    if !COMPRESSIONS.contains(&compression.as_str()) {
        return Err(ApiError::invalid("compression", "Must be plain, gzip, xz or zip."));
    }
    let now = app.now();
    let (n2, c2) = (name.clone(), compression.clone());
    let made = with_jobs(&app, move |app, all| {
        let mut st = all.lock();
        if st.jobs.values().any(Job::unfinished) {
            return Err(ApiError::conflict(
                "import_running",
                "Another import is not finished. Wait for it or discard it first.",
            ));
        }
        let j = Job {
            id: random_id()?,
            name: n2,
            compression: c2,
            status: "uploading".into(),
            pass: 0,
            bytes_total: total,
            uploaded_bytes: 0,
            bytes_read: 0,
            started_ms: now,
            checked_ms: None,
            finished_ms: None,
            error: None,
            cars: Vec::new(),
            report: None,
            kind: None,
            teslafi: None,
            files: parts,
            moves: Vec::new(),
            holds: Vec::new(),
            rehomed: REHOME_VERSION,
            went: BTreeMap::new(),
        };
        save(app, &j)?;
        st.jobs.insert(j.id.clone(), j.clone());
        Ok(j)
    })
    .await?;
    v.audit(&app, "import-created", Some(&target("import", None, &made.id)), &format!("Started uploading {name}."))
        .await?;
    let mut out = made.view(None);
    if let Value::Object(m) = &mut out {
        m.insert("upload_url".into(), json!(format!("/api/v1/imports/{}", made.id)));
        m.insert("chunk_max_bytes".into(), json!(CHUNK_MAX));
    }
    Ok((StatusCode::CREATED, Json(out)).into_response())
}

async fn one(State(app): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    with_jobs(&app, move |_, all| {
        let j = job(all, &id)?;
        Ok(Json(view_of(all, &j)))
    })
    .await
}

/// `Upload-SHA256` in hex or base64.
fn sha_header(h: &HeaderMap) -> ApiResult<[u8; 32]> {
    match sha_header_opt(h) {
        Ok(Some(sha)) => Ok(sha),
        _ => Err(ApiError::bad_input("Upload-SHA256", "Required: the chunk's SHA-256 (hex or base64).")),
    }
}

/// `Upload-SHA256` in hex or base64, when it is sent.
pub(crate) fn sha_header_opt(h: &HeaderMap) -> ApiResult<Option<[u8; 32]>> {
    let bad = || ApiError::bad_input("Upload-SHA256", "The chunk's SHA-256, hex or base64.");
    let Some(v) = h.get("Upload-SHA256") else { return Ok(None) };
    let v = v.to_str().map_err(|_| bad())?.trim();
    let raw = if v.len() == 64 {
        hex::decode(v).map_err(|_| bad())?
    } else {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.decode(v).map_err(|_| bad())?
    };
    raw.try_into().map(Some).map_err(|_| bad())
}

/// `PATCH /imports/{import}`: the next chunk of the file (of the first file, of an import of several).
async fn chunk(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path(id): Path<String>,
    req: Request,
) -> ApiResult<Json<Value>> {
    chunk_of(app, v, id, 0, req).await
}

/// `PATCH /imports/{import}/files/{index}`: the next chunk of one file of an import of several (files go up side by
/// side; each file's chunks in turn).
async fn chunk_file(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path((id, index)): Path<(String, String)>,
    req: Request,
) -> ApiResult<Json<Value>> {
    let k: usize = index.parse().map_err(|_| ApiError::not_found("no_such_file"))?;
    chunk_of(app, v, id, k, req).await
}

async fn chunk_of(app: AppState, v: crate::ctx::Viewer, id: String, k: usize, req: Request) -> ApiResult<Json<Value>> {
    let headers = req.headers().clone();
    let ct = headers.get(http::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !ct.is_empty()
        && !ct.starts_with("application/offset+octet-stream")
        && !ct.starts_with("application/octet-stream")
    {
        return Err(ApiError::bad_input("Content-Type", "Send the chunk as application/offset+octet-stream."));
    }
    let offset = header_u64(&headers, "Upload-Offset")?
        .ok_or_else(|| ApiError::bad_input("Upload-Offset", "Required: where this chunk starts, in bytes."))?;
    let length = header_u64(&headers, "Upload-Length")?;
    let want = sha_header(&headers)?;
    let bytes = read_body(req, CHUNK_MAX).await?;
    if bytes.is_empty() {
        return Err(ApiError::bad_input("body", "Send the chunk's bytes."));
    }
    let (done, out) = with_jobs(&app, move |app, all| {
        let one = all.chunk_lock(&if k == 0 { id.clone() } else { format!("{id}/{k}") });
        let mut held = one.lock().unwrap_or_else(|e| e.into_inner());
        let j = job(all, &id)?;
        if j.status != "uploading" {
            return Err(ApiError::conflict(
                "not_uploading",
                format!("This import is {}; it takes no chunks.", j.status),
            ));
        }
        let part = j.parts().get(k).cloned().ok_or_else(|| ApiError::not_found("no_such_file"))?;
        if length.is_some_and(|l| l != part.bytes_total) {
            return Err(ApiError::invalid("Upload-Length", &format!("This upload is {} bytes.", part.bytes_total)));
        }
        let wrong = |at: u64| {
            ApiError::new(409, "wrong_offset", format!("The upload is at byte {at}; send from there."))
                .with("offset", at)
                .header(http::HeaderName::from_static("upload-offset"), &at.to_string())
        };
        if offset != part.uploaded_bytes {
            return Err(wrong(part.uploaded_bytes));
        }
        if Sha256::digest(&bytes).as_slice() != want {
            return Err(ApiError::new(
                422,
                "checksum_mismatch",
                "The chunk does not match its SHA-256. Send it again.",
            )
            .with_fields(Map::from_iter([("Upload-SHA256".to_string(), json!("Does not match the bytes sent."))])));
        }
        if offset + bytes.len() as u64 > part.bytes_total {
            return Err(ApiError::invalid("body", "More bytes than the upload's length."));
        }
        let rec = match &mut *held {
            Some(rec) => rec,
            slot => {
                let dir = folder(app, &j.id);
                std::fs::create_dir_all(&dir).map_err(|e| ApiError::server(format!("the import's folder: {e}")))?;
                all.upload_opens.fetch_add(1, Ordering::Relaxed);
                let rec = app
                    .vault
                    .records(&dir.join(backup_file(k)), &label(&j.id, &backup_file(k)))
                    .map_err(|e| ApiError::server(format!("opening the upload: {e}")))?;
                slot.insert(rec)
            }
        };
        // the job as it is now (another file's chunk may have changed it meanwhile), with this file at `len`
        let put = |len: u64| -> ApiResult<(Job, bool)> {
            let mut st = all.lock();
            let j = st.jobs.get_mut(&id).ok_or_else(|| ApiError::not_found("no_such_import"))?;
            if j.files.is_empty() {
                j.uploaded_bytes = len;
            } else if let Some(p) = j.files.get_mut(k) {
                p.uploaded_bytes = len;
                j.uploaded_bytes = j.files.iter().map(|p| p.uploaded_bytes).sum();
            }
            j.bytes_read = j.uploaded_bytes;
            let done = j.status == "uploading" && j.uploaded_bytes == j.bytes_total;
            if done {
                j.status = "checking".into();
                j.pass = 1;
                j.bytes_read = 0;
            }
            save(app, j)?;
            Ok((j.clone(), done))
        };
        if rec.plain_len() != part.uploaded_bytes {
            // what is on disk is what counts (a write the job's record missed)
            let at = rec.plain_len();
            put(at)?;
            return Err(wrong(at));
        }
        if let Err(e) = rec.append(&bytes) {
            // opened again for the next chunk, which cuts off whatever this write left
            *held = None;
            return Err(ApiError::server(format!("storing the chunk: {e}")));
        }
        let len = rec.plain_len();
        if len == part.bytes_total {
            // this file is whole; no more chunks come for it
            *held = None;
        }
        let (j, done) = put(len)?;
        if done {
            spawn_check(app, &j.id)?;
        }
        let mut out = view_of(all, &j);
        if let Value::Object(m) = &mut out {
            m.insert("offset".into(), json!(len));
        }
        Ok((done.then(|| j.id.clone()), out))
    })
    .await?;
    match done {
        Some(id) => {
            v.audit(
                &app,
                "import-uploaded",
                Some(&target("import", None, &id)),
                "Uploaded the whole file; checking it.",
            )
            .await?
        }
        // a chunk on its way: the upload's entries are `import-created` and, once whole, `import-uploaded`
        None => v.mark_audited(),
    }
    Ok(Json(out))
}

/// The uploaded file read back in order: from the sealed records it arrived in (a thread reads them; at most a few
/// records wait), or from its pack once compaction packed it. The same bytes either way.
struct Backup {
    rx: std::sync::mpsc::Receiver<io::Result<Vec<u8>>>,
    buf: Vec<u8>,
    pos: usize,
    packed: Option<Box<kept::Reader>>,
}

impl Read for Backup {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if let Some(p) = &mut self.packed {
            return p.read(out);
        }
        while self.pos >= self.buf.len() {
            match self.rx.recv() {
                Ok(Ok(b)) => {
                    self.buf = b;
                    self.pos = 0;
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

fn opener(app: &AppState, id: &str) -> impl FnMut() -> io::Result<Backup> + use<> {
    opener_of(app, id, 0)
}

/// Opens file `k` of the import.
fn opener_of(app: &AppState, id: &str, k: usize) -> impl FnMut() -> io::Result<Backup> + use<> {
    let (vault, path, label) = (app.vault.clone(), folder(app, id).join(backup_file(k)), label(id, &backup_file(k)));
    move || {
        let (tx, rx) = std::sync::mpsc::sync_channel::<io::Result<Vec<u8>>>(4);
        // the upload as it arrived wins while it is there; without it, its pack
        let mut r = match vault.records_read(&path, &label) {
            Ok(r) => r,
            Err(e) if e.is_not_found() && kept::packed_path(&path).exists() => {
                let packed = kept::Reader::open(&vault, &kept::packed_path(&path), &kept::packed_label(&label))?;
                return Ok(Backup { rx, buf: Vec::new(), pos: 0, packed: Some(Box::new(packed)) });
            }
            Err(e) => return Err(io::Error::other(e.to_string())),
        };
        std::thread::Builder::new().name("import-read".into()).spawn(move || {
            let r2 = r.read_all(|rec| tx.send(Ok(rec.to_vec())).map_err(|_| io::Error::other("the reader stopped")));
            if let Err(e) = r2 {
                let _ = tx.send(Err(e));
            }
        })?;
        Ok(Backup { rx, buf: Vec::new(), pos: 0, packed: None })
    }
}

/// Compaction's step for the kept uploads: every upload of an import that is over (`done`, `failed`) is packed
/// ([`kept::pack`]: written beside it, read back and compared, renamed into place, and only then is the upload
/// removed). Uploads still arriving and imports not yet over are not touched; an upload that is packed already
/// (gzip, xz, zip) stays as it was uploaded.
///
/// Whether an import is over is read from its job on the disk (a state an import never leaves), so the step works
/// after a restart before anyone opened the imports. A discard and this step never work in one import's folder at
/// the same time: each holds the import's lock while it does, and a discard first asks a pack under way to stop,
/// which it does after the block it is on.
pub struct KeptStep {
    all: Arc<Imports>,
    dir: PathBuf,
    vault: Arc<vault::Vault>,
}

/// The uploads in an import's folder, by their names as uploaded (`backup`, `backup.1` ...), whichever form is there.
fn kept_names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for f in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = f.file_name().to_string_lossy().into_owned();
        let base = name.strip_suffix(".pack.tmp").or_else(|| name.strip_suffix(".pack")).unwrap_or(&name);
        let numbered =
            base.strip_prefix("backup.").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if (base == "backup" || numbered) && !names.iter().any(|n| n == base) {
            names.push(base.to_string());
        }
    }
    names.sort();
    names
}

impl KeptStep {
    /// The imports that are over, by what their jobs on the disk say.
    fn over(&self) -> Vec<String> {
        let mut ids = Vec::new();
        for d in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            let id = d.file_name().to_string_lossy().into_owned();
            let Ok(Some(raw)) = self.vault.read_file(&d.path().join("job"), &label(&id, "job")) else { continue };
            let Ok(j) = serde_json::from_slice::<Job>(&raw) else { continue };
            // what the server knows now counts too (a job saved a moment ago)
            let unfinished = j.unfinished() || self.all.lock().jobs.get(&id).is_some_and(Job::unfinished);
            if !unfinished {
                ids.push(id);
            }
        }
        ids.sort();
        ids
    }

    fn left_as_uploaded(&self, label: &str) -> bool {
        self.all.as_uploaded.lock().unwrap_or_else(|e| e.into_inner()).contains(label)
    }
}

impl engine::compact::Step for KeptStep {
    fn name(&self) -> &'static str {
        "imports"
    }

    fn dir(&self) -> PathBuf {
        self.dir.clone()
    }

    fn pending(&self) -> u64 {
        let mut n = 0;
        for id in self.over() {
            let dir = self.dir.join(&id);
            let waiting = |f: &&String| dir.join(f).exists() && !self.left_as_uploaded(&label(&id, f));
            n += kept_names(&dir).iter().filter(waiting).count() as u64;
        }
        n
    }

    fn run(&self, next: &dyn Fn() -> bool, done: &dyn Fn()) -> engine::compact::StepDone {
        let mut out = engine::compact::StepDone::default();
        let stop_at = *self.all.kept_stop.lock().unwrap_or_else(|e| e.into_inner());
        for id in self.over() {
            let packing = self.all.packing_of(&id);
            let _held = packing.lock.lock().unwrap_or_else(|e| e.into_inner());
            let dir = self.dir.join(&id);
            // discarded while this waited: its folder is gone, or going
            if packing.discarded.load(Ordering::SeqCst) || !dir.join("job").exists() {
                continue;
            }
            for file in kept_names(&dir) {
                let label = label(&id, &file);
                if self.left_as_uploaded(&label) {
                    continue;
                }
                let gave_way = std::cell::Cell::new(false);
                let stop = || {
                    if packing.discarded.load(Ordering::SeqCst) {
                        return true;
                    }
                    gave_way.set(next());
                    gave_way.get()
                };
                match kept::pack(
                    &self.vault,
                    &dir.join(&file),
                    &label,
                    &kept::Options { stop_at, ..kept::Options::new(&stop) },
                ) {
                    Ok(kept::Outcome::Packed { .. }) => {
                        out.units += 1;
                        done();
                    }
                    Ok(kept::Outcome::AsUploaded) => {
                        self.all.as_uploaded.lock().unwrap_or_else(|e| e.into_inner()).insert(label);
                    }
                    Ok(kept::Outcome::Nothing) => {}
                    Ok(kept::Outcome::Stopped) if gave_way.get() => {
                        out.stopped = true;
                        return out;
                    }
                    // discarded meanwhile: the rest of its files go with it
                    Ok(kept::Outcome::Stopped) => break,
                    Err(kept::Error::TestStop) => {
                        out.stopped = true;
                        return out;
                    }
                    Err(e @ kept::Error::Damaged { .. }) => out.problems.push(e.to_string()),
                    Err(e) => out.problems.push(format!("{label}: {e}")),
                }
            }
        }
        out
    }
}

/// Puts a finished pass's outcome into the job (unless it was discarded meanwhile).
fn finish(app: &AppState, all: &Imports, id: &str, f: impl FnOnce(&mut Job)) {
    let mut st = all.lock();
    let read = st.live.remove(id).map(|l| l.read.load(Ordering::Relaxed) / l.reads.max(1));
    if let Some(j) = st.jobs.get_mut(id) {
        j.bytes_read = read.unwrap_or(j.bytes_read).min(j.bytes_total);
        f(j);
        if let Err(e) = save(app, j) {
            tracing::warn!("import {id}: {}", e.message);
        }
    }
}

/// Whether the upload is TeslaFi's export (its first CSV's header; only the start of the file is read).
fn sniff_of(app: &AppState, id: &str) -> io::Result<import::teslafi::Sniff> {
    import::teslafi::sniff(opener(app, id)()?)
}

/// The first bytes of the upload (to tell a zip from a TeslaMate backup).
fn head_of(app: &AppState, id: &str) -> io::Result<Vec<u8>> {
    let mut r = opener(app, id)()?;
    let mut head = vec![0u8; 4];
    let mut got = 0;
    while got < head.len() {
        match r.read(&mut head[got..])? {
            0 => break,
            n => got += n,
        }
    }
    head.truncate(got);
    Ok(head)
}

/// The files of an import of several, each opened when it is its turn, its reads counted into `live`.
fn several_files(
    app: &AppState,
    id: &str,
    parts: &[Part],
    live: &Live,
) -> im::TeslafiFiles<im::Counted<Backup>, impl Iterator<Item = io::Result<(String, im::Counted<Backup>)>> + use<>> {
    let (app, id, live) = (app.clone(), id.to_string(), live.clone());
    let names: Vec<String> = parts.iter().map(|p| p.name.clone()).collect();
    im::TeslafiFiles::Several(names.into_iter().enumerate().map(move |(k, name)| {
        let mut open = im::counted(opener_of(&app, &id, k), live.read.clone(), live.stop.clone());
        open().map(|r| (name, r))
    }))
}

/// Where a CAN export's pass keeps a log too big for memory while it is read (sealed; removed after).
fn spool_of(app: &AppState, id: &str) -> PathBuf {
    folder(app, id).join("spool")
}

/// Starts the check of a whole upload in the background.
fn spawn_check(app: &AppState, id: &str) -> ApiResult<()> {
    let all = imports(app)?;
    let e = app.service::<Engine>()?;
    let (compression, parts) = all.lock().jobs.get(id).map(|j| (j.compression.clone(), j.parts())).unwrap_or_default();
    let live = Live::new(2);
    all.lock().live.insert(id.to_string(), live.clone());
    let (app, id) = (app.clone(), id.to_string());
    tokio::task::spawn_blocking(move || {
        let head = head_of(&app, &id);
        let zip = head.as_ref().is_ok_and(|h| import::zip::is_zip(h));
        let teslafi = head.is_ok() && sniff_of(&app, &id).is_ok_and(|s| s != import::teslafi::Sniff::Other);
        enum Outcome {
            Tm(Result<im::Checked, import::Error>),
            Tf(Result<im::Checked, import::Error>),
            Can(Result<Value, can::CanImportError>),
            Failed(&'static str, String),
        }
        // several files: TeslaFi's, read as one; anything else is imported on its own
        let several = parts.len() > 1;
        let not_teslafi = || {
            (1..parts.len()).find(|&k| {
                !import::teslafi::sniff(match opener_of(&app, &id, k)() {
                    Ok(r) => r,
                    Err(_) => return true,
                })
                .is_ok_and(|s| s != import::teslafi::Sniff::Other)
            })
        };
        let r = match head {
            Err(err) => Outcome::Failed("read_failed", format!("reading the upload: {err}")),
            Ok(_) if several && !teslafi => Outcome::Failed(
                "one_file_only",
                format!(
                    "{} is not TeslaFi's export. Several files are taken for TeslaFi's monthly CSVs only; a TeslaMate \
                     backup or a CAN export is imported on its own: upload it alone.",
                    parts[0].name
                ),
            ),
            Ok(_) if several => match not_teslafi() {
                Some(k) => Outcome::Failed(
                    "not_a_teslafi_export",
                    format!(
                        "{} is not one of TeslaFi's CSV files (or a zip of them). Upload TeslaFi's files only, or a \
                         TeslaMate backup on its own.",
                        parts[k].name
                    ),
                ),
                None => {
                    if let Some(l) = all.lock().live.get_mut(&id) {
                        l.reads = 1;
                    }
                    Outcome::Tf(im::teslafi_check_files(
                        &e,
                        several_files(&app, &id, &parts, &live),
                        &TeslafiSettings::default(),
                    ))
                }
            },
            Ok(_) if teslafi => {
                if let Some(l) = all.lock().live.get_mut(&id) {
                    l.reads = 1;
                }
                let mut open = im::counted(opener(&app, &id), live.read.clone(), live.stop.clone());
                Outcome::Tf(
                    open()
                        .map_err(import::Error::Io)
                        .and_then(|r| im::teslafi_check(&e, r, &TeslafiSettings::default())),
                )
            }
            Ok(_) if zip => {
                if let Some(l) = all.lock().live.get_mut(&id) {
                    l.reads = 1;
                }
                let mut open = im::counted(opener(&app, &id), live.read.clone(), live.stop.clone());
                let pass = can::Pass { store: false, spool: spool_of(&app, &id), gate: None, stop: live.stop.clone() };
                Outcome::Can(can::guarded(|| {
                    open().map_err(can::CanImportError::from).and_then(|r| can::check(&e, r, &pass))
                }))
            }
            Ok(_) if compression == "zip" => Outcome::Failed(
                "unreadable",
                "This file is not a zip. For a TeslaMate backup upload the .sql (or .sql.gz, .sql.xz) itself; from \
                 TeslaFi, its CSV files (one, or several in a .zip); from Sentry Six Manager, its CAN export bundle \
                 (.zip) as it was downloaded."
                    .into(),
            ),
            Ok(_) => {
                let open = im::counted(opener(&app, &id), live.read.clone(), live.stop.clone());
                Outcome::Tm(im::check(&e, open))
            }
        };
        if live.stop.load(Ordering::Relaxed) {
            all.lock().live.remove(&id);
            return;
        }
        let now = app.now();
        finish(&app, &all, &id, |j| {
            let fail = |j: &mut Job, code: &str, message: String| {
                j.status = "failed".into();
                j.finished_ms = Some(now);
                j.error = Some(JobError { code: code.into(), message });
            };
            match r {
                Outcome::Tm(Ok(c)) => {
                    j.kind = Some(KIND_TESLAMATE.into());
                    j.status = "ready".into();
                    j.checked_ms = Some(now);
                    j.bytes_read = j.bytes_total;
                    j.cars = c
                        .cars
                        .into_iter()
                        .map(|car| {
                            let matched = first_match(&car);
                            CarRow { car, matched }
                        })
                        .collect();
                    j.report = Some(c.report);
                }
                Outcome::Tm(Err(err)) => {
                    j.kind = Some(KIND_TESLAMATE.into());
                    let (code, message) = im::error_of(&err);
                    fail(j, code, message);
                }
                Outcome::Tf(Ok(c)) => {
                    j.kind = Some(KIND_TESLAFI.into());
                    j.status = "ready".into();
                    j.checked_ms = Some(now);
                    j.bytes_read = j.bytes_total;
                    j.cars = c
                        .cars
                        .into_iter()
                        .map(|car| {
                            let matched = first_match(&car);
                            CarRow { car, matched }
                        })
                        .collect();
                    j.report = Some(c.report);
                }
                Outcome::Tf(Err(err)) => {
                    j.kind = Some(KIND_TESLAFI.into());
                    let (code, message) = im::error_of(&err);
                    fail(j, code, message);
                }
                Outcome::Can(Ok(report)) => {
                    j.kind = Some(KIND_CAN.into());
                    j.status = "ready".into();
                    j.checked_ms = Some(now);
                    j.bytes_read = j.bytes_total;
                    j.cars = Vec::new();
                    j.report = Some(report);
                }
                Outcome::Can(Err(err)) => {
                    j.kind = Some(KIND_CAN.into());
                    let (code, message) = can::error_of(&err);
                    fail(j, code, message);
                }
                Outcome::Failed(code, message) => fail(j, code, message),
            }
        });
    });
    Ok(())
}

async fn discard(State(app): State<AppState>, Admin(v): Admin, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let id2 = id.clone();
    let (name, status) = with_jobs(&app, move |app, all| {
        // compaction packing one of its files stops at its next block and lets go of the folder
        let packing = all.packing_of(&id2);
        packing.discarded.store(true, Ordering::SeqCst);
        let _folder = packing.lock.lock().unwrap_or_else(|e| e.into_inner());
        // a chunk being stored finishes first (it would put the job back)
        let one = all.chunk_lock(&id2);
        let mut held = one.lock().unwrap_or_else(|e| e.into_inner());
        let mut st = all.lock();
        let j = st.jobs.remove(&id2).ok_or_else(|| ApiError::not_found("no_such_import"))?;
        if let Some(l) = st.live.remove(&id2) {
            l.stop.store(true, Ordering::Relaxed);
        }
        drop(st);
        *held = None;
        all.forget_chunks(&j.id);
        let _ = std::fs::remove_dir_all(folder(app, &j.id));
        all.packing.lock().unwrap_or_else(|e| e.into_inner()).remove(&j.id);
        Ok((j.name, j.status))
    })
    .await?;
    let summary = format!("Discarded the import {name} ({status}). History already imported stays.");
    v.audit(&app, "import-discarded", Some(&target("import", None, &id)), &summary).await?;
    Ok(Json(json!({ "deleted": true, "id": id, "status": status })))
}

fn car_rows(j: &Job) -> Vec<Value> {
    j.cars
        .iter()
        .filter(|c| c.matched.as_deref() != Some(WITH))
        .map(|c| {
            json!({
                "tm_car": c.car.tm_car.to_string(),
                "name": c.car.name,
                "vin_prefix": c.car.vin_prefix,
                "drives": c.car.drives,
                "charges": c.car.charges,
                "first_ms": c.car.first_ms,
                "last_ms": c.car.last_ms,
                "match": c.matched,
                "suggested": c.car.suggested.map(crate::ids::car),
                "needs_car": c.car.needs_car,
                "rows": c.car.rows,
                "odometer_km": match (c.car.first_odo_km, c.car.last_odo_km) {
                    (Some(a), Some(b)) => json!([a.round(), b.round()]),
                    _ => Value::Null,
                },
            })
        })
        .collect()
}

fn checked(j: &Job) -> ApiResult<()> {
    if j.cars.is_empty() && matches!(j.status.as_str(), "uploading" | "checking") {
        return Err(ApiError::conflict(
            "not_checked",
            "The cars are known once the whole file is uploaded and checked.",
        ));
    }
    Ok(())
}

async fn cars(State(app): State<AppState>, Path(id): Path<String>, q: Params) -> ApiResult<Response> {
    q.wants_csv()?;
    with_jobs(&app, move |_, all| {
        let j = job(all, &id)?;
        checked(&j)?;
        crate::csv::answer(&q, "import-cars", json!({ "import": j.id, "items": car_rows(&j), "next": null }))
    })
    .await
}

/// A TeslaMate car number, as text or a number.
fn tm_car(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::String(s) => s.trim().parse().ok(),
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// `PUT /imports/{import}/cars` `{ items: [{ tm_car, match }] }`: changes only the cars it names.
async fn match_cars(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path(id): Path<String>,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["items"])?;
    let Some(Value::Array(items)) = body.get("items") else {
        return Err(ApiError::bad_input("items", "Send { items: [{ tm_car, match }] }."));
    };
    // each match, a car checked against the account as every car of a body is
    let mut wanted: Vec<(i64, String)> = Vec::new();
    for (i, x) in items.iter().enumerate() {
        let Value::Object(m) = x else { return Err(ApiError::bad_input(&format!("items.{i}"), "Must be an object.")) };
        let one = JsonBody(m.clone());
        one.only(&["tm_car", "match"])
            .map_err(|_| ApiError::bad_input(&format!("items.{i}"), "Only tm_car and match."))?;
        let tm = tm_car(m.get("tm_car"))
            .ok_or_else(|| ApiError::bad_input(&format!("items.{i}.tm_car"), "A TeslaMate car number."))?;
        let word = match m.get("match") {
            Some(Value::String(s)) if s == "new" || s == "leave_out" || s == NEEDS_CAR => s.clone(),
            Some(Value::String(_)) => match one.car(&app, &v, "match") {
                Ok(Some(c)) => crate::ids::car(c),
                Ok(None) | Err(_) => {
                    return Err(ApiError::invalid(&format!("items.{i}.match"), "A car id, \"new\" or \"leave_out\"."));
                }
            },
            _ => return Err(ApiError::bad_input(&format!("items.{i}.match"), "A car id, \"new\" or \"leave_out\".")),
        };
        wanted.push((tm, word));
    }
    let summary = format!(
        "Matched imported cars: {}.",
        wanted.iter().map(|(t, m)| format!("{t} -> {m}")).collect::<Vec<_>>().join(", ")
    );
    let out = with_jobs(&app, move |app, all| {
        let mut j = job(all, &id)?;
        checked(&j)?;
        if !matches!(j.status.as_str(), "checking" | "ready") {
            return Err(ApiError::conflict(
                "not_matchable",
                format!("This import is {}; cars can't be matched now.", j.status),
            ));
        }
        for (i, (tm, m)) in wanted.iter().enumerate() {
            let Some(c) = j.cars.iter().find(|c| c.car.tm_car == *tm) else {
                return Err(ApiError::invalid(&format!("items.{i}.tm_car"), "Not a car in this file."));
            };
            if c.matched.as_deref() == Some(WITH) {
                return Err(ApiError::invalid(
                    &format!("items.{i}.tm_car"),
                    "Its rows go with another car of the file: match that one.",
                ));
            }
            if m == NEEDS_CAR && !c.car.needs_car {
                return Err(ApiError::invalid(&format!("items.{i}.match"), "Only rows without a VIN wait for a car."));
            }
        }
        for (tm, m) in &wanted {
            if let Some(c) = j.cars.iter_mut().find(|c| c.car.tm_car == *tm) {
                c.matched = Some(m.clone());
            }
        }
        // one car here takes at most one imported car (rows that need a car may go to one that has another's)
        let mut seen: HashMap<&str, i64> = HashMap::new();
        for c in j.cars.iter().filter(|c| !c.car.needs_car) {
            if let Some(m) = c.matched.as_deref().filter(|m| !matches!(*m, "new" | "leave_out" | WITH | NEEDS_CAR))
                && let Some(other) = seen.insert(m, c.car.tm_car)
            {
                let i = wanted.iter().position(|(t, _)| *t == c.car.tm_car || *t == other).unwrap_or(0);
                return Err(ApiError::invalid(
                    &format!("items.{i}.match"),
                    &format!("Car {m} is already matched to imported car {other}."),
                ));
            }
        }
        save(app, &j)?;
        all.lock().jobs.insert(j.id.clone(), j.clone());
        Ok(json!({ "import": j.id, "items": car_rows(&j), "next": null }))
    })
    .await?;
    let iid = out["import"].as_str().unwrap_or_default().to_string();
    v.audit(&app, "import-cars", Some(&target("import", None, &iid)), &summary).await?;
    Ok(Json(out))
}

/// `POST /imports/{import}/start` `{ currency?, time_zone?, distance_unit?, temperature_unit? }` (the last three for
/// a TeslaFi import only).
async fn start(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path(id): Path<String>,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["currency", "time_zone", "distance_unit", "temperature_unit"])?;
    let currency = body.str("currency")?.map(|c| c.trim().to_ascii_uppercase());
    if currency.as_deref().is_some_and(|c| c.len() != 3 || !c.chars().all(|x| x.is_ascii_uppercase())) {
        return Err(ApiError::invalid("currency", "A currency code such as EUR."));
    }
    let time_zone = body.str("time_zone")?.map(|z| z.trim().to_string()).filter(|z| !z.is_empty());
    if time_zone.as_deref().is_some_and(|z| !import::teslafi::valid_time_zone(z)) {
        return Err(ApiError::invalid("time_zone", "An IANA time zone such as Europe/Berlin."));
    }
    let distance_unit = match body.str("distance_unit")? {
        None => None,
        Some(w) => Some(
            import::teslafi::Distance::from_word(w).ok_or_else(|| ApiError::invalid("distance_unit", "mi or km."))?,
        ),
    };
    let temperature_unit = match body.str("temperature_unit")? {
        None => None,
        Some(w) => Some(
            import::teslafi::Temperature::from_word(w)
                .ok_or_else(|| ApiError::invalid("temperature_unit", "c or f."))?,
        ),
    };
    let tf = TeslafiSettings { distance_unit, temperature_unit, time_zone };
    let e = app.service::<Engine>()?;
    let (out, name) = with_jobs(&app, move |app, all| {
        let mut j = job(all, &id)?;
        if j.status != "ready" {
            return Err(ApiError::conflict(
                "not_ready",
                format!("This import is {}; it can start once the whole file is checked.", j.status),
            ));
        }
        if !j.is_teslafi() && tf != TeslafiSettings::default() {
            let field = if tf.time_zone.is_some() {
                "time_zone"
            } else if tf.distance_unit.is_some() {
                "distance_unit"
            } else {
                "temperature_unit"
            };
            return Err(ApiError::invalid(field, "Only for a TeslaFi import."));
        }
        if j.is_can() {
            return start_can(app, all, e, j);
        }
        if j.cars.iter().any(|c| c.matched.is_none()) {
            return Err(ApiError::conflict(
                "cars_unmatched",
                "Match every imported car first (a car, new, or leave out).",
            ));
        }
        // one car here takes at most one imported car (as PUT /cars checks: a job matched before, or by an older
        // server's suggestion, is checked again here)
        let mut seen: HashMap<&str, i64> = HashMap::new();
        for c in j.cars.iter().filter(|c| !c.car.needs_car) {
            if let Some(m) = c.matched.as_deref().filter(|m| !matches!(*m, "new" | "leave_out" | WITH | NEEDS_CAR))
                && let Some(other) = seen.insert(m, c.car.tm_car)
            {
                return Err(ApiError::conflict(
                    "cars_unmatched",
                    format!(
                        "Car {m} is matched to imported cars {other} and {}: match each to a car of its own.",
                        c.car.tm_car
                    ),
                ));
            }
        }
        let mut m = im::Matches { hold_import: Some(j.id.clone()), ..im::Matches::default() };
        for c in &j.cars {
            if let Some(to) = c.car.joins.filter(|_| c.matched.as_deref() == Some(WITH)) {
                m.joins.insert(c.car.tm_car, to);
                continue;
            }
            match c.matched.as_deref() {
                Some("leave_out") => m.leave_out.push(c.car.tm_car),
                Some(NEEDS_CAR) => m.hold.push(c.car.tm_car),
                Some("new") => {
                    m.to.insert(c.car.tm_car, None);
                }
                Some(car) => {
                    let id = crate::ids::parse_car(car).filter(|c| app.car_exists(*c)).ok_or_else(|| {
                        ApiError::conflict(
                            "cars_unmatched",
                            format!("Car {car} is no longer here: match that car again."),
                        )
                    })?;
                    m.to.insert(c.car.tm_car, Some(id));
                }
                None => {}
            }
        }
        j.status = "importing".into();
        j.pass = 2;
        j.error = None;
        j.bytes_read = 0;
        if j.is_teslafi() {
            j.teslafi = Some(tf.clone());
        }
        save(app, &j)?;
        let live = Live::new(if j.is_teslafi() { 1 } else { 2 });
        {
            let mut st = all.lock();
            st.jobs.insert(j.id.clone(), j.clone());
            st.live.insert(j.id.clone(), live.clone());
        }
        let (app2, id2, all2) = (app.clone(), j.id.clone(), imports(app)?);
        let tables_before: Vec<CarRow> =
            j.cars.iter().filter(|c| c.matched.as_deref() != Some(WITH)).cloned().collect();
        let teslafi = j.is_teslafi();
        let parts = j.parts();
        tokio::task::spawn_blocking(move || {
            let mut open = im::counted(opener(&app2, &id2), live.read.clone(), live.stop.clone());
            let pairs: Vec<(BackupCar, Value)> =
                tables_before.iter().map(|c| (c.car.clone(), json!(c.matched))).collect();
            let mut went: BTreeMap<i64, model::CarId> = BTreeMap::new();
            let mut note = |cars: &[import::CarReport]| {
                // (rows held for a car to be chosen are on a stand-in car far above any real one)
                went = cars
                    .iter()
                    .filter_map(|c| c.car.filter(|id| *id < 0xFE00_0000).map(|id| (c.tm_car_id, id)))
                    .collect();
            };
            let r = if teslafi && parts.len() > 1 {
                im::teslafi_run_files(&e, several_files(&app2, &id2, &parts, &live), &m, &tf).map(|done| {
                    note(&done.cars);
                    im::teslafi_report_json(&done.report, &done.cars, &pairs, true)
                })
            } else if teslafi {
                open().map_err(import::Error::Io).and_then(|r| im::teslafi_run(&e, r, &m, &tf)).map(|done| {
                    note(&done.cars);
                    im::teslafi_report_json(&done.report, &done.cars, &pairs, true)
                })
            } else {
                im::run(&e, open, &m, currency).map(|done| {
                    note(&done.cars);
                    im::report_json(&done.report, &done.cars, &pairs, true)
                })
            };
            if live.stop.load(Ordering::Relaxed) {
                all2.lock().live.remove(&id2);
                return;
            }
            let now = app2.now();
            let lots = engine::rehome::held_of_import(e.db(), &id2).unwrap_or_default();
            finish(&app2, &all2, &id2, |j| match r {
                Ok(report) => {
                    j.status = "done".into();
                    j.finished_ms = Some(now);
                    j.bytes_read = j.bytes_total;
                    j.report = Some(report);
                    j.holds = lots.iter().map(lot_view).collect();
                    j.rehomed = REHOME_VERSION;
                    j.went = went;
                }
                Err(err) => {
                    let (code, message) = im::error_of(&err);
                    j.status = "failed".into();
                    j.finished_ms = Some(now);
                    j.error = Some(JobError { code: code.into(), message });
                }
            });
        });
        let view = view_of(all, &j);
        Ok((view, j.name))
    })
    .await?;
    let iid = out["id"].as_str().unwrap_or_default().to_string();
    v.audit(&app, "import-started", Some(&target("import", None, &iid)), &format!("Started importing {name}.")).await?;
    Ok(Json(out))
}

/// Pass 2 of a CAN export: each log checked, stored and judged as an upload is (a run started again skips what an
/// earlier one stored).
fn start_can(app: &AppState, all: &Imports, e: Arc<Engine>, mut j: Job) -> ApiResult<(Value, String)> {
    j.status = "importing".into();
    j.pass = 2;
    j.error = None;
    j.bytes_read = 0;
    save(app, &j)?;
    let live = Live::new(1);
    {
        let mut st = all.lock();
        st.jobs.insert(j.id.clone(), j.clone());
        st.live.insert(j.id.clone(), live.clone());
    }
    let (app2, id2, all2) = (app.clone(), j.id.clone(), imports(app)?);
    tokio::task::spawn_blocking(move || {
        let gate = all2.gate.get().cloned();
        let pass =
            can::Pass { store: true, spool: spool_of(&app2, &id2), gate: gate.as_ref(), stop: live.stop.clone() };
        let mut open = im::counted(opener(&app2, &id2), live.read.clone(), live.stop.clone());
        let r = can::guarded(|| open().map_err(can::CanImportError::from).and_then(|r| can::run(&e, r, &pass)));
        let _ = std::fs::remove_dir_all(spool_of(&app2, &id2));
        if live.stop.load(Ordering::Relaxed) {
            // discarded: its folder went with it (unless the spool came back meanwhile: then it goes now)
            if !all2.lock().jobs.contains_key(&id2) {
                let _ = std::fs::remove_dir(folder(&app2, &id2));
            }
            all2.lock().live.remove(&id2);
            return;
        }
        let now = app2.now();
        finish(&app2, &all2, &id2, |j| match r {
            Ok(report) => {
                j.status = "done".into();
                j.finished_ms = Some(now);
                j.bytes_read = j.bytes_total;
                j.report = Some(report);
            }
            Err(err) => {
                let (code, message) = can::error_of(&err);
                j.status = "failed".into();
                j.finished_ms = Some(now);
                j.error = Some(JobError { code: code.into(), message });
            }
        });
    });
    let view = view_of(all, &j);
    Ok((view, j.name))
}

/// A lot of rows that waits for a car, as `/imports` and `/import-holds` write it.
fn lot_view(h: &engine::rehome::Held) -> Value {
    json!({
        "hold": h.id,
        "import": h.import,
        "tm_car": h.tm_car.map(|t| t.to_string()),
        "name": h.name,
        "source": h.source.site().map(model::ImportSite::word),
        "status": h.status,
        "why": h.why,
        "from_car": h.from_car.map(crate::ids::car),
        "to_car": h.to_car.map(crate::ids::car),
        "rows": h.held.rows,
        "drives": h.held.drives,
        "charges": h.held.charges,
        "first_ms": h.held.first_ms,
        "last_ms": h.held.last_ms,
        "odometer_km": match (h.first_odo_km, h.last_odo_km) {
            (Some(a), Some(b)) => json!([a.round(), b.round()]),
            _ => Value::Null,
        },
    })
}

/// A move as `/imports` lists it: where the rows went (`to_car` a car, or null with `hold`: they wait for one).
fn move_view(e: &Engine, m: &engine::rehome::Move, kind: &str) -> Value {
    let name = |c: Option<model::CarId>| c.and_then(|c| e.db().car(c).ok().flatten()).map(|c| c.name);
    json!({
        "kind": kind,
        "at_ms": m.at_ms,
        "from_car": m.from_car.map(crate::ids::car),
        "from_name": name(m.from_car),
        "to_car": m.to_car.map(crate::ids::car),
        "to_name": name(m.to_car),
        "hold": m.held,
        "rows": m.moved.rows,
        "drives": m.moved.drives,
        "charges": m.moved.charges,
        "owner_entries": m.moved.owner_entries,
        "first_ms": m.moved.first_ms,
        "last_ms": m.moved.last_ms,
    })
}

/// The sentence of a move's audit entry.
fn move_said(e: &Engine, m: &engine::rehome::Move, why: &str) -> String {
    let name = |c: model::CarId| e.db().car(c).ok().flatten().map_or_else(|| crate::ids::car(c), |c| c.name);
    let what = m.moved.words();
    let owner = match m.moved.owner_entries {
        0 => String::new(),
        n => format!(
            ", with the {n} cost{} and purpose{} entered on them",
            if n == 1 { "" } else { "s" },
            if n == 1 { "" } else { "s" }
        ),
    };
    match (m.from_car, m.to_car) {
        (Some(f), Some(t)) => format!("Moved {what}{owner} from {} to {}: {why}.", name(f), name(t)),
        (Some(f), None) => format!("Moved {what}{owner} off {} to wait for a car: {why}.", name(f)),
        (None, Some(t)) => format!("Gave {what} that waited for a car to {}.", name(t)),
        (None, None) => format!("Moved {what}: {why}."),
    }
}

/// `GET /import-holds`: every lot of imported rows that waits for a car (or waited), oldest first.
async fn holds(State(app): State<AppState>, q: Params) -> ApiResult<Response> {
    q.wants_csv()?;
    let e = app.service::<Engine>()?;
    let items = app
        .blocking(move |_| {
            engine::rehome::held_all(e.db())
                .map_err(|x| ApiError::server(x.to_string()))
                .map(|v| v.iter().map(lot_view).collect::<Vec<_>>())
        })
        .await?;
    crate::csv::answer(&q, "import-holds", json!({ "items": items, "next": null }))
}

/// `POST /import-holds/{hold}` `{ match: car | "new" | "ignore" | "wait" }`: gives the lot's rows a car (a new one,
/// named as the rows are, with `new`), leaves them aside (`ignore`: kept, not shown in any car) or takes them back to
/// wait (`wait`). A car's rows that moved are rebuilt with it.
async fn assign_hold(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path(hold): Path<String>,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["match"])?;
    let word = body.req_str("match")?.to_string();
    let to = match word.as_str() {
        "new" | "ignore" | "wait" => None,
        _ => Some(
            body.car(&app, &v, "match")?
                .ok_or_else(|| ApiError::invalid("match", "A car id, \"new\", \"ignore\" or \"wait\"."))?,
        ),
    };
    let e = app.service::<Engine>()?;
    let all = imports(&app)?;
    let (e2, h2) = (e.clone(), hold.clone());
    let (lot, mv) = with_jobs(&app, move |app, all| {
        let db = e2.db();
        let h = engine::rehome::held(db, &h2)
            .map_err(|x| ApiError::server(x.to_string()))?
            .ok_or_else(|| ApiError::not_found("no_such_hold"))?;
        if h.status == "assigned" {
            return Err(ApiError::conflict("already_assigned", "These rows went to a car already."));
        }
        let mv = match (word.as_str(), to) {
            ("ignore" | "wait", _) => {
                engine::rehome::set_ignored(db, &h2, word == "ignore").map_err(|x| ApiError::server(x.to_string()))?;
                None
            }
            (_, car) => {
                let car = match car {
                    Some(c) => c,
                    None => {
                        let name =
                            h.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Imported car".into());
                        engine::rehome::new_car(&e2, &name).map_err(|x| ApiError::server(x.to_string()))?
                    }
                };
                let m = engine::rehome::assign(&e2, &h2, car).map_err(|x| ApiError::server(x.to_string()))?;
                if let Err(x) = e2.rebuild_because(
                    engine::Scope::car(car),
                    engine::jobs::What::WHOLE,
                    engine::versions::Cause::import_moved(),
                    true,
                ) {
                    tracing::warn!("rebuild after a move: {x}");
                }
                Some(m)
            }
        };
        let lot = engine::rehome::held(db, &h2).map_err(|x| ApiError::server(x.to_string()))?.unwrap_or(h);
        // the import it came from lists it as it is now, and the move
        if let Some(id) = lot.import.clone() {
            let mut st = all.lock();
            if let Some(j) = st.jobs.get_mut(&id) {
                j.holds = engine::rehome::held_of_import(db, &id).unwrap_or_default().iter().map(lot_view).collect();
                if let Some(m) = &mv {
                    j.moves.push(move_view(&e2, m, "assigned"));
                }
                let j = j.clone();
                drop(st);
                save(app, &j)?;
            }
        }
        Ok((lot, mv))
    })
    .await?;
    drop(all);
    let target = target("import-hold", None, &hold);
    match &mv {
        Some(m) => {
            let said = move_said(&e, m, "an admin chose their car");
            v.audit(&app, "import-rows-moved", None, &said).await?;
        }
        None => {
            let said = if lot.status == "ignored" {
                format!("Left aside {} that wait for a car (kept).", lot.held.words())
            } else {
                format!("Took back {} to wait for a car.", lot.held.words())
            };
            v.audit(&app, "import-rows-ignored", Some(&target), &said).await?;
        }
    }
    Ok(Json(json!({ "hold": lot_view(&lot), "move": mv.as_ref().map(|m| move_view(&e, m, "assigned")) })))
}

/// Checks the rows of every done TeslaFi import against today's matching, once (in the background at start): each
/// import's kept files are read again (nothing is read into a car) and rows an earlier server put on the wrong car
/// are moved to the right one, or to wait for a car; without the files, a car's own odometer tells which imported
/// drives can't be its own. Nothing is deleted; each move is in the audit log (`import-rows-moved`, in the server's
/// name) and on the import; each car that lost or got rows is rebuilt.
pub fn spawn_rehome(app: &AppState) -> tokio::task::JoinHandle<()> {
    let app = app.clone();
    tokio::task::spawn_blocking(move || {
        // a server whose data moved away writes nothing
        if crate::moving::data_moved(&app) {
            return;
        }
        if let Err(e) = rehome(&app) {
            tracing::warn!("checking imported rows' cars: {}", e.message);
        }
    })
}

/// The first admin was made: the start's one-time pass over imported rows, which notes nothing on a server no one
/// has set up (such a server writes nothing of its own to the database), is noted done now, with nothing to check.
pub(crate) fn first_admin_made(app: &AppState) {
    if app.service::<Imports>().is_ok() && app.service::<Engine>().is_ok() {
        drop(spawn_rehome(app));
    }
}

fn rehome(app: &AppState) -> ApiResult<()> {
    let all = imports(app)?;
    let e = app.service::<Engine>()?;
    load(app, &all)?;
    // a move names no one item (rows of an import, from one car to another or to wait): the sentence names them
    let audit = |summary: &str| {
        let details = crate::ctx::audit_details(None, summary);
        if let Err(x) = app.accounts.audit().record(None, None, "import-rows-moved", &details) {
            tracing::warn!("audit: {x}");
        }
    };
    let mut touched: std::collections::BTreeSet<model::CarId> = std::collections::BTreeSet::new();
    let todo: Vec<Job> = all
        .lock()
        .jobs
        .values()
        .filter(|j| j.is_teslafi() && j.status == "done" && j.rehomed < REHOME_VERSION)
        .cloned()
        .collect();
    for j in todo {
        let cars: Vec<model::CarId> =
            j.cars.iter().filter_map(|c| c.matched.as_deref().and_then(crate::ids::parse_car)).collect();
        let parts = j.parts();
        let have = (0..parts.len()).all(|k| folder(app, &j.id).join(backup_file(k)).exists());
        if !have || cars.is_empty() {
            continue;
        }
        let tf = j.teslafi.clone().unwrap_or_default();
        let live = Live::new(1);
        let r = if parts.len() > 1 {
            im::teslafi_rehome(&e, several_files(app, &j.id, &parts, &live), &tf, &cars, &j.id)
        } else {
            match opener(app, &j.id)() {
                Ok(r) => im::teslafi_rehome(
                    &e,
                    im::TeslafiFiles::<_, Vec<io::Result<(String, Backup)>>>::One(r),
                    &tf,
                    &cars,
                    &j.id,
                ),
                Err(x) => Err(import::Error::Io(x)),
            }
        };
        let (checked, moves) = match r {
            Ok(x) => x,
            Err(x) => {
                tracing::warn!("import {}: its files could not be read again: {x}", j.id);
                continue;
            }
        };
        let views: Vec<Value> = moves.iter().map(|m| move_view(&e, m, "rehomed")).collect();
        for m in &moves {
            touched.extend(m.from_car.into_iter().chain(m.to_car));
            audit(&move_said(&e, m, &format!("the import {} matched them to the wrong car", j.name)));
        }
        // the import's cars as the rows tell them now, each with the match it had (by its VIN prefix), else as a
        // check would start it
        let old: HashMap<Option<String>, Option<String>> =
            j.cars.iter().filter(|c| !c.car.needs_car).map(|c| (c.car.vin_prefix.clone(), c.matched.clone())).collect();
        let lots = engine::rehome::held_of_import(e.db(), &j.id).unwrap_or_default();
        let mut st = all.lock();
        if let Some(job) = st.jobs.get_mut(&j.id) {
            job.cars = checked
                .cars
                .into_iter()
                .map(|car| {
                    let matched = match old.get(&car.vin_prefix) {
                        Some(m) if car.vin_prefix.is_some() && !car.needs_car && car.joins.is_none() => m.clone(),
                        _ => first_match(&car),
                    };
                    CarRow { car, matched }
                })
                .collect();
            job.moves.extend(views);
            job.holds = lots.iter().map(lot_view).collect();
            job.rehomed = REHOME_VERSION;
            let job = job.clone();
            drop(st);
            save(app, &job)?;
        }
    }
    // without an import's files: what each car's own odometer says. A server no one has set up yet, with no car,
    // has nothing to check and notes nothing: it wrote nothing of its own to the database, so a move or a restore
    // into it still finds it new (the pass is noted done once its first admin is made, `/setup/fresh`)
    let cars = engine::rehome::car_ids(&e).unwrap_or_default();
    let unused = cars.is_empty() && app.accounts.needs_setup();
    if !unused && !engine::rehome::done_once(e.db(), "strays-v1").unwrap_or(true) {
        for car in cars {
            match engine::rehome::rehome_strays(&e, car) {
                Ok(moves) => {
                    for m in &moves {
                        touched.insert(car);
                        audit(&move_said(&e, m, "their odometer reads above the car's own later drives"));
                    }
                }
                Err(x) => tracing::warn!("car {car}: checking its imported drives: {x}"),
            }
        }
        let _ = engine::rehome::mark_done(e.db(), "strays-v1");
    }
    for car in touched {
        if let Err(x) = e.rebuild_because(
            engine::Scope::car(car),
            engine::jobs::What::WHOLE,
            engine::versions::Cause::import_moved(),
            false,
        ) {
            tracing::warn!("car {car}: rebuild after its rows moved: {x}");
        }
    }
    Ok(())
}

/// Every import as `/imports/{import}` gives it now, `(id, view)` (the live stream's `job` events of kind `import`).
pub(crate) fn views_now(app: &AppState) -> Vec<(String, Value)> {
    let Ok(all) = imports(app) else { return Vec::new() };
    if load(app, &all).is_err() {
        return Vec::new();
    }
    let st = all.lock();
    st.jobs.values().map(|j| (j.id.clone(), j.view(st.live.get(&j.id)))).collect()
}

/// How a damaged drives table gets its imported rows back: the kept uploads of the imports are read again, for one
/// car, nothing stored (see [`engine::repair`]). Register it with [`Engine::set_import_replay`] at the server's start.
pub fn replay(app: &AppState) -> Arc<dyn engine::repair::ImportReplay> {
    Arc::new(Replay { app: app.clone() })
}

struct Replay {
    app: AppState,
}

impl Replay {
    /// One import's drives of a car, read again from its kept files.
    fn read(&self, e: &Engine, j: &Job, plan: engine::importer::ReplayPlan) -> Result<Vec<model::Drive>, String> {
        let app = &self.app;
        let parts = j.parts();
        let live = Live::new(1);
        let r = if j.is_teslafi() {
            let tf = j.teslafi.clone().unwrap_or_default();
            if parts.len() > 1 {
                im::replay_teslafi_drives(e, several_files(app, &j.id, &parts, &live), &tf, plan)
            } else {
                match opener(app, &j.id)() {
                    Ok(r) => im::replay_teslafi_drives(
                        e,
                        im::TeslafiFiles::<_, Vec<io::Result<(String, Backup)>>>::One(r),
                        &tf,
                        plan,
                    ),
                    Err(x) => Err(import::Error::Io(x)),
                }
            }
        } else {
            im::replay_drives(e, opener(app, &j.id), plan)
        };
        r.map_err(|x| x.to_string())
    }
}

impl engine::repair::ImportReplay for Replay {
    fn drives(&self, e: &Engine, car: model::CarId) -> engine::repair::Replayed {
        use engine::repair::{Gone, HeldOut};
        let app = &self.app;
        let mut out = engine::repair::Replayed::default();
        let Ok(all) = imports(app) else { return out };
        if load(app, &all).is_err() {
            return out;
        }
        let mut jobs: Vec<Job> =
            all.lock().jobs.values().filter(|j| j.status == "done" && !j.is_can()).cloned().collect();
        // newest first: where two imports hold the same time, the newer one is what the car has
        jobs.sort_by_key(|j| std::cmp::Reverse((j.started_ms, j.id.clone())));
        let mut sets: Vec<Vec<model::Drive>> = Vec::new();
        let me = crate::ids::car(car);
        for j in jobs {
            let source = if j.is_teslafi() { model::Source::Teslafi } else { model::Source::Teslamate };
            // the cars of the file that went to this car: matched to it, or made for it (`new`)
            let mine: Vec<&CarRow> = j
                .cars
                .iter()
                .filter(|c| c.matched.as_deref() == Some(me.as_str()) || j.went.get(&c.car.tm_car) == Some(&car))
                .collect();
            if mine.is_empty() {
                continue;
            }
            let from_ms = mine.iter().filter_map(|c| c.car.first_ms).min();
            let to_ms = mine.iter().filter_map(|c| c.car.last_ms).max();
            let gone = |why: String| Gone { source, name: j.name.clone(), from_ms, to_ms, why };
            let names = kept_names(&folder(app, &j.id));
            if !(0..j.parts().len()).all(|k| names.contains(&backup_file(k))) {
                out.gone.push(gone("its upload is not kept".into()));
                continue;
            }
            let joins: HashMap<i64, i64> = j
                .cars
                .iter()
                .filter(|c| c.matched.as_deref() == Some(WITH))
                .filter_map(|c| c.car.joins.map(|to| (c.car.tm_car, to)))
                .collect();
            let plan = engine::importer::ReplayPlan { car, mine: mine.iter().map(|c| c.car.tm_car).collect(), joins };
            let drives = match self.read(e, &j, plan) {
                Ok(d) => d,
                Err(why) => {
                    out.gone.push(gone(format!("its upload can't be read: {why}")));
                    continue;
                }
            };
            // rows the import's moves took off this car are not put back (they went to another car, or wait for one)
            let moved: Vec<(u64, u64)> = j
                .moves
                .iter()
                .filter(|m| m["from_car"].as_str() == Some(me.as_str()))
                .filter_map(|m| Some((m["first_ms"].as_u64()?, m["last_ms"].as_u64()?)))
                .collect();
            let (held, kept): (Vec<_>, Vec<_>) =
                drives.into_iter().partition(|d| moved.iter().any(|(a, b)| d.start_ms >= *a && d.start_ms <= *b));
            if !held.is_empty() {
                out.held_out.push(HeldOut {
                    source,
                    name: j.name.clone(),
                    rows: held.len() as u64,
                    from_ms: held.iter().map(|d| d.start_ms).min(),
                    to_ms: held.iter().map(|d| d.end_ms).max(),
                });
            }
            sets.push(kept);
        }
        out.drives = newest_wins(sets);
        out
    }
}

/// The drives of several imports' kept uploads, the newest import's set first: a drive of an older import is left out
/// where a newer import has a drive of the same source over the same time (a newer file replaced it, or took it
/// away), and a row id comes once.
fn newest_wins(sets: Vec<Vec<model::Drive>>) -> Vec<model::Drive> {
    let mut out: Vec<model::Drive> = Vec::new();
    let mut ids = std::collections::HashSet::new();
    // (start, end) of what newer sets hold, by source, sorted by start
    let mut spans: HashMap<model::Source, Vec<(u64, u64)>> = HashMap::new();
    for set in sets {
        let mut mine: Vec<(model::Source, u64, u64)> = Vec::new();
        for d in set {
            let (a, b) = (d.start_ms, d.end_ms.max(d.start_ms + 1));
            let covered = spans.get(&d.source).is_some_and(|v| {
                let i = v.partition_point(|x| x.0 < b);
                v[i.saturating_sub(8)..i].iter().any(|x| a < x.1)
            });
            if covered || !ids.insert(d.id) {
                continue;
            }
            mine.push((d.source, a, b));
            out.push(d);
        }
        for (s, a, b) in mine {
            let v = spans.entry(s).or_default();
            let i = v.partition_point(|x| x.0 < a);
            v.insert(i, (a, b));
        }
    }
    out
}

#[cfg(test)]
mod replay_tests {
    use super::newest_wins;
    use model::Source;

    fn d(id: u64, start: u64, end: u64, source: Source) -> model::Drive {
        model::Drive {
            id,
            car: 1,
            start_ms: start,
            end_ms: end,
            start: None,
            end: None,
            start_place: None,
            end_place: None,
            start_address: None,
            end_address: None,
            start_odometer_km: None,
            end_odometer_km: None,
            distance_km: None,
            speed_max_kmh: None,
            power_max_kw: None,
            power_min_kw: None,
            energy_used_kwh: None,
            start_soc: None,
            end_soc: None,
            start_range_km: None,
            end_range_km: None,
            outside_temp_avg_c: None,
            inside_temp_avg_c: None,
            ascent_m: None,
            descent_m: None,
            source,
            inputs: model::Inputs::NONE,
            rules_version: 0,
        }
    }

    #[test]
    fn a_newer_import_wins_where_two_hold_the_same_time() {
        let tm = Source::Teslamate;
        // the newer import (first) holds drive 2 over 1000..2000 and no longer holds drive 1 (it was joined into 2)
        let newer = vec![d(2, 1000, 2000, tm), d(9, 5000, 6000, tm)];
        let older =
            vec![d(1, 1100, 1900, tm), d(2, 1000, 1500, tm), d(3, 8000, 9000, tm), d(4, 1500, 1800, Source::Teslafi)];
        let got: Vec<u64> = newest_wins(vec![newer, older]).iter().map(|x| x.id).collect();
        assert_eq!(got, [2, 9, 3, 4], "the older drive 1 is the newer drive 2's; ids once; another site's stays");
    }
}
