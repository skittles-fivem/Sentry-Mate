//! Where uploaded logs are kept: exactly as received, never replaced, and sealed with the vault (nothing is written
//! to storage in the clear, uploads in progress included).
//!
//! ```text
//! <can>/<vehicleKey>/<name>.log.xz                     a finished log (sealed)
//! <can>/<vehicleKey>/<name>.log.xz.json                its sidecar (sealed)
//! <can>/<vehicleKey>/packs/<YYYY-MM-DD>.pack           the finished logs and sidecars of one UTC day (the day in
//!                                                      their names), packed into one sealed file (see below)
//! <can>/<vehicleKey>/packs/<YYYY-MM-DD>.pack.tmp       a pack being written, renamed into place once it read back
//! <can>/<vehicleKey>/counts.json                       how many finished logs the car has (sealed)
//! <can>/<vehicleKey>/uploading/<name>.part             a file still arriving (sealed records, see vault::records)
//! <can>/<vehicleKey>/uploading/<name>.part.json        what that upload promised: length, SHA-256, when it started
//! <can>/<vehicleKey>/uploading/<name>.sealing          the finished file being sealed, renamed into place when done
//! <can>/<vehicleKey>/uploading/<name>.part.to-v2       a part from before the vault's format 2 being written again
//! <can>/<vehicleKey>/quarantine/<name>                 a log (or sidecar) that failed its check and was sent again:
//!                                                      the first bad copy, kept (sealed under its own name's label)
//!                                                      until the copy sent again verifies
//! ```
//!
//! A finished file is never replaced, with one exception: a log that failed its check (and its sidecar) may be sent
//! again under its name ([`Store::receive_replacing`], when the judge says so). The new copy arrives as any upload
//! does, and only once its last byte is here and its SHA-256 matches is the bad copy linked into `quarantine/` and the
//! new one renamed over it, so the name always holds one whole copy.
//!
//! Day packs. Thousands of small files each take a whole storage block and a seal of their own, and the sidecars are
//! nearly the same JSON over and over. So a day that is over is packed ([`Store::pack_finished`], [`Store::pack_day`]):
//! its logs go into one file byte for byte (never compressed again: they are the signed originals), its sidecars
//! compressed together (see [`crate::pack`] for the format), and every file read from a pack is exactly what was
//! uploaded. Nothing is lost on the way: the pack is written under a temporary name and synced, read back from
//! storage and compared with every file it was made from, renamed into place, and only then are the files it holds
//! removed. A crash at any point leaves each file in the folder, in the pack, or in both; the next pass finishes.
//! A file in the car's folder always wins over a pack's entry of its name: a log sent again after it failed its check
//! arrives beside the pack (the bad copy is taken out of the pack into `quarantine/`), and the next pass puts the new
//! copy into the pack in place of the bad one. Reading, hashing, asking whether a name is here and its upload state
//! all look in the folder first and then in the pack, so a packed file answers exactly as it did before. A pack that
//! doesn't open (damaged, changed) is an error, never an empty day, and nothing is removed because of it. Packing
//! doesn't change a car's log count. A data folder with no packs works as it is.
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
use crate::pack::{self, Pack};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
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
/// A car's folder of bad copies that were sent again.
pub const QUARANTINE: &str = "quarantine";
/// A car's folder of day packs.
pub const PACKS: &str = "packs";
/// The most a day pack holds (bytes as uploaded): a pack is opened whole. Files of a day beyond it stay as they are.
pub const MAX_PACK_BYTES: u64 = 32 << 20;
/// A file larger than this is not packed (one storage block is nothing to it, and a pack is opened whole).
pub const MAX_PACKED_FILE: u64 = 8 << 20;
/// Opened packs kept in memory (reading a day's logs in a row opens its pack once).
const CACHED_PACKS: usize = 2;
/// Packs whose list of names, lengths and SHA-256 is kept in memory (a few KB each).
const CACHED_PACK_INDEXES: usize = 64;
const COUNTS: &str = "counts.json";
const DAY_MS: i64 = 86_400_000;

/// What a packing pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct PackReport {
    /// Days whose pack was written or completed.
    pub days: usize,
    /// Files (logs and sidecars) now only in their day's pack.
    pub files: usize,
    /// Bytes stored before: those files, and the packs of their days as they were.
    pub before_bytes: u64,
    /// Bytes of those days' packs now.
    pub after_bytes: u64,
    /// Days that could not be packed (a damaged pack, an error): left exactly as they were.
    pub failed: usize,
    /// `stop` said so before every day was looked at.
    pub stopped: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("a day pack doesn't open: {0}")]
    Damaged(String),
    #[error("the new day pack did not read back as the files it was made from ({0}); nothing was removed")]
    Verify(String),
    #[error("that is not a car key and a day (YYYY-MM-DD)")]
    BadName,
    #[error("stopped where a test asked")]
    TestStop,
}

/// For tests of what a crash leaves: where a packing pass stops (as if the server died there).
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackStop {
    /// The temporary pack is written and synced; nothing renamed.
    TempWritten,
    /// The pack is in place; no file removed yet.
    Renamed,
    /// This many packed files are removed, the rest not.
    Removed(usize),
}

/// What tells one version of a pack file from the next (a pack is replaced by renaming a new file over it).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    ino: u64,
    len: u64,
    mtime: (i64, i64),
}

fn stamp(m: &std::fs::Metadata) -> Stamp {
    use std::os::unix::fs::MetadataExt;
    Stamp { ino: m.ino(), len: m.len(), mtime: (m.mtime(), m.mtime_nsec()) }
}

/// Where one day's pack is.
struct PackFile {
    file: PathBuf,
    tmp: PathBuf,
    label: String,
    /// `<key>/<day>`: its name in the caches.
    id: String,
}

/// The UTC day of a log's start, as packs are named.
fn day_of(started_ms: i64) -> String {
    let (y, m, d) = trust::name::civil_from_days(started_ms.div_euclid(DAY_MS));
    format!("{y:04}-{m:02}-{d:02}")
}

fn valid_day(day: &str) -> bool {
    let b = day.as_bytes();
    b.len() == 10
        && day.is_ascii()
        && b[4] == b'-'
        && b[7] == b'-'
        && trust::name::parse_stamp(&format!("{}{}{}T000000Z", &day[0..4], &day[5..7], &day[8..10]))
            .is_some_and(|ms| day_of(ms) == day)
}

fn remove_if_there(file: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

fn pack_vault_err(e: vault::Error) -> PackError {
    if let vault::Error::Io(e) = e { PackError::Io(e) } else { PackError::Damaged(e.to_string()) }
}

fn pack_err(e: PackError) -> ApiError {
    ApiError::server(e)
}

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
    /// It took the place of a copy that failed its check (now in `quarantine/`).
    #[serde(skip)]
    pub replaced: bool,
    /// A sidecar that arrived with no log of its name stored: nothing bounds how many of those a car keeps (the logs
    /// per car don't count them), so its request counts against the token's rate.
    #[serde(skip)]
    pub orphan: bool,
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
    /// The packs opened last, by `<key>/<day>`, each good for the file version it was read from.
    packs: Mutex<Lru<(Stamp, Arc<Pack>)>>,
    pack_indexes: Mutex<Lru<(Stamp, pack::Index)>>,
    /// How many times a pack file was read and unsealed (tests check a day read in a row opens its pack once).
    pack_opens: AtomicU64,
    pack_stop: Mutex<Option<PackStop>>,
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

    /// SHA-256 and length of a finished file's plaintext: the file in the car's folder, else its day pack's entry
    /// (whose SHA-256 was compared with the file when it was packed, and is sealed with the pack).
    fn hash_finished(&self, name: &str) -> Result<(String, u64), ApiError> {
        let (file, label) = self.file_of(name)?;
        let f = match std::fs::File::open(&file) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return match self.packed(name).map_err(pack_err)? {
                    Some(entry) => Ok((hex::encode(entry.sha), entry.len)),
                    None => Err(e.into()),
                };
            }
            Err(e) => return Err(e.into()),
        };
        self.hash_complete(f, &label)
    }

    /// A finished file's plaintext, checked as it is unsealed: the file in the car's folder, else its day pack's
    /// entry. `Ok(None)`: not here.
    fn read_finished(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, ApiError> {
        let (file, label) = self.file_of(name)?;
        if let Some(b) = self.vault.read_file(&file, &label).map_err(vault_err)? {
            return Ok(Some(b));
        }
        self.read_packed(name).map_err(pack_err)
    }

    /// Whether a finished file of this name is here, in the car's folder or in its day pack. A pack that doesn't
    /// open is logged and answers no (reading or uploading the name says the error).
    fn finished(&self, name: &str) -> bool {
        let Ok((file, _)) = self.file_of(name) else { return false };
        if file.exists() {
            return true;
        }
        match self.packed(name) {
            Ok(e) => e.is_some(),
            Err(e) => {
                tracing::error!("CAN logs: {e}");
                false
            }
        }
    }

    fn pack_file(&self, key: &str, day: &str) -> PackFile {
        let dir = self.dir.join(key).join(PACKS);
        PackFile {
            file: dir.join(format!("{day}.pack")),
            tmp: dir.join(format!("{day}.pack.tmp")),
            label: format!("can/{key}/{PACKS}/{day}.pack"),
            id: format!("{key}/{day}"),
        }
    }

    /// Reads a pack file from storage (`file`: the pack, or the temporary file of a new one; sealed under the
    /// pack's label either way). `Ok(None)`: no such file.
    fn open_pack(&self, file: &Path, label: &str) -> Result<Option<(Stamp, Pack)>, PackError> {
        let mut f = match std::fs::File::open(file) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        // the stamp of the very file read, should a new pack be renamed over this one meanwhile
        let st = stamp(&f.metadata()?);
        let mut sealed = Vec::with_capacity(st.len as usize);
        f.read_to_end(&mut sealed)?;
        self.pack_opens.fetch_add(1, Ordering::Relaxed);
        let plain = self.vault.open_sealed(label, &sealed).map_err(pack_vault_err)?;
        Ok(Some((st, Pack::parse(plain).map_err(PackError::Damaged)?)))
    }

    /// The pack file's version now. `Ok(None)`: there is no pack.
    fn pack_stamp(pf: &PackFile) -> Result<Option<Stamp>, PackError> {
        match std::fs::metadata(&pf.file) {
            Ok(m) => Ok(Some(stamp(&m))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// A day's pack, opened (from memory when it is one of the last opened and its file hasn't changed).
    fn pack_body(&self, pf: &PackFile) -> Result<Option<Arc<Pack>>, PackError> {
        let Some(now) = Inner::pack_stamp(pf)? else { return Ok(None) };
        if let Some((st, p)) = lock(&self.packs).get(&pf.id)
            && *st == now
        {
            return Ok(Some(p.clone()));
        }
        let Some((st, p)) = self.open_pack(&pf.file, &pf.label)? else { return Ok(None) };
        let p = Arc::new(p);
        lock(&self.pack_indexes).put(pf.id.clone(), (st, p.index.clone()));
        lock(&self.packs).put(pf.id.clone(), (st, p.clone()));
        Ok(Some(p))
    }

    /// A day's pack's entries (names, lengths, SHA-256), without its bytes when they are known already.
    fn pack_index(&self, pf: &PackFile) -> Result<Option<pack::Index>, PackError> {
        let Some(now) = Inner::pack_stamp(pf)? else { return Ok(None) };
        if let Some((st, ix)) = lock(&self.pack_indexes).get(&pf.id)
            && *st == now
        {
            return Ok(Some(ix.clone()));
        }
        Ok(self.pack_body(pf)?.map(|p| p.index.clone()))
    }

    fn forget_pack(&self, pf: &PackFile) {
        lock(&self.packs).remove(&pf.id);
        lock(&self.pack_indexes).remove(&pf.id);
    }

    /// The day pack a name would be in.
    fn pack_file_of(&self, name: &str) -> Option<PackFile> {
        let n = LogName::parse(name)?;
        Some(self.pack_file(n.vehicle_key, &day_of(n.started_ms)))
    }

    /// A name's entry in its day pack. `Ok(None)`: not packed.
    fn packed(&self, name: &str) -> Result<Option<pack::Entry>, PackError> {
        let Some(pf) = self.pack_file_of(name) else { return Ok(None) };
        Ok(self.pack_index(&pf)?.and_then(|ix| pack::find(&ix, name).cloned()))
    }

    /// A name's bytes from its day pack, exactly as uploaded (checked against their SHA-256).
    fn read_packed(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, PackError> {
        let Some(pf) = self.pack_file_of(name) else { return Ok(None) };
        match self.pack_body(&pf)? {
            Some(p) => p.read(name).map_err(PackError::Damaged),
            None => Ok(None),
        }
    }

    fn stop_at(&self, point: PackStop) -> Result<(), PackError> {
        if *lock(&self.pack_stop) == Some(point) { Err(PackError::TestStop) } else { Ok(()) }
    }

    /// The finished logs and sidecars in a car's folder (not its packs), by the UTC day in their names; with
    /// `before_ms`, only days that ended by then.
    fn loose_days(&self, key: &str, before_ms: Option<u64>) -> BTreeMap<String, Vec<String>> {
        let mut days: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for e in std::fs::read_dir(self.dir.join(key)).into_iter().flatten().flatten() {
            let Ok(name) = e.file_name().into_string() else { continue };
            let Some(n) = LogName::parse(&name).filter(|n| n.vehicle_key == key) else { continue };
            let day_end = (n.started_ms.div_euclid(DAY_MS) + 1).saturating_mul(DAY_MS);
            if before_ms.is_some_and(|b| i64::try_from(b).is_ok_and(|b| day_end > b)) {
                continue;
            }
            if e.file_type().is_ok_and(|t| t.is_file()) {
                days.entry(day_of(n.started_ms)).or_default().push(name);
            }
        }
        days
    }

    /// The days a car has packs of, with each pack file's bytes.
    fn pack_sizes(&self, key: &str) -> Vec<(String, u64)> {
        let mut v = Vec::new();
        for e in std::fs::read_dir(self.dir.join(key).join(PACKS)).into_iter().flatten().flatten() {
            let Ok(name) = e.file_name().into_string() else { continue };
            let Some(day) = name.strip_suffix(".pack").filter(|d| valid_day(d)) else { continue };
            if let Ok(m) = e.metadata()
                && m.is_file()
            {
                v.push((day.to_string(), m.len()));
            }
        }
        v.sort();
        v
    }

    /// Checks a pack file as it is in storage against what it must hold: `entries` (the index it was built with),
    /// every entry's bytes against its SHA-256, and each of `loose` (files in the car's folder, read again now)
    /// byte for byte.
    fn verify_pack(
        &self,
        file: &Path,
        pf: &PackFile,
        key: &str,
        loose: &[(String, u64)],
        entries: &[pack::Entry],
    ) -> Result<(), PackError> {
        let wrong = |what: &str| PackError::Verify(what.to_string());
        let p = match self.open_pack(file, &pf.label) {
            Ok(Some((_, p))) => p,
            Ok(None) => return Err(wrong("it is not there")),
            Err(PackError::Io(e)) => return Err(PackError::Io(e)),
            Err(e) => return Err(PackError::Verify(e.to_string())),
        };
        if p.index.len() != entries.len() || !p.index.iter().zip(entries).all(|(a, b)| a.same_file(b)) {
            return Err(wrong("its list of files differs"));
        }
        if !p.index.iter().all(|e| p.intact(e)) {
            return Err(wrong("an entry differs from its SHA-256"));
        }
        let car_dir = self.dir.join(key);
        for (name, _) in loose {
            let b = self
                .vault
                .read_file(&car_dir.join(name), &format!("can/{key}/{name}"))
                .map_err(pack_vault_err)?
                .ok_or_else(|| wrong("a file it was made from is gone"))?;
            let same = pack::find(&p.index, name).is_some_and(|e| p.bytes(e) == b.as_slice());
            if !same {
                return Err(wrong("an entry differs from its file"));
            }
        }
        Ok(())
    }

    /// Packs `names` (finished logs and sidecars of `day` in the car's folder) into the day's pack, with what the
    /// pack holds already; a file in the folder takes the place of the pack's entry of its name. See the module
    /// docs for the order that keeps every file readable whatever happens.
    fn pack_day(&self, key: &str, day: &str, mut names: Vec<String>) -> Result<PackReport, PackError> {
        let mut report = PackReport::default();
        let pf = self.pack_file(key, day);
        let car_dir = self.dir.join(key);
        let car_lock = self.locks.get(key);
        let _car = lock(&car_lock);
        // what a crash left while a pack was being written (the lock is held: nobody is writing one now)
        remove_if_there(&pf.tmp)?;
        names.sort();
        names.dedup();
        // a pack that doesn't open stops here: nothing is written over it and nothing is removed
        let old_index = self.pack_index(&pf)?;
        let old_len =
            |name: &str| old_index.as_deref().and_then(|ix| pack::find(ix, name)).map_or(0, |e: &pack::Entry| e.len);
        let mut total: u64 = old_index.iter().flat_map(|ix| ix.iter()).map(|e| e.len).sum();
        let mut chosen = Vec::new();
        for name in names {
            let Ok(m) = std::fs::symlink_metadata(car_dir.join(&name)) else { continue };
            let Some(len) = vault::plaintext_len(m.len()).filter(|_| m.is_file()) else { continue };
            let after = total.saturating_sub(old_len(&name)).saturating_add(len);
            if len > MAX_PACKED_FILE || after > MAX_PACK_BYTES {
                continue;
            }
            total = after;
            chosen.push((name, m.len()));
        }
        // each file's bytes as uploaded; one that doesn't open stays where it is
        let mut files = Vec::new();
        let mut loose: Vec<(String, u64)> = Vec::new();
        for (name, sealed_len) in chosen {
            match self.vault.read_file(&car_dir.join(&name), &format!("can/{key}/{name}")) {
                Ok(Some(b)) => {
                    files.push((name.clone(), b));
                    loose.push((name, sealed_len));
                }
                Ok(None) => {}
                // not the name (it holds the car's key): the server's log is kept in the clear
                Err(e) => tracing::warn!("CAN logs: a stored file doesn't open and is left out of its day pack: {e}"),
            }
        }
        if loose.is_empty() {
            return Ok(report);
        }
        let old = self.pack_body(&pf)?;
        if let Some(old) = &old {
            for e in old.index.iter() {
                if loose.binary_search_by(|(n, _)| n.as_str().cmp(&e.name)).is_err() {
                    let b = old.read(&e.name).map_err(PackError::Damaged)?;
                    files.push((e.name.clone(), b.ok_or_else(|| PackError::Damaged("an entry is gone".into()))?));
                }
            }
        }
        let old_bytes = if old.is_some() { std::fs::metadata(&pf.file).map_or(0, |m| m.len()) } else { 0 };
        let (plain, entries) = pack::build(files)?;
        // the pack holds exactly these files already (a pass that stopped before it removed them): no new pack
        let unchanged = old.as_ref().is_some_and(|o| {
            o.index.len() == entries.len() && o.index.iter().zip(&entries).all(|(a, b)| a.same_file(b))
        });
        drop(old);
        if unchanged {
            drop(plain);
            self.verify_pack(&pf.file, &pf, key, &loose, &entries)?;
        } else {
            let sealed = self.vault.seal(&pf.label, &plain).map_err(pack_vault_err)?;
            drop(plain);
            if let Some(d) = pf.tmp.parent() {
                std::fs::create_dir_all(d)?;
            }
            {
                use std::os::unix::fs::OpenOptionsExt;
                let mut f =
                    std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&pf.tmp)?;
                f.write_all(&sealed)?;
                f.sync_all()?;
            }
            drop(sealed);
            self.stop_at(PackStop::TempWritten)?;
            // read back from storage before it takes the pack's name: a pack that is there is always a checked one,
            // and the pack before it (if any) stays until this one is
            if let Err(e) = self.verify_pack(&pf.tmp, &pf, key, &loose, &entries) {
                let _ = std::fs::remove_file(&pf.tmp);
                return Err(e);
            }
            std::fs::rename(&pf.tmp, &pf.file)?;
            if let Some(d) = pf.file.parent() {
                sync_dir(d);
            }
            self.forget_pack(&pf);
            self.stop_at(PackStop::Renamed)?;
        }
        for (name, sealed_len) in &loose {
            self.stop_at(PackStop::Removed(report.files))?;
            remove_if_there(&car_dir.join(name))?;
            report.files += 1;
            report.before_bytes += sealed_len;
        }
        sync_dir(&car_dir);
        report.days = 1;
        report.before_bytes += old_bytes;
        report.after_bytes = std::fs::metadata(&pf.file).map_or(0, |m| m.len());
        Ok(report)
    }

    fn pack_finished(&self, before_ms: u64, stop: &dyn Fn() -> bool) -> PackReport {
        let mut report = PackReport::default();
        let mut keys: Vec<String> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|k| crate::ledger::valid_key(k))
            .collect();
        keys.sort();
        for key in keys {
            for (day, names) in self.loose_days(&key, Some(before_ms)) {
                if stop() {
                    report.stopped = true;
                    return report;
                }
                match self.pack_day(&key, &day, names) {
                    Ok(r) => {
                        report.days += r.days;
                        report.files += r.files;
                        report.before_bytes += r.before_bytes;
                        report.after_bytes += r.after_bytes;
                    }
                    Err(e) => {
                        report.failed += 1;
                        // not the key: the server's log is kept in the clear
                        tracing::warn!("CAN logs: the logs of {day} of a car were not packed: {e}");
                    }
                }
            }
        }
        report
    }

    /// A car's finished logs counted from storage: those in its folder, and those only in its day packs.
    fn count_logs(&self, key: &str, car_dir: &Path) -> Result<u64, ApiError> {
        let mut n = std::fs::read_dir(car_dir)?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".log.xz"))
            .count() as u64;
        for (day, _) in self.pack_sizes(key) {
            match self.pack_index(&self.pack_file(key, &day)) {
                Ok(Some(ix)) => {
                    n += ix.iter().filter(|e| !e.sidecar && !car_dir.join(&e.name).exists()).count() as u64;
                }
                Ok(None) => {}
                // the count is a safety net: a little low is better than a car that can't upload
                Err(e) => tracing::warn!("CAN upload: a day pack was not counted: {e}"),
            }
        }
        Ok(n)
    }

    /// SHA-256 and length of a sealed file's plaintext.
    fn hash_complete(&self, f: std::fs::File, label: &str) -> Result<(String, u64), ApiError> {
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
        if let Some(e) = self.packed(name).map_err(pack_err)? {
            return Ok(FileState { state: State::Complete, received: e.len, length: Some(e.len) });
        }
        Ok(self.part_state(&p))
    }

    /// What has arrived of an upload under way (a new file's, or a replacement's): partial, or missing.
    fn part_state(&self, p: &Paths) -> FileState {
        if p.part.exists() {
            // read only: an upload being written at this moment is neither cut nor changed
            let received = self.vault.records_read(&p.part, &p.part_label).map(|r| r.plain_len()).unwrap_or(0);
            return FileState { state: State::Partial, received, length: self.meta(p).map(|m| m.length) };
        }
        FileState { state: State::Missing, received: 0, length: None }
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
                let n = self.count_logs(key, car_dir)?;
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
        replace: bool,
    ) -> Result<Received, ApiError> {
        let n = LogName::parse(name).ok_or_else(bad_name)?;
        if length == 0 {
            return Err(ApiError::new(400, "bad_length", "Upload-Length must be the whole file's size in bytes."));
        }
        if sha256.len() != 64 || !sha256.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(ApiError::new(400, "bad_sha256", "Upload-SHA256 must be the whole file's SHA-256 in hex."));
        }
        // an empty chunk adds nothing (and would start the upload over at offset 0 every time, at the cost of synced
        // disk writes): refused
        if body.is_empty() {
            return Err(ApiError::new(400, "empty_chunk", "A chunk must hold at least one byte."));
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

        let mut had = self.state_of(name)?;
        let mut replacing = false;
        if had.state == State::Complete {
            // a replacement under way (its part promised this file) goes on without hashing the stored copy again
            let under_way =
                replace && offset > 0 && self.meta(&p).is_some_and(|m| m.length == length && m.sha256 == sha256);
            if !under_way {
                if had.length == Some(length) && self.hash_finished(name)?.0 == sha256 {
                    return Ok(Received {
                        received: length,
                        length,
                        complete: true,
                        already: true,
                        replaced: false,
                        orphan: false,
                    });
                }
                if !replace {
                    return Err(ApiError::new(
                        409,
                        "exists_different",
                        "A different file with this name is already here.",
                    ));
                }
            }
            replacing = true;
            had = self.part_state(&p);
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
            if !n.sidecar && !replacing && logs >= self.max_logs_per_car as u64 {
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
        let mut records = match self.vault.records(&p.part, &p.part_label) {
            Ok(r) => r,
            // a part from before the vault's format 2 with a damaged record: it can't be written again in format 2
            // (that checks every record), so it is dropped and the upload starts over, as a damaged part found at
            // the last chunk is
            Err(vault::Error::Damaged) => {
                let _ = std::fs::remove_file(&p.part);
                let _ = std::fs::remove_file(&p.meta);
                return Err(ApiError::new(
                    409,
                    "offset_mismatch",
                    "What had arrived of this file was damaged here and was discarded; upload it again from offset 0.",
                )
                .received(0));
            }
            Err(e) => return Err(vault_err(e)),
        };
        records.append(body)?;
        let received = records.plain_len();
        if received < length {
            return Ok(Received { received, length, complete: false, already: false, replaced: false, orphan: false });
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
        let before = if n.sidecar || replacing { None } else { Some(self.logs_of(&p.key, &p.car_dir, false)?) };
        if replacing {
            // the bad copy aside first (a link: the name holds the old copy until the new one is renamed over it); the
            // first bad copy stays there, a later one is dropped
            let q_dir = p.car_dir.join(QUARANTINE);
            std::fs::create_dir_all(&q_dir)?;
            let q = q_dir.join(name);
            if q.exists() {
                tracing::warn!("CAN upload: a log sent again replaced a bad copy; the first bad copy is kept");
            } else {
                let kept = match std::fs::hard_link(&p.file, &q) {
                    // the bad copy is only in its day pack: taken out of it (the new copy beside the pack wins from
                    // now on, and the next packing pass puts it in the pack in place of the bad one)
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => match self.read_packed(name) {
                        Ok(Some(bad)) => self.vault.write_file(&q, &p.label, &bad).map_err(vault_err),
                        Ok(None) => Ok(()),
                        Err(e) => Err(pack_err(e)),
                    },
                    r => r.map_err(ApiError::server),
                };
                if let Err(e) = kept {
                    discard(Some(tmp));
                    return Err(e);
                }
            }
            if let Ok(d) = std::fs::File::open(&q_dir) {
                let _ = d.sync_all();
            }
        }
        std::fs::rename(tmp, &p.file)?;
        discard(None);
        if let Ok(d) = std::fs::File::open(&p.car_dir) {
            let _ = d.sync_all();
        }
        if let Some(before) = before {
            lock(&self.logs).put(p.key.clone(), before + 1);
            // the log is kept either way; a count not written down is only a little low after a restart
            if let Err(e) = self.save_logs(&p.key, before + 1) {
                // not the key: the server's log is kept in the clear
                tracing::warn!("CAN upload: could not write a car's log count: {e}");
            }
        }
        let orphan = n.sidecar
            && match name.strip_suffix(".json") {
                Some(log) => !self.finished(log),
                None => true,
            };
        Ok(Received { received, length, complete: true, already: false, replaced: replacing, orphan })
    }

    /// Drops one car's unfinished uploads older than `max_age_ms`, and leftovers of a crash while sealing or while
    /// the vault wrote a part again in its newer format (`<name>.part.to-v2`, see `vault::records`).
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
            let converting = || f.strip_suffix(vault::records::CONVERTING).and_then(|n| n.strip_suffix(".part"));
            let Some(name) = f.strip_suffix(".part").or_else(|| f.strip_suffix(".sealing")).or_else(converting) else {
                // a promise whose upload is gone
                if let Some(name) = f.strip_suffix(".part.json")
                    && !up_dir.join(format!("{name}.part")).exists()
                    && old(&e)
                {
                    let _ = std::fs::remove_file(e.path());
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
            packs: Mutex::new(Lru::new(CACHED_PACKS)),
            pack_indexes: Mutex::new(Lru::new(CACHED_PACK_INDEXES)),
            pack_opens: AtomicU64::new(0),
            pack_stop: Mutex::new(None),
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
            packs: Mutex::new(Lru::new(CACHED_PACKS)),
            pack_indexes: Mutex::new(Lru::new(CACHED_PACK_INDEXES)),
            pack_opens: AtomicU64::new(0),
            pack_stop: Mutex::new(None),
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
        self.receive_replacing(name, body, offset, length, sha256, false).await
    }

    /// [`Store::receive`], and with `replace` a file of this name that is here already is replaced by the new one
    /// (only for a log that failed its check, or its sidecar: the caller asks the judge). The same bytes again are
    /// still `already`; the bad copy goes to `quarantine/` once the new one has arrived whole.
    pub async fn receive_replacing(
        &self,
        name: &str,
        body: axum::body::Bytes,
        offset: u64,
        length: u64,
        sha256: &str,
        replace: bool,
    ) -> Result<Received, ApiError> {
        let (inner, name, sha) = (self.0.clone(), name.to_string(), sha256.to_string());
        blocking(move || inner.receive(&name, &body, offset, length, &sha, replace)).await
    }

    /// The logs (and sidecars) of a car key still arriving: the names of its `uploading/` parts.
    pub async fn uploading(&self, key: &str) -> HashSet<String> {
        if !crate::ledger::valid_key(key) {
            return HashSet::new();
        }
        let dir = self.0.dir.join(key).join(UPLOADING);
        blocking(move || {
            Ok(std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| e.file_name().to_str().and_then(|f| f.strip_suffix(".part")).map(str::to_string))
                .collect())
        })
        .await
        .unwrap_or_default()
    }

    /// The bad copies of a car key kept in `quarantine/` (log and sidecar names), in name order.
    pub async fn quarantined(&self, key: &str) -> Vec<String> {
        if !crate::ledger::valid_key(key) {
            return Vec::new();
        }
        let dir = self.0.dir.join(key).join(QUARANTINE);
        blocking(move || {
            let mut v: Vec<String> = std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| e.file_name().to_str().filter(|f| LogName::parse(f).is_some()).map(str::to_string))
                .collect();
            v.sort();
            Ok(v)
        })
        .await
        .unwrap_or_default()
    }

    /// A bad copy's plaintext, checked as it is unsealed (it keeps the label of its name). `Ok(None)`: none kept.
    pub async fn read_quarantined(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, ApiError> {
        let inner = self.0.clone();
        let (file, label) = self.0.file_of(name)?;
        let q = file.parent().map(|car| car.join(QUARANTINE).join(name)).ok_or_else(bad_name)?;
        blocking(move || inner.vault.read_file(&q, &label).map_err(vault_err)).await
    }

    /// Drops the bad copies of a log and its sidecar (its copy sent again verified). Returns how many were there.
    pub async fn drop_quarantined(&self, log_name: &str) -> usize {
        let Ok((file, _)) = self.0.file_of(log_name) else { return 0 };
        let Some(q_dir) = file.parent().map(|car| car.join(QUARANTINE)) else { return 0 };
        let names = [log_name.to_string(), format!("{log_name}.json")];
        blocking(move || Ok(names.iter().filter(|n| std::fs::remove_file(q_dir.join(n)).is_ok()).count()))
            .await
            .unwrap_or(0)
    }

    /// A finished file's plaintext (a log or sidecar), checked as it is unsealed. `Ok(None)`: not here.
    pub async fn read(&self, name: &str) -> Result<Option<Zeroizing<Vec<u8>>>, ApiError> {
        self.0.file_of(name)?;
        let (inner, name) = (self.0.clone(), name.to_string());
        blocking(move || inner.read_finished(&name)).await
    }

    /// SHA-256 and length of a finished file's plaintext.
    pub async fn hash(&self, name: &str) -> Result<(String, u64), ApiError> {
        self.0.file_of(name)?;
        let (inner, name) = (self.0.clone(), name.to_string());
        blocking(move || inner.hash_finished(&name)).await
    }

    /// Packs one UTC day (`YYYY-MM-DD`) of a car key: its finished logs and sidecars in the car's folder go into the
    /// day's pack (with what the pack holds already) and are removed from the folder once the pack has read back as
    /// exactly those files. Not for a blocking-sensitive thread: it reads and writes the whole day.
    pub fn pack_day(&self, key: &str, day: &str) -> Result<PackReport, PackError> {
        if !crate::ledger::valid_key(key) || !valid_day(day) {
            return Err(PackError::BadName);
        }
        let names = self.0.loose_days(key, None).remove(day).unwrap_or_default();
        self.0.pack_day(key, day, names)
    }

    /// Packs every UTC day that ended by `before_ms` and still has finished logs or sidecars in a car's folder, car
    /// by car and day by day; `stop` is asked before each day. A day that fails is left as it was and counted in
    /// `failed`. Call it from a blocking thread.
    pub fn pack_finished(&self, before_ms: u64, stop: &dyn Fn() -> bool) -> PackReport {
        self.0.pack_finished(before_ms, stop)
    }

    /// The UTC days (`YYYY-MM-DD`) a car key has packs of, with each pack file's bytes as stored, in day order.
    pub fn pack_sizes(&self, key: &str) -> Vec<(String, u64)> {
        if !crate::ledger::valid_key(key) {
            return Vec::new();
        }
        self.0.pack_sizes(key)
    }

    /// How many times a pack file was read and unsealed.
    pub fn pack_opens(&self) -> u64 {
        self.0.pack_opens.load(Ordering::Relaxed)
    }

    /// For tests: the next packing passes stop at `point` with [`PackError::TestStop`], as a crash there would.
    #[doc(hidden)]
    pub fn pack_stop_at(&self, point: Option<PackStop>) {
        *lock(&self.0.pack_stop) = point;
    }

    /// Whether an upload of this name is under way (a part is here).
    pub fn part_exists(&self, name: &str) -> bool {
        self.0.paths(name).is_ok_and(|p| p.part.exists())
    }

    pub fn exists(&self, name: &str) -> bool {
        self.0.finished(name)
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
