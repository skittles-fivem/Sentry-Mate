//! Sealed files: XChaCha20-Poly1305 in 64 KiB chunks (the STREAM construction).
//!
//! ```text
//! "SMENC" 0x01 | file id (16 random bytes) | chunk 0 | chunk 1 | ... | last chunk
//! chunk = ciphertext of up to 64 KiB + 16-byte tag
//! ```
//!
//! Each file gets its own key, derived from the data key and its random file id (HKDF-SHA256). A chunk's nonce is
//! its number and whether it is the last one, so chunks can't be reordered, dropped or the file cut short without
//! it being found. The file's label (its path under the data folder) is authenticated with every chunk, so a sealed
//! file moved or copied under another name doesn't open.

use crate::{Error, Vault};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use std::io::{self, Read, Write};
use zeroize::Zeroizing;

pub const MAGIC: &[u8; 6] = b"SMENC\x01";
pub const CHUNK: usize = 64 * 1024;
pub const TAG: usize = 16;
pub const HEADER: usize = MAGIC.len() + 16;

pub(crate) fn nonce(counter: u64, flag: u8) -> XNonce {
    let mut n = [0u8; 24];
    n[..8].copy_from_slice(&counter.to_be_bytes());
    n[8] = flag;
    XNonce::from(n)
}

/// How many plaintext bytes a sealed file of `sealed_len` bytes holds (None: not a valid length).
pub fn plaintext_len(sealed_len: u64) -> Option<u64> {
    let body = sealed_len.checked_sub(HEADER as u64)?;
    if body < TAG as u64 {
        return None;
    }
    let chunks = body.div_ceil((CHUNK + TAG) as u64);
    Some(body - chunks * TAG as u64)
}

/// Whether bytes start like a sealed file.
pub fn is_sealed(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

/// Writes a sealed file: write the plaintext, then [`SealWriter::finish`] (without it the file is incomplete and
/// will not open).
pub struct SealWriter<W: Write> {
    out: W,
    cipher: XChaCha20Poly1305,
    label: Vec<u8>,
    buf: Zeroizing<Vec<u8>>,
    counter: u64,
}

impl<W: Write> SealWriter<W> {
    pub(crate) fn new(vault: &Vault, label: &str, mut out: W) -> Result<SealWriter<W>, Error> {
        let mut id = [0u8; 16];
        getrandom::fill(&mut id).map_err(|e| Error::Random(e.to_string()))?;
        out.write_all(MAGIC)?;
        out.write_all(&id)?;
        let key = vault.file_key(b"sentry-mate/stream/v1", &id);
        Ok(SealWriter {
            out,
            cipher: XChaCha20Poly1305::new((&*key).into()),
            label: label.as_bytes().to_vec(),
            buf: Zeroizing::new(Vec::with_capacity(CHUNK)),
            counter: 0,
        })
    }

    fn seal_chunk(&mut self, last: bool) -> io::Result<()> {
        let ct = self
            .cipher
            .encrypt(&nonce(self.counter, u8::from(last)), Payload { msg: &self.buf, aad: &self.label })
            .map_err(|_| io::Error::other("encryption failed"))?;
        self.out.write_all(&ct)?;
        self.counter += 1;
        self.buf.clear();
        Ok(())
    }

    /// Seals the last chunk and returns the inner writer.
    pub fn finish(mut self) -> io::Result<W> {
        self.seal_chunk(true)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

impl<W: Write> Write for SealWriter<W> {
    fn write(&mut self, mut data: &[u8]) -> io::Result<usize> {
        let n = data.len();
        while !data.is_empty() {
            // a full chunk is sealed only once more data comes, so the last chunk is never empty unless the file is
            if self.buf.len() == CHUNK {
                self.seal_chunk(false)?;
            }
            let take = (CHUNK - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Reads a sealed file's plaintext, checking every chunk. `sealed_len` is the file's whole size.
pub struct OpenReader<R: Read> {
    input: R,
    cipher: XChaCha20Poly1305,
    label: Vec<u8>,
    chunks: u64,
    counter: u64,
    plain: Zeroizing<Vec<u8>>,
    pos: usize,
    sealed: Vec<u8>,
}

impl<R: Read> OpenReader<R> {
    pub(crate) fn new(vault: &Vault, label: &str, mut input: R, sealed_len: u64) -> Result<OpenReader<R>, Error> {
        plaintext_len(sealed_len).ok_or(Error::NotSealed)?;
        let mut head = [0u8; HEADER];
        input.read_exact(&mut head).map_err(|_| Error::NotSealed)?;
        if !is_sealed(&head) {
            return Err(Error::NotSealed);
        }
        let key = vault.file_key(b"sentry-mate/stream/v1", &head[MAGIC.len()..]);
        let body = sealed_len - HEADER as u64;
        Ok(OpenReader {
            input,
            cipher: XChaCha20Poly1305::new((&*key).into()),
            label: label.as_bytes().to_vec(),
            chunks: body.div_ceil((CHUNK + TAG) as u64),
            counter: 0,
            plain: Zeroizing::new(Vec::new()),
            pos: 0,
            sealed: vec![0u8; CHUNK + TAG],
        })
    }

    fn next_chunk(&mut self) -> io::Result<bool> {
        if self.counter >= self.chunks {
            return Ok(false);
        }
        let last = self.counter + 1 == self.chunks;
        let mut got = 0;
        while got < self.sealed.len() {
            let n = self.input.read(&mut self.sealed[got..])?;
            if n == 0 {
                break;
            }
            got += n;
        }
        if got < TAG || (!last && got != CHUNK + TAG) {
            return Err(io::Error::other(Error::Damaged));
        }
        let pt = self
            .cipher
            .decrypt(&nonce(self.counter, u8::from(last)), Payload { msg: &self.sealed[..got], aad: &self.label })
            .map_err(|_| io::Error::other(Error::Damaged))?;
        self.plain = Zeroizing::new(pt);
        self.pos = 0;
        self.counter += 1;
        Ok(true)
    }
}

impl<R: Read> Read for OpenReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.plain.len() {
            if !self.next_chunk()? {
                return Ok(0);
            }
        }
        let n = (self.plain.len() - self.pos).min(out.len());
        out[..n].copy_from_slice(&self.plain[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}
