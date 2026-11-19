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
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
pub struct Identity {
    pub chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
}

const KEY_LABEL: &str = "tls/key";

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
        if plain_key.exists() && cert.exists() {
            // the owner's own key (or one from before sealing): sealed now, the plain copy overwritten and removed
            let pem = zeroize::Zeroizing::new(std::fs::read(&plain_key)?);
            vault.write_file(&sealed, KEY_LABEL, &pem).map_err(other)?;
            let _ = std::fs::write(&plain_key, vec![0u8; pem.len()]);
            std::fs::remove_file(&plain_key)?;
        }
        if !cert.exists() || !sealed.exists() {
            let mut sans = vec!["localhost".to_string()];
            sans.extend(names.iter().filter(|n| !n.is_empty()).cloned());
            let made = rcgen::generate_simple_self_signed(sans).map_err(other)?;
            let pem = zeroize::Zeroizing::new(made.key_pair.serialize_pem());
            vault.write_file(&sealed, KEY_LABEL, pem.as_bytes()).map_err(other)?;
            // whole or not at all, like every file of the data folder (a copy never sees half a certificate)
            vault::write_private(&cert, made.cert.pem().as_bytes())?;
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
