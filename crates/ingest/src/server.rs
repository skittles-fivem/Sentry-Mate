//! Serving the upload API: TLS 1.3 only, HTTP/1.1, with connection guards: a TLS handshake within 10 s, headers
//! within 10 s, a request at most 10 minutes.
//!
//! Connections are counted per address, an IPv4 address or an IPv6 /64 ([`address_key`], as the web port and the
//! wrong-token window count them): at most [`MAX_PER_ADDRESS`] each and [`MAX_CONNECTIONS`] in all. A connection
//! counts as anonymous until a request on it is answered with success (it carried a valid token); at most
//! [`MAX_ANONYMOUS`] are, and a new one beyond closes the oldest anonymous one, so connections nobody holds a token
//! for can't fill the port and keep the CAN Bridge out.

use crate::api::{Conn, EXPORTER_LABEL, Exporter};
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
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tower::ServiceExt;

pub const MAX_PER_ADDRESS: usize = 6;
pub const MAX_CONNECTIONS: usize = 256;
/// Connections no request has shown a valid token on yet, at most (the rest of [`MAX_CONNECTIONS`] stays for
/// uploads).
pub const MAX_ANONYMOUS: usize = MAX_CONNECTIONS * 3 / 4;
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

/// A new self-signed certificate for `localhost` and `names` (the same generator the first start uses): its PEM
/// and its key's PEM.
fn make(names: &[String]) -> std::io::Result<(String, zeroize::Zeroizing<String>)> {
    let mut sans = vec!["localhost".to_string()];
    for n in names.iter().filter(|n| !n.is_empty()) {
        if !sans.contains(n) {
            sans.push(n.clone());
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
    // what `write_private` leaves when it stops before its rename
    for t in ["next-cert.tmp", "next-key.tmp"] {
        let _ = std::fs::remove_file(dir.join(t));
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

    /// Makes a new self-signed certificate for `localhost` and `names`, seals its key, puts the pair in place
    /// (crash-safe: see [`Identity`]) and serves it from now on. Blocking (file writes). Answers its SHA-256.
    /// Refused when the server runs with the owner's own certificate.
    pub fn renew(&self, names: &[String]) -> Result<String, RenewError> {
        let _one = self.making.lock().unwrap_or_else(|e| e.into_inner());
        if !self.made_here() || self.dir.join("key.pem").exists() {
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

/// Connections open now, per address and in all, and the anonymous ones (oldest first, with what closes each).
#[derive(Default)]
struct Slots {
    per: HashMap<IpAddr, usize>,
    total: usize,
    anonymous: BTreeMap<u64, Arc<Notify>>,
    next: u64,
}

struct Slot {
    slots: Arc<Mutex<Slots>>,
    key: IpAddr,
    seq: u64,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut s = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        s.total = s.total.saturating_sub(1);
        s.anonymous.remove(&self.seq);
        if let Some(n) = s.per.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                s.per.remove(&self.key);
            }
        }
    }
}

impl Slot {
    /// A request on this connection carried a valid token: no longer anonymous, never closed to make room.
    fn authorized(&self) {
        let mut s = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        s.anonymous.remove(&self.seq);
    }
}

/// A slot for a new connection from `ip`, and what tells it to close (to make room), or `None`: refused.
fn take_slot(slots: &Arc<Mutex<Slots>>, ip: IpAddr) -> Option<(Arc<Slot>, Arc<Notify>)> {
    let key = address_key(ip);
    let mut s = slots.lock().unwrap_or_else(|e| e.into_inner());
    if s.per.get(&key).copied().unwrap_or(0) >= MAX_PER_ADDRESS {
        return None;
    }
    if s.anonymous.len() >= MAX_ANONYMOUS
        && let Some((_, oldest)) = s.anonymous.pop_first()
    {
        oldest.notify_one();
    }
    if s.total >= MAX_CONNECTIONS {
        return None;
    }
    s.total += 1;
    *s.per.entry(key).or_default() += 1;
    let seq = s.next;
    s.next += 1;
    let close = Arc::new(Notify::new());
    s.anonymous.insert(seq, close.clone());
    Some((Arc::new(Slot { slots: slots.clone(), key, seq }), close))
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
        let Some((slot, close)) = take_slot(&slots, ip) else {
            drop(tcp);
            continue;
        };
        let (acceptor, app) = (acceptor.clone(), app.clone());
        let run = async move {
            let Ok(Ok(stream)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await else {
                return;
            };
            let conn = Conn { tls: true, ip };
            // taken once after the handshake: what a move's claim binds its proofs to
            let exporter =
                stream.get_ref().1.export_keying_material([0u8; 32], EXPORTER_LABEL, None).ok().map(Exporter);
            let service =
                hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                    req.extensions_mut().insert(conn);
                    if let Some(e) = exporter {
                        req.extensions_mut().insert(e);
                    }
                    let (app, slot) = (app.clone(), slot.clone());
                    async move {
                        let req = req.map(axum::body::Body::new);
                        match tokio::time::timeout(REQUEST_TIMEOUT, app.oneshot(req)).await {
                            Ok(r) => {
                                // only a request with a valid token is answered with success
                                if r.as_ref().is_ok_and(|r| r.status().is_success()) {
                                    slot.authorized();
                                }
                                r
                            }
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
    fn one_ipv6_64_is_one_address_and_mapped_ipv4_is_its_ipv4() {
        let slots = Arc::new(Mutex::new(Slots::default()));
        let mut held = Vec::new();
        for i in 0..MAX_PER_ADDRESS {
            held.push(take_slot(&slots, ip(&format!("2001:db8:1:2::{:x}", i + 1))).unwrap());
        }
        assert!(take_slot(&slots, ip("2001:db8:1:2:ffff::9")).is_none(), "the same /64");
        assert!(take_slot(&slots, ip("2001:db8:1:3::1")).is_some(), "another /64");
        for _ in 0..MAX_PER_ADDRESS {
            held.push(take_slot(&slots, ip("192.0.2.7")).unwrap());
        }
        assert!(take_slot(&slots, ip("::ffff:192.0.2.7")).is_none(), "the mapped form is the same address");
        held.truncate(MAX_PER_ADDRESS - 1);
        assert!(take_slot(&slots, ip("2001:db8:1:2::abcd")).is_some(), "a slot freed is free again");
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
            for _ in 0..MAX_PER_ADDRESS {
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
}
