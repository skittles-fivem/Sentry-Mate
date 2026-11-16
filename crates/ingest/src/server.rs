//! Serving the upload API: TLS 1.3 only, HTTP/1.1, with Sentry Six Manager's connection guards (`docs/can-bridge-api.md`,
//! "TLS"): headers within 20 s, a request at most 10 minutes, at most 6 connections per address and 256 in all, a TLS
//! handshake within 15 s.

use crate::api::Conn;
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

/// The server's certificate and key (PEM files in `dir`), made on first start: a self-signed certificate the app
/// pins by its SHA-256, or the owner's own (put `cert.pem` and `key.pem` there).
pub struct Identity {
    pub chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
}

impl Identity {
    /// SHA-256 of the leaf certificate, lowercase hex: what the app's "certificate SHA-256" field takes.
    pub fn fingerprint(&self) -> String {
        self.chain
            .first()
            .map(|c| hex::encode(Sha256::digest(c.as_ref())))
            .unwrap_or_default()
    }

    pub fn load_or_create(dir: &Path, names: &[String]) -> std::io::Result<Identity> {
        let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
        if !cert.exists() || !key.exists() {
            std::fs::create_dir_all(dir)?;
            let mut sans = vec!["localhost".to_string()];
            sans.extend(names.iter().filter(|n| !n.is_empty()).cloned());
            let made = rcgen::generate_simple_self_signed(sans).map_err(std::io::Error::other)?;
            write_private(&key, made.key_pair.serialize_pem().as_bytes())?;
            std::fs::write(&cert, made.cert.pem())?;
        }
        let chain =
            rustls_pemfile::certs(&mut std::io::BufReader::new(std::fs::File::open(&cert)?))
                .collect::<Result<Vec<_>, _>>()?;
        let key =
            rustls_pemfile::private_key(&mut std::io::BufReader::new(std::fs::File::open(&key)?))?
                .ok_or_else(|| std::io::Error::other("key.pem holds no private key"))?;
        if chain.is_empty() {
            return Err(std::io::Error::other("cert.pem holds no certificate"));
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

/// A file only its owner can read.
fn write_private(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(file)?
    };
    #[cfg(not(unix))]
    let mut f = std::fs::File::create(file)?;
    f.write_all(bytes)?;
    f.sync_all()
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

/// Serves `app` on `listener` over TLS until the task is dropped.
pub async fn serve_tls(
    listener: TcpListener,
    tls: Arc<rustls::ServerConfig>,
    app: Router,
) -> std::io::Result<()> {
    let acceptor = tokio_rustls::TlsAcceptor::from(tls);
    let slots = Arc::new(Mutex::new(Slots::default()));
    loop {
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
        tokio::spawn(async move {
            let _slot = slot;
            let Ok(Ok(stream)) =
                tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await
            else {
                return;
            };
            let conn = Conn { tls: true, ip };
            let service = hyper::service::service_fn(
                move |mut req: hyper::Request<hyper::body::Incoming>| {
                    req.extensions_mut().insert(conn);
                    let app = app.clone();
                    async move {
                        let req = req.map(axum::body::Body::new);
                        match tokio::time::timeout(REQUEST_TIMEOUT, app.oneshot(req)).await {
                            Ok(r) => r,
                            Err(_) => Ok(crate::error::ApiError::new(
                                408,
                                "timeout",
                                "The request took too long.",
                            )
                            .into_response()),
                        }
                    }
                },
            );
            let mut http = hyper::server::conn::http1::Builder::new();
            http.timer(TokioTimer::new())
                .header_read_timeout(HEADER_TIMEOUT)
                .keep_alive(true);
            let _ = http.serve_connection(TokioIo::new(stream), service).await;
        });
    }
}

/// Where the server keeps its TLS files under the data folder.
pub fn tls_dir(data: &Path) -> PathBuf {
    data.join("tls")
}
