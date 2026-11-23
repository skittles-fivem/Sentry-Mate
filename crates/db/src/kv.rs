//! A sealed key/value store for the other parts of Sentry Mate (the address cache, the engine's records, kept
//! answers, a UI pack's notes...): `namespace` + `key` → bytes. A write replaces one small file. File names are a
//! keyed hash (HMAC-SHA256 with a random key kept sealed next to them), so neither namespaces nor keys show on disk:
//!
//! ```text
//! db/kv/names.key                        the hashing key (sealed)
//! db/kv/<hash of namespace>/<hash of namespace and key>.kv     a record written since the last compaction
//!       = "SMDB" | kind 8 | version | varint namespace length | namespace | varint key length | key
//!         | flag (0 a value, 1 deleted) | compressed value (see codec)
//!       (kind 4, written before values were compressed: ... | key | value. Still read.)
//! db/kv/<hash of namespace>/bundles      how the namespace's bundles are split
//!       = "SMDB" | kind 10 | version | varint namespace length | namespace | varint L
//! db/kv/<hash of namespace>/b<L>-<first L hex digits of the records' file names>.kvb     records packed together
//!       = "SMDB" | kind 9 | version | varint namespace length | namespace | varint unpacked length
//!         | compressed ( varint count | ( varint key length | key | varint value length | value )* )
//! ```
//!
//! # Compaction
//!
//! A record in a file of its own takes at least a block of the disk (4 KiB) and its own sealing, however small it
//! is, and its value is compressed alone. [`Kv::compact`] (run when the server is idle) packs a namespace's records
//! into bundles: all the values of a bundle are compressed together, at the cold level, and a bundle is one file.
//! A record's own file always wins over what a bundle holds for its key, so a write after compaction is again one
//! small file (a delete of a bundled key leaves a file that says so), taken into the bundle by the next pass.
//!
//! A namespace is split into 16^L bundles by the first L hex digits of its records' file names, L chosen so a bundle
//! unpacks to about [`BUNDLE_TARGET`] at most: a read opens one bundle (kept in memory, a bounded number of bytes),
//! never the whole namespace, however many keys there are. The split in use is written in `bundles`; bundles of
//! another split are leftovers and are removed.
//!
//! Nothing is removed before its copy is safe. A bundle is written to a temporary file and synced; that file is
//! read back from the disk, opened and compared record by record with what it must hold; only then is it renamed into
//! place, and only then are the records' own files removed. A crash at any point leaves every record readable: from
//! its own file, or from a bundle that holds the same bytes. Splitting further writes all the new bundles first,
//! then `bundles` (the switch), then removes the old ones.

use crate::codec;
use crate::error::{Error, Result};
use crate::file::{Ctx, HEADER, Kind, MAGIC, Which, check_header, header, remove_if_exists, sync_dir, with_suffix};
use crate::plain::{Stamp, stamp};
use crate::segment::columns::{Reader, put_varint};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use zeroize::{Zeroize, Zeroizing};

/// The largest value kept (a store of small things; big files belong elsewhere).
pub const MAX_VALUE: usize = 16 << 20;
const STRIPES: usize = 64;
/// About how much a bundle unpacks to at most before its namespace is split further.
pub const BUNDLE_TARGET: u64 = 2 << 20;
/// A namespace is split by at most this many hex digits (16^6 bundles).
const MAX_SPLIT: u32 = 6;
/// Unpacked bundles kept in memory at most.
const CACHED_BUNDLES: u64 = 64 << 20;
const META: &str = "bundles";
const VALUE: u8 = 0;
const DELETED: u8 = 1;

/// What a namespace takes on disk ([`Kv::usage`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KvUsage {
    pub namespace: String,
    /// Files: records of their own and bundles.
    pub files: u64,
    pub bytes: u64,
    pub disk_bytes: u64,
}

/// What [`Kv::compact`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KvCompacted {
    /// Records taken into bundles, and bundles written.
    pub records: u64,
    pub bundles: u64,
    /// Stopped before the end: the next pass carries on.
    pub stopped: bool,
}

/// A bundle unpacked: its records by key.
struct Bundle {
    blob: Zeroizing<Vec<u8>>,
    at: HashMap<String, (usize, usize)>,
}

impl Bundle {
    fn get(&self, key: &str) -> Option<&[u8]> {
        self.at.get(key).map(|&(a, n)| &self.blob[a..a + n])
    }
}

#[derive(Default)]
struct Bundles {
    files: HashMap<PathBuf, (Stamp, Arc<Bundle>, u64)>,
    bytes: u64,
    tick: u64,
}

/// A record's own file, read.
enum Loose {
    Value(Vec<u8>),
    Deleted,
}

pub struct Kv {
    ctx: Arc<Ctx>,
    dir: PathBuf,
    names: OnceLock<Zeroizing<[u8; 32]>>,
    init: Mutex<()>,
    /// Writes of the same key one at a time (their temporary file is the same).
    stripes: Vec<Mutex<()>>,
    /// Writers share it; a bundle taking records in holds it alone, so no record changes between being read and
    /// its own file being removed.
    gate: RwLock<()>,
    /// One compaction at a time.
    compacting: Mutex<()>,
    bundles: Mutex<Bundles>,
}

fn text(r: &mut Reader<'_>) -> Result<String> {
    let n = r.count(1)?;
    String::from_utf8(r.bytes(n)?.to_vec()).map_err(|_| Error::Corrupt("not text".into()))
}

fn put_text(b: &mut Vec<u8>, t: &str) {
    put_varint(b, t.len() as u64);
    b.extend_from_slice(t.as_bytes());
}

/// The namespace a kv file of any kind says it is of, from its first bytes.
fn peek_namespace(head: &[u8]) -> Option<String> {
    if head.len() < HEADER || &head[..4] != MAGIC {
        return None;
    }
    text(&mut Reader::new(&head[HEADER..])).ok()
}

/// The bundle's file name for a record's file name, at split `l`.
fn bundle_name(l: u32, hash: &str) -> String {
    format!("b{l}-{}.kvb", &hash[..l as usize])
}

/// `(l, prefix)` of a bundle's file name.
fn parse_bundle_name(name: &str) -> Option<(u32, &str)> {
    let (l, prefix) = name.strip_prefix('b')?.strip_suffix(".kvb")?.split_once('-')?;
    let l: u32 = l.parse().ok()?;
    (l <= MAX_SPLIT && prefix.len() == l as usize && prefix.bytes().all(|c| c.is_ascii_hexdigit()))
        .then_some((l, prefix))
}

/// The split that keeps a bundle of `total` unpacked bytes at about [`BUNDLE_TARGET`].
fn split_for(total: u64) -> u32 {
    let mut l = 0;
    while l < MAX_SPLIT && (total >> (4 * l)) > BUNDLE_TARGET {
        l += 1;
    }
    l
}

fn encode_bundle(ctx: &Ctx, ns: &str, records: &BTreeMap<String, Zeroizing<Vec<u8>>>) -> Result<Zeroizing<Vec<u8>>> {
    let mut plain = Zeroizing::new(Vec::new());
    put_varint(&mut plain, records.len() as u64);
    for (k, v) in records {
        put_text(&mut plain, k);
        put_varint(&mut plain, v.len() as u64);
        plain.extend_from_slice(v);
    }
    let mut b = Zeroizing::new(header(Kind::KvBundle));
    put_text(&mut b, ns);
    put_varint(&mut b, plain.len() as u64);
    b.extend_from_slice(&codec::compress(&plain, ctx.level(Which::Cold))?);
    Ok(b)
}

impl Kv {
    pub(crate) fn new(ctx: Arc<Ctx>, dir: PathBuf) -> Kv {
        Kv {
            ctx,
            dir,
            names: OnceLock::new(),
            init: Mutex::new(()),
            stripes: (0..STRIPES).map(|_| Mutex::new(())).collect(),
            gate: RwLock::new(()),
            compacting: Mutex::new(()),
            bundles: Mutex::default(),
        }
    }

    fn name_key(&self) -> Result<&[u8; 32]> {
        if let Some(k) = self.names.get() {
            return Ok(k);
        }
        let _one = self.init.lock().map_err(|_| Error::invalid("kv lock"))?;
        if let Some(k) = self.names.get() {
            return Ok(k);
        }
        let file = self.dir.join("names.key");
        let label = self.ctx.label(&file);
        let key = match self.ctx.read(&file)? {
            Some(b) => {
                let body = check_header(&b, Kind::NameKey, &label)?;
                let mut k = Zeroizing::new([0u8; 32]);
                if body.len() != 32 {
                    return Err(Error::Damaged { file: label, why: "not a key".into() });
                }
                k.copy_from_slice(body);
                k
            }
            None => {
                let mut k = Zeroizing::new([0u8; 32]);
                getrandom::fill(&mut *k).map_err(|e| Error::invalid(format!("no random numbers: {e}")))?;
                let mut b = Zeroizing::new(header(Kind::NameKey));
                b.extend_from_slice(&*k);
                self.ctx.write_atomic(&file, &b)?;
                k
            }
        };
        Ok(self.names.get_or_init(|| key))
    }

    fn hash(&self, parts: &[&str]) -> Result<String> {
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(self.name_key()?).map_err(|_| Error::invalid("hmac key"))?;
        for p in parts {
            m.update(&(p.len() as u64).to_be_bytes());
            m.update(p.as_bytes());
        }
        Ok(hex::encode(&m.finalize().into_bytes()[..16]))
    }

    /// A record's own file, the hash that names it, and its stripe.
    fn file(&self, ns: &str, key: &str) -> Result<(PathBuf, String, usize)> {
        let d = self.hash(&["namespace", ns])?;
        let f = self.hash(&["key", ns, key])?;
        let stripe = usize::from_str_radix(&f[..2], 16).unwrap_or(0) % STRIPES;
        Ok((self.dir.join(d).join(format!("{f}.kv")), f, stripe))
    }

    /// A record's own file opened: its namespace, key and what it holds (either format).
    fn parse(&self, file: &Path, b: &[u8]) -> Result<(String, String, Loose)> {
        let label = self.ctx.label(file);
        let packed = b.get(4) == Some(&(Kind::KvPacked as u8));
        let body = check_header(b, if packed { Kind::KvPacked } else { Kind::Kv }, &label)?;
        let mut r = Reader::new(body);
        let ns = text(&mut r).map_err(|e| e.in_file(&label))?;
        let key = text(&mut r).map_err(|e| e.in_file(&label))?;
        if !packed {
            return Ok((ns, key, Loose::Value(r.rest().to_vec())));
        }
        let what = match r.u8().map_err(|e| e.in_file(&label))? {
            VALUE => Loose::Value(codec::decompress(r.rest()).map_err(|e| e.in_file(&label))?),
            DELETED if r.done() => Loose::Deleted,
            _ => return Err(Error::Damaged { file: label, why: "not a record".into() }),
        };
        Ok((ns, key, what))
    }

    /// A record's own file, if there is one, checked to be this key's.
    fn loose(&self, file: &Path, ns: &str, key: &str) -> Result<Option<Loose>> {
        let Some(b) = self.ctx.read(file)? else { return Ok(None) };
        let (n, k, what) = self.parse(file, &b)?;
        if n != ns || k != key {
            return Err(Error::Damaged { file: self.ctx.label(file), why: "holds another key".into() });
        }
        Ok(Some(what))
    }

    /// The split a namespace's bundles are in. `None`: it has no bundles.
    fn split(&self, dir: &Path) -> Result<Option<u32>> {
        let file = dir.join(META);
        let Some(b) = self.ctx.read(&file)? else { return Ok(None) };
        let label = self.ctx.label(&file);
        let mut r = Reader::new(check_header(&b, Kind::KvBundles, &label)?);
        text(&mut r).map_err(|e| e.in_file(&label))?;
        let l = r.varint().map_err(|e| e.in_file(&label))?;
        if l > u64::from(MAX_SPLIT) || !r.done() {
            return Err(Error::Damaged { file: label, why: "not a split".into() });
        }
        Ok(Some(l as u32))
    }

    /// Opens and unpacks a bundle's file. `Ok(None)`: there is no such file.
    fn read_bundle(&self, file: &Path, label: &str, ns: &str) -> Result<Option<Bundle>> {
        let sealed = match std::fs::read(file) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let b = self.ctx.vault.open_sealed(label, &sealed).map_err(|e| Error::from(e).in_file(label))?;
        let damaged = |why: &str| Error::Damaged { file: label.to_string(), why: why.to_string() };
        let mut r = Reader::new(check_header(&b, Kind::KvBundle, label)?);
        if text(&mut r).map_err(|e| e.in_file(label))? != ns {
            return Err(damaged("holds another namespace"));
        }
        let len = r.varint().map_err(|e| e.in_file(label))?;
        let blob = Zeroizing::new(codec::decompress(r.rest()).map_err(|e| e.in_file(label))?);
        if blob.len() as u64 != len {
            return Err(damaged("a bundle of another length than it says"));
        }
        let mut at = HashMap::new();
        let mut r = Reader::new(&blob);
        let n = r.count(2).map_err(|e| e.in_file(label))?;
        for _ in 0..n {
            let key = text(&mut r).map_err(|e| e.in_file(label))?;
            let len = r.count(1).map_err(|e| e.in_file(label))?;
            let start = blob.len() - r.rest().len();
            r.bytes(len).map_err(|e| e.in_file(label))?;
            if at.insert(key, (start, len)).is_some() {
                return Err(damaged("a key twice"));
            }
        }
        if !r.done() {
            return Err(damaged("bytes after the last record"));
        }
        Ok(Some(Bundle { blob, at }))
    }

    /// A bundle, from memory while its file is the same. `Ok(None)`: there is no such file.
    fn bundle(&self, file: &Path, ns: &str) -> Result<Option<Arc<Bundle>>> {
        let Some(st) = stamp(file)? else { return Ok(None) };
        {
            let mut c = self.bundles.lock().map_err(|_| Error::invalid("kv lock"))?;
            c.tick += 1;
            let tick = c.tick;
            if let Some(e) = c.files.get_mut(file).filter(|e| e.0 == st) {
                e.2 = tick;
                return Ok(Some(e.1.clone()));
            }
        }
        let Some(b) = self.read_bundle(file, &self.ctx.label(file), ns)? else { return Ok(None) };
        let b = Arc::new(b);
        // kept only when the file read is the one looked at
        if stamp(file)? == Some(st) {
            let mut c = self.bundles.lock().map_err(|_| Error::invalid("kv lock"))?;
            c.tick += 1;
            let tick = c.tick;
            if let Some(old) = c.files.insert(file.to_path_buf(), (st, b.clone(), tick)) {
                c.bytes -= old.1.blob.len() as u64;
            }
            c.bytes += b.blob.len() as u64;
            while c.bytes > CACHED_BUNDLES && c.files.len() > 1 {
                let Some(oldest) = c.files.iter().min_by_key(|(_, e)| e.2).map(|(p, _)| p.clone()) else { break };
                if let Some(e) = c.files.remove(&oldest) {
                    c.bytes -= e.1.blob.len() as u64;
                }
            }
        }
        Ok(Some(b))
    }

    /// What the namespace's bundles hold for a key (the record has no file of its own).
    fn bundled(&self, dir: &Path, hash: &str, ns: &str, key: &str) -> Result<Option<Vec<u8>>> {
        // a split under way moves the bundle looked for: look again with the new split
        for _ in 0..3 {
            let Some(l) = self.split(dir)? else { return Ok(None) };
            match self.bundle(&dir.join(bundle_name(l, hash)), ns)? {
                Some(b) => return Ok(b.get(key).map(<[u8]>::to_vec)),
                None if self.split(dir)? == Some(l) => return Ok(None),
                None => {}
            }
        }
        Ok(None)
    }

    pub fn get(&self, ns: &str, key: &str) -> Result<Option<Vec<u8>>> {
        let (file, hash, _) = self.file(ns, key)?;
        match self.loose(&file, ns, key)? {
            Some(Loose::Value(v)) => Ok(Some(v)),
            Some(Loose::Deleted) => Ok(None),
            None => self.bundled(file.parent().unwrap_or(&self.dir), &hash, ns, key),
        }
    }

    fn record(ns: &str, key: &str) -> Zeroizing<Vec<u8>> {
        let mut b = Zeroizing::new(header(Kind::KvPacked));
        put_text(&mut b, ns);
        put_text(&mut b, key);
        b
    }

    pub fn put(&self, ns: &str, key: &str, value: &[u8]) -> Result<()> {
        if value.len() > MAX_VALUE {
            return Err(Error::invalid(format!("a value can be at most {MAX_VALUE} bytes")));
        }
        let (file, _, stripe) = self.file(ns, key)?;
        let mut b = Kv::record(ns, key);
        b.push(VALUE);
        let mut packed = codec::compress(value, self.ctx.level(Which::Hot))?;
        b.extend_from_slice(&packed);
        packed.zeroize();
        let _shared = self.gate.read().map_err(|_| Error::invalid("kv lock"))?;
        let _one = self.stripes[stripe].lock().map_err(|_| Error::invalid("kv lock"))?;
        self.ctx.write_atomic(&file, &b)
    }

    /// Writes a record the way it was written before values were compressed (for tests of old data).
    #[doc(hidden)]
    pub fn put_format1(&self, ns: &str, key: &str, value: &[u8]) -> Result<()> {
        let (file, _, stripe) = self.file(ns, key)?;
        let mut b = Zeroizing::new(header(Kind::Kv));
        put_text(&mut b, ns);
        put_text(&mut b, key);
        b.extend_from_slice(value);
        let _shared = self.gate.read().map_err(|_| Error::invalid("kv lock"))?;
        let _one = self.stripes[stripe].lock().map_err(|_| Error::invalid("kv lock"))?;
        self.ctx.write_atomic(&file, &b)
    }

    /// Whether there was such a key.
    pub fn delete(&self, ns: &str, key: &str) -> Result<bool> {
        let (file, hash, stripe) = self.file(ns, key)?;
        let _shared = self.gate.read().map_err(|_| Error::invalid("kv lock"))?;
        let _one = self.stripes[stripe].lock().map_err(|_| Error::invalid("kv lock"))?;
        let own = self.loose(&file, ns, key)?;
        let bundled = self.bundled(file.parent().unwrap_or(&self.dir), &hash, ns, key)?.is_some();
        if bundled {
            // the bundle still holds it: a file that says it is gone, until the next compaction
            if !matches!(own, Some(Loose::Deleted)) {
                let mut b = Kv::record(ns, key);
                b.push(DELETED);
                self.ctx.write_atomic(&file, &b)?;
            }
        } else {
            remove_if_exists(&file)?;
        }
        Ok(match own {
            Some(Loose::Value(_)) => true,
            Some(Loose::Deleted) => false,
            None => bundled,
        })
    }

    /// The namespace's bundle files of the split in use.
    fn bundle_files(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let Some(l) = self.split(dir)? else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            if e.file_name().to_str().and_then(parse_bundle_name).is_some_and(|(bl, _)| bl == l) {
                out.push(e.path());
            }
        }
        out.sort();
        Ok(out)
    }

    /// Every key of a namespace (this reads each of its files: for listing now and then, not every request).
    pub fn keys(&self, ns: &str) -> Result<Vec<String>> {
        let dir = self.dir.join(self.hash(&["namespace", ns])?);
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        // a compaction moves records from their own files into bundles: not while this looks at both
        let _shared = self.gate.read().map_err(|_| Error::invalid("kv lock"))?;
        let mut keys: BTreeMap<String, bool> = BTreeMap::new();
        for f in self.bundle_files(&dir)? {
            if let Some(b) = self.bundle(&f, ns)? {
                keys.extend(b.at.keys().map(|k| (k.clone(), true)));
            }
        }
        for e in rd {
            let path = e?.path();
            if path.extension().is_none_or(|x| x != "kv") {
                continue;
            }
            if let Some(b) = self.ctx.read(&path)? {
                let (n, k, what) = self.parse(&path, &b)?;
                if n == ns {
                    keys.insert(k, matches!(what, Loose::Value(_)));
                }
            }
        }
        Ok(keys.into_iter().filter(|(_, there)| *there).map(|(k, _)| k).collect())
    }

    /// Every namespace with how many files it has and their bytes on disk as stored, and as allocated (512-byte
    /// blocks; the stored size where the system doesn't say). A namespace's name is read from the first of its files
    /// that opens; a folder none of whose files open is left out.
    pub fn usage(&self) -> Result<Vec<KvUsage>> {
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for d in rd {
            let d = d?;
            if !d.file_type()?.is_dir() {
                continue;
            }
            let mut u = KvUsage { namespace: String::new(), files: 0, bytes: 0, disk_bytes: 0 };
            let mut named = false;
            for f in std::fs::read_dir(d.path())? {
                let f = f?;
                let path = f.path();
                let Ok(m) = f.metadata() else { continue };
                if !m.is_file() || path.extension().is_some_and(|x| x == "tmp") {
                    continue;
                }
                u.files += 1;
                u.bytes += m.len();
                u.disk_bytes += crate::file::allocated(&m);
                if !named
                    && let Ok(Some(head)) = self.ctx.read_head(&path, 1024)
                    && let Some(ns) = peek_namespace(&head)
                {
                    u.namespace = ns;
                    named = true;
                }
            }
            if named {
                out.push(u);
            }
        }
        out.sort_by(|a, b| a.namespace.cmp(&b.namespace));
        Ok(out)
    }

    /// Records in files of their own: what a compaction would take into bundles.
    pub fn pending(&self) -> Result<u64> {
        let mut n = 0;
        for dir in self.namespace_dirs()? {
            n += loose_files(&dir)?.len() as u64;
        }
        Ok(n)
    }

    fn namespace_dirs(&self) -> Result<Vec<PathBuf>> {
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for d in rd {
            let d = d?;
            if d.file_type()?.is_dir() {
                out.push(d.path());
            }
        }
        out.sort();
        Ok(out)
    }

    /// Packs every namespace's records into bundles (see the module docs). `stop` is asked between bundles.
    pub fn compact(&self, stop: &dyn Fn() -> bool) -> Result<KvCompacted> {
        let _one = self.compacting.lock().map_err(|_| Error::invalid("kv lock"))?;
        let mut out = KvCompacted::default();
        for dir in self.namespace_dirs()? {
            if out.stopped {
                break;
            }
            self.compact_namespace(&dir, stop, &mut out)?;
        }
        Ok(out)
    }

    /// The namespace a folder holds, from the first of its files that says.
    fn namespace_of(&self, dir: &Path) -> Result<Option<String>> {
        for f in std::fs::read_dir(dir)? {
            let path = f?.path();
            if path.extension().is_some_and(|x| x == "tmp") {
                continue;
            }
            if let Ok(Some(head)) = self.ctx.read_head(&path, 1024)
                && let Some(ns) = peek_namespace(&head)
            {
                return Ok(Some(ns));
            }
        }
        Ok(None)
    }

    fn write_split(&self, dir: &Path, ns: &str, l: u32) -> Result<()> {
        let mut b = header(Kind::KvBundles);
        put_text(&mut b, ns);
        put_varint(&mut b, u64::from(l));
        self.ctx.write_atomic(&dir.join(META), &b)
    }

    /// Writes a bundle whole: to a temporary file, read back from the disk and compared record by record with
    /// `records`, then renamed into place. No records: the bundle is removed.
    fn write_bundle(&self, file: &Path, ns: &str, records: &BTreeMap<String, Zeroizing<Vec<u8>>>) -> Result<()> {
        if records.is_empty() {
            remove_if_exists(file)?;
            return Ok(());
        }
        let label = self.ctx.label(file);
        let sealed = self.ctx.vault.seal(&label, &encode_bundle(&self.ctx, ns, records)?)?;
        let tmp = with_suffix(file, ".tmp");
        {
            use std::io::Write;
            let mut f = crate::file::private_create(&tmp)?;
            f.write_all(&sealed)?;
            f.sync_all()?;
        }
        let back = self.read_bundle(&tmp, &label, ns)?;
        let same = back.is_some_and(|b| {
            b.at.len() == records.len() && records.iter().all(|(k, v)| b.get(k) == Some(v.as_slice()))
        });
        if !same {
            remove_if_exists(&tmp)?;
            return Err(Error::Damaged { file: label, why: "the bundle written doesn't read back the same".into() });
        }
        self.ctx.crash_point(crate::Crash::BeforeRename)?;
        std::fs::rename(&tmp, file)?;
        if let Some(d) = file.parent() {
            sync_dir(d);
        }
        Ok(())
    }

    /// A bundle's records, to change (empty when there is no such bundle).
    fn bundle_records(&self, file: &Path, ns: &str) -> Result<BTreeMap<String, Zeroizing<Vec<u8>>>> {
        let Some(b) = self.read_bundle(file, &self.ctx.label(file), ns)? else { return Ok(BTreeMap::new()) };
        Ok(b.at.keys().filter_map(|k| b.get(k).map(|v| (k.clone(), Zeroizing::new(v.to_vec())))).collect())
    }

    /// Splits a namespace's bundles further, from `old` to `new` hex digits: the new bundles first (each from the
    /// one old bundle its records are in), then the note of the split, then the old ones go.
    fn resplit(&self, dir: &Path, ns: &str, old: u32, new: u32) -> Result<()> {
        let olds: Vec<PathBuf> = self.bundle_files(dir)?;
        for file in &olds {
            let records = self.bundle_records(file, ns)?;
            let mut parts: BTreeMap<String, BTreeMap<String, Zeroizing<Vec<u8>>>> = BTreeMap::new();
            for (k, v) in records {
                let hash = self.hash(&["key", ns, &k])?;
                parts.entry(bundle_name(new, &hash)).or_default().insert(k, v);
            }
            for (name, records) in &parts {
                self.write_bundle(&dir.join(name), ns, records)?;
            }
        }
        self.write_split(dir, ns, new)?;
        debug_assert!(old < new);
        for file in olds {
            remove_if_exists(&file)?;
        }
        sync_dir(dir);
        Ok(())
    }

    fn compact_namespace(&self, dir: &Path, stop: &dyn Fn() -> bool, out: &mut KvCompacted) -> Result<()> {
        let split = self.split(dir)?;
        // what a crash left: bundles of another split than the one in use, temporary bundles
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let stray = match parse_bundle_name(name) {
                Some((l, _)) => split != Some(l),
                None => name.ends_with(".kvb.tmp"),
            };
            if stray {
                remove_if_exists(&e.path())?;
            }
        }
        let loose = loose_files(dir)?;
        if loose.is_empty() {
            return Ok(());
        }
        let Some(ns) = self.namespace_of(dir)? else { return Ok(()) };
        // how far to split: what the bundles unpack to (each says so in its first bytes) and the loose records
        let mut total: u64 = loose.iter().map(|(_, len)| *len).sum();
        for f in self.bundle_files(dir)? {
            if let Some(head) = self.ctx.read_head(&f, 1024)? {
                let mut r = Reader::new(head.get(HEADER..).unwrap_or_default());
                if text(&mut r).is_ok() {
                    total += r.varint().unwrap_or(0);
                }
            }
        }
        let want = split_for(total).max(split.unwrap_or(0));
        match split {
            Some(l) if l < want => self.resplit(dir, &ns, l, want)?,
            Some(_) => {}
            None => self.write_split(dir, &ns, want)?,
        }
        let mut groups: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for (file, _) in loose {
            let Some(hash) = file.file_stem().and_then(|s| s.to_str()).filter(|h| h.len() == 32) else { continue };
            groups.entry(bundle_name(want, hash)).or_default().push(file);
        }
        for (name, files) in groups {
            if stop() {
                out.stopped = true;
                return Ok(());
            }
            let bundle = dir.join(name);
            // no record changes from here until its own file is gone
            let _alone = self.gate.write().map_err(|_| Error::invalid("kv lock"))?;
            let mut records = self.bundle_records(&bundle, &ns)?;
            let mut taken = Vec::new();
            for file in files {
                // one written or removed since the listing is read as it is now
                let Some(b) = self.ctx.read(&file)? else { continue };
                let (n, k, what) = self.parse(&file, &b)?;
                if n != ns {
                    return Err(Error::Damaged { file: self.ctx.label(&file), why: "holds another namespace".into() });
                }
                match what {
                    Loose::Value(v) => records.insert(k, Zeroizing::new(v)),
                    Loose::Deleted => records.remove(&k),
                };
                taken.push(file);
            }
            self.write_bundle(&bundle, &ns, &records)?;
            self.ctx.crash_point(crate::Crash::BeforeLooseRemoved)?;
            for file in &taken {
                remove_if_exists(file)?;
            }
            sync_dir(dir);
            out.records += taken.len() as u64;
            out.bundles += 1;
        }
        Ok(())
    }
}

/// The records of a namespace's folder that are files of their own, with their size.
fn loose_files(dir: &Path) -> Result<Vec<(PathBuf, u64)>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let path = e.path();
        if path.extension().is_some_and(|x| x == "kv") {
            out.push((path, e.metadata()?.len()));
        }
    }
    out.sort();
    Ok(out)
}
