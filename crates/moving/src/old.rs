//! The old server's side of a direct move (`docs/plans/move-server.md`, section 2): the move code, the claim, the
//! lists and fetches the new server pulls with, the pause, the finish, and the moved state afterwards.
//!
//! Everything is kept in `<data>/.move/` (a dot folder: never carried), sealed with the vault:
//!
//! - `code.sealed`: the code waiting, or claimed: its keys (never the code, except one made by
//!   `SENTRY_MATE_OFFER_MOVE`), when it expires, the wrong tries, and once claimed, who took it and the session's
//!   SHA-256.
//! - `moved.sealed`: written when the move finishes (the commit point): when, to where, the session and the keys to
//!   answer the finish again, and when the new server confirmed it serves. Kept until `move undo`.
//! - `cancelled.sealed`: the last sessions that were cancelled here, so their requests are answered `410 cancelled`.
//!
//! The running server reads these files on every request, so a command run beside it (`move code`, `move cancel`,
//! `move undo`) changes what it does at once; `.move/lock` keeps changes one at a time. The pause is in memory only:
//! a restart ends it.

use crate::Error;
use crate::code::MoveCode;
use crate::proto::{
    self, ClaimAnswer, ClaimRequest, End, FetchRequest, FinishAnswer, FinishRequest, Frame, Line, PauseAnswer,
    StatusAnswer, SwitchedRequest,
};
use crate::tree::{self, Found, Kind, Purpose, Skips};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as AxPath, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use ingest::{Conn, Exporter};
use rustix::fs::{FlockOperation, flock};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::net::IpAddr;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use vault::Vault;
use zeroize::Zeroizing;

/// The move's own folder in the data folder (never carried).
pub const DIR: &str = crate::import::NOTES;
const LOCK: &str = "lock";
const CODE: &str = "code.sealed";
const MOVED: &str = "moved.sealed";
const CANCELLED: &str = "cancelled.sealed";

/// A code can be claimed for this long.
pub const CODE_VALID_MS: u64 = 24 * 3600 * 1000;
/// Wrong tries from one address (IPv6: its /64) in [`WRONG_WINDOW`] before it must wait.
pub const MAX_WRONG_PER_ADDRESS: usize = 10;
pub const WRONG_WINDOW: Duration = Duration::from_secs(600);
/// Wrong tries in all before the code stops working.
pub const MAX_WRONG: u32 = 100;
/// A move whose new server sends nothing for this long ends (while copying; never once moved).
pub const SESSION_IDLE_MS: u64 = 7 * 24 * 3600 * 1000;
/// A pause ends when the new server sends nothing for this long, or after [`PAUSE_MOST`] in all.
pub const PAUSE_QUIET: Duration = Duration::from_secs(60);
pub const PAUSE_MOST: Duration = Duration::from_secs(15 * 60);
/// A pause waits this long once the writes have stopped, so file times settle (nothing is racy then).
pub const PAUSE_SETTLE: Duration = Duration::from_secs(2);
/// How long a pause waits for the writes under way.
pub const PAUSE_WAIT: Duration = Duration::from_secs(60);
/// What the bridge is told to wait on a moved server.
pub const MOVED_RETRY_S: u64 = 3600;
/// The heads kept in memory at most (about 100 bytes each).
const MAX_HEADS: usize = 2_000_000;

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn now_us() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_micros() as u64)
}

/// A time for people: "2026-11-17 at 14:02" (UTC).
pub fn when(ms: u64) -> String {
    let s = ms / 1000;
    format!("{} at {:02}:{:02} UTC", crate::export::day_of(ms), (s / 3600) % 24, (s / 60) % 60)
}

fn other(e: impl std::fmt::Display) -> Error {
    Error::Io(io::Error::other(e.to_string()))
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct CodeRecord {
    v: u32,
    id: String,
    auth: String,
    wrap: String,
    made_ms: u64,
    expires_ms: u64,
    anywhere: bool,
    /// The code itself: only for one made by `SENTRY_MATE_OFFER_MOVE`, printed in the log on every start until it
    /// is claimed (with no shell, the log is the owner's only screen).
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    wrong: u32,
    #[serde(default)]
    claimed: Option<Claimed>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Claimed {
    claim: String,
    session_sha256: String,
    from: String,
    at_ms: u64,
    app: String,
    last_seen_ms: u64,
    #[serde(default)]
    sent_bytes: u64,
    files: u64,
    bytes: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct MovedRecord {
    v: u32,
    at_ms: u64,
    to: String,
    code_id: String,
    claim: String,
    auth: String,
    wrap: String,
    session_sha256: String,
    key_id: String,
    app: String,
    switched_ms: Option<u64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct CancelledRecord {
    session_sha256: String,
    at_ms: u64,
    why: String,
}

fn path(data: &Path, name: &str) -> PathBuf {
    data.join(DIR).join(name)
}

fn read<T: serde::de::DeserializeOwned>(data: &Path, vault: &Vault, name: &str) -> Result<Option<T>, Error> {
    let Some(b) = vault.read_file(&path(data, name), &format!("{DIR}/{name}"))? else { return Ok(None) };
    serde_json::from_slice(&b).map(Some).map_err(|e| other(format!("{DIR}/{name}: {e}")))
}

fn write<T: serde::Serialize>(data: &Path, vault: &Vault, name: &str, v: &T) -> Result<(), Error> {
    let b = Zeroizing::new(serde_json::to_vec(v).map_err(other)?);
    vault.write_file(&path(data, name), &format!("{DIR}/{name}"), &b)?;
    Ok(())
}

/// `.move/lock`, held while the move's state changes.
struct DirLock(#[allow(dead_code)] File);

fn lock_dir(data: &Path) -> io::Result<DirLock> {
    std::fs::create_dir_all(data.join(DIR))?;
    let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path(data, LOCK))?;
    flock(&f, FlockOperation::LockExclusive)?;
    Ok(DirLock(f))
}

/// Whether this server's data moved away (cheap: one look for the file).
pub fn is_moved(data: &Path) -> bool {
    path(data, MOVED).exists()
}

/// What the gate answers on a server whose data moved away.
pub fn moved_closed() -> gate::Closed {
    gate::Closed {
        code: "moved",
        message: "This server's data moved to another server: change the server address in the CAN Bridge app.".into(),
        retry_after_s: MOVED_RETRY_S,
    }
}

fn moved_words(m: &MovedRecord) -> String {
    format!("this server's data moved to {} on {}", m.to, when(m.at_ms))
}

/// A refusal of a command or a change on a moved server.
pub fn refuse_if_moved(data: &Path, vault: &Vault) -> Result<(), Error> {
    match read::<MovedRecord>(data, vault, MOVED)? {
        Some(m) => Err(Error::Move(format!(
            "{}: it no longer takes changes (to take uploads here again: `move undo`)",
            moved_words(&m)
        ))),
        None => Ok(()),
    }
}

/// A code just made, shown once.
pub struct Made {
    pub code: Zeroizing<String>,
    pub id: String,
    pub expires_ms: u64,
    pub anywhere: bool,
}

fn new_code(data: &Path, vault: &Vault, anywhere: bool, keep_text: bool) -> Result<Made, Error> {
    let code = MoveCode::random().map_err(other)?;
    let k = code.keys();
    let now = now_ms();
    let text = code.text();
    let rec = CodeRecord {
        v: 1,
        id: k.id.clone(),
        auth: hex::encode(*k.auth),
        wrap: hex::encode(*k.wrap),
        made_ms: now,
        expires_ms: now + CODE_VALID_MS,
        anywhere,
        text: keep_text.then(|| text.to_string()),
        wrong: 0,
        claimed: None,
    };
    write(data, vault, CODE, &rec)?;
    Ok(Made { code: text, id: k.id, expires_ms: rec.expires_ms, anywhere })
}

fn add_cancelled(data: &Path, vault: &Vault, session_sha256: &str, why: &str) -> Result<(), Error> {
    let mut list: Vec<CancelledRecord> = read(data, vault, CANCELLED)?.unwrap_or_default();
    list.push(CancelledRecord { session_sha256: session_sha256.into(), at_ms: now_ms(), why: why.into() });
    let keep = list.len().saturating_sub(20);
    list.drain(..keep);
    write(data, vault, CANCELLED, &list)
}

/// Makes a move code (`move code`). With a move under way it refuses, unless `replace` (the move under way ends).
pub fn make_code(data: &Path, vault: &Vault, anywhere: bool, replace: bool) -> Result<Made, Error> {
    let _l = lock_dir(data)?;
    if let Some(m) = read::<MovedRecord>(data, vault, MOVED)? {
        return Err(Error::Move(format!("{}: to take uploads here again first: `move undo`", moved_words(&m))));
    }
    if let Some(c) = read::<CodeRecord>(data, vault, CODE)? {
        if let Some(cl) = &c.claimed {
            if !replace {
                return Err(Error::MoveUnderWay(format!(
                    "a move is under way: {} took the move code {} on {}",
                    cl.from,
                    c.id,
                    when(cl.at_ms)
                )));
            }
            add_cancelled(data, vault, &cl.session_sha256, "replaced by a new move code")?;
        }
    }
    new_code(data, vault, anywhere, false)
}

/// What `SENTRY_MATE_OFFER_MOVE` found or made on start.
pub enum EnvOffer {
    /// A new code (to print in the log).
    Made(Made),
    /// The code it made on an earlier start, still waiting (printed again).
    Waiting(Made),
    /// A code made with `move code` is waiting (it can't be shown again).
    WaitingUnshown { id: String, expires_ms: u64 },
    /// A move is under way: nothing new is made.
    UnderWay { from: String, at_ms: u64 },
    /// This server moved already: ignored.
    Moved,
}

/// Keeps a move code ready (`SENTRY_MATE_OFFER_MOVE`): a new one only when there is none, or the one there expired
/// unclaimed or had too many wrong tries; never while a move is under way or after one.
pub fn offer_from_env(data: &Path, vault: &Vault, anywhere: bool) -> Result<EnvOffer, Error> {
    let _l = lock_dir(data)?;
    if read::<MovedRecord>(data, vault, MOVED)?.is_some() {
        return Ok(EnvOffer::Moved);
    }
    if let Some(c) = read::<CodeRecord>(data, vault, CODE)? {
        if let Some(cl) = &c.claimed {
            return Ok(EnvOffer::UnderWay { from: cl.from.clone(), at_ms: cl.at_ms });
        }
        if c.expires_ms > now_ms() && c.wrong < MAX_WRONG {
            return Ok(match c.text {
                Some(t) => EnvOffer::Waiting(Made {
                    code: Zeroizing::new(t),
                    id: c.id,
                    expires_ms: c.expires_ms,
                    anywhere: c.anywhere,
                }),
                None => EnvOffer::WaitingUnshown { id: c.id, expires_ms: c.expires_ms },
            });
        }
    }
    new_code(data, vault, anywhere, true).map(EnvOffer::Made)
}

/// What `move cancel` did on the old server.
#[derive(Debug, PartialEq, Eq)]
pub enum Cancelled {
    Nothing,
    /// A code not claimed yet stopped working.
    Revoked {
        id: String,
    },
    /// A move under way ended (the new server is told `410 cancelled`).
    Ended {
        to: String,
    },
}

/// `move cancel` on the old server: the code stops working, a move not finished ends, a pause ends (the running
/// server sees it within seconds). After the move: refused, use `move undo`.
pub fn cancel(data: &Path, vault: &Vault) -> Result<Cancelled, Error> {
    let _l = lock_dir(data)?;
    if let Some(m) = read::<MovedRecord>(data, vault, MOVED)? {
        return Err(Error::Finished(format!(
            "the move finished: {}. To take uploads here again: `move undo`",
            moved_words(&m)
        )));
    }
    let Some(c) = read::<CodeRecord>(data, vault, CODE)? else { return Ok(Cancelled::Nothing) };
    if let Some(cl) = &c.claimed {
        add_cancelled(data, vault, &cl.session_sha256, "cancelled on the old server")?;
    }
    crate::import::remove_file(&path(data, CODE))?;
    Ok(match c.claimed {
        Some(cl) => Cancelled::Ended { to: cl.from },
        None => Cancelled::Revoked { id: c.id },
    })
}

/// Where this server's data moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedInfo {
    pub at_ms: u64,
    pub to: String,
    pub code_id: String,
    /// When the new server said it serves the data.
    pub switched_ms: Option<u64>,
}

impl From<&MovedRecord> for MovedInfo {
    fn from(m: &MovedRecord) -> MovedInfo {
        MovedInfo { at_ms: m.at_ms, to: m.to.clone(), code_id: m.code_id.clone(), switched_ms: m.switched_ms }
    }
}

/// `move undo`: this server takes uploads again (the running one within seconds). `None`: it hadn't moved.
pub fn undo(data: &Path, vault: &Vault) -> Result<Option<MovedInfo>, Error> {
    let _l = lock_dir(data)?;
    let Some(m) = read::<MovedRecord>(data, vault, MOVED)? else { return Ok(None) };
    // kept as a record of what happened (it opens under its old label)
    std::fs::rename(path(data, MOVED), path(data, &format!("undone-{}.sealed", now_ms())))?;
    crate::import::sync_dir(&data.join(DIR));
    Ok(Some(MovedInfo::from(&m)))
}

/// The code waiting or claimed, as `move status` and the UI show it.
#[derive(Debug, Clone)]
pub struct CodeInfo {
    pub id: String,
    pub expires_ms: u64,
    pub anywhere: bool,
    pub wrong: u32,
    pub claimed: Option<ClaimInfo>,
}

#[derive(Debug, Clone)]
pub struct ClaimInfo {
    pub from: String,
    pub at_ms: u64,
    pub app: String,
    pub last_seen_ms: u64,
    pub sent_bytes: u64,
    pub files: u64,
    pub bytes: u64,
}

/// What the old side of a move is doing (`move status`).
#[derive(Debug, Clone, Default)]
pub struct OldStatus {
    pub code: Option<CodeInfo>,
    pub moved: Option<MovedInfo>,
    pub last_cancelled: Option<(u64, String)>,
}

pub fn status(data: &Path, vault: &Vault) -> Result<OldStatus, Error> {
    let code = read::<CodeRecord>(data, vault, CODE)?.map(|c| CodeInfo {
        id: c.id,
        expires_ms: c.expires_ms,
        anywhere: c.anywhere,
        wrong: c.wrong,
        claimed: c.claimed.map(|cl| ClaimInfo {
            from: cl.from,
            at_ms: cl.at_ms,
            app: cl.app,
            last_seen_ms: cl.last_seen_ms,
            sent_bytes: cl.sent_bytes,
            files: cl.files,
            bytes: cl.bytes,
        }),
    });
    let moved = read::<MovedRecord>(data, vault, MOVED)?.map(|m| MovedInfo::from(&m));
    let last_cancelled =
        read::<Vec<CancelledRecord>>(data, vault, CANCELLED)?.and_then(|l| l.last().map(|c| (c.at_ms, c.why.clone())));
    Ok(OldStatus { code, moved, last_cancelled })
}

/// Makes the waiting code expire now (tests).
#[doc(hidden)]
pub fn expire_code_now(data: &Path, vault: &Vault) -> Result<(), Error> {
    let _l = lock_dir(data)?;
    if let Some(mut c) = read::<CodeRecord>(data, vault, CODE)? {
        c.expires_ms = now_ms().saturating_sub(1);
        write(data, vault, CODE, &c)?;
    }
    Ok(())
}

/// Where the data moved, if it did.
pub fn moved_info(data: &Path, vault: &Vault) -> Result<Option<MovedInfo>, Error> {
    Ok(read::<MovedRecord>(data, vault, MOVED)?.map(|m| MovedInfo::from(&m)))
}

/// Whether an address is on the home network: loopback, private IPv4, link-local, IPv6 unique local (the rule of
/// `accounts::net`).
pub fn home_network(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => a.is_loopback() || a.is_private() || a.is_link_local(),
        IpAddr::V6(a) => match a.to_ipv4_mapped() {
            Some(v4) => home_network(IpAddr::V4(v4)),
            None => {
                let first = a.segments()[0];
                a.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
            }
        },
    }
}

/// Who wrong tries count against: the address, or for IPv6 its /64.
fn who(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(a) => a.to_string(),
        IpAddr::V6(a) => match a.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = a.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
    }
}

fn shown(ip: IpAddr) -> String {
    match ip {
        IpAddr::V6(a) => a.to_ipv4_mapped().map_or_else(|| a.to_string(), |v4| v4.to_string()),
        v4 => v4.to_string(),
    }
}

/// An answer of the move protocol that refuses: `{ "error": "<words>", "code": "<code>", ... }`.
#[derive(Debug)]
pub struct Refusal {
    status: StatusCode,
    code: &'static str,
    message: String,
    extra: serde_json::Map<String, serde_json::Value>,
    retry_after: Option<u64>,
}

impl Refusal {
    fn new(status: u16, code: &'static str, message: impl Into<String>) -> Refusal {
        Refusal {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code,
            message: message.into(),
            extra: serde_json::Map::new(),
            retry_after: None,
        }
    }

    fn with(mut self, k: &str, v: impl Into<serde_json::Value>) -> Refusal {
        self.extra.insert(k.into(), v.into());
        self
    }

    fn retry(mut self, s: u64) -> Refusal {
        self.retry_after = Some(s);
        self
    }

    fn server(e: impl std::fmt::Display) -> Refusal {
        tracing::error!("move: {e}");
        Refusal::new(500, "server_error", "Something went wrong on the old server; try again later.")
    }
}

impl From<Error> for Refusal {
    fn from(e: Error) -> Refusal {
        Refusal::server(e)
    }
}

impl From<vault::Error> for Refusal {
    fn from(e: vault::Error) -> Refusal {
        Refusal::server(e)
    }
}

impl From<io::Error> for Refusal {
    fn from(e: io::Error) -> Refusal {
        Refusal::server(e)
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let mut body = self.extra;
        body.insert("error".into(), self.message.into());
        body.insert("code".into(), self.code.into());
        let mut res = (self.status, axum::Json(serde_json::Value::Object(body))).into_response();
        if let Some(s) = self.retry_after {
            res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(s));
        }
        res
    }
}

fn not_found() -> Refusal {
    Refusal::new(404, "not_found", "Nothing here.")
}

fn https_only() -> Refusal {
    Refusal::new(403, "https_only", "Use HTTPS: this address only answers over TLS.")
}

fn bad_request(why: &str) -> Refusal {
    Refusal::new(400, "bad_request", format!("The request doesn't read: {why}."))
}

use proto::hex_n;

/// What a file's head says, as the list and the digest give it.
#[derive(Clone, Copy)]
struct Head {
    len: u64,
    mt: u64,
    kind: Kind,
    id: Option<[u8; 16]>,
    /// For a records file: the end of its last whole record, once known.
    whole: Option<u64>,
}

fn open_nofollow(p: &Path) -> io::Result<File> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut o, rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    o.open(p)
}

#[cfg(unix)]
fn ids(md: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (md.dev(), md.ino())
}

#[cfg(not(unix))]
fn ids(_: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

/// A pause of the writes for the move's last round.
struct Paused {
    id: String,
    session: String,
    _pause: Option<gate::Pause>,
    /// Files, bytes and digest of the tree at the pause (made again after a mismatch).
    digest: (u64, u64, String),
    started: Instant,
    last: Instant,
    /// Requests being answered: while any is, the pause's timers don't run.
    busy: usize,
}

#[derive(Default)]
struct Seen {
    saved: Option<Instant>,
    last_ms: u64,
    sent: u64,
    logged_tenths: u64,
}

/// The old server's side of a move, for the running server: the `/api/move/v1` routes ([`router`]) and the moved
/// state (the write gate closed and held while moved).
pub struct Offer {
    data: PathBuf,
    vault: Arc<Vault>,
    gate: gate::Gate,
    app: String,
    cert_sha256: String,
    wrong: Mutex<HashMap<String, VecDeque<Instant>>>,
    heads: Mutex<HashMap<(u64, u64), Head>>,
    paused: Mutex<Option<Paused>>,
    /// The pause, the finish and the end of a session, one at a time.
    op: tokio::sync::Mutex<()>,
    /// The gate held for good while moved.
    hold: Mutex<Option<gate::Pause>>,
    moved: watch::Sender<bool>,
    seen: Mutex<Seen>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

enum Who {
    Copying(Box<CodeRecord>),
    Moved(Box<MovedRecord>),
}

impl Offer {
    /// The move side of a server on `data`. A server whose data moved closes its write gate and holds it from here
    /// on (blocking: waits for writes under way, at most a minute). `cert_sha256` is the upload port's certificate.
    pub fn open(
        data: &Path,
        vault: Arc<Vault>,
        gate: gate::Gate,
        app: &str,
        cert_sha256: &str,
    ) -> Result<Arc<Offer>, Error> {
        let (tx, _) = watch::channel(false);
        let o = Arc::new(Offer {
            data: data.to_path_buf(),
            vault,
            gate,
            app: app.to_string(),
            cert_sha256: cert_sha256.to_string(),
            wrong: Mutex::new(HashMap::new()),
            heads: Mutex::new(HashMap::new()),
            paused: Mutex::new(None),
            op: tokio::sync::Mutex::new(()),
            hold: Mutex::new(None),
            moved: tx,
            seen: Mutex::new(Seen::default()),
        });
        if let Some(m) = read::<MovedRecord>(&o.data, &o.vault, MOVED)? {
            o.hold_moved()?;
            tracing::info!(
                "move: {}; it takes no uploads and tells the CAN Bridge to try later. To take uploads here again: `move undo`",
                moved_words(&m)
            );
        }
        Ok(o)
    }

    fn hold_moved(&self) -> Result<(), Error> {
        self.gate.close(moved_closed());
        let pause = self.gate.pause(PAUSE_WAIT)?;
        *lock(&self.hold) = Some(pause);
        self.moved.send_replace(true);
        Ok(())
    }

    /// Whether this server's data moved (it takes no uploads).
    pub fn is_moved(&self) -> bool {
        lock(&self.hold).is_some()
    }

    /// Follows the moved state: `true` once moved, `false` again after `move undo`.
    pub fn watch(&self) -> watch::Receiver<bool> {
        self.moved.subscribe()
    }

    /// Runs the timers: a pause that went quiet ends, `move undo` and `move cancel` from the command line are seen,
    /// a move not heard from in 7 days ends. Stop it by aborting the task.
    pub fn spawn_ticker(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let o = self.clone();
        tokio::spawn(async move {
            let mut n: u64 = 0;
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                o.pause_timers();
                if n % 5 == 0 {
                    let t = o.clone();
                    let _ = tokio::task::spawn_blocking(move || t.look_at_disk(n % 60 == 0)).await;
                }
                n += 1;
            }
        })
    }

    /// What the ticker does, at once (tests).
    #[doc(hidden)]
    pub fn tick_now(&self) {
        self.pause_timers();
        self.look_at_disk(true);
    }

    fn pause_timers(&self) {
        let mut p = lock(&self.paused);
        let Some(x) = p.as_ref() else { return };
        if x.busy > 0 {
            return;
        }
        let why = if x.started.elapsed() > PAUSE_MOST {
            "it lasted 15 minutes"
        } else if x.last.elapsed() > PAUSE_QUIET {
            "the new server went quiet"
        } else {
            return;
        };
        *p = None;
        tracing::info!("move: the pause ended ({why}); uploads go on, and the new server catches up and asks again");
    }

    fn end_pause(&self, why: &str) {
        if lock(&self.paused).take().is_some() {
            tracing::info!("move: the pause ended ({why}); uploads go on");
        }
    }

    fn look_at_disk(&self, idle_check: bool) {
        let moved_file = is_moved(&self.data);
        if self.is_moved() && !moved_file {
            // `move undo` from the command line
            *lock(&self.hold) = None;
            self.gate.reopen();
            self.moved.send_replace(false);
            tracing::warn!(
                "move: undone: this server takes uploads again. If the new server also takes uploads, the two drift apart: stop it."
            );
            return;
        }
        let code = read::<CodeRecord>(&self.data, &self.vault, CODE).ok().flatten();
        let session = code.as_ref().and_then(|c| c.claimed.as_ref()).map(|c| c.session_sha256.clone());
        let paused_for = lock(&self.paused).as_ref().map(|p| p.session.clone());
        if let Some(s) = paused_for {
            if session.as_deref() != Some(s.as_str()) && !self.is_moved() {
                self.end_pause("the move was cancelled here");
            }
        }
        self.save_seen(false);
        if idle_check {
            if let Some(cl) = code.and_then(|c| c.claimed) {
                let last = cl.last_seen_ms.max(lock(&self.seen).last_ms);
                if now_ms().saturating_sub(last) > SESSION_IDLE_MS {
                    let _l = lock_dir(&self.data);
                    let _ = add_cancelled(&self.data, &self.vault, &cl.session_sha256, "not heard from in 7 days");
                    let _ = crate::import::remove_file(&path(&self.data, CODE));
                    tracing::warn!(
                        "move: the move to {} ended: not heard from since {}. Make a new move code to move again.",
                        cl.from,
                        when(last)
                    );
                }
            }
        }
    }

    /// Writes when the new server was last heard from, and how much it fetched (at most once a minute).
    fn save_seen(&self, force: bool) {
        let (last_ms, sent) = {
            let mut s = lock(&self.seen);
            if s.last_ms == 0 || (!force && s.saved.is_some_and(|t| t.elapsed() < Duration::from_secs(60))) {
                return;
            }
            s.saved = Some(Instant::now());
            (s.last_ms, s.sent)
        };
        let Ok(_l) = lock_dir(&self.data) else { return };
        if let Ok(Some(mut c)) = read::<CodeRecord>(&self.data, &self.vault, CODE) {
            if let Some(cl) = c.claimed.as_mut() {
                cl.last_seen_ms = cl.last_seen_ms.max(last_ms);
                cl.sent_bytes = cl.sent_bytes.max(sent);
                let _ = write(&self.data, &self.vault, CODE, &c);
            }
        }
    }

    fn wrong_count(&self, who: &str) -> usize {
        let mut w = lock(&self.wrong);
        let now = Instant::now();
        let n = match w.get_mut(who) {
            Some(q) => {
                while q.front().is_some_and(|t| now.duration_since(*t) > WRONG_WINDOW) {
                    q.pop_front();
                }
                q.len()
            }
            None => 0,
        };
        if w.len() > 100_000 {
            w.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) <= WRONG_WINDOW));
        }
        n
    }

    fn count_wrong(&self, who: &str) {
        let mut w = lock(&self.wrong);
        let q = w.entry(who.to_string()).or_default();
        q.push_back(Instant::now());
        while q.len() > MAX_WRONG_PER_ADDRESS + 1 {
            q.pop_front();
        }
    }

    /// The session of a request, and what it belongs to.
    fn authorize(&self, headers: &HeaderMap) -> Result<(Who, [u8; 32]), Refusal> {
        let given = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .and_then(|s| hex_n::<32>(s.trim()));
        let moved = read::<MovedRecord>(&self.data, &self.vault, MOVED)?;
        let code = read::<CodeRecord>(&self.data, &self.vault, CODE)?;
        let Some(session) = given else {
            if moved.is_none() && code.is_none() {
                return Err(not_found());
            }
            return Err(Refusal::new(401, "bad_session", "No session, or not one of this move."));
        };
        let sh = proto::session_hash(&session);
        if let Some(m) = moved {
            if proto::same(m.session_sha256.as_bytes(), sh.as_bytes()) {
                self.heard();
                return Ok((Who::Moved(Box::new(m)), session));
            }
        } else if let Some(c) = &code {
            if c.claimed.as_ref().is_some_and(|cl| proto::same(cl.session_sha256.as_bytes(), sh.as_bytes())) {
                self.heard();
                return Ok((Who::Copying(Box::new(c.clone())), session));
            }
        }
        if let Some(list) = read::<Vec<CancelledRecord>>(&self.data, &self.vault, CANCELLED)? {
            if let Some(c) = list.iter().rev().find(|c| c.session_sha256 == sh) {
                return Err(Refusal::new(
                    410,
                    "cancelled",
                    format!("The move was cancelled on the old server ({}).", c.why),
                )
                .with("at_ms", c.at_ms));
            }
        }
        if !is_moved(&self.data) && code.is_none() {
            return Err(not_found());
        }
        Err(Refusal::new(401, "bad_session", "No session, or not one of this move."))
    }

    fn heard(&self) {
        lock(&self.seen).last_ms = now_ms();
    }

    /// The pause this session holds, counted as busy while the guard lives (its timers don't run meanwhile).
    fn busy(self: &Arc<Self>, session: &str) -> Option<(String, Busy)> {
        let mut p = lock(&self.paused);
        let x = p.as_mut().filter(|x| x.session == session)?;
        x.busy += 1;
        x.last = Instant::now();
        Some((x.id.clone(), Busy(self.clone())))
    }

    fn head(&self, f: &Found<'_>, need_whole: bool) -> io::Result<Option<Head>> {
        let key = (f.dev, f.ino);
        let cached = lock(&self.heads).get(&key).copied();
        if let Some(h) = cached {
            if h.len == f.len && h.mt == f.mtime_us && (h.kind != Kind::Records || !need_whole || h.whole.is_some()) {
                return Ok(Some(h));
            }
        }
        let mut file = match open_nofollow(f.full) {
            Ok(x) => x,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let md = file.metadata()?;
        let (len, mt, key) = (md.len(), tree::mtime_us(&md), ids(&md));
        let mut buf = [0u8; tree::HEAD];
        let n = crate::export::read_up_to(&mut file, &mut buf)?;
        let (kind, id) = tree::head_kind(&buf[..n]);
        let whole = if kind == Kind::Records && need_whole {
            // a file that only grew since: walked on from where it was walked to
            let from =
                cached.filter(|c| c.id == id && c.kind == Kind::Records).and_then(|c| c.whole).filter(|w| *w <= len);
            match from {
                Some(w) => vault::records::whole_end_from(&file, w)?,
                None => vault::records::whole_end(&file)?,
            }
        } else {
            None
        };
        let h = Head { len, mt, kind, id, whole };
        let mut heads = lock(&self.heads);
        if heads.len() >= MAX_HEADS {
            heads.clear();
        }
        heads.insert(key, h);
        Ok(Some(h))
    }

    /// Every carried file's line in one walk, as the digest takes them (records whole as stored).
    fn digest(&self) -> Result<(u64, u64, String), Error> {
        let mut d = proto::Digest::default();
        let mut skips = Skips::default();
        tree::walk(&self.data, Purpose::Move, &mut skips, |f| -> Result<ControlFlow<()>, Error> {
            if let Some(h) = self.head(f, false)? {
                d.add(f.path, h.kind, h.len, h.mt, h.id.map(hex::encode).as_deref());
            }
            Ok(ControlFlow::Continue(()))
        })?;
        Ok((d.files, d.bytes, d.hex()))
    }

    fn count(&self) -> Result<(u64, u64), Error> {
        let (mut files, mut bytes) = (0u64, 0u64);
        tree::walk(&self.data, Purpose::Move, &mut Skips::default(), |f| -> Result<ControlFlow<()>, Error> {
            files += 1;
            bytes += f.len;
            Ok(ControlFlow::Continue(()))
        })?;
        Ok((files, bytes))
    }

    fn claim_blocking(&self, ip: IpAddr, exporter: [u8; 32], r: &ClaimRequest) -> Result<ClaimAnswer, Refusal> {
        let _l = lock_dir(&self.data)?;
        let Some(mut c) = read::<CodeRecord>(&self.data, &self.vault, CODE)? else {
            if is_moved(&self.data) {
                return Err(Refusal::new(409, "code_used", "This server's data moved already."));
            }
            return Err(not_found());
        };
        if !c.anywhere && !home_network(ip) {
            return Err(Refusal::new(
                403,
                "not_home_network",
                "This move code works from the old server's home network only: make one with `move code --anywhere` for a new server elsewhere.",
            ));
        }
        let who = who(ip);
        if self.wrong_count(&who) >= MAX_WRONG_PER_ADDRESS {
            return Err(Refusal::new(
                429,
                "slow_down",
                "Too many wrong move codes from this address; wait a few minutes.",
            )
            .retry(WRONG_WINDOW.as_secs()));
        }
        if c.wrong >= MAX_WRONG {
            return Err(Refusal::new(
                403,
                "wrong_code",
                "This move code had too many wrong tries and no longer works: make a new one on the old server.",
            ));
        }
        let auth =
            Zeroizing::new(hex_n::<32>(&c.auth).ok_or_else(|| Refusal::server("a code record that doesn't read"))?);
        let proof = hex_n::<32>(&r.proof).unwrap_or([0; 32]);
        if !proto::same(&proto::client_proof(&auth, &exporter), &proof) {
            self.count_wrong(&who);
            c.wrong += 1;
            write(&self.data, &self.vault, CODE, &c)?;
            if c.wrong == MAX_WRONG {
                tracing::warn!(
                    "move: the move code {} had {MAX_WRONG} wrong tries and no longer works: make a new one",
                    c.id
                );
            } else {
                tracing::warn!("move: a wrong move code from {}", shown(ip));
            }
            return Err(Refusal::new(
                403,
                "wrong_code",
                "The old server didn't take this code: check it, or make a new one there.",
            ));
        }
        // proven. An older new server is refused before the code is used up
        match (crate::import::version(&r.app), crate::import::version(&self.app)) {
            (Some(theirs), Some(ours)) if theirs < ours => {
                return Err(Refusal::new(
                    409,
                    "too_old",
                    format!(
                        "The old server runs a newer Sentry Mate ({}) than the new one ({}): update the new server first (the code still works).",
                        self.app, r.app
                    ),
                )
                .with("app", self.app.clone()));
            }
            (Some(_), Some(_)) => {}
            _ => return Err(bad_request("its version")),
        }
        let claim = hex_n::<16>(&r.claim).ok_or_else(|| bad_request("its claim"))?;
        let session = proto::session(&auth, &claim);
        let (files, bytes) = self.count()?;
        match &c.claimed {
            Some(cl) if cl.claim == r.claim => {}
            Some(cl) => {
                return Err(Refusal::new(
                    409,
                    "code_used",
                    format!("This move code was used already, by {} on {}.", cl.from, when(cl.at_ms)),
                )
                .with("at_ms", cl.at_ms)
                .with("from", cl.from.clone()));
            }
            None => {
                if now_ms() > c.expires_ms {
                    return Err(Refusal::new(
                        410,
                        "code_expired",
                        format!("This move code expired on {}.", when(c.expires_ms)),
                    )
                    .with("expired_ms", c.expires_ms));
                }
                let now = now_ms();
                c.claimed = Some(Claimed {
                    claim: r.claim.clone(),
                    session_sha256: proto::session_hash(&session),
                    from: shown(ip),
                    at_ms: now,
                    app: r.app.clone(),
                    last_seen_ms: now,
                    sent_bytes: 0,
                    files,
                    bytes,
                });
                c.text = None;
                write(&self.data, &self.vault, CODE, &c)?;
                *lock(&self.seen) = Seen { last_ms: now, ..Seen::default() };
                tracing::info!(
                    "move: a server at {} took the move code ({}; Sentry Mate {}) and is copying the data: {files} files, {}",
                    shown(ip),
                    c.id,
                    r.app,
                    crate::size(bytes)
                );
            }
        }
        let part = Vault::recovery_part(&self.data)?;
        let mut a = ClaimAnswer {
            session: hex::encode(session),
            server_proof: String::new(),
            app: self.app.clone(),
            cert_sha256: self.cert_sha256.clone(),
            files,
            bytes,
            recovery: serde_json::to_string(&part).map_err(Refusal::server)?,
        };
        a.server_proof = hex::encode(proto::server_proof(&auth, &exporter, &a));
        Ok(a)
    }

    fn list_blocking(&self, after: Option<&str>, limit: usize, paused: Option<String>) -> Result<Vec<u8>, Error> {
        let mut out = Vec::with_capacity(limit.min(4096) * 128);
        let mut skips = Skips::default();
        let (mut n, mut last, mut more) = (0usize, None::<String>, false);
        let whole = paused.is_some();
        let now = now_us();
        tree::walk_from(&self.data, Purpose::Move, &mut skips, after, |f| -> Result<ControlFlow<()>, Error> {
            if n >= limit {
                more = true;
                return Ok(ControlFlow::Break(()));
            }
            let Some(h) = self.head(f, !whole)? else { return Ok(ControlFlow::Continue(())) };
            let len = match (h.kind, whole) {
                (Kind::Records, false) => h.whole.unwrap_or(h.len),
                _ => h.len,
            };
            let line = Line {
                p: f.path.to_string(),
                k: proto::kind_letter(h.kind).into(),
                len,
                mt: h.mt,
                id: h.id.map(hex::encode),
            };
            serde_json::to_writer(&mut out, &line).map_err(other)?;
            out.push(b'\n');
            n += 1;
            last = Some(f.path.to_string());
            Ok(ControlFlow::Continue(()))
        })?;
        let end = End {
            end: true,
            next: if more { last } else { None },
            now_us: now,
            paused,
            skipped: skips.named.iter().map(Into::into).collect(),
            skipped_count: skips.count,
        };
        serde_json::to_writer(&mut out, &end).map_err(other)?;
        out.push(b'\n');
        Ok(out)
    }

    /// Writes the frames of a fetch answer through `send` (`false`: the new server went away).
    fn fetch_blocking(&self, wants: &[proto::Want], mut send: impl FnMut(Bytes) -> bool) -> io::Result<()> {
        const OUT: usize = 1 << 20;
        let mut buf: Vec<u8> = Vec::with_capacity(OUT + 256);
        let (mut budget, mut frames, mut sent) = (proto::MAX_FETCH, 0u64, 0u64);
        let mut flush = |buf: &mut Vec<u8>| -> io::Result<()> {
            if !send(Bytes::from(std::mem::take(buf))) {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the new server went away"));
            }
            *buf = Vec::with_capacity(OUT + 256);
            Ok(())
        };
        // folders checked to be real folders (not links), so a want can't reach outside the data folder
        let mut real_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut inside = |p: &str| -> bool {
            let mut at = 0;
            while let Some(i) = p[at..].find('/') {
                let dir = &p[..at + i];
                if !real_dirs.contains(dir) {
                    match std::fs::symlink_metadata(self.data.join(dir)) {
                        Ok(m) if m.is_dir() => {
                            real_dirs.insert(dir.to_string());
                        }
                        _ => return false,
                    }
                }
                at += i + 1;
            }
            true
        };
        for w in wants {
            if budget == 0 {
                break;
            }
            let read_at = now_us();
            let mut frame = Frame { p: w.p.clone(), from: w.from, count: 0, len_now: 0, mt: 0, read_at, flags: 0 };
            let opened = if tree::carried(&w.p, Purpose::Move) && w.to >= w.from && inside(&w.p) {
                match open_nofollow(&self.data.join(&w.p)) {
                    Ok(f) if f.metadata().is_ok_and(|m| m.is_file()) => Some(f),
                    Ok(_) => None,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                    Err(e) => return Err(e),
                }
            } else {
                None
            };
            let Some(mut file) = opened else {
                frame.flags = proto::GONE;
                frame.encode(&mut buf);
                frames += 1;
                continue;
            };
            let md = file.metadata()?;
            let (len, mt) = (md.len(), tree::mtime_us(&md));
            frame.len_now = len;
            frame.mt = mt;
            let mut head = [0u8; tree::HEAD];
            let n = crate::export::read_up_to(&mut file, &mut head)?;
            let (kind, id) = tree::head_kind(&head[..n]);
            let version = match (kind, id) {
                (Kind::Plain, _) | (_, None) => proto::plain_version(len, mt),
                (_, Some(id)) => hex::encode(id),
            };
            let mut ok = version == w.v && w.to <= len;
            if ok {
                if let Some(t) = &w.tail {
                    ok = w.from >= proto::TAIL && {
                        let mut b = [0u8; proto::TAIL as usize];
                        file.seek(SeekFrom::Start(w.from - proto::TAIL))?;
                        file.read_exact(&mut b).is_ok() && hex::encode(b) == *t
                    };
                }
            }
            if !ok {
                frame.flags = proto::CHANGED;
                frame.encode(&mut buf);
                frames += 1;
                continue;
            }
            let count = (w.to - w.from).min(budget);
            frame.count = count;
            frame.encode(&mut buf);
            frames += 1;
            file.seek(SeekFrom::Start(w.from))?;
            let mut left = count;
            while left > 0 {
                let at = buf.len();
                let take = (left as usize).min(OUT.saturating_sub(at).max(64 * 1024));
                buf.resize(at + take, 0);
                // the bytes were checked to be there: a file shorter now ends the answer (the new server asks again)
                file.read_exact(&mut buf[at..])?;
                left -= take as u64;
                if buf.len() >= OUT {
                    flush(&mut buf)?;
                }
            }
            budget -= count;
            sent += count;
            if count < w.to - w.from {
                break;
            }
        }
        proto::encode_end(&mut buf, frames);
        flush(&mut buf)?;
        self.sent(sent);
        Ok(())
    }

    fn sent(&self, n: u64) {
        let total = read::<CodeRecord>(&self.data, &self.vault, CODE)
            .ok()
            .flatten()
            .and_then(|c| c.claimed.map(|cl| (cl.from, cl.bytes.max(1))));
        let mut s = lock(&self.seen);
        s.sent += n;
        if let Some((to, total)) = total {
            let tenths = (s.sent.saturating_mul(10) / total).min(10);
            if tenths > s.logged_tenths {
                s.logged_tenths = tenths;
                tracing::info!(
                    "move: {to} has copied {} % ({} of {})",
                    tenths * 10,
                    crate::size(s.sent.min(total)),
                    crate::size(total)
                );
            }
        }
    }

    fn commit(&self, c: &CodeRecord, session: &[u8; 32]) -> Result<MovedRecord, Error> {
        let _l = lock_dir(&self.data)?;
        let cl = c.claimed.as_ref().ok_or_else(|| other("no claim"))?;
        let m = MovedRecord {
            v: 1,
            at_ms: now_ms(),
            to: cl.from.clone(),
            code_id: c.id.clone(),
            claim: cl.claim.clone(),
            auth: c.auth.clone(),
            wrap: c.wrap.clone(),
            session_sha256: proto::session_hash(session),
            key_id: self.vault.key_id().to_string(),
            app: cl.app.clone(),
            switched_ms: None,
        };
        write(&self.data, &self.vault, MOVED, &m)?;
        crate::import::remove_file(&path(&self.data, CODE))?;
        crate::import::sync_dir(&self.data.join(DIR));
        Ok(m)
    }

    fn finish_answer(&self, m: &MovedRecord, session: &[u8; 32]) -> Result<FinishAnswer, Refusal> {
        let wrap =
            Zeroizing::new(hex_n::<32>(&m.wrap).ok_or_else(|| Refusal::server("a moved record that doesn't read"))?);
        let data_key = self.vault.wrap_key(&wrap, &proto::data_key_purpose(&m.key_id, session))?;
        Ok(FinishAnswer { moved_ms: m.at_ms, key_id: m.key_id.clone(), data_key })
    }
}

/// A request being answered in a pause.
struct Busy(Arc<Offer>);

impl Drop for Busy {
    fn drop(&mut self) {
        if let Some(x) = lock(&self.0.paused).as_mut() {
            x.busy = x.busy.saturating_sub(1);
            x.last = Instant::now();
        }
    }
}

type St = State<Arc<Offer>>;

/// The move's routes (`/api/move/v1/...`), to merge into the upload port's router. With no code made, no move under
/// way and no move done, every one answers `404 not_found` like any unknown path.
pub fn router(offer: Arc<Offer>) -> Router {
    Router::new().route("/api/move/v1/{what}", any(dispatch)).with_state(offer)
}

async fn dispatch(State(o): St, AxPath(what): AxPath<String>, req: Request) -> Response {
    let r = match (req.method().as_str(), what.as_str()) {
        ("POST", "claim") => claim(o, req).await,
        ("GET", "list") => list(o, req).await,
        ("POST", "fetch") => fetch(o, req).await,
        ("POST", "pause") => pause(o, req).await,
        ("POST", "finish") => finish(o, req).await,
        ("POST", "switched") => switched(o, req).await,
        ("GET", "status") => status_route(o, req).await,
        ("DELETE", "session") => end_session(o, req).await,
        _ => Err(not_found()),
    };
    r.unwrap_or_else(IntoResponse::into_response)
}

fn tls_of(req: &Request) -> Result<Conn, Refusal> {
    req.extensions().get::<Conn>().copied().filter(|c| c.tls).ok_or_else(https_only)
}

async fn body_json<T: serde::de::DeserializeOwned>(req: Request, max: usize) -> Result<T, Refusal> {
    let b = axum::body::to_bytes(req.into_body(), max).await.map_err(|_| bad_request("too long"))?;
    serde_json::from_slice(&b).map_err(|e| bad_request(&e.to_string()))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, Refusal> + Send + 'static) -> Result<T, Refusal> {
    tokio::task::spawn_blocking(f).await.map_err(Refusal::server)?
}

/// The session of a request (the request's parts are taken before anything waits: a body isn't `Sync`).
fn auth(o: &Arc<Offer>, req: &Request) -> impl std::future::Future<Output = Result<(Who, [u8; 32]), Refusal>> + use<> {
    let tls = tls_of(req).map(|_| ());
    let (o, h) = (o.clone(), req.headers().clone());
    async move {
        tls?;
        blocking(move || o.authorize(&h)).await
    }
}

fn json(v: impl serde::Serialize) -> Result<Response, Refusal> {
    Ok(axum::Json(v).into_response())
}

async fn claim(o: Arc<Offer>, req: Request) -> Result<Response, Refusal> {
    let conn = tls_of(&req)?;
    let exporter = req.extensions().get::<Exporter>().copied().ok_or_else(https_only)?;
    let r: ClaimRequest = body_json(req, 64 * 1024).await?;
    if r.v != 1 {
        return Err(bad_request("an unknown claim version"));
    }
    let a = blocking(move || o.claim_blocking(conn.ip, exporter.0, &r)).await?;
    json(a)
}

/// `?after=<path>&limit=<n>` (a path the walk carries needs no escaping; `%XX` is read all the same).
fn list_query(q: Option<&str>) -> Result<(Option<String>, Option<usize>), Refusal> {
    let (mut after, mut limit) = (None, None);
    for pair in q.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = unescape(v).ok_or_else(|| bad_request("its query"))?;
        match k {
            "after" if !v.is_empty() => after = Some(v),
            "after" => {}
            "limit" => limit = Some(v.parse().map_err(|_| bad_request("its limit"))?),
            _ => {}
        }
    }
    Ok((after, limit))
}

fn unescape(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

async fn list(o: Arc<Offer>, req: Request) -> Result<Response, Refusal> {
    let (who, session) = auth(&o, &req).await?;
    let (after, limit) = list_query(req.uri().query())?;
    let limit = limit.unwrap_or(proto::MAX_LIST).clamp(1, proto::MAX_LIST);
    // a moved server writes nothing: its lists are of one moment, like a pause's
    let moved = matches!(who, Who::Moved(_));
    let busy = o.busy(&proto::session_hash(&session));
    let o2 = o.clone();
    let body = blocking(move || {
        let paused = if moved { Some("moved".to_string()) } else { busy.as_ref().map(|b| b.0.clone()) };
        let out = o2.list_blocking(after.as_deref(), limit, paused)?;
        drop(busy);
        Ok(out)
    })
    .await?;
    let mut res = (StatusCode::OK, body).into_response();
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-ndjson"));
    Ok(res)
}

/// A body fed by a blocking task through a channel.
struct ChanBody(tokio::sync::mpsc::Receiver<Result<Bytes, io::Error>>);

impl http_body::Body for ChanBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, io::Error>>> {
        self.0.poll_recv(cx).map(|o| o.map(|r| r.map(http_body::Frame::data)))
    }
}

async fn fetch(o: Arc<Offer>, req: Request) -> Result<Response, Refusal> {
    let (_, session) = auth(&o, &req).await?;
    let r: FetchRequest = body_json(req, 8 << 20).await?;
    if r.want.len() > proto::MAX_WANTS {
        return Err(bad_request("too many wants"));
    }
    let busy = o.busy(&proto::session_hash(&session));
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, io::Error>>(4);
    tokio::task::spawn_blocking(move || {
        let _busy = busy;
        if let Err(e) = o.fetch_blocking(&r.want, |b| tx.blocking_send(Ok(b)).is_ok()) {
            if e.kind() != io::ErrorKind::BrokenPipe {
                tracing::warn!("move: a fetch stopped part way: {e}");
            }
            let _ = tx.blocking_send(Err(e));
        }
    });
    let mut res = Response::new(axum::body::Body::new(ChanBody(rx)));
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    Ok(res)
}

async fn pause(o: Arc<Offer>, req: Request) -> Result<Response, Refusal> {
    let (who, session) = auth(&o, &req).await?;
    let sh = proto::session_hash(&session);
    let _op = o.op.lock().await;
    let c = match who {
        Who::Moved(_) => return json(PauseAnswer { pause: "moved".into(), until_ms: 0 }),
        Who::Copying(c) => c,
    };
    if let Some(p) = lock(&o.paused).as_mut().filter(|p| p.session == sh) {
        p.last = Instant::now();
        let until = now_ms() + PAUSE_MOST.saturating_sub(p.started.elapsed()).as_millis() as u64;
        return json(PauseAnswer { pause: p.id.clone(), until_ms: until });
    }
    let g = o.gate.clone();
    let held = match tokio::task::spawn_blocking(move || g.pause(PAUSE_WAIT)).await.map_err(Refusal::server)? {
        Ok(p) => p,
        Err(gate::Refused::Busy) => {
            return Err(
                Refusal::new(503, "busy", "The old server's writes didn't stop in time; try again shortly.").retry(30)
            );
        }
        Err(e) => return Err(Refusal::server(e)),
    };
    tokio::time::sleep(PAUSE_SETTLE).await;
    let o2 = o.clone();
    let digest = blocking(move || o2.digest().map_err(Refusal::from)).await?;
    let mut id = [0u8; 8];
    getrandom::fill(&mut id).map_err(Refusal::server)?;
    let id = hex::encode(id);
    let now = Instant::now();
    *lock(&o.paused) =
        Some(Paused { id: id.clone(), session: sh, _pause: Some(held), digest, started: now, last: now, busy: 0 });
    let from = c.claimed.as_ref().map_or_else(String::new, |cl| cl.from.clone());
    tracing::info!("move: uploads paused for the last changes ({from} is catching up)");
    json(PauseAnswer { pause: id, until_ms: now_ms() + PAUSE_MOST.as_millis() as u64 })
}

async fn finish(o: Arc<Offer>, req: Request) -> Result<Response, Refusal> {
    let (who, session) = auth(&o, &req).await?;
    let sh = proto::session_hash(&session);
    let r: FinishRequest = body_json(req, 64 * 1024).await?;
    let _op = o.op.lock().await;
    let c = match who {
        // a lost finish answer: the same again (a new nonce), also weeks later
        Who::Moved(m) => return json(o.finish_answer(&m, &session)?),
        Who::Copying(c) => c,
    };
    let Some((id, busy)) = o.busy(&sh) else {
        return Err(Refusal::new(409, "not_paused", "No pause in force: catch up and ask for one again."));
    };
    if id != r.pause {
        return Err(Refusal::new(409, "not_paused", "That pause ended: catch up and ask for one again."));
    }
    let want = lock(&o.paused).as_ref().map(|p| p.digest.clone()).unwrap_or_default();
    if (r.files, r.bytes, r.digest.as_str()) != (want.0, want.1, want.2.as_str()) {
        // made again, so a change made by something else during the pause is caught up with in the next round
        let o2 = o.clone();
        let fresh = blocking(move || o2.digest().map_err(Refusal::from)).await?;
        if let Some(p) = lock(&o.paused).as_mut() {
            p.digest = fresh.clone();
        }
        drop(busy);
        tracing::info!(
            "move: the new server's copy differs ({} files, {}; here {} files, {}): it copies again",
            r.files,
            crate::size(r.bytes),
            fresh.0,
            crate::size(fresh.1)
        );
        return Err(Refusal::new(
            409,
            "digest_mismatch",
            "The copy differs from this server's files: copy what changed again.",
        )
        .with("files", fresh.0)
        .with("bytes", fresh.1));
    }
    // the commit: moved in memory first (every writer that gets the gate from now on is refused), then on disk
    o.gate.close(moved_closed());
    let o2 = o.clone();
    let committed = blocking(move || o2.commit(&c, &session).map_err(Refusal::from)).await;
    let m = match committed {
        Ok(m) => m,
        Err(e) => {
            o.gate.reopen();
            drop(busy);
            tracing::error!(
                "move: couldn't write that the data moved (is the disk full?); the move doesn't finish yet"
            );
            return Err(e);
        }
    };
    // the pause becomes the moved state's hold
    let held = lock(&o.paused).take().and_then(|mut p| p._pause.take());
    *lock(&o.hold) = held;
    drop(busy);
    o.moved.send_replace(true);
    tracing::info!(
        "move: moved: the data moved to {} on {}; it is switching now. Keep this server running until this log says done.",
        m.to,
        when(m.at_ms)
    );
    json(o.finish_answer(&m, &session)?)
}

async fn switched(o: Arc<Offer>, req: Request) -> Result<Response, Refusal> {
    let (who, _) = auth(&o, &req).await?;
    let r: SwitchedRequest = body_json(req, 4096).await?;
    let Who::Moved(m) = who else {
        return Err(Refusal::new(409, "not_moved", "The move hasn't finished here."));
    };
    if m.switched_ms.is_none() {
        let o2 = o.clone();
        let done = blocking(move || {
            let _l = lock_dir(&o2.data)?;
            let mut m: MovedRecord =
                read(&o2.data, &o2.vault, MOVED)?.ok_or_else(|| Refusal::server("no moved record"))?;
            m.switched_ms = Some(r.at_ms);
            write(&o2.data, &o2.vault, MOVED, &m)?;
            Ok(m)
        })
        .await?;
        tracing::info!(
            "move: done: {} serves the data since {}. This server no longer takes uploads and tells the CAN Bridge to try later. You can stop it now; keep its folders as a backup for a while, then delete them. To take uploads here again: `move undo`.",
            done.to,
            when(done.at_ms)
        );
    }
    json(serde_json::json!({ "ok": true }))
}

async fn status_route(o: Arc<Offer>, req: Request) -> Result<Response, Refusal> {
    let (who, session) = auth(&o, &req).await?;
    let a = match who {
        Who::Moved(m) => StatusAnswer {
            state: if m.switched_ms.is_some() { "done" } else { "moved" }.into(),
            pause: None,
            moved_ms: Some(m.at_ms),
            switched_ms: m.switched_ms,
        },
        Who::Copying(_) => {
            let sh = proto::session_hash(&session);
            let pause = lock(&o.paused).as_ref().filter(|p| p.session == sh).map(|p| p.id.clone());
            StatusAnswer {
                state: if pause.is_some() { "paused" } else { "copying" }.into(),
                pause,
                moved_ms: None,
                switched_ms: None,
            }
        }
    };
    json(a)
}

async fn end_session(o: Arc<Offer>, req: Request) -> Result<Response, Refusal> {
    let (who, _) = auth(&o, &req).await?;
    let _op = o.op.lock().await;
    let Who::Copying(c) = who else {
        return Err(Refusal::new(409, "finished", "The move finished; it can't be given up now."));
    };
    let o2 = o.clone();
    blocking(move || {
        let _l = lock_dir(&o2.data)?;
        if let Some(cl) = &c.claimed {
            add_cancelled(&o2.data, &o2.vault, &cl.session_sha256, "the new server gave it up")?;
        }
        crate::import::remove_file(&path(&o2.data, CODE))?;
        Ok(())
    })
    .await?;
    o.end_pause("the new server gave the move up");
    tracing::info!("move: the new server gave the move up; this server takes uploads as before");
    json(serde_json::json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner runs `move cancel` (or `move code --replace`) on the old server while the new server's finish is
    /// being answered: the finish read the claim before the cancel, and its commit must not write the moved state
    /// (nor hand over the data key) for a move the owner just ended.
    #[test]
    #[allow(clippy::unwrap_used)]
    fn a_cancel_made_during_a_finish_is_not_overridden_by_its_commit() {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
        let v = Arc::new(v);
        let o = Offer::open(&data, v.clone(), gate::Gate::memory(), "0.4.1", &"ab".repeat(32)).unwrap();
        make_code(&data, &v, false, false).unwrap();
        // the claim, as claim_blocking writes it
        let session = [7u8; 32];
        let mut c: CodeRecord = read(&data, &v, CODE).unwrap().unwrap();
        c.claimed = Some(Claimed {
            claim: hex::encode([1u8; 16]),
            session_sha256: proto::session_hash(&session),
            from: "192.0.2.10".into(),
            at_ms: now_ms(),
            app: "0.4.1".into(),
            last_seen_ms: now_ms(),
            sent_bytes: 0,
            files: 0,
            bytes: 0,
        });
        write(&data, &v, CODE, &c).unwrap();
        // the finish has authorized the request (it holds `c`) when the owner cancels
        let what = cancel(&data, &v).unwrap();
        assert_eq!(what, Cancelled::Ended { to: "192.0.2.10".into() });
        let committed = o.commit(&c, &session);
        assert!(committed.is_err(), "the commit wrote the moved state for a move the owner had cancelled");
        assert!(!is_moved(&data), "the old server now refuses uploads although `move cancel` said they go on");
    }

    #[test]
    fn the_home_network_rule() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.30",
            "169.254.1.1",
            "::1",
            "fd00::5",
            "fe80::1",
            "::ffff:192.168.1.2",
        ] {
            assert!(home_network(ip.parse().unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))), "{ip}");
        }
        for ip in ["8.8.8.8", "2001:db8::1", "::ffff:8.8.8.8", "100.64.0.1"] {
            assert!(!home_network(ip.parse().unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))), "{ip}");
        }
    }
}
