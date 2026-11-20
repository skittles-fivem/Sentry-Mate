//! People and sign-in for Sentry Mate's web UI.
//!
//! - **Accounts** ([`Accounts`]): a name, an argon2id password hash, a role ([`Role::Admin`] or
//!   [`Role::WatchOnly`]), optionally a set of cars it is limited to (any size, checked in O(1)), and its own choice
//!   of interface (UI pack). There is always at least one admin. Adding, changing or removing an account, or changing
//!   a password or role, asks for the acting account's own password again, and its two-step code when that is on
//!   ([`Confirm`]).
//! - **First run**: with no account yet, the first admin can only be made with a one-time setup code
//!   ([`Accounts::start_setup`]): random, written to a file the owner reads (e.g. `/keys/SETUP-CODE.txt`), used once,
//!   valid for 24 hours. Whoever reaches the page first can't claim the server.
//! - **Sessions** ([`sessions`]): a random 256-bit cookie token, only its SHA-256 kept; 30 days at most, 7 days idle;
//!   a CSRF token each; listed per account (device, created, last seen, address) and signed out one by one or all.
//! - **Two-step sign-in** ([`totp`]): authenticator app codes (RFC 6238), ten one-time recovery codes, and the
//!   options to require it for everyone or from outside the home network ([`TwoStepPolicy`]).
//! - **Throttling** ([`throttle`]): growing waits after failed tries, per address and per account, the same for
//!   accounts that don't exist (and the same time spent: a dummy hash is checked).
//! - **Audit log** ([`Audit`]): every sign-in and change, sealed, append-only, rotated monthly.
//!
//! Everything is kept sealed with the vault; nothing names an account in a file name. Built for any number of cars
//! and accounts: per-account files read only when that account is used (a bounded cache), a lock per account,
//! journals instead of rewritten files.
//!
//! The API is synchronous: hashing a password takes a few hundred milliseconds of CPU on purpose, so an async web
//! layer calls these from blocking tasks (`spawn_blocking`). At most a few hashes run at once whatever the load.

pub mod audit;
mod error;
mod journal;
pub mod net;
pub mod password;
pub mod role;
pub mod sessions;
mod store;
pub mod throttle;
pub mod totp;

pub use audit::{Audit, Entry, Page};
pub use error::{Error, Result};
pub use model::CarId;
pub use net::{is_local, parse_address};
pub use password::{Hashing, MIN_PASSWORD_CHARS, check_password};
pub use role::{CarAccess, Permission, Role};
pub use sessions::{
    COOKIE, COOKIE_ATTRIBUTES, CSRF_HEADER, IDLE_MS, SESSION_MS, SessionView, clear_cookie, csrf_matches, csrf_token,
    set_cookie, token_from_cookies,
};

use base64::Engine;
use password::Hasher;
use sessions::{Hash, Sessions};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use store::{Cache, Files, Index, Loaded, Secrets, Slot, SlotState, Summary, TotpSetup, TotpState};
use subtle::ConstantTimeEq;
use vault::Vault;
use zeroize::Zeroizing;

pub type AccountId = u64;

/// The time now, ms since 1970 UTC (a function, so tests can move time).
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The system clock.
pub fn system_clock() -> Clock {
    Arc::new(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64))
}

/// A sign-in waiting for its two-step code lasts this long.
pub const PENDING_MS: u64 = 5 * 60_000;
/// Wrong two-step codes before a waiting sign-in is dropped.
pub const PENDING_TRIES: u32 = 5;
/// Sign-ins waiting for a code, at most (the oldest go first).
pub const MAX_PENDING: usize = 10_000;
/// A two-step setup must be confirmed with a code within this.
pub const TWO_STEP_SETUP_MS: u64 = 15 * 60_000;
/// The setup code for the first admin is valid this long.
pub const SETUP_CODE_MS: u64 = 24 * 3600 * 1000;
pub const MAX_NAME_CHARS: usize = 64;
/// Sign-ins with a right password per account (home and outside counted apart) per minute, at most. Each takes one
/// of the few hash slots, and a right password clears the failure count: so someone who knows a password can't keep
/// the slots busy and the others out.
pub const RIGHT_PASSWORDS_PER_MINUTE: u32 = 20;

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Text cut to one line (control characters become spaces) of at most `max` characters.
pub(crate) fn one_line(s: &str, max: usize) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().chars().take(max).collect()
}

fn io_random(e: getrandom::Error) -> Error {
    Error::Io(std::io::Error::other(e))
}

/// How the accounts are kept and checked.
#[derive(Clone)]
pub struct Options {
    pub hashing: Hashing,
    pub clock: Clock,
    /// The name authenticator apps show for the codes.
    pub issuer: String,
    /// Password hashes at once at most (each takes `hashing.m_kib` of memory).
    pub max_parallel_hashes: usize,
    /// Accounts kept loaded at most (their secrets and cars).
    pub cached_accounts: usize,
    /// Addresses and accounts the throttle remembers at most.
    pub throttle_entries: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            hashing: Hashing::STRONG,
            clock: system_clock(),
            issuer: "Sentry Mate".into(),
            max_parallel_hashes: 2,
            cached_accounts: 1024,
            throttle_entries: throttle::MAX_TRACKED,
        }
    }
}

/// Who a request comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub address: IpAddr,
    /// Whether the address is on the home network (the web layer decides; [`is_local`] is the default rule).
    pub local: bool,
    /// The device, for the list of signed-in devices (e.g. "Firefox on Windows").
    pub device: String,
}

impl Client {
    /// A client at `address`, home or not by [`is_local`].
    pub fn new(address: IpAddr, device: &str) -> Client {
        Client { address: net::canonical(address), local: is_local(address), device: one_line(device, 120) }
    }
}

/// An account as shown and used.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AccountView {
    pub id: AccountId,
    pub name: String,
    pub role: Role,
    pub two_step: bool,
    /// How many cars it is limited to (`None`: all cars).
    pub cars: Option<u64>,
    /// The interface (UI pack) it chose for itself (`None`: the one chosen for everyone).
    pub ui_pack: Option<String>,
    pub created_ms: u64,
}

/// A new account.
pub struct NewAccount<'a> {
    pub name: &'a str,
    pub password: &'a str,
    pub role: Role,
    /// `None`: all cars.
    pub cars: Option<Vec<CarId>>,
}

/// A session just made: give the browser [`NewSession::set_cookie`] and the CSRF token.
pub struct NewSession {
    /// The cookie's value; never stored (only its SHA-256 is).
    pub token: Zeroizing<String>,
    pub csrf: String,
    pub session: SessionView,
    pub account: AccountView,
}

impl NewSession {
    /// The `Set-Cookie` header value.
    pub fn set_cookie(&self) -> String {
        set_cookie(&self.token)
    }
}

/// How a right password goes on.
pub enum SignIn {
    /// Signed in.
    Done(Box<NewSession>),
    /// The account has two-step sign-in: send `pending` back with a code ([`Accounts::finish_sign_in`]).
    NeedCode { pending: Zeroizing<String>, expires_ms: u64 },
}

/// A request's signed-in account, from [`Accounts::authenticate`]. Changes take it as proof of who acts.
pub struct SignedIn {
    pub account: AccountView,
    pub session: SessionView,
    pub cars: CarAccess,
    /// Two-step sign-in is required for every account and this one hasn't set it up: only that is open to it.
    pub two_step_setup_only: bool,
    csrf: String,
    client: Client,
    hash: Hash,
}

/// The acting account's own password asked again before a change, and its two-step code (or a recovery code), which
/// is needed too when that account has two-step sign-in on (each code works once). A bare `&str` is a password
/// without a code.
#[derive(Clone, Copy)]
pub struct Confirm<'a> {
    pub password: &'a str,
    pub code: Option<&'a str>,
    once: Option<&'a CodeOnce>,
}

/// For several changes made in one request with one [`Confirm`]: the first uses the code up, the ones after it in
/// the same request ask for the password again but not for a second code (an authenticator shows one per 30 s).
#[derive(Default)]
pub struct CodeOnce(AtomicBool);

impl<'a> Confirm<'a> {
    pub fn new(password: &'a str, code: Option<&'a str>) -> Confirm<'a> {
        Confirm { password, code, once: None }
    }

    /// One of several changes in one request (see [`CodeOnce`]).
    pub fn in_request(password: &'a str, code: Option<&'a str>, once: &'a CodeOnce) -> Confirm<'a> {
        Confirm { password, code, once: Some(once) }
    }
}

impl<'a> From<&'a str> for Confirm<'a> {
    fn from(password: &'a str) -> Confirm<'a> {
        Confirm::new(password, None)
    }
}

impl<'a> From<&'a String> for Confirm<'a> {
    fn from(password: &'a String) -> Confirm<'a> {
        Confirm::new(password, None)
    }
}

impl<'a> From<&'a Zeroizing<String>> for Confirm<'a> {
    fn from(password: &'a Zeroizing<String>) -> Confirm<'a> {
        Confirm::new(password, None)
    }
}

impl SignedIn {
    /// Whether this account may do this now.
    pub fn may(&self, p: Permission) -> bool {
        if self.two_step_setup_only {
            return p == Permission::OwnAccount;
        }
        self.account.role.may(p)
    }

    /// Whether this car may be shown to it.
    pub fn sees(&self, car: CarId) -> bool {
        !self.two_step_setup_only && self.cars.allows(car)
    }

    /// The session's CSRF token, for the page.
    pub fn csrf(&self) -> &str {
        &self.csrf
    }

    /// Checks a change's CSRF header.
    pub fn check_csrf(&self, header: Option<&str>) -> Result<()> {
        if csrf_matches(&self.csrf, header) { Ok(()) } else { Err(Error::Csrf) }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }
}

/// A two-step setup: the secret to type into the authenticator app, or the link that opens it there.
pub struct TwoStepSetup {
    pub secret: Zeroizing<String>,
    pub uri: Zeroizing<String>,
}

/// Recovery codes, shown once.
pub type RecoveryCodes = Zeroizing<Vec<String>>;

/// When two-step sign-in is required (both off by default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TwoStepPolicy {
    /// For every account, everywhere: an account without it can sign in only at home, and then only to set it up.
    pub everyone: bool,
    /// For sign-ins from outside the home network: an account without it can sign in only at home, and its sessions
    /// end on their first request from outside.
    pub outside: bool,
}

/// The setup code for the first admin, as written to the file.
pub struct SetupCode {
    pub code: Zeroizing<String>,
    pub expires_ms: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SetupState {
    sha256: String,
    expires_ms: u64,
    file: PathBuf,
}

struct Pending {
    account: AccountId,
    key: String,
    until_ms: u64,
    tries: u32,
    /// The password hash its password was checked against (see [`stamp`]).
    stamp: Stamp,
}

/// A password hash as it was when a password was checked against it. Every hash has its own salt, so a new password
/// (even the same one again) has another stamp: a sign-in whose stamp no longer matches was let through by a
/// password that has changed since, and gets no session.
type Stamp = [u8; 32];

fn stamp(phc: &str) -> Stamp {
    Sha256::digest(phc.as_bytes()).into()
}

/// How long a password check waits for another one of the same account name to end.
const ONE_AT_A_TIME_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The account names being checked now (one hash at a time per name and place, see [`Accounts::one_at_a_time`]).
#[derive(Default)]
struct Checking {
    names: Mutex<std::collections::HashSet<String>>,
    freed: std::sync::Condvar,
}

struct CheckingPass<'a> {
    checking: &'a Checking,
    key: String,
}

impl Drop for CheckingPass<'_> {
    fn drop(&mut self) {
        lock(&self.checking.names).remove(&self.key);
        self.checking.freed.notify_all();
    }
}

enum CodeUsed {
    Totp,
    Recovery { left: usize },
}

/// The accounts, sessions and sign-in rules of one server.
pub struct Accounts {
    dir: PathBuf,
    vault: Arc<Vault>,
    clock: Clock,
    issuer: String,
    hasher: Hasher,
    index: RwLock<Index>,
    files: Files,
    cache: Mutex<Cache>,
    sessions: Sessions,
    pending: Mutex<HashMap<Hash, Pending>>,
    throttle: throttle::Throttle,
    policy: RwLock<TwoStepPolicy>,
    audit: Arc<Audit>,
    setup_lock: Mutex<()>,
    /// The throttle key (name and place) of each password being checked now.
    checking: Checking,
    /// Right passwords per throttle key in the current minute: (minute, count).
    right: Mutex<HashMap<String, (u64, u32)>>,
}

fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Account names: 1 to 64 letters, digits and `. _ @ -` (so they are safe anywhere they are shown).
fn valid_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty()
        || n.len() > MAX_NAME_CHARS
        || !n.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'@' | b'-'))
    {
        return Err(Error::BadInput("An account name is 1 to 64 letters, digits and . _ @ -"));
    }
    Ok(n.to_string())
}

/// UI pack ids: 1 to 64 letters, digits and `. _ -`, not starting with a dot.
fn valid_pack(id: &str) -> Result<String> {
    let p = id.trim();
    if p.is_empty()
        || p.len() > 64
        || p.starts_with('.')
        || !p.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
    {
        return Err(Error::BadInput("An interface id is 1 to 64 letters, digits and . _ -"));
    }
    Ok(p.to_string())
}

const SETUP_SYMBOLS: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// What the setup code is printed with in front (`SETUP-XXXX-...`): it may be typed or left out.
pub const SETUP_PREFIX: &str = "SETUP";

/// The setup code as typed, in the form it is checked in: case, spaces, dashes and the [`SETUP_PREFIX`] in front
/// don't matter, and `O`, `I` and `L` read as `0` and `1`. (The code itself never has a `U`, so a code can't start
/// with `SETUP`.)
pub fn normal_setup_code(typed: &str) -> String {
    let text: String = typed.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect();
    let text = text.strip_prefix(SETUP_PREFIX).unwrap_or(&text);
    text.chars()
        .map(|c| match c {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect()
}

fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn random<const N: usize>() -> Result<Zeroizing<[u8; N]>> {
    let mut b = Zeroizing::new([0u8; N]);
    getrandom::fill(&mut *b).map_err(io_random)?;
    Ok(b)
}

fn pending_hash(token: &str) -> Option<Hash> {
    let body = token.strip_prefix("smp_")?;
    (body.len() == 43 && body.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'))
        .then(|| Sha256::digest(token.as_bytes()).into())
}

fn role_name(r: Role) -> &'static str {
    match r {
        Role::Admin => "admin",
        Role::WatchOnly => "watch only",
    }
}

fn cars_words(c: Option<u64>) -> String {
    match c {
        None => "all cars".into(),
        Some(1) => "1 car".into(),
        Some(n) => format!("{n} cars"),
    }
}

fn new_recovery(salt: &[u8]) -> Result<(RecoveryCodes, Vec<String>)> {
    let mut shown = Zeroizing::new(Vec::with_capacity(totp::RECOVERY_CODES));
    let mut kept = Vec::with_capacity(totp::RECOVERY_CODES);
    for _ in 0..totp::RECOVERY_CODES {
        let code = totp::new_recovery_code()?;
        let n = totp::normalize_recovery(&code).ok_or_else(|| error::corrupt("a new recovery code doesn't read"))?;
        kept.push(totp::hash_recovery(salt, &n));
        shown.push(code);
    }
    Ok((shown, kept))
}

impl Accounts {
    /// Opens the accounts kept in `dir` (made when needed). `audit` is the server's audit log, shared with its other
    /// parts. One `Accounts` per folder at a time (share it with an `Arc`).
    pub fn open(dir: impl Into<PathBuf>, vault: Arc<Vault>, audit: Arc<Audit>, options: Options) -> Result<Accounts> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        let now = (options.clock)();
        let index = Index::open(&vault, &dir.join("index.journal"))?;
        let sessions = Sessions::open(vault.clone(), &dir.join("sessions.journal"), now)?;
        let policy = match vault.read_file(&dir.join("policy.sealed"), "accounts/policy")? {
            Some(raw) => serde_json::from_slice(&raw).map_err(error::corrupt)?,
            None => TwoStepPolicy::default(),
        };
        Ok(Accounts {
            hasher: Hasher::new(options.hashing, options.max_parallel_hashes)?,
            files: Files::new(dir.clone(), vault.clone()),
            dir,
            vault,
            clock: options.clock,
            issuer: options.issuer,
            index: RwLock::new(index),
            cache: Mutex::new(Cache::new(options.cached_accounts)),
            sessions,
            pending: Mutex::new(HashMap::new()),
            throttle: throttle::Throttle::new(options.throttle_entries),
            policy: RwLock::new(policy),
            audit,
            setup_lock: Mutex::new(()),
            checking: Checking::default(),
            right: Mutex::new(HashMap::new()),
        })
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    fn index(&self) -> RwLockReadGuard<'_, Index> {
        self.index.read().unwrap_or_else(|e| e.into_inner())
    }

    fn index_mut(&self) -> RwLockWriteGuard<'_, Index> {
        self.index.write().unwrap_or_else(|e| e.into_inner())
    }

    fn summary(&self, id: AccountId) -> Option<Summary> {
        self.index().by_id.get(&id).cloned()
    }

    /// The throttle keys of a password try: the address (an IPv6 /64), and the account name typed (whether or not
    /// it exists), counted apart for home and outside.
    fn keys(&self, key: &str, client: &Client) -> Vec<String> {
        self.keys_for("account", key, client)
    }

    /// The throttle keys of a two-step code try. Codes are counted apart from passwords and only cleared by a right
    /// code, so someone who knows the password still can't try codes faster than the waits allow.
    fn code_keys(&self, key: &str, client: &Client) -> Vec<String> {
        self.keys_for("code", key, client)
    }

    fn keys_for(&self, what: &str, key: &str, client: &Client) -> Vec<String> {
        let name = &sha256_hex(key)[..32];
        let place = if client.local { "home" } else { "out" };
        vec![format!("ip:{}", net::throttle_key(client.address)), format!("{what}:{name}:{place}")]
    }

    fn slot(&self, id: AccountId) -> Arc<Slot> {
        lock(&self.cache).get(id)
    }

    /// Runs `f` on the account's secrets and cars under its own lock (loading them if needed).
    fn with_account<R>(&self, id: AccountId, f: impl FnOnce(&mut Loaded) -> Result<R>) -> Result<R> {
        let slot = self.slot(id);
        let mut st = lock(&slot.state);
        if matches!(*st, SlotState::Unloaded) {
            *st = match self.files.load(id)? {
                Some(l) => {
                    self.repair_two_step_flag(id, &l);
                    SlotState::Loaded(l)
                }
                None => SlotState::Gone,
            };
        }
        match &mut *st {
            SlotState::Loaded(l) => f(l),
            _ => Err(Error::NoSuchAccount),
        }
    }

    /// Whether two-step sign-in is on is the account's own file's say; the index's flag is written just after the
    /// file (under the account's lock), so a crash in between leaves them apart. Set right when the file is read, else
    /// an account whose two-step sign-in was being turned off would be asked for a code it can never give.
    fn repair_two_step_flag(&self, id: AccountId, l: &Loaded) {
        let on = l.secrets.totp.is_some();
        if self.summary(id).is_some_and(|s| s.two_step != on) {
            // the next load tries again if this can't be written
            let _ = self.set_two_step_flag(id, on);
        }
    }

    /// Changes an account's secrets: written first, then taken in (so memory never says what the disk doesn't).
    fn change_secrets<R>(&self, id: AccountId, f: impl FnOnce(&mut Secrets) -> Result<R>) -> Result<R> {
        self.change_secrets_and(id, f, |_| Ok(()))
    }

    /// The same, then `after` (e.g. the index's two-step flag) still under the account's lock, so a change made at
    /// the same time can't leave the index saying something else than the account's file.
    fn change_secrets_and<R>(
        &self,
        id: AccountId,
        f: impl FnOnce(&mut Secrets) -> Result<R>,
        after: impl FnOnce(&R) -> Result<()>,
    ) -> Result<R> {
        self.with_account(id, |l| {
            let mut next = l.secrets.clone();
            let r = f(&mut next)?;
            self.files.save_secrets(id, &next)?;
            l.secrets = next;
            after(&r)?;
            Ok(r)
        })
    }

    fn record(&self, who: Option<&str>, client: Option<&Client>, action: &str, details: &str) -> Result<()> {
        self.audit.record(who, client.map(|c| c.address), action, details)
    }

    /// The server's audit log.
    pub fn audit(&self) -> &Arc<Audit> {
        &self.audit
    }

    // ---- first run

    /// Whether there is no account yet (the first admin needs the setup code).
    pub fn needs_setup(&self) -> bool {
        self.index().by_id.is_empty()
    }

    /// Makes a new setup code (any earlier one stops working), writes it to `code_file` for the owner (readable by
    /// the server's user only), and returns it to print in the log too. Valid for [`SETUP_CODE_MS`], once.
    pub fn start_setup(&self, code_file: &Path) -> Result<SetupCode> {
        if !self.needs_setup() {
            return Err(Error::SetupDone);
        }
        let raw = random::<20>()?;
        let symbols: String = raw.iter().map(|b| SETUP_SYMBOLS[usize::from(b & 31)] as char).collect();
        // SETUP-XXXX-XXXX-XXXX-XXXX-XXXX (only the 20 symbols count)
        let groups = symbols.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned());
        let code =
            Zeroizing::new(std::iter::once(SETUP_PREFIX.to_string()).chain(groups).collect::<Vec<_>>().join("-"));
        let expires_ms = self.now() + SETUP_CODE_MS;
        vault::write_private(code_file, format!("{}\n", code.as_str()).as_bytes())?;
        let state = SetupState { sha256: sha256_hex(&symbols), expires_ms, file: code_file.to_path_buf() };
        let plain = serde_json::to_vec(&state).map_err(error::corrupt)?;
        self.vault.write_file(&self.dir.join("setup.sealed"), "accounts/setup", &plain)?;
        self.record(None, None, "setup-code-made", "a setup code for the first admin")?;
        Ok(SetupCode { code, expires_ms })
    }

    /// Makes the first admin with the setup code, and signs them in.
    pub fn setup(&self, code: &str, name: &str, password: &str, client: &Client) -> Result<NewSession> {
        self.setup_when(code, name, password, client, || Ok(()))
    }

    /// The same, asking `ready` once the code is known to be right and before the password is hashed: its error is
    /// the answer (say, a move or a restore runs), and the right code is not counted as a wrong try.
    pub fn setup_when<E: From<Error>>(
        &self,
        code: &str,
        name: &str,
        password: &str,
        client: &Client,
        ready: impl FnOnce() -> std::result::Result<(), E>,
    ) -> std::result::Result<NewSession, E> {
        let keys = vec![format!("ip:{}", net::throttle_key(client.address))];
        self.throttle.check(&keys, self.now())?;
        if !self.needs_setup() {
            return Err(Error::SetupDone.into());
        }
        let name = valid_name(name)?;
        check_password(&name, password)?;
        self.throttle.begin(&keys, self.now())?;
        let given = sha256_hex(&normal_setup_code(code));
        if let Err(e) = self.check_setup_code(&given) {
            if matches!(e, Error::WrongSetupCode) {
                self.record(None, Some(client), "setup-code-wrong", "")?;
            }
            return Err(e.into());
        }
        if let Err(e) = ready() {
            self.throttle.success(&keys);
            return Err(e);
        }
        let phc = {
            let _pass = self.hasher.gate.enter()?;
            Zeroizing::new(self.hasher.hash(password)?)
        };
        // one setup at a time from here: the code is checked again and used up under this lock, so two people with
        // the same code at the same moment make one admin
        let one = lock(&self.setup_lock);
        if !self.needs_setup() {
            return Err(Error::SetupDone.into());
        }
        let state = self.check_setup_code(&given)?;
        // the code is used up before the account exists: a crash in between leaves no code that works twice
        store::remove_if_there(&self.dir.join("setup.sealed"))?;
        store::remove_if_there(&state.file)?;
        let s = self.insert(&name, &phc, Role::Admin, None)?;
        drop(one);
        self.throttle.success(&keys);
        let ns = self.new_session(&s, client)?;
        self.record(Some(&s.name), Some(client), "setup", "the first admin account was made")?;
        Ok(ns)
    }

    /// The setup state, if `given` (the SHA-256 of the typed code) is the current code and it hasn't expired.
    fn check_setup_code(&self, given: &str) -> Result<SetupState> {
        let state: Option<SetupState> = match self.vault.read_file(&self.dir.join("setup.sealed"), "accounts/setup")? {
            Some(raw) => Some(serde_json::from_slice(&raw).map_err(error::corrupt)?),
            None => None,
        };
        let state = state.filter(|s| bool::from(s.sha256.as_bytes().ct_eq(given.as_bytes())));
        match state {
            None => Err(Error::WrongSetupCode),
            Some(s) if self.now() >= s.expires_ms => Err(Error::SetupCodeExpired),
            Some(s) => Ok(s),
        }
    }

    /// Adds an account: its id is taken and its name checked under the index lock, its files are written outside it
    /// (sign-ins and other changes don't wait for the disk), then its index line is written under the lock again.
    fn insert(&self, name: &str, phc: &str, role: Role, cars: Option<Vec<CarId>>) -> Result<Summary> {
        let key = name_key(name);
        let id = {
            let mut idx = self.index_mut();
            if idx.by_key.contains_key(&key) {
                return Err(Error::NameTaken);
            }
            let id = idx.next_id;
            idx.next_id += 1;
            id
        };
        let now = self.now();
        let secrets = Secrets {
            password: phc.to_string(),
            password_changed_ms: now,
            totp: None,
            totp_setup: None,
            recovery_salt: hex::encode(*random::<16>()?),
            recovery: Vec::new(),
        };
        let cars = CarAccess::from_list(cars);
        self.files.save_secrets(id, &secrets)?;
        self.files.save_cars(id, &cars)?;
        let s = Summary {
            id,
            name: name.to_string(),
            key,
            role,
            two_step: false,
            ui_pack: None,
            cars: cars.count().map(|n| n as u64),
            created_ms: now,
        };
        let mut idx = self.index_mut();
        if idx.by_key.contains_key(&s.key) {
            // the same name was added meanwhile
            drop(idx);
            self.files.delete(id)?;
            return Err(Error::NameTaken);
        }
        idx.put(&self.vault, s.clone())?;
        Ok(s)
    }

    // ---- signing in

    /// One password check at a time per account name and place: another for the same name waits for it (without
    /// taking a hash slot), then [`Error::Busy`]. So one account (or someone typing its name) holds at most one of
    /// the hash slots, however many tries it sends at once, and the others stay free for everyone else.
    fn one_at_a_time(&self, key: &str) -> Result<CheckingPass<'_>> {
        let names = lock(&self.checking.names);
        let (mut names, timeout) = self
            .checking
            .freed
            .wait_timeout_while(names, ONE_AT_A_TIME_WAIT, |n| n.contains(key))
            .unwrap_or_else(|e| e.into_inner());
        if timeout.timed_out() && names.contains(key) {
            return Err(Error::Busy);
        }
        names.insert(key.to_string());
        Ok(CheckingPass { checking: &self.checking, key: key.to_string() })
    }

    /// Refused while this name and place has had [`RIGHT_PASSWORDS_PER_MINUTE`] right passwords this minute (a right
    /// password clears the failure count, so this is what keeps a loop of them from filling the hash slots).
    fn right_check(&self, key: &str, now: u64) -> Result<()> {
        match lock(&self.right).get(key) {
            Some(&(minute, n)) if minute == now / 60_000 && n >= RIGHT_PASSWORDS_PER_MINUTE => {
                Err(Error::Throttled { retry_after_s: (60_000 - now % 60_000).div_ceil(1000).max(1) })
            }
            _ => Ok(()),
        }
    }

    fn right_noted(&self, key: &str, now: u64) {
        let minute = now / 60_000;
        let mut m = lock(&self.right);
        let e = m.entry(key.to_string()).or_insert((minute, 0));
        if e.0 != minute {
            *e = (minute, 0);
        }
        e.1 = e.1.saturating_add(1);
    }

    /// Checks a name and password. Unknown names get exactly the answer a wrong password gets, after the same work.
    /// So does a right password from where the two-step rules don't let that account in (it can't be used there,
    /// and the answer mustn't confirm a guess).
    pub fn sign_in(&self, name: &str, password: &str, client: &Client) -> Result<SignIn> {
        let key = name_key(name);
        let keys = self.keys(&key, client);
        let now = self.now();
        self.throttle.check(&keys, now)?;
        self.right_check(&keys[1], now)?;
        let one = self.one_at_a_time(&keys[1])?;
        let pass = self.hasher.gate.enter()?;
        self.throttle.begin(&keys, self.now())?;
        let found = self.index().find(&key).cloned();
        let (mut phc, mut two_step) = (None, false);
        let ok = match &found {
            // whether a code is needed comes from the account's own file (with its hash), never only from the index
            Some(s) => match self
                .with_account(s.id, |l| Ok((Zeroizing::new(l.secrets.password.clone()), l.secrets.totp.is_some())))
            {
                Ok((h, on)) => {
                    let ok = self.hasher.verify(&h, password);
                    (phc, two_step) = (Some(h), on);
                    ok
                }
                Err(Error::NoSuchAccount) => self.hasher.verify_dummy(password),
                Err(e) => return Err(e),
            },
            None => self.hasher.verify_dummy(password),
        };
        drop(pass);
        let s = match (found, phc) {
            // as it is now that the account's file has been read (see `repair_two_step_flag`)
            (Some(s), Some(phc)) if ok => (self.summary(s.id).unwrap_or(s), phc),
            (Some(s), _) => {
                self.record(Some(&s.name), Some(client), "sign-in-failed", "wrong password")?;
                return Err(Error::WrongCredentials);
            }
            (None, _) => {
                self.record(None, Some(client), "sign-in-failed", "no such account")?;
                return Err(Error::WrongCredentials);
            }
        };
        let (s, mut phc) = s;
        let setup_only = if two_step || s.two_step {
            false
        } else {
            match self.two_step_gate(&s, client) {
                Ok(v) => v,
                Err(e) => {
                    // counted as a failed try (it was counted when it began) and answered as a wrong password
                    self.record(Some(&s.name), Some(client), "sign-in-refused", "two-step sign-in is required here")?;
                    return Err(if matches!(e, Error::TwoStepRequired) { Error::WrongCredentials } else { e });
                }
            }
        };
        if self.hasher.outdated(&phc) {
            // made again with today's settings, now that the password is known to be right
            let fresh = {
                let _pass = self.hasher.gate.enter()?;
                Zeroizing::new(self.hasher.hash(password)?)
            };
            // only over the hash that was checked: a password changed meanwhile stays changed
            let changed = self.change_secrets(s.id, |sec| {
                if !bool::from(sec.password.as_bytes().ct_eq(phc.as_bytes())) {
                    return Ok(false);
                }
                sec.password = fresh.to_string();
                Ok(true)
            })?;
            if !changed {
                return Err(self.password_changed_meanwhile(&s, client));
            }
            phc = fresh;
        }
        drop(one);
        let checked = stamp(&phc);
        if two_step || s.two_step {
            // the password was right; the code has its own count (see `code_keys`)
            let (pending, expires_ms) = match self.start_pending(&s, checked)? {
                Some(p) => p,
                None => return Err(self.password_changed_meanwhile(&s, client)),
            };
            self.throttle.success(&keys);
            self.right_noted(&keys[1], now);
            return Ok(SignIn::NeedCode { pending, expires_ms });
        }
        let Some(ns) = self.new_session_checked(&s, client, checked)? else {
            return Err(self.password_changed_meanwhile(&s, client));
        };
        self.throttle.success(&keys);
        self.right_noted(&keys[1], now);
        let details = if setup_only { "only to set up two-step sign-in" } else { "" };
        self.record(Some(&s.name), Some(client), "sign-in", details)?;
        Ok(SignIn::Done(Box::new(ns)))
    }

    /// A sign-in whose password was right when checked but has been changed since: no session, the answer a wrong
    /// password gets.
    fn password_changed_meanwhile(&self, s: &Summary, client: &Client) -> Error {
        if let Err(e) = self.record(Some(&s.name), Some(client), "sign-in-failed", "the password changed meanwhile") {
            return e;
        }
        Error::WrongCredentials
    }

    /// For an account without two-step sign-in: refused when the rules require it here, else whether only its setup
    /// is open.
    fn two_step_gate(&self, s: &Summary, client: &Client) -> Result<bool> {
        if s.two_step {
            return Ok(false);
        }
        let p = self.two_step_policy();
        if (p.everyone || p.outside) && !client.local {
            return Err(Error::TwoStepRequired);
        }
        Ok(p.everyone)
    }

    /// A sign-in waiting for its code, made under the account's lock only while its password hash is still the one
    /// checked (`None`: it changed meanwhile). A password change takes that lock to write the new hash and drops
    /// the waiting sign-ins after: one made before is dropped, one after sees the new hash.
    fn start_pending(&self, s: &Summary, checked: Stamp) -> Result<Option<(Zeroizing<String>, u64)>> {
        let raw = random::<32>()?;
        let token = Zeroizing::new(format!("smp_{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(*raw)));
        let h = pending_hash(&token).ok_or_else(|| error::corrupt("a new code token has the wrong shape"))?;
        let now = self.now();
        let until_ms = now + PENDING_MS;
        self.with_account(s.id, |l| {
            if stamp(&l.secrets.password) != checked {
                return Ok(None);
            }
            let mut p = lock(&self.pending);
            if p.len() >= MAX_PENDING {
                p.retain(|_, v| v.until_ms > now);
            }
            if p.len() >= MAX_PENDING
                && let Some(oldest) = p.iter().min_by_key(|(_, v)| v.until_ms).map(|(k, _)| *k)
            {
                p.remove(&oldest);
            }
            p.insert(h, Pending { account: s.id, key: s.key.clone(), until_ms, tries: 0, stamp: checked });
            Ok(Some((token, until_ms)))
        })
    }

    /// The second step: the authenticator code (or a recovery code) for a sign-in whose password was right.
    pub fn finish_sign_in(&self, pending: &str, code: &str, client: &Client) -> Result<NewSession> {
        let now = self.now();
        let h = pending_hash(pending).ok_or(Error::SignInExpired)?;
        let (account, key, checked) = match lock(&self.pending).get(&h) {
            Some(p) if p.until_ms > now => (p.account, p.key.clone(), p.stamp),
            _ => return Err(Error::SignInExpired),
        };
        let keys = self.code_keys(&key, client);
        self.throttle.begin(&keys, now)?;
        let s = self.summary(account).ok_or(Error::SignInExpired)?;
        match self.use_code(&s, code, now)? {
            Some(used) => {
                lock(&self.pending).remove(&h);
                self.throttle.success(&keys);
                // the password that let it this far may have changed since
                let Some(ns) = self.new_session_checked(&s, client, checked)? else {
                    self.record(Some(&s.name), Some(client), "sign-in-failed", "the password changed meanwhile")?;
                    return Err(Error::SignInExpired);
                };
                let how = match used {
                    CodeUsed::Totp => "with a two-step code".to_string(),
                    CodeUsed::Recovery { left } => format!("with a recovery code ({left} left)"),
                };
                self.record(Some(&s.name), Some(client), "sign-in", &how)?;
                Ok(ns)
            }
            None => {
                let mut p = lock(&self.pending);
                let over = match p.get_mut(&h) {
                    Some(e) => {
                        e.tries += 1;
                        e.tries >= PENDING_TRIES
                    }
                    None => true,
                };
                if over {
                    p.remove(&h);
                }
                drop(p);
                self.record(Some(&s.name), Some(client), "two-step-failed", "wrong two-step code")?;
                Err(if over { Error::SignInExpired } else { Error::WrongCode })
            }
        }
    }

    /// Checks a two-step or recovery code for an account and uses it up. `None`: not right (or used already).
    fn use_code(&self, s: &Summary, code: &str, now: u64) -> Result<Option<CodeUsed>> {
        // a wrong code writes nothing
        self.with_account(s.id, |l| {
            let mut next = l.secrets.clone();
            let used = Self::take_code(&mut next, code, now)?;
            if used.is_some() {
                self.files.save_secrets(s.id, &next)?;
                l.secrets = next;
            }
            Ok(used)
        })
    }

    fn take_code(sec: &mut Secrets, code: &str, now: u64) -> Result<Option<CodeUsed>> {
        {
            let Some(t) = sec.totp.as_mut() else { return Ok(None) };
            let secret = Zeroizing::new(totp::base32_decode(&t.secret).ok_or_else(|| error::corrupt("totp secret"))?);
            if let Some(step) = totp::matching_step(&secret, code, now) {
                // a step is used once; one before the last used is never taken either
                if step <= t.last_step {
                    return Ok(None);
                }
                t.last_step = step;
                return Ok(Some(CodeUsed::Totp));
            }
            let Some(n) = totp::normalize_recovery(code) else { return Ok(None) };
            let salt = hex::decode(&sec.recovery_salt).map_err(error::corrupt)?;
            let h = totp::hash_recovery(&salt, &n);
            match sec.recovery.iter().position(|x| bool::from(x.as_bytes().ct_eq(h.as_bytes()))) {
                Some(i) => {
                    sec.recovery.remove(i);
                    Ok(Some(CodeUsed::Recovery { left: sec.recovery.len() }))
                }
                None => Ok(None),
            }
        }
    }

    fn new_session(&self, s: &Summary, client: &Client) -> Result<NewSession> {
        self.new_session_at(s, client, self.now())
    }

    fn new_session_at(&self, s: &Summary, client: &Client, now: u64) -> Result<NewSession> {
        let (token, _, session) = self.sessions.create(s.id, client.address, &client.device, now)?;
        let csrf = csrf_token(&token);
        Ok(NewSession { token, csrf, session, account: s.view() })
    }

    /// A session for a sign-in whose password was checked against `checked`, made under the account's lock only
    /// while that is still its hash (`None`: the password changed meanwhile). A password change writes the new hash
    /// under that lock and ends the account's sessions after it, so a session is either made before (and ended) or
    /// refused: none made with the old password outlives the change.
    fn new_session_checked(&self, s: &Summary, client: &Client, checked: Stamp) -> Result<Option<NewSession>> {
        let now = self.now();
        self.with_account(s.id, |l| {
            if stamp(&l.secrets.password) != checked {
                return Ok(None);
            }
            self.new_session_at(s, client, now).map(Some)
        })
    }

    /// The signed-in account of a request's cookie token. Notes the session as used (written in batches), ends it
    /// when the rules no longer let it in from where it is, and reads the account's role and cars afresh.
    pub fn authenticate(&self, token: &str, client: &Client) -> Result<SignedIn> {
        let now = self.now();
        let (h, session) = self.sessions.touch(token, client.address, now)?;
        let Some(s) = self.summary(session.account) else {
            self.sessions.end(&h)?;
            return Err(Error::NotSignedIn);
        };
        let policy = self.two_step_policy();
        if !s.two_step && (policy.everyone || policy.outside) && !client.local {
            self.sessions.end(&h)?;
            self.record(
                Some(&s.name),
                Some(client),
                "signed-out",
                "two-step sign-in is required outside the home network",
            )?;
            return Err(Error::TwoStepRequired);
        }
        let cars = self.with_account(s.id, |l| Ok(l.cars.clone()))?;
        Ok(SignedIn {
            account: s.view(),
            session,
            cars,
            two_step_setup_only: policy.everyone && !s.two_step,
            csrf: csrf_token(token),
            client: client.clone(),
            hash: h,
        })
    }

    /// Signs a cookie token's session out. Whether there was one.
    pub fn sign_out(&self, token: &str, client: &Client) -> Result<bool> {
        let Some(h) = sessions::token_hash(token) else { return Ok(false) };
        let Some(account) = self.sessions.end(&h)? else { return Ok(false) };
        let name = self.summary(account).map(|s| s.name);
        self.record(name.as_deref(), Some(client), "sign-out", "")?;
        Ok(true)
    }

    // ---- sessions

    /// May `by` look after `target`'s sessions and two-step sign-in: its own, or anyone's for an admin.
    fn own_or_admin(&self, by: &SignedIn, target: AccountId) -> Result<()> {
        if by.account.id == target || by.may(Permission::ManageAccounts) { Ok(()) } else { Err(Error::NotAllowed) }
    }

    /// An account's signed-in devices, most recently used first.
    pub fn sessions(&self, by: &SignedIn, target: AccountId) -> Result<Vec<SessionView>> {
        self.own_or_admin(by, target)?;
        Ok(self.sessions.list(target, self.now()))
    }

    /// Signs out one of an account's devices (by its listed id). Whether it was signed in.
    pub fn revoke_session(&self, by: &SignedIn, target: AccountId, session_id: &str) -> Result<bool> {
        self.own_or_admin(by, target)?;
        let ended = self.sessions.end_by_id(target, session_id)?;
        if ended {
            let name = self.summary(target).map(|s| s.name).unwrap_or_default();
            self.record(Some(&by.account.name), Some(&by.client), "session-revoked", &format!("a device of {name}"))?;
        }
        Ok(ended)
    }

    /// Signs out all of an account's devices (but this one, with `keep_current`). How many were signed out.
    pub fn revoke_sessions(&self, by: &SignedIn, target: AccountId, keep_current: bool) -> Result<usize> {
        self.own_or_admin(by, target)?;
        let keep = (keep_current && target == by.account.id).then_some(&by.hash);
        let n = self.sessions.end_account(target, keep)?;
        let name = self.summary(target).map(|s| s.name).unwrap_or_default();
        self.record(Some(&by.account.name), Some(&by.client), "sessions-revoked", &format!("{n} of {name}"))?;
        Ok(n)
    }

    // ---- asking for the password again

    /// Checks the acting account's own password again (throttled like a sign-in), and its two-step code when that
    /// is on (used up; once per request, see [`CodeOnce`]); with `admin`, it must be an admin whose two-step setup
    /// isn't pending.
    fn confirm_actor(&self, by: &SignedIn, confirm: Confirm<'_>, admin: bool) -> Result<Summary> {
        let s = self.summary(by.account.id).ok_or(Error::NotSignedIn)?;
        if admin && (s.role != Role::Admin || by.two_step_setup_only) {
            return Err(Error::NotAllowed);
        }
        let keys = self.keys(&s.key, &by.client);
        self.throttle.check(&keys, self.now())?;
        // one at a time per account (no limit per minute: an admin's script may make many accounts)
        let one = self.one_at_a_time(&keys[1])?;
        let pass = self.hasher.gate.enter()?;
        self.throttle.begin(&keys, self.now())?;
        let (phc, two_step) =
            self.with_account(s.id, |l| Ok((Zeroizing::new(l.secrets.password.clone()), l.secrets.totp.is_some())))?;
        let ok = self.hasher.verify(&phc, confirm.password);
        drop(pass);
        drop(one);
        if !ok {
            self.record(Some(&s.name), Some(&by.client), "confirm-failed", "wrong password given again")?;
            return Err(Error::WrongPassword);
        }
        self.throttle.success(&keys);
        let given = confirm.once.is_some_and(|o| o.0.load(Ordering::SeqCst));
        if (two_step || s.two_step) && !given {
            let keys = self.code_keys(&s.key, &by.client);
            self.throttle.begin(&keys, self.now())?;
            let used = match confirm.code {
                Some(c) => self.use_code(&s, c, self.now())?,
                None => None,
            };
            if used.is_none() {
                self.record(Some(&s.name), Some(&by.client), "confirm-failed", "wrong two-step code")?;
                return Err(Error::WrongCode);
            }
            self.throttle.success(&keys);
            if let Some(o) = confirm.once {
                o.0.store(true, Ordering::SeqCst);
            }
        }
        Ok(s)
    }

    /// Asks for the password again (and the two-step code, when it is on) before something sensitive elsewhere
    /// (the vault's recovery key, interfaces for everyone).
    pub fn confirm(&self, by: &SignedIn, password: &str, code: Option<&str>) -> Result<()> {
        self.confirm_actor(by, Confirm::new(password, code), false).map(|_| ())
    }

    // ---- accounts

    pub fn account(&self, id: AccountId) -> Option<AccountView> {
        self.index().by_id.get(&id).map(Summary::view)
    }

    /// Accounts in name order, `limit` at a time, after the name `after` (for the next page).
    pub fn list(&self, after: Option<&str>, limit: usize) -> Vec<AccountView> {
        let idx = self.index();
        let start = after.map(name_key);
        let range = match &start {
            Some(k) => idx.by_key.range::<String, _>((std::ops::Bound::Excluded(k), std::ops::Bound::Unbounded)),
            None => idx.by_key.range::<String, _>(..),
        };
        range.take(limit).filter_map(|(_, id)| idx.by_id.get(id).map(Summary::view)).collect()
    }

    pub fn count(&self) -> usize {
        self.index().by_id.len()
    }

    /// Adds an account (admins; their password again).
    pub fn create_account<'a>(
        &self,
        by: &SignedIn,
        confirm: impl Into<Confirm<'a>>,
        new: NewAccount<'_>,
    ) -> Result<AccountView> {
        let actor = self.confirm_actor(by, confirm.into(), true)?;
        let name = valid_name(new.name)?;
        check_password(&name, new.password)?;
        if self.index().by_key.contains_key(&name_key(&name)) {
            return Err(Error::NameTaken);
        }
        let phc = {
            let _pass = self.hasher.gate.enter()?;
            Zeroizing::new(self.hasher.hash(new.password)?)
        };
        let s = self.insert(&name, &phc, new.role, new.cars)?;
        let details = format!("{} ({}, {})", s.name, role_name(s.role), cars_words(s.cars));
        self.record(Some(&actor.name), Some(&by.client), "account-created", &details)?;
        Ok(s.view())
    }

    /// Removes an account and signs it out everywhere (admins; their password again). Never the last admin.
    pub fn remove_account<'a>(&self, by: &SignedIn, confirm: impl Into<Confirm<'a>>, target: AccountId) -> Result<()> {
        let actor = self.confirm_actor(by, confirm.into(), true)?;
        let slot = self.slot(target);
        let mut st = lock(&slot.state);
        let mut idx = self.index_mut();
        let s = idx.by_id.get(&target).cloned().ok_or(Error::NoSuchAccount)?;
        if s.role == Role::Admin && idx.admins <= 1 {
            return Err(Error::LastAdmin);
        }
        idx.remove(&self.vault, target)?;
        drop(idx);
        *st = SlotState::Gone;
        self.files.delete(target)?;
        drop(st);
        lock(&self.cache).forget(target);
        self.sessions.end_account(target, None)?;
        lock(&self.pending).retain(|_, p| p.account != target);
        self.record(Some(&actor.name), Some(&by.client), "account-removed", &s.name)?;
        Ok(())
    }

    /// Changes an account's role (admins; their password again). Never demotes the last admin.
    pub fn set_role<'a>(
        &self,
        by: &SignedIn,
        confirm: impl Into<Confirm<'a>>,
        target: AccountId,
        role: Role,
    ) -> Result<()> {
        let actor = self.confirm_actor(by, confirm.into(), true)?;
        let mut idx = self.index_mut();
        let mut s = idx.by_id.get(&target).cloned().ok_or(Error::NoSuchAccount)?;
        if s.role == role {
            return Ok(());
        }
        if s.role == Role::Admin && idx.admins <= 1 {
            return Err(Error::LastAdmin);
        }
        let details = format!("{}: {} to {}", s.name, role_name(s.role), role_name(role));
        s.role = role;
        idx.put(&self.vault, s)?;
        drop(idx);
        self.record(Some(&actor.name), Some(&by.client), "role-changed", &details)
    }

    /// Sets a new password: one's own (with the current password), or anyone's for an admin (with the admin's own).
    /// The account's other sessions are signed out; returns how many.
    pub fn set_password<'a>(
        &self,
        by: &SignedIn,
        confirm: impl Into<Confirm<'a>>,
        target: AccountId,
        new_password: &str,
    ) -> Result<usize> {
        let own = target == by.account.id;
        let actor = self.confirm_actor(by, confirm.into(), !own)?;
        let s = self.summary(target).ok_or(Error::NoSuchAccount)?;
        check_password(&s.name, new_password)?;
        let phc = {
            let _pass = self.hasher.gate.enter()?;
            self.hasher.hash(new_password)?
        };
        let now = self.now();
        self.change_secrets(target, |sec| {
            sec.password = phc;
            sec.password_changed_ms = now;
            Ok(())
        })?;
        // a sign-in waiting for its two-step code was let through by the old password: it ends too
        lock(&self.pending).retain(|_, p| p.account != target);
        let keep = own.then_some(&by.hash);
        let n = self.sessions.end_account(target, keep)?;
        let details = format!("{} ({n} other sessions signed out)", s.name);
        self.record(Some(&actor.name), Some(&by.client), "password-changed", &details)?;
        Ok(n)
    }

    /// Limits an account to some cars (`None`: all cars) (admins; their password again).
    pub fn set_cars<'a>(
        &self,
        by: &SignedIn,
        confirm: impl Into<Confirm<'a>>,
        target: AccountId,
        cars: Option<Vec<CarId>>,
    ) -> Result<()> {
        let actor = self.confirm_actor(by, confirm.into(), true)?;
        let access = CarAccess::from_list(cars);
        self.change_cars(&actor, by, target, |_| Ok(access))
    }

    /// Adds and removes cars from an account's list (admins; their password again), without sending the whole list.
    /// An account that sees all cars gets a list with [`Accounts::set_cars`] first.
    pub fn update_cars<'a>(
        &self,
        by: &SignedIn,
        confirm: impl Into<Confirm<'a>>,
        target: AccountId,
        add: &[CarId],
        remove: &[CarId],
    ) -> Result<()> {
        let actor = self.confirm_actor(by, confirm.into(), true)?;
        self.change_cars(&actor, by, target, |cars| match cars {
            CarAccess::All => Err(Error::BadInput("This account sees every car: give it a list of cars first.")),
            CarAccess::Only(set) => {
                let mut set = (**set).clone();
                set.extend(add.iter().copied());
                for c in remove {
                    set.remove(c);
                }
                Ok(CarAccess::Only(Arc::new(set)))
            }
        })
    }

    /// Works out, writes and takes in an account's new cars, and its count in the index, all under the account's
    /// lock (two changes at once can't lose one, or leave the count wrong).
    fn change_cars(
        &self,
        actor: &Summary,
        by: &SignedIn,
        target: AccountId,
        f: impl FnOnce(&CarAccess) -> Result<CarAccess>,
    ) -> Result<()> {
        let details = self.with_account(target, |l| {
            let next = f(&l.cars)?;
            let count = next.count().map(|n| n as u64);
            self.files.save_cars(target, &next)?;
            l.cars = next;
            let mut idx = self.index_mut();
            let Some(mut s) = idx.by_id.get(&target).cloned() else { return Err(Error::NoSuchAccount) };
            s.cars = count;
            let details = format!("{}: {}", s.name, cars_words(count));
            idx.put(&self.vault, s)?;
            Ok(details)
        })?;
        self.record(Some(&actor.name), Some(&by.client), "cars-changed", &details)
    }

    /// Which cars an account sees.
    pub fn car_access(&self, id: AccountId) -> Result<CarAccess> {
        self.with_account(id, |l| Ok(l.cars.clone()))
    }

    /// Chooses the interface (UI pack) for one's own account (`None`: the one chosen for everyone).
    pub fn set_ui_pack(&self, by: &SignedIn, pack: Option<&str>) -> Result<()> {
        let pack = pack.map(valid_pack).transpose()?;
        let mut idx = self.index_mut();
        let Some(mut s) = idx.by_id.get(&by.account.id).cloned() else { return Err(Error::NotSignedIn) };
        if s.ui_pack == pack {
            return Ok(());
        }
        s.ui_pack = pack.clone();
        let name = s.name.clone();
        idx.put(&self.vault, s)?;
        drop(idx);
        let details = pack.unwrap_or_else(|| "the one for everyone".into());
        self.record(Some(&name), Some(&by.client), "ui-pack-chosen", &details)
    }

    // ---- two-step sign-in

    /// Starts setting up two-step sign-in for one's own account: the secret for the authenticator app. It is on once
    /// [`Accounts::confirm_two_step`] gets a code from the app.
    pub fn start_two_step(&self, by: &SignedIn) -> Result<TwoStepSetup> {
        let s = self.summary(by.account.id).ok_or(Error::NotSignedIn)?;
        if s.two_step {
            return Err(Error::TwoStepOn);
        }
        let secret = Zeroizing::new(totp::base32_encode(&*random::<{ totp::SECRET_BYTES }>()?));
        let now = self.now();
        self.change_secrets(s.id, |sec| {
            sec.totp_setup = Some(TotpSetup { secret: secret.to_string(), started_ms: now });
            Ok(())
        })?;
        let uri = Zeroizing::new(totp::otpauth_uri(&self.issuer, &s.name, &secret));
        Ok(TwoStepSetup { secret, uri })
    }

    /// Turns two-step sign-in on with the first code from the app (and the password again); returns the recovery
    /// codes, shown once.
    pub fn confirm_two_step(&self, by: &SignedIn, password: &str, code: &str) -> Result<RecoveryCodes> {
        let s = self.confirm_actor(by, password.into(), false)?;
        if s.two_step {
            return Err(Error::TwoStepOn);
        }
        let keys = self.code_keys(&s.key, &by.client);
        self.throttle.begin(&keys, self.now())?;
        let now = self.now();
        let salt = random::<16>()?;
        let (shown, kept) = new_recovery(&*salt)?;
        let r = self.change_secrets_and(
            s.id,
            |sec| {
                let setup = sec.totp_setup.clone().ok_or(Error::TwoStepSetupExpired)?;
                if now > setup.started_ms + TWO_STEP_SETUP_MS {
                    return Err(Error::TwoStepSetupExpired);
                }
                let secret =
                    Zeroizing::new(totp::base32_decode(&setup.secret).ok_or_else(|| error::corrupt("totp secret"))?);
                let step = totp::matching_step(&secret, code, now).ok_or(Error::WrongCode)?;
                sec.totp = Some(TotpState { secret: setup.secret.clone(), last_step: step, since_ms: now });
                sec.totp_setup = None;
                sec.recovery_salt = hex::encode(*salt);
                sec.recovery = kept;
                Ok(())
            },
            |_| self.set_two_step_flag(s.id, true),
        );
        if let Err(e) = r {
            if matches!(e, Error::WrongCode) {
                self.record(Some(&s.name), Some(&by.client), "two-step-failed", "wrong code while setting it up")?;
            }
            return Err(e);
        }
        self.throttle.success(&keys);
        self.record(Some(&s.name), Some(&by.client), "two-step-on", "")?;
        Ok(shown)
    }

    fn set_two_step_flag(&self, id: AccountId, on: bool) -> Result<()> {
        let mut idx = self.index_mut();
        let Some(mut s) = idx.by_id.get(&id).cloned() else { return Err(Error::NoSuchAccount) };
        s.two_step = on;
        idx.put(&self.vault, s)
    }

    /// Turns two-step sign-in off: one's own (password again), or anyone's for an admin (e.g. a lost phone). One's
    /// own stays on while the rules require it.
    pub fn disable_two_step<'a>(
        &self,
        by: &SignedIn,
        confirm: impl Into<Confirm<'a>>,
        target: AccountId,
    ) -> Result<()> {
        let own = target == by.account.id;
        let actor = self.confirm_actor(by, confirm.into(), !own)?;
        let s = self.summary(target).ok_or(Error::NoSuchAccount)?;
        if !s.two_step {
            return Err(Error::TwoStepOff);
        }
        let p = self.two_step_policy();
        if own && (p.everyone || (p.outside && !by.client.local)) {
            return Err(Error::TwoStepKept);
        }
        self.change_secrets_and(
            target,
            |sec| {
                sec.totp = None;
                sec.totp_setup = None;
                sec.recovery.clear();
                Ok(())
            },
            |_| self.set_two_step_flag(target, false),
        )?;
        self.record(Some(&actor.name), Some(&by.client), "two-step-off", &s.name)
    }

    /// New recovery codes for one's own account (the old ones stop working), shown once.
    pub fn new_recovery_codes<'a>(&self, by: &SignedIn, confirm: impl Into<Confirm<'a>>) -> Result<RecoveryCodes> {
        let s = self.confirm_actor(by, confirm.into(), false)?;
        if !s.two_step {
            return Err(Error::TwoStepOff);
        }
        let salt = random::<16>()?;
        let (shown, kept) = new_recovery(&*salt)?;
        self.change_secrets(s.id, |sec| {
            sec.recovery_salt = hex::encode(*salt);
            sec.recovery = kept;
            Ok(())
        })?;
        self.record(Some(&s.name), Some(&by.client), "recovery-codes-new", "")?;
        Ok(shown)
    }

    /// Recovery codes left on one's own account.
    pub fn recovery_codes_left(&self, by: &SignedIn) -> Result<usize> {
        self.with_account(by.account.id, |l| Ok(l.secrets.recovery.len()))
    }

    pub fn two_step_policy(&self) -> TwoStepPolicy {
        *self.policy.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Changes when two-step sign-in is required (admins; their password again). The acting admin needs it on
    /// first, so the rule can't lock them out.
    pub fn set_two_step_policy<'a>(
        &self,
        by: &SignedIn,
        confirm: impl Into<Confirm<'a>>,
        policy: TwoStepPolicy,
    ) -> Result<()> {
        let actor = self.confirm_actor(by, confirm.into(), true)?;
        if !actor.two_step && (policy.everyone || (policy.outside && !by.client.local)) {
            return Err(Error::TwoStepFirst);
        }
        let mut p = self.policy.write().unwrap_or_else(|e| e.into_inner());
        let plain = serde_json::to_vec(&policy).map_err(error::corrupt)?;
        self.vault.write_file(&self.dir.join("policy.sealed"), "accounts/policy", &plain)?;
        *p = policy;
        drop(p);
        let onoff = |b: bool| if b { "on" } else { "off" };
        let details = format!("for everyone: {}, from outside: {}", onoff(policy.everyone), onoff(policy.outside));
        self.record(Some(&actor.name), Some(&by.client), "two-step-policy", &details)
    }

    // ---- upkeep

    /// Writes the gathered "last seen" times and forgets what has ended (sessions, waiting sign-ins, old throttle
    /// entries). Call it every minute or so; requests also write "last seen" by themselves at most once a minute.
    pub fn maintain(&self) -> Result<()> {
        let now = self.now();
        self.sessions.flush()?;
        self.sessions.sweep(now);
        self.throttle.sweep(now);
        lock(&self.pending).retain(|_, p| p.until_ms > now);
        lock(&self.right).retain(|_, (minute, _)| *minute == now / 60_000);
        Ok(())
    }

    /// Accounts loaded right now (bounded by [`Options::cached_accounts`], except for those in use).
    pub fn cached_accounts(&self) -> usize {
        lock(&self.cache).len()
    }

    /// Live sessions, all accounts together.
    pub fn live_sessions(&self) -> usize {
        self.sessions.live()
    }

    /// Addresses and names the throttle remembers now.
    pub fn throttle_entries(&self) -> usize {
        self.throttle.tracked()
    }
}
