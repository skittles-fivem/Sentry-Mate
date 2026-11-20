//! Importing an export file into a new server: nothing in the data folder is touched until every byte has been
//! checked, and a half-done import is never mistaken for a complete one.
//!
//! 1. **The header** ([`open_file`], or [`ExportFile::from_head`] for a file that arrives in pieces, a browser's
//!    upload): the magic, a known format, an app version no newer than this one, Argon2id settings within bounds,
//!    and the file's length equal to its `size`.
//! 2. **The recovery key** ([`begin`]): it must open the header's recovery part (the data key), and the first and
//!    last chunks must open with it. Wrong: refused, nothing written. A file in pieces ([`begin_streamed`]) has its
//!    first and last chunks checked as they arrive ([`Import::feed`]): wrong, and the import is removed.
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
/// The recovery key a first start (or a new recovery key) wrote into the keys folder, for the owner to take away.
pub const RECOVERY_FILE: &str = "RECOVERY-KEY.txt";

/// Writes `key` into the keys folder's [`RECOVERY_FILE`] (only the server's own user may read it), with a few words
/// around it on what it is for. Returns the file's path.
pub fn write_recovery_file(keys: &Path, key: &vault::RecoveryKey) -> io::Result<PathBuf> {
    let file = keys.join(RECOVERY_FILE);
    let text = zeroize::Zeroizing::new(format!(
        "Sentry Mate recovery key:\n\n  {}\n\nCopy it somewhere safe away from this server (a password manager, \
         paper), then delete this file.\n",
        key.as_str()
    ));
    vault::write_private(&file, text.as_bytes())?;
    Ok(file)
}

/// Why [`new_recovery_key`] made no new key.
#[derive(Debug, thiserror::Error)]
pub enum NewKeyError {
    #[error("the recovery key file can't be written ({0}): nothing changed, the old recovery key still works")]
    File(io::Error),
    #[error("the vault file can't take the new recovery key ({0}): nothing changed, the old recovery key still works")]
    Vault(vault::Error),
    #[error(
        "the vault file can't take the new recovery key ({0}): the old recovery key still works, but {RECOVERY_FILE} \
         now holds a key that doesn't, and it can't be put back ({1}): delete that file"
    )]
    FileLeft(vault::Error, io::Error),
}

/// A new recovery key for `vault` (the old one stops working), written to the keys folder's [`RECOVERY_FILE`] first
/// and only then into `data/vault.json`: when either write fails, the old key still opens the vault and the file
/// holds a key that opens it. Returns the key and the file's path.
pub fn new_recovery_key(
    vault: &Vault,
    data: &Path,
    keys: &Path,
    kdf: vault::Kdf,
) -> Result<(vault::RecoveryKey, PathBuf), NewKeyError> {
    let new = vault.make_recovery_key(kdf).map_err(NewKeyError::Vault)?;
    let path = keys.join(RECOVERY_FILE);
    let old = match std::fs::read(&path) {
        Ok(b) => Some(zeroize::Zeroizing::new(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(NewKeyError::File(e)),
    };
    let file = write_recovery_file(keys, new.key()).map_err(NewKeyError::File)?;
    if let Err(e) = vault.set_recovery_key(data, &new) {
        // the file must not hold a key that doesn't open the vault
        let back = match &old {
            Some(b) => vault::write_private(&file, b),
            None => std::fs::remove_file(&file),
        };
        return Err(match back {
            Ok(()) => NewKeyError::Vault(e),
            Err(b) => NewKeyError::FileLeft(e, b),
        });
    }
    Ok((new.into_key(), file))
}

/// The recovery key in the file `path` ([`write_recovery_file`]'s): the `SMRK-` word, else the whole text when it
/// holds only a key. `None` when there is no such file or it is empty.
pub fn recovery_from_file(path: &Path) -> Option<zeroize::Zeroizing<String>> {
    let t = zeroize::Zeroizing::new(std::fs::read_to_string(path).ok()?);
    if let Some(w) = t.split_whitespace().find(|w| w.starts_with("SMRK-")) {
        return Some(zeroize::Zeroizing::new(w.to_string()));
    }
    let t = t.trim();
    (!t.is_empty()).then(|| zeroize::Zeroizing::new(t.to_string()))
}
const BEING_MADE: &str = ".importing.new";
const BEING_REMOVED: &str = ".importing.done";
const STATE: &str = "state.sealed";
const STATE_LABEL: &str = ".importing/state.sealed";
const IMPORTED_LABEL: &str = ".move/imported.sealed";
pub(crate) const STAGED: &str = "data";
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
    /// Top-level names in the data folder that a first start made here besides the usual ones, and so don't count as
    /// the owner's (the web side's [`WEB_FIRST_START`] on a server with no account yet). Kept with the import: the
    /// switch sets them aside too, so nothing sealed with the first start's vault is left beside the imported data.
    pub first_start_also: Vec<String>,
}

impl Options {
    pub fn new(app: &str) -> Options {
        Options {
            app: app.to_string(),
            min_free: 2 << 30,
            checkpoint: 256 << 20,
            free_bytes: Box::new(free_space),
            first_start_also: Vec::new(),
        }
    }
}

/// What a running server writes before it is set up (no account yet) that an import or a move from the web pages
/// may replace ([`Options::first_start_also`]): the web side's set-up code state and audit log, and the first
/// start's certificate and migration note (set aside too when the export has none: sealed with the first start's
/// vault, they would open with nothing after the switch; a new certificate is made at the next start).
pub const WEB_FIRST_START: &[&str] = &["accounts", "audit", "tls", "migrated.json"];

/// The options an import runs with: its own, or shared with whoever holds the import (a web upload keeps one open).
pub enum Opts<'a> {
    Borrowed(&'a Options),
    Shared(std::sync::Arc<Options>),
}

impl std::ops::Deref for Opts<'_> {
    type Target = Options;
    fn deref(&self) -> &Options {
        match self {
            Opts::Borrowed(o) => o,
            Opts::Shared(o) => o,
        }
    }
}

impl<'a> From<&'a Options> for Opts<'a> {
    fn from(o: &'a Options) -> Opts<'a> {
        Opts::Borrowed(o)
    }
}

impl From<std::sync::Arc<Options>> for Opts<'_> {
    fn from(o: std::sync::Arc<Options>) -> Self {
        Opts::Shared(o)
    }
}

pub(crate) fn free_space(dir: &Path) -> u64 {
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

/// An export file whose header has been checked ([`open_file`], or [`ExportFile::from_head`] for one that arrives
/// in pieces).
#[derive(Clone)]
pub struct ExportFile {
    /// Where it is read from (empty for one made [`ExportFile::from_head`]: its bytes are [`Import::feed`]).
    pub path: PathBuf,
    pub header: Header,
    /// The file's name, as the owner chose it (kept with the import, to show).
    pub name: String,
    id: [u8; 16],
    /// The file's first bytes up to the body: the magic, the header's length and the header.
    prefix: Vec<u8>,
    prefix_len: u64,
    /// SHA-256 of the file's first bytes up to the body (hex): names this export file.
    prefix_sha256: String,
    label: String,
    len: u64,
    chunks: u64,
    last: usize,
}

pub(crate) fn version(s: &str) -> Option<(u64, u64, u64)> {
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
    let mut head = vec![0u8; 9 + MAX_HEADER];
    let n = read_up_to(&mut f, &mut head)?;
    head.truncate(n);
    let mut ex = check_head(&head, len, app, n as u64 == len)?;
    ex.path = path.to_path_buf();
    ex.name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(ex)
}

/// The header checks over the file's first bytes `head` (`whole`: they are all of it), for a file of `len` bytes.
fn check_head(head: &[u8], len: u64, app: &str, whole: bool) -> Result<ExportFile, Error> {
    // a header that doesn't fit in `head`: cut short when `head` is the whole file, else too long to be one
    let short = |need: u64| if whole { Error::CutShort { need, have: len } } else { malformed("a header too long") };
    if head.len() < 5 || !head.starts_with(b"SMEX") {
        return Err(Error::NotAnExport);
    }
    if &head[..5] != MAGIC {
        return Err(Error::NewerFormat(u32::from(head[4])));
    }
    if head.len() < 9 {
        return Err(short(9));
    }
    let hlen = u32::from_be_bytes([head[5], head[6], head[7], head[8]]) as usize;
    if hlen > MAX_HEADER {
        return Err(malformed("a header too long"));
    }
    if head.len() < 9 + hlen {
        return Err(short(9 + hlen as u64));
    }
    let prefix = head[..9 + hlen].to_vec();
    let header: Header = serde_json::from_slice(&prefix[9..]).map_err(|e| malformed(format!("its header: {e}")))?;
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
    let (chunks, last) = vault::chunks::chunks_of(len - prefix.len() as u64).ok_or_else(|| malformed("its size"))?;
    Ok(ExportFile {
        path: PathBuf::new(),
        name: String::new(),
        id,
        prefix_len: prefix.len() as u64,
        prefix_sha256: hex::encode(Sha256::digest(&prefix)),
        label: body_label(&prefix),
        prefix,
        len,
        chunks,
        last,
        header,
    })
}

impl ExportFile {
    /// Checks the header of an export file that arrives in pieces (a browser's upload), from its first bytes `head`
    /// (up to 64 KiB, or all of it when smaller) and its whole length `size`: the same checks as [`open_file`].
    /// Nothing is read or written; [`begin_streamed`] takes it on, and [`Import::feed`] the bytes.
    pub fn from_head(head: &[u8], size: u64, app: &str) -> Result<ExportFile, Error> {
        if head.len() as u64 > size {
            return Err(malformed("more bytes than its size"));
        }
        check_head(head, size, app, head.len() as u64 == size)
    }

    /// The same, with the name the owner gave the file (kept with the import, to show).
    pub fn named(mut self, name: &str) -> ExportFile {
        self.name = name.to_string();
        self
    }

    /// Opens the header's recovery part with `recovery` (Argon2id: a second or so), for [`begin_streamed`].
    /// [`Error::WrongRecoveryKey`] when it doesn't open it; nothing is written.
    pub fn open_key(&self, recovery: &str) -> Result<Vault, Error> {
        Ok(self.part().open(recovery)?)
    }

    fn part(&self) -> vault::RecoveryPart {
        vault::RecoveryPart { key_id: self.header.key_id.clone(), slot: self.header.recovery.clone() }
    }

    /// The whole file's length.
    pub fn size(&self) -> u64 {
        self.len
    }

    /// SHA-256 of the file's first bytes up to the body (hex): names this export file.
    pub fn prefix_sha256(&self) -> &str {
        &self.prefix_sha256
    }

    /// Where the body starts.
    pub fn prefix_len(&self) -> u64 {
        self.prefix_len
    }

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
    /// Where the body starts in the file (states saved before it was kept: 0).
    #[serde(default)]
    prefix_len: u64,
    /// The file's name, as the owner chose it.
    #[serde(default)]
    name: String,
    /// When the import began (ms since 1970).
    #[serde(default)]
    started_ms: u64,
    /// [`Options::first_start_also`] when it began: the switch sets them aside.
    #[serde(default)]
    first_start_also: Vec<String>,
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
    opts: Opts<'a>,
    /// The staged files being written (opened at the first chunk).
    sink: Option<Sink>,
    /// Plaintext bytes since the last saved place.
    since: u64,
    /// The bytes of the sealed chunk not yet whole ([`Import::feed`]; in memory only).
    partial: Vec<u8>,
    /// Bytes of the file taken so far ([`Import::feed`]); after a restart, the last saved chunk boundary.
    at: u64,
}

fn chain(h: &[u8; 32], piece: &[u8]) -> [u8; 32] {
    let mut s = Sha256::new();
    s.update(h);
    s.update(piece);
    s.finalize().into()
}

pub(crate) fn remove_all(p: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(p) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

pub(crate) fn remove_file(p: &Path) -> io::Result<()> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

pub(crate) fn exists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

pub(crate) fn sync_dir(d: &Path) {
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
    unused_but(data, keys, &[])
}

/// [`unused`], with the top-level names `also` counted as a first start's too ([`Options::first_start_also`]).
pub fn unused_but(data: &Path, keys: &Path, also: &[String]) -> Result<(), Error> {
    if !data.exists() {
        return Ok(());
    }
    let mut found = None;
    let mut skips = Skips::default();
    tree::walk(data, Purpose::Move, &mut skips, |f| -> Result<ControlFlow<()>, Error> {
        let top = f.path.split('/').next().unwrap_or("");
        if first_start(f.path, f.len) || also.iter().any(|a| a == top) {
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
pub fn resume<'a>(data: &Path, keys: &Path, file: &ExportFile, opts: impl Into<Opts<'a>>) -> Result<Resume<'a>, Error> {
    let Some((vault, state)) = open_state(data, keys)? else { return Ok(Resume::None) };
    if state.export_id != file.header.id || state.prefix_sha256 != file.prefix_sha256 || state.file_len != file.len {
        return Ok(Resume::Other(state.phase));
    }
    let chunks = vault.chunks(&file.label, &file.id);
    let at = file.chunk_at(state.next_chunk).min(file.len);
    Ok(Resume::Same(Box::new(Import::new(data, keys, vault, chunks, file, state, opts.into(), at))))
}

/// Starts an import of `file`, opened with `recovery` (the recovery key that was in use when it was made). Refused,
/// with nothing written, for a wrong key, a damaged header or a server that isn't new. A half-done import of another
/// file that is still being read is removed first.
pub fn begin<'a>(
    data: &Path,
    keys: &Path,
    file: &ExportFile,
    recovery: &str,
    opts: impl Into<Opts<'a>>,
) -> Result<Import<'a>, Error> {
    let vault = file.open_key(recovery)?;
    let chunks = vault.chunks(&file.label, &file.id);
    // an edited header, or one from another export, doesn't fit the body: found before anything is written
    file.check_chunk(&chunks, 0)?;
    file.check_chunk(&chunks, file.chunks - 1)?;
    stage(data, keys, file, vault, opts.into())
}

/// Starts an import of `file` ([`ExportFile::from_head`]) whose bytes arrive in pieces ([`Import::feed`]), with
/// `opened`, its recovery part opened by [`ExportFile::open_key`]. The same checks as [`begin`], except that the
/// first and last chunks are checked as they arrive: a wrong one removes the import.
pub fn begin_streamed<'a>(
    data: &Path,
    keys: &Path,
    file: &ExportFile,
    opened: Vault,
    opts: impl Into<Opts<'a>>,
) -> Result<Import<'a>, Error> {
    if opened.key_id() != file.header.key_id {
        return Err(Error::WrongRecoveryKey);
    }
    stage(data, keys, file, opened, opts.into())
}

/// The staging folder of a new import of `file`, made with `vault` (the export's data key).
fn stage<'a>(data: &Path, keys: &Path, file: &ExportFile, vault: Vault, opts: Opts<'a>) -> Result<Import<'a>, Error> {
    let part = file.part();
    let chunks = vault.chunks(&file.label, &file.id);
    unused_but(data, keys, &opts.first_start_also)?;
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
        prefix_len: file.prefix_len,
        name: file.name.clone(),
        started_ms: now_ms(),
        first_start_also: opts.first_start_also.clone(),
    };
    save_state(&vault, &made, &state)?;
    std::fs::rename(&made_keys, keys.join(STAGING))?;
    std::fs::rename(&made, data.join(STAGING))?;
    sync_dir(data);
    sync_dir(keys);
    Ok(Import::new(data, keys, vault, chunks, file, state, opts, 0))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
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
        if let Some(parent) = full.parent()
            && self.last_dir.as_deref() != Some(parent)
        {
            std::fs::create_dir_all(parent)?;
            self.last_dir = Some(parent.to_path_buf());
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

impl<'a> Import<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        data: &Path,
        keys: &Path,
        vault: Vault,
        chunks: vault::Chunks,
        file: &ExportFile,
        state: State,
        opts: Opts<'a>,
        at: u64,
    ) -> Import<'a> {
        Import {
            data: data.to_path_buf(),
            keys: keys.to_path_buf(),
            vault,
            chunks,
            file: file.clone(),
            state,
            opts,
            sink: None,
            since: 0,
            partial: Vec::new(),
            at,
        }
    }
}

impl Import<'_> {
    pub fn progress(&self) -> Progress {
        Progress {
            files_done: files_done(&self.state.parser),
            files: self.state.files,
            read: self.file.prefix_len + self.state.next_chunk * SEALED_CHUNK,
            size: self.file.len,
        }
        .clamped()
    }

    pub fn phase(&self) -> Phase {
        self.state.phase
    }

    /// The export file being imported.
    pub fn file(&self) -> &ExportFile {
        &self.file
    }

    /// Bytes of the file taken so far by [`Import::feed`]: the next byte to send. After a restart ([`resume`]) it
    /// is the last saved sealed-chunk boundary, a little before the last byte sent.
    pub fn received(&self) -> u64 {
        self.at
    }

    fn save(&self) -> Result<(), Error> {
        save_state(&self.vault, &self.data.join(STAGING), &self.state)
    }

    /// A fault of the file itself (damaged, cut, a bad entry) or too little space removes the half-done import:
    /// running it again can't help.
    fn or_discard<T>(&mut self, r: Result<T, Error>) -> Result<T, Error> {
        match r {
            Err(e) if e.is_the_file() || matches!(e, Error::NoSpace { .. }) => {
                self.sink = None;
                self.partial = Vec::new();
                let _ = discard(&self.data, &self.keys);
                Err(e)
            }
            r => r,
        }
    }

    /// Reads and checks the rest of the body into the staging folder. `each` is called after every chunk with how
    /// far it is (`Break`: stop here; it carries on later from the last saved place). A fault of the file itself
    /// (damaged, cut, a bad entry) or too little space removes the half-done import: running it again can't help.
    pub fn run(&mut self, each: impl FnMut(&Progress) -> ControlFlow<()>) -> Result<Outcome, Error> {
        if self.state.phase != Phase::Reading {
            return Ok(Outcome::Ready);
        }
        let r = self.read_body(each);
        self.or_discard(r)
    }

    fn read_body(&mut self, mut each: impl FnMut(&Progress) -> ControlFlow<()>) -> Result<Outcome, Error> {
        let mut f = File::open(&self.file.path)?;
        f.seek(SeekFrom::Start(self.file.chunk_at(self.state.next_chunk)))?;
        let mut buf = vec![0u8; SEALED_CHUNK as usize];
        for c in self.state.next_chunk..self.file.chunks {
            let len = self.file.chunk_len(c);
            if read_up_to(&mut f, &mut buf[..len])? < len {
                return Err(Error::CutShort { need: self.file.len, have: f.metadata()?.len() });
            }
            self.feed_chunk(c, &buf[..len])?;
            if each(&self.progress()).is_break() {
                return Ok(Outcome::Stopped);
            }
        }
        self.finish_body()
    }

    /// Takes the next bytes of the file, from [`Import::received`] on (an upload's chunk, of any length): each
    /// sealed chunk is opened and read as soon as it is whole, so nothing is stored twice; the bytes of one not yet
    /// whole wait in memory. The first chunk is checked when it arrives (a wrong key or an edited header is found
    /// there), the last at the end; after the last byte the import is [`Phase::Ready`]. A fault of the file or too
    /// little space removes the import, as [`Import::run`] does.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Progress, Error> {
        if self.state.phase != Phase::Reading {
            return Err(malformed("bytes after its end"));
        }
        let r = self.feed_bytes(bytes);
        self.or_discard(r)?;
        Ok(self.progress())
    }

    fn feed_bytes(&mut self, mut b: &[u8]) -> Result<(), Error> {
        let plen = self.file.prefix_len;
        if self.at < plen && !b.is_empty() {
            // the header again, as the file has it: it must be the one the import was begun with
            let from = self.at as usize;
            let take = ((plen - self.at) as usize).min(b.len());
            if b[..take] != self.file.prefix[from..from + take] {
                return Err(Error::Damaged(self.at));
            }
            self.at += take as u64;
            b = &b[take..];
        }
        while !b.is_empty() {
            let c = self.state.next_chunk;
            if c >= self.file.chunks {
                return Err(malformed("longer than its header says"));
            }
            let len = self.file.chunk_len(c);
            let take = (len - self.partial.len()).min(b.len());
            self.partial.extend_from_slice(&b[..take]);
            self.at += take as u64;
            b = &b[take..];
            if self.partial.len() == len {
                let whole = std::mem::take(&mut self.partial);
                self.feed_chunk(c, &whole)?;
                self.partial = whole;
                self.partial.clear();
            }
        }
        if self.state.next_chunk == self.file.chunks {
            self.partial = Vec::new();
            self.finish_body()?;
        }
        Ok(())
    }

    /// Opens sealed chunk `c` (its bytes `sealed`), reads it into the staging folder, and saves the place reached
    /// every [`Options::checkpoint`] bytes.
    fn feed_chunk(&mut self, c: u64, sealed: &[u8]) -> Result<(), Error> {
        let (last, at) = (c + 1 == self.file.chunks, self.file.chunk_at(c));
        if self.sink.is_none() {
            let root = self.data.join(STAGING).join(STAGED);
            let mut sink = Sink { root, file: None, last_dir: None };
            if let Some(cur) = &self.state.parser.cur {
                sink.reopen(&cur.entry.path, cur.written)?;
            }
            self.sink = Some(sink);
        }
        let plain = self.chunks.open(c, last, sealed).map_err(|_| Error::Damaged(at))?;
        debug_assert!(plain.len() <= CHUNK);
        let Some(sink) = self.sink.as_mut() else { return Err(Error::Io(io::Error::other("no staging open"))) };
        self.state.parser.feed(&plain, sink, &self.opts)?;
        self.state.next_chunk = c + 1;
        self.state.plain_done += plain.len() as u64;
        self.at = self.at.max(self.file.chunk_at(c + 1).min(self.file.len));
        self.since += plain.len() as u64;
        if self.since >= self.opts.checkpoint && !last {
            // the files on disk first, then the place reached
            sink.sync()?;
            self.save()?;
            self.since = 0;
        }
        Ok(())
    }

    /// After the last chunk: the end checked, the files on disk, and the state `ready`.
    fn finish_body(&mut self) -> Result<Outcome, Error> {
        if self.state.parser.section != Section::Done {
            return Err(Error::Malformed("it ends too soon".into()));
        }
        if let Some(sink) = self.sink.as_mut() {
            sink.sync()?;
        }
        self.sink = None;
        self.state.phase = Phase::Ready;
        self.save()?;
        Ok(Outcome::Ready)
    }
}

fn files_done(p: &Parser) -> u64 {
    match p.section {
        Section::Files => p.i - u64::from(p.cur.is_some()),
        Section::End | Section::Done => p.count,
        _ => 0,
    }
}

/// An import staged here, as its saved state says ([`staged`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub export_id: String,
    /// The file's name, as the owner chose it.
    pub name: String,
    pub phase: Phase,
    /// The export file's length.
    pub size: u64,
    pub files: u64,
    pub files_done: u64,
    /// Bytes of the file read and saved: the last sealed-chunk boundary reached.
    pub received: u64,
    pub started_ms: u64,
}

/// The import staged here, if any (`None` too when its state can't be read: [`pending`] says so).
pub fn staged(data: &Path, keys: &Path) -> Option<Staged> {
    let (_, st) = open_state(data, keys).ok()??;
    let received = if st.phase == Phase::Reading {
        (st.prefix_len + st.next_chunk * SEALED_CHUNK).min(st.file_len)
    } else {
        st.file_len
    };
    Some(Staged {
        files_done: files_done(&st.parser),
        export_id: st.export_id,
        name: st.name,
        phase: st.phase,
        size: st.file_len,
        files: st.files,
        received,
        started_ms: st.started_ms,
    })
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
    #[serde(default)]
    name: String,
    /// The export file's length.
    #[serde(default)]
    size: u64,
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
            unused_but(data, keys, &st.first_start_also)?;
            st.phase = Phase::Switching;
            save_state(&v, &data.join(STAGING), &st)?;
            step()?;
        }
        Phase::Switching => {}
    }
    let note = ImportedNote {
        export_id: st.export_id.clone(),
        name: st.name.clone(),
        size: st.file_len,
        made_ms: st.made_ms,
        app: st.app.clone(),
        files: st.files,
        bytes: st.bytes,
        at_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64),
    };
    let note = serde_json::to_vec(&note).map_err(|e| Error::Io(io::Error::other(e.to_string())))?;
    let replaced_first_start = bring_all_in(data, keys, STAGING, IMPORTED, &note, &v, &st.first_start_also, &mut step)?;
    Ok(Switched {
        export_id: st.export_id,
        made_ms: st.made_ms,
        app: st.app,
        files: st.files,
        bytes: st.bytes,
        replaced_first_start,
    })
}

/// Steps 2 to 7 of a switch (`docs/plans/move-server.md`, 3.3), the same for an import and a move: each entry of
/// `<staging>/data/` renamed into the data folder (a first start's of the same name set aside), then the staged
/// `vault.json` and key file, the note `.move/<note_name>` written (sealed with the data vault now in place, or
/// `fallback`, the same data key), a first start's recovery key file removed, and the staging folders removed.
/// Every step can run again after a crash: an entry still in `<staging>/data/` hasn't been brought in. `aside`: more
/// top-level names a first start made ([`Options::first_start_also`]), set aside even when nothing replaces them.
/// Returns whether a first start's vault was set aside.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bring_all_in(
    data: &Path,
    keys: &Path,
    staging_name: &str,
    note_name: &str,
    note: &[u8],
    fallback: &Vault,
    aside: &[String],
    step: &mut dyn FnMut() -> Result<(), Error>,
) -> Result<bool, Error> {
    let staging = data.join(staging_name);
    let (staged, replaced) = (staging.join(STAGED), staging.join(REPLACED));
    std::fs::create_dir_all(&replaced)?;
    // sealed with the first start's vault: they would open with nothing once the imported vault is in place
    for name in aside.iter().filter(|n| tree::carried(n, Purpose::Move) && !n.contains('/')) {
        if exists(&data.join(name)) && !exists(&staged.join(name)) && !exists(&replaced.join(name)) {
            std::fs::rename(data.join(name), replaced.join(name))?;
        }
    }
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
    let staged_keys = keys.join(staging_name);
    let key = keys.join(vault::KEY_FILE);
    bring_in(&staged_keys.join(vault::KEY_FILE), &key, &staged_keys.join(REPLACED_KEY))?;
    sync_dir(keys);
    step()?;
    let opened = Vault::open(data, keys);
    let v = opened.as_ref().unwrap_or(fallback);
    v.write_file(&data.join(NOTES).join(note_name), &format!("{NOTES}/{note_name}"), note)?;
    step()?;
    // the recovery key a first start wrote opens nothing now
    let replaced_first_start = exists(&staged_keys.join(REPLACED_KEY)) || exists(&replaced.join(vault::VAULT_FILE));
    if replaced_first_start {
        remove_file(&keys.join(RECOVERY_FILE))?;
    }
    step()?;
    let gone = data.join(format!("{staging_name}.done"));
    remove_all(&gone)?;
    std::fs::rename(&staging, &gone)?;
    sync_dir(data);
    step()?;
    remove_all(&gone)?;
    remove_all(&staged_keys)?;
    Ok(replaced_first_start)
}

/// Whether this data folder took an import, and of which export (`.move/imported.sealed`).
pub fn imported(data: &Path, vault: &Vault) -> Option<String> {
    imported_note(data, vault).map(|n| n.export_id)
}

/// What [`imported_note`] says of the import this data folder took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub export_id: String,
    /// The export file's name, as the owner chose it (empty when the note doesn't say).
    pub name: String,
    /// The export file's length (0 when the note doesn't say).
    pub size: u64,
    pub made_ms: u64,
    pub app: String,
    pub files: u64,
    pub bytes: u64,
    /// When the switch wrote the note.
    pub at_ms: u64,
}

/// The import this data folder took (`.move/imported.sealed`), if any.
pub fn imported_note(data: &Path, vault: &Vault) -> Option<Imported> {
    let b = vault.read_file(&data.join(NOTES).join(IMPORTED), IMPORTED_LABEL).ok()??;
    let n = serde_json::from_slice::<ImportedNote>(&b).ok()?;
    Some(Imported {
        export_id: n.export_id,
        name: n.name,
        size: n.size,
        made_ms: n.made_ms,
        app: n.app,
        files: n.files,
        bytes: n.bytes,
        at_ms: n.at_ms,
    })
}
