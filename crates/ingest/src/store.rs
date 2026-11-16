//! Where uploaded logs are kept: exactly as received, never replaced, and sealed with the vault (nothing is written
//! to storage in the clear, uploads in progress included).
//!
//! ```text
//! <can>/<vehicleKey>/<name>.log.xz           a finished log (sealed)
//! <can>/<vehicleKey>/<name>.log.xz.json      its sidecar (sealed)
//! <can>/<vehicleKey>/<name>.part             a file still arriving (sealed records, see vault::records)
//! <can>/<vehicleKey>/<name>.part.json        what that upload promised: length, SHA-256, when it started (sealed)
//! ```
//!
//! Each file's label (its path under the data folder) is bound into its sealing, so a file moved or copied under
//! another name doesn't open. What the bridge signed is the plaintext; it is checked after unsealing.

use crate::error::ApiError;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
}

#[derive(Clone)]
pub struct Store(Arc<Inner>);

/// Frees a file's "being uploaded" mark when the request ends, however it ends.
struct Busy(Arc<Inner>, PathBuf);
impl Drop for Busy {
    fn drop(&mut self) {
        if let Ok(mut b) = self.0.busy.lock() {
            b.remove(&self.1);
        }
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

fn with_suffix(file: &Path, suffix: &str) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
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

    fn meta(&self, file: &Path, label: &str) -> Option<PartMeta> {
        let b = self.vault.read_file(&with_suffix(file, ".part.json"), &format!("{label}.part.json")).ok()??;
        serde_json::from_slice(&b).ok()
    }

    fn state_of(&self, name: &str) -> Result<FileState, ApiError> {
        let (file, label) = self.file_of(name)?;
        if let Ok(m) = std::fs::metadata(&file) {
            let len = vault::plaintext_len(m.len()).unwrap_or(0);
            return Ok(FileState { state: State::Complete, received: len, length: Some(len) });
        }
        let part = with_suffix(&file, ".part");
        if part.exists() {
            let received = self.vault.records(&part, &format!("{label}.part")).map(|r| r.plain_len()).unwrap_or(0);
            return Ok(FileState {
                state: State::Partial,
                received,
                length: self.meta(&file, &label).map(|m| m.length),
            });
        }
        Ok(FileState { state: State::Missing, received: 0, length: None })
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
        let (file, label) = self.file_of(name)?;
        let taken = self.busy.lock().map(|mut b| b.insert(file.clone())).map_err(|_| ApiError::server("busy lock"))?;
        if !taken {
            let r = self.state_of(name)?.received;
            return Err(ApiError::new(409, "busy", "This file is being uploaded by another request.").received(r));
        }
        let _busy = Busy(self.clone(), file.clone());

        let had = self.state_of(name)?;
        if had.state == State::Complete {
            if had.length == Some(length) && self.hash_complete(&file, &label)?.0 == sha256 {
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
        let car_dir = file.parent().map(Path::to_path_buf).unwrap_or_else(|| self.dir.clone());
        if offset == 0 {
            if (self.free_bytes)(&self.dir) < length.saturating_mul(2).saturating_add(self.min_free) {
                return Err(ApiError::new(507, "no_space", "There is not enough free space for this file."));
            }
            let (mut logs, mut parts) = (0usize, 0usize);
            if let Ok(rd) = std::fs::read_dir(&car_dir) {
                for e in rd.flatten() {
                    let f = e.file_name();
                    let f = f.to_string_lossy();
                    if f.ends_with(".log.xz") {
                        logs += 1;
                    } else if f.ends_with(".part") {
                        parts += 1;
                    }
                }
            }
            if !n.sidecar && logs >= self.max_logs_per_car {
                let max = self.max_logs_per_car;
                return Err(ApiError::new(
                    507,
                    "car_full",
                    format!("This car has {max} CAN logs here already; delete old ones first."),
                )
                .received(0));
            }
            if parts >= MAX_PARTIAL {
                return Err(ApiError::new(
                    429,
                    "too_many_partial",
                    "Too many unfinished uploads for this car; finish or wait for them.",
                )
                .received(0));
            }
        }
        std::fs::create_dir_all(&car_dir)?;
        let part = with_suffix(&file, ".part");
        let part_label = format!("{label}.part");
        let meta_file = with_suffix(&file, ".part.json");
        if offset == 0 {
            let _ = std::fs::remove_file(&part);
            let meta = PartMeta { length, sha256: sha256.to_string(), started_ms: now_ms() };
            let bytes = serde_json::to_vec(&meta).map_err(ApiError::server)?;
            self.vault.write_file(&meta_file, &format!("{label}.part.json"), &bytes).map_err(vault_err)?;
        }
        let mut records = self.vault.records(&part, &part_label).map_err(vault_err)?;
        records.append(body)?;
        let received = records.plain_len();
        if received < length {
            return Ok(Received { received, length, complete: false, already: false });
        }

        // the last byte: kept only if it is the file the app hashed, sealed again as a finished file
        let discard = |tmp: Option<&Path>| {
            let _ = std::fs::remove_file(&part);
            let _ = std::fs::remove_file(&meta_file);
            if let Some(t) = tmp {
                let _ = std::fs::remove_file(t);
            }
        };
        let tmp = with_suffix(&file, ".sealing");
        let mut hash = Sha256::new();
        let mut side = Zeroizing::new(Vec::new());
        {
            let out = std::fs::File::create(&tmp)?;
            let mut w = self.vault.seal_writer(&label, std::io::BufWriter::new(out)).map_err(vault_err)?;
            let r = records.read_all(|piece| {
                hash.update(piece);
                if n.sidecar {
                    side.extend_from_slice(piece);
                }
                w.write_all(piece)
            });
            if let Err(e) = r {
                discard(Some(&tmp));
                return Err(vault_err(vault::unwrap_io(e)));
            }
            let out = w.finish()?;
            out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        }
        if hex::encode(hash.finalize()) != sha256 {
            discard(Some(&tmp));
            return Err(ApiError::new(
                422,
                "sha256_mismatch",
                "The file arrived different from its SHA-256; it was discarded. Upload it again from offset 0.",
            )
            .received(0));
        }
        if n.sidecar && serde_json::from_slice::<serde_json::Value>(&side).is_err() {
            discard(Some(&tmp));
            return Err(ApiError::new(422, "bad_sidecar", "The sidecar is not JSON; it was discarded.").received(0));
        }
        std::fs::rename(&tmp, &file)?;
        discard(None);
        if let Ok(d) = std::fs::File::open(&car_dir) {
            let _ = d.sync_all();
        }
        Ok(Received { received, length, complete: true, already: false })
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
        }))
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

    /// Drops unfinished uploads older than `max_age_ms` (an app that gave up, or someone filling the disk).
    pub async fn clean_stale(&self, max_age_ms: u64) -> usize {
        let dir = self.0.dir.clone();
        blocking(move || {
            let mut n = 0;
            for car in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                for e in std::fs::read_dir(car.path()).into_iter().flatten().flatten() {
                    let p = e.path();
                    if !p.to_string_lossy().ends_with(".part") {
                        continue;
                    }
                    let old = e
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age.as_millis() as u64 > max_age_ms);
                    if old {
                        let _ = std::fs::remove_file(&p);
                        let _ = std::fs::remove_file(with_suffix(&p, ".json"));
                        n += 1;
                    }
                }
            }
            Ok(n)
        })
        .await
        .unwrap_or(0)
    }
}
