//! The new server's connection to the old one: TLS 1.3 (rustls), HTTP/1.1, blocking, one request at a time.
//!
//! For the claim it accepts any certificate (TLS still checks that the server holds its certificate's key) and
//! records the certificate's SHA-256 and the connection's exporter value: the claim's proofs are bound to them. From
//! then on it pins the old server's certificate by that SHA-256: any other certificate ends the connection before a
//! byte is sent.

use crate::proto::{EXPORTER_LABEL, same};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use sha2::{Digest, Sha256};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The upload port, when an address gives none.
pub const DEFAULT_PORT: u16 = 10100;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_HEAD: usize = 64 * 1024;

/// Where the old server is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Address {
    pub host: String,
    pub port: u16,
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

impl Address {
    /// Reads what the owner gives: `192.168.1.20`, `nas.local:10100`, `[fd00::1]:10100`, or the bridge's whole upload
    /// address (`https://192.168.1.20:10100/api/can/logs`). Port 10100 unless given.
    pub fn parse(text: &str) -> Result<Address, String> {
        let t = text.trim();
        let t = t.strip_prefix("https://").or_else(|| t.strip_prefix("HTTPS://")).unwrap_or(t);
        if t.starts_with("http://") {
            return Err("the old server's address is https:// (its upload port), not http://".into());
        }
        let t = t.split('/').next().unwrap_or("");
        let bad = || format!("{text:?} isn't a server address: give it like 192.168.1.20 or nas.local:10100");
        let (host, port) = if let Some(rest) = t.strip_prefix('[') {
            let (h, after) = rest.split_once(']').ok_or_else(bad)?;
            let port = match after.strip_prefix(':') {
                Some(p) => p.parse().map_err(|_| bad())?,
                None if after.is_empty() => DEFAULT_PORT,
                None => return Err(bad()),
            };
            (h.to_string(), port)
        } else if t.matches(':').count() > 1 {
            // a bare IPv6 address
            (t.to_string(), DEFAULT_PORT)
        } else if let Some((h, p)) = t.split_once(':') {
            (h.to_string(), p.parse().map_err(|_| bad())?)
        } else {
            (t.to_string(), DEFAULT_PORT)
        };
        if host.is_empty()
            || port == 0
            || !host.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b':' | b'_' | b'%'))
        {
            return Err(bad());
        }
        Ok(Address { host, port })
    }

    fn resolve(&self) -> io::Result<Vec<SocketAddr>> {
        let v: Vec<SocketAddr> = (self.host.as_str(), self.port).to_socket_addrs()?.collect();
        if v.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "the name gives no address"));
        }
        Ok(v)
    }

    /// Whether the address is this machine's own (the old server ran on this machine: same machine, new disk).
    pub fn is_this_machine(&self) -> bool {
        let Ok(addrs) = self.resolve() else { return false };
        addrs.iter().any(|a| a.ip().is_loopback() || std::net::UdpSocket::bind(SocketAddr::new(a.ip(), 0)).is_ok())
    }
}

/// What went wrong on the network.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("can't find {0} ({1})")]
    Resolve(String, io::Error),
    #[error("nothing answers at {0} ({1})")]
    Connect(String, io::Error),
    #[error(
        "the server at {0} has another certificate than the old server proved in the claim: it isn't the old server, or something in between changed the connection"
    )]
    WrongCertificate(String),
    #[error("the TLS connection to {0} failed ({1})")]
    Tls(String, String),
    #[error("lost the connection to the old server ({0})")]
    Io(#[from] io::Error),
    #[error("the old server's answer doesn't read ({0})")]
    Protocol(String),
}

impl NetError {
    fn protocol(why: impl Into<String>) -> NetError {
        NetError::Protocol(why.into())
    }
}

#[derive(Debug)]
struct Verifier {
    pin: Option<[u8; 32]>,
    seen: Arc<Mutex<Option<[u8; 32]>>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let h: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
        if let Ok(mut s) = self.seen.lock() {
            *s = Some(h);
        }
        match &self.pin {
            Some(p) if !same(p, &h) => {
                Err(rustls::Error::InvalidCertificate(rustls::CertificateError::ApplicationVerificationFailure))
            }
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(rustls::PeerIncompatible::Tls12NotOffered))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // the server must hold its certificate's key, whatever the certificate
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

type Tls = StreamOwned<ClientConnection, TcpStream>;

/// A connection to the old server, made again when it drops.
pub struct Client {
    addr: Address,
    pin: Option<[u8; 32]>,
    conn: Option<BufReader<Tls>>,
    /// The certificate SHA-256 and exporter value of the connection now open.
    seen: Option<([u8; 32], [u8; 32])>,
    timeout: Duration,
}

/// An answer: its status, and its body to read (whole, or as a stream for a fetch).
pub struct Response<'a> {
    pub status: u16,
    pub retry_after: Option<u64>,
    body: Body<'a>,
}

enum Body<'a> {
    Length { conn: &'a mut Client, left: u64, close: bool },
    Chunked { conn: &'a mut Client, left: u64, done: bool, close: bool },
}

impl Client {
    /// A client that accepts any certificate (for the claim): [`Client::seen`] says which one it got.
    pub fn any(addr: Address) -> Client {
        Client { addr, pin: None, conn: None, seen: None, timeout: IO_TIMEOUT }
    }

    /// A client that accepts only the certificate with this SHA-256 (hex).
    pub fn pinned(addr: Address, cert_sha256: &str) -> Result<Client, NetError> {
        let pin: [u8; 32] = hex::decode(cert_sha256)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| NetError::protocol("a pinned certificate SHA-256 that doesn't read"))?;
        Ok(Client { addr, pin: Some(pin), conn: None, seen: None, timeout: IO_TIMEOUT })
    }

    pub fn address(&self) -> &Address {
        &self.addr
    }

    /// Waits at most this long for the old server's bytes (default two minutes).
    pub fn set_timeout(&mut self, t: Duration) {
        self.timeout = t;
    }

    /// The open connection's certificate SHA-256 (hex) and exporter value (connects first if needed).
    pub fn seen(&mut self) -> Result<(String, [u8; 32]), NetError> {
        self.connect()?;
        let (c, e) = self.seen.ok_or_else(|| NetError::protocol("no connection"))?;
        Ok((hex::encode(c), e))
    }

    /// Ends the connection (the next request makes a new one).
    pub fn drop_connection(&mut self) {
        self.conn = None;
        self.seen = None;
    }

    fn connect(&mut self) -> Result<(), NetError> {
        if self.conn.is_some() {
            return Ok(());
        }
        let shown = self.addr.to_string();
        let addrs = self.addr.resolve().map_err(|e| NetError::Resolve(self.addr.host.clone(), e))?;
        let mut last = None;
        let mut tcp = None;
        for a in addrs {
            match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
                Ok(t) => {
                    tcp = Some(t);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let Some(mut tcp) = tcp else {
            return Err(NetError::Connect(shown, last.unwrap_or_else(|| io::Error::other("no address"))));
        };
        tcp.set_nodelay(true)?;
        tcp.set_read_timeout(Some(self.timeout))?;
        tcp.set_write_timeout(Some(self.timeout))?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let seen = Arc::new(Mutex::new(None));
        let verifier = Verifier { pin: self.pin, seen: seen.clone(), provider: provider.clone() };
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| NetError::Tls(shown.clone(), e.to_string()))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let name = ServerName::try_from(self.addr.host.clone())
            .or_else(|_| ServerName::try_from("sentry-mate".to_string()))
            .map_err(|e| NetError::Tls(shown.clone(), e.to_string()))?;
        let mut conn =
            ClientConnection::new(Arc::new(config), name).map_err(|e| NetError::Tls(shown.clone(), e.to_string()))?;
        while conn.is_handshaking() {
            if let Err(e) = conn.complete_io(&mut tcp) {
                let got = seen.lock().ok().and_then(|s| *s);
                if let (Some(pin), Some(got)) = (self.pin, got)
                    && !same(&pin, &got)
                {
                    return Err(NetError::WrongCertificate(shown));
                }
                return Err(NetError::Tls(shown, e.to_string()));
            }
        }
        let cert = seen.lock().ok().and_then(|s| *s).ok_or_else(|| NetError::protocol("no certificate"))?;
        let exporter = conn
            .export_keying_material([0u8; 32], EXPORTER_LABEL, None)
            .map_err(|e| NetError::Tls(shown.clone(), e.to_string()))?;
        self.seen = Some((cert, exporter));
        self.conn = Some(BufReader::with_capacity(1 << 16, StreamOwned::new(conn, tcp)));
        Ok(())
    }

    /// Sends one request. A connection the old server closed (keep-alive ended) is made again once.
    pub fn request(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Response<'_>, NetError> {
        for again in [false, true] {
            let fresh = self.conn.is_none();
            self.connect()?;
            match self.send(method, path, headers, body) {
                Ok(()) => {}
                Err(e) => {
                    self.drop_connection();
                    if fresh || again {
                        return Err(e);
                    }
                    continue;
                }
            }
            match self.read_head() {
                Ok(head) => return Ok(self.response(head)),
                Err(e) => {
                    self.drop_connection();
                    // a kept connection the server had closed: once more on a new one
                    if fresh || again || !matches!(&e, NetError::Io(_)) {
                        return Err(e);
                    }
                }
            }
        }
        Err(NetError::protocol("no answer"))
    }

    fn send(&mut self, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Result<(), NetError> {
        let conn = self.conn.as_mut().ok_or_else(|| NetError::protocol("no connection"))?;
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\n", self.addr, body.len());
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        let s = conn.get_mut();
        s.write_all(head.as_bytes())?;
        s.write_all(body)?;
        s.flush()?;
        Ok(())
    }

    fn read_head(&mut self) -> Result<Head, NetError> {
        let conn = self.conn.as_mut().ok_or_else(|| NetError::protocol("no connection"))?;
        let mut line = String::new();
        let mut total = 0usize;
        let mut read_line = |line: &mut String| -> Result<(), NetError> {
            line.clear();
            let n = conn.by_ref().take((MAX_HEAD - total.min(MAX_HEAD)) as u64).read_line(line)?;
            if n == 0 {
                return Err(NetError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "the connection ended")));
            }
            total += n;
            if total >= MAX_HEAD {
                return Err(NetError::protocol("an answer's head too long"));
            }
            Ok(())
        };
        read_line(&mut line)?;
        let status: u16 = line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| NetError::protocol(format!("a status line {:?}", line.trim())))?;
        let mut head = Head { status, length: None, chunked: false, close: false, retry_after: None };
        loop {
            read_line(&mut line)?;
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            let Some((k, v)) = l.split_once(':') else { continue };
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            match k.as_str() {
                "content-length" => head.length = v.parse().ok(),
                "transfer-encoding" => head.chunked = v.to_ascii_lowercase().contains("chunked"),
                "connection" => head.close = v.eq_ignore_ascii_case("close"),
                "retry-after" => head.retry_after = v.parse().ok(),
                _ => {}
            }
        }
        Ok(head)
    }

    fn response(&mut self, h: Head) -> Response<'_> {
        let body = if h.chunked {
            Body::Chunked { conn: self, left: 0, done: false, close: h.close }
        } else {
            Body::Length { conn: self, left: h.length.unwrap_or(0), close: h.close }
        };
        Response { status: h.status, retry_after: h.retry_after, body }
    }
}

struct Head {
    status: u16,
    length: Option<u64>,
    chunked: bool,
    close: bool,
    retry_after: Option<u64>,
}

impl Response<'_> {
    /// The whole body (at most `max` bytes).
    pub fn bytes(mut self, max: usize) -> Result<Vec<u8>, NetError> {
        let mut out = Vec::new();
        let n = self.by_ref().take(max as u64 + 1).read_to_end(&mut out)?;
        if n > max {
            return Err(NetError::protocol("an answer too long"));
        }
        Ok(out)
    }

    /// The body as JSON.
    pub fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, NetError> {
        let b = self.bytes(16 << 20)?;
        serde_json::from_slice(&b).map_err(|e| NetError::protocol(e.to_string()))
    }

    /// An error answer's `code` and the whole body as JSON (null if it isn't).
    pub fn error(self) -> Result<(String, serde_json::Value), NetError> {
        let b = self.bytes(1 << 20)?;
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null);
        Ok((v["code"].as_str().unwrap_or("").to_string(), v))
    }
}

impl Read for Response<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match &mut self.body {
            Body::Length { conn, left, close } => {
                if *left == 0 {
                    if *close {
                        conn.drop_connection();
                    }
                    return Ok(0);
                }
                let Some(c) = conn.conn.as_mut() else { return Err(io::ErrorKind::NotConnected.into()) };
                let want = (*left).min(out.len() as u64) as usize;
                let n = c.read(&mut out[..want])?;
                if n == 0 {
                    conn.drop_connection();
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the answer was cut short"));
                }
                *left -= n as u64;
                Ok(n)
            }
            Body::Chunked { conn, left, done, close } => {
                if *done {
                    return Ok(0);
                }
                let Some(c) = conn.conn.as_mut() else { return Err(io::ErrorKind::NotConnected.into()) };
                if *left == 0 {
                    let mut line = String::new();
                    c.by_ref().take(1024).read_line(&mut line)?;
                    let size = line.trim().split(';').next().unwrap_or("");
                    let n = u64::from_str_radix(size, 16)
                        .map_err(|_| io::Error::other(format!("a chunk size {:?}", line.trim())))?;
                    if n == 0 {
                        // trailers, then the empty line
                        loop {
                            line.clear();
                            let k = c.by_ref().take(8192).read_line(&mut line)?;
                            if k == 0 || line.trim_end().is_empty() {
                                break;
                            }
                        }
                        *done = true;
                        if *close {
                            conn.drop_connection();
                        }
                        return Ok(0);
                    }
                    *left = n;
                }
                let want = (*left).min(out.len() as u64) as usize;
                let n = c.read(&mut out[..want])?;
                if n == 0 {
                    conn.drop_connection();
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the answer was cut short"));
                }
                *left -= n as u64;
                if *left == 0 {
                    let mut crlf = [0u8; 2];
                    c.read_exact(&mut crlf)?;
                }
                Ok(n)
            }
        }
    }
}

impl Drop for Response<'_> {
    fn drop(&mut self) {
        // a body not read to its end leaves the connection in the middle of an answer: it can't be used again
        let unfinished = match &self.body {
            Body::Length { left, .. } => *left > 0,
            Body::Chunked { done, .. } => !*done,
        };
        if unfinished {
            match &mut self.body {
                Body::Length { conn, .. } | Body::Chunked { conn, .. } => conn.drop_connection(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn addresses_read_as_the_owner_gives_them() {
        let a = |s: &str| Address::parse(s).unwrap();
        assert_eq!(a("192.168.1.20"), Address { host: "192.168.1.20".into(), port: 10100 });
        assert_eq!(a(" nas.local:10101 "), Address { host: "nas.local".into(), port: 10101 });
        assert_eq!(a("https://192.168.1.20:10100/api/can/logs"), Address { host: "192.168.1.20".into(), port: 10100 });
        assert_eq!(a("[fd00::1]:10102"), Address { host: "fd00::1".into(), port: 10102 });
        assert_eq!(a("fd00::1"), Address { host: "fd00::1".into(), port: 10100 });
        assert_eq!(a("[fd00::1]").to_string(), "[fd00::1]:10100");
        for bad in ["", "http://192.168.1.20", "nas:port", "a b", "nas:0", "[fd00::1"] {
            assert!(Address::parse(bad).is_err(), "{bad}");
        }
        assert!(a("127.0.0.1:1").is_this_machine());
    }
}
