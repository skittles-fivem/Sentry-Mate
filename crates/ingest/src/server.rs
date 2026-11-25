//! Serving the upload API: TLS 1.3 only, HTTP/1.1, with connection guards: a TLS handshake within 10 s, headers
//! within 10 s, a request at most 10 minutes.
//!
//! Connections are counted per address, an IPv4 address or an IPv6 /64 ([`address_key`], as the web port and the
//! wrong-token window count them). A connection counts as anonymous until a request on it shows a valid token (as
//! soon as the token is checked, before its body is read, so an upload in flight is never anonymous). The rule:
//!
//! - From one address at most [`MAX_PER_ADDRESS`] (32) connections, anonymous or not: the CAN Bridge uploads up to
//!   16 logs at once, each on a new connection, and keeps finished ones open a while (keep-alive), so 16 plus as
//!   many again for the connections it is about to drop and the ping and status requests alongside.
//! - Of them at most [`MAX_ANONYMOUS_PER_ADDRESS`] (6) anonymous ones, unless the address is a car's: a connection
//!   with a valid token is open from it, or closed less than [`CAR_MEMORY`] (30 minutes) ago. A car's new
//!   connections start anonymous too (each proves nothing until its request's token is checked), so on a car's address
//!   they may use all 32. A stranger never shows a token, so its address never becomes a car's and it gets 6. (A
//!   stranger behind the same address as a car, such as a mobile network's shared address, gets the car's 32: still
//!   bounded, and the cap below closes its oldest anonymous connections first.)
//! - At most [`MAX_CONNECTIONS`] (512) in all, of them at most [`MAX_ANONYMOUS`] (192) anonymous: a new one beyond
//!   closes the oldest anonymous one, so connections nobody holds a token for can't fill the port and keep a car out,
//!   and 320 stay for cars' own (ten bridges at 32).
//!
//! A refused connection is not dropped silently: its TLS handshake is finished and its request answered
//! `503 too_many_connections` with `Retry-After: 5` as soon as its headers are in, without reading its body (an
//! `Expect: 100-continue` client is told before it sends any; another one's body is read for at most [`LINGER`], up
//! to [`LINGER_BYTES`], after the answer, so the answer is not lost to a reset). A refusal takes at most
//! [`REFUSAL_TIMEOUT`] in all. At most [`MAX_REFUSING`] are answered at once, [`MAX_REFUSING_PER_ADDRESS`] from one
//! address: a new one beyond closes an earlier refusal instead, the address's own oldest if it has that many, else the
//! oldest of the address with the most, so no address can fill them and keep another's answer out (a warning is
//! logged). The first refusal per address per minute is logged with the address's open counts.

use crate::api::{Conn, EXPORTER_LABEL, Exporter, TokenShown};
use axum::Router;
use axum::response::IntoResponse;
use hyper_util::rt::{TokioIo, TokioTimer};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tower::ServiceExt;

/// Connections from one address, at most, anonymous or not: the bridge's 16 uploads at once plus keep-alive slack.
pub const MAX_PER_ADDRESS: usize = 32;
/// Uploads the CAN Bridge app should send at once (`maxParallel` of `GET /api/can/logs/ping`): half of
/// [`MAX_PER_ADDRESS`], so each upload's connection has a spare one for keep-alive (a connection the app still holds
/// open while it opens the next).
pub const MAX_PARALLEL_UPLOADS: usize = MAX_PER_ADDRESS / 2;
const _: () = assert!(MAX_PARALLEL_UPLOADS == 16 && MAX_PARALLEL_UPLOADS * 2 <= MAX_PER_ADDRESS);
/// Anonymous connections from one address that is not a car's, at most.
pub const MAX_ANONYMOUS_PER_ADDRESS: usize = 6;
/// How long an address counts as a car's after its last connection with a valid token.
pub const CAR_MEMORY: Duration = Duration::from_secs(30 * 60);
pub const MAX_CONNECTIONS: usize = 512;
/// Connections no request has shown a valid token on yet, at most (the rest of [`MAX_CONNECTIONS`] stays for
/// uploads).
pub const MAX_ANONYMOUS: usize = 192;
/// Refused connections answered `503` at once, at most, in all and from one address.
pub const MAX_REFUSING: usize = 32;
pub const MAX_REFUSING_PER_ADDRESS: usize = 16;
/// How long answering a refused connection may take in all: the handshake, the request's headers, the answer and
/// the linger after it.
pub const REFUSAL_TIMEOUT: Duration = Duration::from_secs(15);
/// After a refusal's answer is sent (and the connection closed for writing), how long, and how much, of what the
/// client still sends is read and thrown away, so the client can read the answer before the connection is reset.
pub const LINGER: Duration = Duration::from_secs(2);
pub const LINGER_BYTES: usize = 1 << 20;
/// The `Retry-After` of a `503 too_many_connections`, in seconds.
pub const RETRY_AFTER: u64 = 5;
const LOG_EVERY: Duration = Duration::from_secs(60);
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The server's certificate (`cert.pem`, public) and its private key (sealed with the vault in `key.sealed`), made on
/// first start: a self-signed certificate the app pins by its SHA-256. To use your own, put `cert.pem` and `key.pem`
/// in the folder: the key is sealed on the next start and the plain `key.pem` removed.
///
/// A new one made while the server runs ([`LiveCert::renew`]) is written beside the old pair first
/// (`next-key.sealed`, then `next-cert.pem`, each whole and synced) and then renamed over it, key first; a start
/// finishes a swap a power cut stopped half way ([`finish_swap`]), so the folder always holds a pair that matches.
pub struct Identity {
    pub chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
}

const KEY_LABEL: &str = "tls/key";
const NEXT_CERT: &str = "next-cert.pem";
const NEXT_KEY: &str = "next-key.sealed";
/// The common name `rcgen` gives the certificates it makes (issuer and subject): how a certificate this server made
/// is told from the owner's own.
const MADE_HERE_CN: &[u8] = b"rcgen self signed cert";

fn other(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

impl Identity {
    /// SHA-256 of the leaf certificate, lowercase hex: what the app's "certificate SHA-256" field takes.
    pub fn fingerprint(&self) -> String {
        self.chain.first().map(|c| hex::encode(Sha256::digest(c.as_ref()))).unwrap_or_default()
    }

    pub fn load_or_create(dir: &Path, names: &[String], vault: &vault::Vault) -> std::io::Result<Identity> {
        let (cert, plain_key, sealed) = (dir.join("cert.pem"), dir.join("key.pem"), dir.join("key.sealed"));
        std::fs::create_dir_all(dir)?;
        finish_swap(dir)?;
        if plain_key.exists() && cert.exists() {
            // the owner's own key (or one from before sealing): sealed now, the plain copy overwritten and removed
            let pem = zeroize::Zeroizing::new(std::fs::read(&plain_key)?);
            vault.write_file(&sealed, KEY_LABEL, &pem).map_err(other)?;
            let _ = std::fs::write(&plain_key, vec![0u8; pem.len()]);
            std::fs::remove_file(&plain_key)?;
        }
        if !cert.exists() || !sealed.exists() {
            let (cert_pem, key_pem) = make(names)?;
            vault.write_file(&sealed, KEY_LABEL, key_pem.as_bytes()).map_err(other)?;
            // whole or not at all, like every file of the data folder (a copy never sees half a certificate)
            vault::write_private(&cert, cert_pem.as_bytes())?;
        }
        let chain = rustls_pemfile::certs(&mut std::io::BufReader::new(std::fs::File::open(&cert)?))
            .collect::<Result<Vec<_>, _>>()?;
        let pem = vault.read_file(&sealed, KEY_LABEL).map_err(other)?.ok_or_else(|| other("no TLS key"))?;
        let key = rustls_pemfile::private_key(&mut &pem[..])?
            .ok_or_else(|| other("the TLS key file holds no private key"))?;
        if chain.is_empty() {
            return Err(other("cert.pem holds no certificate"));
        }
        Ok(Identity { chain, key })
    }

    /// rustls settings: TLS 1.3 only, HTTP/1.1.
    pub fn tls_config(&self) -> Result<Arc<rustls::ServerConfig>, rustls::Error> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut c = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(self.chain.clone(), self.key.clone_key())?;
        c.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(c))
    }

    /// Whether this server made the certificate (self-signed, with the name `rcgen` gives it), not the owner.
    pub fn made_here(&self) -> bool {
        self.chain.first().is_some_and(|c| made_here(c.as_ref()))
    }

    fn certified(&self) -> Result<CertifiedKey, rustls::Error> {
        let provider = rustls::crypto::ring::default_provider();
        let c = CertifiedKey::from_der(self.chain.clone(), self.key.clone_key(), &provider)?;
        Ok(c)
    }
}

/// A name for the certificate, written the way it goes in: a DNS name (letters, digits and hyphens between dots,
/// lower case), an IPv4 address, or an IPv6 address (with or without brackets, compressed). Spaces around it are
/// dropped. `None` for anything else: a name with a space, a wildcard, an empty one.
pub fn cert_name(n: &str) -> Option<String> {
    let n = n.trim().to_ascii_lowercase();
    let bare = n.strip_prefix('[').and_then(|r| r.strip_suffix(']')).unwrap_or(&n);
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return Some(ip.to_string());
    }
    let dns = !n.is_empty()
        && n.len() <= 253
        && n.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !l.starts_with('-')
                && !l.ends_with('-')
        });
    dns.then_some(n)
}

/// `names` as they go into a certificate ([`cert_name`]), each once, in order; and the ones that are not names.
pub fn cert_names(names: &[String]) -> (Vec<String>, Vec<String>) {
    let (mut good, mut bad) = (Vec::new(), Vec::new());
    for n in names {
        match cert_name(n) {
            Some(n) if !good.contains(&n) => good.push(n),
            Some(_) => {}
            None if n.trim().is_empty() => {}
            None => bad.push(n.clone()),
        }
    }
    (good, bad)
}

/// A new self-signed certificate for `localhost` and `names` (the same generator the first start uses): its PEM
/// and its key's PEM. What is not a name ([`cert_name`]) is left out, never written in as a DNS name.
fn make(names: &[String]) -> std::io::Result<(String, zeroize::Zeroizing<String>)> {
    let mut sans = vec!["localhost".to_string()];
    let (good, bad) = cert_names(names);
    if !bad.is_empty() {
        tracing::warn!("left out of the certificate, not host names or addresses: {bad:?}");
    }
    for n in good {
        if !sans.contains(&n) {
            sans.push(n);
        }
    }
    let made = rcgen::generate_simple_self_signed(sans).map_err(other)?;
    Ok((made.cert.pem(), zeroize::Zeroizing::new(made.key_pair.serialize_pem())))
}

fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Finishes (or drops) a swap to a new certificate that a stop or a power cut cut short. `next-cert.pem` is written
/// only once `next-key.sealed` is whole, so with it there the new pair is complete: what is left of it is renamed
/// into place (the key may be in place already). A `next-key.sealed` alone is a new pair not finished: removed,
/// and the old pair stays.
pub fn finish_swap(dir: &Path) -> std::io::Result<()> {
    let (next_cert, next_key) = (dir.join(NEXT_CERT), dir.join(NEXT_KEY));
    if next_cert.exists() {
        if next_key.exists() {
            std::fs::rename(&next_key, dir.join("key.sealed"))?;
        }
        std::fs::rename(&next_cert, dir.join("cert.pem"))?;
        sync_dir(dir);
    } else if next_key.exists() {
        std::fs::remove_file(&next_key)?;
        sync_dir(dir);
    }
    // what a write leaves when it stops before its rename (`<file name>.<writer>.tmp`; `<stem>.tmp` of an older version)
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let left = name.ends_with(".tmp")
                && ["next-cert", "next-key"]
                    .iter()
                    .any(|n| name.strip_prefix(n).is_some_and(|rest| rest.starts_with('.')));
            if left {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

/// One DER element: its tag, its content, the whole element and what follows it.
type Element<'a> = (u8, &'a [u8], &'a [u8], &'a [u8]);

/// The DER element at the start of `b`.
fn der(b: &[u8]) -> Option<Element<'_>> {
    let (&tag, rest) = b.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        (rest[..n].iter().fold(0usize, |a, &x| (a << 8) | usize::from(x)), &rest[n..])
    };
    if rest.len() < len {
        return None;
    }
    let head = b.len() - rest.len();
    Some((tag, &rest[..len], &b[..head + len], &rest[len..]))
}

/// Whether a certificate (DER) is one this server made: its issuer is its subject, and that names `rcgen`'s
/// common name.
fn made_here(cert: &[u8]) -> bool {
    let issuer_and_subject = || -> Option<(&[u8], &[u8])> {
        let (0x30, c, _, _) = der(cert)? else { return None };
        let (0x30, tbs, _, _) = der(c)? else { return None };
        let mut rest = tbs;
        // the version, [0], when there is one
        if rest.first() == Some(&0xa0) {
            rest = der(rest)?.3;
        }
        let (_, _, _, rest) = der(rest)?; // the serial number
        let (_, _, _, rest) = der(rest)?; // the signature algorithm
        let (0x30, _, issuer, rest) = der(rest)? else { return None };
        let (_, _, _, rest) = der(rest)?; // validity
        let (0x30, _, subject, _) = der(rest)? else { return None };
        Some((issuer, subject))
    };
    issuer_and_subject().is_some_and(|(i, s)| i == s && i.windows(MADE_HERE_CN.len()).any(|w| w == MADE_HERE_CN))
}

/// Why no new certificate was made.
#[derive(Debug)]
pub enum RenewError {
    /// The server runs with the owner's own certificate (or one waits in the folder for the next start): not
    /// replaced.
    OwnCertificate,
    Io(std::io::Error),
}

impl std::fmt::Display for RenewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenewError::OwnCertificate => f.write_str("the server runs with its owner's own certificate"),
            RenewError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RenewError {}

/// What a [`LiveCert`] serves now.
struct Live {
    key: Arc<CertifiedKey>,
    sha256: String,
    made_here: bool,
}

/// The certificate both ports serve (their rustls settings from [`LiveCert::tls_config`] ask it on each handshake),
/// swapped in place by [`LiveCert::renew`] without a restart: new connections get the new one, connections open
/// already go on with the one they began with.
pub struct LiveCert {
    dir: PathBuf,
    vault: Arc<vault::Vault>,
    now: RwLock<Live>,
    /// One new certificate at a time.
    making: Mutex<()>,
}

impl std::fmt::Debug for LiveCert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveCert").field("dir", &self.dir).field("sha256", &self.fingerprint()).finish()
    }
}

impl ResolvesServerCert for LiveCert {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.now.read().unwrap_or_else(|e| e.into_inner()).key.clone())
    }
}

impl LiveCert {
    /// Serving `id`, the pair in `dir` (sealed with `vault`).
    pub fn new(id: &Identity, dir: &Path, vault: Arc<vault::Vault>) -> Result<Arc<LiveCert>, rustls::Error> {
        let live = Live { key: Arc::new(id.certified()?), sha256: id.fingerprint(), made_here: id.made_here() };
        Ok(Arc::new(LiveCert { dir: dir.to_path_buf(), vault, now: RwLock::new(live), making: Mutex::new(()) }))
    }

    /// The SHA-256 of the certificate served now, lowercase hex.
    pub fn fingerprint(&self) -> String {
        self.now.read().unwrap_or_else(|e| e.into_inner()).sha256.clone()
    }

    /// Whether the certificate served now is one this server made (not the owner's own).
    pub fn made_here(&self) -> bool {
        self.now.read().unwrap_or_else(|e| e.into_inner()).made_here
    }

    /// rustls settings that serve whatever certificate this holds: TLS 1.3 only, HTTP/1.1.
    pub fn tls_config(self: &Arc<Self>) -> Result<Arc<rustls::ServerConfig>, rustls::Error> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut c = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_cert_resolver(self.clone());
        c.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(c))
    }

    /// Whether [`renew`](Self::renew) refuses: the server runs with its owner's own certificate, or one waits in
    /// the folder for the next start (a `key.pem`).
    pub fn own(&self) -> bool {
        !self.made_here() || self.dir.join("key.pem").exists()
    }

    /// Makes a new self-signed certificate for `localhost` and `names`, seals its key, puts the pair in place
    /// (crash-safe: see [`Identity`]) and serves it from now on. Blocking (file writes). Answers its SHA-256.
    /// Refused when the server runs with the owner's own certificate ([`own`](Self::own)).
    pub fn renew(&self, names: &[String]) -> Result<String, RenewError> {
        let _one = self.making.lock().unwrap_or_else(|e| e.into_inner());
        if self.own() {
            return Err(RenewError::OwnCertificate);
        }
        let dir = &self.dir;
        finish_swap(dir).map_err(RenewError::Io)?;
        let (cert_pem, key_pem) = make(names).map_err(RenewError::Io)?;
        let (next_cert, next_key) = (dir.join(NEXT_CERT), dir.join(NEXT_KEY));
        // the new pair whole beside the old one: the key, then the certificate (which says the pair is complete)
        self.vault.write_file(&next_key, KEY_LABEL, key_pem.as_bytes()).map_err(|e| RenewError::Io(other(e)))?;
        vault::write_private(&next_cert, cert_pem.as_bytes()).map_err(RenewError::Io)?;
        finish_swap(dir).map_err(RenewError::Io)?;
        let id = Identity::load_or_create(dir, &[], &self.vault).map_err(RenewError::Io)?;
        let key = Arc::new(id.certified().map_err(|e| RenewError::Io(other(e)))?);
        let sha256 = id.fingerprint();
        *self.now.write().unwrap_or_else(|e| e.into_inner()) = Live { key, sha256: sha256.clone(), made_here: true };
        Ok(sha256)
    }
}

/// Who a connection counts against: the address (an IPv4-mapped IPv6 address as its IPv4 one), or for IPv6 its /64.
pub fn address_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let s = v6.segments();
                IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
        },
        v4 => v4,
    }
}

/// What is open now from one address.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
struct PerAddress {
    /// Connections, with a token shown or not.
    all: usize,
    /// Of them, those no request has shown a valid token on yet.
    anonymous: usize,
    /// Refused connections being answered `503` now (not in `all`).
    refusing: usize,
}

/// Connections open now, per address and in all, the anonymous ones (oldest first, with what closes each), the
/// addresses that count as a car's, and the refusals being answered.
#[derive(Default)]
struct Slots {
    per: HashMap<IpAddr, PerAddress>,
    total: usize,
    anonymous: BTreeMap<u64, Arc<Notify>>,
    next: u64,
    /// Addresses a connection with a valid token came from, and until when they count as a car's.
    cars: HashMap<IpAddr, Instant>,
    /// Refused connections being answered now (oldest first): from where, and what closes each.
    refusals: BTreeMap<u64, (IpAddr, Arc<Notify>)>,
    /// When a refusal from an address was last logged: answered (`true`) or closed unanswered (`false`).
    logged: HashMap<(IpAddr, bool), Instant>,
}

impl Slots {
    fn tidy(&mut self, key: IpAddr) {
        if self.per.get(&key).is_some_and(|p| p.all == 0 && p.refusing == 0) {
            self.per.remove(&key);
        }
    }

    /// Whether `key` is a car's address: a connection with a valid token is open from it, or one was within
    /// [`CAR_MEMORY`].
    fn car(&self, key: IpAddr, now: Instant) -> bool {
        self.per.get(&key).is_some_and(|p| p.all > p.anonymous) || self.cars.get(&key).is_some_and(|&u| u > now)
    }

    fn remember_car(&mut self, key: IpAddr, now: Instant) {
        if self.cars.len() >= 1024 {
            self.cars.retain(|_, u| *u > now);
        }
        self.cars.insert(key, now + CAR_MEMORY);
    }
}

struct Slot {
    slots: Arc<Mutex<Slots>>,
    key: IpAddr,
    seq: u64,
    authorized: AtomicBool,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let authorized = *self.authorized.get_mut();
        let mut s = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        s.total = s.total.saturating_sub(1);
        s.anonymous.remove(&self.seq);
        if let Some(p) = s.per.get_mut(&self.key) {
            p.all = p.all.saturating_sub(1);
            if !authorized {
                p.anonymous = p.anonymous.saturating_sub(1);
            }
        }
        if authorized {
            // the car's next connections, starting anonymous, still get its allowance
            s.remember_car(self.key, Instant::now());
        }
        s.tidy(self.key);
    }
}

impl Slot {
    /// A request on this connection carried a valid token: no longer anonymous, never closed to make room, and its
    /// address counts as a car's.
    fn authorized(&self) {
        if self.authorized.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut s = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        s.anonymous.remove(&self.seq);
        if let Some(p) = s.per.get_mut(&self.key) {
            p.anonymous = p.anonymous.saturating_sub(1);
        }
        s.remember_car(self.key, Instant::now());
    }
}

/// Which limit refused a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Limit {
    /// [`MAX_PER_ADDRESS`] connections are open from the address.
    PerAddress,
    /// [`MAX_ANONYMOUS_PER_ADDRESS`] anonymous ones are, from an address that is not a car's.
    Anonymous,
    /// [`MAX_CONNECTIONS`] are open in all.
    Total,
}

/// A connection refused: why, and what was open then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Refused {
    key: IpAddr,
    limit: Limit,
    open: PerAddress,
    total: usize,
    car: bool,
}

/// A slot for a new connection from `ip`, and what tells it to close (to make room), or why it is refused.
fn take_slot(slots: &Arc<Mutex<Slots>>, ip: IpAddr) -> Result<(Arc<Slot>, Arc<Notify>), Refused> {
    let key = address_key(ip);
    let now = Instant::now();
    let mut s = slots.lock().unwrap_or_else(|e| e.into_inner());
    let car = s.car(key, now);
    let open = s.per.get(&key).copied().unwrap_or_default();
    let refused = |limit, total| Refused { key, limit, open, total, car };
    if open.all >= MAX_PER_ADDRESS {
        return Err(refused(Limit::PerAddress, s.total));
    }
    if !car && open.anonymous >= MAX_ANONYMOUS_PER_ADDRESS {
        return Err(refused(Limit::Anonymous, s.total));
    }
    if s.anonymous.len() >= MAX_ANONYMOUS
        && let Some((_, oldest)) = s.anonymous.pop_first()
    {
        oldest.notify_one();
    }
    if s.total >= MAX_CONNECTIONS {
        return Err(refused(Limit::Total, s.total));
    }
    s.total += 1;
    let p = s.per.entry(key).or_default();
    p.all += 1;
    p.anonymous += 1;
    let seq = s.next;
    s.next += 1;
    let close = Arc::new(Notify::new());
    s.anonymous.insert(seq, close.clone());
    Ok((Arc::new(Slot { slots: slots.clone(), key, seq, authorized: AtomicBool::new(false) }), close))
}

/// A refused connection being answered `503`.
struct Refusing {
    slots: Arc<Mutex<Slots>>,
    seq: u64,
}

impl Slots {
    /// Forgets the refusal `seq` (closed to make room, or done), if it is still counted.
    fn end_refusal(&mut self, seq: u64) -> Option<(IpAddr, Arc<Notify>)> {
        let (key, close) = self.refusals.remove(&seq)?;
        if let Some(p) = self.per.get_mut(&key) {
            p.refusing = p.refusing.saturating_sub(1);
        }
        self.tidy(key);
        Some((key, close))
    }
}

impl Drop for Refusing {
    fn drop(&mut self) {
        let mut s = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        s.end_refusal(self.seq);
    }
}

/// Room to answer a refused connection from `key`, what tells it to close, and the address of an earlier refusal
/// closed to make room, if one was: the address's own oldest when it has [`MAX_REFUSING_PER_ADDRESS`], else, when
/// [`MAX_REFUSING`] are answered now, the oldest of the address answered most (so one that has fewer always gets in).
fn take_refusal(slots: &Arc<Mutex<Slots>>, key: IpAddr) -> (Refusing, Arc<Notify>, Option<IpAddr>) {
    let mut s = slots.lock().unwrap_or_else(|e| e.into_inner());
    let refusing = |s: &Slots, k: &IpAddr| s.per.get(k).map_or(0, |p| p.refusing);
    let victim = if refusing(&s, &key) >= MAX_REFUSING_PER_ADDRESS {
        s.refusals.iter().find(|(_, (k, _))| *k == key).map(|(&seq, _)| seq)
    } else if s.refusals.len() >= MAX_REFUSING {
        // oldest first, so the first one seen of each address is its oldest
        let mut best: Option<(usize, u64)> = None;
        for (&seq, (k, _)) in &s.refusals {
            let n = refusing(&s, k);
            if best.is_none_or(|(most, _)| n > most) {
                best = Some((n, seq));
            }
        }
        best.map(|(_, seq)| seq)
    } else {
        None
    };
    let closed = victim.and_then(|seq| s.end_refusal(seq)).map(|(k, close)| {
        close.notify_one();
        k
    });
    let seq = s.next;
    s.next += 1;
    let close = Arc::new(Notify::new());
    s.refusals.insert(seq, (key, close.clone()));
    s.per.entry(key).or_default().refusing += 1;
    (Refusing { slots: slots.clone(), seq }, close, closed)
}

/// Whether to log about `key` now: the first time per address per minute, for answered refusals (`true`) and for
/// earlier refusals closed to make room (`false`) each.
fn log_now(slots: &Arc<Mutex<Slots>>, key: IpAddr, answered: bool) -> bool {
    let now = Instant::now();
    let mut s = slots.lock().unwrap_or_else(|e| e.into_inner());
    let k = (key, answered);
    if s.logged.get(&k).is_some_and(|&t| now.duration_since(t) < LOG_EVERY) {
        return false;
    }
    if s.logged.len() >= 1024 {
        s.logged.retain(|_, t| now.duration_since(*t) < LOG_EVERY);
    }
    if s.logged.len() >= 4096 {
        return false;
    }
    s.logged.insert(k, now);
    true
}

/// Logs a refusal at info, the first per address per minute.
fn log_refusal(slots: &Arc<Mutex<Slots>>, r: &Refused) {
    if !log_now(slots, r.key, true) {
        return;
    }
    let why = match r.limit {
        Limit::PerAddress => format!("{MAX_PER_ADDRESS} connections are open from it"),
        Limit::Anonymous => {
            format!("{MAX_ANONYMOUS_PER_ADDRESS} are open from it without a valid token yet and it is not a car's")
        }
        Limit::Total => format!("{MAX_CONNECTIONS} are open in all"),
    };
    let (key, open, total, car) = (r.key, r.open, r.total, r.car);
    let counts = format!(
        "{} open from it ({} without a valid token yet, a car's address: {car}), {total} in all",
        open.all, open.anonymous
    );
    tracing::info!(
        "CAN upload port: refused a connection from {key}: {why}; {counts}; answered 503 too_many_connections \
         (logged once a minute per address)"
    );
}

/// Logs, as a warning and the first per address per minute, that an earlier refusal from `closed` was closed
/// (perhaps before its answer) to make room for one from `key`.
fn log_refusal_closed(slots: &Arc<Mutex<Slots>>, closed: IpAddr, key: IpAddr) {
    if !log_now(slots, closed, false) {
        return;
    }
    let (from_it, all) = {
        let s = slots.lock().unwrap_or_else(|e| e.into_inner());
        (s.per.get(&closed).map_or(0, |p| p.refusing), s.refusals.len())
    };
    tracing::warn!(
        "CAN upload port: closed an earlier refused connection from {closed}, perhaps unanswered, to answer one from \
         {key}: {from_it} refusals from it are being answered, {all} in all (at most {MAX_REFUSING_PER_ADDRESS} from \
         one address, {MAX_REFUSING} in all; logged once a minute per address)"
    );
}

/// Finishes the TLS handshake of a refused connection and answers its request `503 too_many_connections` with
/// `Retry-After` as soon as its headers are in, without reading its body (so a client that sent
/// `Expect: 100-continue` is told before it sends any). Then it closes the connection for writing and reads what
/// the client still sends for at most [`LINGER`], up to [`LINGER_BYTES`], throwing it away: closing on unread bytes
/// would reset the connection, and a reset can lose the answer before the client reads it.
async fn answer_busy(acceptor: tokio_rustls::TlsAcceptor, tcp: tokio::net::TcpStream) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Ok(Ok(stream)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await else {
        return;
    };
    let service = hyper::service::service_fn(|_req: hyper::Request<hyper::body::Incoming>| async move {
        let e = crate::error::ApiError::new(
            503,
            "too_many_connections",
            format!("Too many connections from this address; try again in {RETRY_AFTER} s."),
        )
        .retry_after(RETRY_AFTER);
        Ok::<_, std::convert::Infallible>(e.into_response())
    });
    let mut http = hyper::server::conn::http1::Builder::new();
    http.timer(TokioTimer::new()).header_read_timeout(HEADER_TIMEOUT).keep_alive(false);
    let Ok(parts) = http.serve_connection(TokioIo::new(stream), service).without_shutdown().await else {
        return;
    };
    let mut stream = parts.io.into_inner();
    // the answer is out: close_notify, then the TCP FIN
    if stream.shutdown().await.is_err() {
        return;
    }
    let (mut tcp, _) = stream.into_inner();
    let mut buf = vec![0u8; 16 * 1024];
    let mut left = LINGER_BYTES;
    let _ = tokio::time::timeout(LINGER, async {
        while left > 0 {
            match tcp.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => left = left.saturating_sub(n),
            }
        }
    })
    .await;
}

/// Serves `app` on `listener` over TLS until this future is dropped, which also ends every connection it opened.
pub async fn serve_tls(listener: TcpListener, tls: Arc<rustls::ServerConfig>, app: Router) -> std::io::Result<()> {
    let acceptor = tokio_rustls::TlsAcceptor::from(tls);
    let slots = Arc::new(Mutex::new(Slots::default()));
    let mut conns = tokio::task::JoinSet::new();
    loop {
        while conns.try_join_next().is_some() {}
        let (tcp, peer): (_, SocketAddr) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!("CAN upload port: accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let ip = peer.ip();
        let (slot, close) = match take_slot(&slots, ip) {
            Ok(x) => x,
            Err(r) => {
                let (refusing, close, closed) = take_refusal(&slots, r.key);
                log_refusal(&slots, &r);
                if let Some(c) = closed {
                    log_refusal_closed(&slots, c, r.key);
                }
                let acceptor = acceptor.clone();
                conns.spawn(async move {
                    let _refusing = refusing;
                    tokio::select! {
                        _ = tokio::time::timeout(REFUSAL_TIMEOUT, answer_busy(acceptor, tcp)) => {}
                        () = close.notified() => {}
                    }
                });
                continue;
            }
        };
        let (acceptor, app) = (acceptor.clone(), app.clone());
        let run = async move {
            let Ok(Ok(stream)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await else {
                return;
            };
            let conn = Conn { tls: true, ip };
            // the token of a request on it was checked: the connection is no longer anonymous, from then on (an
            // upload in flight is never closed to make room)
            let shown = TokenShown(Arc::new({
                let slot = slot.clone();
                move || slot.authorized()
            }));
            // taken once after the handshake: what a move's claim binds its proofs to
            let exporter =
                stream.get_ref().1.export_keying_material([0u8; 32], EXPORTER_LABEL, None).ok().map(Exporter);
            let service =
                hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                    req.extensions_mut().insert(conn);
                    req.extensions_mut().insert(shown.clone());
                    if let Some(e) = exporter {
                        req.extensions_mut().insert(e);
                    }
                    let app = app.clone();
                    async move {
                        let req = req.map(axum::body::Body::new);
                        match tokio::time::timeout(REQUEST_TIMEOUT, app.oneshot(req)).await {
                            Ok(r) => r,
                            Err(_) => Ok(crate::error::ApiError::new(408, "timeout", "The request took too long.")
                                .into_response()),
                        }
                    }
                });
            let mut http = hyper::server::conn::http1::Builder::new();
            http.timer(TokioTimer::new()).header_read_timeout(HEADER_TIMEOUT).keep_alive(true);
            let _ = http.serve_connection(TokioIo::new(stream), service).await;
        };
        // closed to make room for a newer anonymous connection: dropping `run` ends it (and frees its slot)
        conns.spawn(async move {
            tokio::select! {
                () = run => {}
                () = close.notified() => {}
            }
        });
    }
}

/// Where the server keeps its TLS files under the data folder.
pub fn tls_dir(data: &Path) -> PathBuf {
    data.join("tls")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_host_names_and_addresses_go_into_a_certificate() {
        assert_eq!(cert_name(" NAS.lan "), Some("nas.lan".into()));
        assert_eq!(cert_name(" 192.0.2.5"), Some("192.0.2.5".into()));
        assert_eq!(cert_name("[FD00::0001]"), Some("fd00::1".into()));
        for bad in ["bad name", "*.lan", "a..b", "-a.lan", "", "nas.lan/x", "[::1"] {
            assert_eq!(cert_name(bad), None, "{bad:?}");
        }
        let names: Vec<String> =
            ["nas.lan", " 192.0.2.5", "bad name", "*.lan", "NAS.lan", " "].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            cert_names(&names),
            (vec!["nas.lan".into(), "192.0.2.5".into()], vec!["bad name".into(), "*.lan".into()])
        );

        // in the certificate: the address as an IP address, nothing of the rest
        let (pem, _) = make(&names).unwrap();
        let der = rustls_pemfile::certs(&mut pem.as_bytes()).next().unwrap().unwrap();
        let has = |b: &[u8]| der.windows(b.len()).any(|w| w == b);
        assert!(has(&[0x87, 4, 192, 0, 2, 5]), "an IP address SAN");
        assert!(has(b"nas.lan") && has(b"localhost"));
        assert!(!has(b"192.0.2.5") && !has(b"bad name") && !has(b"*.lan"));
    }

    #[test]
    fn one_ipv6_64_is_one_address_and_mapped_ipv4_is_its_ipv4() {
        let slots = Arc::new(Mutex::new(Slots::default()));
        let mut held = Vec::new();
        for i in 0..MAX_ANONYMOUS_PER_ADDRESS {
            held.push(take_slot(&slots, ip(&format!("2001:db8:1:2::{:x}", i + 1))).unwrap());
        }
        let r = take_slot(&slots, ip("2001:db8:1:2:ffff::9")).err().expect("the same /64");
        assert_eq!((r.limit, r.open.all, r.open.anonymous, r.car), (Limit::Anonymous, 6, 6, false));
        assert!(take_slot(&slots, ip("2001:db8:1:3::1")).is_ok(), "another /64");
        for _ in 0..MAX_ANONYMOUS_PER_ADDRESS {
            held.push(take_slot(&slots, ip("192.0.2.7")).unwrap());
        }
        assert!(take_slot(&slots, ip("::ffff:192.0.2.7")).is_err(), "the mapped form is the same address");
        held.truncate(MAX_ANONYMOUS_PER_ADDRESS - 1);
        assert!(take_slot(&slots, ip("2001:db8:1:2::abcd")).is_ok(), "a slot freed is free again");
    }

    #[test]
    fn a_cars_address_gets_32_and_a_strangers_6() {
        let slots = Arc::new(Mutex::new(Slots::default()));
        let car = ip("198.51.100.1");
        // before any token: like a stranger
        let mut held: Vec<_> = (0..MAX_ANONYMOUS_PER_ADDRESS).map(|_| take_slot(&slots, car).unwrap()).collect();
        assert_eq!(take_slot(&slots, car).err().map(|r| r.limit), Some(Limit::Anonymous));
        // one request with a valid token: that connection no longer counts as anonymous, and the address is a car's
        held[0].0.authorized();
        assert_eq!(slots.lock().unwrap().per[&car].anonymous, MAX_ANONYMOUS_PER_ADDRESS - 1);
        while held.len() < MAX_PER_ADDRESS {
            held.push(take_slot(&slots, car).expect("a car's new connections get in"));
        }
        let r = take_slot(&slots, car).err().expect("the 33rd is refused");
        assert_eq!((r.limit, r.open.all, r.car), (Limit::PerAddress, MAX_PER_ADDRESS, true));
        // a stranger elsewhere still gets only 6
        let stranger = ip("203.0.113.9");
        let mut other: Vec<_> = (0..MAX_ANONYMOUS_PER_ADDRESS).map(|_| take_slot(&slots, stranger).unwrap()).collect();
        assert_eq!(take_slot(&slots, stranger).err().map(|r| r.limit), Some(Limit::Anonymous));
        other.clear();
        // the authorized connection closed: the address stays a car's for a while
        held.clear();
        assert!(!slots.lock().unwrap().per.contains_key(&car), "nothing open is forgotten");
        let again: Vec<_> = (0..MAX_ANONYMOUS_PER_ADDRESS + 4).map(|_| take_slot(&slots, car).unwrap()).collect();
        drop(again);
        // and then not
        slots.lock().unwrap().cars.insert(car, Instant::now() - Duration::from_secs(1));
        let _again: Vec<_> = (0..MAX_ANONYMOUS_PER_ADDRESS).map(|_| take_slot(&slots, car).unwrap()).collect();
        assert!(take_slot(&slots, car).is_err(), "a car's allowance ends {CAR_MEMORY:?} after its last token");
        let s = slots.lock().unwrap();
        assert_eq!(s.total, MAX_ANONYMOUS_PER_ADDRESS);
        assert_eq!(s.anonymous.len(), MAX_ANONYMOUS_PER_ADDRESS);
    }

    #[tokio::test]
    async fn refusals_answered_at_once_are_bounded_and_no_address_fills_them() {
        let slots = Arc::new(Mutex::new(Slots::default()));
        let (a, b, c) = (ip("203.0.113.1"), ip("203.0.113.2"), ip("198.51.100.200"));
        let told = |n: &Arc<Notify>| {
            let n = n.clone();
            async move { tokio::time::timeout(Duration::from_millis(20), n.notified()).await.is_ok() }
        };
        // one address: at most 16, a 17th closes its own oldest
        let mut held = Vec::new();
        for _ in 0..MAX_REFUSING_PER_ADDRESS {
            let (r, close, closed) = take_refusal(&slots, a);
            assert_eq!(closed, None);
            held.push((r, close));
        }
        let (r, close, closed) = take_refusal(&slots, a);
        assert_eq!(closed, Some(a));
        assert!(told(&held[0].1).await, "its oldest was told to close");
        assert!(!told(&held[1].1).await);
        assert_eq!(slots.lock().unwrap().per[&a].refusing, MAX_REFUSING_PER_ADDRESS);
        held.push((r, close));
        // a second address fills the rest
        while slots.lock().unwrap().refusals.len() < MAX_REFUSING {
            let (r, close, closed) = take_refusal(&slots, b);
            assert_eq!(closed, None);
            held.push((r, close));
        }
        // a third address (a car before its token) still gets answered: one of theirs is closed instead
        let (mine, my_close, closed) = take_refusal(&slots, c);
        assert!(closed == Some(a) || closed == Some(b), "{closed:?}");
        {
            let s = slots.lock().unwrap();
            assert_eq!(s.refusals.len(), MAX_REFUSING);
            assert_eq!(s.per[&c].refusing, 1);
        }
        // more from both of them never close the third's: they close each other's (or their own)
        for i in 0..100 {
            let (r, close, closed) = take_refusal(&slots, if i % 2 == 0 { a } else { b });
            assert!(closed.is_some_and(|k| k != c), "{closed:?}");
            held.push((r, close));
        }
        assert!(!told(&my_close).await);
        {
            let s = slots.lock().unwrap();
            assert_eq!(s.refusals.len(), MAX_REFUSING);
            assert_eq!(s.per[&c].refusing, 1);
        }
        drop(mine);
        held.clear();
        let s = slots.lock().unwrap();
        assert!(s.refusals.is_empty());
        assert!(s.per.is_empty());
    }

    #[tokio::test]
    async fn anonymous_connections_make_room_and_authorized_ones_stay() {
        let slots = Arc::new(Mutex::new(Slots::default()));
        // an uploader that showed its token
        let (bridge, _) = take_slot(&slots, ip("198.51.100.1")).unwrap();
        bridge.authorized();
        // anonymous connections from many addresses fill every slot they may
        let mut anon = Vec::new();
        let mut i = 0u32;
        while anon.len() < MAX_ANONYMOUS {
            let a = format!("203.0.{}.{}", i / 200, i % 200 + 1);
            for _ in 0..MAX_ANONYMOUS_PER_ADDRESS {
                if anon.len() < MAX_ANONYMOUS {
                    anon.push(take_slot(&slots, ip(&a)).unwrap());
                }
            }
            i += 1;
        }
        // one more (a CAN Bridge's new connection) closes the oldest anonymous one, never the authorized one
        let newest = take_slot(&slots, ip("198.51.100.2")).expect("room for a new connection");
        {
            let s = slots.lock().unwrap();
            assert_eq!(s.anonymous.len(), MAX_ANONYMOUS);
            assert!(!s.anonymous.contains_key(&anon[0].0.seq), "the oldest was told to close");
            assert!(!s.anonymous.contains_key(&bridge.seq));
            assert!(s.anonymous.contains_key(&newest.0.seq));
        }
        let told = tokio::time::timeout(Duration::from_secs(1), anon[0].1.notified()).await;
        assert!(told.is_ok(), "the oldest anonymous connection is closed");
    }

    #[tokio::test]
    async fn an_anonymous_flood_from_a_cars_address_never_closes_its_authorized_connections() {
        let slots = Arc::new(Mutex::new(Slots::default()));
        let car = ip("198.51.100.1");
        let mut authed = Vec::new();
        for _ in 0..16 {
            let (s, close) = take_slot(&slots, car).unwrap();
            s.authorized();
            authed.push((s, close));
        }
        // anonymous ones from the same address take the rest of its 32, then are refused
        let mut anon = Vec::new();
        while let Ok(x) = take_slot(&slots, car) {
            anon.push(x);
        }
        assert_eq!(anon.len(), MAX_PER_ADDRESS - 16);
        // and anonymous ones from everywhere else fill the port: only anonymous ones are told to close
        let mut i = 0u32;
        let mut more = Vec::new();
        while more.len() < MAX_ANONYMOUS * 2 {
            let a = ip(&format!("203.0.{}.{}", i / 200, i % 200 + 1));
            for _ in 0..MAX_ANONYMOUS_PER_ADDRESS {
                more.push(take_slot(&slots, a).unwrap());
            }
            i += 1;
        }
        {
            let s = slots.lock().unwrap();
            for (a, _) in &authed {
                assert!(!s.anonymous.contains_key(&a.seq));
            }
            assert_eq!(s.anonymous.len(), MAX_ANONYMOUS);
            assert_eq!(s.per[&car].all, MAX_PER_ADDRESS);
            assert_eq!(s.per[&car].anonymous, MAX_PER_ADDRESS - 16);
        }
        for (_, close) in &authed {
            let told = tokio::time::timeout(Duration::from_millis(10), close.notified()).await;
            assert!(told.is_err(), "an authorized connection was told to close");
        }
    }
}
