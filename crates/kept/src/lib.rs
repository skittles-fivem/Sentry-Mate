//! The files an import keeps (the upload itself: a TeslaMate dump, TeslaFi's CSV files), packed as small as they go
//! and given back byte for byte (docs/plans/storage.md, "The kept uploads of imports").
//!
//! An upload is stored as it arrives, in the vault's sealed records. Once its import is over nothing reads it for
//! a long time, and a dump is text that packs well: [`pack`] writes it again as a **pack**, beside it, and removes
//! the upload only after the pack has been read back from the disk and compared with it, byte for byte.
//!
//! A pack is the upload's bytes cut into blocks, each packed on its own:
//!
//! ```text
//! block: 'B' | how (1 byte) | bytes of text (4, big endian) | bytes that follow (4, big endian) | the packed block
//! end:   'E' | blocks (8) | bytes of text in all (8) | SHA-256 of the whole upload (32)
//! ```
//!
//! The first block is the upload's first 64 KiB, so telling what kind of file it is reads little; the others are
//! about 4 MiB, cut at a line's end. `how` is 2 for a block whose rows were turned into columns ([`text`]) and then
//! compressed (brotli, quality 11), 1 for one compressed as it is, 0 for one stored as it is (what doesn't get
//! smaller). A block turned into columns is turned back and compared before it is used; one that doesn't come back
//! the same is compressed as it is. So a pack takes memory for a block, however long the upload is, a block that
//! doesn't open is known by its number, and reading ends with the count, the length and the SHA-256 checked.
//!
//! The pack's bytes are sealed by the vault under the upload's label with `.pack` added, in the vault's chunks of
//! 64 KiB (`"SMKPK" 0x01`, a stream id, the chunks: `src/file.rs`). Chunks, not the vault's sealed stream, because a
//! chunk sealed again at the same place is safe: a pack of many GB that was stopped is carried on from its last
//! whole block, not started again. A move or an export carries a pack as any file; the temporary file (`*.tmp`) is
//! left behind, as all temporary files are.
//!
//! Uploads that are packed already (gzip, xz, zip, zstd, bzip2) are left as they are: their bytes would have to be
//! made again bit for bit by the packer that made them, which nothing here can promise (measured: neither this
//! build's deflate at any level nor its xz at any preset gives back what `gzip` and `xz` wrote, see
//! `examples/measure.rs`), and a recipe that works today would still depend on a packer never changing.
//!
//! What was measured, and why this way won: docs/plans/storage.md.

mod file;
mod pack;
pub mod text;

pub use file::MAGIC;
pub use pack::{Error, Options, Outcome, Stop, pack, packed_label, packed_path, temporary_path};

use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::path::Path;
use vault::Vault;

const STORED: u8 = 0;
const BROTLI: u8 = 1;
const COLUMNS: u8 = 2;
/// The upload's first block: what a look at its start reads.
pub const FIRST_BLOCK: usize = 64 << 10;
/// The other blocks, about (each ends at a line's end when it has one).
pub const BLOCK: usize = 4 << 20;
/// No block read is larger than this.
const MAX_BLOCK: usize = 64 << 20;
const BLOCK_HEAD: usize = 10;
const END_LEN: usize = 48;

fn brotli_pack(data: &[u8]) -> io::Result<Vec<u8>> {
    let params =
        brotli::enc::BrotliEncoderParams { quality: 11, lgwin: 24, size_hint: data.len(), ..Default::default() };
    let mut out = Vec::with_capacity(data.len() / 4 + 64);
    brotli::BrotliCompress(&mut &data[..], &mut out, &params)?;
    Ok(out)
}

fn brotli_unpack(b: &[u8], max: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(max.min(b.len().saturating_mul(8)));
    brotli::Decompressor::new(b, 1 << 16).take(max as u64 + 1).read_to_end(&mut out)?;
    if out.len() > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "it unpacks to more than it says"));
    }
    Ok(out)
}

/// One block of text, packed: how, and its bytes.
fn pack_block(block: &[u8]) -> io::Result<(u8, Vec<u8>)> {
    let streams = text::encode(block);
    // only columns that give the text back are used
    let exact = text::decode(&streams, block.len()).is_ok_and(|back| back == block);
    let (how, body) = if exact { (COLUMNS, brotli_pack(&streams)?) } else { (BROTLI, brotli_pack(block)?) };
    Ok(if body.len() < block.len() { (how, body) } else { (STORED, block.to_vec()) })
}

fn unpack_block(how: u8, len: usize, body: &[u8]) -> io::Result<Vec<u8>> {
    let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_string());
    let out = match how {
        STORED => body.to_vec(),
        BROTLI => brotli_unpack(body, len)?,
        COLUMNS => {
            // the streams of a block are never much larger than its text
            let streams = brotli_unpack(body, len.saturating_mul(4) + (1 << 16))?;
            text::decode(&streams, len).map_err(|e| bad(e.0))?
        }
        _ => return Err(bad("packed in a way this version doesn't know")),
    };
    if out.len() != len {
        return Err(bad("not the length it says"));
    }
    Ok(out)
}

fn block_head(how: u8, plain: usize, body: usize) -> [u8; BLOCK_HEAD] {
    let mut h = [0u8; BLOCK_HEAD];
    h[0] = b'B';
    h[1] = how;
    h[2..6].copy_from_slice(&(plain as u32).to_be_bytes());
    h[6..10].copy_from_slice(&(body as u32).to_be_bytes());
    h
}

fn be32(b: &[u8]) -> usize {
    b.iter().fold(0usize, |v, x| v << 8 | usize::from(*x))
}

fn be64(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |v, x| v << 8 | u64::from(*x))
}

/// Reads a pack: the upload's bytes, exactly, in order. An error names the file and the block that doesn't open;
/// the end of the bytes is given only after the pack's own count, length and SHA-256 were found right.
pub struct Reader {
    src: file::ChunkReader,
    label: String,
    block: Vec<u8>,
    pos: usize,
    blocks: u64,
    bytes: u64,
    sha: Sha256,
    done: bool,
    failed: Option<String>,
}

impl Reader {
    /// Opens the pack at `path`, sealed under `label` ([`packed_path`], [`packed_label`]).
    pub fn open(vault: &Vault, path: &Path, label: &str) -> io::Result<Reader> {
        let src = file::ChunkReader::open(vault, path, label).map_err(|e| named(label, None, &e))?;
        let (block, sha) = (Vec::new(), Sha256::new());
        Ok(Reader { src, label: label.to_string(), block, pos: 0, blocks: 0, bytes: 0, sha, done: false, failed: None })
    }

    /// The upload's bytes read so far.
    pub fn position(&self) -> u64 {
        self.bytes - (self.block.len() - self.pos) as u64
    }

    fn next_block(&mut self) -> io::Result<()> {
        let mut head = [0u8; BLOCK_HEAD];
        self.src.read_exact(&mut head[..1])?;
        if head[0] == b'E' {
            let mut end = [0u8; END_LEN];
            self.src.read_exact(&mut end)?;
            let sha = self.sha.clone().finalize();
            if be64(&end[..8]) != self.blocks || be64(&end[8..16]) != self.bytes || end[16..] != sha[..] {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "its blocks are not the upload it says"));
            }
            if self.src.read(&mut head[..1])? != 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "bytes after its end"));
            }
            self.done = true;
            return Ok(());
        }
        self.src.read_exact(&mut head[1..])?;
        let (len, body_len) = (be32(&head[2..6]), be32(&head[6..10]));
        if head[0] != b'B' || len > MAX_BLOCK || body_len > MAX_BLOCK {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a block"));
        }
        let mut body = vec![0u8; body_len];
        self.src.read_exact(&mut body)?;
        self.block = unpack_block(head[1], len, &body)?;
        self.pos = 0;
        self.blocks += 1;
        self.bytes += len as u64;
        self.sha.update(&self.block);
        Ok(())
    }
}

/// An error that says which file, and which block of it.
fn named(label: &str, block: Option<u64>, e: &io::Error) -> io::Error {
    let kind = if e.kind() == io::ErrorKind::NotFound { e.kind() } else { io::ErrorKind::InvalidData };
    let what = match (e.kind(), block) {
        (io::ErrorKind::UnexpectedEof, Some(b)) => format!("{label}: cut short in block {b}"),
        (_, Some(b)) => format!("{label}: block {b} doesn't open: {e}"),
        (_, None) => format!("{label}: {e}"),
    };
    io::Error::new(kind, what)
}

impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.block.len() {
            if self.done {
                return Ok(0);
            }
            // nothing more is given after a block that doesn't open
            if let Some(why) = &self.failed {
                return Err(io::Error::new(io::ErrorKind::InvalidData, why.clone()));
            }
            if let Err(e) = self.next_block() {
                let e = named(&self.label, Some(self.blocks + 1), &e);
                self.failed = Some(e.to_string());
                return Err(e);
            }
        }
        let n = out.len().min(self.block.len() - self.pos);
        out[..n].copy_from_slice(&self.block[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Whether a file's first bytes say it is packed already (gzip, xz, zip, zstd, bzip2): left as uploaded.
pub fn is_packed_already(head: &[u8]) -> bool {
    head.starts_with(&[0x1f, 0x8b])
        || head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00])
        || head.starts_with(b"PK\x03\x04")
        || head.starts_with(b"PK\x05\x06")
        || head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
        || head.starts_with(b"BZh")
}
