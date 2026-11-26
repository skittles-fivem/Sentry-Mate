//! `sentry-mate health`, the image's HEALTHCHECK: whether the server answers. The upload port is asked for a
//! connection; the web port for its `/health` page over HTTPS (a route that takes no lock and reads nothing), so a
//! server whose web side has stopped answering reads unhealthy, not just one whose port is closed.
//!
//! The certificate is not checked: the server's own (made here, or the owner's for a name that is not
//! `127.0.0.1`) is whatever it serves, and nothing but the request for `/health` is sent over the connection. The
//! handshake itself is checked as any other (TLS 1.3 only, as the server speaks).

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// How long each port has to answer (the image's HEALTHCHECK gives the whole check 5 s).
pub const WAIT: Duration = Duration::from_secs(2);

/// Takes any certificate; checks the handshake's signature with it.
#[derive(Debug)]
struct AnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // never asked: the client speaks TLS 1.3 only
        Err(rustls::Error::General("TLS 1.2 is not spoken".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Whether something takes a connection on `port` of this machine within `wait`.
pub async fn port_open(port: u16, wait: Duration) -> bool {
    tokio::time::timeout(wait, tokio::net::TcpStream::connect(("127.0.0.1", port))).await.is_ok_and(|r| r.is_ok())
}

/// Whether the web server on `port` of this machine answers `GET /health` with 200 within `wait`.
pub async fn web_answers(port: u16, wait: Duration) -> bool {
    tokio::time::timeout(wait, get_health(port)).await.is_ok_and(|r| r.unwrap_or(false))
}

async fn get_health(port: u16) -> std::io::Result<bool> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(std::io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCert(provider)))
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let name = ServerName::IpAddress(std::net::Ipv4Addr::LOCALHOST.into());
    let mut tls = tokio_rustls::TlsConnector::from(Arc::new(cfg)).connect(name, tcp).await?;
    tls.write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").await?;
    tls.flush().await?;
    // the status line is enough
    let mut head = Vec::with_capacity(64);
    let mut buf = [0u8; 64];
    while !head.contains(&b'\n') && head.len() < 1024 {
        let n = tls.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&buf[..n]);
    }
    let line = head.split(|b| *b == b'\n').next().unwrap_or_default();
    Ok(line.starts_with(b"HTTP/1.1 200 ") || line.starts_with(b"HTTP/1.0 200 "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_port_that_takes_connections_but_never_answers_is_not_healthy() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap_or_else(|e| panic!("{e}"));
        let port = l.local_addr().unwrap_or_else(|e| panic!("{e}")).port();
        // takes every connection and keeps it, saying nothing (as a server whose web side hangs)
        let held = tokio::spawn(async move {
            let mut kept = Vec::new();
            while let Ok((s, _)) = l.accept().await {
                kept.push(s);
            }
        });
        assert!(port_open(port, WAIT).await);
        let t = std::time::Instant::now();
        assert!(!web_answers(port, Duration::from_millis(300)).await);
        assert!(t.elapsed() < Duration::from_secs(2), "the check gives up in time");
        held.abort();
    }
}
