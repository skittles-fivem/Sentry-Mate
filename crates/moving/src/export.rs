//! The export file: the whole data folder in one file, the files exactly as stored, opened on a new server with the
//! recovery key.
//!
//! ```text
//! export = "SMEX" 0x01 | u32 header length (big endian, at most 64 KiB) | header (JSON) | body
//! header = { format, app, id, made_ms, key_id, recovery, files, bytes, size }
//! body   = the plaintext below in sealed chunks (vault::chunks): 64 KiB each, the last one shorter and flagged;
//!          keys from the data key and the export id, label "sentry-mate export v1 " + hex SHA-256 of everything
//!          before the body
//! plain  = "SMXB" | u16 format | varint count | count x entry       the manifest
//!          | count x ( entry | the file's bytes )                    every file, its entry again just before it
//!          | "SMXE" | varint count
//! entry  = varint path length | path | kind (u8: 1 sealed, 2 records, 3 plain) | u64 length | u64 time (µs)
//! ```
//!
//! - **It opens with the recovery key**: the header carries the recovery part of `vault.json` (the data key wrapped
//!   by the recovery key, through Argon2id). Never the key file, never the data key: the file alone is useless.
//! - **All of it is authenticated**: every chunk is sealed with its number and whether it is the last, under a label
//!   that binds the header, so a changed, cut, reordered or swapped chunk, or an edited header, doesn't open. File
//!   names and sizes are inside the sealing.
//! - **The manifest comes first**, so an import checks every path and the space needed before it writes a file; each
//!   entry comes again just before its bytes, so the importer holds one entry at a time, never the whole list.
//! - **`size` is the whole file's length**, so a file cut short is found before anything is read.
//!
//! # Written while the server runs: a snapshot
//!
//! [`Snapshot::make`] pauses the writes ([`gate`]) for as long as it takes to look at every carried file once: sealed
//! whole files are hard-linked into `.export/<id>/files/` (instant, no space: the server never changes them in
//! place, it replaces them, and a link keeps the version it points to); growing files whose records are whole are
//! linked too (only bytes after that length ever change); plain files, and growing files with a tail that isn't a
//! whole record, are copied as stored. Then the writes go on and the export is read from the snapshot.
//!
//! The snapshot can't change, so the export's bytes are the same every time: [`Snapshot::reader`] starts at any
//! byte (a resumed download), making the chunk that holds it again. The chunks' nonces are made from their own
//! bytes (see `vault::chunks`), so even a file that did change under a snapshot is never sealed twice with one nonce.

use crate::Error;
use crate::format::{Entry, put_varint, varint_len};
use crate::tree::{self, Kind, Purpose, Skips};
use rustix::fs::{FlockOperation, flock};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::Duration;
use vault::{Vault, chunks};

pub const MAGIC: &[u8; 5] = b"SMEX\x01";
pub const FORMAT: u32 = 1;
pub const MAX_HEADER: usize = 64 * 1024;
/// Where snapshots are kept, at the top of the data folder (never carried).
pub const SNAPSHOTS: &str = ".export";
pub(crate) const PLAIN_MAGIC: &[u8; 4] = b"SMXB";
pub(crate) const PLAIN_FORMAT: u16 = 1;
pub(crate) const END_MAGIC: &[u8; 4] = b"SMXE";
pub(crate) const CHUNK: usize = chunks::MAX;
pub(crate) const SEALED_CHUNK: u64 = (chunks::MAX + chunks::OVERHEAD) as u64;
const FILES: &str = "files";
const ENTRIES: &str = "entries.sealed";
const PLAN: &str = "plan.sealed";
const LOCK: &str = "lock";

/// An export's header (in the clear, bound to the body).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Header {
    pub format: u32,
    /// The Sentry Mate version that made it.
    pub app: String,
    /// The export id (32 hex digits): the body's keys come from it.
    pub id: String,
    pub made_ms: u64,
    /// The data key's id.
    pub key_id: String,
    /// The data key wrapped by the recovery key.
    pub recovery: vault::RecoverySlot,
    pub files: u64,
    /// The files' bytes, as stored.
    pub bytes: u64,
    /// The whole export file's length.
    pub size: u64,
}

impl Header {
    /// The file's first bytes: the magic, the header's length and the header.
    pub(crate) fn prefix(&self) -> Result<Vec<u8>, Error> {
        let json = serde_json::to_vec(self).map_err(|e| Error::Malformed(e.to_string()))?;
        let len = u32::try_from(json.len()).ok().filter(|&l| l as usize <= MAX_HEADER);
        let len = len.ok_or_else(|| Error::Malformed("a header too long".into()))?;
        let mut p = MAGIC.to_vec();
        p.extend_from_slice(&len.to_be_bytes());
        p.extend_from_slice(&json);
        Ok(p)
    }
}

/// The label of an export's chunks: binds the header (and so the export id, the size and the recovery part).
pub(crate) fn body_label(prefix: &[u8]) -> String {
    format!("sentry-mate export v1 {}", hex::encode(Sha256::digest(prefix)))
}

/// The plaintext's length for `files` files with `manifest` bytes of entries and `bytes` bytes of files.
pub(crate) fn plain_len(files: u64, manifest: u64, bytes: u64) -> u64 {
    4 + 2 + varint_len(files) + 2 * manifest + bytes + 4 + varint_len(files)
}

/// What a snapshot's plan file holds.
#[derive(serde::Serialize, serde::Deserialize)]
struct Plan {
    header: Header,
    manifest: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The day of a time, "2026-11-17" (UTC).
pub fn day_of(ms: u64) -> String {
    // days to a civil date (Howard Hinnant's algorithm)
    let z = (ms / 86_400_000) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

pub(crate) fn create_private(path: &Path) -> io::Result<File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)
}

/// Reads up to `buf.len()` bytes (fewer only at the end of the file).
pub(crate) fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

fn lock_snapshot(dir: &Path) -> io::Result<Option<File>> {
    let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(LOCK))?;
    match flock(&f, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(f)),
        Err(e) if e == rustix::io::Errno::WOULDBLOCK || e == rustix::io::Errno::AGAIN => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Removes snapshots no one uses (a crash's, or one a finished download left). Returns how many.
pub fn remove_unused_snapshots(data: &Path) -> usize {
    let mut n = 0;
    for e in std::fs::read_dir(data.join(SNAPSHOTS)).into_iter().flatten().flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            let _ = std::fs::remove_file(&dir);
            continue;
        }
        // held while it is in use; one with no lock file was cut short while it was made
        if let Ok(Some(_held)) = lock_snapshot(&dir) {
            if std::fs::remove_dir_all(&dir).is_ok() {
                n += 1;
            }
        }
    }
    n
}

/// A snapshot of the data folder for one export (removed when dropped).
pub struct Snapshot {
    dir: PathBuf,
    id: [u8; 16],
    header: Header,
    prefix: Vec<u8>,
    skips: Skips,
    _lock: File,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Snapshot {
    /// Takes the snapshot: pauses the writes (waiting at most `wait` for those under way), looks at every carried file
    /// once, then lets the writes go on. `app` is this Sentry Mate's version.
    pub fn make(data: &Path, vault: &Vault, gate: &gate::Gate, app: &str, wait: Duration) -> Result<Snapshot, Error> {
        remove_unused_snapshots(data);
        let mut id = [0u8; 16];
        getrandom::fill(&mut id).map_err(|e| Error::Io(io::Error::other(e.to_string())))?;
        let hid = hex::encode(id);
        let dir = data.join(SNAPSHOTS).join(&hid);
        std::fs::create_dir_all(dir.join(FILES))?;
        let Some(lock) = lock_snapshot(&dir)? else {
            return Err(Error::Io(io::Error::other("a new snapshot's lock is held")));
        };
        let made = take(data, &dir, &hid, vault, gate, app, wait);
        match made {
            Ok((header, prefix, skips)) => Ok(Snapshot { dir, id, header, prefix, skips, _lock: lock }),
            Err(e) => {
                drop(lock);
                let _ = std::fs::remove_dir_all(&dir);
                Err(e)
            }
        }
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The export file's length.
    pub fn size(&self) -> u64 {
        self.header.size
    }

    /// What the walk skipped (named in the log and the UI).
    pub fn skips(&self) -> &Skips {
        &self.skips
    }

    /// The file's name: `sentry-mate-2026-11-17.smexport`.
    pub fn file_name(&self) -> String {
        format!("sentry-mate-{}.smexport", day_of(self.header.made_ms))
    }

    /// The export file from byte `from` on (a resumed download starts where it was cut).
    pub fn reader<'a>(&'a self, vault: &'a Vault, from: u64) -> Result<ExportReader<'a>, Error> {
        let label = body_label(&self.prefix);
        let chunks = vault.chunks(&label, &self.id);
        let (count, last) = chunks::chunks_of(self.header.size - self.prefix.len() as u64)
            .ok_or_else(|| Error::Malformed("the export's size doesn't add up".into()))?;
        let from = from.min(self.header.size);
        let (prefix_pos, chunk, skip) = if from < self.prefix.len() as u64 {
            (from as usize, 0, 0)
        } else {
            let off = from - self.prefix.len() as u64;
            (self.prefix.len(), off / SEALED_CHUNK, (off % SEALED_CHUNK) as usize)
        };
        let plain = Plain::new(self, vault, chunk.min(count) * CHUNK as u64)?;
        Ok(ExportReader {
            prefix: &self.prefix,
            prefix_pos,
            plain,
            chunks,
            next: chunk,
            count,
            last,
            buf: vec![0u8; CHUNK],
            out: Vec::with_capacity(SEALED_CHUNK as usize),
            out_pos: 0,
            skip,
        })
    }

    /// Writes the whole export to `out`; returns its length.
    pub fn write_to(&self, vault: &Vault, out: &mut impl Write) -> Result<u64, Error> {
        let mut r = self.reader(vault, 0)?;
        let n = io::copy(&mut r, out).map_err(unwrap_changed)?;
        out.flush()?;
        if n != self.header.size {
            return Err(Error::Changed("the export came out at another length".into()));
        }
        Ok(n)
    }
}

fn unwrap_changed(e: io::Error) -> Error {
    if e.kind() == io::ErrorKind::UnexpectedEof { Error::Changed(e.to_string()) } else { Error::Io(e) }
}

type Made = (Header, Vec<u8>, Skips);

fn take(
    data: &Path,
    dir: &Path,
    hid: &str,
    vault: &Vault,
    gate: &gate::Gate,
    app: &str,
    wait: Duration,
) -> Result<Made, Error> {
    let files_dir = dir.join(FILES);
    let entries_label = format!("{SNAPSHOTS}/{hid}/{ENTRIES}");
    let out = create_private(&dir.join(ENTRIES))?;
    let mut w = vault.seal_writer(&entries_label, BufWriter::with_capacity(1 << 16, out))?;
    let (mut files, mut bytes, mut manifest) = (0u64, 0u64, 0u64);
    let mut skips = Skips::default();
    let pause = gate.pause(wait)?;
    // read under the pause too: what an export opens with is the recovery key of that moment
    let recovery = Vault::recovery_part(data)?;
    let made_ms = now_ms();
    let mut last_dir: Option<PathBuf> = None;
    let mut enc = Vec::with_capacity(256);
    let mut unreadable = Skips::default();
    tree::walk(data, Purpose::Export, &mut skips, |f| -> Result<ControlFlow<()>, Error> {
        // a file this server may not open (made by root, say) is skipped and named, like a folder it can't read
        let mut src = match tree::open_file(f.full) {
            Ok(x) => x,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                unreadable.add(f.path.to_string(), tree::UNREADABLE);
                return Ok(ControlFlow::Continue(()));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ControlFlow::Continue(())),
            Err(e) => return Err(e.into()),
        };
        let dst = files_dir.join(f.path);
        if let Some(parent) = dst.parent() {
            if last_dir.as_deref() != Some(parent) {
                std::fs::create_dir_all(parent)?;
                last_dir = Some(parent.to_path_buf());
            }
        }
        let mut head = [0u8; vault::records::HEADER];
        let n = read_up_to(&mut src, &mut head)?;
        let kind = Kind::of(&head[..n]);
        let link = match kind {
            Kind::Sealed => true,
            Kind::Records => vault::records::whole_end(&src)? == Some(f.len),
            Kind::Plain => false,
        };
        let len = if link {
            std::fs::hard_link(f.full, &dst).map_err(Error::NoHardLinks)?;
            f.len
        } else {
            src.seek(SeekFrom::Start(0))?;
            let mut out = create_private(&dst)?;
            io::copy(&mut src, &mut out)?
        };
        let e = Entry { path: f.path.to_string(), kind, len, mtime_us: f.mtime_us };
        enc.clear();
        e.encode(&mut enc);
        w.write_all(&enc)?;
        files += 1;
        bytes += len;
        manifest += enc.len() as u64;
        Ok(ControlFlow::Continue(()))
    })?;
    skips.merge(unreadable);
    drop(pause);
    let out = w.finish()?.into_inner().map_err(|e| e.into_error())?;
    out.sync_all()?;
    let mut header = Header {
        format: FORMAT,
        app: app.to_string(),
        id: hid.to_string(),
        made_ms,
        key_id: recovery.key_id,
        recovery: recovery.slot,
        files,
        bytes,
        size: 0,
    };
    let plain = plain_len(files, manifest, bytes);
    let body = chunks::sealed_len(plain);
    // the header holds the size, which holds the header's length: a few rounds settle it
    for _ in 0..8 {
        let size = header.prefix()?.len() as u64 + body;
        if size == header.size {
            break;
        }
        header.size = size;
    }
    let prefix = header.prefix()?;
    if prefix.len() as u64 + body != header.size {
        return Err(Error::Malformed("the export's size doesn't settle".into()));
    }
    let plan =
        serde_json::to_vec(&Plan { header: header.clone(), manifest }).map_err(|e| Error::Malformed(e.to_string()))?;
    vault.write_file(&dir.join(PLAN), &format!("{SNAPSHOTS}/{hid}/{PLAN}"), &plan)?;
    Ok((header, prefix, skips))
}

/// A piece of the plaintext: bytes made here, or a file of the snapshot.
enum Piece {
    Bytes(Vec<u8>),
    File(String, u64),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Start,
    Manifest,
    Files,
    End,
    Done,
}

enum Cur {
    None,
    Bytes(Vec<u8>, usize),
    File(File, u64),
}

/// The plaintext, from a given byte on.
struct Plain<'a> {
    snap: &'a Snapshot,
    vault: &'a Vault,
    stage: Stage,
    entries: Option<vault::OpenReader<BufReader<File>>>,
    /// A file whose entry was just given: its bytes come next.
    file_next: Option<(String, u64)>,
    cur: Cur,
    /// Bytes still to pass over before the first one given.
    skip: u64,
}

impl<'a> Plain<'a> {
    fn new(snap: &'a Snapshot, vault: &'a Vault, from: u64) -> Result<Plain<'a>, Error> {
        Ok(Plain { snap, vault, stage: Stage::Start, entries: None, file_next: None, cur: Cur::None, skip: from })
    }

    fn open_entries(&self) -> io::Result<vault::OpenReader<BufReader<File>>> {
        let path = self.snap.dir.join(ENTRIES);
        let f = File::open(&path)?;
        let len = f.metadata()?.len();
        let label = format!("{SNAPSHOTS}/{}/{ENTRIES}", self.snap.header.id);
        self.vault.open_reader(&label, BufReader::with_capacity(1 << 16, f), len).map_err(io::Error::other)
    }

    fn next_entry(&mut self) -> io::Result<Option<Entry>> {
        match self.entries.as_mut() {
            Some(r) => Entry::read_from(r),
            None => Ok(None),
        }
    }

    fn next_piece(&mut self) -> io::Result<Option<Piece>> {
        let files = self.snap.header.files;
        loop {
            match self.stage {
                Stage::Start => {
                    let mut b = PLAIN_MAGIC.to_vec();
                    b.extend_from_slice(&PLAIN_FORMAT.to_be_bytes());
                    put_varint(&mut b, files);
                    self.entries = Some(self.open_entries()?);
                    self.stage = Stage::Manifest;
                    return Ok(Some(Piece::Bytes(b)));
                }
                Stage::Manifest => match self.next_entry()? {
                    Some(e) => {
                        let mut b = Vec::with_capacity(64);
                        e.encode(&mut b);
                        return Ok(Some(Piece::Bytes(b)));
                    }
                    None => {
                        self.entries = Some(self.open_entries()?);
                        self.stage = Stage::Files;
                    }
                },
                Stage::Files => {
                    if let Some((path, len)) = self.file_next.take() {
                        return Ok(Some(Piece::File(path, len)));
                    }
                    match self.next_entry()? {
                        Some(e) => {
                            let mut b = Vec::with_capacity(64);
                            e.encode(&mut b);
                            self.file_next = Some((e.path, e.len));
                            return Ok(Some(Piece::Bytes(b)));
                        }
                        None => {
                            self.entries = None;
                            self.stage = Stage::End;
                        }
                    }
                }
                Stage::End => {
                    let mut b = END_MAGIC.to_vec();
                    put_varint(&mut b, files);
                    self.stage = Stage::Done;
                    return Ok(Some(Piece::Bytes(b)));
                }
                Stage::Done => return Ok(None),
            }
        }
    }
}

impl Read for Plain<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            match &mut self.cur {
                Cur::Bytes(b, pos) if *pos < b.len() => {
                    let n = (b.len() - *pos).min(out.len());
                    out[..n].copy_from_slice(&b[*pos..*pos + n]);
                    *pos += n;
                    return Ok(n);
                }
                Cur::File(f, left) if *left > 0 => {
                    let want = (*left).min(out.len() as u64) as usize;
                    let n = f.read(&mut out[..want])?;
                    if n == 0 {
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "a file of the snapshot got shorter"));
                    }
                    *left -= n as u64;
                    return Ok(n);
                }
                _ => {}
            }
            self.cur = Cur::None;
            let Some(piece) = self.next_piece()? else { return Ok(0) };
            match piece {
                Piece::Bytes(b) => {
                    if self.skip >= b.len() as u64 {
                        self.skip -= b.len() as u64;
                        continue;
                    }
                    let pos = self.skip as usize;
                    self.skip = 0;
                    self.cur = Cur::Bytes(b, pos);
                }
                Piece::File(path, len) => {
                    if self.skip >= len {
                        self.skip -= len;
                        continue;
                    }
                    let mut f = File::open(self.snap.dir.join(FILES).join(&path))?;
                    f.seek(SeekFrom::Start(self.skip))?;
                    self.cur = Cur::File(f, len - self.skip);
                    self.skip = 0;
                }
            }
        }
    }
}

/// The export file's bytes from a given byte on ([`Snapshot::reader`]).
pub struct ExportReader<'a> {
    prefix: &'a [u8],
    prefix_pos: usize,
    plain: Plain<'a>,
    chunks: vault::Chunks,
    next: u64,
    count: u64,
    /// The last chunk's plaintext bytes.
    last: usize,
    buf: Vec<u8>,
    out: Vec<u8>,
    out_pos: usize,
    /// Bytes of the next sealed chunk to pass over (a start in the middle of a chunk).
    skip: usize,
}

impl Read for ExportReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.prefix_pos < self.prefix.len() {
            let n = (self.prefix.len() - self.prefix_pos).min(out.len());
            out[..n].copy_from_slice(&self.prefix[self.prefix_pos..self.prefix_pos + n]);
            self.prefix_pos += n;
            return Ok(n);
        }
        loop {
            if self.out_pos < self.out.len() {
                let n = (self.out.len() - self.out_pos).min(out.len());
                out[..n].copy_from_slice(&self.out[self.out_pos..self.out_pos + n]);
                self.out_pos += n;
                return Ok(n);
            }
            if self.next >= self.count {
                return Ok(0);
            }
            let last = self.next + 1 == self.count;
            let want = if last { self.last } else { CHUNK };
            self.plain.read_exact(&mut self.buf[..want])?;
            self.out = self.chunks.seal(self.next, last, &self.buf[..want]).map_err(io::Error::other)?;
            self.out_pos = self.skip.min(self.out.len());
            self.skip = 0;
            self.next += 1;
        }
    }
}
