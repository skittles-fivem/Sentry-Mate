//! Importing an export file into a new server: nothing in the data folder is touched until every byte has been
//! checked, and a half-done import is never mistaken for a complete one.
//!
//! 1. **The header** ([`open_file`]): the magic, a known format, an app version no newer than this one, Argon2id
//!    settings within bounds, and the file's length equal to its `size`.
//! 2. **The recovery key** ([`begin`]): it must open the header's recovery part (the data key), and the first and
//!    last chunks must open with it. Wrong: refused, nothing written.
//! 3. **The target is new** ([`unused`]): nothing of the owner's in the data folder; the keys folder empty, or with
//!    only a first start's key file.
//! 4. **A staging vault**: `<data>/.importing/` with a new key file in `<keys>/.importing/`, and a sealed state
//!    (`.importing/state.sealed`) that says where the import has got to. It opens with that key file, so an import
//!    stopped by a restart carries on without the recovery key ([`resume`]).
//! 5. **The body**, chunk by chunk, every chunk checked: the manifest first (every path by the one rule of
//!    [`crate::tree`], in walk order, none twice, none inside another file's path; then the space needed), then the
//!    files into `.importing/data/` with their times, each one starting the way its kind does. Every 256 MiB the
//!    files are synced and the place reached saved.
//! 6. **The end**: the end marker, the count, the manifest and the entries before the files the same (two running
//!    hashes), nothing after it. Then the state says `ready`.
//! 7. **The switch** ([`switch`]), by whoever holds the data folder: the command when no server runs, else the
//!    server. The state says `switching` first; each entry of `.importing/data/` is renamed into place (a first
//!    start's file of the same name set aside), then `vault.json` and the key file. Every step can run again after a
//!    crash, and the server looks at the state before anything else when it starts.

use crate::Error;
use crate::export::{
    CHUNK, END_MAGIC, FORMAT, Header, MAGIC, MAX_HEADER, PLAIN_FORMAT, PLAIN_MAGIC, SEALED_CHUNK, body_label,
    create_private, read_up_to,
};
use crate::format::{Decoded, Entry, MAX_ENTRY, get_varint};
use crate::tree::{self, Kind, Purpose, Skips, walk_cmp};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use vault::Vault;

/// Where an import is staged, in the data folder and in the keys folder.
pub const STAGING: &str = ".importing";
/// Notes about where this server's data came from.
pub const NOTES: &str = ".move";
pub const IMPORTED: &str = "imported.sealed";
/// The recovery key a first start wrote into the keys folder.
pub const RECOVERY_FILE: &str = "RECOVERY-KEY.txt";
const BEING_MADE: &str = ".importing.new";
const BEING_REMOVED: &str = ".importing.done";
const STATE: &str = "state.sealed";
const STATE_LABEL: &str = ".importing/state.sealed";
const IMPORTED_LABEL: &str = ".move/imported.sealed";
const STAGED: &str = "data";
const REPLACED: &str = "replaced";
const REPLACED_KEY: &str = "replaced.key";

/// How an import runs.
pub struct Options {
    /// This Sentry Mate's version: an export made by a newer one is refused.
    pub app: String,
    /// Free space always left (ingest's `MIN_FREE`).
    pub min_free: u64,
    /// The files are synced and the place reached saved after about this many bytes.
    pub checkpoint: u64,
    pub free_bytes: Box<dyn Fn(&Path) -> u64 + Send + Sync>,
}

impl Options {
    pub fn new(app: &str) -> Options {
        Options { app: app.to_string(), min_free: 2 << 30, checkpoint: 256 << 20, free_bytes: Box::new(free_space) }
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

/// An export file whose header has been checked ([`open_file`]).
#[derive(Clone)]
pub struct ExportFile {
    pub path: PathBuf,
    pub header: Header,
    id: [u8; 16],
    prefix_len: u64,
    /// SHA-256 of the file's first bytes up to the body (hex): names this export file.
    prefix_sha256: String,
    label: String,
    len: u64,
    chunks: u64,
    last: usize,
}

fn version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.');
    let v = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next().unwrap_or("0").parse().ok()?);
    it.next().is_none().then_some(v)
}

fn malformed(why: impl Into<String>) -> Error {
    Error::Malformed(why.into())
}

fn hex_of<const N: usize>(s: &str) -> Option<[u8; N]> {
    let b = hex::decode(s).ok()?;
    b.try_into().ok()
}

/// Opens an export file and checks its header (nothing is written).
pub fn open_file(path: &Path, app: &str) -> Result<ExportFile, Error> {
    // the path in the message: a file copied in by another user may not be readable by the server's
    let mut f = File::open(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    let len = f.metadata()?.len();
    let mut head = [0u8; 9];
    let n = read_up_to(&mut f, &mut head)?;
    if n < 5 || !head.starts_with(b"SMEX") {
        return Err(Error::NotAnExport);
    }
    if &head[..5] != MAGIC {
        return Err(Error::NewerFormat(u32::from(head[4])));
    }
    if n < 9 {
        return Err(Error::CutShort { need: 9, have: len });
    }
    let hlen = u32::from_be_bytes([head[5], head[6], head[7], head[8]]) as usize;
    if hlen > MAX_HEADER {
        return Err(malformed("a header too long"));
    }
    let mut json = vec![0u8; hlen];
    if read_up_to(&mut f, &mut json)? < hlen {
        return Err(Error::CutShort { need: 9 + hlen as u64, have: len });
    }
    let header: Header = serde_json::from_slice(&json).map_err(|e| malformed(format!("its header: {e}")))?;
    if header.format > FORMAT {
        return Err(Error::NewerFormat(header.format));
    }
    if header.format != FORMAT {
        return Err(malformed(format!("an unknown format {}", header.format)));
    }
    match (version(&header.app), version(app)) {
        (Some(made), Some(this)) if made > this => {
            return Err(Error::NewerApp { made_by: header.app.clone(), this: app.to_string() });
        }
        (Some(_), Some(_)) => {}
        _ => return Err(malformed("its version can't be read")),
    }
    let k = header.recovery.kdf();
    if !(8..=4 << 20).contains(&k.m) || !(1..=16).contains(&k.t) || !(1..=16).contains(&k.p) {
        return Err(malformed("recovery settings out of bounds"));
    }
    let id = hex_of::<16>(&header.id).ok_or_else(|| malformed("its id"))?;
    if hex_of::<8>(&header.key_id).is_none() {
        return Err(malformed("its key id"));
    }
    if len < header.size {
        return Err(Error::CutShort { need: header.size, have: len });
    }
    if len > header.size {
        return Err(malformed("longer than its header says"));
    }
    let mut prefix = head.to_vec();
    prefix.extend_from_slice(&json);
    let (chunks, last) = vault::chunks::chunks_of(len - prefix.len() as u64).ok_or_else(|| malformed("its size"))?;
    Ok(ExportFile {
        path: path.to_path_buf(),
        id,
        prefix_len: prefix.len() as u64,
        prefix_sha256: hex::encode(Sha256::digest(&prefix)),
        label: body_label(&prefix),
        len,
        chunks,
        last,
        header,
    })
}

impl ExportFile {
    fn chunk_len(&self, c: u64) -> usize {
        if c + 1 == self.chunks { self.last + vault::chunks::OVERHEAD } else { SEALED_CHUNK as usize }
    }

    fn chunk_at(&self, c: u64) -> u64 {
        self.prefix_len + c * SEALED_CHUNK
    }

    /// Opens chunk `c` on its own (to check a recovery key before anything is written).
    fn check_chunk(&self, chunks: &vault::Chunks, c: u64) -> Result<(), Error> {
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(self.chunk_at(c)))?;
        let mut buf = vec![0u8; self.chunk_len(c)];
        if read_up_to(&mut f, &mut buf)? < buf.len() {
            return Err(Error::CutShort { need: self.len, have: f.metadata()?.len() });
        }
        chunks.open(c, c + 1 == self.chunks, &buf).map_err(|_| Error::Damaged(self.chunk_at(c)))?;
        Ok(())
    }
}

/// Where an import is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// The body is being read and checked.
    Reading,
    /// All of it checked: waiting for the switch.
    Ready,
    /// The switch has begun: it is finished on the next start, whatever happens.
    Switching,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Section {
    #[default]
    Start,
    Manifest,
    Files,
    End,
    Done,
}

mod hexed {
    pub fn serialize<S: serde::Serializer, T: AsRef<[u8]>>(v: &T, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>, T: TryFrom<Vec<u8>>>(d: D) -> Result<T, D::Error> {
        let s = <String as serde::Deserialize>::deserialize(d)?;
        let b = hex::decode(s).map_err(serde::de::Error::custom)?;
        T::try_from(b).map_err(|_| serde::de::Error::custom("wrong length"))
    }
}

/// The file being written.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Cur {
    entry: Entry,
    written: u64,
    #[serde(with = "hexed")]
    head: Vec<u8>,
}

/// Where the reading of the plaintext is: saved with the state, so it carries on after a restart.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct Parser {
    section: Section,
    count: u64,
    /// Entries done in this section.
    i: u64,
    #[serde(with = "hexed")]
    h_manifest: [u8; 32],
    #[serde(with = "hexed")]
    h_files: [u8; 32],
    prev: Option<String>,
    /// The files' bytes, from the manifest.
    total: u64,
    cur: Option<Cur>,
    /// The start of the next piece, when a chunk ended in its middle.
    #[serde(with = "hexed")]
    pending: Vec<u8>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct State {
    v: u32,
    export_id: String,
    prefix_sha256: String,
    file_len: u64,
    app: String,
    made_ms: u64,
    files: u64,
    bytes: u64,
    phase: Phase,
    next_chunk: u64,
    plain_done: u64,
    parser: Parser,
}

/// How far an import is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub files_done: u64,
    pub files: u64,
    /// Bytes of the export file read so far, and in all.
    pub read: u64,
    pub size: u64,
}

/// How [`Import::run`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// All of it read and checked: ready to switch.
    Ready,
    /// Stopped on purpose (the progress callback said so); it carries on from the last saved place.
    Stopped,
}

/// An import being read.
pub struct Import<'a> {
    data: PathBuf,
    keys: PathBuf,
    vault: Vault,
    chunks: vault::Chunks,
    file: ExportFile,
    state: State,
    opts: &'a Options,
}

fn chain(h: &[u8; 32], piece: &[u8]) -> [u8; 32] {
    let mut s = Sha256::new();
    s.update(h);
    s.update(piece);
    s.finalize().into()
}

fn remove_all(p: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(p) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn remove_file(p: &Path) -> io::Result<()> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn exists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

fn sync_dir(d: &Path) {
    if let Ok(f) = File::open(d) {
        let _ = f.sync_all();
    }
}

/// Whether a carried file is one a first start makes by itself (it doesn't count as the owner's data).
fn first_start(path: &str, len: u64) -> bool {
    matches!(path, "migrated.json" | "tls/cert.pem" | "tls/key.sealed")
        || (path.starts_with("can/.waiting/") && len == 0)
}

fn what(path: &str) -> String {
    match path.split('/').next().unwrap_or("") {
        "can" => "it holds CAN logs or uploads".into(),
        "tokens" | "tokens.json" => "it has upload tokens".into(),
        "db" => "it has a database".into(),
        "accounts" => "it has accounts".into(),
        top => format!("it holds files in {top}"),
    }
}

/// Whether a server can take data in: nothing of the owner's in `data` (a first start's own files don't count, nor
/// what the rule of [`crate::tree`] skips), and in `keys` no key file other than one that opens `data`'s vault.
pub fn unused(data: &Path, keys: &Path) -> Result<(), Error> {
    if !data.exists() {
        return Ok(());
    }
    let mut found = None;
    let mut skips = Skips::default();
    tree::walk(data, Purpose::Move, &mut skips, |f| -> Result<ControlFlow<()>, Error> {
        if first_start(f.path, f.len) {
            return Ok(ControlFlow::Continue(()));
        }
        found = Some(f.path.to_string());
        Ok(ControlFlow::Break(()))
    })?;
    if let Some(p) = found {
        return Err(Error::InUse(what(&p)));
    }
    if keys.join(vault::KEY_FILE).exists() && Vault::open(data, keys).is_err() {
        return Err(Error::KeysInUse);
    }
    Ok(())
}

/// Removes what a crash left: a staging folder being made or removed, a staging key folder with no import, unused
/// export snapshots. Run on start, before anything else.
pub fn tidy(data: &Path, keys: &Path) {
    let _ = remove_all(&data.join(BEING_MADE));
    let _ = remove_all(&keys.join(BEING_MADE));
    let _ = remove_all(&data.join(BEING_REMOVED));
    if !exists(&data.join(STAGING)) {
        let _ = remove_all(&keys.join(STAGING));
    }
    crate::export::remove_unused_snapshots(data);
}

fn discard(data: &Path, keys: &Path) -> io::Result<()> {
    if exists(&data.join(STAGING)) {
        // renamed first, so a crash while it is removed never leaves a staging folder without its state
        let gone = data.join(BEING_REMOVED);
        remove_all(&gone)?;
        std::fs::rename(data.join(STAGING), &gone)?;
        remove_all(&gone)?;
    }
    remove_all(&data.join(BEING_MADE))?;
    remove_all(&keys.join(STAGING))?;
    remove_all(&keys.join(BEING_MADE))
}

/// Reads the staging state with whichever vault opens it (the staging one, or the data folder's part way through a
/// switch). `None`: no import here.
fn open_state(data: &Path, keys: &Path) -> Result<Option<(Vault, State)>, Error> {
    let staging = data.join(STAGING);
    if !staging.is_dir() {
        return Ok(None);
    }
    let bytes = match std::fs::read(staging.join(STATE)) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Error::Unreadable),
        Err(e) => return Err(e.into()),
    };
    let staged_keys = keys.join(STAGING);
    for (d, k) in
        [(&staging, &staged_keys), (&data.to_path_buf(), &staged_keys), (&data.to_path_buf(), &keys.to_path_buf())]
    {
        let Ok(v) = Vault::open(d, k) else { continue };
        let Ok(plain) = v.open_sealed(STATE_LABEL, &bytes) else { continue };
        let st: State = serde_json::from_slice(&plain).map_err(|_| Error::Unreadable)?;
        return Ok(Some((v, st)));
    }
    Err(Error::Unreadable)
}

/// What is waiting in the staging folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    None,
    /// Half read: `import` with the same file carries it on.
    Reading {
        percent: u8,
    },
    /// Read and checked: the switch is next.
    Ready {
        export_id: String,
    },
    /// The switch has begun.
    Switching,
    /// There, but its state can't be read.
    Unreadable,
}

/// What is waiting in the staging folder (cheap when there is none: one look for the folder).
pub fn pending(data: &Path, keys: &Path) -> Pending {
    match open_state(data, keys) {
        Ok(None) => Pending::None,
        Ok(Some((_, st))) => match st.phase {
            Phase::Reading => Pending::Reading {
                percent: (st.next_chunk.saturating_mul(100) / (st.file_len / SEALED_CHUNK).max(1)).min(99) as u8,
            },
            Phase::Ready => Pending::Ready { export_id: st.export_id },
            Phase::Switching => Pending::Switching,
        },
        Err(_) => Pending::Unreadable,
    }
}

/// Removes a half-done import (nothing in the data folder itself was touched). Refused once the switch has begun.
/// Returns whether there was one.
pub fn cancel(data: &Path, keys: &Path) -> Result<bool, Error> {
    match open_state(data, keys) {
        Ok(Some((_, st))) if st.phase == Phase::Switching => return Err(Error::Switching),
        Ok(_) | Err(Error::Unreadable) => {}
        Err(e) => return Err(e),
    }
    let had = exists(&data.join(STAGING));
    discard(data, keys)?;
    Ok(had)
}

/// What [`resume`] found.
pub enum Resume<'a> {
    /// No import here.
    None,
    /// A half-done import of this same file: carry on.
    Same(Box<Import<'a>>),
    /// One of another export file, in this phase.
    Other(Phase),
}

/// Carries on a half-done import of `file`, if there is one (no recovery key needed).
pub fn resume<'a>(data: &Path, keys: &Path, file: &ExportFile, opts: &'a Options) -> Result<Resume<'a>, Error> {
    let Some((vault, state)) = open_state(data, keys)? else { return Ok(Resume::None) };
    if state.export_id != file.header.id || state.prefix_sha256 != file.prefix_sha256 || state.file_len != file.len {
        return Ok(Resume::Other(state.phase));
    }
    let chunks = vault.chunks(&file.label, &file.id);
    let (data, keys) = (data.to_path_buf(), keys.to_path_buf());
    Ok(Resume::Same(Box::new(Import { data, keys, vault, chunks, file: file.clone(), state, opts })))
}

/// Starts an import of `file`, opened with `recovery` (the recovery key that was in use when it was made). Refused,
/// with nothing written, for a wrong key, a damaged header or a server that isn't new. A half-done import of another
/// file that is still being read is removed first.
pub fn begin<'a>(
    data: &Path,
    keys: &Path,
    file: &ExportFile,
    recovery: &str,
    opts: &'a Options,
) -> Result<Import<'a>, Error> {
    let part = vault::RecoveryPart { key_id: file.header.key_id.clone(), slot: file.header.recovery.clone() };
    let vault = part.open(recovery)?;
    let chunks = vault.chunks(&file.label, &file.id);
    // an edited header, or one from another export, doesn't fit the body: found before anything is written
    file.check_chunk(&chunks, 0)?;
    file.check_chunk(&chunks, file.chunks - 1)?;
    unused(data, keys)?;
    match open_state(data, keys) {
        Ok(Some((_, st))) if st.phase != Phase::Reading => {
            return Err(if st.phase == Phase::Switching { Error::Switching } else { Error::OtherImport });
        }
        Ok(_) => discard(data, keys)?,
        Err(Error::Unreadable) => return Err(Error::Unreadable),
        Err(e) => return Err(e),
    }
    // made under another name and renamed, so a staging folder always has its state
    let (made, made_keys) = (data.join(BEING_MADE), keys.join(BEING_MADE));
    remove_all(&made)?;
    remove_all(&made_keys)?;
    std::fs::create_dir_all(made.join(STAGED))?;
    vault.install(&made, &made_keys, &part)?;
    let h = &file.header;
    let state = State {
        v: 1,
        export_id: h.id.clone(),
        prefix_sha256: file.prefix_sha256.clone(),
        file_len: file.len,
        app: h.app.clone(),
        made_ms: h.made_ms,
        files: h.files,
        bytes: h.bytes,
        phase: Phase::Reading,
        next_chunk: 0,
        plain_done: 0,
        parser: Parser::default(),
    };
    save_state(&vault, &made, &state)?;
    std::fs::rename(&made_keys, keys.join(STAGING))?;
    std::fs::rename(&made, data.join(STAGING))?;
    sync_dir(data);
    sync_dir(keys);
    let (data, keys) = (data.to_path_buf(), keys.to_path_buf());
    Ok(Import { data, keys, vault, chunks, file: file.clone(), state, opts })
}

fn save_state(vault: &Vault, staging: &Path, state: &State) -> Result<(), Error> {
    let bytes = serde_json::to_vec(state).map_err(|e| Error::Io(io::Error::other(e.to_string())))?;
    vault.write_file(&staging.join(STATE), STATE_LABEL, &bytes)?;
    Ok(())
}

/// Writes the files of an import into the staging folder.
struct Sink {
    root: PathBuf,
    file: Option<BufWriter<File>>,
    last_dir: Option<PathBuf>,
}

impl Sink {
    fn start(&mut self, path: &str) -> io::Result<()> {
        let full = self.root.join(path);
        if let Some(parent) = full.parent() {
            if self.last_dir.as_deref() != Some(parent) {
                std::fs::create_dir_all(parent)?;
                self.last_dir = Some(parent.to_path_buf());
            }
        }
        self.file = Some(BufWriter::with_capacity(1 << 16, create_private(&full)?));
        Ok(())
    }

    /// Opens again the file a saved state was writing, cut back to what was saved.
    fn reopen(&mut self, path: &str, written: u64) -> Result<(), Error> {
        let full = self.root.join(path);
        let mut f = std::fs::OpenOptions::new().write(true).open(&full)?;
        if f.metadata()?.len() < written {
            return Err(Error::Unreadable);
        }
        f.set_len(written)?;
        f.seek(SeekFrom::End(0))?;
        self.file = Some(BufWriter::with_capacity(1 << 16, f));
        Ok(())
    }

    fn write(&mut self, b: &[u8]) -> io::Result<()> {
        match &mut self.file {
            Some(f) => f.write_all(b),
            None => Err(io::Error::other("no file open")),
        }
    }

    fn finish(&mut self, mtime_us: u64) -> io::Result<()> {
        let Some(w) = self.file.take() else { return Err(io::Error::other("no file open")) };
        let f = w.into_inner().map_err(|e| e.into_error())?;
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_micros(mtime_us))?;
        Ok(())
    }

    /// Everything written so far on disk (one `syncfs` for all the files).
    fn sync(&mut self) -> io::Result<()> {
        if let Some(f) = &mut self.file {
            f.flush()?;
        }
        let root = File::open(&self.root)?;
        #[cfg(target_os = "linux")]
        rustix::fs::syncfs(&root)?;
        #[cfg(not(target_os = "linux"))]
        root.sync_all()?;
        Ok(())
    }
}

impl Parser {
    fn bad(why: &str) -> Error {
        Error::Malformed(why.to_string())
    }

    /// The order and path checks every entry gets, in the manifest and before the files.
    fn check(&self, e: &Entry) -> Result<(), Error> {
        if !tree::carried(&e.path, Purpose::Export) {
            return Err(Parser::bad("a path an import doesn't take"));
        }
        if let Some(p) = &self.prev {
            if walk_cmp(p, &e.path) != Ordering::Less {
                return Err(Parser::bad("files out of order, or a file twice"));
            }
            if e.path.len() > p.len() && e.path.starts_with(p.as_str()) && e.path.as_bytes()[p.len()] == b'/' {
                return Err(Parser::bad("a file inside another file's path"));
            }
        }
        Ok(())
    }

    fn advance(&mut self) {
        if self.section == Section::Files && self.cur.is_none() && self.i == self.count {
            self.section = Section::End;
        }
    }

    fn manifest_done(&mut self, root: &Path, opts: &Options) -> Result<(), Error> {
        let need = self.total.saturating_add(opts.min_free);
        let free = (opts.free_bytes)(root);
        if free < need {
            return Err(Error::NoSpace { need, free });
        }
        self.section = Section::Files;
        self.i = 0;
        self.prev = None;
        self.advance();
        Ok(())
    }

    fn end_file(&mut self, sink: &mut Sink) -> Result<(), Error> {
        let Some(cur) = self.cur.take() else { return Ok(()) };
        if Kind::of(&cur.head) != cur.entry.kind {
            return Err(Parser::bad("a file isn't the kind its entry says"));
        }
        sink.finish(cur.entry.mtime_us)?;
        self.advance();
        Ok(())
    }

    /// Takes one whole piece from the front of `pending`: `Some(bytes it took)`, or `None` if more are needed.
    fn element(&mut self, sink: &mut Sink, opts: &Options) -> Result<Option<usize>, Error> {
        let b = &self.pending;
        let bad = Parser::bad;
        match self.section {
            Section::Start => {
                if b.len() < 6 {
                    return if PLAIN_MAGIC.starts_with(&b[..b.len().min(4)]) {
                        Ok(None)
                    } else {
                        Err(bad("its contents"))
                    };
                }
                if &b[..4] != PLAIN_MAGIC || u16::from_be_bytes([b[4], b[5]]) != PLAIN_FORMAT {
                    return Err(bad("its contents"));
                }
                match get_varint(&b[6..]) {
                    Decoded::Need => Ok(None),
                    Decoded::Bad(w) => Err(bad(w)),
                    Decoded::Got(count, k) => {
                        self.count = count;
                        self.section = Section::Manifest;
                        if count == 0 {
                            self.manifest_done(&sink.root, opts)?;
                        }
                        Ok(Some(6 + k))
                    }
                }
            }
            Section::Manifest | Section::Files => {
                let (e, k) = match Entry::decode(b) {
                    Decoded::Need => return Ok(None),
                    Decoded::Bad(w) => return Err(bad(w)),
                    Decoded::Got(e, k) => (e, k),
                };
                self.check(&e)?;
                if self.section == Section::Manifest {
                    self.h_manifest = chain(&self.h_manifest, &self.pending[..k]);
                    self.total = self.total.checked_add(e.len).ok_or_else(|| bad("sizes too large"))?;
                    self.prev = Some(e.path);
                    self.i += 1;
                    if self.i == self.count {
                        self.manifest_done(&sink.root, opts)?;
                    }
                } else {
                    self.h_files = chain(&self.h_files, &self.pending[..k]);
                    self.prev = Some(e.path.clone());
                    self.i += 1;
                    if self.i > self.count {
                        return Err(bad("more files than the manifest lists"));
                    }
                    sink.start(&e.path)?;
                    let empty = e.len == 0;
                    self.cur = Some(Cur { entry: e, written: 0, head: Vec::new() });
                    if empty {
                        self.end_file(sink)?;
                    }
                }
                Ok(Some(k))
            }
            Section::End => {
                if b.len() < 4 {
                    return if END_MAGIC.starts_with(b) { Ok(None) } else { Err(bad("its end")) };
                }
                if &b[..4] != END_MAGIC {
                    return Err(bad("its end"));
                }
                match get_varint(&b[4..]) {
                    Decoded::Need => Ok(None),
                    Decoded::Bad(w) => Err(bad(w)),
                    Decoded::Got(c, k) => {
                        if c != self.count || self.h_manifest != self.h_files {
                            return Err(bad("the manifest and the files don't agree"));
                        }
                        self.section = Section::Done;
                        Ok(Some(4 + k))
                    }
                }
            }
            Section::Done => Err(bad("bytes after its end")),
        }
    }

    fn feed(&mut self, mut data: &[u8], sink: &mut Sink, opts: &Options) -> Result<(), Error> {
        while !data.is_empty() {
            if let Some(cur) = &mut self.cur {
                let take = (cur.entry.len - cur.written).min(data.len() as u64) as usize;
                sink.write(&data[..take])?;
                if cur.head.len() < 6 {
                    let more = (6 - cur.head.len()).min(take);
                    cur.head.extend_from_slice(&data[..more]);
                }
                cur.written += take as u64;
                data = &data[take..];
                if cur.written == cur.entry.len {
                    self.end_file(sink)?;
                }
                continue;
            }
            let old = self.pending.len();
            let add = (MAX_ENTRY - old).min(data.len());
            self.pending.extend_from_slice(&data[..add]);
            match self.element(sink, opts)? {
                Some(used) if used > old => {
                    data = &data[used - old..];
                    self.pending.clear();
                }
                Some(_) => return Err(Parser::bad("its pieces")),
                None if self.pending.len() >= MAX_ENTRY => return Err(Parser::bad("an entry too long")),
                None => data = &data[add..],
            }
        }
        Ok(())
    }
}

impl Import<'_> {
    pub fn progress(&self) -> Progress {
        let p = &self.state.parser;
        let files_done = if p.section == Section::Files { p.i - u64::from(p.cur.is_some()) } else { 0 };
        let files_done = if matches!(p.section, Section::End | Section::Done) { p.count } else { files_done };
        Progress {
            files_done,
            files: self.state.files,
            read: self.file.prefix_len + self.state.next_chunk * SEALED_CHUNK,
            size: self.file.len,
        }
        .clamped()
    }

    pub fn phase(&self) -> Phase {
        self.state.phase
    }

    fn save(&self) -> Result<(), Error> {
        save_state(&self.vault, &self.data.join(STAGING), &self.state)
    }

    /// Reads and checks the rest of the body into the staging folder. `each` is called after every chunk with how
    /// far it is (`Break`: stop here; it carries on later from the last saved place). A fault of the file itself
    /// (damaged, cut, a bad entry) or too little space removes the half-done import: running it again can't help.
    pub fn run(&mut self, each: impl FnMut(&Progress) -> ControlFlow<()>) -> Result<Outcome, Error> {
        if self.state.phase != Phase::Reading {
            return Ok(Outcome::Ready);
        }
        match self.read_body(each) {
            Err(e) if e.is_the_file() || matches!(e, Error::NoSpace { .. }) => {
                let _ = discard(&self.data, &self.keys);
                Err(e)
            }
            r => r,
        }
    }

    fn read_body(&mut self, mut each: impl FnMut(&Progress) -> ControlFlow<()>) -> Result<Outcome, Error> {
        let root = self.data.join(STAGING).join(STAGED);
        let mut sink = Sink { root, file: None, last_dir: None };
        if let Some(cur) = &self.state.parser.cur {
            sink.reopen(&cur.entry.path, cur.written)?;
        }
        let mut f = File::open(&self.file.path)?;
        f.seek(SeekFrom::Start(self.file.chunk_at(self.state.next_chunk)))?;
        let mut buf = vec![0u8; SEALED_CHUNK as usize];
        let mut since = 0u64;
        for c in self.state.next_chunk..self.file.chunks {
            let (last, len, at) = (c + 1 == self.file.chunks, self.file.chunk_len(c), self.file.chunk_at(c));
            if read_up_to(&mut f, &mut buf[..len])? < len {
                return Err(Error::CutShort { need: self.file.len, have: f.metadata()?.len() });
            }
            let plain = self.chunks.open(c, last, &buf[..len]).map_err(|_| Error::Damaged(at))?;
            debug_assert!(plain.len() <= CHUNK);
            self.state.parser.feed(&plain, &mut sink, self.opts)?;
            self.state.next_chunk = c + 1;
            self.state.plain_done += plain.len() as u64;
            since += plain.len() as u64;
            if since >= self.opts.checkpoint && !last {
                // the files on disk first, then the place reached
                sink.sync()?;
                self.save()?;
                since = 0;
            }
            if each(&self.progress()).is_break() {
                return Ok(Outcome::Stopped);
            }
        }
        if self.state.parser.section != Section::Done {
            return Err(Error::Malformed("it ends too soon".into()));
        }
        sink.sync()?;
        self.state.phase = Phase::Ready;
        self.save()?;
        Ok(Outcome::Ready)
    }
}

impl Progress {
    fn clamped(mut self) -> Progress {
        self.read = self.read.min(self.size);
        self
    }
}

/// What [`switch`] brought in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switched {
    pub export_id: String,
    pub made_ms: u64,
    pub app: String,
    pub files: u64,
    pub bytes: u64,
    /// A first start's own vault (and its recovery key file) was set aside and removed.
    pub replaced_first_start: bool,
}

/// Where the data came from (`.move/imported.sealed`).
#[derive(serde::Serialize, serde::Deserialize)]
struct ImportedNote {
    export_id: String,
    made_ms: u64,
    app: String,
    files: u64,
    bytes: u64,
    at_ms: u64,
}

/// Renames `staged` to `live`, setting aside what is at `live` (a first start's) in `aside`. Does nothing when
/// `staged` is gone (brought in already).
fn bring_in(staged: &Path, live: &Path, aside: &Path) -> io::Result<bool> {
    if !exists(staged) {
        return Ok(false);
    }
    if exists(live) {
        if exists(aside) {
            return Err(io::Error::other(format!("{} is in the way of the import", live.display())));
        }
        std::fs::rename(live, aside)?;
    }
    std::fs::rename(staged, live)?;
    Ok(true)
}

/// Switches the data folder to a ready import (or carries on a switch a crash stopped). The caller holds the data
/// folder: no server runs on it, or the server running it has stopped its writes. Checks again that the data folder
/// is new before the first step.
pub fn switch(data: &Path, keys: &Path) -> Result<Switched, Error> {
    switch_steps(data, keys, u32::MAX)
}

/// [`switch`], stopping (as a crash would) after `steps` steps: for tests.
#[doc(hidden)]
pub fn switch_steps(data: &Path, keys: &Path, steps: u32) -> Result<Switched, Error> {
    let mut left = steps;
    let mut step = || -> Result<(), Error> {
        if left == 0 {
            return Err(Error::Io(io::Error::other("stopped here (a test)")));
        }
        left -= 1;
        Ok(())
    };
    let Some((v, mut st)) = open_state(data, keys)? else { return Err(Error::NotReady) };
    match st.phase {
        Phase::Reading => return Err(Error::NotReady),
        Phase::Ready => {
            unused(data, keys)?;
            st.phase = Phase::Switching;
            save_state(&v, &data.join(STAGING), &st)?;
            step()?;
        }
        Phase::Switching => {}
    }
    let staging = data.join(STAGING);
    let (staged, replaced) = (staging.join(STAGED), staging.join(REPLACED));
    std::fs::create_dir_all(&replaced)?;
    let mut names: Vec<String> = Vec::new();
    for e in std::fs::read_dir(&staged).into_iter().flatten() {
        names.push(e?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    for name in names {
        if bring_in(&staged.join(&name), &data.join(&name), &replaced.join(&name))? {
            step()?;
        }
    }
    sync_dir(data);
    bring_in(&staging.join(vault::VAULT_FILE), &data.join(vault::VAULT_FILE), &replaced.join(vault::VAULT_FILE))?;
    sync_dir(data);
    step()?;
    let staged_keys = keys.join(STAGING);
    let key = keys.join(vault::KEY_FILE);
    bring_in(&staged_keys.join(vault::KEY_FILE), &key, &staged_keys.join(REPLACED_KEY))?;
    sync_dir(keys);
    step()?;
    let v = Vault::open(data, keys).unwrap_or(v);
    let note = ImportedNote {
        export_id: st.export_id.clone(),
        made_ms: st.made_ms,
        app: st.app.clone(),
        files: st.files,
        bytes: st.bytes,
        at_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64),
    };
    let note = serde_json::to_vec(&note).map_err(|e| Error::Io(io::Error::other(e.to_string())))?;
    v.write_file(&data.join(NOTES).join(IMPORTED), IMPORTED_LABEL, &note)?;
    step()?;
    // the recovery key a first start wrote opens nothing now
    let replaced_first_start = exists(&staged_keys.join(REPLACED_KEY)) || exists(&replaced.join(vault::VAULT_FILE));
    if replaced_first_start {
        remove_file(&keys.join(RECOVERY_FILE))?;
    }
    step()?;
    let gone = data.join(BEING_REMOVED);
    remove_all(&gone)?;
    std::fs::rename(&staging, &gone)?;
    sync_dir(data);
    step()?;
    remove_all(&gone)?;
    remove_all(&staged_keys)?;
    Ok(Switched {
        export_id: st.export_id,
        made_ms: st.made_ms,
        app: st.app,
        files: st.files,
        bytes: st.bytes,
        replaced_first_start,
    })
}

/// Whether this data folder took an import, and of which export (`.move/imported.sealed`).
pub fn imported(data: &Path, vault: &Vault) -> Option<String> {
    let b = vault.read_file(&data.join(NOTES).join(IMPORTED), IMPORTED_LABEL).ok()??;
    serde_json::from_slice::<ImportedNote>(&b).ok().map(|n| n.export_id)
}
