//! Where uploaded logs are kept: exactly as received, never replaced, and sealed with the vault (nothing is written
//! to storage in the clear, uploads in progress included).
//!
//! ```text
//! <can>/<vehicleKey>/<name>.log.xz                     a finished log (sealed)
//! <can>/<vehicleKey>/<name>.log.xz.json                its sidecar (sealed)
//! <can>/<vehicleKey>/counts.json                       how many finished logs the car has (sealed)
//! <can>/<vehicleKey>/uploading/<name>.part             a file still arriving (sealed records, see vault::records)
//! <can>/<vehicleKey>/uploading/<name>.part.json        what that upload promised: length, SHA-256, when it started
//! <can>/<vehicleKey>/uploading/<name>.sealing          the finished file being sealed, renamed into place when done
//! ```
//!
//! Each file's label (its path under the data folder) is bound into its sealing, so a file moved or copied under
//! another name doesn't open. What the bridge signed is the plaintext; it is checked after unsealing.
//!
//! Built for any number of cars, each with any number of logs: an upload never lists the car's folder. The checks at
//! the start of an upload take the car's log count from memory (a bounded cache of recently used cars) or its
//! `counts.json` (kept up to date as logs arrive; counted from the folder only once, for a car stored before counts
//! were kept), and its unfinished uploads from its small `uploading/` folder. Each car has its own lock, so cars upload
//! in parallel. [`Store::clean_stale`] looks only in the `uploading/` folders.

use crate::cache::{KeyLocks, Lru};
use crate::error::ApiError;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use trust::LogName;
use vault::Vault;
use zeroize::Zeroizing;

pub const MAX_CHUNK: u64 = 16 << 20;
pub const MAX_FILE: u64 = 1 << 30;
pub const MAX_SIDECAR: u64 = 1 << 20;
/// Unfinished uploads at once per car (the bridge sends one at a time).
pub const MAX_PARTIAL: usize = 16;
/// Finished logs per car: a safety net against a stolen token making endless tiny files.
pub const MAX_LOGS_PER_CAR: usize = 200_000;
/// Free space always left for everything else.
pub const MIN_FREE: u64 = 2 << 30;
/// Unfinished uploads older than this are dropped.
pub const STALE_PARTIAL_MS: u64 = 2 * 24 * 3600 * 1000;
/// Cars whose log count is kept in memory (the others read their `counts.json` when they next upload).
pub const CACHED_CARS: usize = 16_384;
/// A car's folder of uploads still arriving.
pub const UPLOADING: &str = "uploading";
const COUNTS: &str = "counts.json";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Complete,
    Partial,
    Missing,
    Invalid,
    Forbidden,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FileState {
    pub state: State,
    pub received: u64,
    pub length: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Received {
    pub received: u64,
    pub length: u64,
    pub complete: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub already: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Counts {
    logs: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PartMeta {
    length: u64,
    sha256: String,
    started_ms: u64,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

type FreeBytes = Box<dyn Fn(&Path) -> u64 + Send + Sync>;

struct Inner {
    dir: PathBuf,
    vault: Arc<Vault>,
    busy: Mutex<HashSet<PathBuf>>,
    min_free: u64,
    max_logs_per_car: usize,
    free_bytes: FreeBytes,
    /// Finished logs per car key, for recently used cars.
    logs: Mutex<Lru<u64>>,
    locks: KeyLocks<Mutex<()>>,
    /// Whole car folders listed (only for a car from before `counts.json`; tests check uploads never do it).
    scans: AtomicU64,
}

/// Where one upload's files are.
struct Paths {
    key: String,
    car_dir: PathBuf,
    up_dir: PathBuf,
    file: PathBuf,
    label: String,
    part: PathBuf,
    part_label: String,
    meta: PathBuf,
    meta_label: String,
    sealing: PathBuf,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct Store(Arc<Inner>);

/// Frees a file's "being uploaded" mark when the request ends, however it ends.
struct Busy(Arc<Inner>, PathBuf);
impl Drop for Busy {
    fn drop(&mut self) {
        lock(&self.0.busy).remove(&self.1);
    }
}

fn bad_name() -> ApiError {
    ApiError::new(400, "bad_name", "That is not a CAN Bridge log name.")
}

fn free_space(dir: &Path) -> u64 {
    let mut d = dir.to_path_buf();
    loop {
        if let Ok(s) = rustix::fs::statvfs(&d) {
            return s.f_bavail.saturating_mul(s.f_frsize);
        }
        if !d.pop() {
            return u64::MAX;
        }
    }
}

fn vault_err(e: vault::Error) -> ApiError {
    ApiError::server(e)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, ApiError> + Send + 'static) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f).await.map_err(ApiError::server)?
}

impl Inner {
    fn file_of(&self, name: &str) -> Result<(PathBuf, String), ApiError> {
        let n = LogName::parse(name).ok_or_else(bad_name)?;
        Ok((self.dir.join(n.vehicle_key).join(name), format!("can/{}/{name}", n.vehicle_key)))
    }

    fn paths(&self, name: &str) -> Result<Paths, ApiError> {
        let n = LogName::parse(name).ok_or_else(bad_name)?;
        let key = n.vehicle_key;
        let car_dir = self.dir.join(key);
        let up_dir = car_dir.join(UPLOADING);
        let up_label = format!("can/{key}/{UPLOADING}");
        Ok(Paths {
            key: key.to_string(),
            file: car_dir.join(name),
            label: format!("can/{key}/{name}"),
            part: up_dir.join(format!("{name}.part")),
            part_label: format!("{up_label}/{name}.part"),
            meta: up_dir.join(format!("{name}.part.json")),
            meta_label: format!("{up_label}/{name}.part.json"),
            sealing: up_dir.join(format!("{name}.sealing")),
            car_dir,
            up_dir,
        })
    }

    /// SHA-256 and length of a finished file's plaintext.
    fn hash_complete(&self, file: &Path, label: &str) -> Result<(String, u64), ApiError> {
        let f = std::fs::File::open(file)?;
        let len = f.metadata()?.len();
        let mut r = self.vault.open_reader(label, std::io::BufReader::new(f), len).map_err(vault_err)?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut total = 0u64;
        loop {
            let n = r.read(&mut buf).map_err(|e| vault_err(vault::unwrap_io(e)))?;
            if n == 0 {
                return Ok((hex::encode(h.finalize()), total));
            }
            total += n as u64;
            h.update(&buf[..n]);
        }
    }

    fn meta(&self, p: &Paths) -> Option<PartMeta> {
        let b = self.vault.read_file(&p.meta, &p.meta_label).ok()??;
        serde_json::from_slice(&b).ok()
    }

    fn state_of(&self, name: &str) -> Result<FileState, ApiError> {
        let p = self.paths(name)?;
        if let Ok(m) = std::fs::metadata(&p.file) {
            let len = vault::plaintext_len(m.len()).unwrap_or(0);
            return Ok(FileState { state: State::Complete, received: len, length: Some(len) });
        }
        if p.part.exists() {
            let received = self.vault.records(&p.part, &p.part_label).map(|r| r.plain_len()).unwrap_or(0);
            return Ok(FileState { state: State::Partial, received, length: self.meta(&p).map(|m| m.length) });
        }
        Ok(FileState { state: State::Missing, received: 0, length: None })
    }

    fn counts_file(&self, key: &str) -> (PathBuf, String) {
        (self.dir.join(key).join(COUNTS), format!("can/{key}/{COUNTS}"))
    }

    /// The car's finished logs (the caller holds the car's lock): from memory, else its `counts.json`, else, for a
    /// car stored before counts were kept, counted from its folder once and written down. `starting`: an upload is
    /// starting, so a new car gets its folder and a `counts.json` of 0 now (its folder is then never counted).
    fn logs_of(&self, key: &str, car_dir: &Path, starting: bool) -> Result<u64, ApiError> {
        if let Some(n) = lock(&self.logs).get(key) {
            return Ok(*n);
        }
        let (file, label) = self.counts_file(key);
        let saved = match self.vault.read_file(&file, &label) {
            Ok(Some(b)) => serde_json::from_slice::<Counts>(&b).ok().map(|c| c.logs),
            // damaged: counted again below
            _ => None,
        };
        let n = match saved {
            Some(n) => n,
            None if !car_dir.exists() => {
                if starting {
                    std::fs::create_dir_all(car_dir)?;
                    self.save_logs(key, 0)?;
                }
                0
            }
            None => {
                self.scans.fetch_add(1, Ordering::Relaxed);
                let n = std::fs::read_dir(car_dir)?
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".log.xz"))
                    .count() as u64;
                self.save_logs(key, n)?;
                n
            }
        };
        lock(&self.logs).put(key.to_string(), n);
        Ok(n)
    }

    fn save_logs(&self, key: &str, n: u64) -> Result<(), ApiError> {
        let (file, label) = self.counts_file(key);
        let bytes = serde_json::to_vec(&Counts { logs: n }).map_err(ApiError::server)?;
        self.vault.write_file(&file, &label, &bytes).map_err(vault_err)
    }

    /// Unfinished uploads of a car: its `uploading/` folder holds only those (a few files), never its logs.
    fn parts_of(up_dir: &Path) -> usize {
        std::fs::read_dir(up_dir)
            .map(|rd| rd.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".part")).count())
            .unwrap_or(0)
    }

    fn receive(
        self: &Arc<Self>,
        name: &str,
        body: &[u8],
        offset: u64,
        length: u64,
        sha256: &str,
    ) -> Result<Received, ApiError> {
        let n = LogName::parse(name).ok_or_else(bad_name)?;
        if length == 0 {
            return Err(ApiError::new(400, "bad_length", "Upload-Length must be the whole file's size in bytes."));
        }
        if sha256.len() != 64 || !sha256.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(ApiError::new(400, "bad_sha256", "Upload-SHA256 must be the whole file's SHA-256 in hex."));
        }
        let limit = if n.sidecar { MAX_SIDECAR } else { MAX_FILE };
        if length > limit {
            let what = if n.sidecar { "sidecar" } else { "log" };
            return Err(ApiError::new(413, "too_large", format!("A {what} can be at most {limit} bytes.")));
        }
        let p = self.paths(name)?;
        let taken = lock(&self.busy).insert(p.file.clone());
        if !taken {
            let r = self.state_of(name)?.received;
            return Err(ApiError::new(409, "busy", "This file is being uploaded by another request.").received(r));
        }
        let _busy = Busy(self.clone(), p.file.clone());

        let had = self.state_of(name)?;
        if had.state == State::Complete {
            if had.length == Some(length) && self.hash_complete(&p.file, &p.label)?.0 == sha256 {
                return Ok(Received { received: length, length, complete: true, already: true });
            }
            return Err(ApiError::new(409, "exists_different", "A different file with this name is already here."));
        }
        if had.state == State::Partial && had.length.is_some_and(|l| l != length) {
            return Err(ApiError::new(
                409,
                "length_changed",
                "Upload-Length differs from the upload already started; start again at offset 0 with a new name or the same length.",
            )
            .received(had.received));
        }
        if offset != had.received {
            return Err(ApiError::new(409, "offset_mismatch", format!("Expected Upload-Offset {}.", had.received))
                .received(had.received));
        }
        if offset + body.len() as u64 > length {
            return Err(ApiError::new(400, "past_end", "The chunk goes past Upload-Length.").received(had.received));
        }
        if body.len() as u64 > MAX_CHUNK {
            return Err(ApiError::new(413, "chunk_too_large", format!("A chunk can be at most {MAX_CHUNK} bytes."))
                .received(had.received));
        }
        if offset == 0 {
            if (self.free_bytes)(&self.dir) < length.saturating_mul(2).saturating_add(self.min_free) {
                return Err(ApiError::new(507, "no_space", "There is not enough free space for this file."));
            }
            let car_lock = self.locks.get(&p.key);
            let _car = lock(&car_lock);
            let logs = self.logs_of(&p.key, &p.car_dir, true)?;
            if !n.sidecar && logs >= self.max_logs_per_car as u64 {
                let max = self.max_logs_per_car;
                return Err(ApiError::new(
                    507,
                    "car_full",
                    format!("This car has {max} CAN logs here already; delete old ones first."),
                )
                .received(0));
            }
            if Inner::parts_of(&p.up_dir) >= MAX_PARTIAL {
                return Err(ApiError::new(
                    429,
                    "too_many_partial",
                    "Too many unfinished uploads for this car; finish or wait for them.",
                )
                .received(0));
            }
            std::fs::create_dir_all(&p.up_dir)?;
            let _ = std::fs::remove_file(&p.part);
            let meta = PartMeta { length, sha256: sha256.to_string(), started_ms: now_ms() };
            let bytes = serde_json::to_vec(&meta).map_err(ApiError::server)?;
            self.vault.write_file(&p.meta, &p.meta_label, &bytes).map_err(vault_err)?;
        }
        let mut records = self.vault.records(&p.part, &p.part_label).map_err(vault_err)?;
        records.append(body)?;
        let received = records.plain_len();
        if received < length {
            return Ok(Received { received, length, complete: false, already: false });
        }

        // the last byte: kept only if it is the file the app hashed, sealed again as a finished file
        let discard = |tmp: Option<&Path>| {
            let _ = std::fs::remove_file(&p.part);
            let _ = std::fs::remove_file(&p.meta);
            if let Some(t) = tmp {
                let _ = std::fs::remove_file(t);
            }
        };
        let tmp = &p.sealing;
        let mut hash = Sha256::new();
        let mut side = Zeroizing::new(Vec::new());
        {
            let out = std::fs::File::create(tmp)?;
            let mut w = self.vault.seal_writer(&p.label, std::io::BufWriter::new(out)).map_err(vault_err)?;
            let r = records.read_all(|piece| {
                hash.update(piece);
                if n.sidecar {
                    side.extend_from_slice(piece);
                }
                w.write_all(piece)
            });
            if let Err(e) = r {
                discard(Some(tmp));
                return Err(vault_err(vault::unwrap_io(e)));
            }
            let out = w.finish()?;
            out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        }
        if hex::encode(hash.finalize()) != sha256 {
            discard(Some(tmp));
            return Err(ApiError::new(
                422,
                "sha256_mismatch",
                "The file arrived different from its SHA-256; it was discarded. Upload it again from offset 0.",
            )
            .received(0));
        }
        if n.sidecar && serde_json::from_slice::<serde_json::Value>(&side).is_err() {
            discard(Some(tmp));
            return Err(ApiError::new(422, "bad_sidecar", "The sidecar is not JSON; it was discarded.").received(0));
        }
        let car_lock = self.locks.get(&p.key);
        let _car = lock(&car_lock);
        // the count before this log is in place (a car counted from its folder now must not count it twice)
        let before = if n.sidecar { None } else { Some(self.logs_of(&p.key, &p.car_dir, false)?) };
        std::fs::rename(tmp, &p.file)?;
        discard(None);
        if let Ok(d) = std::fs::File::open(&p.car_dir) {
            let _ = d.sync_all();
        }
        if let Some(before) = before {
            lock(&self.logs).put(p.key.clone(), before + 1);
            // the log is kept either way; a count not written down is only a little low after a restart
            if let Err(e) = self.save_logs(&p.key, before + 1) {
                tracing::warn!("CAN upload: could not write the log count of {}: {e}", p.key);
            }
        }
        Ok(Received { received, length, complete: true, already: false })
    }

    /// Drops one car's unfinished uploads older than `max_age_ms`, and leftovers of a crash while sealing.
    fn clean_car(&self, up_dir: &Path, max_age_ms: u64) -> usize {
        let mut n = 0;
        let old = |e: &std::fs::DirEntry| {
            e.metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_millis() as u64 > max_age_ms)
        };
        for e in std::fs::read_dir(up_dir).into_iter().flatten().flatten() {
            let file_name = e.file_name();
            let f = file_name.to_string_lossy();
            let Some(name) = f.strip_suffix(".part").or_else(|| f.strip_suffix(".sealing")) else {
                // a promise whose upload is gone
                if let Some(name) = f.strip_suffix(".part.json") {
                    if !up_dir.join(format!("{name}.part")).exists() && old(&e) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
                continue;
            };
            let in_use = up_dir.parent().is_some_and(|car| lock(&self.busy).contains(&car.join(name)));
            if in_use || !old(&e) {
                continue;
            }
            let _ = std::fs::remove_file(e.path());
            if f.ends_with(".part") {
                let _ = std::fs::remove_file(up_dir.join(format!("{name}.part.json")));
                n += 1;
            }
        }
        n
    }
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>, vault: Arc<Vault>) -> Store {
        Store(Arc::new(Inner {
            dir: dir.into(),
            vault,
            busy: Mutex::new(HashSet::new()),
            min_free: MIN_FREE,
            max_logs_per_car: MAX_LOGS_PER_CAR,
            free_bytes: Box::new(free_space),
            logs: Mutex::new(Lru::new(CACHED_CARS)),
            locks: KeyLocks::new(),
            scans: AtomicU64::new(0),
        }))
    }

    /// For tests: a different free-space reading, floor and per-car cap.
    pub fn with_limits(
        dir: impl Into<PathBuf>,
        vault: Arc<Vault>,
        min_free: u64,
        max_logs_per_car: usize,
        free_bytes: impl Fn(&Path) -> u64 + Send + Sync + 'static,
    ) -> Store {
        Store(Arc::new(Inner {
            dir: dir.into(),
            vault,
            busy: Mutex::new(HashSet::new()),
            min_free,
            max_logs_per_car,
            free_bytes: Box::new(free_bytes),
            logs: Mutex::new(Lru::new(CACHED_CARS)),
            locks: KeyLocks::new(),
            scans: AtomicU64::new(0),
        }))
    }

    /// At most `cars` cars' log counts in memory.
    pub fn cache_size(self, cars: usize) -> Store {
        lock(&self.0.logs).set_cap(cars);
        self
    }

    /// Cars whose log count is in memory now.
    pub fn cached_cars(&self) -> usize {
        lock(&self.0.logs).len()
    }

    /// Entries in the per-car lock table now (about the cars uploading at this moment).
    pub fn lock_entries(&self) -> usize {
        self.0.locks.len()
    }

    /// How many times a whole car folder was listed (once per car stored before `counts.json`, never by an upload
    /// of a car that has one).
    pub fn folder_scans(&self) -> u64 {
        self.0.scans.load(Ordering::Relaxed)
    }

    /// A car's finished logs.
    pub async fn log_count(&self, key: &str) -> Result<u64, ApiError> {
        if !crate::ledger::valid_key(key) {
            return Err(bad_name());
        }
        let (inner, key) = (self.0.clone(), key.to_string());
        blocking(move || {
            let car_lock = inner.locks.get(&key);
            let _car = lock(&car_lock);
            inner.logs_of(&key, &inner.dir.join(&key), false)
        })
        .await
    }

    pub fn dir(&self) -> &Path {
        &self.0.dir
    }

    pub fn vault(&self) -> &Arc<Vault> {
        &self.0.vault
    }

    pub async fn state_of(&self, name: &str) -> Result<FileState, ApiError> {
        let (inner, name) = (self.0.clone(), name.to_string());
        blocking(move || inner.state_of(&name)).await
    }

    /// Many names at once; with `key`, another car's names answer `forbidden`.
    pub async fn status(&self, names: &[String], key: Option<&str>) -> Vec<(String, FileState)> {
        let (inner, names, key) = (self.0.clone(), names.to_vec(), key.map(str::to_string));
        blocking(move || {
            Ok(names
                .into_iter()
                .map(|name| {
                    let st = match LogName::parse(&name) {
                        Some(n) if key.as_deref().is_some_and(|k| k != n.vehicle_key) => {
                            FileState { state: State::Forbidden, received: 0, length: None }
                        }
                        Some(_) => inner.state_of(&name).unwrap_or(FileState {
                            state: State::Invalid,
                            received: 0,
                            length: None,
                        }),
                        None => FileState { state: State::Invalid, received: 0, length: None },
                    };
                    (name, st)
                })
                .collect())
        })
        .await
        .unwrap_or_default()
    }

    /// One chunk of a file. `offset` must be what is already here; `length` and `sha256` describe the whole file
    /// (the same on every chunk). The file is kept when its last byte arrives and its SHA-256 matches.
    pub async fn receive(
        &self,
        name: &str,
        body: axum::body::Bytes,
        offset: u64,
        length: u64,
        sha256: &str,
    ) -> Result<Received, ApiError> {
        let (inner, name, sha) = (self.0.clone(), name.to_string(), sha256.to_string());
        blocking(move || inner.receive(&name, &body, offset, length, &sha)).await
    }

    /// A finished file's plaintext (a log or sidecar), checked as it is unsealed. `Ok(None)`: not here.
    pub async fn read(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, ApiError> {
        let inner = self.0.clone();
        let (file, label) = self.0.file_of(name)?;
        blocking(move || inner.vault.read_file(&file, &label).map_err(vault_err)).await
    }

    /// SHA-256 and length of a finished file's plaintext.
    pub async fn hash(&self, name: &str) -> Result<(String, u64), ApiError> {
        let inner = self.0.clone();
        let (file, label) = self.0.file_of(name)?;
        blocking(move || inner.hash_complete(&file, &label)).await
    }

    pub fn exists(&self, name: &str) -> bool {
        self.0.file_of(name).is_ok_and(|(f, _)| f.exists())
    }

    /// Drops unfinished uploads older than `max_age_ms` (an app that gave up, or someone filling the disk). Looks in
    /// each car's `uploading/` folder only: the work is one folder per car plus the uploads there, not every log.
    pub async fn clean_stale(&self, max_age_ms: u64) -> usize {
        let inner = self.0.clone();
        blocking(move || {
            let mut n = 0;
            for car in std::fs::read_dir(&inner.dir).into_iter().flatten().flatten() {
                let name = car.file_name();
                if name.to_str().is_none_or(|k| !crate::ledger::valid_key(k)) {
                    continue;
                }
                n += inner.clean_car(&car.path().join(UPLOADING), max_age_ms);
            }
            Ok(n)
        })
        .await
        .unwrap_or(0)
    }
}
