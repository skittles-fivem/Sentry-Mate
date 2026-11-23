//! Day snapshots of the time series as read last, opened and unpacked, kept in memory up to a size
//! ([`crate::Options::max_cached_bytes`]): a page that reads the same days again (or several pages that read the same
//! days) skips opening and unpacking them. Each is kept with what its file was (inode, size and change time): a day
//! written again is a new file (written whole and renamed into place), so it is read again. The plaintext is held as
//! the open cars' tables are (in this process's memory only, wiped when let go).

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
}

pub(crate) struct PlainCache {
    max: u64,
    map: Mutex<Map>,
}

impl PlainCache {
    pub(crate) fn new(max: u64) -> PlainCache {
        PlainCache { max, map: Mutex::new(Map::default()) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Map> {
        self.map.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn get(&self, file: &Path, stamp: Stamp) -> Option<Plain> {
        let mut m = self.lock();
        m.tick += 1;
        let tick = m.tick;
        let e = m.files.get_mut(file)?;
        if e.stamp != stamp {
            return None;
        }
        e.used = tick;
        Some(e.plain.clone())
    }

    pub(crate) fn put(&self, file: &Path, stamp: Stamp, plain: Plain) {
        let size = plain.1.len() as u64;
        if self.max == 0 || size > self.max / 4 {
            return;
        }
        let mut m = self.lock();
        m.tick += 1;
        let tick = m.tick;
        if let Some(old) = m.files.insert(file.to_path_buf(), Entry { stamp, used: tick, plain }) {
            m.bytes -= old.plain.1.len() as u64;
        }
        m.bytes += size;
        while m.bytes > self.max {
            let Some(oldest) = m.files.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone()) else { break };
            if let Some(e) = m.files.remove(&oldest) {
                m.bytes -= e.plain.1.len() as u64;
            }
        }
    }
}
