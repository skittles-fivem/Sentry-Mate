//! The one way this crate talks to the network: HTTPS with rustls (ring, the public web roots) to TeslaFi's host
//! only ([`Endpoint`]), `GET` only, no redirects followed (hyper never follows one; a 3xx comes back as a status),
//! the token in the `Authorization` header (never in a URL), a fixed honest User-Agent, no cookies, timeouts on every
//! step, and every answer read with a size limit. Through the owner's HTTP proxy (a `CONNECT` tunnel) only when one
//! is set.
//!
//! TLS: 1.3 and 1.2 are offered, rustls takes 1.3 whenever the host has it (and refuses a downgrade from it); 1.2 is
//! only there for this one host, in case TeslaFi's edge stops offering 1.3. [`Http::tls_version`] says which one the
//! last connection used, for the status page. HTTP/2 when the host offers it (`h2`), else HTTP/1.1. A connection
//! is made for each request: the input asks once a minute at most.
//!
//! Errors say what failed in a word and never carry a body, a header or the token.

use crate::hosts::{Endpoint, Origin, Roots};
use http_body_util::{BodyExt, Empty, LengthLimitError, Limited};
use hyper::body::Bytes;
use hyper::header::{ACCEPT, AUTHORIZATION, HOST, HeaderValue, USER_AGENT as UA};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Duration;
use tesla::{Proxy, Secret};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

/// Sent with every request: names the program, never the owner, the car or the server.
pub use tesla::http::USER_AGENT;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Largest answer read, bytes (one poll is a few KB).
pub const CAP: usize = 256 * 1024;

/// Largest charge log read, bytes ([`crate::costs`]: a few KB a charge; a window that holds more is read in smaller
/// windows).
pub const HISTORY_CAP: usize = 8 << 20;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    #[error("not TeslaFi's host")]
    HostNotAllowed,
    #[error("could not connect")]
    Connect,
    #[error("the proxy refused the connection")]
    Proxy,
    #[error("the TLS handshake failed")]
    Tls,
    #[error("timed out")]
    Timeout,
    #[error("the answer was too large")]
    TooLarge,
    #[error("the connection broke")]
    Protocol,
}

impl HttpError {
    pub fn code(&self) -> &'static str {
        match self {
            HttpError::HostNotAllowed => "host_not_allowed",
            HttpError::Connect => "connect",
            HttpError::Proxy => "proxy",
            HttpError::Tls => "tls",
            HttpError::Timeout => "timeout",
            HttpError::TooLarge => "too_large",
            HttpError::Protocol => "protocol",
        }
    }
}

/// One answer: the status, how long TeslaFi asked us to wait (429), and the body.
pub(crate) struct Answer {
    pub status: u16,
    pub retry_after_s: Option<u64>,
    pub body: Bytes,
}

/// The client of one account.
pub struct Http {
    endpoint: Endpoint,
    cfg: Arc<ClientConfig>,
    proxy: Option<Proxy>,
    requests: AtomicU64,
    /// The TLS version of the last connection: 0 none yet, 12 or 13.
    tls: AtomicU8,
}

fn tls_config(roots: &Roots) -> Result<ClientConfig, HttpError> {
    let mut store = rustls::RootCertStore::empty();
    match roots {
        Roots::Public => store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
        #[cfg(feature = "test-endpoints")]
        Roots::Only(certs) => {
            for c in certs.iter() {
                store.add(c.clone()).map_err(|_| HttpError::Tls)?;
            }
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|_| HttpError::Tls)?
        .with_root_certificates(store)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}

impl Http {
    pub fn new(endpoint: Endpoint, proxy: Option<Proxy>) -> Result<Http, HttpError> {
        Ok(Http {
            cfg: Arc::new(tls_config(&endpoint.roots)?),
            endpoint,
            proxy,
            requests: AtomicU64::new(0),
            tls: AtomicU8::new(0),
        })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Requests sent since this client was made.
    pub fn requests(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    /// `1.3` or `1.2`: the TLS version of the last connection (`None` before the first).
    pub fn tls_version(&self) -> Option<&'static str> {
        match self.tls.load(Ordering::Relaxed) {
            13 => Some("1.3"),
            12 => Some("1.2"),
            _ => None,
        }
    }

    async fn tcp(&self, origin: &Origin) -> Result<TcpStream, HttpError> {
        let (host, port) = match &self.proxy {
            Some(p) => p.addr(),
            None => (origin.host.as_str(), origin.port),
        };
        let mut tcp = timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
            .await
            .map_err(|_| HttpError::Timeout)?
            .map_err(|_| HttpError::Connect)?;
        let _ = tcp.set_nodelay(true);
        if self.proxy.is_some() {
            timeout(CONNECT_TIMEOUT, tesla::http::tunnel(&mut tcp, origin))
                .await
                .map_err(|_| HttpError::Timeout)?
                .map_err(|_| HttpError::Proxy)?;
        }
        Ok(tcp)
    }

    /// `GET path` (`path` starts with `/`) with the token, its answer read up to [`CAP`] bytes.
    pub(crate) async fn get(&self, path: &str, token: &Secret) -> Result<Answer, HttpError> {
        self.get_up_to(path, token, CAP).await
    }

    /// [`Http::get`], its answer read up to `cap` bytes.
    pub(crate) async fn get_up_to(&self, path: &str, token: &Secret, cap: usize) -> Result<Answer, HttpError> {
        timeout(REQUEST_TIMEOUT, self.send(path, token, cap)).await.map_err(|_| HttpError::Timeout)?
    }

    async fn send(&self, path: &str, token: &Secret, cap: usize) -> Result<Answer, HttpError> {
        let origin = self.endpoint.origin.clone();
        if !path.starts_with('/') || !self.endpoint.allows(&origin) {
            return Err(HttpError::HostNotAllowed);
        }
        let tcp = self.tcp(&origin).await?;
        let name = ServerName::try_from(origin.host.clone()).map_err(|_| HttpError::Tls)?;
        let tls = timeout(CONNECT_TIMEOUT, TlsConnector::from(self.cfg.clone()).connect(name, tcp))
            .await
            .map_err(|_| HttpError::Timeout)?
            .map_err(|_| HttpError::Tls)?;
        let (h2, version) = {
            let c = tls.get_ref().1;
            (c.alpn_protocol() == Some(b"h2".as_slice()), c.protocol_version())
        };
        let v = match version {
            Some(rustls::ProtocolVersion::TLSv1_3) => 13,
            Some(rustls::ProtocolVersion::TLSv1_2) => 12,
            _ => return Err(HttpError::Tls),
        };
        self.tls.store(v, Ordering::Relaxed);
        let mut auth = HeaderValue::from_str(&format!("Bearer {}", token.expose())).map_err(|_| HttpError::Protocol)?;
        auth.set_sensitive(true);
        let req = hyper::Request::builder()
            .method("GET")
            .header(UA, USER_AGENT)
            .header(ACCEPT, "application/json")
            .header(AUTHORIZATION, auth);
        let io = TokioIo::new(tls);
        self.requests.fetch_add(1, Ordering::Relaxed);
        let resp = if h2 {
            let (mut send, conn) =
                timeout(CONNECT_TIMEOUT, hyper::client::conn::http2::handshake(TokioExecutor::new(), io))
                    .await
                    .map_err(|_| HttpError::Timeout)?
                    .map_err(|_| HttpError::Protocol)?;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            let req = req
                .uri(format!("{}{}", origin.https(), path))
                .body(Empty::<Bytes>::new())
                .map_err(|_| HttpError::Protocol)?;
            send.send_request(req).await.map_err(|_| HttpError::Protocol)?
        } else {
            let (mut send, conn) = timeout(CONNECT_TIMEOUT, hyper::client::conn::http1::handshake(io))
                .await
                .map_err(|_| HttpError::Timeout)?
                .map_err(|_| HttpError::Protocol)?;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            let host = HeaderValue::from_str(&origin.authority()).map_err(|_| HttpError::Protocol)?;
            let req = req.uri(path).header(HOST, host).body(Empty::<Bytes>::new()).map_err(|_| HttpError::Protocol)?;
            send.send_request(req).await.map_err(|_| HttpError::Protocol)?
        };
        let status = resp.status().as_u16();
        let retry_after_s =
            resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
        let body = Limited::new(resp.into_body(), cap).collect().await.map_err(|e| {
            if e.downcast_ref::<LengthLimitError>().is_some() { HttpError::TooLarge } else { HttpError::Protocol }
        })?;
        tracing::debug!(host = %origin.host, status, tls = v, "teslafi request");
        Ok(Answer { status, retry_after_s, body: body.to_bytes() })
    }
}
