//! The one way this crate talks to the network: HTTPS with rustls (ring, the public web roots) to GitHub's API host
//! only ([`Endpoint`]), `GET` only, no redirects followed (a 3xx comes back as a status), the fixed User-Agent
//! `sentry-mate`, no token, no cookie, no body, timeouts on every step, and every answer read with a size limit
//! ([`CAP`]). Through the owner's HTTP proxy (a `CONNECT` tunnel) only when one is set.
//!
//! TLS: 1.3 and 1.2 are offered, rustls takes 1.3 whenever the host has it (and refuses a downgrade from it).
//! HTTP/1.1, one connection per request: the check asks twice a day at most.
//!
//! Errors say what failed in a word and never carry a body or a header.

use crate::hosts::{Endpoint, Origin, Roots};
use http_body_util::{BodyExt, Empty, LengthLimitError, Limited};
use hyper::body::Bytes;
use hyper::header::{ACCEPT, HOST, HeaderValue, IF_NONE_MATCH, USER_AGENT as UA};
use hyper_util::rt::TokioIo;
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tesla::Proxy;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;

/// Sent with every request: the program's name, nothing about the owner, a car or the server.
pub const USER_AGENT: &str = "sentry-mate";
pub const ACCEPT_GITHUB: &str = "application/vnd.github+json";
pub const API_VERSION: &str = "2022-11-28";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Largest answer read, bytes (a comparison over many commits is several MB).
pub const CAP: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
    #[error("not GitHub's API host")]
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

/// One answer: the status, the headers the check reads, and the body.
#[derive(Debug, Clone, Default)]
pub struct Answer {
    pub status: u16,
    pub etag: Option<String>,
    /// `x-ratelimit-remaining`.
    pub remaining: Option<u64>,
    /// `x-ratelimit-reset`: s since 1970.
    pub reset_s: Option<u64>,
    /// `retry-after`: s.
    pub retry_after_s: Option<u64>,
    pub body: Bytes,
}

pub type FetchFuture<'a> = Pin<Box<dyn Future<Output = Result<Answer, HttpError>> + Send + 'a>>;

/// What sends a `GET` (`path` starts with `/`, with its query) to GitHub's API: [`Http`], or a stand-in in a test.
pub trait Fetch: Send + Sync {
    fn get<'a>(&'a self, path: &'a str, if_none_match: Option<&'a str>) -> FetchFuture<'a>;
    /// Follows a change of the owner's proxy.
    fn set_proxy(&self, _proxy: Option<Proxy>) {}
}

pub struct Http {
    endpoint: Endpoint,
    cfg: Arc<ClientConfig>,
    proxy: Mutex<Option<Proxy>>,
    requests: AtomicU64,
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
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(cfg)
}

/// A path the check may ask for: `/`, then letters, digits and `/._-?=&`, with no `.` or `..` segment.
fn path_ok(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 512
        && path.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-?=&".contains(&b))
        && path.split('?').next().unwrap_or_default().split('/').all(|seg| seg != "." && seg != "..")
}

fn header_u64(h: &hyper::HeaderMap, name: &str) -> Option<u64> {
    h.get(name).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok())
}

impl Http {
    pub fn new(endpoint: Endpoint, proxy: Option<Proxy>) -> Result<Http, HttpError> {
        Ok(Http {
            cfg: Arc::new(tls_config(&endpoint.roots)?),
            endpoint,
            proxy: Mutex::new(proxy),
            requests: AtomicU64::new(0),
        })
    }

    /// GitHub's own API host, with no proxy.
    pub fn github() -> Result<Http, HttpError> {
        Http::new(Endpoint::github(), None)
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Requests sent since this client was made.
    pub fn requests(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    async fn tcp(&self, origin: &Origin) -> Result<TcpStream, HttpError> {
        let proxy = self.proxy.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let (host, port) = match &proxy {
            Some(p) => {
                let (h, port) = p.addr();
                (h.to_string(), port)
            }
            None => (origin.host.clone(), origin.port),
        };
        let mut tcp = timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port)))
            .await
            .map_err(|_| HttpError::Timeout)?
            .map_err(|_| HttpError::Connect)?;
        let _ = tcp.set_nodelay(true);
        if proxy.is_some() {
            timeout(CONNECT_TIMEOUT, tesla::http::tunnel(&mut tcp, origin))
                .await
                .map_err(|_| HttpError::Timeout)?
                .map_err(|_| HttpError::Proxy)?;
        }
        Ok(tcp)
    }

    async fn send(&self, path: &str, if_none_match: Option<&str>) -> Result<Answer, HttpError> {
        let origin = self.endpoint.origin.clone();
        if !path_ok(path) || !self.endpoint.allows(&origin) {
            return Err(HttpError::HostNotAllowed);
        }
        let tcp = self.tcp(&origin).await?;
        let name = ServerName::try_from(origin.host.clone()).map_err(|_| HttpError::Tls)?;
        let tls = timeout(CONNECT_TIMEOUT, TlsConnector::from(self.cfg.clone()).connect(name, tcp))
            .await
            .map_err(|_| HttpError::Timeout)?
            .map_err(|_| HttpError::Tls)?;
        let (mut send, conn) = timeout(CONNECT_TIMEOUT, hyper::client::conn::http1::handshake(TokioIo::new(tls)))
            .await
            .map_err(|_| HttpError::Timeout)?
            .map_err(|_| HttpError::Protocol)?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let host = HeaderValue::from_str(&origin.authority()).map_err(|_| HttpError::Protocol)?;
        let mut req = hyper::Request::builder()
            .method("GET")
            .uri(path)
            .header(HOST, host)
            .header(UA, USER_AGENT)
            .header(ACCEPT, ACCEPT_GITHUB)
            .header("x-github-api-version", API_VERSION);
        if let Some(tag) = if_none_match {
            let v = HeaderValue::from_str(tag).map_err(|_| HttpError::Protocol)?;
            req = req.header(IF_NONE_MATCH, v);
        }
        let req = req.body(Empty::<Bytes>::new()).map_err(|_| HttpError::Protocol)?;
        self.requests.fetch_add(1, Ordering::Relaxed);
        let resp = send.send_request(req).await.map_err(|_| HttpError::Protocol)?;
        let h = resp.headers();
        let mut a = Answer {
            status: resp.status().as_u16(),
            etag: h.get("etag").and_then(|v| v.to_str().ok()).map(str::to_string).filter(|e| e.len() <= 256),
            remaining: header_u64(h, "x-ratelimit-remaining"),
            reset_s: header_u64(h, "x-ratelimit-reset"),
            retry_after_s: header_u64(h, "retry-after"),
            body: Bytes::new(),
        };
        let body = Limited::new(resp.into_body(), CAP).collect().await.map_err(|e| {
            if e.downcast_ref::<LengthLimitError>().is_some() { HttpError::TooLarge } else { HttpError::Protocol }
        })?;
        a.body = body.to_bytes();
        tracing::debug!(host = %origin.host, status = a.status, "release check request");
        Ok(a)
    }
}

impl Fetch for Http {
    fn get<'a>(&'a self, path: &'a str, if_none_match: Option<&'a str>) -> FetchFuture<'a> {
        Box::pin(async move {
            timeout(REQUEST_TIMEOUT, self.send(path, if_none_match)).await.map_err(|_| HttpError::Timeout)?
        })
    }

    fn set_proxy(&self, proxy: Option<Proxy>) {
        *self.proxy.lock().unwrap_or_else(|p| p.into_inner()) = proxy;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert!(path_ok("/repos/skittles-fivem/Sentry-Mate/actions/workflows/image.yml/runs?branch=main&per_page=1"));
        assert!(path_ok("/repos/skittles-fivem/Sentry-Mate/compare/abc...def"));
        assert!(!path_ok("repos"));
        assert!(!path_ok("/repos/../x"));
        assert!(!path_ok("/a b"));
        assert!(!path_ok("/a\r\nX: y"));
        assert!(!path_ok("/a#b"));
        assert!(!path_ok("/a@b"));
    }
}
