//! Append-only journals: a growing sealed file ([`vault::Records`]) of entries, each one change (a batch of
//! upserts and deletes, a window of points replaced), synced as it is added.
//!
//! ```text
//! record = flags | piece      (one vault record, at most 64 KiB)
//! flags  = 1 first piece of an entry, 2 last piece (3: an entry in one piece)
//! entry 0 = "SMDB" | kind 2 | format version | generation (u64, big endian)
//! ```
//!
//! An entry is one write, so a crash leaves at most its last entry half written: that entry is dropped when the
//! journal is read (it was never acknowledged). A record damaged anywhere else is reported, never cut off: the
//! vault drops a torn last record on its own, so the records are walked first, and anything after the last whole
//! record that is neither a torn record (right length, bytes missing) nor zeros is damage. (A damaged length that
//! happens to point past the end looks torn and loses the records after it: the one case not told apart.)

use crate::error::{Error, Result};
use crate::file::{Ctx, Kind, check_header, header, private_create, remove_if_exists, sync_dir, with_suffix};
use std::path::{Path, PathBuf};
use vault::stream::{CHUNK, TAG};

const FIRST: u8 = 1;
const LAST: u8 = 2;
const PIECE: usize = CHUNK - 1;
/// The vault's records header: magic and file id.
const RECORDS_HEADER: usize = vault::records::MAGIC.len() + 16;

pub(crate) struct Journal {
    rec: vault::Records,
    /// Payload bytes of the entries (not the header).
    pub bytes: u64,
    pub entries: u64,
}

fn damaged(file: &str, why: &str) -> Error {
    Error::Damaged { file: file.to_string(), why: why.to_string() }
}

/// Refuses a journal whose records stop making sense before its end in a way a crash can't explain. A crash
/// leaves a torn last record (its length is right, bytes are missing) or zeros where the system had not written
/// yet; any other unreadable length is damage, reported instead of letting the vault cut the rest off.
fn check_tail(path: &Path, label: &str) -> Result<()> {
    let b = std::fs::read(path)?;
    if b.len() < RECORDS_HEADER {
        return Err(damaged(label, "too short to be a journal"));
    }
    if !b.starts_with(vault::records::MAGIC) {
        return Err(damaged(label, "not a sealed journal"));
    }
    let mut pos = RECORDS_HEADER;
    while pos + 4 <= b.len() {
        let n = u32::from_be_bytes([b[pos], b[pos + 1], b[pos + 2], b[pos + 3]]) as usize;
        if !(TAG..=CHUNK + TAG).contains(&n) {
            if b[pos..].iter().any(|&x| x != 0) {
                return Err(damaged(label, "a record before its end doesn't read"));
            }
            break;
        }
        if pos + 4 + n > b.len() {
            // torn by a crash: the vault drops it
            break;
        }
        pos += 4 + n;
    }
    Ok(())
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + payload.len() / PIECE + 1);
    let pieces: Vec<&[u8]> = if payload.is_empty() { vec![&[][..]] } else { payload.chunks(PIECE).collect() };
    let last = pieces.len() - 1;
    for (i, p) in pieces.into_iter().enumerate() {
        out.push(if i == 0 { FIRST } else { 0 } | if i == last { LAST } else { 0 });
        // every piece but the last fills a vault record exactly, so records and pieces line up
        out.extend_from_slice(p);
    }
    out
}

/// What reading a journal found.
pub(crate) struct Opened {
    pub journal: Journal,
    pub generation: u64,
    /// The entries after the header, in order (only filled by [`Journal::open`]).
    pub entries: Vec<Vec<u8>>,
}

impl Journal {
    /// Starts a journal for generation `generation`: made under a temporary name (`<file>.new`, holding no data), then
    /// renamed, so a journal file always has its header.
    pub(crate) fn create(ctx: &Ctx, path: &Path, generation: u64) -> Result<Journal> {
        let label = ctx.label(path);
        let dir = path.parent().ok_or_else(|| Error::invalid("a journal needs a folder"))?;
        std::fs::create_dir_all(dir)?;
        let tmp = with_suffix(path, ".new");
        remove_if_exists(&tmp)?;
        // made here, owner-only like every other db file (the vault would make it with the default mode)
        drop(private_create(&tmp)?);
        let mut rec = ctx.vault.records(&tmp, &label)?;
        let mut head = header(Kind::Journal);
        head.extend_from_slice(&generation.to_be_bytes());
        rec.append(&frame(&head))?;
        std::fs::rename(&tmp, path)?;
        sync_dir(dir);
        Ok(Journal { rec, bytes: 0, entries: 0 })
    }

    /// Opens a journal and reads all of it. `Ok(None)`: there is none.
    pub(crate) fn open(ctx: &Ctx, path: &Path) -> Result<Option<Opened>> {
        Journal::read(ctx, path, false)
    }

    /// Opens a journal to add to it, reading only its header.
    pub(crate) fn open_for_append(ctx: &Ctx, path: &Path) -> Result<Option<Opened>> {
        Journal::read(ctx, path, true)
    }

    fn read(ctx: &Ctx, path: &Path, header_only: bool) -> Result<Option<Opened>> {
        if !path.exists() {
            return Ok(None);
        }
        let label = ctx.label(path);
        check_tail(path, &label)?;
        let mut rec = ctx.vault.records(path, &label).map_err(|e| Error::from(e).in_file(&label))?;
        let mut entries: Vec<Vec<u8>> = Vec::new();
        let mut cur: Option<Vec<u8>> = None;
        let mut bad: Option<&'static str> = None;
        let stop = std::io::ErrorKind::Interrupted;
        let r = rec.read_all(|piece| {
            let Some((&flags, body)) = piece.split_first() else {
                bad = Some("an empty record");
                return Err(std::io::Error::other("bad"));
            };
            if flags & !(FIRST | LAST) != 0 {
                bad = Some("bad entry flags");
                return Err(std::io::Error::other("bad"));
            }
            if flags & FIRST != 0 {
                // an entry still open here was cut by a crash and never acknowledged: dropped
                cur = Some(body.to_vec());
            } else {
                match cur.as_mut() {
                    Some(c) => c.extend_from_slice(body),
                    None => {
                        bad = Some("a piece of an entry without its start");
                        return Err(std::io::Error::other("bad"));
                    }
                }
            }
            if flags & LAST != 0 {
                entries.extend(cur.take());
                if header_only {
                    return Err(std::io::Error::new(stop, "header read"));
                }
            }
            Ok(())
        });
        if let Some(why) = bad {
            return Err(damaged(&label, why));
        }
        match r {
            Err(e) if e.kind() == stop && header_only => {}
            Err(e) => return Err(Error::from(vault::unwrap_io(e)).in_file(&label)),
            Ok(()) => {}
        }
        if entries.is_empty() {
            return Err(damaged(&label, "no header"));
        }
        let head = entries.remove(0);
        let body = check_header(&head, Kind::Journal, &label)?;
        let generation = body
            .get(..8)
            .and_then(|g| <[u8; 8]>::try_from(g).ok())
            .map(u64::from_be_bytes)
            .ok_or_else(|| damaged(&label, "bad header"))?;
        let bytes = if header_only { rec.plain_len() } else { entries.iter().map(|e| e.len() as u64).sum() };
        let journal = Journal { rec, bytes, entries: entries.len() as u64 };
        Ok(Some(Opened { journal, generation, entries }))
    }

    /// Adds an entry (synced before this returns).
    pub(crate) fn append(&mut self, payload: &[u8]) -> Result<()> {
        self.rec.append(&frame(payload))?;
        self.bytes += payload.len() as u64;
        self.entries += 1;
        Ok(())
    }
}

/// A file pair: the snapshot and its journal.
#[derive(Debug, Clone)]
pub(crate) struct Files {
    pub snap: PathBuf,
    pub log: PathBuf,
}

impl Files {
    /// `<dir>/<name>.snap` and `<dir>/<name>.log`.
    pub(crate) fn named(dir: &Path, name: &str) -> Files {
        Files { snap: dir.join(format!("{name}.snap")), log: dir.join(format!("{name}.log")) }
    }

    /// Removes what a crash may have left: a snapshot's temporary file, a journal never renamed into place.
    pub(crate) fn clean_temporary(&self) -> Result<()> {
        remove_if_exists(&with_suffix(&self.snap, ".tmp"))?;
        remove_if_exists(&with_suffix(&self.log, ".new"))?;
        Ok(())
    }
}
