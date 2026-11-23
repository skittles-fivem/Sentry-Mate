//! The last contact of each car's CAN Bridge: when a request with its token was last accepted (a ping, a status
//! question or an upload chunk), per token and per car key, which kind of request it was, and when a log of the key
//! last arrived whole. What pages call "Last contact" and "Last upload": the bridge talks to the server every few
//! seconds while it is online, whatever the server is still working through.
//!
//! ```text
//! <data>/tokens/contacts.sealed   every token's and car key's last contact, and each key's newest log arrival
//!                                 (sealed, one small JSON object), written by the server only
//! ```
//!
//! **Recorded** in memory where a request's token is accepted ([`crate::api`]): a mutex held for a map update, no
//! disk on the request path. A car key is recorded once the token is tied to it, and for a token not tied yet, the
//! key its first upload claims, once the claim is granted (never a key it was refused). The maps hold one entry per
//! token that was used and per key a token is tied to: as many as there are tokens.
//!
//! **Saved** at most every [`SAVE_EVERY_MS`] (and only when something changed) by a task of its own that goes
//! through the data folder's write gate like an upload does (a server whose data moved away writes nothing), then
//! writes the file off the async threads. Read back when the tokens are opened, so a restart keeps the last contact
//! (all but the last minute's). The upload server and the engine share one [`Contacts`]
//! ([`crate::Tokens::with_contacts`]); another instance over the same folder only reads the file, again whenever it
//! changed.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};
use vault::Vault;

/// The file's name, in the tokens folder.
pub const FILE: &str = "contacts.sealed";
/// The file is written at most this often.
pub const SAVE_EVERY_MS: u64 = 60_000;

/// Which request it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Call {
    Ping,
    Status,
    Upload,
}

impl Call {
    pub fn word(self) -> &'static str {
        match self {
            Call::Ping => "ping",
            Call::Status => "status",
            Call::Upload => "upload",
        }
    }
}

/// A request whose token was accepted: when, and which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub at_ms: u64,
    pub call: Call,
}

/// What the file holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Book {
    /// Token id → its last contact.
    #[serde(default)]
    tokens: HashMap<String, Contact>,
    /// Car key → its last contact.
    #[serde(default)]
    keys: HashMap<String, Contact>,
    /// Car key → when its newest log arrived whole.
    #[serde(default)]
    logs: HashMap<String, u64>,
}

fn newer(slot: &mut Option<Contact>, c: Contact) -> bool {
    if slot.is_none_or(|o| c.at_ms >= o.at_ms) {
        *slot = Some(c);
        true
    } else {
        false
    }
}

impl Book {
    fn note(map: &mut HashMap<String, Contact>, who: &str, c: Contact) {
        match map.get_mut(who) {
            Some(o) if c.at_ms >= o.at_ms => *o = c,
            Some(_) => {}
            None => {
                map.insert(who.to_string(), c);
            }
        }
    }
}

/// The file's size and time, to see another process's write.
type Stamp = Option<(u64, SystemTime)>;

fn stamp(file: &Path) -> Stamp {
    std::fs::metadata(file).ok().and_then(|m| Some((m.len(), m.modified().ok()?)))
}

struct Inner {
    book: Book,
    /// Something recorded since the last save.
    dirty: bool,
    /// When the file was last written (or the book opened): the next save is [`SAVE_EVERY_MS`] after.
    saved_ms: u64,
    /// Whether this instance has recorded anything: then its memory is newer than the file, never read again.
    recorded: bool,
    /// The file as last read.
    stamp: Stamp,
}

/// The last contacts (see the module): cheap to record, shared by the upload server and the engine.
pub struct Contacts {
    /// `None`: kept in memory only.
    file: Option<(PathBuf, String, Arc<Vault>)>,
    inner: Mutex<Inner>,
    /// A save task is waiting to run.
    scheduled: AtomicBool,
    /// The file is written at most this often ([`SAVE_EVERY_MS`]).
    every_ms: u64,
}

impl std::fmt::Debug for Contacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Contacts").field("file", &self.file.as_ref().map(|f| &f.0)).finish_non_exhaustive()
    }
}

fn other(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

impl Contacts {
    /// Kept in memory only (nothing saved).
    pub fn memory() -> Contacts {
        Contacts::with(None, Book::default(), None)
    }

    /// The contacts kept in `file` (sealed with `label`), read now: a file that is missing or does not open starts
    /// them empty (it is written again at the next save).
    pub fn open(file: impl Into<PathBuf>, label: impl Into<String>, vault: Arc<Vault>) -> Contacts {
        let (file, label) = (file.into(), label.into());
        let st = stamp(&file);
        let book = read(&vault, &file, &label).unwrap_or_else(|e| {
            tracing::warn!("CAN upload: the last contacts can't be read, starting without them: {e}");
            Book::default()
        });
        Contacts::with(Some((file, label, vault)), book, st)
    }

    fn with(file: Option<(PathBuf, String, Arc<Vault>)>, book: Book, stamp: Stamp) -> Contacts {
        Contacts {
            file,
            inner: Mutex::new(Inner { book, dirty: false, saved_ms: crate::store::now_ms(), recorded: false, stamp }),
            scheduled: AtomicBool::new(false),
            every_ms: SAVE_EVERY_MS,
        }
    }

    /// Saved at most every `ms` instead of [`SAVE_EVERY_MS`] (tests).
    pub fn save_every_ms(mut self, ms: u64) -> Contacts {
        self.every_ms = ms;
        self
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The book, read again first when another process wrote the file since (an instance that records never reads
    /// it again).
    fn read_lock(&self) -> MutexGuard<'_, Inner> {
        let mut g = self.lock();
        if let Some((file, label, vault)) = &self.file
            && !g.recorded
        {
            let st = stamp(file);
            if st != g.stamp {
                if let Ok(book) = read(vault, file, label) {
                    g.book = book;
                }
                g.stamp = st;
            }
        }
        g
    }

    /// A request with this token was accepted now; `key`: the car key it is tied to, if it is.
    pub fn seen(&self, token_id: &str, key: Option<&str>, call: Call) {
        self.seen_at(token_id, key, call, crate::store::now_ms());
    }

    pub fn seen_at(&self, token_id: &str, key: Option<&str>, call: Call, at_ms: u64) {
        let c = Contact { at_ms, call };
        let mut g = self.lock();
        Book::note(&mut g.book.tokens, token_id, c);
        if let Some(k) = key {
            Book::note(&mut g.book.keys, k, c);
        }
        g.dirty = true;
        g.recorded = true;
    }

    /// A log of `key` arrived whole now.
    pub fn log_arrived(&self, key: &str) {
        self.log_arrived_at(key, crate::store::now_ms());
    }

    pub fn log_arrived_at(&self, key: &str, at_ms: u64) {
        let mut g = self.lock();
        let t = g.book.logs.entry(key.to_string()).or_insert(at_ms);
        *t = (*t).max(at_ms);
        g.dirty = true;
        g.recorded = true;
    }

    /// The last contact with this token.
    pub fn token(&self, token_id: &str) -> Option<Contact> {
        self.read_lock().book.tokens.get(token_id).copied()
    }

    /// The last contact from this car key's bridge.
    pub fn key(&self, key: &str) -> Option<Contact> {
        self.read_lock().book.keys.get(key).copied()
    }

    /// The newest last contact of any of `keys` (a car's).
    pub fn newest<'a>(&self, keys: impl IntoIterator<Item = &'a str>) -> Option<Contact> {
        let g = self.read_lock();
        let mut out = None;
        for k in keys {
            if let Some(c) = g.book.keys.get(k) {
                newer(&mut out, *c);
            }
        }
        out
    }

    /// When a log of this key last arrived whole (as far as recorded here).
    pub fn newest_log(&self, key: &str) -> Option<u64> {
        self.read_lock().book.logs.get(key).copied()
    }

    /// Writes the file now when something changed since the last save (blocking: off the async threads). Returns
    /// whether it wrote.
    pub fn save(&self) -> std::io::Result<bool> {
        let Some((file, label, vault)) = &self.file else {
            return Ok(false);
        };
        let book = {
            let mut g = self.lock();
            if !g.dirty {
                return Ok(false);
            }
            g.dirty = false;
            g.saved_ms = crate::store::now_ms();
            g.book.clone()
        };
        let bytes = serde_json::to_vec(&book).map_err(other)?;
        if let Err(e) = vault.write_file(file, label, &bytes) {
            self.lock().dirty = true;
            return Err(other(e));
        }
        self.lock().stamp = stamp(file);
        Ok(true)
    }

    /// Saves what was recorded, at most every [`SAVE_EVERY_MS`] ([`Contacts::save_every_ms`]): a task waits until
    /// the next save is due, goes through `gate` (skipped when it is closed or stays paused) and writes the file off
    /// the async threads. At most one is waiting at a time; nothing happens outside a tokio runtime or for contacts
    /// kept in memory only.
    pub fn save_soon(self: &Arc<Self>, gate: &gate::Gate) {
        if self.file.is_none() || self.scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            self.scheduled.store(false, Ordering::Release);
            return;
        };
        let (me, gate) = (self.clone(), gate.clone());
        rt.spawn(async move {
            let due = me.lock().saved_ms.saturating_add(me.every_ms);
            let wait = due.saturating_sub(crate::store::now_ms());
            if wait > 0 {
                tokio::time::sleep(Duration::from_millis(wait)).await;
            }
            // a contact recorded from here on schedules the next save (this one may not take it)
            me.scheduled.store(false, Ordering::Release);
            let Ok(pass) = crate::api::enter_gate(&gate, crate::api::GATE_WAIT).await else {
                return;
            };
            let m2 = me.clone();
            let r = tokio::task::spawn_blocking(move || {
                let _pass = pass;
                m2.save()
            })
            .await;
            match r {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!("CAN upload: the last contacts were not saved: {e}"),
                Err(e) => tracing::warn!("CAN upload: the last contacts were not saved: {e}"),
            }
        });
    }
}

fn read(vault: &Vault, file: &Path, label: &str) -> std::io::Result<Book> {
    match vault.read_file(file, label).map_err(other)? {
        Some(b) => serde_json::from_slice(&b).map_err(other),
        None => Ok(Book::default()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn vault() -> (tempfile::TempDir, Arc<Vault>, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(data.join("tokens")).unwrap();
        let (v, _) = Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
        (dir, Arc::new(v), data.join("tokens").join(FILE))
    }

    #[test]
    fn the_newest_contact_wins_per_token_and_key() {
        let c = Contacts::memory();
        c.seen_at("t1", None, Call::Ping, 100);
        assert_eq!(c.token("t1"), Some(Contact { at_ms: 100, call: Call::Ping }));
        assert_eq!(c.key("k1"), None, "an untied token says nothing of a key");
        c.seen_at("t1", Some("k1"), Call::Upload, 300);
        c.seen_at("t2", Some("k2"), Call::Status, 200);
        // one that comes in late (an older request answered last) does not go back
        c.seen_at("t1", Some("k1"), Call::Status, 250);
        assert_eq!(c.key("k1"), Some(Contact { at_ms: 300, call: Call::Upload }));
        assert_eq!(c.newest(["k1", "k2", "k3"]), Some(Contact { at_ms: 300, call: Call::Upload }));
        assert_eq!(c.newest(["k2", "k3"]), Some(Contact { at_ms: 200, call: Call::Status }));
        assert_eq!(c.newest(["k3"]), None);
        c.log_arrived_at("k1", 290);
        c.log_arrived_at("k1", 280);
        assert_eq!(c.newest_log("k1"), Some(290));
        assert!(!c.save().unwrap(), "memory only");
    }

    #[test]
    fn saved_sealed_and_read_back_and_seen_by_a_reader() {
        let (_d, v, file) = vault();
        let c = Contacts::open(&file, "tokens/contacts.sealed", v.clone());
        assert_eq!(c.key("k1"), None);
        assert!(!c.save().unwrap(), "nothing to save");
        assert!(!file.exists());
        c.seen_at("t1", Some("k1"), Call::Ping, 1_000);
        c.log_arrived_at("k1", 900);
        // another instance over the same file (the engine's, when not given this one) reads what was saved
        let reader = Contacts::open(&file, "tokens/contacts.sealed", v.clone());
        assert_eq!(reader.key("k1"), None, "not saved yet");
        assert!(c.save().unwrap());
        assert!(vault::is_sealed(&std::fs::read(&file).unwrap()));
        assert_eq!(reader.key("k1"), Some(Contact { at_ms: 1_000, call: Call::Ping }), "read again once written");
        // a restart reads them back
        let again = Contacts::open(&file, "tokens/contacts.sealed", v.clone());
        assert_eq!(again.key("k1"), Some(Contact { at_ms: 1_000, call: Call::Ping }));
        assert_eq!(again.token("t1"), Some(Contact { at_ms: 1_000, call: Call::Ping }));
        assert_eq!(again.newest_log("k1"), Some(900));
        // a file that does not open starts them empty
        std::fs::write(&file, b"not sealed").unwrap();
        assert_eq!(Contacts::open(&file, "tokens/contacts.sealed", v).key("k1"), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn saved_at_most_once_a_while_through_the_gate() {
        let (_d, v, file) = vault();
        let c = Arc::new(Contacts::open(&file, "tokens/contacts.sealed", v.clone()).save_every_ms(400));
        let gate = gate::Gate::memory();
        c.seen("t1", Some("k1"), Call::Ping);
        c.save_soon(&gate);
        c.save_soon(&gate);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!file.exists(), "not before the wait since the start has passed");
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while !file.exists() && std::time::Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(file.exists());
        let again = Contacts::open(&file, "tokens/contacts.sealed", v.clone());
        assert_eq!(again.key("k1").map(|c| c.call), Some(Call::Ping));
        // a closed gate (a server whose data moved away) writes nothing
        let closed = tempfile::tempdir().unwrap();
        let file2 = closed.path().join(FILE);
        let c2 = Arc::new(Contacts::open(&file2, "tokens/contacts.sealed", v).save_every_ms(0));
        let gate = gate::Gate::memory();
        gate.close(gate::Closed { code: "moved", message: "Moved away.".into(), retry_after_s: 60 });
        c2.seen("t1", Some("k1"), Call::Upload);
        c2.save_soon(&gate);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!file2.exists());
    }
}
