//! Upload tokens: one per car, write-only, as many as there are cars. Only each token's SHA-256 is kept. A token's
//! first upload ties it to that car's CAN Bridge key; from then on it can upload, and ask about, only that key's files.
//!
//! ```text
//! <data>/tokens/<sha256>.json   a token (sealed): its id, the car's name, when it was made, whether it is revoked;
//!                               written by `token new` and `token revoke`
//! <data>/tokens/<sha256>.key    the CAN Bridge key it is tied to (sealed), written once, by its first upload
//! <data>/tokens.json            every token in one file (the format before): moved into the folder when opened
//! ```
//!
//! One small file per token, named by the token's SHA-256 (which can't be turned back into the token), so finding a
//! token is opening one file whatever the number of cars, and making, revoking or tying one writes only that token's
//! files. The owner's commands and the server write different files (the server only `.key`), so neither can undo the
//! other's change when both run at once.
//!
//! Tokens in use are kept in memory (a bounded cache). Each request still opens its token's file and reads its first
//! bytes: a sealed file starts with a random id that is new on every write, so a token revoked from the command line
//! while the server runs stops working at its next request, with nothing decrypted when nothing changed.

use crate::cache::{KeyLocks, Lru};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use vault::Vault;

/// Tokens kept in memory (the others are read from their file at their next request).
pub const CACHED_TOKENS: usize = 4096;
const PREFIX: &str = "smt_";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Token {
    pub id: String,
    /// The car's name, as the owner typed it.
    pub car: String,
    /// SHA-256 (hex) of the token.
    pub hash: String,
    /// The CAN Bridge key it is tied to, after its first upload.
    pub vehicle_key: Option<String>,
    pub created_ms: u64,
    pub revoked: bool,
}

/// What `<sha256>.json` holds.
#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    id: String,
    car: String,
    created_ms: u64,
    revoked: bool,
}

/// What `<sha256>.key` holds.
#[derive(serde::Serialize, serde::Deserialize)]
struct Tie {
    vehicle_key: String,
    tied_ms: u64,
}

/// The token file of the format before.
#[derive(serde::Deserialize)]
struct Legacy {
    tokens: Vec<Token>,
}

/// A token as last read, and which write of its file that was.
struct Cached {
    stamp: Stamp,
    token: Token,
}

/// A sealed file's size and random id (new on every write).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    id: [u8; 16],
}

pub struct Tokens {
    dir: PathBuf,
    /// The folder's name, the start of each file's label.
    label: String,
    vault: Arc<Vault>,
    cache: Mutex<Lru<Cached>>,
    /// One write at a time per token (in this process).
    locks: KeyLocks<tokio::sync::Mutex<()>>,
}

const LEGACY_LABEL: &str = "tokens.json";

fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.trim().as_bytes()))
}

fn valid_hash(h: &str) -> bool {
    h.len() == 64 && h.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

fn other(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

fn stamp_of(f: &mut std::fs::File) -> std::io::Result<Stamp> {
    let len = f.metadata()?.len();
    let mut head = [0u8; vault::stream::HEADER];
    f.read_exact(&mut head)?;
    let mut id = [0u8; 16];
    id.copy_from_slice(&head[vault::stream::MAGIC.len()..]);
    Ok(Stamp { len, id })
}

impl Tokens {
    /// The tokens next to `file` (`<data>/tokens.json` → `<data>/tokens/`); a token file of the format before at
    /// `file` is moved into the folder.
    pub async fn open(file: impl Into<PathBuf>, vault: Arc<Vault>) -> std::io::Result<Tokens> {
        let legacy = file.into();
        let dir = legacy.with_extension("");
        let label = dir.file_name().and_then(|n| n.to_str()).unwrap_or("tokens").to_string();
        std::fs::create_dir_all(&dir)?;
        let t = Tokens { dir, label, vault, cache: Mutex::new(Lru::new(CACHED_TOKENS)), locks: KeyLocks::new() };
        t.migrate(&legacy)?;
        Ok(t)
    }

    /// At most `n` tokens in memory.
    pub fn cache_size(self, n: usize) -> Tokens {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).set_cap(n);
        self
    }

    /// Tokens in memory now.
    pub fn cached(&self) -> usize {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    fn file_of(&self, hash: &str, ext: &str) -> (PathBuf, String) {
        (self.dir.join(format!("{hash}.{ext}")), format!("{}/{hash}.{ext}", self.label))
    }

    /// Every token of the old single file gets its own files (none already there is replaced, so a move cut short by
    /// a crash is finished on the next start without undoing anything done since), then the old file goes.
    fn migrate(&self, legacy: &std::path::Path) -> std::io::Result<()> {
        let Some(bytes) = self.vault.read_file(legacy, LEGACY_LABEL).map_err(other)? else {
            return Ok(());
        };
        let old: Legacy = serde_json::from_slice(&bytes).map_err(other)?;
        for t in old.tokens.into_iter().filter(|t| valid_hash(&t.hash)) {
            let (file, label) = self.file_of(&t.hash, "json");
            if !file.exists() {
                let s = Stored { id: t.id, car: t.car, created_ms: t.created_ms, revoked: t.revoked };
                self.write(&file, &label, &s)?;
            }
            if let Some(k) = t.vehicle_key {
                let (file, label) = self.file_of(&t.hash, "key");
                if !file.exists() {
                    self.write(&file, &label, &Tie { vehicle_key: k, tied_ms: crate::store::now_ms() })?;
                }
            }
        }
        std::fs::remove_file(legacy)
    }

    fn write(&self, file: &std::path::Path, label: &str, what: &impl serde::Serialize) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(what).map_err(other)?;
        self.vault.write_file(file, label, &bytes).map_err(other)
    }

    fn read_tie(&self, hash: &str) -> std::io::Result<Option<String>> {
        let (file, label) = self.file_of(hash, "key");
        match self.vault.read_file(&file, &label).map_err(other)? {
            Some(b) => Ok(Some(serde_json::from_slice::<Tie>(&b).map_err(other)?.vehicle_key)),
            None => Ok(None),
        }
    }

    /// The token stored under `hash`, read from its files.
    fn load(&self, hash: &str, f: &mut std::fs::File) -> std::io::Result<Token> {
        let (_, label) = self.file_of(hash, "json");
        let mut bytes = Vec::new();
        f.rewind()?;
        f.read_to_end(&mut bytes)?;
        let s: Stored =
            serde_json::from_slice(&self.vault.open_sealed(&label, &bytes).map_err(other)?).map_err(other)?;
        Ok(Token {
            id: s.id,
            car: s.car,
            hash: hash.to_string(),
            vehicle_key: self.read_tie(hash)?,
            created_ms: s.created_ms,
            revoked: s.revoked,
        })
    }

    /// A new token for a car: returns it (shown once) and its record. There is no limit on how many.
    pub async fn create(&self, car: &str) -> std::io::Result<(String, Token)> {
        let mut raw = [0u8; 32];
        getrandom::fill(&mut raw).map_err(std::io::Error::other)?;
        let token = format!("{PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw));
        let mut id = [0u8; 6];
        getrandom::fill(&mut id).map_err(std::io::Error::other)?;
        let t = Token {
            id: hex::encode(id),
            car: trust::one_line(car, 80),
            hash: hash(&token),
            vehicle_key: None,
            created_ms: crate::store::now_ms(),
            revoked: false,
        };
        let (file, label) = self.file_of(&t.hash, "json");
        let s = Stored { id: t.id.clone(), car: t.car.clone(), created_ms: t.created_ms, revoked: false };
        self.write(&file, &label, &s)?;
        Ok((token, t))
    }

    /// The token's record, if it is one in use. Opens one file (and decrypts it only if it changed since last read).
    pub async fn find(&self, token: &str) -> Option<Token> {
        let h = hash(token);
        let (file, _) = self.file_of(&h, "json");
        let Ok(mut f) = std::fs::File::open(&file) else {
            self.cache.lock().unwrap_or_else(PoisonError::into_inner).remove(&h);
            return None;
        };
        let stamp = stamp_of(&mut f).ok()?;
        if let Some(c) = self.cache.lock().unwrap_or_else(PoisonError::into_inner).get(&h) {
            if c.stamp == stamp {
                return (!c.token.revoked).then(|| c.token.clone());
            }
        }
        let token = match self.load(&h, &mut f) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("CAN upload: a token file can't be read: {e}");
                return None;
            }
        };
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).put(h, Cached { stamp, token: token.clone() });
        (!token.revoked).then_some(token)
    }

    /// Ties a token to a car key (its first upload). Returns the key it is tied to (an earlier one wins).
    pub async fn tie(&self, token: &Token, vehicle_key: &str) -> std::io::Result<String> {
        if !valid_hash(&token.hash) {
            return Err(other("no such token"));
        }
        let lock = self.locks.get(&token.hash);
        let _one = lock.lock().await;
        let tied = match self.read_tie(&token.hash)? {
            Some(k) => k,
            None => {
                if !self.file_of(&token.hash, "json").0.exists() {
                    return Err(other("no such token"));
                }
                let (file, label) = self.file_of(&token.hash, "key");
                self.write(
                    &file,
                    &label,
                    &Tie { vehicle_key: vehicle_key.to_string(), tied_ms: crate::store::now_ms() },
                )?;
                vehicle_key.to_string()
            }
        };
        if let Some(c) = self.cache.lock().unwrap_or_else(PoisonError::into_inner).get(&token.hash) {
            c.token.vehicle_key = Some(tied.clone());
        }
        Ok(tied)
    }

    /// [`Tokens::tie`] by the token's id (for tools; reads every token when it isn't one in memory).
    pub async fn bind(&self, id: &str, vehicle_key: &str) -> std::io::Result<String> {
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .find(|c| c.token.id == id)
            .map(|c| c.token.clone());
        let token = match cached {
            Some(t) => t,
            None => self.list().await.into_iter().find(|t| t.id == id).ok_or_else(|| other("no such token"))?,
        };
        self.tie(&token, vehicle_key).await
    }

    pub async fn revoke(&self, id: &str) -> std::io::Result<bool> {
        let Some(t) = self.list().await.into_iter().find(|t| t.id == id && !t.revoked) else {
            return Ok(false);
        };
        let lock = self.locks.get(&t.hash);
        let _one = lock.lock().await;
        let (file, label) = self.file_of(&t.hash, "json");
        let s = Stored { id: t.id, car: t.car, created_ms: t.created_ms, revoked: true };
        self.write(&file, &label, &s)?;
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).remove(&t.hash);
        Ok(true)
    }

    /// Every token, oldest first (reads them all: for the owner's list, not for a request).
    pub async fn list(&self) -> Vec<Token> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            let name = e.file_name();
            let Some(h) = name.to_str().and_then(|n| n.strip_suffix(".json")).filter(|h| valid_hash(h)) else {
                continue;
            };
            let loaded = std::fs::File::open(e.path()).and_then(|mut f| self.load(h, &mut f));
            match loaded {
                Ok(t) => out.push(t),
                Err(err) => tracing::warn!("a token file can't be read: {err}"),
            }
        }
        out.sort_by(|a, b| (a.created_ms, &a.id).cmp(&(b.created_ms, &b.id)));
        out
    }

    /// Whether any token was made yet (stops at the first).
    pub async fn has_any(&self) -> bool {
        std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).is_some_and(valid_hash))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    async fn setup() -> (tempfile::TempDir, Arc<Vault>, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
        (dir, Arc::new(v), data)
    }

    #[tokio::test]
    async fn any_number_of_tokens_and_a_change_from_another_process_counts_at_once() {
        let (_d, v, data) = setup().await;
        let server = Tokens::open(data.join("tokens.json"), v.clone()).await.unwrap().cache_size(8);
        let mut made = Vec::new();
        for i in 0..60 {
            made.push(server.create(&format!("car {i}")).await.unwrap());
        }
        for (tok, t) in &made {
            assert_eq!(server.find(tok).await.as_ref(), Some(t));
        }
        assert!(server.cached() <= 8);
        assert!(server.find("smt_nope").await.is_none());
        // the command line (another Tokens on the same folder) revokes one: the server sees it at its next request
        let cli = Tokens::open(data.join("tokens.json"), v.clone()).await.unwrap();
        let (tok, t) = &made[59];
        assert!(server.find(tok).await.is_some(), "in memory now");
        assert!(cli.revoke(&t.id).await.unwrap());
        assert!(server.find(tok).await.is_none(), "revoked elsewhere, refused at once");
        assert!(!cli.revoke(&t.id).await.unwrap());
        // ties are kept apart from the owner's writes
        let (tok, t) = &made[3];
        assert_eq!(server.tie(t, "abc").await.unwrap(), "abc");
        assert_eq!(server.tie(t, "zzz").await.unwrap(), "abc", "an earlier one wins");
        assert_eq!(server.find(tok).await.unwrap().vehicle_key.as_deref(), Some("abc"));
        assert_eq!(cli.bind(&made[4].1.id, "def").await.unwrap(), "def");
        let list = cli.list().await;
        assert_eq!(list.len(), 60);
        assert_eq!(list.iter().filter(|t| t.revoked).count(), 1);
        assert_eq!(list.iter().find(|t| t.id == made[4].1.id).unwrap().vehicle_key.as_deref(), Some("def"));
        assert!(cli.has_any().await);
    }

    #[tokio::test]
    async fn the_single_file_of_before_is_moved_into_the_folder() {
        let (_d, v, data) = setup().await;
        let old = |id: &str, h: String, key: Option<&str>, revoked: bool| Token {
            id: id.into(),
            car: format!("car {id}"),
            hash: h,
            vehicle_key: key.map(str::to_string),
            created_ms: 5,
            revoked,
        };
        let tokens = vec![
            old("a1", hash("smt_one"), Some("key-one"), false),
            old("a2", hash("smt_two"), None, false),
            old("a3", hash("smt_three"), None, true),
        ];
        let bytes = serde_json::to_vec_pretty(&serde_json::json!({ "tokens": tokens })).unwrap();
        v.write_file(&data.join("tokens.json"), LEGACY_LABEL, &bytes).unwrap();
        let t = Tokens::open(data.join("tokens.json"), v.clone()).await.unwrap();
        assert!(!data.join("tokens.json").exists());
        assert_eq!(t.find("smt_one").await.unwrap().vehicle_key.as_deref(), Some("key-one"));
        assert_eq!(t.find("smt_two").await.unwrap().id, "a2");
        assert!(t.find("smt_three").await.is_none(), "still revoked");
        assert_eq!(t.list().await.len(), 3);
        for e in std::fs::read_dir(data.join("tokens")).unwrap() {
            assert!(vault::is_sealed(&std::fs::read(e.unwrap().path()).unwrap()));
        }
    }
}
