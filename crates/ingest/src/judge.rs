//! What each uploaded log is worth: judged once its log and sidecar are both here, against the car key's pairing.
//!
//! The first signed log of a key pins its public key; from then on only logs signed by it are verified. A log
//! whose sidecar hasn't come after [`WAIT_MS`] is judged without it (unsigned, or failed once the car is paired).
//! Records judged by older rules ([`trust::RULES_VERSION`]) are judged again by [`Judge::rejudge_all`].
//!
//! The owner can trust another key that signed a car key's logs ([`Judge::trust_key`]): it becomes the pin, and the
//! key trusted before stays trusted for the logs that came while it was ([`Former`]), as Sentry Six Manager keeps
//! them verified.
//!
//! The server and an owner's command can use a car key's records at the same time: each holds the key's lock file
//! while it does (see [`crate::ledger`]), and a head kept in memory is read again when another process changed it.
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
use trust::{Check, Entry, Link, LogName, SIGNED_BY_ANOTHER_KEY, Verdict, same_fingerprint};

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

    /// The key that signed it, when its signature checks out (verified, or failed only for being another key's).
    pub fn signed_by(&self) -> Option<&str> {
        let good = self.status == Status::Verified
            || (self.status == Status::Failed && self.reason.as_deref() == Some(SIGNED_BY_ANOTHER_KEY));
        self.link.as_ref().filter(|_| good).map(|l| l.key_fingerprint.as_str())
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Pin {
    pub fingerprint: String,
    pub pinned_ms: u64,
    /// "first-signed-log" or "owner".
    pub how: String,
}

/// A key trusted before the one trusted now. Its logs that came while it was trusted stay verified; later ones fail.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Former {
    pub fingerprint: String,
    /// When the owner trusted another key in its place.
    pub until_ms: u64,
}

/// A key's whole trust state, every log's record in it: for one car's page or a tool, never for every car at once.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Book {
    pub v: u32,
    pub pin: Option<Pin>,
    /// Another key that signed logs here, offered to the owner to trust.
    pub new_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub former: Vec<Former>,
    pub logs: BTreeMap<String, Record>,
}

/// A key's trust state without its records: cheap at any size.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Summary {
    pub v: u32,
    pub pin: Option<Pin>,
    pub new_key: Option<String>,
    pub former: Vec<Former>,
    /// Logs waiting for their sidecar.
    pub waiting: usize,
}

/// One signing key named by a car key's logs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Signer {
    /// Its fingerprint as the CAN Bridge app shows it ("Log signing key"): the id [`Judge::trust_key`] takes and the
    /// chain report names.
    pub fingerprint: String,
    /// Logs it signed: the signature checks out, whether or not the key is trusted.
    pub signed: u64,
    /// Logs that name it but failed their check (a changed file, a bad signature, signed for another car key).
    pub failed: u64,
    /// When the first and the last of those logs arrived.
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
}

/// The signing keys of a car key, and which is trusted.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Signers {
    pub pin: Option<Pin>,
    pub former: Vec<Former>,
    /// In the order they were first seen.
    pub keys: Vec<Signer>,
}

/// What [`Judge::trust_key`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trusted {
    /// It is the trusted key now. `verified`: its logs, all verified now. `before`: the key trusted until now, which
    /// stays trusted for the logs that came before.
    Now { fingerprint: String, verified: u64, before: Option<String> },
    /// It was the trusted key already.
    Already,
    /// No log of this car key is signed by it.
    NotSeen,
}

/// The chain report of a car key's trusted key.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct KeyChain {
    /// The key the report covers (its fingerprint, as `keys` lists it); `None` before the first signed log.
    pub signing_key: Option<String>,
    #[serde(flatten)]
    pub report: trust::ChainReport,
}

/// What the CAN Bridge app is told of a complete log: the `check` of `POST /api/can/logs/status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadCheck {
    /// The file's SHA-256 is the sidecar's, the signature is the paired key's and its chain link fits the log before.
    Verified,
    /// It failed, and why. A failed log may be sent again under its name ([`crate::store::Store::receive_replacing`]).
    Failed(Fault),
    /// Not judged yet (no sidecar yet, or the log before it is still on its way): asked again next round.
    Pending,
}

/// Why a log failed its check, in the app's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// Signed, but the signature, its key (another key than the paired one, another car key's) or its fields are wrong.
    BadSignature,
    /// Its chain link does not fit: the log before it is here but is not the one it names, failed its check, or is
    /// missing although older logs of its chain are here (and nothing of it is on its way).
    ChainBreak,
    /// Not signed (or the bridge could not sign it).
    Unsigned,
    /// The file differs from what its sidecar names (SHA-256 or length).
    Mismatch,
}

impl UploadCheck {
    pub fn word(self) -> &'static str {
        match self {
            UploadCheck::Verified => "verified",
            UploadCheck::Failed(_) => "failed",
            UploadCheck::Pending => "pending",
        }
    }

    /// The `check_reason` of a failed log.
    pub fn reason(self) -> Option<&'static str> {
        match self {
            UploadCheck::Failed(f) => Some(f.word()),
            UploadCheck::Verified | UploadCheck::Pending => None,
        }
    }
}

impl Fault {
    pub fn word(self) -> &'static str {
        match self {
            Fault::BadSignature => "bad_signature",
            Fault::ChainBreak => "chain_break",
            Fault::Unsigned => "unsigned",
            Fault::Mismatch => "mismatch",
        }
    }

    /// The fault of a failed record's reason (the words of [`trust::check`] and [`trust::judge`]).
    fn of(reason: Option<&str>) -> Fault {
        match reason {
            Some(r) if r == trust::SHA256_DIFFERS || r == trust::LENGTH_DIFFERS => Fault::Mismatch,
            Some(r) if r == trust::NOT_SIGNED_BUT_PAIRED => Fault::Unsigned,
            Some(r) if r == trust::BAD_CHAIN_INDEX || r == trust::BAD_CHAIN_FIELDS => Fault::ChainBreak,
            // the signature, its key or its fields
            Some(_) | None => Fault::BadSignature,
        }
    }
}

/// How long a status question waits for a key another task holds (a long judging) before it answers `pending`.
pub const CHECK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// What one pass over a key's records keeps of each log, to fit the chain links of the logs asked about.
struct Seen {
    /// Verified (`Some(true)`), waiting (`None`) or anything else (`Some(false)`).
    good: Option<bool>,
    /// Its chain index and SHA-256, when signed.
    link: Option<(u64, [u8; 32])>,
}

fn sha_bytes(hex_sha: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex_sha.to_ascii_lowercase(), &mut out).ok()?;
    Some(out)
}

/// A log's record as it was just written: judged on arrival, by the sweep, judged again, or after its key was trusted.
/// What [`Judge::on_judged`] is told (a log still waiting for its sidecar is not told).
#[derive(Debug, Clone)]
pub struct Judged {
    /// The car key (`vehicleKey`) the log belongs to.
    pub key: String,
    /// The log's name (`<car key>_<UTC start>_<seq>.log.xz`).
    pub name: String,
    pub record: Record,
}

/// Told of every record [`Judge`] writes, once it is on disk (see [`Judge::on_judged`]). Called on a blocking thread
/// while the key is held: it must be quick and never wait (send on a channel, set a flag).
pub type OnJudged = Arc<dyn Fn(&Judged) + Send + Sync>;

pub struct Judge {
    dir: PathBuf,
    store: Store,
    heads: Mutex<Lru<Head>>,
    locks: KeyLocks<tokio::sync::Mutex<()>>,
    wait_ms: u64,
    compact_after: usize,
    on_judged: Option<OnJudged>,
}

fn io(e: crate::ApiError) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

fn bad_key() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a CAN Bridge key")
}

/// A key held by this process: its lock in memory, then its lock file (released in that order's reverse).
struct Held {
    _file: ledger::Locked,
    _one: tokio::sync::OwnedMutexGuard<()>,
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
            on_judged: None,
        }
    }

    /// Tells `f` of every log's record once it is written (not of logs left waiting for their sidecar): what the
    /// background decoding listens to. It changes nothing in how logs are judged.
    pub fn on_judged(mut self, f: OnJudged) -> Judge {
        self.on_judged = Some(f);
        self
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

    /// Holds a key: one task of this process at a time, and one process at a time (its lock file), until dropped.
    async fn hold(&self, key: &str) -> std::io::Result<Held> {
        let one = self.locks.get(key).lock_owned().await;
        let ledger = self.ledger(key);
        let file = blocking(move || ledger.lock()).await?;
        Ok(Held { _file: file, _one: one })
    }

    /// The key's head (the caller holds the key): from memory while the files are as it last saw them, else from
    /// disk (another process changed them, or it isn't in memory).
    async fn take_head(&self, key: &str) -> std::io::Result<Head> {
        let cached = self.heads.lock().unwrap_or_else(PoisonError::into_inner).take(key);
        let ledger = self.ledger(key);
        blocking(move || {
            if let Some(h) = cached
                && h.stamp.is_some()
                && h.stamp == Some(ledger.stamp(h.generation)?)
            {
                return Ok(h);
            }
            let mut h = ledger.load_head()?;
            h.stamp = Some(ledger.stamp(h.generation)?);
            Ok(h)
        })
        .await
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
        let told: Vec<Judged> = match &self.on_judged {
            Some(_) => changes
                .iter()
                .filter_map(|c| match c {
                    Change::Log { name, record } if record.status != Status::Waiting => {
                        Some(Judged { key: key.clone(), name: name.clone(), record: record.clone() })
                    }
                    _ => None,
                })
                .collect(),
            None => Vec::new(),
        };
        let on_judged = self.on_judged.clone();
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
            head.stamp = Some(ledger.stamp(head.generation)?);
            if let Some(f) = &on_judged {
                told.iter().for_each(|j| f(j));
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
        let _held = self.hold(&key).await?;
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
        // signed by a key the owner trusted before the one trusted now, and here while it was: still verified
        let earlier = match &check {
            Check::Signed(l) => head
                .former
                .iter()
                .any(|f| same_fingerprint(&f.fingerprint, &l.key_fingerprint) && arrived_ms < f.until_ms),
            _ => false,
        };
        let verdict = match trust::judge(&check, head.pin.as_ref().map(|p| p.fingerprint.as_str())) {
            Verdict::Failed(r) if earlier && r == SIGNED_BY_ANOTHER_KEY => Verdict::Verified,
            v => v,
        };
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
            && reason == SIGNED_BY_ANOTHER_KEY
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
        let _held = self.hold(key).await?;
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
        let _held = self.hold(key).await?;
        let mut head = self.take_head(key).await?;
        let rules = Change::Rules { v: trust::RULES_VERSION };
        head.apply(&rules);
        let (head, n) = self.rejudge_locked(key, head, vec![rules], |_| true).await?;
        self.put_head(key, head);
        Ok(n)
    }

    /// The logs `which` picks again, in name order (the key held), with `changes` already applied to `head` (a new
    /// pin). Nothing is written until every one is judged; then it all goes into the next generation at once, so a
    /// crash or an error part way (a log that can't be read) leaves the key exactly as it was, never half judged by a
    /// new pin. Memory: the picked names once (a fold of a journal this long needs them anyway).
    async fn rejudge_locked(
        &self,
        key: &str,
        mut head: Head,
        mut changes: Vec<Change>,
        which: impl Fn(&Record) -> bool + Send + 'static,
    ) -> std::io::Result<(Head, usize)> {
        let (ledger, h) = (self.ledger(key), head.clone());
        let names: Vec<(String, u64, bool)> = blocking(move || {
            let mut v = Vec::new();
            ledger.for_each(&h, |name, r| {
                if which(r) {
                    v.push((name.to_string(), r.arrived_ms, r.status != Status::Waiting));
                }
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
        let head = self.commit(key, head, marker, changes, Fold::Whole).await?;
        Ok((head, names.len()))
    }

    /// The owner trusts another key that signed this car key's logs (checked by its fingerprint in the app): it
    /// becomes the pin, and its logs are judged again (they are verified now). The key trusted until now stays trusted
    /// for the logs that came before ([`Former`]); its later logs fail. Only a key that signed at least one of the car
    /// key's logs can be trusted. Any case, with or without colons.
    pub async fn trust_key(&self, key: &str, fingerprint: &str) -> std::io::Result<Trusted> {
        if !ledger::valid_key(key) {
            return Err(bad_key());
        }
        let _held = self.hold(key).await?;
        let mut head = self.take_head(key).await?;
        if head.pin.as_ref().is_some_and(|p| same_fingerprint(&p.fingerprint, fingerprint)) {
            self.put_head(key, head);
            return Ok(Trusted::Already);
        }
        // the key as its logs name it, from a log it really signed
        let (ledger, h, want) = (self.ledger(key), head.clone(), fingerprint.to_string());
        let seen = blocking(move || {
            let mut found = None;
            ledger.for_each(&h, |_, r| match r.signed_by() {
                Some(fp) if same_fingerprint(fp, &want) => {
                    found = Some(fp.to_string());
                    ControlFlow::Break(())
                }
                _ => ControlFlow::Continue(()),
            })?;
            Ok(found)
        })
        .await?;
        let Some(fp) = seen else {
            self.put_head(key, head);
            return Ok(Trusted::NotSeen);
        };
        let now = now_ms();
        let before = head.pin.as_ref().map(|p| p.fingerprint.clone());
        let mut former: Vec<Former> = head.former.clone();
        former.retain(|f| {
            !same_fingerprint(&f.fingerprint, &fp)
                && before.as_ref().is_none_or(|b| !same_fingerprint(&f.fingerprint, b))
        });
        if let Some(b) = &before {
            former.push(Former { fingerprint: b.clone(), until_ms: now });
        }
        let pin = Pin { fingerprint: fp.clone(), pinned_ms: now, how: "owner".into() };
        let changes =
            vec![Change::Pin { pin: Some(pin) }, Change::NewKey { fingerprint: None }, Change::Former { former }];
        for c in &changes {
            head.apply(c);
        }
        // Only logs signed and intact but by another key can change: those of the new key become verified, the old
        // key's stay verified (they came before), others' stay failed. The pin is written with them, or not at all (on
        // an error the head in memory, with the new pin, is dropped: the next use reads the key as it is on disk).
        let other_key = |r: &Record| r.status == Status::Failed && r.reason.as_deref() == Some(SIGNED_BY_ANOTHER_KEY);
        let (head, _) = self.rejudge_locked(key, head, changes, other_key).await?;
        let (ledger, h, want) = (self.ledger(key), head.clone(), fp.clone());
        let verified = blocking(move || {
            let mut n = 0u64;
            ledger.for_each(&h, |_, r| {
                if r.status == Status::Verified && r.link.as_ref().is_some_and(|l| l.key_fingerprint == want) {
                    n += 1;
                }
                ControlFlow::Continue(())
            })?;
            Ok(n)
        })
        .await?;
        self.put_head(key, head);
        Ok(Trusted::Now { fingerprint: fp, verified, before })
    }

    /// Whether a car key has a folder here (an upload of it began). A mistyped key has none.
    pub fn known(&self, key: &str) -> bool {
        ledger::valid_key(key) && self.dir.join(key).is_dir()
    }

    /// The signing keys named by a car key's logs, and which one is trusted (its whole history: one key at a time).
    pub async fn signers(&self, key: &str) -> std::io::Result<Signers> {
        self.read(key, |l, h| {
            // (a map: logs uploaded with the token could each name a key of their own)
            let mut by_key: std::collections::HashMap<String, Signer> = std::collections::HashMap::new();
            l.for_each(h, |_, r| {
                let Some(link) = r.link.as_ref() else { return ControlFlow::Continue(()) };
                let k = by_key.entry(link.key_fingerprint.clone()).or_insert_with(|| Signer {
                    fingerprint: link.key_fingerprint.clone(),
                    signed: 0,
                    failed: 0,
                    first_seen_ms: r.arrived_ms,
                    last_seen_ms: r.arrived_ms,
                });
                if r.signed_by().is_some() {
                    k.signed += 1;
                } else {
                    k.failed += 1;
                }
                k.first_seen_ms = k.first_seen_ms.min(r.arrived_ms);
                k.last_seen_ms = k.last_seen_ms.max(r.arrived_ms);
                ControlFlow::Continue(())
            })?;
            let mut keys: Vec<Signer> = by_key.into_values().collect();
            keys.sort_by(|a, b| a.first_seen_ms.cmp(&b.first_seen_ms).then_with(|| a.fingerprint.cmp(&b.fingerprint)));
            Ok(Signers { pin: h.pin.clone(), former: h.former.clone(), keys })
        })
        .await
    }

    /// The names of a car key's `n` newest logs whose data may be used, newest first.
    pub async fn latest_usable(&self, key: &str, n: usize) -> std::io::Result<Vec<String>> {
        self.read(key, move |l, h| {
            let mut last = std::collections::VecDeque::with_capacity(n + 1);
            l.for_each(h, |name, r| {
                if r.usable() && n > 0 {
                    if last.len() == n {
                        last.pop_front();
                    }
                    last.push_back(name.to_string());
                }
                ControlFlow::Continue(())
            })?;
            Ok(last.into_iter().rev().collect())
        })
        .await
    }

    /// A car key's newest `n` records among those `keep` takes, oldest first. They are read in name order (a key's
    /// names sort by their start) and the walk ends at the first name `stop` names, so a page of a long history holds
    /// at most `n` records, never the key's whole book.
    pub async fn newest(
        &self,
        key: &str,
        n: usize,
        keep: impl Fn(&str, &Record) -> bool + Send + Sync + 'static,
        stop: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) -> std::io::Result<Vec<(String, Record)>> {
        self.read(key, move |l, h| {
            let mut last = std::collections::VecDeque::with_capacity(n + 1);
            l.for_each(h, |name, r| {
                if stop(name) {
                    return ControlFlow::Break(());
                }
                if n > 0 && keep(name, r) {
                    if last.len() == n {
                        last.pop_front();
                    }
                    last.push_back((name.to_string(), r.clone()));
                }
                ControlFlow::Continue(())
            })?;
            Ok(last.into_iter().collect())
        })
        .await
    }

    /// When a car key's newest log arrived (its upload), `None` before its first (each record read, none kept).
    pub async fn last_arrived(&self, key: &str) -> std::io::Result<Option<u64>> {
        self.read(key, |l, h| {
            let mut last: Option<u64> = h.waiting.values().copied().max();
            l.for_each(h, |_, r| {
                last = last.max(Some(r.arrived_ms));
                ControlFlow::Continue(())
            })?;
            Ok(last)
        })
        .await
    }

    /// Reads a key's records with `f` (the key held). A process that doesn't take the lock file (an older version)
    /// may fold the journal at the same moment: a generation that vanished while being read is read again from the
    /// new head.
    async fn read<T: Send + 'static>(
        &self,
        key: &str,
        f: impl Fn(&Ledger, &Head) -> std::io::Result<T> + Send + Sync + 'static,
    ) -> std::io::Result<T> {
        if !ledger::valid_key(key) {
            return Err(bad_key());
        }
        let held = self.hold(key).await?;
        self.read_held(held, key, f).await
    }

    /// [`Judge::read`] with the key already held.
    async fn read_held<T: Send + 'static>(
        &self,
        _held: Held,
        key: &str,
        f: impl Fn(&Ledger, &Head) -> std::io::Result<T> + Send + Sync + 'static,
    ) -> std::io::Result<T> {
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

    /// The check of each of `logs` (a car key's log names) as the CAN Bridge app is told it, from the stored records
    /// only (nothing is judged here): one pass over the key's records, whatever the number of names. `uploading`: the
    /// logs of the key still arriving (a log whose previous one is among them is pending). A log with no record yet is
    /// pending; so are all of them when the key is held longer than [`CHECK_WAIT`] (a long judging).
    pub async fn upload_checks(
        &self,
        key: &str,
        logs: &[String],
        uploading: std::collections::HashSet<String>,
    ) -> std::io::Result<std::collections::HashMap<String, UploadCheck>> {
        use std::collections::{HashMap, HashSet};
        if !ledger::valid_key(key) {
            return Err(bad_key());
        }
        let pending = || logs.iter().map(|n| (n.clone(), UploadCheck::Pending)).collect::<HashMap<_, _>>();
        let Ok(held) = tokio::time::timeout(CHECK_WAIT, self.hold(key)).await else {
            return Ok(pending());
        };
        let wanted: Arc<HashSet<String>> = Arc::new(logs.iter().cloned().collect());
        let (wait_ms, now) = (self.wait_ms, now_ms());
        let w = wanted.clone();
        let found = self
            .read_held(held?, key, move |l, h| {
                let mut seen: HashMap<Box<str>, Seen> = HashMap::new();
                // the lowest chain index of each signing key so far, in name order (a log's older logs come before it)
                let mut lowest: HashMap<String, u64> = HashMap::new();
                let mut asked: Vec<(String, Record, bool)> = Vec::new();
                l.for_each(h, |name, r| {
                    let good = match r.status {
                        Status::Verified => Some(true),
                        Status::Waiting => None,
                        Status::Failed | Status::CouldNotSign | Status::Unsigned => Some(false),
                    };
                    let link = r.link.as_ref().and_then(|k| sha_bytes(&k.sha256).map(|sha| (k.index, sha)));
                    if w.contains(name) {
                        let older = r.link.as_ref().is_some_and(|k| {
                            lowest.iter().any(|(fp, &i)| i < k.index && same_fingerprint(fp, &k.key_fingerprint))
                        });
                        asked.push((name.to_string(), r.clone(), older));
                    }
                    if let Some(k) = r.link.as_ref() {
                        let e = lowest.entry(k.key_fingerprint.clone()).or_insert(k.index);
                        *e = (*e).min(k.index);
                    }
                    seen.insert(name.into(), Seen { good, link });
                    ControlFlow::Continue(())
                })?;
                let fit = |r: &Record, older: bool| -> UploadCheck {
                    let Some(k) = r.link.as_ref() else { return UploadCheck::Failed(Fault::ChainBreak) };
                    if k.prev_name == "-" {
                        return UploadCheck::Verified;
                    }
                    match seen.get(k.prev_name.as_str()) {
                        Some(Seen { good: None, .. }) => UploadCheck::Pending,
                        Some(Seen { good: Some(true), link: Some((i, sha)) }) => {
                            let fits = i.checked_add(1) == Some(k.index) && sha_bytes(&k.prev_sha256) == Some(*sha);
                            if fits { UploadCheck::Verified } else { UploadCheck::Failed(Fault::ChainBreak) }
                        }
                        Some(Seen { good: Some(true), link: None }) | Some(Seen { good: Some(false), .. }) => {
                            UploadCheck::Failed(Fault::ChainBreak)
                        }
                        None if uploading.contains(&k.prev_name) => UploadCheck::Pending,
                        // the first of its chain here (a chain received from its middle)
                        None if !older => UploadCheck::Verified,
                        // older logs of its chain are here but not the one before it: it may still come
                        None if now.saturating_sub(r.arrived_ms) < wait_ms => UploadCheck::Pending,
                        None => UploadCheck::Failed(Fault::ChainBreak),
                    }
                };
                Ok(asked
                    .into_iter()
                    .map(|(name, r, older)| {
                        let c = match r.status {
                            Status::Verified => fit(&r, older),
                            Status::Waiting => UploadCheck::Pending,
                            Status::Unsigned | Status::CouldNotSign => UploadCheck::Failed(Fault::Unsigned),
                            Status::Failed => UploadCheck::Failed(Fault::of(r.reason.as_deref())),
                        };
                        (name, c)
                    })
                    .collect::<HashMap<_, _>>())
            })
            .await?;
        let mut all = pending();
        all.extend(found);
        Ok(all)
    }

    /// Whether a stored log failed its check, so that a new upload of its name (or its sidecar's) replaces it.
    pub async fn replaceable(&self, log_name: &str, uploading: std::collections::HashSet<String>) -> bool {
        let Some(n) = LogName::parse(log_name).filter(|n| !n.sidecar) else { return false };
        let checks = self.upload_checks(n.vehicle_key, &[log_name.to_string()], uploading).await;
        checks.is_ok_and(|c| matches!(c.get(log_name), Some(UploadCheck::Failed(_))))
    }

    /// A key's whole state with every record (its whole history: one key at a time).
    pub async fn book(&self, key: &str) -> Book {
        self.read(key, |l, h| {
            let mut logs = BTreeMap::new();
            l.for_each(h, |name, r| {
                logs.insert(name.to_string(), r.clone());
                ControlFlow::Continue(())
            })?;
            Ok(Book { v: h.v, pin: h.pin.clone(), new_key: h.new_key.clone(), former: h.former.clone(), logs })
        })
        .await
        .unwrap_or_else(|_| Book { v: trust::RULES_VERSION, ..Book::default() })
    }

    /// Where a key's records are: its generation and the changes written since (every record written, every
    /// pairing changed, moves it on), without reading them. What is worked out of a key's records can be kept while
    /// this stays the same. `None` for a key with nothing here.
    pub async fn changes(&self, key: &str) -> Option<(u64, usize)> {
        self.read(key, |_, h| Ok((h.generation, h.journal_len))).await.ok()
    }

    /// A key's pairing and how many logs wait, without reading its records.
    pub async fn summary(&self, key: &str) -> Option<Summary> {
        self.read(key, |_, h| {
            Ok(Summary {
                v: h.v,
                pin: h.pin.clone(),
                new_key: h.new_key.clone(),
                former: h.former.clone(),
                waiting: h.waiting.len(),
            })
        })
        .await
        .ok()
    }

    pub async fn record(&self, key: &str, log_name: &str) -> Option<Record> {
        let name = log_name.to_string();
        self.read(key, move |l, h| l.lookup(h, &name)).await.ok().flatten()
    }

    /// The chain report of a key's signed logs by its paired key, and which key that is.
    pub async fn chain(&self, key: &str) -> KeyChain {
        self.chain_with(key, None, Vec::new()).await
    }

    /// The chain report [`Judge::chain`] would give once the logs of `extra` are here too: for a check of logs not
    /// stored yet. `pin`: the key a key not paired yet would be paired with. A log here with its chain fields counts
    /// once, from its record (an `extra` of the same name is left out).
    pub async fn chain_with(&self, key: &str, pin: Option<&str>, extra: Vec<Entry>) -> KeyChain {
        let extra = Arc::new(extra);
        let given = pin.map(str::to_string);
        let (x, g) = (extra.clone(), given.clone());
        let of_pin = |pin: &str, x: &[Entry], skip: &std::collections::HashSet<String>| -> Vec<Entry> {
            x.iter()
                .filter(|e| same_fingerprint(&e.link.key_fingerprint, pin) && !skip.contains(&e.link.name))
                .cloned()
                .collect()
        };
        let r = self
            .read(key, move |l, h| {
                let Some(pin) = h.pin.as_ref().map(|p| p.fingerprint.clone()).or_else(|| g.clone()) else {
                    return Ok(KeyChain::default());
                };
                let mut entries: Vec<Entry> = Vec::new();
                let mut linked = std::collections::HashSet::new();
                l.for_each(h, |name, r| {
                    let Some(link) = r.link.as_ref() else { return ControlFlow::Continue(()) };
                    linked.insert(name.to_string());
                    if trust::same_fingerprint(&link.key_fingerprint, &pin) {
                        match r.status {
                            Status::Verified => entries.push(Entry { link: link.clone(), failed: None }),
                            Status::Failed => entries.push(Entry { link: link.clone(), failed: r.reason.clone() }),
                            _ => {}
                        }
                    }
                    ControlFlow::Continue(())
                })?;
                entries.extend(of_pin(&pin, &x, &linked));
                Ok(KeyChain { signing_key: Some(pin), report: trust::chain(&entries) })
            })
            .await;
        match (r, given) {
            (Ok(c), _) => c,
            // a key with nothing here yet: the extra logs alone
            (Err(_), Some(pin)) if !extra.is_empty() => {
                let entries = of_pin(&pin, &extra, &Default::default());
                KeyChain { signing_key: Some(pin), report: trust::chain(&entries) }
            }
            (Err(_), _) => KeyChain::default(),
        }
    }
}
