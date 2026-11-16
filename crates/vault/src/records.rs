//! Files that grow a piece at a time (an upload arriving in chunks), sealed as they grow:
//!
//! ```text
//! "SMPRT" 0x01 | file id (16 random bytes) | record 0 | record 1 | ...
//! record = length of its ciphertext (4 bytes, big endian) | ciphertext of up to 64 KiB + 16-byte tag
//! ```
//!
//! A record's nonce is its number, the label is authenticated with each, and each has its own tag, so pieces can't
//! be reordered, swapped or changed. A record cut short by a crash is dropped when the file is opened again.

use crate::stream::{CHUNK, TAG, nonce};
use crate::{Error, Vault};
use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const MAGIC: &[u8; 6] = b"SMPRT\x01";
const HEADER: usize = MAGIC.len() + 16;
const FLAG: u8 = 2;

/// A growing sealed file, opened to add to it or read it back.
pub struct Records {
    file: File,
    cipher: XChaCha20Poly1305,
    label: Vec<u8>,
    /// Plaintext bytes in the whole records so far.
    plain_len: u64,
    count: u64,
    end: u64,
}

impl Records {
    /// Opens (or starts) the file at `path`. Anything after the last whole record is cut off.
    pub(crate) fn open(vault: &Vault, label: &str, path: &Path) -> Result<Records, Error> {
        let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        let len = file.metadata()?.len();
        let mut head = [0u8; HEADER];
        if len < HEADER as u64 {
            getrandom::fill(&mut head[MAGIC.len()..]).map_err(|e| Error::Random(e.to_string()))?;
            head[..MAGIC.len()].copy_from_slice(MAGIC);
            file.set_len(0)?;
            file.write_all(&head)?;
        } else {
            file.read_exact(&mut head)?;
            if !head.starts_with(MAGIC) {
                return Err(Error::NotSealed);
            }
        }
        let key = vault.file_key(b"sentry-mate/records/v1", &head[MAGIC.len()..]);
        let cipher = XChaCha20Poly1305::new((&*key).into());
        // walk the records: lengths are enough to know how much plaintext is here
        let (mut pos, mut plain_len, mut count) = (HEADER as u64, 0u64, 0u64);
        let len = file.metadata()?.len();
        loop {
            if pos + 4 > len {
                break;
            }
            file.seek(SeekFrom::Start(pos))?;
            let mut l = [0u8; 4];
            file.read_exact(&mut l)?;
            let n = u64::from(u32::from_be_bytes(l));
            if !(TAG as u64..=(CHUNK + TAG) as u64).contains(&n) || pos + 4 + n > len {
                break;
            }
            pos += 4 + n;
            plain_len += n - TAG as u64;
            count += 1;
        }
        if pos < len {
            file.set_len(pos)?;
        }
        Ok(Records { file, cipher, label: label.as_bytes().to_vec(), plain_len, count, end: pos })
    }

    pub fn plain_len(&self) -> u64 {
        self.plain_len
    }

    /// Adds `data`, sealed in records of up to 64 KiB, and syncs it.
    pub fn append(&mut self, data: &[u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(self.end))?;
        let mut out = Vec::with_capacity(data.len() + (data.len() / CHUNK + 1) * (TAG + 4));
        for piece in data.chunks(CHUNK) {
            let ct = self
                .cipher
                .encrypt(&nonce(self.count, FLAG), Payload { msg: piece, aad: &self.label })
                .map_err(|_| io::Error::other("encryption failed"))?;
            out.extend_from_slice(&(ct.len() as u32).to_be_bytes());
            out.extend_from_slice(&ct);
            self.count += 1;
            self.plain_len += piece.len() as u64;
        }
        self.file.write_all(&out)?;
        self.file.sync_data()?;
        self.end += out.len() as u64;
        Ok(())
    }

    /// Calls `each` with every record's plaintext, in order, checking each.
    pub fn read_all(&mut self, mut each: impl FnMut(&[u8]) -> io::Result<()>) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(HEADER as u64))?;
        let mut buf = vec![0u8; CHUNK + TAG];
        for i in 0..self.count {
            let mut l = [0u8; 4];
            self.file.read_exact(&mut l)?;
            let n = u32::from_be_bytes(l) as usize;
            if n > buf.len() {
                return Err(io::Error::other(Error::Damaged));
            }
            self.file.read_exact(&mut buf[..n])?;
            let pt = zeroize::Zeroizing::new(
                self.cipher
                    .decrypt(&nonce(i, FLAG), Payload { msg: &buf[..n], aad: &self.label })
                    .map_err(|_| io::Error::other(Error::Damaged))?,
            );
            each(&pt)?;
        }
        Ok(())
    }
}
