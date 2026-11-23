//! The one way this crate talks to the network: HTTPS over TLS 1.3 only (rustls with ring, the public web roots),
//! HTTP/2 (Tesla's sign-in and API hosts refuse anything else since mid 2026), only to the account's own
//! [`Endpoints`], no redirects followed (hyper never follows one; a 3xx comes back as a status), a fixed honest
//! User-Agent, no cookies, timeouts on every step, and every answer read with a size limit. Through the owner's
//! HTTP proxy (a `CONNECT` tunnel) only when one is set.
//!
//! Errors say what failed in a word and never carry a body, a header or a token.

use crate::hosts::{Endpoints, Origin, Roots};
use crate::secret::Secret;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Bytes;
use hyper::client::conn::http2;
use hyper::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue, USER_AGENT as UA};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// Sent with every request: names the program, never the owner, the car or the server.
pub const USER_AGENT: &str = concat!("SentryMate/", env!("CARGO_PKG_VERSION"), " (self-hosted)");

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Largest answers read, bytes.
pub(crate) const CAP_TOKEN: usize = 32 * 1024;
pub(crate) const CAP_LIST: usize = 256 * 1024;
pub(crate) const CAP_DATA: usize = 512 * 1024;
/// A page of the charging history, and an invoice's PDF.
pub(crate) const CAP_HISTORY: usize = 2 * 1024 * 1024;
pub(crate) const CAP_PDF: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    #[error("not one of Tesla's hosts")]
    HostNotAllowed,
    #[error("could not connect")]
    Connect,
    #[error("the proxy refused the connection")]
    Proxy,
    #[error("the TLS handshake failed")]
    Tls,
    #[error("the server does not speak HTTP/2")]
    NoHttp2,
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
            HttpError::NoHttp2 => "no_http2",
            HttpError::Timeout => "timeout",
            HttpError::TooLarge => "too_large",
            HttpError::Protocol => "protocol",
        }
    }
}

/// The owner's HTTP proxy (`http://host:port`), when the server's network needs one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proxy {
    host: String,
    port: u16,
}

impl Proxy {
    /// `http://host:port` (no user name or password, no path).
    pub fn parse(s: &str) -> Option<Proxy> {
        let rest = s.trim().strip_prefix("http://")?.trim_end_matches('/');
        let (host, port) = rest.rsplit_once(':')?;
        let ok = !host.is_empty() && host.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
        let port = port.parse::<u16>().ok().filter(|p| *p > 0)?;
        ok.then(|| Proxy { host: host.to_string(), port })
    }

    /// Where the proxy listens.
    pub fn addr(&self) -> (&str, u16) {
        (&self.host, self.port)
    }
}

/// One answer: the status, how long Tesla asked us to wait (429), and the body.
pub(crate) struct Answer {
    pub status: u16,
    pub retry_after_s: Option<u64>,
    #[allow(dead_code)]
    pub content_type: Option<String>,
    pub body: Bytes,
}

/// What a request carries.
pub(crate) enum Body<'a> {
    None,
    /// `application/x-www-form-urlencoded`.
    Form(&'a str),
}

/// A client for one account: its own connections, its own counters.
pub struct Http {
    endpoints: Endpoints,
    h2: Arc<ClientConfig>,
    h1: Arc<ClientConfig>,
    proxy: Option<Proxy>,
    conns: tokio::sync::Mutex<HashMap<String, http2::SendRequest<Full<Bytes>>>>,
    requests: AtomicU64,
}

fn tls_config(roots: &Roots, alpn: &[u8]) -> Result<ClientConfig, HttpError> {
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
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| HttpError::Tls)?
        .with_root_certificates(store)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![alpn.to_vec()];
    Ok(cfg)
}

impl Http {
    pub fn new(endpoints: Endpoints, proxy: Option<Proxy>) -> Result<Http, HttpError> {
        Ok(Http {
            h2: Arc::new(tls_config(&endpoints.roots, b"h2")?),
            h1: Arc::new(tls_config(&endpoints.roots, b"http/1.1")?),
            endpoints,
            proxy,
            conns: tokio::sync::Mutex::new(HashMap::new()),
            requests: AtomicU64::new(0),
        })
    }

    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    /// Requests sent since this client was made (token requests included).
    pub fn requests(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    async fn tcp(&self, origin: &Origin) -> Result<TcpStream, HttpError> {
        let (host, port) = match &self.proxy {
            Some(p) => (p.host.as_str(), p.port),
            None => (origin.host.as_str(), origin.port),
        };
        let mut tcp = timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
            .await
            .map_err(|_| HttpError::Timeout)?
            .map_err(|_| HttpError::Connect)?;
        let _ = tcp.set_nodelay(true);
        if self.proxy.is_some() {
            timeout(CONNECT_TIMEOUT, tunnel(&mut tcp, origin)).await.map_err(|_| HttpError::Timeout)??;
        }
        Ok(tcp)
    }

    /// A TLS connection to one of the account's servers: ALPN `h2` for the REST API, `http/1.1` for the websocket.
    pub(crate) async fn tls(&self, origin: &Origin, h2: bool) -> Result<TlsStream<TcpStream>, HttpError> {
        if !self.endpoints.allows(origin) {
            return Err(HttpError::HostNotAllowed);
        }
        let tcp = self.tcp(origin).await?;
        let name = ServerName::try_from(origin.host.clone()).map_err(|_| HttpError::Tls)?;
        let cfg = if h2 { self.h2.clone() } else { self.h1.clone() };
        let tls = timeout(CONNECT_TIMEOUT, TlsConnector::from(cfg).connect(name, tcp))
            .await
            .map_err(|_| HttpError::Timeout)?
            .map_err(|_| HttpError::Tls)?;
        if h2 && tls.get_ref().1.alpn_protocol() != Some(b"h2".as_slice()) {
            return Err(HttpError::NoHttp2);
        }
        Ok(tls)
    }

    async fn sender(&self, origin: &Origin) -> Result<http2::SendRequest<Full<Bytes>>, HttpError> {
        let key = origin.authority();
        {
            let mut map = self.conns.lock().await;
            if let Some(s) = map.get(&key) {
                if !s.is_closed() {
                    return Ok(s.clone());
                }
                map.remove(&key);
            }
        }
        let tls = self.tls(origin, true).await?;
        let (send, conn) = timeout(CONNECT_TIMEOUT, http2::handshake(TokioExecutor::new(), TokioIo::new(tls)))
            .await
            .map_err(|_| HttpError::Timeout)?
            .map_err(|_| HttpError::Protocol)?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        self.conns.lock().await.insert(key, send.clone());
        Ok(send)
    }

    /// One request to `origin` (`path` starts with `/`), its answer read up to `cap` bytes.
    pub(crate) async fn request(
        &self,
        origin: &Origin,
        post: bool,
        path: &str,
        bearer: Option<&Secret>,
        body: Body<'_>,
        cap: usize,
    ) -> Result<Answer, HttpError> {
        self.request_accept(origin, post, path, bearer, body, cap, "application/json").await
    }

    /// [`Http::request`] asking for another type (an invoice's PDF).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn request_accept(
        &self,
        origin: &Origin,
        post: bool,
        path: &str,
        bearer: Option<&Secret>,
        body: Body<'_>,
        cap: usize,
        accept: &'static str,
    ) -> Result<Answer, HttpError> {
        let wait = if cap > CAP_DATA { REQUEST_TIMEOUT * 3 } else { REQUEST_TIMEOUT };
        timeout(wait, self.send(origin, post, path, bearer, body, cap, accept)).await.map_err(|_| HttpError::Timeout)?
    }

    #[allow(clippy::too_many_arguments)]
    async fn send(
        &self,
        origin: &Origin,
        post: bool,
        path: &str,
        bearer: Option<&Secret>,
        body: Body<'_>,
        cap: usize,
        accept: &'static str,
    ) -> Result<Answer, HttpError> {
        if !path.starts_with('/') {
            return Err(HttpError::HostNotAllowed);
        }
        let mut send = self.sender(origin).await?;
        if send.ready().await.is_err() {
            self.conns.lock().await.remove(&origin.authority());
            send = self.sender(origin).await?;
            send.ready().await.map_err(|_| HttpError::Protocol)?;
        }
        let mut req = hyper::Request::builder()
            .method(if post { "POST" } else { "GET" })
            .uri(format!("{}{}", origin.https(), path))
            .header(UA, USER_AGENT)
            .header(ACCEPT, accept);
        if let Some(t) = bearer {
            let mut v = HeaderValue::from_str(&format!("Bearer {}", t.expose())).map_err(|_| HttpError::Protocol)?;
            v.set_sensitive(true);
            req = req.header(AUTHORIZATION, v);
        }
        let payload = match body {
            Body::None => Full::new(Bytes::new()),
            Body::Form(f) => {
                req = req.header(CONTENT_TYPE, "application/x-www-form-urlencoded");
                Full::new(Bytes::copy_from_slice(f.as_bytes()))
            }
        };
        let req = req.body(payload).map_err(|_| HttpError::Protocol)?;
        self.requests.fetch_add(1, Ordering::Relaxed);
        let resp = send.send_request(req).await.map_err(|_| HttpError::Protocol)?;
        let status = resp.status().as_u16();
        let wait = |name: &str| {
            resp.headers().get(name).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok())
        };
        let retry_after_s = wait("retry-after").or_else(|| wait("ratelimit-reset"));
        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or_default().trim().to_ascii_lowercase());
        let body = Limited::new(resp.into_body(), cap).collect().await.map_err(|e| {
            if e.downcast_ref::<LengthLimitError>().is_some() { HttpError::TooLarge } else { HttpError::Protocol }
        })?;
        tracing::debug!(host = %origin.host, status, "tesla request");
        Ok(Answer { status, retry_after_s, content_type, body: body.to_bytes() })
    }
}

/// Asks the proxy for a tunnel to `origin` (`CONNECT`); the TLS handshake then runs through it end to end. (Also the
/// TeslaFi input's way through the owner's proxy.)
pub async fn tunnel(tcp: &mut TcpStream, origin: &Origin) -> Result<(), HttpError> {
    let target = format!("{}:{}", origin.host, origin.port);
    let req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nUser-Agent: {USER_AGENT}\r\n\r\n");
    tcp.write_all(req.as_bytes()).await.map_err(|_| HttpError::Proxy)?;
    let mut got = Vec::with_capacity(256);
    let mut buf = [0u8; 512];
    while !got.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = tcp.read(&mut buf).await.map_err(|_| HttpError::Proxy)?;
        if n == 0 || got.len() + n > 8192 {
            return Err(HttpError::Proxy);
        }
        got.extend_from_slice(&buf[..n]);
    }
    let line = got.split(|b| *b == b'\r').next().unwrap_or_default();
    let ok = line.starts_with(b"HTTP/1.") && line.get(8..13) == Some(b" 200 ".as_slice())
        || line.get(8..) == Some(b" 200".as_slice());
    if ok { Ok(()) } else { Err(HttpError::Proxy) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_addresses() {
        assert_eq!(Proxy::parse("http://proxy.lan:3128"), Some(Proxy { host: "proxy.lan".into(), port: 3128 }));
        assert_eq!(Proxy::parse("https://proxy.lan:3128"), None);
        assert_eq!(Proxy::parse("http://user:pw@proxy.lan:3128"), None);
        assert_eq!(Proxy::parse("http://proxy.lan"), None);
    }

    #[test]
    fn user_agent_is_plain() {
        assert!(USER_AGENT.starts_with("SentryMate/"));
    }
}
