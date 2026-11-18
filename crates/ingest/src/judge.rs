//! What each uploaded log is worth: judged once its log and sidecar are both here, against the car key's pairing.
//!
//! The first signed log of a key pins its public key; from then on only logs signed by it are verified. A log
//! whose sidecar hasn't come after [`WAIT_MS`] is judged without it (unsigned, or failed once the car is paired).
//! Records judged by older rules ([`trust::RULES_VERSION`]) are judged again by [`Judge::rejudge_all`].
//!
//! Built for any number of car keys: each key's records are kept on disk per key (see [`crate::ledger`]: a small head,
//! the records, and a journal an upload adds one batch to), only the heads of recently used keys stay in memory (a
//! bounded cache), each key has its own lock so keys never wait on each other, and the sweep looks only at the keys
//! that have logs waiting for their sidecar.

use crate::cache::{KeyLocks, Lru};
use crate::ledger::{self, Change, Head, Ledger};
use crate::store::Store;
use crate::store::now_ms;
use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use trust::{Check, Entry, Link, LogName, Verdict};

/// A log without its sidecar this long is judged without it.
pub const WAIT_MS: u64 = 10 * 60 * 1000;
/// Keys whose trust head is kept in memory (the others are read from disk when next used; a head is small).
pub const CACHED_HEADS: usize = 4096;
/// Changes a key's journal holds before it is folded into the next generation of its records.
pub const COMPACT_AFTER: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Verified,
    Failed,
    CouldNotSign,
    Unsigned,
    Waiting,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub v: u32,
    pub status: Status,
    pub reason: Option<String>,
    /// The chain fields, for a log signed by some key (its key may not be the paired one).
    pub link: Option<Link>,
    /// The owner said to use it anyway.
    #[serde(default)]
    pub override_use: bool,
    pub judged_ms: u64,
    /// When the log itself arrived (for the sidecar wait).
    pub arrived_ms: u64,
}

impl Record {
    /// Whether its data may be decoded.
    pub fn usable(&self) -> bool {
        matches!(self.status, Status::Verified | Status::Unsigned) || self.override_use
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Pin {
    pub fingerprint: String,
    pub pinned_ms: u64,
    /// "first-signed-log" or "owner".
    pub how: String,
}

/// A key's whole trust state, every log's record in it: for one car's page or a tool, never for every car at once.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Book {
    pub v: u32,
    pub pin: Option<Pin>,
    /// Another key that signed logs here, offered to the owner to trust.
    pub new_key: Option<String>,
    pub logs: BTreeMap<String, Record>,
}

/// A key's trust state without its records: cheap at any size.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Summary {
    pub v: u32,
    pub pin: Option<Pin>,
    pub new_key: Option<String>,
    /// Logs waiting for their sidecar.
    pub waiting: usize,
}

pub struct Judge {
    dir: PathBuf,
    store: Store,
    heads: Mutex<Lru<Head>>,
    locks: KeyLocks<tokio::sync::Mutex<()>>,
    wait_ms: u64,
    compact_after: usize,
}

fn io(e: crate::ApiError) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

fn bad_key() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a CAN Bridge key")
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> std::io::Result<T> + Send + 'static) -> std::io::Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(std::io::Error::other)?
}

/// How a write is kept.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fold {
    /// Added to the journal, which is folded when it is long.
    WhenLong,
    /// Folded straight into the next generation, with the journal: all of it at once or (a crash, an error) none of
    /// it, as the new head is the one commit point. For changes that only make sense together, such as a new pin and
    /// every log judged again by it.
    Whole,
}

impl Judge {
    /// Judges the logs `store` keeps (its trust records are sealed with the same vault).
    pub fn new(store: Store) -> Judge {
        Judge {
            dir: store.dir().to_path_buf(),
            store,
            heads: Mutex::new(Lru::new(CACHED_HEADS)),
            locks: KeyLocks::new(),
            wait_ms: WAIT_MS,
            compact_after: COMPACT_AFTER,
        }
    }

    /// At most `heads` keys' heads in memory.
    pub fn cache_size(self, heads: usize) -> Judge {
        self.heads.lock().unwrap_or_else(PoisonError::into_inner).set_cap(heads);
        self
    }

    /// How long a log waits for its sidecar (tests: shorter than [`WAIT_MS`]).
    pub fn wait_ms(mut self, ms: u64) -> Judge {
        self.wait_ms = ms;
        self
    }

    /// Changes a journal holds before it is folded (tests: fewer than [`COMPACT_AFTER`]).
    pub fn compact_after(mut self, changes: usize) -> Judge {
        self.compact_after = changes.max(1);
        self
    }

    /// Keys whose head is in memory now.
    pub fn cached_heads(&self) -> usize {
        self.heads.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Entries in the per-key lock table now (about the keys in use at this moment).
    pub fn lock_entries(&self) -> usize {
        self.locks.len()
    }

    fn ledger(&self, key: &str) -> Ledger {
        Ledger::new(&self.dir, key, self.store.vault().clone())
    }

    /// The key's head (the caller holds the key's lock): from memory, else from disk.
    async fn take_head(&self, key: &str) -> std::io::Result<Head> {
        if let Some(h) = self.heads.lock().unwrap_or_else(PoisonError::into_inner).take(key) {
            return Ok(h);
        }
        let ledger = self.ledger(key);
        blocking(move || ledger.load_head()).await
    }

    fn put_head(&self, key: &str, head: Head) {
        self.heads.lock().unwrap_or_else(PoisonError::into_inner).put(key.to_string(), head);
    }

    /// Writes `changes` (already applied to `head`) and keeps the key's waiting marker in step: made before a change
    /// that leaves logs waiting is written, removed after the last waiting one is judged. `marker`: one may be there
    /// already.
    async fn commit(
        &self,
        key: &str,
        head: Head,
        marker: bool,
        changes: Vec<Change>,
        fold: Fold,
    ) -> std::io::Result<Head> {
        let (ledger, can, key, limit) = (self.ledger(key), self.dir.clone(), key.to_string(), self.compact_after);
        blocking(move || {
            let mut head = head;
            if !head.waiting.is_empty() && !head.marked {
                ledger::mark_waiting(&can, &key)?;
                head.marked = true;
            }
            if fold == Fold::Whole {
                ledger.compact(&mut head, changes)?;
            } else {
                ledger.append(&mut head, &changes)?;
                if head.journal_len >= limit {
                    ledger.compact(&mut head, Vec::new())?;
                }
            }
            if head.waiting.is_empty() && (marker || head.marked) {
                ledger::unmark_waiting(&can, &key)?;
                head.marked = false;
            }
            Ok(head)
        })
        .await
    }

    /// A log or sidecar finished arriving: judge its log if both are here (or mark it waiting).
    pub async fn arrived(&self, name: &str) -> std::io::Result<Option<Record>> {
        let Some(n) = LogName::parse(name) else {
            return Ok(None);
        };
        let log_name = name.strip_suffix(".json").unwrap_or(name).to_string();
        let key = n.vehicle_key.to_string();
        let lock = self.locks.get(&key);
        let _one = lock.lock().await;
        let mut head = self.take_head(&key).await?;
        if !self.store.exists(&log_name) {
            self.put_head(&key, head);
            return Ok(None);
        }
        // when the log came: from the waiting list, or (a sidecar after its log was judged without it) its record
        let arrived_ms = match head.waiting.get(&log_name) {
            Some(&t) => Some(t),
            None if n.sidecar => {
                let (ledger, h, name) = (self.ledger(&key), head.clone(), log_name.clone());
                blocking(move || ledger.lookup(&h, &name)).await?.map(|r| r.arrived_ms)
            }
            None => None,
        };
        let Some((record, changes)) =
            self.judge_one(&head, &log_name, arrived_ms.unwrap_or_else(now_ms), false).await?
        else {
            self.put_head(&key, head);
            return Ok(None);
        };
        let marker = !head.waiting.is_empty();
        for c in &changes {
            head.apply(c);
        }
        let head = self.commit(&key, head, marker, changes, Fold::WhenLong).await?;
        self.put_head(&key, head);
        Ok(Some(record))
    }

    /// Judges one log against `head`: its record and the changes that make it so (pairing first, then the record).
    /// `force`: judge without the sidecar if it is still missing. `None`: the log isn't here.
    async fn judge_one(
        &self,
        head: &Head,
        log_name: &str,
        arrived_ms: u64,
        force: bool,
    ) -> std::io::Result<Option<(Record, Vec<Change>)>> {
        if !self.store.exists(log_name) {
            return Ok(None);
        }
        let side_bytes = self.store.read(&format!("{log_name}.json")).await.ok().flatten();
        let waited = now_ms().saturating_sub(arrived_ms) >= self.wait_ms;
        let Some(side) = side_bytes
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .or(if force || waited { Some(serde_json::Value::Null) } else { None })
        else {
            let r = Record {
                v: trust::RULES_VERSION,
                status: Status::Waiting,
                reason: None,
                link: None,
                override_use: false,
                judged_ms: now_ms(),
                arrived_ms,
            };
            return Ok(Some((r.clone(), vec![Change::Log { name: log_name.to_string(), record: r }])));
        };
        let (sha, len) = self.store.hash(log_name).await.map_err(io)?;
        let check = trust::check(log_name, &sha, len, &side);
        let verdict = trust::judge(&check, head.pin.as_ref().map(|p| p.fingerprint.as_str()));
        let link = match &check {
            Check::Signed(l) => Some(l.clone()),
            Check::Bad { link, .. } => link.clone(),
            Check::NotSigned { .. } => None,
        };
        let mut changes = Vec::new();
        if let (Verdict::Verified, None, Some(l)) = (&verdict, &head.pin, &link) {
            let pin =
                Pin { fingerprint: l.key_fingerprint.clone(), pinned_ms: now_ms(), how: "first-signed-log".into() };
            changes.push(Change::Pin { pin: Some(pin) });
        }
        if let (Verdict::Failed(reason), Some(l)) = (&verdict, &link)
            && reason == "signed by another key"
            && head.new_key.is_none()
        {
            changes.push(Change::NewKey { fingerprint: Some(l.key_fingerprint.clone()) });
        }
        let (status, reason) = match verdict {
            Verdict::Verified => (Status::Verified, None),
            Verdict::Failed(r) => (Status::Failed, Some(r)),
            Verdict::CouldNotSign(r) => (Status::CouldNotSign, Some(r)),
            Verdict::Unsigned => (Status::Unsigned, None),
        };
        let override_use = head.overrides.contains(log_name);
        let r = Record { v: trust::RULES_VERSION, status, reason, link, override_use, judged_ms: now_ms(), arrived_ms };
        changes.push(Change::Log { name: log_name.to_string(), record: r.clone() });
        Ok(Some((r, changes)))
    }

    /// Logs still waiting for their sidecar past the wait are judged without it. Returns how many. Only keys with
    /// logs waiting are looked at (their markers); a key that can't be judged now is skipped (and logged), the others
    /// go on.
    pub async fn sweep(&self) -> std::io::Result<usize> {
        let can = self.dir.clone();
        if !can.exists() {
            return Ok(0);
        }
        let keys = blocking(move || ledger::waiting_keys(&can)).await?;
        let mut n = 0;
        for key in keys {
            match self.sweep_key(&key).await {
                Ok(k) => n += k,
                // not the key: the server's log is kept in the clear
                Err(e) => tracing::warn!("judging the waiting logs of a car: {e}"),
            }
        }
        Ok(n)
    }

    async fn sweep_key(&self, key: &str) -> std::io::Result<usize> {
        let lock = self.locks.get(key);
        let _one = lock.lock().await;
        let mut head = self.take_head(key).await?;
        let now = now_ms();
        let due: Vec<(String, u64)> = head
            .waiting
            .iter()
            .filter(|&(_, &t)| now.saturating_sub(t) >= self.wait_ms)
            .map(|(k, &t)| (k.clone(), t))
            .collect();
        let mut changes = Vec::new();
        for (name, arrived_ms) in &due {
            if let Some((_, ch)) = self.judge_one(&head, name, *arrived_ms, true).await? {
                for c in &ch {
                    head.apply(c);
                }
                changes.extend(ch);
            }
        }
        // the marker is there (that is how this key was found): commit removes it once nothing waits
        let head = self.commit(key, head, true, changes, Fold::WhenLong).await?;
        self.put_head(key, head);
        Ok(due.len())
    }

    /// Judges every log of a key again (after a pairing change, or records from older rules).
    pub async fn rejudge_all(&self, key: &str) -> std::io::Result<usize> {
        if !ledger::valid_key(key) {
            return Err(bad_key());
        }
        let lock = self.locks.get(key);
        let _one = lock.lock().await;
        let head = self.take_head(key).await?;
        let (head, n) = self.rejudge_locked(key, head, Vec::new()).await?;
        self.put_head(key, head);
        Ok(n)
    }

    /// Every log again, in name order (the key's lock held), with `changes` already applied to `head` (a new pin).
    /// Nothing is written until every log is judged; then it all goes into the next generation at once, so a crash or
    /// an error part way (a log that can't be read) leaves the key exactly as it was, never half judged by a new pin.
    /// Memory: this key's records once (a fold of a journal this long needs them anyway).
    async fn rejudge_locked(
        &self,
        key: &str,
        mut head: Head,
        mut changes: Vec<Change>,
    ) -> std::io::Result<(Head, usize)> {
        let (ledger, h) = (self.ledger(key), head.clone());
        let names: Vec<(String, u64, bool)> = blocking(move || {
            let mut v = Vec::new();
            ledger.for_each(&h, |name, r| {
                v.push((name.to_string(), r.arrived_ms, r.status != Status::Waiting));
                ControlFlow::Continue(())
            })?;
            Ok(v)
        })
        .await?;
        let marker = !head.waiting.is_empty();
        for (name, arrived_ms, force) in &names {
            if let Some((_, ch)) = self.judge_one(&head, name, *arrived_ms, *force).await? {
                for c in &ch {
                    head.apply(c);
                }
                changes.extend(ch);
            }
        }
        let rules = Change::Rules { v: trust::RULES_VERSION };
        head.apply(&rules);
        changes.push(rules);
        let head = self.commit(key, head, marker, changes, Fold::Whole).await?;
        Ok((head, names.len()))
    }

    /// The owner trusts another key (checked by its fingerprint in the app): it becomes the pin, all logs judged again.
    pub async fn trust_key(&self, key: &str, fingerprint: &str) -> std::io::Result<()> {
        if !ledger::valid_key(key) {
            return Err(bad_key());
        }
        let lock = self.locks.get(key);
        let _one = lock.lock().await;
        let mut head = self.take_head(key).await?;
        let pin = Pin { fingerprint: fingerprint.to_string(), pinned_ms: now_ms(), how: "owner".into() };
        let changes = vec![Change::Pin { pin: Some(pin) }, Change::NewKey { fingerprint: None }];
        for c in &changes {
            head.apply(c);
        }
        // the pin is written with every log judged by it, or not at all (on an error the head in memory, with the
        // new pin, is dropped: the next use reads the key as it is on disk)
        let (head, _) = self.rejudge_locked(key, head, changes).await?;
        self.put_head(key, head);
        Ok(())
    }

    /// Reads a key's records with `f` (the key's lock held). Another process (a command) may fold the journal at the
    /// same moment: a generation that vanished while being read is read again from the new head.
    async fn read<T: Send + 'static>(
        &self,
        key: &str,
        f: impl Fn(&Ledger, &Head) -> std::io::Result<T> + Send + Sync + 'static,
    ) -> std::io::Result<T> {
        if !ledger::valid_key(key) {
            return Err(bad_key());
        }
        let lock = self.locks.get(key);
        let _one = lock.lock().await;
        let f = Arc::new(f);
        let mut tries = 0;
        loop {
            let head = self.take_head(key).await?;
            let (ledger, f) = (self.ledger(key), f.clone());
            let (head, r) = tokio::task::spawn_blocking(move || {
                let r = f(&ledger, &head);
                (head, r)
            })
            .await
            .map_err(std::io::Error::other)?;
            tries += 1;
            match r {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && tries < 3 => continue,
                r => {
                    self.put_head(key, head);
                    return r;
                }
            }
        }
    }

    /// A key's whole state with every record (its whole history: one key at a time).
    pub async fn book(&self, key: &str) -> Book {
        self.read(key, |l, h| {
            let mut logs = BTreeMap::new();
            l.for_each(h, |name, r| {
                logs.insert(name.to_string(), r.clone());
                ControlFlow::Continue(())
            })?;
            Ok(Book { v: h.v, pin: h.pin.clone(), new_key: h.new_key.clone(), logs })
        })
        .await
        .unwrap_or_else(|_| Book { v: trust::RULES_VERSION, ..Book::default() })
    }

    /// A key's pairing and how many logs wait, without reading its records.
    pub async fn summary(&self, key: &str) -> Option<Summary> {
        self.read(key, |_, h| {
            Ok(Summary { v: h.v, pin: h.pin.clone(), new_key: h.new_key.clone(), waiting: h.waiting.len() })
        })
        .await
        .ok()
    }

    pub async fn record(&self, key: &str, log_name: &str) -> Option<Record> {
        let name = log_name.to_string();
        self.read(key, move |l, h| l.lookup(h, &name)).await.ok().flatten()
    }

    /// The chain report of a key's signed logs by its paired key.
    pub async fn chain(&self, key: &str) -> trust::ChainReport {
        self.read(key, |l, h| {
            let Some(pin) = &h.pin else {
                return Ok(trust::ChainReport::default());
            };
            let mut entries: Vec<Entry> = Vec::new();
            l.for_each(h, |_, r| {
                let Some(link) = r.link.as_ref() else { return ControlFlow::Continue(()) };
                if trust::same_fingerprint(&link.key_fingerprint, &pin.fingerprint) {
                    match r.status {
                        Status::Verified => entries.push(Entry { link: link.clone(), failed: None }),
                        Status::Failed => entries.push(Entry { link: link.clone(), failed: r.reason.clone() }),
                        _ => {}
                    }
                }
                ControlFlow::Continue(())
            })?;
            Ok(trust::chain(&entries))
        })
        .await
        .unwrap_or_default()
    }
}
