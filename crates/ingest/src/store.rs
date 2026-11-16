//! Where uploaded logs are kept: exactly as received, never replaced.
//!
//! ```text
//! <can>/<vehicleKey>/<name>.log.xz           a finished log
//! <can>/<vehicleKey>/<name>.log.xz.json      its sidecar
//! <can>/<vehicleKey>/<name>.part             a file still arriving
//! <can>/<vehicleKey>/<name>.part.json        what that upload promised (length, SHA-256, when it started)
//! ```

use crate::error::ApiError;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use trust::LogName;

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
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

pub struct Store {
    dir: PathBuf,
    busy: Mutex<HashSet<PathBuf>>,
    min_free: u64,
    max_logs_per_car: usize,
    free_bytes: Box<dyn Fn(&Path) -> u64 + Send + Sync>,
}

/// Frees a file's "being uploaded" mark when the request ends, however it ends.
struct Busy<'a>(&'a Mutex<HashSet<PathBuf>>, PathBuf);
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        if let Ok(mut b) = self.0.lock() {
            b.remove(&self.1);
        }
    }
}

fn bad_name() -> ApiError {
    ApiError::new(400, "bad_name", "That is not a CAN Bridge log name.")
}

async fn sha256_of(file: &Path) -> std::io::Result<String> {
    let mut f = tokio::fs::File::open(file).await?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            return Ok(hex::encode(h.finalize()));
        }
        h.update(&buf[..n]);
    }
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

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Store {
        Store {
            dir: dir.into(),
            busy: Mutex::new(HashSet::new()),
            min_free: MIN_FREE,
            max_logs_per_car: MAX_LOGS_PER_CAR,
            free_bytes: Box::new(free_space),
        }
    }

    /// For tests: a different free-space reading, floor and per-car cap.
    pub fn with_limits(
        mut self,
        min_free: u64,
        max_logs_per_car: usize,
        free_bytes: impl Fn(&Path) -> u64 + Send + Sync + 'static,
    ) -> Store {
        self.min_free = min_free;
        self.max_logs_per_car = max_logs_per_car;
        self.free_bytes = Box::new(free_bytes);
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The path of a finished file (`name` checked: a key folder and a file name, nothing else).
    pub fn file_of(&self, name: &str) -> Result<PathBuf, ApiError> {
        let n = LogName::parse(name).ok_or_else(bad_name)?;
        Ok(self.dir.join(n.vehicle_key).join(name))
    }

    pub async fn state_of(&self, name: &str) -> Result<FileState, ApiError> {
        let file = self.file_of(name)?;
        if let Ok(m) = tokio::fs::metadata(&file).await {
            return Ok(FileState {
                state: State::Complete,
                received: m.len(),
                length: Some(m.len()),
            });
        }
        let part = part_of(&file);
        if let Ok(m) = tokio::fs::metadata(&part).await {
            let meta: Option<PartMeta> = tokio::fs::read(meta_of(&file))
                .await
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            return Ok(FileState {
                state: State::Partial,
                received: m.len(),
                length: meta.map(|m| m.length),
            });
        }
        Ok(FileState {
            state: State::Missing,
            received: 0,
            length: None,
        })
    }

    /// Many names at once; with `key`, another car's names answer `forbidden`.
    pub async fn status(&self, names: &[String], key: Option<&str>) -> Vec<(String, FileState)> {
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let st = match LogName::parse(name) {
                Some(n) if key.is_some_and(|k| k != n.vehicle_key) => FileState {
                    state: State::Forbidden,
                    received: 0,
                    length: None,
                },
                Some(_) => self.state_of(name).await.unwrap_or(FileState {
                    state: State::Invalid,
                    received: 0,
                    length: None,
                }),
                None => FileState {
                    state: State::Invalid,
                    received: 0,
                    length: None,
                },
            };
            out.push((name.clone(), st));
        }
        out
    }

    /// One chunk of a file. `offset` must be what is already here; `length` and `sha256` describe the whole file
    /// (the same on every chunk). The file is kept when its last byte arrives and its SHA-256 matches.
    pub async fn receive(
        &self,
        name: &str,
        body: &[u8],
        offset: u64,
        length: u64,
        sha256: &str,
    ) -> Result<Received, ApiError> {
        let n = LogName::parse(name).ok_or_else(bad_name)?;
        if length == 0 {
            return Err(ApiError::new(
                400,
                "bad_length",
                "Upload-Length must be the whole file's size in bytes.",
            ));
        }
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(ApiError::new(
                400,
                "bad_sha256",
                "Upload-SHA256 must be the whole file's SHA-256 in hex.",
            ));
        }
        let limit = if n.sidecar { MAX_SIDECAR } else { MAX_FILE };
        if length > limit {
            let what = if n.sidecar { "sidecar" } else { "log" };
            return Err(ApiError::new(
                413,
                "too_large",
                format!("A {what} can be at most {limit} bytes."),
            ));
        }
        let file = self.file_of(name)?;
        let taken = self
            .busy
            .lock()
            .map(|mut b| b.insert(file.clone()))
            .map_err(|_| ApiError::server("busy lock"))?;
        if !taken {
            let r = self.state_of(name).await?.received;
            return Err(ApiError::new(
                409,
                "busy",
                "This file is being uploaded by another request.",
            )
            .received(r));
        }
        let _busy = Busy(&self.busy, file.clone());

        let had = self.state_of(name).await?;
        if had.state == State::Complete {
            if had.length == Some(length) && sha256_of(&file).await? == sha256 {
                return Ok(Received {
                    received: length,
                    length,
                    complete: true,
                    already: true,
                });
            }
            return Err(ApiError::new(
                409,
                "exists_different",
                "A different file with this name is already here.",
            ));
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
            return Err(ApiError::new(
                409,
                "offset_mismatch",
                format!("Expected Upload-Offset {}.", had.received),
            )
            .received(had.received));
        }
        if offset + body.len() as u64 > length {
            return Err(
                ApiError::new(400, "past_end", "The chunk goes past Upload-Length.")
                    .received(had.received),
            );
        }
        if body.len() as u64 > MAX_CHUNK {
            return Err(ApiError::new(
                413,
                "chunk_too_large",
                format!("A chunk can be at most {MAX_CHUNK} bytes."),
            )
            .received(had.received));
        }
        let car_dir = file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.dir.clone());
        if offset == 0 {
            if (self.free_bytes)(&self.dir) < length.saturating_mul(2).saturating_add(self.min_free)
            {
                return Err(ApiError::new(
                    507,
                    "no_space",
                    "There is not enough free space for this file.",
                ));
            }
            let (mut logs, mut parts) = (0usize, 0usize);
            if let Ok(mut rd) = tokio::fs::read_dir(&car_dir).await {
                while let Ok(Some(e)) = rd.next_entry().await {
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
        tokio::fs::create_dir_all(&car_dir).await?;
        let part = part_of(&file);
        if offset == 0 {
            let meta = PartMeta {
                length,
                sha256: sha256.to_string(),
                started_ms: now_ms(),
            };
            tokio::fs::write(
                meta_of(&file),
                serde_json::to_vec(&meta).map_err(ApiError::server)?,
            )
            .await?;
        }
        {
            let mut f = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&part)
                .await?;
            f.write_all(body).await?;
            f.flush().await?;
        }
        let received = offset + body.len() as u64;
        if received < length {
            return Ok(Received {
                received,
                length,
                complete: false,
                already: false,
            });
        }

        // the last byte: kept only if it is the file the app hashed
        let discard = || async {
            let _ = tokio::fs::remove_file(&part).await;
            let _ = tokio::fs::remove_file(meta_of(&file)).await;
        };
        if sha256_of(&part).await? != sha256 {
            discard().await;
            return Err(ApiError::new(
                422,
                "sha256_mismatch",
                "The file arrived different from its SHA-256; it was discarded. Upload it again from offset 0.",
            )
            .received(0));
        }
        if n.sidecar {
            let ok = tokio::fs::read(&part)
                .await
                .ok()
                .is_some_and(|b| serde_json::from_slice::<serde_json::Value>(&b).is_ok());
            if !ok {
                discard().await;
                return Err(ApiError::new(
                    422,
                    "bad_sidecar",
                    "The sidecar is not JSON; it was discarded.",
                )
                .received(0));
            }
        }
        // on storage for good before it is renamed into place, so a power cut can't leave it half there
        tokio::fs::File::open(&part).await?.sync_all().await?;
        tokio::fs::rename(&part, &file).await?;
        let _ = tokio::fs::remove_file(meta_of(&file)).await;
        if let Ok(d) = tokio::fs::File::open(&car_dir).await {
            let _ = d.sync_all().await;
        }
        Ok(Received {
            received,
            length,
            complete: true,
            already: false,
        })
    }

    /// Drops unfinished uploads older than `max_age_ms` (an app that gave up, or someone filling the disk).
    pub async fn clean_stale(&self, max_age_ms: u64) -> usize {
        let mut n = 0;
        let Ok(mut cars) = tokio::fs::read_dir(&self.dir).await else {
            return 0;
        };
        while let Ok(Some(car)) = cars.next_entry().await {
            let Ok(mut rd) = tokio::fs::read_dir(car.path()).await else {
                continue;
            };
            while let Ok(Some(e)) = rd.next_entry().await {
                let p = e.path();
                if !p.to_string_lossy().ends_with(".part") {
                    continue;
                }
                let old = e
                    .metadata()
                    .await
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age.as_millis() as u64 > max_age_ms);
                if old {
                    let _ = tokio::fs::remove_file(&p).await;
                    let _ = tokio::fs::remove_file(PathBuf::from(format!("{}.json", p.display())))
                        .await;
                    n += 1;
                }
            }
        }
        n
    }
}

fn part_of(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

fn meta_of(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(".part.json");
    PathBuf::from(s)
}
