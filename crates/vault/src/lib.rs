//! Encryption at rest, in pure Rust.
//!
//! One random 256-bit **data key** seals everything Sentry Mate stores. It is never written down as it is: it is
//! kept wrapped twice in `<data>/vault.json`, and either wrapper opens it:
//!
//! 1. **The key file** (`<keys>/sentry-mate.key`, 32 random bytes): for everyday restarts with nothing to type. It
//!    lives in its own folder (a separate Docker volume), never inside the data folder, so a copy or backup of the
//!    data alone gives nothing.
//! 2. **The recovery key** (shown once when the vault is made): turned into a wrapping key with Argon2id at heavy
//!    settings (1 GiB of memory), for a new machine or a lost key file. It is never stored.
//!
//! Files are sealed with XChaCha20-Poly1305 ([`stream`] for finished files, [`records`] for files that grow), each
//! with its own key derived from the data key (HKDF-SHA256) and its path bound in. A file that grows gives every
//! record a random nonce of its own, so a record written again after a crash never reuses one, and binds each record
//! to the one before it. Keys are wiped from memory when dropped, and core dumps are turned off ([`harden_process`]).
//!
//! What this protects: data on a stolen or copied disk, or a backup. What it can't: a server someone has taken over
//! while it runs (the key is in memory then). Losing both the key file and the recovery key loses the data, by design.

pub mod chunks;
pub mod records;
pub mod stream;

use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zeroize::{Zeroize, Zeroizing};

pub use chunks::Chunks;
pub use records::{Records, RecordsReader};
pub use stream::{OpenReader, SealWriter, is_sealed, plaintext_len};

pub const VAULT_FILE: &str = "vault.json";
pub const KEY_FILE: &str = "sentry-mate.key";
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("no random numbers: {0}")]
    Random(String),
    #[error("not a sealed file")]
    NotSealed,
    #[error("the sealed file is damaged, was changed, or belongs to another name")]
    Damaged,
    #[error("no vault here yet")]
    NoVault,
    #[error("the vault file can't be read: {0}")]
    BadVault(String),
    #[error("the key file doesn't open this vault (another machine's, or replaced)")]
    WrongKeyFile,
    #[error("the key file is missing: restore it with the recovery key")]
    NoKeyFile,
    #[error("that recovery key doesn't open this vault")]
    WrongRecoveryKey,
    #[error("the key folder {0} is inside the data folder: give it a folder (volume) of its own")]
    KeysInsideData(PathBuf),
    #[error("there is a vault in {0} already")]
    VaultThere(PathBuf),
    #[error(
        "the keys folder has a key file ({key}), but the data folder {data} has no vault: check that the right \
         folder is mounted there and that the data was put back. To start over with a new, empty vault on \
         purpose, delete the key file"
    )]
    KeyFileButNoVault { data: PathBuf, key: PathBuf },
}

impl Error {
    /// Whether this is a missing file (an [`Error::Io`] of kind `NotFound`).
    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
    }
}

/// Argon2id settings for the recovery key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Kdf {
    /// Memory in KiB.
    pub m: u32,
    pub t: u32,
    pub p: u32,
}

impl Kdf {
    /// 1 GiB, 3 passes: a few seconds once, out of reach for guessing.
    pub const STRONG: Kdf = Kdf { m: 1 << 20, t: 3, p: 1 };
    /// For tests only.
    pub const FAST_FOR_TESTS: Kdf = Kdf { m: 64, t: 1, p: 1 };
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Wrapped {
    nonce: String,
    ct: String,
}

/// The data key wrapped by the recovery key, as `vault.json` holds it: what opens the data on a new machine. It is
/// safe to carry (an export's header, a move's claim answer): it opens only with the recovery key, through Argon2id.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RecoverySlot {
    salt: String,
    kdf: Kdf,
    #[serde(flatten)]
    wrapped: Wrapped,
}

impl RecoverySlot {
    /// Its Argon2id settings (a slot from elsewhere is checked against bounds before it is used).
    pub fn kdf(&self) -> Kdf {
        self.kdf
    }
}

/// A vault's recovery part: the key id and the recovery slot. With the recovery key it gives the data key
/// ([`RecoveryPart::open`]), with no key file and nothing written.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RecoveryPart {
    pub key_id: String,
    pub slot: RecoverySlot,
}

impl RecoveryPart {
    /// The vault this part belongs to, opened with the recovery key (in memory only: nothing is written).
    pub fn open(&self, recovery: &str) -> Result<Vault, Error> {
        let r = decode_recovery(recovery).ok_or(Error::WrongRecoveryKey)?;
        let salt = B64.decode(&self.slot.salt).map_err(|e| Error::BadVault(e.to_string()))?;
        let rkek = recovery_kek(&r, &salt, self.slot.kdf)?;
        let key = unwrap(&rkek, &self.slot.wrapped, RECOVERY_PURPOSE).ok_or(Error::WrongRecoveryKey)?;
        if key_id(&key) != self.key_id {
            return Err(Error::WrongRecoveryKey);
        }
        Ok(Vault { id: self.key_id.clone(), key })
    }
}

/// The data key wrapped by a key of its own (XChaCha20-Poly1305, a random nonce, the purpose as associated data):
/// what a move's finish answer carries, wrapped with a key from the move code, inside TLS.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct WrappedKey(Wrapped);

#[derive(serde::Serialize, serde::Deserialize)]
struct VaultFile {
    v: u32,
    /// Names the data key without revealing it.
    key_id: String,
    key_file: Wrapped,
    recovery: RecoverySlot,
}

/// A recovery key as shown to the owner: 32 random bytes in base32, in groups of four.
pub struct RecoveryKey(Zeroizing<String>);

/// A recovery key made but not yet in the vault file ([`Vault::make_recovery_key`]).
pub struct NewRecoveryKey {
    key: RecoveryKey,
    key_id: String,
    slot: RecoverySlot,
}

impl NewRecoveryKey {
    pub fn key(&self) -> &RecoveryKey {
        &self.key
    }

    pub fn into_key(self) -> RecoveryKey {
        self.key
    }
}

impl RecoveryKey {
    /// Whether `text` reads as a recovery key (`SMRK-` and 13 groups of 4; case, spaces and dashes don't matter):
    /// what a form checks before the slow Argon2id step. Says nothing of which vault it opens.
    pub fn reads(text: &str) -> bool {
        decode_recovery(text).is_some()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Crockford base32: no I, L, O or U, so it reads back without mix-ups.
const B32: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn encode_recovery(bytes: &[u8; 32]) -> String {
    // 256 bits as 52 symbols of 5 bits (the last one padded), in groups of four
    let (mut bits, mut nbits, mut out) = (0u32, 0u32, String::with_capacity(72));
    let push = |v: u32, out: &mut String| {
        if !out.is_empty() && out.replace('-', "").len().is_multiple_of(4) {
            out.push('-');
        }
        out.push(B32[v as usize] as char);
    };
    for &b in bytes {
        bits = (bits << 8) | u32::from(b);
        nbits += 8;
        while nbits >= 5 {
            nbits -= 5;
            push((bits >> nbits) & 31, &mut out);
        }
        bits &= (1 << nbits) - 1;
    }
    if nbits > 0 {
        push((bits << (5 - nbits)) & 31, &mut out);
    }
    format!("SMRK-{out}")
}

fn decode_recovery(text: &str) -> Option<Zeroizing<[u8; 32]>> {
    let t = Zeroizing::new(text.trim().to_ascii_uppercase());
    let t = t.strip_prefix("SMRK").unwrap_or(&t);
    let mut out = Zeroizing::new([0u8; 32]);
    let (mut bits, mut nbits, mut i, mut symbols) = (0u32, 0u32, 0usize, 0usize);
    for c in t.bytes().filter(|c| !matches!(c, b'-' | b' ')) {
        let c = match c {
            b'O' => b'0',
            b'I' | b'L' => b'1',
            c => c,
        };
        let v = B32.iter().position(|&a| a == c)? as u32;
        symbols += 1;
        bits = ((bits << 5) | v) & 0xffff;
        nbits += 5;
        if nbits >= 8 {
            nbits -= 8;
            if i < 32 {
                out[i] = ((bits >> nbits) & 0xff) as u8;
            }
            i += 1;
        }
    }
    (symbols == 52 && i == 32).then_some(out)
}

/// The opened vault: holds the data key in memory (wiped when dropped).
pub struct Vault {
    key: Zeroizing<[u8; 32]>,
    id: String,
}

fn random<const N: usize>() -> Result<Zeroizing<[u8; N]>, Error> {
    let mut b = Zeroizing::new([0u8; N]);
    getrandom::fill(&mut *b).map_err(|e| Error::Random(e.to_string()))?;
    Ok(b)
}

fn key_id(key: &[u8; 32]) -> String {
    let mut h = Sha256::new();
    h.update(b"sentry-mate key id v1");
    h.update(key);
    hex::encode(&h.finalize()[..8])
}

fn wrap(kek: &[u8; 32], what: &[u8], purpose: &[u8]) -> Result<Wrapped, Error> {
    let nonce = random::<24>()?;
    let ct = XChaCha20Poly1305::new(kek.into())
        .encrypt(XNonce::from_slice(&*nonce), Payload { msg: what, aad: purpose })
        .map_err(|_| Error::Damaged)?;
    Ok(Wrapped { nonce: B64.encode(*nonce), ct: B64.encode(ct) })
}

fn unwrap(kek: &[u8; 32], w: &Wrapped, purpose: &[u8]) -> Option<Zeroizing<[u8; 32]>> {
    let nonce = B64.decode(&w.nonce).ok().filter(|n| n.len() == 24)?;
    let ct = B64.decode(&w.ct).ok()?;
    let pt = Zeroizing::new(
        XChaCha20Poly1305::new(kek.into())
            .decrypt(XNonce::from_slice(&nonce), Payload { msg: &ct, aad: purpose })
            .ok()?,
    );
    let mut key = Zeroizing::new([0u8; 32]);
    if pt.len() != 32 {
        return None;
    }
    key.copy_from_slice(&pt);
    Some(key)
}

fn recovery_kek(recovery: &[u8; 32], salt: &[u8], kdf: Kdf) -> Result<Zeroizing<[u8; 32]>, Error> {
    let params = argon2::Params::new(kdf.m, kdf.t, kdf.p, Some(32)).map_err(|e| Error::BadVault(e.to_string()))?;
    let a = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    a.hash_password_into(recovery, salt, &mut *out).map_err(|e| Error::BadVault(e.to_string()))?;
    Ok(out)
}

const KEY_FILE_PURPOSE: &[u8] = b"sentry-mate vault v1: key file";
const RECOVERY_PURPOSE: &[u8] = b"sentry-mate vault v1: recovery key";

/// Writes a file whole or not at all, readable by its owner only. The temporary file is the file's own full name
/// with a name of this write's own after it (still ending in `.tmp`): two writers of one file, or of two files of one
/// stem (`slot.sealed`, `slot.json`), never share one.
pub fn write_private(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d)?;
    }
    let name = file.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).map_err(std::io::Error::other)?;
    let mut tmp_name = name.to_owned();
    tmp_name.push(format!(".{}-{}.tmp", std::process::id(), hex::encode(rnd)));
    let tmp = file.with_file_name(tmp_name);
    let result = (|| {
        let mut f = {
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
            o.open(&tmp)?
        };
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, file)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    if let Some(d) = file.parent()
        && let Ok(d) = std::fs::File::open(d)
    {
        let _ = d.sync_all();
    }
    Ok(())
}

fn check_apart(data: &Path, keys: &Path) -> Result<(), Error> {
    let abs = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (d, k) = (abs(data), abs(keys));
    if k.starts_with(&d) {
        return Err(Error::KeysInsideData(keys.to_path_buf()));
    }
    Ok(())
}

impl Vault {
    /// Opens the vault in `data` with the key file in `keys`. No vault but a key file: [`Error::KeyFileButNoVault`]
    /// (the data folder is likely the wrong one), not [`Error::NoVault`].
    pub fn open(data: &Path, keys: &Path) -> Result<Vault, Error> {
        check_apart(data, keys)?;
        let vf = match read_vault(data) {
            Err(Error::NoVault) => {
                let key = keys.join(KEY_FILE);
                return Err(if there(&key)? {
                    Error::KeyFileButNoVault { data: data.to_path_buf(), key }
                } else {
                    Error::NoVault
                });
            }
            r => r?,
        };
        let kek = read_key_file(keys)?;
        let key = unwrap(&kek, &vf.key_file, KEY_FILE_PURPOSE).ok_or(Error::WrongKeyFile)?;
        if key_id(&key) != vf.key_id {
            return Err(Error::WrongKeyFile);
        }
        Ok(Vault { id: vf.key_id, key })
    }

    /// Makes a new vault: a new data key, a new key file, and a recovery key (returned once, never stored). Never
    /// over a vault or a key file that is there: that key file may open data that is only missing from the data
    /// folder (a wrong path, a backup not put back), and the new one would replace it.
    pub fn create(data: &Path, keys: &Path, kdf: Kdf) -> Result<(Vault, RecoveryKey), Error> {
        check_apart(data, keys)?;
        if there(&data.join(VAULT_FILE))? {
            return Err(Error::VaultThere(data.to_path_buf()));
        }
        let key_file = keys.join(KEY_FILE);
        if there(&key_file)? {
            return Err(Error::KeyFileButNoVault { data: data.to_path_buf(), key: key_file });
        }
        let key = random::<32>()?;
        let kek = random::<32>()?;
        let recovery = random::<32>()?;
        let salt = random::<16>()?;
        let rkek = recovery_kek(&recovery, &*salt, kdf)?;
        let vf = VaultFile {
            v: 1,
            key_id: key_id(&key),
            key_file: wrap(&kek, &*key, KEY_FILE_PURPOSE)?,
            recovery: RecoverySlot { salt: B64.encode(*salt), kdf, wrapped: wrap(&rkek, &*key, RECOVERY_PURPOSE)? },
        };
        // (cut off between the two, a key file is left alone: the next start says to delete it)
        write_private(&key_file, &*kek)?;
        write_vault(data, &vf)?;
        let shown = RecoveryKey(Zeroizing::new(encode_recovery(&recovery)));
        Ok((Vault { id: vf.key_id, key }, shown))
    }

    /// Opens the vault, or makes one if there is none yet (the recovery key is then returned, once).
    pub fn open_or_create(data: &Path, keys: &Path, kdf: Kdf) -> Result<(Vault, Option<RecoveryKey>), Error> {
        match Vault::open(data, keys) {
            Ok(v) => Ok((v, None)),
            Err(Error::NoVault) => Vault::create(data, keys, kdf).map(|(v, r)| (v, Some(r))),
            Err(e) => Err(e),
        }
    }

    /// A new machine, or a lost key file: opens the vault with the recovery key and writes a new key file.
    pub fn restore(data: &Path, keys: &Path, recovery: &str) -> Result<Vault, Error> {
        check_apart(data, keys)?;
        let mut vf = read_vault(data)?;
        let r = decode_recovery(recovery).ok_or(Error::WrongRecoveryKey)?;
        let salt = B64.decode(&vf.recovery.salt).map_err(|e| Error::BadVault(e.to_string()))?;
        let rkek = recovery_kek(&r, &salt, vf.recovery.kdf)?;
        let key = unwrap(&rkek, &vf.recovery.wrapped, RECOVERY_PURPOSE).ok_or(Error::WrongRecoveryKey)?;
        if key_id(&key) != vf.key_id {
            return Err(Error::WrongRecoveryKey);
        }
        let kek = random::<32>()?;
        vf.key_file = wrap(&kek, &*key, KEY_FILE_PURPOSE)?;
        write_private(&keys.join(KEY_FILE), &*kek)?;
        write_vault(data, &vf)?;
        Ok(Vault { id: vf.key_id, key })
    }

    /// The recovery part of the vault in `data` (what an export or a move carries, so the data opens elsewhere with
    /// the same recovery key).
    pub fn recovery_part(data: &Path) -> Result<RecoveryPart, Error> {
        let vf = read_vault(data)?;
        Ok(RecoveryPart { key_id: vf.key_id, slot: vf.recovery })
    }

    /// Writes this vault somewhere else: a new key file in `keys`, then `data/vault.json` with the data key wrapped
    /// by it and `part` as its recovery slot (the part must be this vault's). For a new server taking data in (an
    /// import, a move): the data then opens there with its own key file, and with the same recovery key as before.
    /// Refuses if `data` has a vault already.
    pub fn install(&self, data: &Path, keys: &Path, part: &RecoveryPart) -> Result<(), Error> {
        check_apart(data, keys)?;
        if part.key_id != self.id {
            return Err(Error::BadVault("that recovery part belongs to another data key".into()));
        }
        if data.join(VAULT_FILE).exists() {
            return Err(Error::BadVault(format!("there is a vault in {} already", data.display())));
        }
        let kek = random::<32>()?;
        let vf = VaultFile {
            v: 1,
            key_id: self.id.clone(),
            key_file: wrap(&kek, &*self.key, KEY_FILE_PURPOSE)?,
            recovery: part.slot.clone(),
        };
        write_private(&keys.join(KEY_FILE), &*kek)?;
        write_vault(data, &vf)?;
        Ok(())
    }

    /// Replaces the recovery key (the old one stops working); the new one is returned once.
    pub fn new_recovery_key(&self, data: &Path, kdf: Kdf) -> Result<RecoveryKey, Error> {
        let new = self.make_recovery_key(kdf)?;
        self.set_recovery_key(data, &new)?;
        Ok(new.key)
    }

    /// A new recovery key for this vault, in memory only: nothing is written, and the old key keeps working until
    /// [`Vault::set_recovery_key`]. So the key can be put somewhere safe first.
    pub fn make_recovery_key(&self, kdf: Kdf) -> Result<NewRecoveryKey, Error> {
        let recovery = random::<32>()?;
        let salt = random::<16>()?;
        let rkek = recovery_kek(&recovery, &*salt, kdf)?;
        let slot = RecoverySlot { salt: B64.encode(*salt), kdf, wrapped: wrap(&rkek, &*self.key, RECOVERY_PURPOSE)? };
        Ok(NewRecoveryKey {
            key: RecoveryKey(Zeroizing::new(encode_recovery(&recovery))),
            key_id: self.id.clone(),
            slot,
        })
    }

    /// Puts `new` (from [`Vault::make_recovery_key`]) into `data/vault.json`: from then on it opens the vault, and the
    /// old key no longer does.
    pub fn set_recovery_key(&self, data: &Path, new: &NewRecoveryKey) -> Result<(), Error> {
        let mut vf = read_vault(data)?;
        if new.key_id != self.id || vf.key_id != self.id {
            return Err(Error::BadVault("that recovery key belongs to another data key".into()));
        }
        vf.recovery = new.slot.clone();
        write_vault(data, &vf)
    }

    pub fn key_id(&self) -> &str {
        &self.id
    }

    /// The data key wrapped with `kek` for `purpose` (a move hands it to the new server this way).
    pub fn wrap_key(&self, kek: &[u8; 32], purpose: &[u8]) -> Result<WrappedKey, Error> {
        Ok(WrappedKey(wrap(kek, &*self.key, purpose)?))
    }

    /// The vault whose data key `w` holds, opened with `kek` for `purpose`; its key id must be `key_id`. In memory
    /// only: nothing is written (write it somewhere with [`Vault::install`]).
    pub fn unwrap_key(kek: &[u8; 32], w: &WrappedKey, purpose: &[u8], key_id: &str) -> Result<Vault, Error> {
        let key = unwrap(kek, &w.0, purpose).ok_or(Error::Damaged)?;
        if self::key_id(&key) != key_id {
            return Err(Error::Damaged);
        }
        Ok(Vault { id: key_id.to_string(), key })
    }

    /// A key of its own for `purpose`, made from the data key (the same on every start and after a move, since a
    /// move carries the data key): for keyed hashes that must not be worked back, such as a tag for a VIN.
    pub fn derive_key(&self, purpose: &[u8]) -> Zeroizing<[u8; 32]> {
        self.file_key(b"sentry-mate derived key v1", purpose)
    }

    /// The key of one file, from its random id.
    pub(crate) fn file_key(&self, purpose: &[u8], file_id: &[u8]) -> Zeroizing<[u8; 32]> {
        let hk = Hkdf::<Sha256>::new(Some(file_id), &*self.key);
        let mut k = Zeroizing::new([0u8; 32]);
        // 32 bytes is always a valid HKDF-SHA256 output length
        let _ = hk.expand(purpose, &mut *k);
        k
    }

    /// Seals and opens the chunks of one long stream, each on its own (see [`crate::chunks`]): for a stream made again from
    /// any chunk on (an export read again from the middle) or read from any chunk on (an import carrying on).
    pub fn chunks(&self, label: &str, id: &[u8; 16]) -> Chunks {
        Chunks::new(self, label, id)
    }

    /// A writer that seals what is written to it (call `finish`).
    pub fn seal_writer<W: Write>(&self, label: &str, out: W) -> Result<SealWriter<W>, Error> {
        SealWriter::new(self, label, out)
    }

    /// A reader of a sealed file's plaintext (`sealed_len`: the sealed file's size).
    pub fn open_reader<R: Read>(&self, label: &str, input: R, sealed_len: u64) -> Result<OpenReader<R>, Error> {
        OpenReader::new(self, label, input, sealed_len)
    }

    /// Seals bytes whole.
    pub fn seal(&self, label: &str, plain: &[u8]) -> Result<Vec<u8>, Error> {
        let mut w = self.seal_writer(label, Vec::with_capacity(plain.len() + plain.len() / 4096 + 64))?;
        w.write_all(plain)?;
        Ok(w.finish()?)
    }

    /// Opens bytes sealed with [`Vault::seal`].
    pub fn open_sealed(&self, label: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
        let mut r = self.open_reader(label, sealed, sealed.len() as u64)?;
        let mut out = Zeroizing::new(Vec::new());
        r.read_to_end(&mut out).map_err(unwrap_io)?;
        Ok(out)
    }

    /// Writes a sealed file whole or not at all (a temporary file, synced, renamed into place).
    pub fn write_file(&self, path: &Path, label: &str, plain: &[u8]) -> Result<(), Error> {
        let sealed = self.seal(label, plain)?;
        write_private(path, &sealed)?;
        Ok(())
    }

    /// Reads a whole sealed file. `Ok(None)`: there is no such file.
    pub fn read_file(&self, path: &Path, label: &str) -> Result<Option<Zeroizing<Vec<u8>>>, Error> {
        match std::fs::read(path) {
            Ok(b) => self.open_sealed(label, &b).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Opens a growing sealed file to add to it, made if missing (see [`records`]). One writer per file at a time.
    pub fn records(&self, path: &Path, label: &str) -> Result<Records, Error> {
        Records::open(self, label, path)
    }

    /// Opens a growing sealed file to read only (see [`records`]): never made, cut or changed; a record still being
    /// written is not read. A missing file is an error of kind `NotFound` ([`Error::is_not_found`]).
    pub fn records_read(&self, path: &Path, label: &str) -> Result<RecordsReader, Error> {
        RecordsReader::open(self, label, path)
    }
}

impl Drop for Vault {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

/// The vault error inside an I/O error from a reader, if it is one.
pub fn unwrap_io(e: std::io::Error) -> Error {
    match e.get_ref().and_then(|i| i.downcast_ref::<Error>()) {
        Some(Error::Damaged) => Error::Damaged,
        Some(Error::NotSealed) => Error::NotSealed,
        _ => Error::Io(e),
    }
}

/// Whether something is at `path` (a file, a folder, a link): an error other than "not found" is passed on.
fn there(path: &Path) -> Result<bool, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn read_vault(data: &Path) -> Result<VaultFile, Error> {
    match std::fs::read(data.join(VAULT_FILE)) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| Error::BadVault(e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NoVault),
        Err(e) => Err(e.into()),
    }
}

fn write_vault(data: &Path, vf: &VaultFile) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(vf).map_err(|e| Error::BadVault(e.to_string()))?;
    write_private(&data.join(VAULT_FILE), &bytes)?;
    Ok(())
}

fn read_key_file(keys: &Path) -> Result<Zeroizing<[u8; 32]>, Error> {
    let b = Zeroizing::new(match std::fs::read(keys.join(KEY_FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Error::NoKeyFile),
        Err(e) => return Err(e.into()),
    });
    if b.len() != 32 {
        return Err(Error::WrongKeyFile);
    }
    let mut k = Zeroizing::new([0u8; 32]);
    k.copy_from_slice(&b);
    Ok(k)
}

/// Turns off core dumps, so a crash can't write the key to disk.
pub fn harden_process() {
    let none = rustix::process::Rlimit { current: Some(0), maximum: Some(0) };
    let _ = rustix::process::setrlimit(rustix::process::Resource::Core, none);
}
