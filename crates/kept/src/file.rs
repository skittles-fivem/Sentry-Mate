//! The sealed file a pack is written to and read from:
//!
//! ```text
//! "SMKPK" 0x01 | stream id (16 random bytes) | chunk 0 | chunk 1 | ... | last chunk
//! ```
//!
//! The chunks are the vault's ([`vault::Chunks`]): 64 KiB of the pack's bytes each (the last one what is left),
//! sealed one by one under the file's label, each bound to its number and to whether it is the last. So a chunk
//! changed, moved, dropped or taken from another file doesn't open, and a file cut short is found. The same bytes at
//! the same place seal to the same chunk, and other bytes there to another one under another nonce, which is what
//! lets a pack that was stopped half-way be carried on: the chunks on disk are read, the file is cut after the last
//! whole block in them, and writing goes on from there (the chunk that block ends in is sealed again, longer).

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use vault::chunks::{MAX, OVERHEAD};
use vault::{Chunks, Vault};

pub const MAGIC: &[u8; 6] = b"SMKPK\x01";
pub(crate) const HEADER: u64 = MAGIC.len() as u64 + 16;
const FULL: u64 = (MAX + OVERHEAD) as u64;

fn damaged(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

/// A new file only its owner reads.
fn private_create(path: &Path) -> io::Result<File> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true).write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

fn read_header(file: &mut File, vault: &Vault, label: &str) -> io::Result<Chunks> {
    let mut head = [0u8; HEADER as usize];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut head).map_err(|_| damaged("not a pack: it has no header"))?;
    if &head[..MAGIC.len()] != MAGIC {
        return Err(damaged("not a pack"));
    }
    let mut id = [0u8; 16];
    id.copy_from_slice(&head[MAGIC.len()..]);
    Ok(vault.chunks(label, &id))
}

/// Writes a pack's bytes, sealing each 64 KiB as it fills.
pub(crate) struct Writer {
    file: File,
    chunks: Chunks,
    index: u64,
    buf: Vec<u8>,
}

impl Writer {
    pub fn create(vault: &Vault, path: &Path, label: &str) -> io::Result<Writer> {
        let mut id = [0u8; 16];
        getrandom::fill(&mut id).map_err(|e| io::Error::other(e.to_string()))?;
        let mut file = private_create(path)?;
        file.write_all(MAGIC)?;
        file.write_all(&id)?;
        Ok(Writer { file, chunks: vault.chunks(label, &id), index: 0, buf: Vec::with_capacity(MAX) })
    }

    fn seal(&mut self, last: bool) -> io::Result<()> {
        let n = self.buf.len().min(MAX);
        let sealed = self.chunks.seal(self.index, last, &self.buf[..n]).map_err(|e| io::Error::other(e.to_string()))?;
        self.file.write_all(&sealed)?;
        self.index += 1;
        self.buf.drain(..n);
        Ok(())
    }

    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.buf.extend_from_slice(data);
        // a full chunk is sealed once more bytes follow it, so the last chunk is the only short one
        while self.buf.len() > MAX {
            self.seal(false)?;
        }
        Ok(())
    }

    /// Writes what is buffered as a short chunk that is not the last, so a writer asked to stop loses nothing:
    /// the next one reads it, cuts it off and writes on ([`scan`], [`Resume::writer`]).
    pub fn pause(mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            self.seal(false)?;
        }
        Ok(())
    }

    /// Seals the last chunk and syncs the file.
    pub fn finish(mut self) -> io::Result<()> {
        self.seal(true)?;
        self.file.sync_all()
    }
}

/// What a file that was being written holds.
pub(crate) enum Found {
    /// Not a pack's start: to be written anew.
    Nothing,
    /// Every chunk, the last one too.
    Whole,
    /// Whole chunks up to somewhere: their bytes were handed over, and writing goes on after `keep` of them.
    Part(Resume),
}

pub(crate) struct Resume {
    file: File,
    chunks: Chunks,
}

impl Resume {
    /// Cuts the file after the first `keep` bytes of the stream (whose last chunk's bytes so far are `tail`) and
    /// writes on from there.
    pub fn writer(mut self, keep: u64, tail: Vec<u8>) -> io::Result<Writer> {
        let index = keep / MAX as u64;
        debug_assert_eq!(tail.len() as u64, keep % MAX as u64);
        self.file.set_len(HEADER + index * FULL)?;
        self.file.seek(SeekFrom::End(0))?;
        Ok(Writer { file: self.file, chunks: self.chunks, index, buf: tail })
    }
}

/// Reads the whole chunks of a file that was being written, handing each chunk's bytes to `each` in order, up to
/// the first that doesn't open.
pub(crate) fn scan(
    vault: &Vault,
    path: &Path,
    label: &str,
    mut each: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<Found> {
    let mut file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let len = file.metadata()?.len();
    let Ok(chunks) = read_header(&mut file, vault, label) else { return Ok(Found::Nothing) };
    let body = len - HEADER;
    let mut sealed = vec![0u8; FULL as usize];
    let mut whole = false;
    for index in 0..body.div_ceil(FULL) {
        let n = (body - index * FULL).min(FULL) as usize;
        if file.read_exact(&mut sealed[..n]).is_err() {
            break;
        }
        let on_disk_last = (index + 1) * FULL >= body;
        // the last one on disk is the file's last chunk only when the writer got to its end; a short one that is
        // not is what a writer asked to stop left ([`Writer::pause`])
        let plain = match chunks.open(index, false, &sealed[..n]) {
            Ok(p) if n == FULL as usize || on_disk_last => p,
            _ if on_disk_last => match chunks.open(index, true, &sealed[..n]) {
                Ok(p) => {
                    whole = true;
                    p
                }
                Err(_) => break,
            },
            _ => break,
        };
        each(&plain)?;
    }
    Ok(if whole { Found::Whole } else { Found::Part(Resume { file, chunks }) })
}

/// Reads a finished pack file's bytes in order, checking every chunk.
pub(crate) struct ChunkReader {
    file: BufReader<File>,
    chunks: Chunks,
    count: u64,
    last: usize,
    index: u64,
    plain: Vec<u8>,
    pos: usize,
    sealed: Vec<u8>,
}

impl ChunkReader {
    pub fn open(vault: &Vault, path: &Path, label: &str) -> io::Result<ChunkReader> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        let chunks = read_header(&mut file, vault, label)?;
        let (count, last) =
            vault::chunks::chunks_of(len - HEADER).ok_or_else(|| damaged("it is cut short (not a whole pack)"))?;
        // its last chunk says it is the last: a file cut short is known before any of it is read
        let mut sealed = vec![0u8; FULL as usize];
        let n = last + OVERHEAD;
        file.seek(SeekFrom::Start(HEADER + (count - 1) * FULL))?;
        file.read_exact(&mut sealed[..n])?;
        if chunks.open(count - 1, true, &sealed[..n]).is_err() {
            return Err(damaged("it is cut short, or its end doesn't open"));
        }
        file.seek(SeekFrom::Start(HEADER))?;
        Ok(ChunkReader {
            file: BufReader::with_capacity(FULL as usize, file),
            chunks,
            count,
            last,
            index: 0,
            plain: Vec::new(),
            pos: 0,
            sealed,
        })
    }
}

impl Read for ChunkReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.plain.len() {
            if self.index >= self.count {
                return Ok(0);
            }
            let last = self.index + 1 == self.count;
            let n = if last { self.last + OVERHEAD } else { FULL as usize };
            self.file.read_exact(&mut self.sealed[..n])?;
            let plain = self
                .chunks
                .open(self.index, last, &self.sealed[..n])
                .map_err(|_| damaged(&format!("chunk {} of {} doesn't open", self.index + 1, self.count)))?;
            self.plain.clear();
            self.plain.extend_from_slice(&plain);
            self.pos = 0;
            self.index += 1;
        }
        let n = out.len().min(self.plain.len() - self.pos);
        out[..n].copy_from_slice(&self.plain[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}
