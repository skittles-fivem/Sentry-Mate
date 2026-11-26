//! Sealed files of the db: every one starts (inside its sealing) with `"SMDB" | kind | format version`, is written
//! whole under a temporary name that is sealed too, synced, then renamed into place, and is bound to its path under
//! the data folder (its vault label), so a file moved under another name doesn't open.

use crate::Crash;
use crate::codec::Level;
use crate::error::{Error, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use vault::Vault;

pub(crate) const MAGIC: &[u8; 4] = b"SMDB";
/// The db's format version, in every file. A file of another version is reported, never guessed at.
pub const FORMAT_VERSION: u16 = 1;
pub(crate) const HEADER: usize = 7;

/// What a file holds (checked on reading, so a file can't stand in for another kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Snapshot = 1,
    Journal = 2,
    Segment = 3,
    Kv = 4,
    Backup = 5,
    NameKey = 6,
    Meta = 7,
    /// A key/value record whose value is compressed (see kv).
    KvPacked = 8,
    /// Key/value records packed together, and the note of how a namespace's bundles are split.
    KvBundle = 9,
    KvBundles = 10,
    /// A month of a series' finished days, packed together (see segment::pack).
    SegmentPack = 11,
}

pub(crate) fn header(kind: Kind) -> Vec<u8> {
    let mut h = Vec::with_capacity(HEADER + 16);
    h.extend_from_slice(MAGIC);
    h.push(kind as u8);
    h.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    h
}

/// The body after a checked header.
pub(crate) fn check_header<'a>(b: &'a [u8], kind: Kind, file: &str) -> Result<&'a [u8]> {
    Ok(check_header_upto(b, kind, file, FORMAT_VERSION)?.1)
}

/// The version and the body after a checked header, for a kind of file that has more versions than the first
/// (`newest`: the last one this program reads).
pub(crate) fn check_header_upto<'a>(b: &'a [u8], kind: Kind, file: &str, newest: u16) -> Result<(u16, &'a [u8])> {
    if b.len() < HEADER || &b[..4] != MAGIC {
        return Err(Error::Damaged { file: file.to_string(), why: "not a Sentry Mate db file".into() });
    }
    let version = u16::from_be_bytes([b[5], b[6]]);
    if !(FORMAT_VERSION..=newest).contains(&version) {
        return Err(Error::Version { file: file.to_string(), found: version, supported: newest });
    }
    if b[4] != kind as u8 {
        return Err(Error::Damaged { file: file.to_string(), why: format!("holds kind {}, not {}", b[4], kind as u8) });
    }
    Ok((version, &b[HEADER..]))
}

pub(crate) fn with_suffix(file: &Path, suffix: &str) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

pub(crate) fn remove_if_exists(file: &Path) -> Result<bool> {
    match std::fs::remove_file(file) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Bytes a file takes on disk: its allocated blocks where the system says, else its length.
pub fn allocated(m: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        m.blocks().saturating_mul(512)
    }
    #[cfg(not(unix))]
    {
        m.len()
    }
}

pub(crate) fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Settings every part of the db shares.
pub(crate) struct Ctx {
    pub vault: Arc<Vault>,
    /// The data folder: labels are paths under it.
    pub data: PathBuf,
    crash: Mutex<Option<Crash>>,
    /// A pause before each file a switch copies (tests: a slow disk), in milliseconds.
    copy_pause_ms: std::sync::atomic::AtomicU64,
    pub opts: crate::Options,
    /// Day snapshots read last, opened and unpacked.
    pub plain: crate::plain::PlainCache,
    /// Indexes of the months' packs of days, and the packs opened last.
    pub packs: crate::segment::pack::PackCache,
}

impl Ctx {
    pub(crate) fn new(vault: Arc<Vault>, data: PathBuf, opts: crate::Options) -> Ctx {
        let plain = crate::plain::PlainCache::new(opts.max_cached_bytes);
        Ctx {
            vault,
            data,
            crash: Mutex::new(None),
            copy_pause_ms: Default::default(),
            opts,
            plain,
            packs: Default::default(),
        }
    }

    /// A file's label: its path under the data folder, with `/` between parts.
    pub(crate) fn label(&self, file: &Path) -> String {
        let rel = file.strip_prefix(&self.data).unwrap_or(file);
        rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
    }

    pub(crate) fn set_crash(&self, at: Option<Crash>) {
        if let Ok(mut c) = self.crash.lock() {
            *c = at;
        }
    }

    pub(crate) fn set_copy_pause(&self, ms: u64) {
        self.copy_pause_ms.store(ms, std::sync::atomic::Ordering::Relaxed);
    }

    /// Waits as long as a test asked before a switch copies a file.
    pub(crate) fn copy_pause(&self) {
        let ms = self.copy_pause_ms.load(std::sync::atomic::Ordering::Relaxed);
        if ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }

    /// Stops here, once, if a test asked for a crash at this point.
    pub(crate) fn crash_point(&self, at: Crash) -> Result<()> {
        let mut c = self.crash.lock().map_err(|_| Error::invalid("crash lock"))?;
        if *c == Some(at) {
            *c = None;
            return Err(Error::Crashed(at));
        }
        Ok(())
    }

    /// Seals `plain` and writes it whole or not at all: a sealed temporary file (`<file>.tmp`, owner-only), synced,
    /// renamed into place, the folder synced.
    pub(crate) fn write_atomic(&self, file: &Path, plain: &[u8]) -> Result<()> {
        let label = self.label(file);
        let sealed = self.vault.seal(&label, plain)?;
        let dir = file.parent().ok_or_else(|| Error::invalid("a file needs a folder"))?;
        std::fs::create_dir_all(dir)?;
        let tmp = with_suffix(file, ".tmp");
        {
            let mut f = private_create(&tmp)?;
            f.write_all(&sealed)?;
            f.sync_all()?;
        }
        self.crash_point(Crash::BeforeRename)?;
        std::fs::rename(&tmp, file)?;
        sync_dir(dir);
        Ok(())
    }

    /// [`Ctx::write_atomic`] with a look at what was written before it takes the file's place: the temporary file
    /// is read back from the disk and opened, and `check` is given its plaintext. If that fails the temporary file
    /// is removed and `file` stays as it was.
    pub(crate) fn write_checked(&self, file: &Path, plain: &[u8], check: &dyn Fn(&[u8]) -> Result<()>) -> Result<()> {
        let label = self.label(file);
        let sealed = self.vault.seal(&label, plain)?;
        let dir = file.parent().ok_or_else(|| Error::invalid("a file needs a folder"))?;
        std::fs::create_dir_all(dir)?;
        let tmp = with_suffix(file, ".tmp");
        {
            let mut f = private_create(&tmp)?;
            f.write_all(&sealed)?;
            f.sync_all()?;
        }
        let read_back = || -> Result<()> {
            let sealed = std::fs::read(&tmp)?;
            let back = self.vault.open_sealed(&label, &sealed).map_err(|e| Error::from(e).in_file(&label))?;
            check(&back)
        };
        if let Err(e) = read_back() {
            remove_if_exists(&tmp)?;
            return Err(e);
        }
        self.crash_point(Crash::BeforeRename)?;
        std::fs::rename(&tmp, file)?;
        sync_dir(dir);
        Ok(())
    }

    /// Reads and opens a whole sealed file. `Ok(None)`: there is no such file.
    pub(crate) fn read(&self, file: &Path) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
        let label = self.label(file);
        match std::fs::read(file) {
            Ok(b) => self.vault.open_sealed(&label, &b).map(Some).map_err(|e| Error::from(e).in_file(&label)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The first `n` plaintext bytes of a sealed file (only its first 64 KiB chunk is opened).
    pub(crate) fn read_head(&self, file: &Path, n: usize) -> Result<Option<Vec<u8>>> {
        let label = self.label(file);
        let f = match std::fs::File::open(file) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let len = f.metadata()?.len();
        let mut r = self.vault.open_reader(&label, std::io::BufReader::new(f), len).map_err(|e| {
            let e: Error = e.into();
            e.in_file(&label)
        })?;
        let mut head = vec![0u8; n];
        let mut got = 0;
        while got < n {
            let k = r.read(&mut head[got..]).map_err(|e| Error::from(vault::unwrap_io(e)).in_file(&label))?;
            if k == 0 {
                break;
            }
            got += k;
        }
        head.truncate(got);
        Ok(Some(head))
    }

    /// Compression levels in use.
    pub(crate) fn level(&self, which: Which) -> Level {
        match which {
            Which::Hot => self.opts.hot,
            Which::Warm => self.opts.warm,
            Which::Cold => self.opts.cold,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Which {
    Hot,
    Warm,
    Cold,
}

/// Creates (or empties) a file only its owner can read.
pub(crate) fn private_create(file: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(file)
    }
    #[cfg(not(unix))]
    {
        std::fs::File::create(file)
    }
}
