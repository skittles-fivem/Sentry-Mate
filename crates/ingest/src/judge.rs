//! What each uploaded log is worth: judged once its log and sidecar are both here, against the car key's pairing.
//!
//! The first signed log of a key pins its public key; from then on only logs signed by it are verified. A log
//! whose sidecar hasn't come after [`WAIT_MS`] is judged without it (unsigned, or failed once the car is paired).
//! A key whose records were judged by older rules ([`trust::RULES_VERSION`]) has its failed logs judged again once,
//! at the server's start ([`Judge::rejudge_old`]); [`Judge::rejudge_all`] judges every log again.
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
use std::collections::{BTreeMap, HashMap};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use trust::{Check, Entry, Link, LogName, SIGNED_BY_ANOTHER_KEY, Verdict, same_fingerprint};

pub use crate::owner_use::{OwnerUse, Pick, UnsignedSummary};

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
    /// The log's SHA-256 and length as it was judged (records from before these were kept have none: a signed one's
    /// link names them). What a log the server lost must match when it is sent again ([`crate::missing`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u64>,
    /// The SHA-256 of its sidecar as it was judged, `None` when it was judged without one (or before this was kept).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar_sha256: Option<String>,
    /// Why a log without a signature has none (`None`: it is signed, or bad, or it was judged before this was kept:
    /// see [`Record::unsigned_kind`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsigned: Option<UnsignedKind>,
}

/// Why a log has no signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsignedKind {
    /// No sidecar came (the log was judged without one after [`WAIT_MS`]).
    NoSidecar,
    /// Its sidecar has no `integrity` object: made by a CAN Bridge app from before it signed logs.
    NoIntegrity,
    /// Its sidecar's `integrity` says it is not signed, without a reason.
    SignedFalse,
    /// Its sidecar's `integrity` says the bridge tried and could not sign it, and why.
    CouldNotSign,
}

impl UnsignedKind {
    /// The API's word for it.
    pub fn word(self) -> &'static str {
        match self {
            UnsignedKind::NoSidecar => "no_sidecar",
            UnsignedKind::NoIntegrity => "no_integrity",
            UnsignedKind::SignedFalse => "signed_false",
            UnsignedKind::CouldNotSign => "could_not_sign",
        }
    }

    /// The kind of a log's check (`had_sidecar`: it was judged with its sidecar). `None` when it is signed (well or
    /// badly).
    pub fn of(check: &Check, had_sidecar: bool) -> Option<UnsignedKind> {
        match check {
            Check::NotSigned { bridge_error: Some(_), .. } => Some(UnsignedKind::CouldNotSign),
            Check::NotSigned { integrity: true, .. } => Some(UnsignedKind::SignedFalse),
            Check::NotSigned { integrity: false, .. } if had_sidecar => Some(UnsignedKind::NoIntegrity),
            Check::NotSigned { .. } => Some(UnsignedKind::NoSidecar),
            Check::Signed(_) | Check::Bad { .. } => None,
        }
    }
}

impl Record {
    /// Whether its data may be decoded.
    pub fn usable(&self) -> bool {
        matches!(self.status, Status::Verified | Status::Unsigned) || self.override_use
    }

    /// Whether it failed its check only for having no signature, from a car that is paired: what the owner may
    /// choose to use anyway ([`Judge::use_unsigned`]). A bad signature, another key's, a changed or cut file, bad
    /// chain fields or a wrong name never are.
    pub fn unsigned_only(&self) -> bool {
        match self.status {
            Status::CouldNotSign => true,
            Status::Failed => self.reason.as_deref() == Some(trust::NOT_SIGNED_BUT_PAIRED),
            Status::Verified | Status::Unsigned | Status::Waiting => false,
        }
    }

    /// Why it has no signature, in the API's words (`no_sidecar`, `no_integrity`, `signed_false`,
    /// `could_not_sign`; `unknown` for a record judged before the kind was kept that does not show it), `None` for
    /// a log that is signed (well or badly) or not judged yet.
    pub fn unsigned_kind(&self) -> Option<&'static str> {
        let unsigned = match self.status {
            Status::Unsigned | Status::CouldNotSign => true,
            Status::Failed => self.reason.as_deref() == Some(trust::NOT_SIGNED_BUT_PAIRED),
            Status::Verified | Status::Waiting => false,
        };
        if !unsigned {
            return None;
        }
        Some(match (self.unsigned, &self.status) {
            (Some(k), _) => k.word(),
            (None, Status::CouldNotSign) => UnsignedKind::CouldNotSign.word(),
            (None, _) => "unknown",
        })
    }

    /// The key that signed it, when its signature checks out (verified, or failed only for being another key's).
    /// The log's SHA-256 and length as it was stored: kept with the record, else its signed link's.
    pub fn original(&self) -> (Option<&str>, Option<u64>) {
        let link = self.link.as_ref();
        (self.sha256.as_deref().or(link.map(|l| l.sha256.as_str())), self.length.or(link.map(|l| l.length)))
    }

    /// Whether the log had a sidecar when it was judged.
    pub fn had_sidecar(&self) -> bool {
        self.sidecar_sha256.is_some() || self.link.is_some()
    }

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

/// What [`Judge::arrivals`] tells of a car key's logs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Arrivals {
    pub first_ms: Option<u64>,
    pub last_ms: Option<u64>,
    /// Logs waiting for their sidecar.
    pub waiting: u64,
}

/// What [`Judge::rejudge_old`] did: failed logs judged again, and how many of them are verified now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rejudged {
    pub judged: u64,
    pub verified: u64,
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
    /// Its own chain fields can't be read, or its link contradicts the log before it: that log is here and verified,
    /// yet it is not the one this log names (another SHA-256, or not the index before). A log before it that failed
    /// its check, or is missing, is not this log's fault: the break is reported once, as a gap or that log's own
    /// failure, and this log starts a new segment of its chain.
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
#[derive(Clone, Copy)]
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

/// What the chain check of one log needs of it.
struct Asked {
    name: String,
    status: Status,
    reason: Option<String>,
    link: Option<Link>,
    arrived_ms: u64,
    /// Logs of its signing key with a lower index came before it (in name order).
    older: bool,
}

fn seen_of(r: &Record) -> Seen {
    let good = match r.status {
        Status::Verified => Some(true),
        Status::Waiting => None,
        Status::Failed | Status::CouldNotSign | Status::Unsigned => Some(false),
    };
    Seen { good, link: r.link.as_ref().and_then(|k| sha_bytes(&k.sha256).map(|sha| (k.index, sha))) }
}

/// One pass over a key's records: the check of each log `want` takes, told to `f` in name order (with when it
/// arrived). `uploading`: the key's logs still arriving.
///
/// A log's own check (its file, its signature, its key) is its record's. Its chain link then only fails when it
/// contradicts the log before it as it is stored and verified ([`Fault::ChainBreak`]); a log before it that failed
/// its check or is missing leaves it verified (a new segment of its chain starts there, and the break is reported
/// once: the earlier log's own failure, or a gap in the chain report and the missing-log scan). While the log before it
/// is waiting, arriving, or missing for less than `wait_ms` after this one came, it is pending.
fn checks_pass(
    l: &View,
    want: &dyn Fn(&str) -> bool,
    uploading: &std::collections::HashSet<String>,
    now: u64,
    wait_ms: u64,
    mut f: impl FnMut(&str, u64, UploadCheck),
) -> std::io::Result<()> {
    use std::collections::HashMap;
    let mut seen: HashMap<Box<str>, Seen> = HashMap::new();
    // the lowest chain index of each signing key so far, in name order (a log's older logs come before it)
    let mut lowest: HashMap<String, u64> = HashMap::new();
    let mut asked: Vec<Asked> = Vec::new();
    let mem = matches!(l, View::Mem(_));
    l.for_each(|name, r| {
        if want(name) {
            let older = r
                .link
                .as_ref()
                .is_some_and(|k| lowest.iter().any(|(fp, &i)| i < k.index && same_fingerprint(fp, &k.key_fingerprint)));
            asked.push(Asked {
                name: name.to_string(),
                status: r.status.clone(),
                reason: r.reason.clone(),
                link: r.link.clone(),
                arrived_ms: r.arrived_ms,
                older,
            });
        }
        if let Some(k) = r.link.as_ref() {
            match lowest.get_mut(&k.key_fingerprint) {
                Some(e) => *e = (*e).min(k.index),
                None => {
                    lowest.insert(k.key_fingerprint.clone(), k.index);
                }
            }
        }
        // (a mirror is looked up directly)
        if !mem {
            seen.insert(name.into(), seen_of(r));
        }
        ControlFlow::Continue(())
    })?;
    let fit = |a: &Asked| -> UploadCheck {
        let Some(k) = a.link.as_ref() else { return UploadCheck::Failed(Fault::ChainBreak) };
        if k.prev_name == "-" {
            return UploadCheck::Verified;
        }
        let before = match l {
            View::Mem(m) => m.logs.get(k.prev_name.as_str()).map(seen_of),
            View::Disk(..) => seen.get(k.prev_name.as_str()).copied(),
        };
        match before {
            Some(Seen { good: None, .. }) => UploadCheck::Pending,
            Some(Seen { good: Some(true), link: Some((i, sha)) }) => {
                let fits = i.checked_add(1) == Some(k.index) && sha_bytes(&k.prev_sha256) == Some(sha);
                if fits { UploadCheck::Verified } else { UploadCheck::Failed(Fault::ChainBreak) }
            }
            // a verified log has its link; one without can't be fitted
            Some(Seen { good: Some(true), link: None }) => UploadCheck::Failed(Fault::ChainBreak),
            // the log before it failed its own check: that one is failed, this one starts a new segment
            Some(Seen { good: Some(false), .. }) => UploadCheck::Verified,
            None if uploading.contains(&k.prev_name) => UploadCheck::Pending,
            // the first of its chain here (a chain received from its middle)
            None if !a.older => UploadCheck::Verified,
            // older logs of its chain are here but not the one before it: it may still come
            None if now.saturating_sub(a.arrived_ms) < wait_ms => UploadCheck::Pending,
            // it did not come: a gap (the chain report and the missing-log scan name it), this one starts a new segment
            None => UploadCheck::Verified,
        }
    };
    for a in &asked {
        let c = match a.status {
            Status::Verified => fit(a),
            Status::Waiting => UploadCheck::Pending,
            Status::Unsigned | Status::CouldNotSign => UploadCheck::Failed(Fault::Unsigned),
            Status::Failed => UploadCheck::Failed(Fault::of(a.reason.as_deref())),
        };
        f(&a.name, a.arrived_ms, c);
    }
    Ok(())
}

/// The checks of a car key's logs counted ([`Judge::log_checks`]): no names, no keys.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct LogChecks {
    pub verified: u64,
    pub pending: u64,
    pub failed: u64,
    /// The failed ones per `check_reason`.
    pub mismatch: u64,
    pub bad_signature: u64,
    pub unsigned: u64,
    pub chain_break: u64,
    /// Runs of failed logs one after another (in name order), oldest first: at most [`LogChecks::RUNS_MAX`], the
    /// newest; `runs_total` counts them all.
    pub failed_runs: Vec<FailedRun>,
    pub runs_total: u64,
}

/// Logs that failed their check one after another.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct FailedRun {
    /// The start (from its name, UTC ms) of the run's first and last log.
    pub first_start_ms: i64,
    pub last_start_ms: i64,
    pub logs: u64,
    pub mismatch: u64,
    pub bad_signature: u64,
    pub unsigned: u64,
    pub chain_break: u64,
}

impl FailedRun {
    fn add(&mut self, f: Fault) {
        match f {
            Fault::Mismatch => self.mismatch += 1,
            Fault::BadSignature => self.bad_signature += 1,
            Fault::Unsigned => self.unsigned += 1,
            Fault::ChainBreak => self.chain_break += 1,
        }
    }
}

impl LogChecks {
    pub const RUNS_MAX: usize = 200;

    fn add(&mut self, c: UploadCheck) {
        match c {
            UploadCheck::Verified => self.verified += 1,
            UploadCheck::Pending => self.pending += 1,
            UploadCheck::Failed(f) => {
                self.failed += 1;
                match f {
                    Fault::Mismatch => self.mismatch += 1,
                    Fault::BadSignature => self.bad_signature += 1,
                    Fault::Unsigned => self.unsigned += 1,
                    Fault::ChainBreak => self.chain_break += 1,
                }
            }
        }
    }

    fn push_run(&mut self, r: FailedRun) {
        self.runs_total += 1;
        if self.failed_runs.len() == Self::RUNS_MAX {
            self.failed_runs.remove(0);
        }
        self.failed_runs.push(r);
    }

    /// Another key's counts added (a car with several car keys): runs of both, oldest first.
    pub fn merge(&mut self, o: LogChecks) {
        self.verified += o.verified;
        self.pending += o.pending;
        self.failed += o.failed;
        self.mismatch += o.mismatch;
        self.bad_signature += o.bad_signature;
        self.unsigned += o.unsigned;
        self.chain_break += o.chain_break;
        self.runs_total += o.runs_total;
        self.failed_runs.extend(o.failed_runs);
        self.failed_runs.sort_by_key(|r| (r.first_start_ms, r.last_start_ms));
        let over = self.failed_runs.len().saturating_sub(Self::RUNS_MAX);
        self.failed_runs.drain(..over);
    }
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

/// Records of all keys kept in memory at most ([`Judge::mirror_size`]): about 1 KB each.
pub const MIRRORED_RECORDS: usize = 250_000;

/// A key's records as they are on disk, kept in memory ([`Shared::mirrors`]): its head as of these records (with the
/// files' stamp then) and every log's record. Changed only with the key held, as the files are; read without holding
/// the key while the files are still the ones it was made from.
pub(crate) struct Mirror {
    pub(crate) head: Head,
    pub(crate) logs: BTreeMap<String, Record>,
}

/// The mirrors kept, the most recently used last.
struct Mirrors {
    /// Each with how many records it holds.
    keys: Vec<(String, Arc<RwLock<Mirror>>, usize)>,
    budget: usize,
    /// Keys found to hold more records than the budget, with the generation of their files then: read from their
    /// files without trying again to mirror them until their files are folded into another generation (else every
    /// read of such a key read the budget's worth of records first, to throw them away).
    over: HashMap<String, u64>,
    /// Mirrors made from the files, or tried (tests).
    tries: u64,
}

impl Mirrors {
    fn total(&self) -> usize {
        self.keys.iter().map(|(.., n)| n).sum()
    }

    /// Drops the least recently used until all fit (the newest is kept whatever its size: it fit when it was made).
    fn trim(&mut self) {
        while self.keys.len() > 1 && self.total() > self.budget {
            self.keys.remove(0);
        }
    }
}

fn read_lock<T>(l: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(PoisonError::into_inner)
}

fn write_lock<T>(l: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(PoisonError::into_inner)
}

/// What every [`Judge`] of one CAN folder in this process shares (the server's uploads and its engine each have one):
/// the keys' locks, heads and mirrors, so they never wait on each other's lock files nor read records again that the
/// other just wrote.
pub(crate) struct Shared {
    heads: Mutex<Lru<Head>>,
    locks: KeyLocks<tokio::sync::Mutex<()>>,
    mirrors: Mutex<Mirrors>,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            heads: Mutex::new(Lru::new(CACHED_HEADS)),
            locks: KeyLocks::new(),
            mirrors: Mutex::new(Mirrors { keys: Vec::new(), budget: MIRRORED_RECORDS, over: HashMap::new(), tries: 0 }),
        }
    }

    /// The one for `dir` in this process.
    fn of(dir: &std::path::Path) -> Arc<Shared> {
        static ALL: Mutex<Vec<(PathBuf, std::sync::Weak<Shared>)>> = Mutex::new(Vec::new());
        let mut all = ALL.lock().unwrap_or_else(PoisonError::into_inner);
        all.retain(|(_, w)| w.strong_count() > 0);
        if let Some(s) = all.iter().find(|(d, _)| d == dir).and_then(|(_, w)| w.upgrade()) {
            return s;
        }
        let s = Arc::new(Shared::new());
        all.push((dir.to_path_buf(), Arc::downgrade(&s)));
        s
    }

    fn mirrors(&self) -> MutexGuard<'_, Mirrors> {
        self.mirrors.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The key's mirror, if one is kept (made the most recently used).
    fn mirror(&self, key: &str) -> Option<Arc<RwLock<Mirror>>> {
        let mut m = self.mirrors();
        let i = m.keys.iter().position(|(k, ..)| k == key)?;
        let e = m.keys.remove(i);
        let found = e.1.clone();
        m.keys.push(e);
        Some(found)
    }

    /// The key's mirror when it is of `head` (the key held).
    fn mirror_of(&self, key: &str, head: &Head) -> Option<Arc<RwLock<Mirror>>> {
        self.mirror(key).filter(|m| head.stamp.is_some() && read_lock(m).head.stamp == head.stamp)
    }

    fn forget_mirror(&self, key: &str) {
        self.mirrors().keys.retain(|(k, ..)| k != key);
    }

    /// Changes written to a key's files (the key held): into its mirror when it was of the head before them
    /// (`before`), else the mirror is dropped (it is made again at its next read).
    fn mirror_commit<'a>(
        &self,
        key: &str,
        before: Option<&ledger::Stamp>,
        head: &Head,
        changes: impl Iterator<Item = &'a Change>,
    ) {
        let Some(m) = self.mirror(key) else { return };
        let mut g = write_lock(&m);
        if before.is_none() || g.head.stamp.as_ref() != before {
            drop(g);
            self.forget_mirror(key);
            return;
        }
        for c in changes {
            if let Change::Log { name, record } = c {
                g.logs.insert(name.clone(), record.clone());
            }
        }
        g.head = head.clone();
        let n = g.logs.len();
        drop(g);
        let mut all = self.mirrors();
        if let Some(e) = all.keys.iter_mut().find(|(k, ..)| k == key) {
            e.2 = n;
        }
        all.trim();
    }
}

/// A key's records to read: its mirror, or its files with its head (the key held).
pub(crate) enum View<'a> {
    Mem(&'a Mirror),
    Disk(&'a Ledger, &'a Head),
}

impl View<'_> {
    pub(crate) fn head(&self) -> &Head {
        match self {
            View::Mem(m) => &m.head,
            View::Disk(_, h) => h,
        }
    }

    /// Every log's record, in name order.
    pub(crate) fn for_each(&self, mut f: impl FnMut(&str, &Record) -> ControlFlow<()>) -> std::io::Result<()> {
        match self {
            View::Mem(m) => {
                for (n, r) in &m.logs {
                    if f(n, r).is_break() {
                        break;
                    }
                }
                Ok(())
            }
            View::Disk(l, h) => l.for_each(h, f),
        }
    }

    pub(crate) fn lookup(&self, name: &str) -> std::io::Result<Option<Record>> {
        match self {
            View::Mem(m) => Ok(m.logs.get(name).cloned()),
            View::Disk(l, h) => l.lookup(h, name),
        }
    }
}

/// A log's files as read for its check before its key is held: on the judges' threads, any number at once.
pub(crate) struct Prepared {
    /// The versions of the log and its sidecar that were read (judged again with the key held if either changed).
    log: Option<crate::store::Version>,
    side: Option<crate::store::Version>,
    /// The sidecar's SHA-256, when it is here.
    side_sha: Option<String>,
    /// The log's SHA-256 and length and its check against the sidecar: when the sidecar is here (and reads as JSON),
    /// or the log is judged without it.
    judged: Option<((String, u64), Check)>,
}

fn side_of(log_name: &str) -> String {
    format!("{log_name}.json")
}

fn hash_check(store: &Store, log_name: &str, side: &serde_json::Value) -> std::io::Result<((String, u64), Check)> {
    let (sha, len) = store.hash_here(log_name).map_err(io)?;
    let check = trust::check(log_name, &sha, len, side);
    Ok(((sha, len), check))
}

/// Reads a log's sidecar and, when it is here, the log (once: its SHA-256 as it is unsealed), and checks one against
/// the other (the signature too). `force`: check it without its sidecar if that is missing. `None`: no log here.
/// Nothing is decoded. On a blocking thread.
pub(crate) fn prepare(store: &Store, log_name: &str, force: bool) -> std::io::Result<Option<Prepared>> {
    let side_name = side_of(log_name);
    let (log, side) = (store.version(log_name), store.version(&side_name));
    if !store.exists(log_name) {
        return Ok(None);
    }
    let side_bytes = store.read_here(&side_name).ok().flatten();
    let side_sha = side_bytes.as_ref().map(|b| hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&b[..])));
    let json = side_bytes.and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    let judged = match (json, force) {
        (Some(v), _) => Some(hash_check(store, log_name, &v)?),
        (None, true) => Some(hash_check(store, log_name, &serde_json::Value::Null)?),
        (None, false) => None,
    };
    Ok(Some(Prepared { log, side, side_sha, judged }))
}

/// A log's verdict against the key's `head` (its pairing): its record and the changes that make it so (pairing
/// first, then the record). On a blocking thread (a log judged without its sidecar is read here).
fn settle(
    store: &Store,
    wait_ms: u64,
    head: &Head,
    log_name: &str,
    pre: Prepared,
    arrived_ms: u64,
    force: bool,
) -> std::io::Result<(Record, Vec<Change>)> {
    let waited = now_ms().saturating_sub(arrived_ms) >= wait_ms;
    let ((sha, len), check) = match pre.judged {
        Some(j) => j,
        None if force || waited => hash_check(store, log_name, &serde_json::Value::Null)?,
        None => {
            let r = Record {
                v: trust::RULES_VERSION,
                status: Status::Waiting,
                reason: None,
                link: None,
                override_use: false,
                judged_ms: now_ms(),
                arrived_ms,
                sha256: None,
                length: None,
                sidecar_sha256: None,
                unsigned: None,
            };
            return Ok((r.clone(), vec![Change::Log { name: log_name.to_string(), record: r }]));
        }
    };
    // signed by a key the owner trusted before the one trusted now, and here while it was: still verified
    let earlier = match &check {
        Check::Signed(l) => {
            head.former.iter().any(|f| same_fingerprint(&f.fingerprint, &l.key_fingerprint) && arrived_ms < f.until_ms)
        }
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
        let pin = Pin { fingerprint: l.key_fingerprint.clone(), pinned_ms: now_ms(), how: "first-signed-log".into() };
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
    let unsigned = UnsignedKind::of(&check, pre.side_sha.is_some());
    let mut r = Record {
        v: trust::RULES_VERSION,
        status,
        reason,
        link,
        override_use: false,
        judged_ms: now_ms(),
        arrived_ms,
        sha256: Some(sha),
        length: Some(len),
        sidecar_sha256: pre.side_sha,
        unsigned,
    };
    // the owner's word stands while the log still failed only for being unsigned: judged again as anything else
    // (verified, or bad), it is dropped
    r.override_use = head.overrides.contains(log_name) && r.unsigned_only();
    changes.push(Change::Log { name: log_name.to_string(), record: r.clone() });
    Ok((r, changes))
}

/// An arrival waiting for its key ([`Judge::arrived`]): judged with the others that came meanwhile, all written at
/// once.
struct Queued {
    name: String,
    log_name: String,
    sidecar: bool,
    pre: Option<Prepared>,
    tx: tokio::sync::oneshot::Sender<std::io::Result<Option<Record>>>,
}

fn copy_err(e: &std::io::Error) -> std::io::Error {
    std::io::Error::new(e.kind(), e.to_string())
}

/// What a write of a key's records needs, for a blocking thread.
#[derive(Clone)]
pub(crate) struct Ctx {
    dir: PathBuf,
    store: Store,
    shared: Arc<Shared>,
    wait_ms: u64,
    compact_after: usize,
    on_judged: Option<OnJudged>,
}

impl Ctx {
    fn ledger(&self, key: &str) -> Ledger {
        Ledger::new(&self.dir, key, self.store.vault().clone())
    }

    /// Writes the groups of changes (already applied to `head`, each group whole in one journal record) and keeps
    /// the key's waiting marker and mirror in step: the marker made before a change that leaves logs waiting is
    /// written, removed after the last waiting one is judged. `marker`: one may be there already.
    pub(crate) fn commit(
        &self,
        key: &str,
        mut head: Head,
        marker: bool,
        groups: Vec<Vec<Change>>,
        fold: Fold,
    ) -> std::io::Result<Head> {
        let ledger = self.ledger(key);
        let before = head.stamp.clone();
        let told: Vec<Judged> = match &self.on_judged {
            Some(_) => groups
                .iter()
                .flatten()
                .filter_map(|c| match c {
                    Change::Log { name, record } if record.status != Status::Waiting => {
                        Some(Judged { key: key.to_string(), name: name.clone(), record: record.clone() })
                    }
                    _ => None,
                })
                .collect(),
            None => Vec::new(),
        };
        if !head.waiting.is_empty() && !head.marked {
            ledger::mark_waiting(&self.dir, key)?;
            head.marked = true;
        }
        if fold == Fold::Whole {
            ledger.compact(&mut head, groups.iter().flatten().cloned().collect())?;
        } else {
            let refs: Vec<&[Change]> = groups.iter().map(Vec::as_slice).collect();
            ledger.append_groups(&mut head, &refs)?;
            if head.journal_len >= self.compact_after {
                ledger.compact(&mut head, Vec::new())?;
            }
        }
        if head.waiting.is_empty() && (marker || head.marked) {
            ledger::unmark_waiting(&self.dir, key)?;
            head.marked = false;
        }
        head.stamp = Some(ledger.stamp(head.generation)?);
        self.shared.mirror_commit(key, before.as_ref(), &head, groups.iter().flatten());
        if let Some(f) = &self.on_judged {
            told.iter().for_each(|j| f(j));
        }
        Ok(head)
    }

    /// `f` over the key's records (the key held, `head` read from the files or with changes not written yet): from
    /// its mirror when that is of the files as they are, else from the files. A mirror is made only by a read
    /// ([`Judge::read_held`]): from a head as it is on disk, never one with changes not written yet.
    pub(crate) fn view<T>(
        &self,
        key: &str,
        head: &Head,
        f: impl FnOnce(&View) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        if let Some(m) = self.shared.mirror_of(key, head) {
            return f(&View::Mem(&read_lock(&m)));
        }
        f(&View::Disk(&self.ledger(key), head))
    }

    /// [`Ctx::view`] for a head just read from the files (the key held): the mirror is made now when there is none.
    pub(crate) fn view_read<T>(
        &self,
        key: &str,
        head: &Head,
        f: impl FnOnce(&View) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        if let Some(m) = self.mirror_held(key, head)? {
            return f(&View::Mem(&read_lock(&m)));
        }
        f(&View::Disk(&self.ledger(key), head))
    }

    /// The key's mirror of `head` (the key held), made from its files when there is none: `None` when its records
    /// are more than all mirrors may hold.
    fn mirror_held(&self, key: &str, head: &Head) -> std::io::Result<Option<Arc<RwLock<Mirror>>>> {
        if let Some(m) = self.shared.mirror_of(key, head) {
            return Ok(Some(m));
        }
        if head.stamp.is_none() {
            return Ok(None);
        }
        let budget = {
            let mut all = self.shared.mirrors();
            if all.over.get(key) == Some(&head.generation) {
                return Ok(None);
            }
            all.tries += 1;
            all.budget
        };
        let mut logs = BTreeMap::new();
        let mut over = false;
        self.ledger(key).for_each(head, |name, r| {
            if logs.len() >= budget {
                over = true;
                return ControlFlow::Break(());
            }
            logs.insert(name.to_string(), r.clone());
            ControlFlow::Continue(())
        })?;
        if over {
            self.shared.forget_mirror(key);
            self.shared.mirrors().over.insert(key.to_string(), head.generation);
            return Ok(None);
        }
        let n = logs.len();
        let m = Arc::new(RwLock::new(Mirror { head: head.clone(), logs }));
        let mut all = self.shared.mirrors();
        all.keys.retain(|(k, ..)| k != key);
        all.keys.push((key.to_string(), m.clone(), n));
        all.trim();
        Ok(Some(m))
    }

    /// The prepared logs of `items` (name, when it arrived, force), judged in this order against `head`, which takes
    /// each one's changes: the groups to commit, and how many of those that were not verified are verified now.
    fn settle_all(
        &self,
        head: &mut Head,
        items: Vec<(String, u64, bool, bool)>,
        prepared: Vec<Option<Prepared>>,
    ) -> std::io::Result<(Vec<Vec<Change>>, usize, usize)> {
        let (mut groups, mut judged, mut now_verified) = (Vec::new(), 0, 0);
        for ((name, arrived_ms, force, was_verified), pre) in items.into_iter().zip(prepared) {
            let Some(pre) = pre else { continue };
            let (r, ch) = settle(&self.store, self.wait_ms, head, &name, pre, arrived_ms, force)?;
            judged += 1;
            if !was_verified && r.status == Status::Verified {
                now_verified += 1;
            }
            for c in &ch {
                head.apply(c);
            }
            groups.push(ch);
        }
        Ok((groups, judged, now_verified))
    }
}

/// Prepares many logs at once on the judges' threads (`skip_gone_sidecar`: one judged with its sidecar whose sidecar
/// is gone now is left out, as `None`). From a blocking thread; in batches, so their sidecars are never all in memory.
fn prepare_many(store: &Store, items: &[(String, bool, bool)], urgent: bool) -> std::io::Result<Vec<Option<Prepared>>> {
    let mut out = Vec::with_capacity(items.len());
    for part in items.chunks(1024) {
        let jobs: Vec<Box<dyn FnOnce() -> std::io::Result<Option<Prepared>> + Send>> = part
            .iter()
            .map(|(name, force, had_sidecar)| {
                let (store, name, force, had_sidecar) = (store.clone(), name.clone(), *force, *had_sidecar);
                Box::new(move || {
                    if had_sidecar && !store.exists(&side_of(&name)) {
                        return Ok(None);
                    }
                    prepare(&store, &name, force)
                }) as Box<dyn FnOnce() -> std::io::Result<Option<Prepared>> + Send>
            })
            .collect();
        for r in crate::pool::run_all(urgent, jobs)? {
            out.push(r?);
        }
    }
    Ok(out)
}

pub struct Judge {
    pub(crate) dir: PathBuf,
    pub(crate) store: Store,
    shared: Arc<Shared>,
    /// Arrivals waiting for their key, per key: whoever holds the key next judges them all.
    queued: Mutex<std::collections::HashMap<String, Vec<Queued>>>,
    pub(crate) wait_ms: u64,
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
pub(crate) struct Held {
    _file: ledger::Locked,
    _one: tokio::sync::OwnedMutexGuard<()>,
}

pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(std::io::Error::other)?
}

/// How a write is kept.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fold {
    /// Added to the journal, which is folded when it is long.
    WhenLong,
    /// Folded straight into the next generation, with the journal: all of it at once or (a crash, an error) none of
    /// it, as the new head is the one commit point. For changes that only make sense together, such as a new pin and
    /// every log judged again by it.
    Whole,
}

impl Judge {
    /// Judges the logs `store` keeps (its trust records are sealed with the same vault). Every judge of the same
    /// folder in this process shares its keys' locks, heads and records in memory.
    pub fn new(store: Store) -> Judge {
        let shared = Shared::of(store.dir());
        Judge::with_shared(store, shared)
    }

    /// A judge with memory of its own, as another process would have (tests: the owner's command beside the server).
    #[doc(hidden)]
    pub fn alone(store: Store) -> Judge {
        Judge::with_shared(store, Arc::new(Shared::new()))
    }

    fn with_shared(store: Store, shared: Arc<Shared>) -> Judge {
        Judge {
            dir: store.dir().to_path_buf(),
            store,
            shared,
            queued: Mutex::new(std::collections::HashMap::new()),
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
        self.shared.heads.lock().unwrap_or_else(PoisonError::into_inner).set_cap(heads);
        self
    }

    /// At most `records` records of all keys in memory (a key with more is read from its files every time).
    pub fn mirror_size(self, records: usize) -> Judge {
        let mut m = self.shared.mirrors();
        m.budget = records;
        m.over.clear();
        m.trim();
        drop(m);
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
        self.shared.heads.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Keys whose records are in memory now, and how many records that is.
    /// How many times a key's records were read from its files to mirror them (tests).
    #[doc(hidden)]
    pub fn mirror_tries(&self) -> u64 {
        self.shared.mirrors().tries
    }

    pub fn mirrored(&self) -> (usize, usize) {
        let m = self.shared.mirrors();
        (m.keys.len(), m.total())
    }

    /// Entries in the per-key lock table now (about the keys in use at this moment).
    pub fn lock_entries(&self) -> usize {
        self.shared.locks.len()
    }

    pub(crate) fn ledger(&self, key: &str) -> Ledger {
        Ledger::new(&self.dir, key, self.store.vault().clone())
    }

    pub(crate) fn ctx(&self) -> Ctx {
        Ctx {
            dir: self.dir.clone(),
            store: self.store.clone(),
            shared: self.shared.clone(),
            wait_ms: self.wait_ms,
            compact_after: self.compact_after,
            on_judged: self.on_judged.clone(),
        }
    }

    /// Holds a key: one task of this process at a time, and one process at a time (its lock file), until dropped.
    pub(crate) async fn hold(&self, key: &str) -> std::io::Result<Held> {
        let one = self.shared.locks.get(key).lock_owned().await;
        let ledger = self.ledger(key);
        let file = blocking(move || ledger.lock()).await?;
        Ok(Held { _file: file, _one: one })
    }

    /// The key's head (the caller holds the key): from memory while the files are as it last saw them, else from
    /// disk (another process changed them, or it isn't in memory).
    pub(crate) async fn take_head(&self, key: &str) -> std::io::Result<Head> {
        let cached = self.shared.heads.lock().unwrap_or_else(PoisonError::into_inner).take(key);
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

    pub(crate) fn put_head(&self, key: &str, head: Head) {
        self.shared.heads.lock().unwrap_or_else(PoisonError::into_inner).put(key.to_string(), head);
    }

    /// A log or sidecar finished arriving: judge its log if both are here (or mark it waiting).
    ///
    /// The log and its sidecar are read and checked first (the SHA-256, the signature), on the judges' threads,
    /// without holding the key: arrivals of one car are checked as many at once as there are cores. Only the verdict
    /// against the key's pairing and its writing hold the key, and whoever holds it next writes every arrival checked
    /// meanwhile with one sync ([`Judge::arrived_batch`]).
    pub async fn arrived(&self, name: &str) -> std::io::Result<Option<Record>> {
        let Some(n) = LogName::parse(name) else {
            return Ok(None);
        };
        let log_name = name.strip_suffix(".json").unwrap_or(name).to_string();
        let key = n.vehicle_key.to_string();
        let (store, ln) = (self.store.clone(), log_name.clone());
        let pre = crate::pool::run(true, move || prepare(&store, &ln, false)).await??;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let q = Queued { name: name.to_string(), log_name, sidecar: n.sidecar, pre, tx };
        self.queued.lock().unwrap_or_else(PoisonError::into_inner).entry(key.clone()).or_default().push(q);
        let held = self.hold(&key).await;
        // (taken with the key held: an arrival queued after this waits for the key itself)
        let batch = self.queued.lock().unwrap_or_else(PoisonError::into_inner).remove(&key).unwrap_or_default();
        match held {
            Ok(held) => {
                if !batch.is_empty() {
                    self.arrived_batch(held, &key, batch).await;
                }
            }
            Err(e) => {
                for q in batch {
                    let _ = q.tx.send(Err(copy_err(&e)));
                }
            }
        }
        rx.await.unwrap_or_else(|_| Err(std::io::Error::other("a log's judging stopped")))
    }

    /// Arrivals of one key, the key held: each judged against the head as the ones before it left it (in the order
    /// they came), all written at once, each told its record.
    async fn arrived_batch(&self, held: Held, key: &str, batch: Vec<Queued>) {
        let (mut items, mut txs) = (Vec::with_capacity(batch.len()), Vec::with_capacity(batch.len()));
        for q in batch {
            items.push((q.name, q.log_name, q.sidecar, q.pre));
            txs.push(q.tx);
        }
        let names: Vec<String> = items.iter().map(|i| i.0.clone()).collect();
        let head = match self.take_head(key).await {
            Ok(h) => h,
            Err(e) => {
                for tx in txs {
                    let _ = tx.send(Err(copy_err(&e)));
                }
                return;
            }
        };
        let (ctx, key_s) = (self.ctx(), key.to_string());
        let done = blocking(move || {
            let mut head = head;
            let marker = !head.waiting.is_empty();
            let mirror = ctx.shared.mirror_of(&key_s, &head);
            let ledger = ctx.ledger(&key_s);
            // the records judged so far in this batch (a log and its sidecar may both be in it)
            let mut fresh: std::collections::HashMap<String, Record> = std::collections::HashMap::new();
            let (mut groups, mut out) = (Vec::new(), Vec::new());
            for (_, log_name, sidecar, pre) in items {
                let one = || -> std::io::Result<Option<Record>> {
                    // read again when a file changed since (sent again in place of a failed one, packed): the verdict
                    // is always of the files as they are now
                    let pre = match pre {
                        Some(p)
                            if p.log == ctx.store.version(&log_name)
                                && p.side == ctx.store.version(&side_of(&log_name)) =>
                        {
                            Some(p)
                        }
                        _ => prepare(&ctx.store, &log_name, false)?,
                    };
                    let Some(pre) = pre else { return Ok(None) };
                    // when the log came: from the waiting list, or (a sidecar after its log was judged without it)
                    // its record
                    let arrived_ms = match head.waiting.get(&log_name) {
                        Some(&t) => Some(t),
                        None if sidecar => match fresh.get(&log_name) {
                            Some(r) => Some(r.arrived_ms),
                            None => match &mirror {
                                Some(m) => read_lock(m).logs.get(&log_name).map(|r| r.arrived_ms),
                                None => ledger.lookup(&head, &log_name)?.map(|r| r.arrived_ms),
                            },
                        },
                        None => None,
                    };
                    let (record, changes) = settle(
                        &ctx.store,
                        ctx.wait_ms,
                        &head,
                        &log_name,
                        pre,
                        arrived_ms.unwrap_or_else(now_ms),
                        false,
                    )?;
                    for c in &changes {
                        head.apply(c);
                    }
                    fresh.insert(log_name.clone(), record.clone());
                    groups.push(changes);
                    Ok(Some(record))
                };
                out.push(one());
            }
            let head = ctx.commit(&key_s, head, marker, groups, Fold::WhenLong)?;
            Ok((head, out))
        })
        .await;
        match done {
            Ok((head, out)) => {
                self.put_head(key, head);
                for ((name, tx), r) in names.iter().zip(txs).zip(out) {
                    // a log (or sidecar) the server had lost, back as it was: off the list of those it asks the car for
                    if let Err(e) = self.missing_arrived(key, name, r.as_ref().ok().and_then(Option::as_ref)).await {
                        // not the name: it holds the car's key, and the server's log is kept in the clear
                        tracing::warn!("CAN upload: could not update a car's missing logs: {e}");
                    }
                    let _ = tx.send(r);
                }
            }
            Err(e) => {
                for tx in txs {
                    let _ = tx.send(Err(copy_err(&e)));
                }
            }
        }
        drop(held);
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
        let head = self.take_head(key).await?;
        let now = now_ms();
        let due: Vec<(String, u64)> = head
            .waiting
            .iter()
            .filter(|&(_, &t)| now.saturating_sub(t) >= self.wait_ms)
            .map(|(k, &t)| (k.clone(), t))
            .collect();
        let n = due.len();
        let (ctx, key_s) = (self.ctx(), key.to_string());
        let head = blocking(move || {
            let mut head = head;
            let want: Vec<(String, bool, bool)> = due.iter().map(|(name, _)| (name.clone(), true, false)).collect();
            let prepared = prepare_many(&ctx.store, &want, true)?;
            let items = due.into_iter().map(|(name, t)| (name, t, true, true)).collect();
            let (groups, _, _) = ctx.settle_all(&mut head, items, prepared)?;
            // the marker is there (that is how this key was found): commit removes it once nothing waits
            ctx.commit(&key_s, head, true, groups, Fold::WhenLong)
        })
        .await?;
        self.put_head(key, head);
        Ok(n)
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
        let (head, n, _) = self.rejudge_locked(key, head, vec![rules], |_| true).await?;
        self.put_head(key, head);
        Ok(n)
    }

    /// The logs `which` picks again, in name order (the key held), with `changes` already applied to `head` (a new
    /// pin). Nothing is written until every one is judged; then it all goes into the next generation at once, so a
    /// crash or an error part way (a log that can't be read) leaves the key exactly as it was, never half judged by a
    /// new pin. The logs are read and checked on all the judges' threads at once, then judged in name order.
    ///
    /// A log whose file is gone, or that was judged with its sidecar and whose sidecar is gone now (a backup put back
    /// in part, a move under way), keeps its record: judging it again could only make up a failure. Returns the head,
    /// how many were judged again, and how many of those that were not verified are verified now.
    async fn rejudge_locked(
        &self,
        key: &str,
        head: Head,
        changes: Vec<Change>,
        which: impl Fn(&Record) -> bool + Send + 'static,
    ) -> std::io::Result<(Head, usize, usize)> {
        let (ctx, key_s) = (self.ctx(), key.to_string());
        blocking(move || {
            let mut head = head;
            // (name, arrived, judged with or without its sidecar already, had its sidecar, verified)
            let names: Vec<(String, u64, bool, bool, bool)> = ctx.view(&key_s, &head, |v| {
                let mut out = Vec::new();
                v.for_each(|name, r| {
                    if which(r) {
                        out.push((
                            name.to_string(),
                            r.arrived_ms,
                            r.status != Status::Waiting,
                            r.had_sidecar(),
                            r.status == Status::Verified,
                        ));
                    }
                    ControlFlow::Continue(())
                })?;
                Ok(out)
            })?;
            let marker = !head.waiting.is_empty();
            let want: Vec<(String, bool, bool)> = names.iter().map(|n| (n.0.clone(), n.2, n.3)).collect();
            let prepared = prepare_many(&ctx.store, &want, false)?;
            let items = names.into_iter().map(|(n, t, force, _, was)| (n, t, force, was)).collect();
            let (groups, judged, now_verified) = ctx.settle_all(&mut head, items, prepared)?;
            let mut all = vec![changes];
            all.extend(groups);
            let head = ctx.commit(&key_s, head, marker, all, Fold::Whole)?;
            Ok((head, judged, now_verified))
        })
        .await
    }

    /// Once per key after an update to newer rules ([`trust::RULES_VERSION`]): its logs that failed their check (or
    /// could not be signed) and were judged by older rules are judged again, all at once (see
    /// [`Judge::rejudge_locked`]); verified logs are left as they are. Nothing is sent again. `None`: the key was
    /// judged by these rules already.
    pub async fn rejudge_old(&self, key: &str) -> std::io::Result<Option<Rejudged>> {
        if !ledger::valid_key(key) {
            return Err(bad_key());
        }
        let _held = self.hold(key).await?;
        let mut head = self.take_head(key).await?;
        if head.v >= trust::RULES_VERSION {
            self.put_head(key, head);
            return Ok(None);
        }
        let rules = Change::Rules { v: trust::RULES_VERSION };
        head.apply(&rules);
        let old = |r: &Record| r.v < trust::RULES_VERSION && matches!(r.status, Status::Failed | Status::CouldNotSign);
        let (head, judged, verified) = self.rejudge_locked(key, head, vec![rules], old).await?;
        self.put_head(key, head);
        Ok(Some(Rejudged { judged: judged as u64, verified: verified as u64 }))
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
        let (head, _, _) = self.rejudge_locked(key, head, changes, other_key).await?;
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
            l.for_each(|_, r| {
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
        self.read(key, move |l, _| {
            let mut last = std::collections::VecDeque::with_capacity(n + 1);
            l.for_each(|name, r| {
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
        self.read(key, move |l, _| {
            let mut last = std::collections::VecDeque::with_capacity(n + 1);
            l.for_each(|name, r| {
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

    /// When a car key's first and newest logs arrived and how many wait for their sidecar, from its records (what a
    /// whole [`Judge::book`] gives, without a copy of every record). All `None` and 0 for a key with nothing here.
    pub async fn arrivals(&self, key: &str) -> Arrivals {
        self.read(key, |l, _| {
            let mut a = Arrivals::default();
            l.for_each(|_, r| {
                a.first_ms = Some(a.first_ms.map_or(r.arrived_ms, |t| t.min(r.arrived_ms)));
                a.last_ms = Some(a.last_ms.map_or(r.arrived_ms, |t| t.max(r.arrived_ms)));
                if r.status == Status::Waiting {
                    a.waiting += 1;
                }
                ControlFlow::Continue(())
            })?;
            Ok(a)
        })
        .await
        .unwrap_or_default()
    }

    /// When a car key's newest log arrived (its upload), `None` before its first (each record read, none kept).
    pub async fn last_arrived(&self, key: &str) -> std::io::Result<Option<u64>> {
        self.read(key, |l, h| {
            let mut last: Option<u64> = h.waiting.values().copied().max();
            l.for_each(|_, r| {
                last = last.max(Some(r.arrived_ms));
                ControlFlow::Continue(())
            })?;
            Ok(last)
        })
        .await
    }

    /// Reads a key's records with `f`: from the key's mirror without holding the key while its files are still the
    /// ones the mirror was made from (an upload being written, a rebuild reading every record: neither waits for the
    /// other), else with the key held ([`Judge::read_held`]).
    pub(crate) async fn read<T: Send + 'static>(
        &self,
        key: &str,
        f: impl Fn(&View, &Head) -> std::io::Result<T> + Send + Sync + 'static,
    ) -> std::io::Result<T> {
        if !ledger::valid_key(key) {
            return Err(bad_key());
        }
        let f = Arc::new(f);
        if let Some(r) = self.read_quick(key, f.clone()).await? {
            return Ok(r);
        }
        let held = self.hold(key).await?;
        self.read_held(held, key, f).await
    }

    /// `f` over the key's mirror when the key's files are still the ones it was made from (checked now: a few
    /// bytes of its head file and its journal's size), without holding the key. `None`: no such mirror.
    async fn read_quick<T: Send + 'static>(
        &self,
        key: &str,
        f: Arc<impl Fn(&View, &Head) -> std::io::Result<T> + Send + Sync + 'static>,
    ) -> std::io::Result<Option<T>> {
        let Some(m) = self.shared.mirror(key) else { return Ok(None) };
        let ledger = self.ledger(key);
        blocking(move || {
            let g = read_lock(&m);
            if g.head.stamp.is_none() || g.head.stamp != Some(ledger.stamp(g.head.generation)?) {
                return Ok(None);
            }
            let v = View::Mem(&g);
            Ok(Some(f(&v, &g.head)?))
        })
        .await
    }

    /// [`Judge::read`] with the key held: from its mirror (made now when there is none and its records fit), else
    /// from its files. A process that doesn't take the lock file (an older version) may fold the journal at the same
    /// moment: a generation that vanished while being read is read again from the new head.
    async fn read_held<T: Send + 'static>(
        &self,
        _held: Held,
        key: &str,
        f: Arc<impl Fn(&View, &Head) -> std::io::Result<T> + Send + Sync + 'static>,
    ) -> std::io::Result<T> {
        let mut tries = 0;
        loop {
            let head = self.take_head(key).await?;
            let (ctx, key_s, f) = (self.ctx(), key.to_string(), f.clone());
            let (head, r) = tokio::task::spawn_blocking(move || {
                let r = ctx.view_read(&key_s, &head, |v| f(v, v.head()));
                (head, r)
            })
            .await
            .map_err(std::io::Error::other)?;
            tries += 1;
            match r {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && tries < 3 => {
                    self.shared.forget_mirror(key);
                    continue;
                }
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
    /// pending; so are all of them when the key is held longer than [`CHECK_WAIT`] (a long judging, or a judging of
    /// every log again: never a failure made up meanwhile).
    ///
    /// The chain is read afresh on every call, so a log whose previous log arrives, or is sent again and verifies, is
    /// told of at once on the next call: nothing about the chain is stored with a log.
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
        let wanted: Arc<HashSet<String>> = Arc::new(logs.iter().cloned().collect());
        let (wait_ms, now) = (self.wait_ms, now_ms());
        let f = Arc::new(move |l: &View, _: &Head| {
            let w = wanted.clone();
            let mut out = HashMap::new();
            checks_pass(l, &|name: &str| w.contains(name), &uploading, now, wait_ms, |name, _, c| {
                out.insert(name.to_string(), c);
            })?;
            Ok(out)
        });
        // from memory, the key not held, while its records there are the ones on disk: never behind an upload
        // being written or a rebuild reading them
        let found = match self.read_quick(key, f.clone()).await? {
            Some(found) => found,
            None => {
                let Ok(held) = tokio::time::timeout(CHECK_WAIT, self.hold(key)).await else {
                    return Ok(pending());
                };
                self.read_held(held?, key, f).await?
            }
        };
        let mut all = pending();
        all.extend(found);
        Ok(all)
    }

    /// The check of every log of a car key as the CAN Bridge app would be told it now, counted per result and
    /// `check_reason`, with the runs of failed logs (by their start times). Names no log and no key: for an owner to
    /// pass on (`sentry-mate log-checks`) and for Data health. One pass over the key's records; only reads.
    pub async fn log_checks(
        &self,
        key: &str,
        uploading: std::collections::HashSet<String>,
    ) -> std::io::Result<LogChecks> {
        let (wait_ms, now) = (self.wait_ms, now_ms());
        self.read(key, move |l, _| {
            let mut c = LogChecks::default();
            let mut run: Option<FailedRun> = None;
            checks_pass(l, &|_| true, &uploading, now, wait_ms, |name, _, check| {
                c.add(check);
                let start = LogName::parse(name).map_or(0, |n| n.started_ms);
                match (check, &mut run) {
                    (UploadCheck::Failed(f), Some(r)) => {
                        r.last_start_ms = r.last_start_ms.max(start);
                        r.logs += 1;
                        r.add(f);
                    }
                    (UploadCheck::Failed(f), None) => {
                        let mut r = FailedRun { first_start_ms: start, last_start_ms: start, ..FailedRun::default() };
                        r.logs = 1;
                        r.add(f);
                        run = Some(r);
                    }
                    (_, r) => {
                        if let Some(r) = r.take() {
                            c.push_run(r);
                        }
                    }
                }
            })?;
            if let Some(r) = run {
                c.push_run(r);
            }
            Ok(c)
        })
        .await
    }

    /// Whether a stored log failed its check, so that a new upload of its name (or its sidecar's) replaces it.
    pub async fn replaceable(&self, log_name: &str, uploading: std::collections::HashSet<String>) -> bool {
        let Some(n) = LogName::parse(log_name).filter(|n| !n.sidecar) else { return false };
        // the owner chose to use this copy (an unsigned log): another one sent under its name would take its place
        // with their word on it, so it is kept as it is
        if self.record(n.vehicle_key, log_name).await.is_some_and(|r| r.override_use) {
            return false;
        }
        let checks = self.upload_checks(n.vehicle_key, &[log_name.to_string()], uploading).await;
        checks.is_ok_and(|c| matches!(c.get(log_name), Some(UploadCheck::Failed(_))))
    }

    /// A key's whole state with every record (its whole history: one key at a time).
    pub async fn book(&self, key: &str) -> Book {
        self.read(key, |l, h| {
            let mut logs = BTreeMap::new();
            l.for_each(|name, r| {
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
        self.read(key, move |l, _| l.lookup(&name)).await.ok().flatten()
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
                l.for_each(|name, r| {
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
