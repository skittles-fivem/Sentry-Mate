//! Serving the upload API: TLS 1.3 only, HTTP/1.1, with Sentry Six Manager's connection guards (`docs/can-bridge-api.md`,
//! "TLS"): headers within 20 s, a request at most 10 minutes, at most 6 connections per address and 256 in all, a TLS
//! handshake within 15 s.

use crate::api::{Conn, EXPORTER_LABEL, Exporter};
use axum::Router;
use axum::response::IntoResponse;
use hyper_util::rt::{TokioIo, TokioTimer};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tower::ServiceExt;

pub const MAX_PER_ADDRESS: usize = 6;
pub const MAX_CONNECTIONS: usize = 256;
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(20);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

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

/// Connections open now, per address and in all.
#[derive(Default)]
struct Slots {
    per: HashMap<IpAddr, usize>,
    total: usize,
}

struct Slot(Arc<Mutex<Slots>>, IpAddr);
impl Drop for Slot {
    fn drop(&mut self) {
        if let Ok(mut s) = self.0.lock() {
            s.total = s.total.saturating_sub(1);
            if let Some(n) = s.per.get_mut(&self.1) {
                *n -= 1;
                if *n == 0 {
                    s.per.remove(&self.1);
                }
            }
        }
    }
}

fn take_slot(slots: &Arc<Mutex<Slots>>, ip: IpAddr) -> Option<Slot> {
    let mut s = slots.lock().ok()?;
    if s.total >= MAX_CONNECTIONS || s.per.get(&ip).copied().unwrap_or(0) >= MAX_PER_ADDRESS {
        return None;
    }
    s.total += 1;
    *s.per.entry(ip).or_default() += 1;
    Some(Slot(slots.clone(), ip))
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
        let Some(slot) = take_slot(&slots, ip) else {
            drop(tcp);
            continue;
        };
        let (acceptor, app) = (acceptor.clone(), app.clone());
        conns.spawn(async move {
            let _slot = slot;
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
        });
    }
}

/// Where the server keeps its TLS files under the data folder.
pub fn tls_dir(data: &Path) -> PathBuf {
    data.join("tls")
}
