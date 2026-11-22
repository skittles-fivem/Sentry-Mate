//! TLS to the owner's own TeslaMate (its MQTT broker, and its database for the catch-up): rustls with the ring
//! provider, TLS 1.2 or 1.3 (a home broker or PostgreSQL often offers only 1.2). A server is trusted one of two ways,
//! as the MQTT publisher does it (`crates/mqtt/src/security.rs`):
//!
//! - **Pinned** (`pin_sha256`): the SHA-256 of the server's certificate equals the pin. For a self-signed certificate
//!   on the home network; the pin is the server's identity, so its name and CA are not checked. The handshake's
//!   signature is still checked against that certificate's key.
//! - **Public CA**: the certificate chains to a root in webpki-roots and matches the host name.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// `"AB:cd:01…"` or `"abcd01…"` (colons or spaces allowed) → the 32 bytes; `None` if it isn't 64 hex digits.
pub fn parse_pin(pin: &str) -> Option<[u8; 32]> {
    let hex: String = pin.chars().filter(|c| *c != ':' && !c.is_whitespace()).collect();
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex.to_ascii_lowercase(), &mut out).ok()?;
    Some(out)
}

/// SHA-256 of a DER certificate, lowercase hex: what goes in `pin_sha256`.
pub fn fingerprint(cert_der: &[u8]) -> String {
    hex::encode(Sha256::digest(cert_der))
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The client's TLS settings: pinned when `pin` is set, else webpki roots and the host name.
pub fn client_config(pin: Option<[u8; 32]>) -> Result<Arc<ClientConfig>, rustls::Error> {
    let provider = provider();
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])?;
    let config = match pin {
        Some(pin) => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedVerifier {
                pin,
                algorithms: provider.signature_verification_algorithms,
            }))
            .with_no_client_auth(),
        None => {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            builder.with_root_certificates(roots).with_no_client_auth()
        }
    };
    Ok(Arc::new(config))
}

/// Trusts exactly the certificate whose SHA-256 is `pin`.
#[derive(Debug)]
struct PinnedVerifier {
    pin: [u8; 32],
    algorithms: WebPkiSupportedAlgorithms,
}

/// A refused pin, with the fingerprint the server sent so the owner can compare.
#[derive(Debug)]
pub struct PinMismatch(pub String);

impl std::fmt::Display for PinMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the server's certificate {} does not match the pinned SHA-256", self.0)
    }
}

impl std::error::Error for PinMismatch {}

/// Whether a rustls error is a refused pin.
pub fn is_pin_mismatch(e: &rustls::Error) -> bool {
    matches!(e, rustls::Error::InvalidCertificate(rustls::CertificateError::Other(o))
        if o.0.downcast_ref::<PinMismatch>().is_some())
}

/// Compares without stopping at the first different byte.
fn same(a: &[u8; 32], b: &[u8]) -> bool {
    b.len() == a.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let got = Sha256::digest(end_entity.as_ref());
        if same(&self.pin, &got) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(rustls::CertificateError::Other(rustls::OtherError(Arc::new(
                PinMismatch(hex::encode(got)),
            )))))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins() {
        let want = [0x0f; 32];
        assert_eq!(parse_pin(&"0f".repeat(32)), Some(want));
        assert_eq!(parse_pin(&vec!["0F"; 32].join(":")), Some(want));
        for bad in ["", "0f", &"0f".repeat(31), &"0g".repeat(32), &format!("x{}", "0f".repeat(32))] {
            assert_eq!(parse_pin(bad), None, "{bad}");
        }
        assert!(client_config(Some(want)).is_ok());
        assert!(client_config(None).is_ok());
        assert!(same(&[7; 32], &[7; 32]) && !same(&[7; 32], &[7; 31]));
    }
}
