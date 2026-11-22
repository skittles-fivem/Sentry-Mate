//! Chunks of a long stream sealed one by one, so a stream can be made again from any chunk on (an export file read
//! again from the middle when a download resumes) and read from any chunk on (an import carrying on after a restart).
//!
//! ```text
//! chunk = nonce (24 bytes) | ciphertext of up to 64 KiB | tag (16 bytes)
//! ```
//!
//! Each stream has its own keys, derived from the data key and the stream's random 16-byte id (HKDF-SHA256): one
//! that seals, one that makes nonces. A chunk's associated data is its number, whether it is the last one, and the
//! stream's label, so chunks can't be reordered, dropped, moved to another stream or the stream cut short without it
//! being found.
//!
//! The nonce is made from the chunk itself: HMAC-SHA256 (nonce key) over the associated data and the plaintext, cut
//! to 24 bytes. So sealing the same chunk again gives the same bytes (what a resumed download needs), and sealing
//! different bytes at the same place gives a different nonce: a stream whose source changed between two reads never
//! seals two plaintexts with one key and nonce. The cost is that equal chunks at the same place look equal, which is
//! what "the same bytes every time" means anyway.

use crate::stream::{CHUNK, TAG};
use crate::{Error, Vault};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

/// A chunk's nonce.
pub const NONCE: usize = 24;
/// A sealed chunk's bytes besides its plaintext.
pub const OVERHEAD: usize = NONCE + TAG;
/// The most plaintext in one chunk.
pub const MAX: usize = CHUNK;

const SEAL_PURPOSE: &[u8] = b"sentry-mate/chunks/v1: seal";
const NONCE_PURPOSE: &[u8] = b"sentry-mate/chunks/v1: nonce";

/// Seals and opens the chunks of one stream ([`Vault::chunks`]).
pub struct Chunks {
    cipher: XChaCha20Poly1305,
    nonce_key: Zeroizing<[u8; 32]>,
    label: Vec<u8>,
}

impl Chunks {
    pub(crate) fn new(vault: &Vault, label: &str, id: &[u8; 16]) -> Chunks {
        let key = vault.file_key(SEAL_PURPOSE, id);
        Chunks {
            cipher: XChaCha20Poly1305::new((&*key).into()),
            nonce_key: vault.file_key(NONCE_PURPOSE, id),
            label: label.as_bytes().to_vec(),
        }
    }

    fn aad(&self, index: u64, last: bool) -> Vec<u8> {
        let mut a = Vec::with_capacity(13 + self.label.len());
        a.extend_from_slice(b"SMXC");
        a.extend_from_slice(&index.to_be_bytes());
        a.push(u8::from(last));
        a.extend_from_slice(&self.label);
        a
    }

    /// Seals chunk number `index` (at most 64 KiB of plaintext). The same chunk always seals to the same bytes.
    pub fn seal(&self, index: u64, last: bool, plain: &[u8]) -> Result<Vec<u8>, Error> {
        if plain.len() > MAX {
            return Err(Error::Damaged);
        }
        let aad = self.aad(index, last);
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&*self.nonce_key).map_err(|_| Error::Damaged)?;
        mac.update(&aad);
        mac.update(plain);
        let full = mac.finalize().into_bytes();
        let nonce = XNonce::from_slice(&full[..NONCE]);
        let ct = self.cipher.encrypt(nonce, Payload { msg: plain, aad: &aad }).map_err(|_| Error::Damaged)?;
        let mut out = Vec::with_capacity(NONCE + ct.len());
        out.extend_from_slice(nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Opens chunk number `index` (its sealed bytes, nonce first). [`Error::Damaged`]: changed, moved, of another
    /// stream, or not the last chunk when `last` says it is (and the other way round).
    pub fn open(&self, index: u64, last: bool, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
        if sealed.len() < OVERHEAD || sealed.len() > OVERHEAD + MAX {
            return Err(Error::Damaged);
        }
        let (nonce, ct) = sealed.split_at(NONCE);
        let aad = self.aad(index, last);
        let plain = self
            .cipher
            .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad: &aad })
            .map_err(|_| Error::Damaged)?;
        Ok(Zeroizing::new(plain))
    }
}

/// How many sealed bytes a stream of `plain` plaintext bytes takes: full chunks, and a last one of at least one byte
/// (a stream of no bytes is one empty chunk).
pub fn sealed_len(plain: u64) -> u64 {
    let chunks = plain.div_ceil(MAX as u64).max(1);
    plain + chunks * OVERHEAD as u64
}

/// How many chunks a stream of `sealed` sealed bytes has, and the plaintext bytes of its last one (`None`: no stream
/// is that long).
pub fn chunks_of(sealed: u64) -> Option<(u64, usize)> {
    let full = (MAX + OVERHEAD) as u64;
    if sealed < OVERHEAD as u64 {
        return None;
    }
    let n = sealed.div_ceil(full);
    let last = sealed - (n - 1) * full;
    if last < OVERHEAD as u64 {
        return None;
    }
    Some((n, (last - OVERHEAD as u64) as usize))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::Kdf;

    #[test]
    fn chunks_seal_the_same_bytes_again_and_refuse_any_change() {
        let t = tempfile::tempdir().unwrap();
        let (data, keys) = (t.path().join("data"), t.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
        let c = v.chunks("an export", &[7u8; 16]);
        let a = c.seal(3, false, b"some bytes").unwrap();
        assert_eq!(a, c.seal(3, false, b"some bytes").unwrap(), "the same chunk seals the same");
        assert_ne!(a[..NONCE], c.seal(3, false, b"other bytes").unwrap()[..NONCE], "other bytes: another nonce");
        assert_ne!(a[..NONCE], c.seal(4, false, b"some bytes").unwrap()[..NONCE], "another place: another nonce");
        assert_eq!(&**c.open(3, false, &a).unwrap(), b"some bytes");
        assert!(c.open(4, false, &a).is_err(), "moved");
        assert!(c.open(3, true, &a).is_err(), "said to be the last");
        assert!(v.chunks("another", &[7u8; 16]).open(3, false, &a).is_err(), "another label");
        assert!(v.chunks("an export", &[8u8; 16]).open(3, false, &a).is_err(), "another stream");
        for i in 0..a.len() {
            let mut b = a.clone();
            b[i] ^= 1;
            assert!(c.open(3, false, &b).is_err(), "bit {i}");
        }
        assert!(c.open(3, false, &a[..a.len() - 1]).is_err(), "cut");
    }

    #[test]
    fn lengths_add_up() {
        for plain in [0u64, 1, 65_535, 65_536, 65_537, 10 * 65_536, 10 * 65_536 + 5] {
            let sealed = sealed_len(plain);
            let (n, last) = chunks_of(sealed).unwrap();
            assert_eq!((n - 1) * MAX as u64 + last as u64, plain, "{plain}");
        }
        assert!(chunks_of(10).is_none());
        assert!(chunks_of((MAX + OVERHEAD) as u64 + 3).is_none(), "a last chunk too short to be one");
    }
}
