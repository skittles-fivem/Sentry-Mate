//! Packing a kept upload in place, safely: the pack is written beside the upload under a temporary name and
//! synced; read back from the disk, unpacked whole and compared with the upload byte for byte (and by SHA-256);
//! renamed into place; and only then is the upload removed. Stopped anywhere (asked to, a crash, a restart), the
//! upload's bytes still read, from the upload or from the pack:
//!
//! - a temporary file is carried on from its last whole block by the next call (or written anew when it doesn't
//!   read as the start of a pack);
//! - when both the upload and its pack are there (stopped between the rename and the removal), the **upload wins**:
//!   it is what is read, and the next call compares the pack with it again before removing it. A pack that is not
//!   the upload is removed and reported, and made again by the call after.

use crate::file::{self, Found};
use crate::{BLOCK, BLOCK_HEAD, END_LEN, FIRST_BLOCK, Reader, be32, block_head, is_packed_already, pack_block};
use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use vault::Vault;

/// Bytes compared between two looks at whether to stop.
const COMPARE_UNIT: u64 = 16 << 20;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Vault(#[from] vault::Error),
    /// The pack named doesn't open, or is not the upload it stands for.
    #[error("{file}: {why}")]
    Damaged { file: String, why: String },
    #[error("stopped where a test asked")]
    TestStop,
}

/// For tests of what a crash leaves: where packing stops (as if the server died there).
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// This many blocks are written to the temporary file (not synced, not finished).
    Blocks(u64),
    /// The temporary file is whole, synced and compared; nothing renamed.
    TempWritten,
    /// The pack is in place; the upload not yet removed.
    Renamed,
}

pub struct Options<'a> {
    /// Asked after every block written and every 16 MiB compared: whether to stop now (the next call carries on).
    pub stop: &'a dyn Fn() -> bool,
    #[doc(hidden)]
    pub stop_at: Option<Stop>,
    /// About how much text goes into a block ([`BLOCK`]; tests take less).
    #[doc(hidden)]
    pub block_bytes: usize,
}

impl<'a> Options<'a> {
    pub fn new(stop: &'a dyn Fn() -> bool) -> Options<'a> {
        Options { stop, stop_at: None, block_bytes: BLOCK }
    }
}

/// What a call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The upload is now its pack: their sizes.
    Packed { before_bytes: u64, after_bytes: u64 },
    /// Left as uploaded: it is packed already (gzip, xz, zip ...).
    AsUploaded,
    /// Stopped when asked; what was written so far is carried on from.
    Stopped,
    /// Nothing to do: it is packed, or there is no such upload.
    Nothing,
}

/// Where an upload's pack is.
pub fn packed_path(upload: &Path) -> PathBuf {
    let mut s = upload.as_os_str().to_os_string();
    s.push(".pack");
    PathBuf::from(s)
}

/// Where it is written before it is in place.
pub fn temporary_path(upload: &Path) -> PathBuf {
    let mut s = upload.as_os_str().to_os_string();
    s.push(".pack.tmp");
    PathBuf::from(s)
}

/// The label a pack is sealed under, from its upload's.
pub fn packed_label(label: &str) -> String {
    format!("{label}.pack")
}

/// An error's words without the file's name in front (the caller says the name).
fn unnamed(label: &str, e: &io::Error) -> String {
    let s = e.to_string();
    s.strip_prefix(label).and_then(|r| r.strip_prefix(": ")).map(String::from).unwrap_or(s)
}

fn remove_if_there(p: &Path) -> io::Result<()> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn sync_dir(file: &Path) {
    if let Some(d) = file.parent()
        && let Ok(d) = std::fs::File::open(d)
    {
        let _ = d.sync_all();
    }
}

/// Why reading the upload ended early.
enum Ended {
    Asked,
    Test,
    Differs(String),
}

fn ended(e: Ended) -> io::Error {
    io::Error::other(Carried(e))
}

struct Carried(Ended);

impl std::fmt::Debug for Carried {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ended early")
    }
}

impl std::fmt::Display for Carried {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ended early")
    }
}

impl std::error::Error for Carried {}

/// Takes an early end back out of the error it travelled in.
fn carried(e: io::Error) -> Result<Ended, io::Error> {
    if e.get_ref().is_some_and(|x| x.is::<Carried>()) {
        match e.into_inner().map(|b| b.downcast::<Carried>()) {
            Some(Ok(c)) => Ok(c.0),
            _ => Err(io::Error::other("ended early")),
        }
    } else {
        Err(e)
    }
}

/// The upload's first bytes.
fn head(vault: &Vault, upload: &Path, label: &str) -> Result<Vec<u8>, Error> {
    let mut head = Vec::new();
    let r = vault.records_read(upload, label)?.read_all(|rec| {
        head.extend_from_slice(&rec[..rec.len().min(8)]);
        Err(ended(Ended::Asked))
    });
    match r.map_err(carried) {
        Ok(()) | Err(Ok(_)) => Ok(head),
        Err(Err(e)) => Err(e.into()),
    }
}

enum Compared {
    Same,
    Stopped,
}

/// Reads the pack at `path` whole and compares it with the upload, byte for byte and by SHA-256.
fn compare(vault: &Vault, upload: &Path, label: &str, path: &Path, o: &Options<'_>) -> Result<Compared, Error> {
    let plabel = packed_label(label);
    let damaged = |why: String| Error::Damaged { file: plabel.clone(), why };
    let damaged = |why: String| damaged(why.strip_prefix(&format!("{plabel}: ")).map(String::from).unwrap_or(why));
    let mut pack = Reader::open(vault, path, &plabel).map_err(|e| damaged(e.to_string()))?;
    let mut buf = vec![0u8; vault::chunks::MAX];
    let (mut sha, mut since) = (Sha256::new(), 0u64);
    let r = vault.records_read(upload, label)?.read_all(|rec| {
        let at = pack.position();
        let mut rest = rec;
        while !rest.is_empty() {
            let n = rest.len().min(buf.len());
            match pack.read_exact(&mut buf[..n]) {
                Ok(()) if buf[..n] == rest[..n] => {}
                Ok(()) => return Err(ended(Ended::Differs(format!("it differs from the upload near byte {at}")))),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return Err(ended(Ended::Differs(format!("it ends before the upload does, near byte {at}"))));
                }
                Err(e) => return Err(ended(Ended::Differs(e.to_string()))),
            }
            rest = &rest[n..];
        }
        sha.update(rec);
        since += rec.len() as u64;
        if since >= COMPARE_UNIT {
            since = 0;
            if (o.stop)() {
                return Err(ended(Ended::Asked));
            }
        }
        Ok(())
    });
    match r.map_err(carried) {
        Ok(()) => {}
        Err(Ok(Ended::Differs(why))) => return Err(damaged(why)),
        Err(Ok(_)) => return Ok(Compared::Stopped),
        Err(Err(e)) => return Err(e.into()),
    }
    // the pack ends where the upload does, and its own count, length and SHA-256 are right
    match pack.read(&mut buf[..1]) {
        Ok(0) => Ok(Compared::Same),
        Ok(_) => Err(damaged("it goes on after the upload's end".into())),
        Err(e) => Err(damaged(e.to_string())),
    }
}

/// What the whole blocks of a temporary file hold: how far into the pack's bytes they go, how many they are, how
/// many of the upload's bytes they stand for, and the bytes of the pack's last chunk up to there.
#[derive(Default)]
struct Blocks {
    end: u64,
    blocks: u64,
    upload_bytes: u64,
    tail: Vec<u8>,
    // while reading: the block's head so far, what is left of its body, and where the reading is
    head: Vec<u8>,
    left: usize,
    at: u64,
    over: bool,
}

impl Blocks {
    fn chunk(&mut self, mut b: &[u8]) {
        let whole = b;
        let from = self.at;
        while !b.is_empty() && !self.over {
            if self.head.len() < BLOCK_HEAD {
                let n = (BLOCK_HEAD - self.head.len()).min(b.len());
                self.head.extend_from_slice(&b[..n]);
                (b, self.at) = (&b[n..], self.at + n as u64);
                if self.head[0] != b'B' {
                    self.over = true;
                } else if self.head.len() == BLOCK_HEAD {
                    self.left = be32(&self.head[6..10]);
                }
            }
            if self.head.len() == BLOCK_HEAD {
                let n = self.left.min(b.len());
                (b, self.at, self.left) = (&b[n..], self.at + n as u64, self.left - n);
                if self.left == 0 {
                    self.blocks += 1;
                    self.upload_bytes += be32(&self.head[2..6]) as u64;
                    self.end = self.at;
                    self.head.clear();
                    // the bytes of the pack's chunk this block ends in, up to its end (chunks are handed over one
                    // by one, each starting where a chunk starts)
                    let chunk_from = self.end - self.end % vault::chunks::MAX as u64;
                    self.tail.clear();
                    if chunk_from >= from {
                        self.tail.extend_from_slice(&whole[(chunk_from - from) as usize..(self.end - from) as usize]);
                    }
                }
            }
        }
    }
}

/// Packs the upload at `upload` (the vault's records, sealed under `label`) into its pack beside it, or carries on
/// where an earlier call stopped. See the module's notes for what each stop leaves.
pub fn pack(vault: &Vault, upload: &Path, label: &str, o: &Options<'_>) -> Result<Outcome, Error> {
    let (packed, tmp) = (packed_path(upload), temporary_path(upload));
    let before_bytes = match std::fs::metadata(upload) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // packed, or gone: a temporary file left behind stands for nothing
            remove_if_there(&tmp)?;
            if packed.exists() {
                // a look at its start (its header, its length, its first block): a pack that doesn't open is said
                // now, not when it is needed. The whole of it is checked by whoever reads it.
                let plabel = packed_label(label);
                let mut first = [0u8; 1];
                Reader::open(vault, &packed, &plabel)
                    .and_then(|mut r| r.read(&mut first))
                    .map_err(|e| Error::Damaged { file: plabel.clone(), why: unnamed(&plabel, &e) })?;
            }
            return Ok(Outcome::Nothing);
        }
        Err(e) => return Err(e.into()),
    };
    let finish = |after_bytes: u64| -> Result<Outcome, Error> {
        if o.stop_at == Some(Stop::Renamed) {
            return Err(Error::TestStop);
        }
        std::fs::remove_file(upload)?;
        sync_dir(upload);
        Ok(Outcome::Packed { before_bytes, after_bytes })
    };
    if let Ok(m) = std::fs::metadata(&packed) {
        // stopped between the rename and the removal: the upload wins until the pack is found to be it
        remove_if_there(&tmp)?;
        return match compare(vault, upload, label, &packed, o) {
            Ok(Compared::Same) => finish(m.len()),
            Ok(Compared::Stopped) => Ok(Outcome::Stopped),
            Err(e @ Error::Damaged { .. }) => {
                std::fs::remove_file(&packed)?;
                sync_dir(&packed);
                Err(e)
            }
            Err(e) => Err(e),
        };
    }
    if is_packed_already(&head(vault, upload, label)?) {
        remove_if_there(&tmp)?;
        return Ok(Outcome::AsUploaded);
    }
    let plabel = packed_label(label);
    // what an earlier call wrote, if it reads as the start of this upload's pack
    let mut had = Blocks::default();
    let found = match std::fs::metadata(&tmp) {
        Ok(_) => file::scan(vault, &tmp, &plabel, |chunk| {
            had.chunk(chunk);
            Ok(())
        })?,
        Err(_) => Found::Nothing,
    };
    let writer = match found {
        Found::Whole => None,
        Found::Part(resume) => Some(resume.writer(had.end, std::mem::take(&mut had.tail))?),
        Found::Nothing => {
            had = Blocks::default();
            Some(file::Writer::create(vault, &tmp, &plabel)?)
        }
    };
    if let Some(mut w) = writer {
        let (mut skip, mut blocks) = (had.upload_bytes, had.blocks);
        let (mut sha, mut bytes) = (Sha256::new(), 0u64);
        let block_bytes = o.block_bytes.clamp(1, crate::MAX_BLOCK);
        let mut pending: Vec<u8> = Vec::with_capacity(block_bytes + vault::chunks::MAX);
        let emit = |w: &mut file::Writer, block: &[u8], blocks: &mut u64| -> io::Result<()> {
            let (how, body) = pack_block(block)?;
            w.write(&block_head(how, block.len(), body.len()))?;
            w.write(&body)?;
            *blocks += 1;
            if o.stop_at == Some(Stop::Blocks(*blocks)) {
                return Err(ended(Ended::Test));
            }
            if (o.stop)() {
                return Err(ended(Ended::Asked));
            }
            Ok(())
        };
        let r = vault.records_read(upload, label)?.read_all(|rec| {
            sha.update(rec);
            bytes += rec.len() as u64;
            // what the blocks already written stand for is only read
            let n = skip.min(rec.len() as u64) as usize;
            skip -= n as u64;
            pending.extend_from_slice(&rec[n..]);
            loop {
                let target = if blocks == 0 { FIRST_BLOCK.min(block_bytes) } else { block_bytes };
                if pending.len() < target {
                    return Ok(());
                }
                // at a line's end, when the block has one
                let cut = pending[..target].iter().rposition(|b| *b == b'\n').map_or(target, |p| p + 1);
                emit(&mut w, &pending[..cut], &mut blocks)?;
                pending.drain(..cut);
            }
        });
        let r = match r {
            Ok(()) if skip > 0 => Err(io::Error::other("the upload is shorter than what was packed of it")),
            Ok(()) if !pending.is_empty() => emit(&mut w, &pending, &mut blocks),
            r => r,
        };
        match r.map_err(carried) {
            Ok(()) => {}
            // as a crash: what was not yet written out is lost
            Err(Ok(Ended::Test)) => return Err(Error::TestStop),
            Err(Ok(_)) => {
                w.pause()?;
                return Ok(Outcome::Stopped);
            }
            Err(Err(e)) => return Err(e.into()),
        }
        let mut end = [0u8; 1 + END_LEN];
        end[0] = b'E';
        end[1..9].copy_from_slice(&blocks.to_be_bytes());
        end[9..17].copy_from_slice(&bytes.to_be_bytes());
        end[17..].copy_from_slice(&sha.finalize());
        w.write(&end)?;
        w.finish()?;
    }
    // read back from the disk and compared before it stands for the upload
    match compare(vault, upload, label, &tmp, o) {
        Ok(Compared::Same) => {}
        Ok(Compared::Stopped) => return Ok(Outcome::Stopped),
        Err(e) => {
            if matches!(e, Error::Damaged { .. }) {
                remove_if_there(&tmp)?;
            }
            return Err(e);
        }
    }
    if o.stop_at == Some(Stop::TempWritten) {
        return Err(Error::TestStop);
    }
    let after_bytes = std::fs::metadata(&tmp)?.len();
    std::fs::rename(&tmp, &packed)?;
    sync_dir(&packed);
    finish(after_bytes)
}
