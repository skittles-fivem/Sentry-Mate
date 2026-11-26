//! Day snapshots of the time series as read last, opened and unpacked, kept in memory: a page that reads the same days again (or several pages that read the same
//! days) skips opening and unpacking them. Each is kept with what its file was (inode, size and change time): a day
//! written again is a new file (written whole and renamed into place), so it is read again. A day inside a month's
//! pack is kept under `<pack>#<day>` with what the day is there ([`Stamp::packed`]). The plaintext is held as
//! the open cars' tables are (in this process's memory only, wiped when let go).
//!
//! How much: a quarter of the memory free for it (the machine's available memory, or less under a container's
//! limit, counting what the cache holds), at most [`crate::Options::max_cached_bytes`]; looked at again every few
//! seconds, so the cache gives memory back when the machine runs short. Without a way to tell (no `/proc`), 256 MB.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

/// What a file was when it was read: a file written since is another file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Stamp {
    ino: u64,
    len: u64,
    mtime_ns: i128,
}

impl Stamp {
    /// When the file was last written, in nanoseconds since 1970.
    pub(crate) fn mtime_ns(&self) -> i128 {
        self.mtime_ns
    }

    /// The stamp of a day kept inside a month's pack ([`crate::segment::pack`]): what the day is there (its
    /// generation, its body's length and content hash), not what the pack file is, so it stays the same when the
    /// month is packed again around it. Never equal to a file's stamp.
    pub(crate) fn packed(generation: u64, len: u64, hash: u64) -> Stamp {
        Stamp { ino: hash, len, mtime_ns: -1 - i128::from(generation) }
    }
}

pub(crate) fn stamp(file: &Path) -> std::io::Result<Option<Stamp>> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(file) {
        Ok(m) => Ok(Some(Stamp {
            ino: m.ino(),
            len: m.len(),
            mtime_ns: i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()),
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// A snapshot's generation and its unpacked body.
pub(crate) type Plain = Arc<(u64, Zeroizing<Vec<u8>>)>;

struct Entry {
    stamp: Stamp,
    used: u64,
    plain: Plain,
}

#[derive(Default)]
struct Map {
    files: HashMap<PathBuf, Entry>,
    bytes: u64,
    tick: u64,
    /// What the cache may hold now, and when that was worked out.
    max: u64,
    looked: Option<std::time::Instant>,
}

impl Map {
    /// The size allowed now (worked out again every [`LOOK_EVERY`]); lets the least recently used go down to it.
    fn fit(&mut self, cap: u64) {
        if self.looked.is_none_or(|t| t.elapsed() >= LOOK_EVERY) {
            self.looked = Some(std::time::Instant::now());
            self.max = match free_memory() {
                Some(free) => (free.saturating_add(self.bytes) / 4).min(cap),
                None => UNKNOWN_FREE.min(cap),
            };
        }
        while self.bytes > self.max {
            let Some(oldest) = self.files.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone()) else { break };
            if let Some(e) = self.files.remove(&oldest) {
                self.bytes -= e.plain.1.len() as u64;
            }
        }
    }
}

pub(crate) struct PlainCache {
    /// The most it may hold, whatever memory there is.
    cap: u64,
    map: Mutex<Map>,
}

/// Bytes the cache may hold without a way to tell the memory free.
const UNKNOWN_FREE: u64 = 256 << 20;
/// How often the memory free is looked at again.
const LOOK_EVERY: std::time::Duration = std::time::Duration::from_secs(3);

/// Memory free for the process now: the machine's available memory, less if a container's limit (cgroup v2) leaves
/// less.
fn free_memory() -> Option<u64> {
    let kb = |text: &str, key: &str| -> Option<u64> {
        let line = text.lines().find(|l| l.starts_with(key))?;
        line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|k| k * 1024)
    };
    let machine = std::fs::read_to_string("/proc/meminfo").ok().and_then(|t| kb(&t, "MemAvailable:"));
    let read = |f: &str| std::fs::read_to_string(f).ok().and_then(|t| t.trim().parse::<u64>().ok());
    let container = match (read("/sys/fs/cgroup/memory.max"), read("/sys/fs/cgroup/memory.current")) {
        (Some(max), Some(used)) => Some(max.saturating_sub(used)),
        _ => None,
    };
    match (machine, container) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

impl PlainCache {
    pub(crate) fn new(cap: u64) -> PlainCache {
        PlainCache { cap, map: Mutex::new(Map::default()) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Map> {
        self.map.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn get(&self, file: &Path, stamp: Stamp) -> Option<Plain> {
        let mut m = self.lock();
        m.fit(self.cap);
        m.tick += 1;
        let tick = m.tick;
        let e = m.files.get_mut(file)?;
        if e.stamp != stamp {
            return None;
        }
        e.used = tick;
        Some(e.plain.clone())
    }

    /// Forgets every day kept of the files under `dir` (a car's folder swapped for another: nothing read from the
    /// old one is given again, even a file whose stamp the new one happens to share).
    pub(crate) fn forget_under(&self, dir: &Path) {
        let mut m = self.lock();
        let gone: Vec<PathBuf> = m.files.keys().filter(|p| p.starts_with(dir)).cloned().collect();
        for p in gone {
            if let Some(e) = m.files.remove(&p) {
                m.bytes -= e.plain.1.len() as u64;
            }
        }
    }

    pub(crate) fn put(&self, file: &Path, stamp: Stamp, plain: Plain) {
        let size = plain.1.len() as u64;
        let mut m = self.lock();
        m.fit(self.cap);
        if size > m.max / 4 {
            return;
        }
        m.tick += 1;
        let tick = m.tick;
        if let Some(old) = m.files.insert(file.to_path_buf(), Entry { stamp, used: tick, plain }) {
            m.bytes -= old.plain.1.len() as u64;
        }
        m.bytes += size;
        m.fit(self.cap);
    }
}
