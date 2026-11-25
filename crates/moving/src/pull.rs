//! The new server's side of a direct move (`docs/plans/move-server.md`, sections 2 and 3): the claim, the copy
//! rounds while the old server keeps taking uploads, the pause and the last round, the finish, and the switch.
//!
//! Everything is staged in `<data>/.moving/` (a dot folder: a half-moved folder never looks like a data folder):
//!
//! - `data/`: the copy, at the same paths, times kept. A file there is always one whole version of the old file: it
//!   is fetched into `part/<n>` first, given the old file's time, synced, and only then renamed into place. A growing
//!   file's catch-up is added to its copy, synced, then timed.
//! - `state.sealed`: where the move is, sealed with a key of its own, `<keys>/.moving/state.key`: the address, the
//!   old certificate's SHA-256, the session, the keys from the code, the recovery part, the files not settled yet
//!   (racy ones, pieces of big files with the version they continue), and once finishing, the finish answer. A
//!   restart reads it and goes on.
//! - `lock`: held shared while the copy writes, exclusively by `move cancel` and `move finish --recovery-key`.
//!
//! The copy can always be opened with the recovery key: the claim answer carries the old vault's recovery part.

use crate::Error;
use crate::client::{Address, Client, NetError};
use crate::code::MoveCode;
use crate::proto::{self, ClaimAnswer, ClaimRequest, End, FinishAnswer, FinishRequest, Frame, Line, Want};
use crate::tree::{self, Kind, Purpose, Skips, walk_cmp};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rustix::fs::{FlockOperation, flock};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtOrd};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use vault::Vault;
use zeroize::Zeroizing;

/// Where a move coming in is staged, in the data folder and in the keys folder.
pub const STAGING: &str = ".moving";
const BEING_MADE: &str = ".moving.new";
const BEING_REMOVED: &str = ".moving.done";
const STATE: &str = "state.sealed";
const STATE_KEY: &str = "state.key";
const LOCK: &str = "lock";
/// Put there by `move finish --recovery-key`: switch to what was copied.
const FORCED: &str = "forced";
/// Put there by `move cancel` before it waits for the lock: the copy stops at its next step.
const CANCELLING: &str = "cancelling";
/// Held (flock, shared) by [`run`] for as long as it runs: `move finish --recovery-key` sees a copy under way.
const RUNNING: &str = "running";
const PARTS: &str = "part";
/// The note a move leaves on the new server (`.move/moved-in.sealed`).
pub const MOVED_IN: &str = "moved-in.sealed";
const STATE_MAGIC: &[u8; 5] = b"SMMS\x01";

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Why the move doesn't go on now: in words for the owner, with a code for the UI (`problem` events), and whether it
/// carries on by itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct Problem {
    pub code: String,
    pub message: String,
    /// It tries again by itself (a network cut, a full disk); else it waits for the owner.
    pub retry: bool,
}

impl Problem {
    fn new(code: &str, message: impl Into<String>, retry: bool) -> Problem {
        Problem { code: code.into(), message: message.into(), retry }
    }

    fn io(e: io::Error) -> Problem {
        if e.raw_os_error() == Some(28) || e.kind() == io::ErrorKind::StorageFull {
            return Problem::new(
                "no_space",
                format!(
                    "the disk is full ({e}); it carries on by itself when there is room (checked every minute); to give up: `move cancel`"
                ),
                true,
            );
        }
        Problem::new("disk", format!("writing the copy failed ({e}); trying again"), true)
    }

    fn stopped() -> Problem {
        Problem::new("stopped", "stopped", false)
    }

    fn cancelled_here() -> Problem {
        Problem::new("cancelled_here", "the move was cancelled here", false)
    }

    fn net(addr: &Address, e: &NetError) -> Problem {
        match e {
            NetError::WrongCertificate(_) => {
                Problem::new("not_proven", format!("{e}; nothing was sent. Trying again"), true)
            }
            NetError::Connect(_, io) => Problem::new("unreachable", format!("nothing answers at {addr} ({io})"), true),
            NetError::Resolve(h, io) => Problem::new("unreachable", format!("can't find {h} ({io})"), true),
            NetError::Io(io) if matches!(io.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                Problem::new("unreachable", "lost the connection to the old server (no answer in time)", true)
            }
            NetError::Io(_) => Problem::new("unreachable", e.to_string(), true),
            _ => Problem::new("unreachable", format!("lost the connection to the old server ({e})"), true),
        }
    }
}

impl From<vault::Error> for Problem {
    fn from(e: vault::Error) -> Problem {
        Problem::from(Error::from(e))
    }
}

impl From<Error> for Problem {
    fn from(e: Error) -> Problem {
        match e {
            Error::Io(e) => Problem::io(e),
            e => Problem::new("disk", e.to_string(), true),
        }
    }
}

/// Where the move is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// The code is being proven to the old server.
    Claiming,
    /// The copy rounds, the pause, the last round, the finish.
    Copying,
    /// The finish answer is saved: the switch is finished on the next start, whatever happens.
    Switching,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Claimed {
    session: String,
    cert_sha256: String,
    app: String,
    files: u64,
    bytes: u64,
    /// The old vault's recovery part (JSON of [`vault::RecoveryPart`]).
    recovery: String,
    at_ms: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PartRec {
    n: u64,
    v: String,
    len: u64,
    got: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct FinishRec {
    moved_ms: u64,
    #[serde(default)]
    files: u64,
    #[serde(default)]
    bytes: u64,
    key_id: String,
    /// The data key wrapped with the code's wrap key (none for a switch forced with the recovery key).
    data_key: Option<vault::WrappedKey>,
    forced: bool,
}

/// How far a move is (saved in the state; `move status` and the UI show it).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Progress {
    pub round: u32,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct State {
    v: u32,
    phase: Phase,
    address: Address,
    code_id: String,
    auth: String,
    wrap: String,
    claim: String,
    started_ms: u64,
    #[serde(default)]
    claimed: Option<Claimed>,
    /// A copy kept from a move cancelled on the old server: carried on if the next claim proves this certificate.
    #[serde(default)]
    kept_cert: Option<String>,
    #[serde(default)]
    parts: BTreeMap<String, PartRec>,
    #[serde(default)]
    next_part: u64,
    /// Fetched within 2 s of their time: fetched again next round.
    #[serde(default)]
    racy: BTreeSet<String>,
    /// Answered "changed" while being carried on: fetched whole next round.
    #[serde(default)]
    refetch: BTreeSet<String>,
    #[serde(default)]
    progress: Progress,
    #[serde(default)]
    cancelled: bool,
    #[serde(default)]
    problem: Option<Problem>,
    #[serde(default)]
    skipped: Vec<proto::SkippedLine>,
    #[serde(default)]
    skipped_count: u64,
    #[serde(default)]
    finish: Option<FinishRec>,
    #[serde(default)]
    installed: bool,
    /// The move's own id, made by the first [`begin`] and kept when a stopped move carries on with a new code (the
    /// web pages' SetupJob id). Empty in a state an older version wrote: the code id stands in.
    #[serde(default)]
    job_id: String,
    /// The pause was had: the last round and the finish run.
    #[serde(default)]
    finishing: bool,
    /// Top-level names a first start wrote that don't count as the owner's ([`Options::first_start_also`]).
    #[serde(default)]
    first_start_also: Vec<String>,
    /// The move's log for the owner ([`crate::log`]): carried into the moved-in note at the switch.
    #[serde(default)]
    log: Vec<crate::log::Line>,
}

/// A new move's id: 8 random hex digits.
fn new_job_id() -> String {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    hex::encode(b)
}

impl State {
    fn job_id(&self) -> String {
        if self.job_id.is_empty() { self.code_id.clone() } else { self.job_id.clone() }
    }
}

/// The problems after which a stopped move carries on with a new move code from the same old server (the copy so far
/// kept): cancelled there, its session ended there, or the old server didn't prove itself.
pub const CARRIES_ON: &[&str] = &["cancelled", "bad_session", "not_proven"];

fn state_cipher(key: &[u8; 32]) -> XChaCha20Poly1305 {
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(b"sentry-mate move state v1"), key);
    let mut k = Zeroizing::new([0u8; 32]);
    let _ = hk.expand(b"seal", &mut *k);
    XChaCha20Poly1305::new((&*k).into())
}

const STATE_AAD: &[u8] = b"sentry-mate .moving/state.sealed";

fn seal_state(key: &[u8; 32], st: &State) -> Result<Vec<u8>, Error> {
    let plain = Zeroizing::new(serde_json::to_vec(st).map_err(|e| Error::Io(io::Error::other(e.to_string())))?);
    let mut nonce = [0u8; 24];
    getrandom::fill(&mut nonce).map_err(|e| Error::Io(io::Error::other(e.to_string())))?;
    let ct = state_cipher(key)
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: &plain, aad: STATE_AAD })
        .map_err(|_| Error::Io(io::Error::other("sealing the move's state failed")))?;
    let mut out = STATE_MAGIC.to_vec();
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn open_state(key: &[u8; 32], b: &[u8]) -> Option<State> {
    let rest = b.strip_prefix(STATE_MAGIC)?;
    if rest.len() < 24 {
        return None;
    }
    let plain = Zeroizing::new(
        state_cipher(key)
            .decrypt(XNonce::from_slice(&rest[..24]), Payload { msg: &rest[24..], aad: STATE_AAD })
            .ok()?,
    );
    serde_json::from_slice(&plain).ok()
}

/// Writes `name` in `dir` whole or not at all, and never makes `dir` (a staging folder removed meanwhile stays
/// removed).
fn write_in(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    {
        let mut f = crate::export::create_private(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join(name))?;
    crate::import::sync_dir(dir);
    Ok(())
}

fn read_key(keys: &Path) -> Option<Zeroizing<[u8; 32]>> {
    let b = Zeroizing::new(std::fs::read(keys.join(STAGING).join(STATE_KEY)).ok()?);
    let k: [u8; 32] = b.as_slice().try_into().ok()?;
    Some(Zeroizing::new(k))
}

/// The state key and the state.
type Loaded = (Zeroizing<[u8; 32]>, State);

fn load(data: &Path, keys: &Path) -> Result<Option<Loaded>, Error> {
    let staging = data.join(STAGING);
    if !staging.is_dir() {
        return Ok(None);
    }
    let unreadable = || Error::Move("the half-done move here can't be read: remove it with `move cancel`".into());
    let b = std::fs::read(staging.join(STATE)).map_err(|_| unreadable())?;
    let key = read_key(keys).ok_or_else(unreadable)?;
    let st = open_state(&key, &b).ok_or_else(unreadable)?;
    Ok(Some((key, st)))
}

/// What is waiting in `.moving/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    None,
    /// A move is under way (or waits for the owner, with the problem it had).
    Moving {
        phase: Phase,
        percent: u8,
        from: String,
        code_id: String,
        problem: Option<Problem>,
        cancelled: bool,
        /// `move finish --recovery-key` (or the web's finish) asked for the switch: it runs at once.
        forced: bool,
    },
    /// There, but its state can't be read.
    Unreadable,
}

/// What is waiting in `.moving/` (cheap when nothing is: one look for the folder).
pub fn pending(data: &Path, keys: &Path) -> Pending {
    if data.join(STAGING).join(CANCELLING).exists() {
        // being removed by `move cancel` (or what a stop part way through it left: removed on the next start)
        return Pending::None;
    }
    match load(data, keys) {
        Ok(None) => Pending::None,
        Ok(Some((_, st))) => Pending::Moving {
            phase: st.phase,
            percent: percent(&st.progress),
            from: st.address.to_string(),
            code_id: st.code_id,
            problem: st.problem,
            cancelled: st.cancelled,
            forced: data.join(STAGING).join(FORCED).exists(),
        },
        Err(_) => Pending::Unreadable,
    }
}

fn percent(p: &Progress) -> u8 {
    if p.bytes_total == 0 {
        return 0;
    }
    (p.bytes_done.saturating_mul(100) / p.bytes_total).min(99) as u8
}

/// What `move status` shows of a move coming in.
#[derive(Debug, Clone)]
pub struct Status {
    pub phase: Phase,
    pub from: String,
    pub code_id: String,
    pub old_app: Option<String>,
    pub cert_sha256: Option<String>,
    pub progress: Progress,
    pub problem: Option<Problem>,
    pub cancelled: bool,
    pub skipped: Vec<proto::SkippedLine>,
    pub skipped_count: u64,
    pub started_ms: u64,
    /// The move's id ([`begin`] makes it; kept when a stopped move carries on with a new code).
    pub job_id: String,
    /// The pause was had: the last round and the finish run.
    pub finishing: bool,
    /// A switch with the recovery key was asked for and waits to run.
    pub forced: bool,
    /// The move's log ([`crate::log`]).
    pub log: Vec<crate::log::Line>,
}

pub fn status(data: &Path, keys: &Path) -> Result<Option<Status>, Error> {
    let forced = data.join(STAGING).join(FORCED).exists();
    Ok(load(data, keys)?.map(|(_, st)| Status {
        job_id: st.job_id(),
        finishing: st.finishing,
        forced,
        phase: st.phase,
        from: st.address.to_string(),
        code_id: st.code_id,
        old_app: st.claimed.as_ref().map(|c| c.app.clone()),
        cert_sha256: st.claimed.as_ref().map(|c| c.cert_sha256.clone()),
        progress: st.progress,
        problem: st.problem,
        cancelled: st.cancelled,
        skipped: st.skipped,
        skipped_count: st.skipped_count,
        started_ms: st.started_ms,
        log: st.log,
    }))
}

/// Removes what a crash left of a staging folder being made or removed. Run on start, before anything else.
pub fn tidy(data: &Path, keys: &Path) {
    if data.join(STAGING).join(CANCELLING).exists() {
        // a `move cancel` stopped part way
        let _ = discard(data, keys);
    }
    let _ = crate::import::remove_all(&data.join(BEING_MADE));
    let _ = crate::import::remove_all(&keys.join(BEING_MADE));
    let _ = crate::import::remove_all(&data.join(BEING_REMOVED));
    if !data.join(STAGING).exists() {
        let _ = crate::import::remove_all(&keys.join(STAGING));
    }
}

/// How a move runs.
pub struct Options {
    /// This Sentry Mate's version (an older one than the old server's is refused in the claim).
    pub app: String,
    /// Free space always left (ingest's `MIN_FREE`).
    pub min_free: u64,
    pub free_bytes: Box<dyn Fn(&Path) -> u64 + Send + Sync>,
    /// Connections that fetch at once.
    pub connections: usize,
    /// Set to stop at the next step (the server is stopping); it carries on from there on the next start.
    pub stop: Arc<AtomicBool>,
    /// A round that finds at most this much to fetch is followed by the pause.
    pub small_bytes: u64,
    pub small_files: u64,
    /// The longest wait between tries after a network problem (from 1 s, doubling).
    pub retry_most: Duration,
    /// The wait after a round when the old server changes faster than the copy catches up.
    pub between_rounds: Duration,
    /// How long a request may wait for the old server's bytes.
    pub timeout: Duration,
    /// Stop when the switch is next instead of switching (`Err` with the code [`SWITCH_NEXT`]): a running server
    /// stops its web side first, then switches on its next start.
    pub stop_before_switch: bool,
    /// Top-level names in the data folder that count as a first start's too, not the owner's (a move begun from the
    /// web pages: [`crate::import::WEB_FIRST_START`]); set aside at the switch. Kept in the move's state by [`begin`].
    pub first_start_also: Vec<String>,
}

/// The problem code of a run told to stop before the switch ([`Options::stop_before_switch`]).
pub const SWITCH_NEXT: &str = "switch_next";

impl Options {
    pub fn new(app: &str) -> Options {
        Options {
            app: app.to_string(),
            min_free: 2 << 30,
            free_bytes: Box::new(crate::import::free_space),
            connections: 4,
            stop: Arc::new(AtomicBool::new(false)),
            small_bytes: 64 << 20,
            small_files: 1000,
            retry_most: Duration::from_secs(60),
            between_rounds: Duration::from_secs(60),
            timeout: Duration::from_secs(120),
            stop_before_switch: false,
            first_start_also: Vec::new(),
        }
    }
}

/// What a finished move brought.
#[derive(Debug, Clone)]
pub struct Done {
    pub from: String,
    pub cert_sha256: String,
    pub old_app: String,
    pub files: u64,
    pub bytes: u64,
    pub moved_ms: u64,
    /// The old server ran on this machine (another port, or another app on the same machine).
    pub same_machine: bool,
    /// Switched with the recovery key, without the old server's finish.
    pub forced: bool,
    /// A first start's vault was set aside and removed.
    pub replaced_first_start: bool,
}

/// The note a move leaves on the new server.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct MovedInNote {
    from: Address,
    cert_sha256: String,
    session: String,
    code_id: String,
    moved_ms: u64,
    at_ms: u64,
    files: u64,
    bytes: u64,
    forced: bool,
    switched_sent: bool,
    #[serde(default)]
    job_id: String,
    #[serde(default)]
    started_ms: u64,
    #[serde(default)]
    same_machine: bool,
    /// The move's log, carried from the staged state, and what this server did after the switch.
    #[serde(default)]
    log: Vec<crate::log::Line>,
}

const MOVED_IN_LABEL: &str = ".move/moved-in.sealed";

fn moved_in_path(data: &Path) -> PathBuf {
    data.join(crate::import::NOTES).join(MOVED_IN)
}

/// Where this server's data came from by a move, and whether the old server was told it serves.
#[derive(Debug, Clone)]
pub struct MovedIn {
    pub from: String,
    pub code_id: String,
    pub moved_ms: u64,
    pub confirmed: bool,
    pub forced: bool,
    /// The move's id (the code id for a note an older version wrote).
    pub job_id: String,
    pub started_ms: u64,
    /// When the switch was made.
    pub at_ms: u64,
    /// The old server's upload certificate SHA-256, as the claim had it (this server serves the one in its `tls`
    /// folder: the same after a full copy, a new one made at start when the copy had none).
    pub cert_sha256: String,
    /// The old server ran on this machine.
    pub same_machine: bool,
    /// The move's log.
    pub log: Vec<crate::log::Line>,
}

/// The note of a move that brought this server's data (none if it didn't come by a move).
pub fn moved_in(data: &Path, vault: &Vault) -> Option<MovedIn> {
    let b = vault.read_file(&moved_in_path(data), MOVED_IN_LABEL).ok()??;
    let n: MovedInNote = serde_json::from_slice(&b).ok()?;
    Some(MovedIn {
        from: n.from.to_string(),
        code_id: n.code_id.clone(),
        moved_ms: n.moved_ms,
        confirmed: n.switched_sent,
        forced: n.forced,
        job_id: if n.job_id.is_empty() { n.code_id.clone() } else { n.job_id },
        started_ms: n.started_ms,
        at_ms: n.at_ms,
        cert_sha256: n.cert_sha256,
        same_machine: n.same_machine,
        log: n.log,
    })
}

/// Held while the moved-in note is read and written again (its log and its `switched` mark are changed from two
/// tasks).
static NOTE_WRITE: Mutex<()> = Mutex::new(());

/// Adds a line to the log of the move that brought this server's data (what it started after the switch). `false`:
/// there is no such note, or it couldn't be written.
pub fn note_moved_in(data: &Path, vault: &Vault, text: &str) -> bool {
    let _one = lock(&NOTE_WRITE);
    let Ok(Some(b)) = vault.read_file(&moved_in_path(data), MOVED_IN_LABEL) else { return false };
    let Ok(mut n) = serde_json::from_slice::<MovedInNote>(&b) else { return false };
    crate::log::push(&mut n.log, now_ms(), text);
    let Ok(b) = serde_json::to_vec(&n).map(Zeroizing::new) else { return false };
    vault.write_file(&moved_in_path(data), MOVED_IN_LABEL, &b).is_ok()
}

/// The log of the move coming in here (staged, not switched yet): empty when there is none.
pub fn log(data: &Path, keys: &Path) -> Vec<crate::log::Line> {
    load(data, keys).ok().flatten().map(|(_, st)| st.log).unwrap_or_default()
}

/// Writes the note a switch leaves, from `from` with the old server's certificate `cert_sha256` (tests of what a
/// moved-in server shows: a real one needs two servers). Nothing else of a move is made.
#[doc(hidden)]
pub fn mark_moved_in_for_tests(
    data: &Path,
    vault: &Vault,
    from: Address,
    cert_sha256: &str,
    forced: bool,
) -> Result<(), Error> {
    let now = now_ms();
    let n = MovedInNote {
        from,
        cert_sha256: cert_sha256.to_string(),
        session: String::new(),
        code_id: "TESTS".into(),
        moved_ms: now,
        at_ms: now,
        files: 0,
        bytes: 0,
        forced,
        switched_sent: forced,
        job_id: "0badc0de".into(),
        started_ms: now,
        same_machine: true,
        log: Vec::new(),
    };
    let b = Zeroizing::new(serde_json::to_vec(&n).map_err(|e| Error::Move(e.to_string()))?);
    std::fs::create_dir_all(data.join(crate::import::NOTES))?;
    vault.write_file(&moved_in_path(data), MOVED_IN_LABEL, &b)?;
    Ok(())
}

/// Tells the old server this one serves the data now (`switched`), once, after the switch. `Ok(true)`: told now;
/// `Ok(false)`: nothing to tell. The old server's log then says "done".
pub fn confirm(data: &Path, vault: &Vault, timeout: Duration) -> Result<bool, Problem> {
    let Some(b) = vault.read_file(&moved_in_path(data), MOVED_IN_LABEL).map_err(Problem::from)? else {
        return Ok(false);
    };
    let mut n: MovedInNote = serde_json::from_slice(&b).map_err(|e| Problem::new("disk", e.to_string(), false))?;
    if n.switched_sent || n.forced {
        return Ok(false);
    }
    let mut c = Client::pinned(n.from.clone(), &n.cert_sha256).map_err(|e| Problem::net(&n.from, &e))?;
    c.set_timeout(timeout);
    let body = serde_json::to_vec(&proto::SwitchedRequest { at_ms: now_ms() }).unwrap_or_default();
    let bearer = format!("Bearer {}", n.session);
    let r = c
        .request(
            "POST",
            &format!("{}/switched", proto::BASE),
            &[("Authorization", &bearer), ("Content-Type", "application/json")],
            &body,
        )
        .map_err(|e| Problem::net(&n.from, &e))?;
    if r.status != 200 {
        let status = r.status;
        let (code, _) = r.error().map_err(|e| Problem::net(&n.from, &e))?;
        return Err(Problem::new(&code, format!("the old server answered {status} {code} to `switched`"), true));
    }
    let _ = r.bytes(4096);
    // read again under the note's lock: a line added to its log meanwhile stays
    let _one = lock(&NOTE_WRITE);
    if let Ok(Some(b)) = vault.read_file(&moved_in_path(data), MOVED_IN_LABEL)
        && let Ok(again) = serde_json::from_slice::<MovedInNote>(&b)
    {
        n = again;
    }
    n.switched_sent = true;
    crate::log::push(&mut n.log, now_ms(), "Told the old server that this server serves the data now.");
    let b = Zeroizing::new(serde_json::to_vec(&n).map_err(|e| Problem::new("disk", e.to_string(), false))?);
    vault.write_file(&moved_in_path(data), MOVED_IN_LABEL, &b).map_err(Problem::from)?;
    Ok(true)
}

/// Starts a move into this server: checks the code and that the server is new, stages `.moving/` and proves the
/// code to the old server (the claim, tried once). A move already staged here with this code is carried on as it
/// is; one with another code that was cancelled on the old server, or never claimed, takes the new code (and keeps
/// its copy if the new claim proves the same old certificate).
pub fn begin(data: &Path, keys: &Path, address: &str, code: &str, opts: &Options) -> Result<(), Problem> {
    let code = MoveCode::parse(code).map_err(|e| Problem::new("wrong_code", e.to_string(), false))?;
    let address = Address::parse(address).map_err(|e| Problem::new("address", e, false))?;
    let k = code.keys();
    let mut claim = [0u8; 16];
    getrandom::fill(&mut claim).map_err(|e| Problem::new("disk", e.to_string(), false))?;
    let fresh_words = format!("Moving here from {address}: proving the move code to the old server.");
    match load(data, keys).map_err(|e| Problem::new("unreadable", e.to_string(), false))? {
        Some((key, mut st)) => {
            if st.code_id == k.id {
                return Ok(());
            }
            if st.phase == Phase::Switching {
                return Err(Problem::new("finished", "a move is switching here already", false));
            }
            let carries_on = st.problem.as_ref().is_some_and(|p| !p.retry && CARRIES_ON.contains(&p.code.as_str()));
            if st.phase == Phase::Copying && !st.cancelled && !carries_on {
                return Err(Problem::new(
                    "move_under_way",
                    format!("a move from {} is under way here; to start another: `move cancel` first", st.address),
                    false,
                ));
            }
            // a new code for a move that stopped: the copy is kept until the claim says which server it is
            st.kept_cert = st.claimed.take().map(|c| c.cert_sha256).or(st.kept_cert.take());
            st.phase = Phase::Claiming;
            st.address = address;
            st.code_id = k.id.clone();
            st.auth = hex::encode(*k.auth);
            st.wrap = hex::encode(*k.wrap);
            st.claim = hex::encode(claim);
            st.cancelled = false;
            st.problem = None;
            st.finishing = false;
            if st.job_id.is_empty() {
                st.job_id = new_job_id();
            }
            for a in &opts.first_start_also {
                if !st.first_start_also.contains(a) {
                    st.first_start_also.push(a.clone());
                }
            }
            let words = format!("A new move code for {}: carrying on with what was copied.", st.address);
            crate::log::push(&mut st.log, now_ms(), words);
            write_in(&data.join(STAGING), STATE, &seal_state(&key, &st)?).map_err(Problem::io)?;
        }
        None => {
            if data.join(crate::import::STAGING).exists() {
                return Err(Problem::new(
                    "not_empty",
                    "an import is half done here: finish it, or remove it with `import --cancel`, first",
                    false,
                ));
            }
            crate::import::unused_but(data, keys, &opts.first_start_also)
                .map_err(|e| Problem::new("not_empty", e.to_string(), false))?;
            // before the code is used: a disk that rounds file times could never finish a move
            exact_times(data)?;
            stage(data, keys, &address, &k, &claim, &opts.first_start_also).map_err(Problem::from)?;
        }
    }
    let pull = Pull::open(data, keys, opts).map_err(|e| Problem::new("unreadable", e.to_string(), false))?;
    if lock(&pull.st).log.is_empty() {
        pull.note(&fresh_words);
    }
    match pull.claim_once() {
        Ok(()) => Ok(()),
        Err(p) if p.retry => Err(p),
        Err(p) => {
            pull.remember(&p);
            // nothing was copied by a fresh claim that failed: no staging left behind
            if pull.nothing_copied() {
                let _ = discard(data, keys);
            }
            Err(p)
        }
    }
}

/// A file time with microseconds, as the old server lists them.
const PROBE_US: u64 = 1_700_000_000_123_457;

/// Whether the disk under `dir` keeps a file time to the microsecond: a copy is matched to the old server's file by
/// its length and time, so on a disk that rounds them (whole seconds: ext3, ext4 made with small inodes, FAT, exFAT,
/// some network shares) every copied file looks changed, every round copies everything again and the move never
/// finishes. `set` sets the time (the tests round it).
fn keeps_exact_times(dir: &Path, set: impl Fn(&File, std::time::SystemTime) -> io::Result<()>) -> io::Result<bool> {
    let p = dir.join(".moving-times.tmp");
    let f = crate::export::create_private(&p)?;
    let r = set(&f, std::time::UNIX_EPOCH + Duration::from_micros(PROBE_US))
        .and_then(|()| f.sync_all())
        .and_then(|()| std::fs::metadata(&p));
    drop(f);
    let _ = std::fs::remove_file(&p);
    Ok(tree::mtime_us(&r?) == PROBE_US)
}

fn exact_times(data: &Path) -> Result<(), Problem> {
    let r = std::fs::create_dir_all(data).and_then(|()| keeps_exact_times(data, |f, t| f.set_modified(t)));
    match r {
        Ok(true) => Ok(()),
        Ok(false) => Err(times_problem()),
        Err(e) => Err(Problem::new("disk", format!("the data folder can't be written ({e})"), false)),
    }
}

fn times_problem() -> Problem {
    Problem::new(
        "disk_times",
        "this server's data folder is on a disk that keeps file times only roughly (to the second), so the move \
         could never tell a copied file from a changed one. Put the data folder on a disk that keeps exact times \
         (ZFS, as TrueNAS uses, or ext4, xfs, btrfs) and try again: the code still works",
        false,
    )
}

fn stage(
    data: &Path,
    keys: &Path,
    address: &Address,
    k: &crate::code::Keys,
    claim: &[u8; 16],
    also: &[String],
) -> Result<(), Error> {
    let (made, made_keys) = (data.join(BEING_MADE), keys.join(BEING_MADE));
    crate::import::remove_all(&made)?;
    crate::import::remove_all(&made_keys)?;
    std::fs::create_dir_all(made.join(crate::import::STAGED))?;
    std::fs::create_dir_all(made.join(PARTS))?;
    std::fs::create_dir_all(&made_keys)?;
    let mut key = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *key).map_err(|e| Error::Io(io::Error::other(e.to_string())))?;
    vault::write_private(&made_keys.join(STATE_KEY), &*key)?;
    let st = State {
        v: 1,
        phase: Phase::Claiming,
        address: address.clone(),
        code_id: k.id.clone(),
        auth: hex::encode(*k.auth),
        wrap: hex::encode(*k.wrap),
        claim: hex::encode(claim),
        started_ms: now_ms(),
        claimed: None,
        kept_cert: None,
        parts: BTreeMap::new(),
        next_part: 0,
        racy: BTreeSet::new(),
        refetch: BTreeSet::new(),
        progress: Progress::default(),
        cancelled: false,
        problem: None,
        skipped: Vec::new(),
        skipped_count: 0,
        finish: None,
        installed: false,
        job_id: new_job_id(),
        finishing: false,
        first_start_also: also.to_vec(),
        log: Vec::new(),
    };
    write_in(&made, STATE, &seal_state(&key, &st)?)?;
    std::fs::File::create(made.join(LOCK))?;
    std::fs::rename(&made_keys, keys.join(STAGING))?;
    std::fs::rename(&made, data.join(STAGING))?;
    crate::import::sync_dir(data);
    crate::import::sync_dir(keys);
    Ok(())
}

fn discard(data: &Path, keys: &Path) -> io::Result<()> {
    if data.join(STAGING).exists() {
        let gone = data.join(BEING_REMOVED);
        crate::import::remove_all(&gone)?;
        std::fs::rename(data.join(STAGING), &gone)?;
        crate::import::remove_all(&gone)?;
    }
    crate::import::remove_all(&keys.join(STAGING))
}

/// `.moving/lock`, opened without making it: gone means the move was cancelled here.
fn open_lock(data: &Path) -> Result<File, Problem> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(data.join(STAGING).join(LOCK))
        .map_err(|_| Problem::cancelled_here())
}

/// `move cancel` on the new server: the move stops and what was copied is removed; the old server is told (so its
/// uploads go on and its code is used up). Refused once switching. Returns whether there was a move.
pub fn cancel(data: &Path, keys: &Path) -> Result<bool, Error> {
    let loaded = match load(data, keys) {
        Ok(l) => l,
        Err(_) => {
            // a staging folder that can't be read: removed all the same
            let had = data.join(STAGING).exists();
            discard(data, keys)?;
            return Ok(had);
        }
    };
    let Some((_, st)) = loaded else {
        discard(data, keys)?;
        return Ok(false);
    };
    if st.phase == Phase::Switching {
        return Err(Error::Finished("the move is done; this server holds the data".into()));
    }
    // the copy sees this at its next step and lets go of the lock
    std::fs::File::create(data.join(STAGING).join(CANCELLING))?;
    let lockf = std::fs::OpenOptions::new().read(true).write(true).open(data.join(STAGING).join(LOCK))?;
    flock(&lockf, FlockOperation::LockExclusive).map_err(io::Error::from)?;
    if let (Some(c), false) = (&st.claimed, st.cancelled) {
        // best effort: the old server ends the move and takes uploads as before
        if let Ok(mut client) = Client::pinned(st.address.clone(), &c.cert_sha256) {
            client.set_timeout(Duration::from_secs(10));
            let bearer = format!("Bearer {}", c.session);
            let _ = client.request("DELETE", &format!("{}/session", proto::BASE), &[("Authorization", &bearer)], b"");
        }
    }
    discard(data, keys)?;
    Ok(true)
}

/// `move finish --recovery-key`: the last resort when the old server is gone. Opens the old vault's recovery part
/// (from the claim) with the recovery key and marks the move to switch to what was copied. The server carries it out
/// (or [`run`] when no server runs). Warn the owner first: the last changes may be missing, and the old server
/// doesn't know it moved.
pub fn force_finish(data: &Path, keys: &Path, recovery: &str) -> Result<(), Error> {
    let Some((_, st)) = load(data, keys)? else { return Err(Error::Move("there is no move here".into())) };
    if st.phase == Phase::Switching {
        return Err(Error::Finished("the move is switching already".into()));
    }
    let Some(c) = &st.claimed else {
        return Err(Error::Move("nothing was copied yet: the old server never took the code".into()));
    };
    if st.progress.bytes_done == 0 && st.progress.files_done == 0 {
        return Err(Error::Move("nothing was copied yet: there is nothing to switch to".into()));
    }
    // a copy that runs with no problem is not stopped: the old server is up and it finishes by itself
    if st.problem.is_none() && copy_runs(data) {
        return Err(Error::Move(
            "the copy is under way and the old server answers: it finishes by itself (finish with the recovery key \
             only when the old server is gone for good)"
                .into(),
        ));
    }
    let part: vault::RecoveryPart =
        serde_json::from_str(&c.recovery).map_err(|e| Error::Move(format!("the recovery part doesn't read: {e}")))?;
    let v = part.open(recovery)?;
    let staging = data.join(STAGING);
    let lockf = std::fs::OpenOptions::new().read(true).write(true).open(staging.join(LOCK))?;
    flock(&lockf, FlockOperation::LockExclusive).map_err(io::Error::from)?;
    let staged_keys = keys.join(STAGING);
    crate::import::remove_file(&staging.join(vault::VAULT_FILE))?;
    crate::import::remove_file(&staged_keys.join(vault::KEY_FILE))?;
    v.install(&staging, &staged_keys, &part)?;
    write_in(&staging, FORCED, b"")?;
    Ok(())
}

/// Whether a [`run`] holds the move staged here now.
fn copy_runs(data: &Path) -> bool {
    let Ok(f) = File::open(data.join(STAGING).join(RUNNING)) else { return false };
    flock(&f, FlockOperation::NonBlockingLockExclusive).is_err()
}

/// Carries on the move staged here until it is done and switched: the claim (if it wasn't taken yet), the copy
/// rounds, the pause and the last round, the finish, the switch. It tries again by itself after network problems,
/// a restart of either server and a full disk; a problem only the owner can solve (a wrong, used or expired code,
/// a cancel on the old server) ends it with that problem, saved in the state.
pub fn run(data: &Path, keys: &Path, opts: &Options) -> Result<Done, Problem> {
    let pull = Pull::open(data, keys, opts).map_err(|e| Problem::new("unreadable", e.to_string(), false))?;
    // held until this returns (the staging folder may be gone by then: the handle stays good)
    let running = File::create(data.join(STAGING).join(RUNNING)).map_err(Problem::io)?;
    flock(&running, FlockOperation::LockShared).map_err(|e| Problem::io(e.into()))?;
    let r = pull.run();
    drop(running);
    match &r {
        Err(p) if p.code == "stopped" || p.code == SWITCH_NEXT => {
            // what it was waiting for stays in the state, for `move status`
            let _ = pull.save();
        }
        Err(p) if p.code != "cancelled_here" => pull.remember(p),
        _ => {}
    }
    r
}

/// A local file of the copy.
struct Local {
    path: String,
    len: u64,
    mt: u64,
}

#[derive(Clone)]
enum What {
    Whole,
    /// A growing file's catch-up from `from`; `tail` is the copy's last 16 bytes.
    Append {
        from: u64,
        tail: [u8; 16],
    },
}

#[derive(Clone)]
struct Task {
    p: String,
    kind: Kind,
    len: u64,
    v: String,
    id: Option<[u8; 16]>,
    what: What,
}

#[derive(Default, Clone, Copy)]
struct Round {
    files: u64,
    bytes: u64,
}

struct Pull<'a> {
    data: PathBuf,
    keys: PathBuf,
    opts: &'a Options,
    key: Zeroizing<[u8; 32]>,
    st: Mutex<State>,
    /// Held while the state is sealed and written: the fetch threads save at once, and two writers of the one
    /// `.tmp` file would rename it from under each other (or mix their bytes), or put an older state last.
    saving: Mutex<()>,
    /// The last progress line logged.
    logged: Mutex<(Instant, u8)>,
    /// When this run's first copy began, and how much was copied then (for the time left).
    began: Mutex<Option<(Instant, u64)>>,
    /// When the move's log last named a file copied (one such line every [`FILE_LINE_EVERY`] at most).
    file_line: Mutex<Option<Instant>>,
}

/// The move's log names a file copied at most this often (its kind, never its path).
const FILE_LINE_EVERY: Duration = Duration::from_secs(2);

/// One answer of the old server that refuses.
struct Refused {
    status: u16,
    code: String,
    body: serde_json::Value,
    retry_after: Option<u64>,
}

enum Fail {
    Net(NetError),
    Refused(Refused),
    Here(Problem),
}

impl From<NetError> for Fail {
    fn from(e: NetError) -> Fail {
        Fail::Net(e)
    }
}

impl From<Problem> for Fail {
    fn from(p: Problem) -> Fail {
        Fail::Here(p)
    }
}

impl From<io::Error> for Fail {
    fn from(e: io::Error) -> Fail {
        Fail::Here(Problem::io(e))
    }
}

fn refused(r: crate::client::Response<'_>) -> Fail {
    let (status, retry_after) = (r.status, r.retry_after);
    match r.error() {
        Ok((code, body)) => Fail::Refused(Refused { status, code, body, retry_after }),
        Err(e) => Fail::Net(e),
    }
}

impl<'a> Pull<'a> {
    fn open(data: &Path, keys: &Path, opts: &'a Options) -> Result<Pull<'a>, Error> {
        let (key, st) = load(data, keys)?.ok_or_else(|| Error::Move("there is no move here".into()))?;
        Ok(Pull {
            data: data.to_path_buf(),
            keys: keys.to_path_buf(),
            opts,
            key,
            st: Mutex::new(st),
            saving: Mutex::new(()),
            logged: Mutex::new((Instant::now(), 0)),
            began: Mutex::new(None),
            file_line: Mutex::new(None),
        })
    }

    fn staging(&self) -> PathBuf {
        self.data.join(STAGING)
    }

    fn copy_dir(&self) -> PathBuf {
        self.staging().join(crate::import::STAGED)
    }

    fn save(&self) -> Result<(), Problem> {
        let _one = lock(&self.saving);
        let b = seal_state(&self.key, &lock(&self.st)).map_err(Problem::from)?;
        write_in(&self.staging(), STATE, &b)
            .map_err(|e| if self.staging().exists() { Problem::io(e) } else { Problem::cancelled_here() })
    }

    fn remember(&self, p: &Problem) {
        let cancelled = p.code == "cancelled";
        self.set_problem(p.clone());
        lock(&self.st).cancelled |= cancelled;
        let _ = self.save();
    }

    /// Adds a line to the move's log (saved with the state at its next save).
    fn note(&self, text: &str) {
        crate::log::push(&mut lock(&self.st).log, now_ms(), text);
    }

    /// What the move waits for now; a new one goes in the move's log (the same one again does not).
    fn set_problem(&self, p: Problem) {
        let mut st = lock(&self.st);
        if st.problem.as_ref().is_some_and(|q| q.code == p.code && q.message == p.message) {
            return;
        }
        let words = if p.retry {
            format!("Waiting: {}; trying again by itself.", p.message)
        } else {
            format!("Stopped: {}.", p.message)
        };
        if p.code != "stopped" && p.code != SWITCH_NEXT {
            crate::log::push(&mut st.log, now_ms(), words);
        }
        st.problem = Some(p);
    }

    /// A file copied whole: the move's log names its kind (in the first round, one line every
    /// [`FILE_LINE_EVERY`] at most; the round's end says how many in all).
    fn file_copied(&self, path: &str) {
        let (round, done, total) = {
            let st = lock(&self.st);
            if st.finishing {
                return;
            }
            (st.progress.round, st.progress.files_done, st.progress.files_total.max(st.progress.files_done))
        };
        if round != 1 {
            return;
        }
        let mut last = lock(&self.file_line);
        if last.is_some_and(|t| t.elapsed() < FILE_LINE_EVERY) {
            return;
        }
        *last = Some(Instant::now());
        drop(last);
        self.note(&format!("Copying file {done} of {total} ({}).", crate::log::kind_of(path)));
    }

    fn nothing_copied(&self) -> bool {
        let st = lock(&self.st);
        st.claimed.is_none()
            && st.kept_cert.is_none()
            && std::fs::read_dir(self.copy_dir()).map(|mut d| d.next().is_none()).unwrap_or(true)
    }

    fn check_stop(&self) -> Result<(), Problem> {
        if self.opts.stop.load(AtOrd::Relaxed) {
            return Err(Problem::stopped());
        }
        if !self.staging().join(LOCK).exists() || self.staging().join(CANCELLING).exists() {
            return Err(Problem::cancelled_here());
        }
        if self.staging().join(FORCED).exists() {
            return Err(Problem::new("forced", "switching to what was copied (move finish --recovery-key)", false));
        }
        Ok(())
    }

    /// Sleeps `d`, looking every second whether to stop.
    fn sleep(&self, d: Duration) -> Result<(), Problem> {
        let until = Instant::now() + d;
        loop {
            self.check_stop()?;
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            std::thread::sleep(left.min(Duration::from_millis(250)));
        }
    }

    fn client(&self) -> Result<Client, Problem> {
        let st = lock(&self.st);
        let c = st.claimed.as_ref().ok_or_else(|| Problem::new("not_claimed", "the code wasn't taken yet", false))?;
        let mut client =
            Client::pinned(st.address.clone(), &c.cert_sha256).map_err(|e| Problem::net(&st.address, &e))?;
        client.set_timeout(self.opts.timeout);
        Ok(client)
    }

    fn bearer(&self) -> String {
        format!("Bearer {}", lock(&self.st).claimed.as_ref().map_or("", |c| c.session.as_str()))
    }

    /// What the owner sees of an answer that refuses, and whether it is worth trying again.
    fn judge(&self, r: &Refused) -> Problem {
        let at = |k: &str| r.body[k].as_u64().map(crate::old::when).unwrap_or_default();
        let addr = lock(&self.st).address.to_string();
        match (r.status, r.code.as_str()) {
            (401, _) => Problem::new(
                "bad_session",
                "the old server doesn't know this move any more (its move state was removed?): make a new move code there, put it in SENTRY_MATE_MOVE_CODE and restart this server",
                false,
            ),
            (410, "cancelled") => Problem::new(
                "cancelled",
                "the move was cancelled on the old server. What was copied here is kept: with a new move code from the same server it carries on from there. To remove it: `move cancel` here.",
                false,
            ),
            (404, _) => Problem::new(
                "no_move",
                format!(
                    "there is no move waiting at {addr}: make a move code there (update it first if it has no `move code` command)"
                ),
                false,
            ),
            (403, "wrong_code") => Problem::new(
                "wrong_code",
                "the old server didn't take this code: check it, or make a new one there",
                false,
            ),
            (403, "not_home_network") => Problem::new(
                "not_home_network",
                "the old server's move code works from its home network only: make one there with `move code --anywhere`",
                false,
            ),
            (409, "code_used") => Problem::new(
                "code_used",
                format!(
                    "this move code was used already, by {} on {}. If that wasn't this server, run `move cancel` on the old server now.",
                    r.body["from"].as_str().unwrap_or("another server"),
                    at("at_ms")
                ),
                false,
            ),
            (410, "code_expired") => Problem::new(
                "code_expired",
                format!(
                    "this move code expired on {}: make a new one on the old server, put it in SENTRY_MATE_MOVE_CODE and restart this server",
                    at("expired_ms")
                ),
                false,
            ),
            (409, "too_old") => Problem::new(
                "too_old",
                format!(
                    "the old server runs a newer Sentry Mate ({}) than this one ({}): update this server first, then start it again (the code still works)",
                    r.body["app"].as_str().unwrap_or("?"),
                    self.opts.app
                ),
                false,
            ),
            (429, _) => Problem::new("slow_down", "the old server asks to wait (too many wrong codes from here)", true),
            (s, c) if s >= 500 => {
                Problem::new("unreachable", format!("the old server answered {s} {c}; trying again"), true)
            }
            (s, c) => Problem::new(
                "refused",
                format!("the old server answered {s} {c}: {}", r.body["error"].as_str().unwrap_or("")),
                false,
            ),
        }
    }

    /// Runs `f` until it works, trying again after network problems and answers worth trying again (1 s, doubling,
    /// at most `retry_most`).
    fn call<T>(&self, client: &mut Client, mut f: impl FnMut(&mut Client) -> Result<T, Fail>) -> Result<T, Problem> {
        let mut wait = Duration::from_secs(1);
        let mut said = false;
        loop {
            self.check_stop()?;
            let problem = match f(client) {
                Ok(v) => {
                    if said {
                        tracing::info!("move: connected to the old server again; carrying on where it stopped");
                        self.note("Connected to the old server again: carrying on where it stopped.");
                    }
                    return Ok(v);
                }
                Err(Fail::Here(p)) if !p.retry => return Err(p),
                Err(Fail::Here(p)) => p,
                Err(Fail::Net(e)) => {
                    client.drop_connection();
                    let addr = lock(&self.st).address.clone();
                    Problem::net(&addr, &e)
                }
                Err(Fail::Refused(r)) => {
                    let p = self.judge(&r);
                    if !p.retry {
                        return Err(p);
                    }
                    if let Some(s) = r.retry_after {
                        wait = wait.max(Duration::from_secs(s.min(600)));
                    }
                    p
                }
            };
            let wait_now = wait.min(self.opts.retry_most.max(Duration::from_millis(10)));
            if !said || wait_now >= self.opts.retry_most {
                tracing::warn!(
                    "move: {}; trying again in {} s (it carries on where it stopped)",
                    problem.message,
                    wait_now.as_secs().max(1)
                );
                said = true;
            }
            self.set_problem(problem.clone());
            self.sleep(wait_now)?;
            wait = (wait * 2).min(self.opts.retry_most.max(Duration::from_millis(10)));
        }
    }

    /// One try of the claim: proves the code over this connection, checks the old server's proof, saves it all.
    fn claim_once(&self) -> Result<(), Problem> {
        let (addr, claim, auth) = {
            let st = lock(&self.st);
            let claim = proto::hex_n::<16>(&st.claim).ok_or_else(|| Problem::new("unreadable", "the claim", false))?;
            let auth = Zeroizing::new(
                proto::hex_n::<32>(&st.auth).ok_or_else(|| Problem::new("unreadable", "the code", false))?,
            );
            (st.address.clone(), claim, auth)
        };
        let mut c = Client::any(addr.clone());
        c.set_timeout(self.opts.timeout);
        let (cert, exporter) = c.seen().map_err(|e| Problem::net(&addr, &e))?;
        let req = ClaimRequest {
            v: 1,
            claim: hex::encode(claim),
            app: self.opts.app.clone(),
            proof: hex::encode(proto::client_proof(&auth, &exporter)),
        };
        let body = serde_json::to_vec(&req).unwrap_or_default();
        let r = c
            .request("POST", &format!("{}/claim", proto::BASE), &[("Content-Type", "application/json")], &body)
            .map_err(|e| Problem::net(&addr, &e))?;
        if r.status != 200 {
            return match refused(r) {
                Fail::Refused(r) => Err(self.judge(&r)),
                Fail::Net(e) => Err(Problem::net(&addr, &e)),
                Fail::Here(p) => Err(p),
            };
        }
        let a: ClaimAnswer = r.json().map_err(|e| Problem::net(&addr, &e))?;
        let not_proven = || {
            Problem::new(
                "not_proven",
                format!(
                    "the server at {} couldn't prove it made this code: it isn't the old server, or something in between changed the connection. Nothing was sent.",
                    addr.host
                ),
                false,
            )
        };
        let proof = proto::hex_n::<32>(&a.server_proof).ok_or_else(not_proven)?;
        if !proto::same(&proof, &proto::server_proof(&auth, &exporter, &a))
            || !a.cert_sha256.eq_ignore_ascii_case(&cert)
        {
            return Err(not_proven());
        }
        let session = proto::hex_n::<32>(&a.session).ok_or_else(not_proven)?;
        if !proto::same(&session, &proto::session(&auth, &claim)) {
            return Err(not_proven());
        }
        let part: vault::RecoveryPart = serde_json::from_str(&a.recovery).map_err(|_| not_proven())?;
        if let (Some(theirs), Some(ours)) = (crate::import::version(&a.app), crate::import::version(&self.opts.app))
            && theirs > ours
        {
            return Err(Problem::new(
                "too_old",
                format!(
                    "the old server runs a newer Sentry Mate ({}) than this one ({}): update this server first, then start it again",
                    a.app, self.opts.app
                ),
                false,
            ));
        }
        let k = part.slot.kdf();
        if !(8..=4 << 20).contains(&k.m) || !(1..=16).contains(&k.t) || !(1..=16).contains(&k.p) {
            return Err(not_proven());
        }
        tracing::info!(
            "move: the old server proved the code; it runs Sentry Mate {}, this one {}",
            a.app,
            self.opts.app
        );
        let free = (self.opts.free_bytes)(&self.data);
        tracing::info!("move: {} files, {} to copy; {} free here", a.files, crate::size(a.bytes), crate::size(free));
        self.note(&format!(
            "Connected: the old server proved the move code (Sentry Mate {}, this one {}): {} files, {} to copy.",
            a.app,
            self.opts.app,
            a.files,
            crate::size(a.bytes)
        ));
        {
            let mut st = lock(&self.st);
            // a copy kept from before counts as done so far (the page doesn't drop back to nothing)
            let mut so_far = (st.progress.files_done, st.progress.bytes_done);
            if st.kept_cert.is_none() {
                so_far = (0, 0);
            }
            if let Some(kept) = st.kept_cert.take() {
                if !kept.eq_ignore_ascii_case(&a.cert_sha256) {
                    // another server: what was copied before isn't of its data
                    drop(st);
                    let _ = crate::import::remove_all(&self.copy_dir());
                    let _ = crate::import::remove_all(&self.staging().join(PARTS));
                    std::fs::create_dir_all(self.copy_dir()).map_err(Problem::io)?;
                    std::fs::create_dir_all(self.staging().join(PARTS)).map_err(Problem::io)?;
                    st = lock(&self.st);
                    st.parts.clear();
                    st.racy.clear();
                    st.refetch.clear();
                    so_far = (0, 0);
                } else {
                    tracing::info!("move: the same old server as before: carrying on with what was copied");
                    crate::log::push(
                        &mut st.log,
                        now_ms(),
                        "The same old server as before: carrying on with what was copied.",
                    );
                }
            }
            st.claimed = Some(Claimed {
                session: a.session.clone(),
                cert_sha256: a.cert_sha256.to_ascii_lowercase(),
                app: a.app.clone(),
                files: a.files,
                bytes: a.bytes,
                recovery: a.recovery.clone(),
                at_ms: now_ms(),
            });
            st.phase = Phase::Copying;
            st.problem = None;
            st.progress = Progress {
                round: 0,
                files_done: so_far.0.min(a.files),
                files_total: a.files,
                bytes_done: so_far.1.min(a.bytes),
                bytes_total: a.bytes,
            };
        }
        self.save()
    }

    fn run(&self) -> Result<Done, Problem> {
        loop {
            let phase = lock(&self.st).phase;
            let step = match phase {
                Phase::Claiming => self.claim_with_retries(),
                Phase::Copying => self.copy_and_finish(),
                Phase::Switching if self.opts.stop_before_switch => {
                    return Err(Problem::new(SWITCH_NEXT, "the copy is done: switching to it", false));
                }
                Phase::Switching => return self.switch(),
            };
            match step {
                Ok(()) => {}
                Err(p) if p.code == "forced" => self.forced()?,
                Err(p) => return Err(p),
            }
        }
    }

    fn claim_with_retries(&self) -> Result<(), Problem> {
        let mut wait = Duration::from_secs(1);
        loop {
            self.check_stop()?;
            match self.claim_once() {
                Ok(()) => return Ok(()),
                Err(p) if !p.retry => return Err(p),
                Err(p) => {
                    let w = wait.min(self.opts.retry_most);
                    tracing::warn!("move: {}; trying again in {} s", p.message, w.as_secs().max(1));
                    self.set_problem(p);
                    self.sleep(w)?;
                    wait *= 2;
                }
            }
        }
    }

    /// The switch was asked for with the recovery key: the staged vault (written by `force_finish`) opens the copy.
    fn copy_size(&self) -> (u64, u64) {
        let mut d = (0u64, 0u64);
        let _ =
            tree::walk(&self.copy_dir(), Purpose::Move, &mut Skips::default(), |x| -> io::Result<ControlFlow<()>> {
                d.0 += 1;
                d.1 += x.len;
                Ok(ControlFlow::Continue(()))
            });
        d
    }

    fn forced(&self) -> Result<(), Problem> {
        let staging = self.staging();
        Vault::open(&staging, &self.keys.join(STAGING))
            .map_err(|e| Problem::new("forced", format!("the staged vault doesn't open ({e})"), false))?;
        {
            let mut st = lock(&self.st);
            let (files, bytes) = self.copy_size();
            st.finish = Some(FinishRec {
                moved_ms: now_ms(),
                files,
                bytes,
                key_id: String::new(),
                data_key: None,
                forced: true,
            });
            st.installed = true;
            st.phase = Phase::Switching;
            st.finishing = false;
        }
        self.save()?;
        crate::import::remove_file(&staging.join(FORCED)).map_err(Problem::io)?;
        tracing::warn!(
            "move: switching to what was copied, opened with the recovery key: the last changes may be missing, and the old server doesn't know it moved (don't start it again with the CAN Bridge pointed at it)"
        );
        Ok(())
    }

    fn copy_and_finish(&self) -> Result<(), Problem> {
        let mut conns: Vec<Client> = Vec::new();
        for _ in 0..self.opts.connections.max(1) {
            conns.push(self.client()?);
        }
        let first_copy_started = Instant::now();
        let mut last_round: Option<Round> = None;
        loop {
            self.check_stop()?;
            let round_no = {
                let mut st = lock(&self.st);
                st.progress.round += 1;
                st.progress.round
            };
            if round_no == 1 {
                self.check_space()?;
                self.note("Round 1: copying the old server's files.");
            }
            let r = self.round(&mut conns, None)?;
            if round_no == 1 {
                self.note(&format!("Round 1 done: {} files, {} copied.", r.files, crate::size(r.bytes)));
            } else {
                self.note(&format!("Round {round_no} caught up {} files ({}).", r.files, crate::size(r.bytes)));
            }
            if round_no == 1 {
                self.log_skipped();
                tracing::info!(
                    "move: first copy done in {}; catching up with what changed meanwhile",
                    minutes(first_copy_started.elapsed())
                );
            }
            let small = r.bytes <= self.opts.small_bytes && r.files <= self.opts.small_files;
            if small {
                match self.finish_with_pause(&mut conns)? {
                    true => return Ok(()),
                    false => continue,
                }
            }
            if let Some(prev) = last_round
                && r.bytes >= prev.bytes
                && round_no > 2
            {
                tracing::warn!(
                    "move: the old server changes faster than this copy catches up ({} in the last round); waiting {} s. If a big job runs there (an import), let it end first.",
                    crate::size(r.bytes),
                    self.opts.between_rounds.as_secs()
                );
                self.set_problem(Problem::new(
                    "changing_fast",
                    "the old server changes faster than this copy catches up",
                    true,
                ));
                self.sleep(self.opts.between_rounds)?;
            }
            if round_no > 1 {
                tracing::info!("move: catching up: {} files, {} changed", r.files, crate::size(r.bytes));
            }
            last_round = Some(r);
        }
    }

    fn check_space(&self) -> Result<(), Problem> {
        loop {
            let need = lock(&self.st)
                .claimed
                .as_ref()
                .map_or(0, |c| c.bytes)
                .saturating_sub(self.copied_bytes())
                .saturating_add(self.opts.min_free);
            let free = (self.opts.free_bytes)(&self.data);
            if free >= need {
                return Ok(());
            }
            let p = Problem::new(
                "no_space",
                format!(
                    "not enough space: {} (with 2 GB to spare) are needed, {} are free. It carries on by itself when there is room (checked every minute); to give up: `move cancel`",
                    crate::size(need),
                    crate::size(free)
                ),
                true,
            );
            tracing::warn!("move: {}", p.message);
            self.set_problem(p);
            let _ = self.save();
            self.sleep(Duration::from_secs(60))?;
        }
    }

    fn copied_bytes(&self) -> u64 {
        let mut n = 0u64;
        let _ =
            tree::walk(&self.copy_dir(), Purpose::Move, &mut Skips::default(), |f| -> io::Result<ControlFlow<()>> {
                n += f.len;
                Ok(ControlFlow::Continue(()))
            });
        n
    }

    fn log_skipped(&self) {
        let st = lock(&self.st);
        if st.skipped_count > 0 {
            let names: Vec<String> = st.skipped.iter().map(|s| format!("{} ({})", s.path, s.why)).collect();
            let more = st.skipped_count.saturating_sub(st.skipped.len() as u64);
            let more = if more > 0 { format!(", and {more} more") } else { String::new() };
            tracing::warn!(
                "move: {} entries were skipped on the old server (not carried): {}{more}",
                st.skipped_count,
                names.join(", ")
            );
        }
    }

    /// The pause, the last round under it and the finish. `Ok(false)`: the pause couldn't be had or ended: catch up
    /// and ask again.
    fn finish_with_pause(&self, conns: &mut [Client]) -> Result<bool, Problem> {
        let r = self.pause_and_finish(conns);
        if !matches!(r, Ok(true)) && std::mem::take(&mut lock(&self.st).finishing) {
            let _ = self.save();
        }
        r
    }

    fn pause_and_finish(&self, conns: &mut [Client]) -> Result<bool, Problem> {
        let also = lock(&self.st).first_start_also.clone();
        crate::import::unused_but(&self.data, &self.keys, &also)
            .map_err(|e| Problem::new("not_empty", format!("{e}; the move can't switch here"), false))?;
        tracing::info!("move: caught up; the old server pauses uploads for the last changes");
        self.note("Caught up: asking the old server for the last-round pause.");
        let bearer = self.bearer();
        let pause: Option<proto::PauseAnswer> = self.call(&mut conns[0], |c| {
            let r = c.request("POST", &format!("{}/pause", proto::BASE), &[("Authorization", &bearer)], b"")?;
            match r.status {
                200 => Ok(Some(r.json()?)),
                503 => {
                    let _ = r.bytes(1 << 16);
                    Ok(None)
                }
                _ => Err(refused(r)),
            }
        })?;
        let Some(pause) = pause else {
            tracing::info!("move: the old server's writes didn't stop in time; catching up and asking again");
            self.note("The old server's writes didn't stop in time: catching up and asking again.");
            self.sleep(Duration::from_secs(10).min(self.opts.between_rounds))?;
            return Ok(false);
        };
        lock(&self.st).finishing = true;
        self.note("Last-round pause: uploads wait on the old server while the last changes are copied.");
        self.save()?;
        let started = Instant::now();
        for _ in 0..5 {
            let r = match self.round(conns, Some(&pause.pause)) {
                Ok(r) => r,
                Err(p) if p.code == "pause_ended" => {
                    self.note("The pause ended on the old server: catching up and asking again.");
                    return Ok(false);
                }
                Err(p) => return Err(p),
            };
            let (files, bytes, digest) = self.local_digest().map_err(Problem::io)?;
            let req = FinishRequest { pause: pause.pause.clone(), files, bytes, digest };
            let body = serde_json::to_vec(&req).unwrap_or_default();
            let answer: Result<FinishAnswer, Refused> = self.call(&mut conns[0], |c| {
                let r = c.request(
                    "POST",
                    &format!("{}/finish", proto::BASE),
                    &[("Authorization", &bearer), ("Content-Type", "application/json")],
                    &body,
                )?;
                if r.status == 200 {
                    return Ok(Ok(r.json()?));
                }
                match refused(r) {
                    Fail::Refused(x) if x.status == 409 => Ok(Err(x)),
                    f => Err(f),
                }
            })?;
            match answer {
                Ok(a) => {
                    tracing::info!(
                        "move: last changes copied ({} files, {}) in {} s; the old server confirmed",
                        r.files,
                        crate::size(r.bytes),
                        started.elapsed().as_secs()
                    );
                    self.note(&format!(
                        "Last round: {} files ({}) in {} s. The old server confirmed the copy and stopped its inputs.",
                        r.files,
                        crate::size(r.bytes),
                        started.elapsed().as_secs()
                    ));
                    self.finished(a, files, bytes)?;
                    return Ok(true);
                }
                Err(x) if x.code == "digest_mismatch" => {
                    self.note(
                        "The copy differs from the old server's files in the last round: copying what changed again.",
                    );
                    tracing::info!(
                        "move: the copy differs from the old server's files in the last round; copying what changed again"
                    );
                }
                Err(_) => return Ok(false),
            }
        }
        // the copy keeps differing under the pause: the pause ends by itself while this waits, then it catches up
        tracing::warn!(
            "move: the copy kept differing from the old server's files during the pause; catching up and trying again"
        );
        self.sleep(self.opts.between_rounds)?;
        Ok(false)
    }

    fn finished(&self, a: FinishAnswer, files: u64, bytes: u64) -> Result<(), Problem> {
        let (part, wrap, session) = {
            let st = lock(&self.st);
            let c = st.claimed.as_ref().ok_or_else(|| Problem::new("not_claimed", "no claim", false))?;
            let part: vault::RecoveryPart =
                serde_json::from_str(&c.recovery).map_err(|e| Problem::new("unreadable", e.to_string(), false))?;
            let wrap = Zeroizing::new(
                proto::hex_n::<32>(&st.wrap).ok_or_else(|| Problem::new("unreadable", "the code", false))?,
            );
            let session =
                proto::hex_n::<32>(&c.session).ok_or_else(|| Problem::new("unreadable", "the session", false))?;
            (part, wrap, session)
        };
        if a.key_id != part.key_id {
            return Err(Problem::new("not_proven", "the old server's data key isn't the one its claim named", false));
        }
        Vault::unwrap_key(&wrap, &a.data_key, &proto::data_key_purpose(&a.key_id, &session), &part.key_id).map_err(
            |_| Problem::new("not_proven", "the data key from the old server doesn't open with this move code", false),
        )?;
        {
            let mut st = lock(&self.st);
            st.finish = Some(FinishRec {
                moved_ms: a.moved_ms,
                files,
                bytes,
                key_id: a.key_id,
                data_key: Some(a.data_key),
                forced: false,
            });
            st.phase = Phase::Switching;
            st.problem = None;
            st.finishing = false;
        }
        // saved and synced before anything else is touched: from here the switch always finishes
        self.save()
    }

    /// The vault the copy opens with: from the finish answer, or (forced) the staged one.
    fn data_vault(&self) -> Result<Vault, Problem> {
        let st = lock(&self.st).clone();
        let f = st.finish.as_ref().ok_or_else(|| Problem::new("unreadable", "no finish answer", false))?;
        if f.forced {
            return Vault::open(&self.staging(), &self.keys.join(STAGING))
                .or_else(|_| Vault::open(&self.data, &self.keys))
                .map_err(|e| Problem::new("unreadable", format!("the staged vault doesn't open ({e})"), false));
        }
        let c = st.claimed.as_ref().ok_or_else(|| Problem::new("unreadable", "no claim", false))?;
        let wrap =
            Zeroizing::new(proto::hex_n::<32>(&st.wrap).ok_or_else(|| Problem::new("unreadable", "the code", false))?);
        let session = proto::hex_n::<32>(&c.session).ok_or_else(|| Problem::new("unreadable", "the session", false))?;
        let w = f.data_key.as_ref().ok_or_else(|| Problem::new("unreadable", "no data key", false))?;
        Vault::unwrap_key(&wrap, w, &proto::data_key_purpose(&f.key_id, &session), &f.key_id)
            .map_err(|_| Problem::new("unreadable", "the saved data key doesn't open", false))
    }

    fn switch(&self) -> Result<Done, Problem> {
        tracing::info!("move: switching to the copied data");
        if !lock(&self.st).log.last().is_some_and(|l| l.text.starts_with("Switching")) {
            self.note("Switching to the copied data.");
            self.save()?;
        }
        let v = self.data_vault()?;
        let st = lock(&self.st).clone();
        let c = st.claimed.clone().ok_or_else(|| Problem::new("unreadable", "no claim", false))?;
        let f = st.finish.clone().ok_or_else(|| Problem::new("unreadable", "no finish answer", false))?;
        let staging = self.staging();
        let staged_keys = self.keys.join(STAGING);
        if !st.installed {
            let part: vault::RecoveryPart =
                serde_json::from_str(&c.recovery).map_err(|e| Problem::new("unreadable", e.to_string(), false))?;
            // a crash after writing these and before the state said so: written again
            crate::import::remove_file(&staging.join(vault::VAULT_FILE)).map_err(Problem::io)?;
            crate::import::remove_file(&staged_keys.join(vault::KEY_FILE)).map_err(Problem::io)?;
            v.install(&staging, &staged_keys, &part).map_err(Problem::from)?;
            lock(&self.st).installed = true;
            self.save()?;
        }
        let same_machine = st.address.is_this_machine();
        let (files, bytes) = (f.files, f.bytes);
        let note = MovedInNote {
            from: st.address.clone(),
            cert_sha256: c.cert_sha256.clone(),
            session: c.session.clone(),
            code_id: st.code_id.clone(),
            moved_ms: f.moved_ms,
            at_ms: now_ms(),
            files,
            bytes,
            forced: f.forced,
            switched_sent: false,
            job_id: st.job_id(),
            started_ms: st.started_ms,
            same_machine,
            log: {
                let mut l = st.log.clone();
                crate::log::push(&mut l, now_ms(), "Switched: this server holds the data now.");
                l
            },
        };
        let note = Zeroizing::new(serde_json::to_vec(&note).map_err(|e| Problem::new("disk", e.to_string(), false))?);
        let replaced = crate::import::bring_all_in(
            &self.data,
            &self.keys,
            STAGING,
            MOVED_IN,
            &note,
            &v,
            &st.first_start_also,
            &mut || Ok(()),
        )
        .map_err(Problem::from)?;
        Ok(Done {
            from: st.address.to_string(),
            cert_sha256: c.cert_sha256,
            old_app: c.app,
            files,
            bytes,
            moved_ms: f.moved_ms,
            same_machine,
            forced: f.forced,
            replaced_first_start: replaced,
        })
    }

    /// The copy's digest, as the old server makes it.
    fn local_digest(&self) -> io::Result<(u64, u64, String)> {
        let mut d = proto::Digest::default();
        tree::walk(&self.copy_dir(), Purpose::Move, &mut Skips::default(), |f| -> io::Result<ControlFlow<()>> {
            let (kind, id) = local_head(f.full)?;
            d.add(f.path, kind, f.len, f.mtime_us, id.map(hex::encode).as_deref());
            Ok(ControlFlow::Continue(()))
        })?;
        Ok((d.files, d.bytes, d.hex()))
    }

    /// One round: the old server's list a page at a time, each page compared with the copy and fetched.
    fn round(&self, conns: &mut [Client], paused: Option<&str>) -> Result<Round, Problem> {
        let bearer = self.bearer();
        let mut after: Option<String> = None;
        let mut total = Round::default();
        {
            let mut st = lock(&self.st);
            st.skipped.clear();
            st.skipped_count = 0;
        }
        loop {
            self.check_stop()?;
            let q = match &after {
                Some(a) => format!("{}/list?after={a}&limit={}", proto::BASE, proto::MAX_LIST),
                None => format!("{}/list?limit={}", proto::BASE, proto::MAX_LIST),
            };
            let (lines, end) = self.call(&mut conns[0], |c| {
                let r = c.request("GET", &q, &[("Authorization", &bearer)], b"")?;
                if r.status != 200 {
                    return Err(refused(r));
                }
                let b = r.bytes(64 << 20)?;
                parse_list(&b, after.as_deref()).map_err(|e| Fail::Net(NetError::Protocol(e)))
            })?;
            if paused.is_some() && end.paused.as_deref() != paused {
                return Err(Problem::new("pause_ended", "the pause ended", true));
            }
            // records come whole as stored whenever the old server says nothing writes
            let whole = end.paused.is_some();
            {
                let mut st = lock(&self.st);
                for s in &end.skipped {
                    if st.skipped.len() < tree::NAMED_SKIPS && !st.skipped.contains(s) {
                        st.skipped.push(s.clone());
                    }
                }
                st.skipped_count += end.skipped_count;
            }
            let upto = if end.next.is_some() { lines.last().map(|l| l.p.clone()) } else { None };
            let locals = self.locals(after.as_deref(), upto.as_deref()).map_err(Problem::io)?;
            let tasks = self.merge(&lines, &locals, whole).map_err(Problem::io)?;
            let r = self.fetch_all(conns, tasks, whole)?;
            total.files += r.files;
            total.bytes += r.bytes;
            self.save()?;
            match end.next {
                Some(n) => after = Some(n),
                None => return Ok(total),
            }
        }
    }

    /// The copy's files after `after` up to `upto` (to the end with none), in walk order.
    fn locals(&self, after: Option<&str>, upto: Option<&str>) -> io::Result<Vec<Local>> {
        let mut out = Vec::new();
        tree::walk_from(
            &self.copy_dir(),
            Purpose::Move,
            &mut Skips::default(),
            after,
            |f| -> io::Result<ControlFlow<()>> {
                if let Some(u) = upto
                    && walk_cmp(f.path, u) == Ordering::Greater
                {
                    return Ok(ControlFlow::Break(()));
                }
                out.push(Local { path: f.path.to_string(), len: f.len, mt: f.mtime_us });
                Ok(ControlFlow::Continue(()))
            },
        )?;
        Ok(out)
    }

    fn remove_local(&self, p: &str) -> io::Result<()> {
        crate::import::remove_file(&self.copy_dir().join(p))?;
        let mut st = lock(&self.st);
        if let Some(part) = st.parts.remove(p) {
            let _ = std::fs::remove_file(self.staging().join(PARTS).join(part.n.to_string()));
        }
        st.racy.remove(p);
        st.refetch.remove(p);
        Ok(())
    }

    /// What to fetch for one page; removes copies of files no longer on the old server.
    fn merge(&self, lines: &[Line], locals: &[Local], paused: bool) -> io::Result<Vec<Task>> {
        let mut tasks = Vec::new();
        let (mut i, mut j) = (0usize, 0usize);
        while i < lines.len() || j < locals.len() {
            let ord = match (lines.get(i), locals.get(j)) {
                (Some(l), Some(c)) => walk_cmp(&l.p, &c.path),
                (Some(_), None) => Ordering::Less,
                (None, _) => Ordering::Greater,
            };
            match ord {
                Ordering::Less => {
                    if let Some(t) = self.task(&lines[i], None, paused)? {
                        tasks.push(t);
                    }
                    i += 1;
                }
                Ordering::Greater => {
                    self.remove_local(&locals[j].path)?;
                    j += 1;
                }
                Ordering::Equal => {
                    if let Some(t) = self.task(&lines[i], Some(&locals[j]), paused)? {
                        tasks.push(t);
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        // parts of files no longer listed in this page are left: the next round settles them
        Ok(tasks)
    }

    /// What one listed file needs, given its copy (if any). Outside a pause a copy with the same length and time
    /// (not racy) is taken as the same; under the pause the file id is compared too, so the last round is exact.
    fn task(&self, l: &Line, c: Option<&Local>, paused: bool) -> io::Result<Option<Task>> {
        let kind = proto::kind_of_letter(&l.k).unwrap_or(Kind::Plain);
        let id = l.id.as_deref().and_then(proto::hex_n::<16>);
        let v = match (kind, &l.id) {
            (Kind::Plain, _) | (_, None) => proto::plain_version(l.len, l.mt),
            (_, Some(id)) => id.to_ascii_lowercase(),
        };
        let whole = Task { p: l.p.clone(), kind, len: l.len, v, id, what: What::Whole };
        let (racy, refetch) = {
            let st = lock(&self.st);
            (st.racy.contains(&l.p), st.refetch.contains(&l.p))
        };
        let Some(c) = c else { return Ok(Some(whole)) };
        if refetch {
            return Ok(Some(whole));
        }
        let same = c.len == l.len && c.mt == l.mt;
        if same && !racy && !paused {
            return Ok(None);
        }
        let full = self.copy_dir().join(&c.path);
        let (ckind, cid) = local_head(&full)?;
        if ckind != kind || cid != id {
            return Ok(Some(whole));
        }
        if same && !racy {
            return Ok(None);
        }
        match kind {
            Kind::Sealed | Kind::Plain => Ok(Some(whole)),
            Kind::Records => {
                if c.len > l.len || (c.len as usize) < tree::HEAD + proto::TAIL as usize {
                    return Ok(Some(whole));
                }
                let tail = read_tail(&full, c.len)?;
                Ok(Some(Task { what: What::Append { from: c.len, tail }, ..whole }))
            }
        }
    }

    /// Fetches the tasks with the connections at once: small files many to a request, big ones a piece at a time.
    fn fetch_all(&self, conns: &mut [Client], tasks: Vec<Task>, paused: bool) -> Result<Round, Problem> {
        if tasks.is_empty() {
            return Ok(Round::default());
        }
        let mut queue: VecDeque<Vec<Task>> = VecDeque::new();
        let mut batch: Vec<Task> = Vec::new();
        let mut size = 0u64;
        for t in tasks {
            let n = match t.what {
                What::Whole => t.len,
                What::Append { from, .. } => t.len - from,
            };
            if n > proto::PIECE {
                queue.push_back(vec![t]);
                continue;
            }
            if batch.len() >= proto::MAX_WANTS || size + n > proto::MAX_FETCH {
                queue.push_back(std::mem::take(&mut batch));
                size = 0;
            }
            size += n;
            batch.push(t);
        }
        if !batch.is_empty() {
            queue.push_back(batch);
        }
        let queue = Mutex::new(queue);
        let total = Mutex::new(Round::default());
        let failed: Mutex<Option<Problem>> = Mutex::new(None);
        std::thread::scope(|s| {
            for c in conns.iter_mut() {
                let (queue, total, failed) = (&queue, &total, &failed);
                s.spawn(move || {
                    loop {
                        if lock(failed).is_some() {
                            return;
                        }
                        let Some(b) = lock(queue).pop_front() else { return };
                        match self.fetch_batch(c, b, paused) {
                            Ok(r) => {
                                let mut t = lock(total);
                                t.files += r.files;
                                t.bytes += r.bytes;
                            }
                            Err(p) => {
                                let mut f = lock(failed);
                                if f.is_none() {
                                    *f = Some(p);
                                }
                                return;
                            }
                        }
                    }
                });
            }
        });
        if let Some(p) = lock(&failed).take() {
            return Err(p);
        }
        let r = *lock(&total);
        Ok(r)
    }

    fn want(&self, t: &Task) -> Want {
        match &t.what {
            What::Whole => {
                let from = lock(&self.st).parts.get(&t.p).filter(|p| p.v == t.v && p.len == t.len).map_or(0, |p| p.got);
                Want { p: t.p.clone(), from, to: (from + proto::PIECE).min(t.len), v: t.v.clone(), tail: None }
            }
            What::Append { from, tail } => Want {
                p: t.p.clone(),
                from: *from,
                to: (*from + proto::PIECE).min(t.len),
                v: t.v.clone(),
                tail: Some(hex::encode(tail)),
            },
        }
    }

    /// Fetches one batch until every task in it is settled (big files a piece at a time).
    fn fetch_batch(&self, c: &mut Client, mut batch: Vec<Task>, paused: bool) -> Result<Round, Problem> {
        let bearer = self.bearer();
        let mut done = Round::default();
        while !batch.is_empty() {
            self.check_stop()?;
            // what this request settled survives a cut connection: the next try asks only for the rest
            let mut left: Vec<Task> = Vec::new();
            let mut answered = 0usize;
            self.call(c, |c| {
                let todo = &batch[answered..];
                if todo.is_empty() {
                    return Ok(());
                }
                let wants: Vec<Want> = todo.iter().map(|t| self.want(t)).collect();
                let body = serde_json::to_vec(&proto::FetchRequest { want: wants.clone() }).unwrap_or_default();
                let mut r = c.request(
                    "POST",
                    &format!("{}/fetch", proto::BASE),
                    &[("Authorization", &bearer), ("Content-Type", "application/json")],
                    &body,
                )?;
                if r.status != 200 {
                    return Err(refused(r));
                }
                let mut buf = Vec::with_capacity(2048);
                let mut k = 0usize;
                while let Some(f) = read_frame(&mut r, &mut buf)? {
                    let i = k;
                    k += 1;
                    let (Some(t), Some(w)) = (todo.get(i), wants.get(i)) else {
                        return Err(Fail::Net(NetError::Protocol("more frames than wants".into())));
                    };
                    if f.p != w.p || f.from != w.from || f.count > w.to - w.from {
                        return Err(Fail::Net(NetError::Protocol("a frame for another want".into())));
                    }
                    // the copy is written under the lock, shared: `move cancel` waits for it, then removes the folder
                    let lockf = open_lock(&self.data)?;
                    flock(&lockf, FlockOperation::LockShared).map_err(|e| Problem::io(e.into()))?;
                    self.check_stop()?;
                    let next = self.apply(&mut r, t, w, &f, paused, &mut done)?;
                    drop(lockf);
                    answered += 1;
                    if let Some(next) = next {
                        left.push(next);
                    }
                }
                // the wants the answer left out are asked again
                left.extend(todo.iter().skip(k).cloned());
                answered = batch.len();
                // nothing may follow the end; reading to it keeps the connection for the next request
                let mut rest = [0u8; 64];
                if r.read(&mut rest).map_err(|e| Fail::Net(NetError::Io(e)))? > 0 {
                    return Err(Fail::Net(NetError::Protocol("bytes after a fetch answer's end".into())));
                }
                Ok(())
            })?;
            self.save()?;
            self.log_progress();
            batch = left;
        }
        Ok(done)
    }

    fn settled(&self, p: &str, f: &Frame) {
        let mut st = lock(&self.st);
        st.parts.remove(p);
        st.refetch.remove(p);
        if proto::racy(f.mt, f.read_at) {
            st.racy.insert(p.to_string());
        } else {
            st.racy.remove(p);
        }
    }

    /// Writes one frame's bytes into the copy. `Some(task)`: what is left of the file, for the next request.
    fn apply(
        &self,
        r: &mut impl Read,
        t: &Task,
        w: &Want,
        f: &Frame,
        paused: bool,
        done: &mut Round,
    ) -> Result<Option<Task>, Fail> {
        if f.flags & proto::GONE != 0 {
            self.remove_local(&t.p)?;
            return Ok(None);
        }
        if f.flags & proto::CHANGED != 0 {
            let mut st = lock(&self.st);
            if let Some(part) = st.parts.remove(&t.p) {
                let _ = std::fs::remove_file(self.staging().join(PARTS).join(part.n.to_string()));
            }
            st.refetch.insert(t.p.clone());
            return Ok(None);
        }
        match &t.what {
            What::Whole => {
                let (n, carry_on) = {
                    let mut st = lock(&self.st);
                    match st.parts.get(&t.p).filter(|p| p.v == t.v && p.len == t.len && p.got == f.from && f.from > 0) {
                        Some(p) => (p.n, true),
                        None => {
                            let n = st.next_part;
                            st.next_part += 1;
                            (n, false)
                        }
                    }
                };
                let part_path = self.staging().join(PARTS).join(n.to_string());
                let mut file = if carry_on {
                    let mut x = std::fs::OpenOptions::new().write(true).open(&part_path)?;
                    x.set_len(f.from)?;
                    x.seek(SeekFrom::End(0))?;
                    x
                } else {
                    if f.from != 0 {
                        // a piece of a part this copy no longer has: start the file over
                        copy_exact(r, &mut io::sink(), f.count)?;
                        lock(&self.st).parts.remove(&t.p);
                        return Ok(Some(t.clone()));
                    }
                    crate::export::create_private(&part_path)?
                };
                copy_exact(r, &mut file, f.count)?;
                let got = f.from + f.count;
                done.bytes += f.count;
                self.progress_bytes(f.count, 0);
                if got < t.len {
                    file.sync_all()?;
                    lock(&self.st).parts.insert(t.p.clone(), PartRec { n, v: t.v.clone(), len: t.len, got });
                    return Ok(Some(t.clone()));
                }
                drop(file);
                // whole: checked, timed, synced, then renamed into place
                let (kind, id) = local_head(&part_path)?;
                if kind != t.kind || id != t.id {
                    let _ = std::fs::remove_file(&part_path);
                    let mut st = lock(&self.st);
                    st.parts.remove(&t.p);
                    st.refetch.insert(t.p.clone());
                    return Ok(None);
                }
                let file = std::fs::OpenOptions::new().write(true).open(&part_path)?;
                file.set_modified(std::time::UNIX_EPOCH + Duration::from_micros(f.mt))?;
                file.sync_all()?;
                drop(file);
                let dst = self.copy_dir().join(&t.p);
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if dst.is_dir() {
                    std::fs::remove_dir_all(&dst)?;
                }
                std::fs::rename(&part_path, &dst)?;
                if let Some(parent) = dst.parent() {
                    crate::import::sync_dir(parent);
                }
                self.settled(&t.p, f);
                done.files += 1;
                self.progress_bytes(0, 1);
                self.file_copied(&t.p);
                Ok(None)
            }
            What::Append { from, .. } => {
                let dst = self.copy_dir().join(&t.p);
                let mut file = std::fs::OpenOptions::new().read(true).write(true).open(&dst)?;
                let have = file.metadata()?.len();
                if have != *from {
                    drop(file);
                    // its bytes still have to be read off the answer
                    copy_exact(r, &mut io::sink(), f.count)?;
                    // asked again after a cut connection, once it had arrived: nothing more to do
                    if have != t.len {
                        lock(&self.st).refetch.insert(t.p.clone());
                    }
                    return Ok(None);
                }
                file.seek(SeekFrom::End(0))?;
                copy_exact(r, &mut file, f.count)?;
                let mut end = f.from + f.count;
                if !paused {
                    // whole records only while writes go on (a piece may end inside one: it is fetched again)
                    let whole = vault::records::whole_end_from(&file, f.from)?.unwrap_or(f.from);
                    if whole < end {
                        file.set_len(whole)?;
                        end = whole;
                    }
                    if end < w.to && f.count == w.to - w.from {
                        // bytes the old server listed as whole records aren't: fetched whole again
                        file.set_len(f.from)?;
                        lock(&self.st).refetch.insert(t.p.clone());
                        return Ok(None);
                    }
                }
                file.sync_all()?;
                done.bytes += end - f.from;
                self.progress_bytes(end - f.from, 0);
                if end < t.len {
                    drop(file);
                    let tail = read_tail(&dst, end)?;
                    return Ok(Some(Task { what: What::Append { from: end, tail }, ..t.clone() }));
                }
                // synced, then timed
                file.set_modified(std::time::UNIX_EPOCH + Duration::from_micros(f.mt))?;
                self.settled(&t.p, f);
                done.files += 1;
                self.progress_bytes(0, 1);
                Ok(None)
            }
        }
    }

    fn progress_bytes(&self, bytes: u64, files: u64) {
        let mut st = lock(&self.st);
        st.progress.bytes_done += bytes;
        st.progress.files_done += files;
        if st.progress.bytes_done > st.progress.bytes_total {
            st.progress.bytes_total = st.progress.bytes_done;
        }
    }

    fn log_progress(&self) {
        let p = lock(&self.st).progress.clone();
        if p.round != 1 {
            return;
        }
        let pc = percent(&p);
        let (t0, b0) = *lock(&self.began).get_or_insert((Instant::now(), p.bytes_done));
        let mut l = lock(&self.logged);
        if pc / 5 > l.1 / 5 && l.0.elapsed() >= Duration::from_secs(10) {
            *l = (Instant::now(), pc);
            let rate = p.bytes_done.saturating_sub(b0) as f64 / t0.elapsed().as_secs_f64().max(1.0);
            let left = if rate > 0.0 {
                let s = p.bytes_total.saturating_sub(p.bytes_done) as f64 / rate;
                format!(", about {} left", minutes(Duration::from_secs_f64(s.min(1e7))))
            } else {
                String::new()
            };
            tracing::info!(
                "move: copying: {pc} % ({} of {}, {} of {} files){left}",
                crate::size(p.bytes_done),
                crate::size(p.bytes_total),
                p.files_done,
                p.files_total
            );
        }
    }
}

fn minutes(d: Duration) -> String {
    let m = d.as_secs() / 60;
    if m == 0 { format!("{} s", d.as_secs()) } else { format!("{m} min") }
}

/// The kind and file id of a file of the copy.
fn local_head(p: &Path) -> io::Result<(Kind, Option<[u8; 16]>)> {
    let mut f = File::open(p)?;
    let mut head = [0u8; tree::HEAD];
    let n = crate::export::read_up_to(&mut f, &mut head)?;
    Ok(tree::head_kind(&head[..n]))
}

/// The `n` bytes of a frame from the answer `r` into `w`: a failed read is the network's, a failed write the disk's.
fn copy_exact(r: &mut impl Read, w: &mut impl Write, n: u64) -> Result<(), Fail> {
    let mut buf = vec![0u8; (n as usize).min(1 << 16)];
    let mut left = n;
    while left > 0 {
        let k = (left as usize).min(buf.len());
        r.read_exact(&mut buf[..k]).map_err(|e| Fail::Net(NetError::Io(e)))?;
        w.write_all(&buf[..k]).map_err(|e| Fail::Here(Problem::io(e)))?;
        left -= k as u64;
    }
    Ok(())
}

/// The 16 bytes of a copy before `end`.
fn read_tail(p: &Path, end: u64) -> io::Result<[u8; 16]> {
    let mut f = File::open(p)?;
    let mut tail = [0u8; 16];
    f.seek(SeekFrom::Start(end - proto::TAIL))?;
    f.read_exact(&mut tail)?;
    Ok(tail)
}

/// Reads the next frame's head (`None`: the end).
fn read_frame(r: &mut impl Read, buf: &mut Vec<u8>) -> Result<Option<Frame>, Fail> {
    buf.clear();
    let mut one = [0u8; 1];
    loop {
        if buf.len() > 2048 {
            return Err(Fail::Net(NetError::Protocol("a frame head too long".into())));
        }
        let n = r.read(&mut one).map_err(|e| Fail::Net(NetError::Io(e)))?;
        if n == 0 {
            return Err(Fail::Net(NetError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "a fetch answer was cut short",
            ))));
        }
        buf.push(one[0]);
        match proto::decode(buf) {
            proto::Read::Need => {}
            proto::Read::Bad(w) => return Err(Fail::Net(NetError::Protocol(w.into()))),
            proto::Read::End(_, _) => return Ok(None),
            proto::Read::Frame(f, _) => return Ok(Some(f)),
        }
    }
}

/// A list answer: its lines (checked: carried paths, in walk order, after `after`) and its end.
fn parse_list(b: &[u8], after: Option<&str>) -> Result<(Vec<Line>, End), String> {
    let mut lines = Vec::new();
    let mut prev: Option<String> = after.map(str::to_string);
    for raw in b.split(|&c| c == b'\n').filter(|l| !l.is_empty()) {
        let v: serde_json::Value = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
        if v.get("end").and_then(serde_json::Value::as_bool) == Some(true) {
            let end: End = serde_json::from_value(v).map_err(|e| e.to_string())?;
            return Ok((lines, end));
        }
        let l: Line = serde_json::from_value(v).map_err(|e| e.to_string())?;
        if !tree::carried(&l.p, Purpose::Move) || proto::kind_of_letter(&l.k).is_none() {
            return Err(format!("a path the move doesn't carry: {:?}", l.p));
        }
        if let Some(p) = &prev
            && walk_cmp(p, &l.p) != Ordering::Less
        {
            return Err("a list out of order".into());
        }
        prev = Some(l.p.clone());
        lines.push(l);
    }
    Err("a list cut short".into())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// A disk that rounds file times (to the second) is found before the code is used, and refused with words the
    /// owner can act on; this sandbox's disk keeps them exactly.
    #[test]
    fn a_disk_that_rounds_file_times_is_refused_before_the_claim() {
        let d = tempfile::tempdir().unwrap();
        assert!(keeps_exact_times(d.path(), |f, t| f.set_modified(t)).unwrap());
        let to_seconds = |f: &File, t: std::time::SystemTime| {
            let s = t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
            f.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(s))
        };
        assert!(!keeps_exact_times(d.path(), to_seconds).unwrap());
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0, "the test file is gone");
        exact_times(d.path()).unwrap();
        let p = times_problem();
        assert_eq!(p.code, "disk_times");
        assert!(!p.retry && p.message.contains("ZFS"), "{}", p.message);
    }

    /// The fetch threads save the state at once: every save works, and the state still opens after.
    #[test]
    fn saves_at_once_dont_trip_over_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&keys).unwrap();
        let code = crate::code::MoveCode::random().unwrap();
        stage(&data, &keys, &Address::parse("127.0.0.1").unwrap(), &code.keys(), &[7u8; 16], &[]).unwrap();
        let opts = Options::new("test");
        let pull = Pull::open(&data, &keys, &opts).unwrap();
        std::thread::scope(|s| {
            for t in 0..8u64 {
                let pull = &pull;
                s.spawn(move || {
                    for i in 0..40u64 {
                        lock(&pull.st).progress.bytes_done = t * 1000 + i;
                        pull.save().unwrap();
                    }
                });
            }
        });
        assert!(load(&data, &keys).unwrap().is_some());
    }
}
