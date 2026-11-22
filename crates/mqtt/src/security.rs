//! The broker link's TLS: rustls with the ring provider (no C library besides ring, no OpenSSL). A broker is trusted
//! one of two ways, as SMO's `MqttSecurity.kt` does it:
//!
//! - **Pinned**: the SHA-256 of the broker's certificate equals the owner's pin. Meant for a home Mosquitto with a
//!   self-signed certificate; the pin is the broker's identity, so its name and CA are not checked. The handshake's
//!   signature is still checked against that certificate's key, so a copy of the certificate alone gets nobody in.
//! - **Public CA**: the certificate chains to a root in webpki-roots (Mozilla's list, built in, the same on every
//!   machine) and matches the host name.
//!
//! TLS 1.3 only, unless the owner allows 1.2 for an old broker.

use crate::error::Error;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// `"AB:cd:01…"`, `"abcd01…"`, with spaces, or openssl's whole `sha256 Fingerprint=AB:CD…` line → the 32 bytes;
/// `None` if it isn't a SHA-256 (64 hex digits).
pub fn normalize_fingerprint(fp: &str) -> Option<[u8; 32]> {
    let after = fp.rsplit_once('=').map_or(fp, |(_, h)| h);
    let hex: String = after.chars().filter(char::is_ascii_alphanumeric).collect();
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex.to_ascii_lowercase(), &mut out).ok()?;
    Some(out)
}

/// SHA-256 of a DER certificate, lowercase hex: what to paste into the pin field.
pub fn fingerprint(cert_der: &[u8]) -> String {
    hex::encode(Sha256::digest(cert_der))
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The client's TLS settings: pinned when `pin` is set, else webpki roots and the host name.
pub(crate) fn client_config(pin: Option<[u8; 32]>, allow_tls12: bool) -> Result<Arc<ClientConfig>, Error> {
    let provider = provider();
    let versions: &[&rustls::SupportedProtocolVersion] =
        if allow_tls12 { &[&rustls::version::TLS13, &rustls::version::TLS12] } else { &[&rustls::version::TLS13] };
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(versions)
        .map_err(|e| Error::Tls(e.to_string()))?;
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
pub(crate) struct PinnedVerifier {
    pin: [u8; 32],
    algorithms: WebPkiSupportedAlgorithms,
}

/// A refused pin, with the fingerprint the broker sent so the owner can compare.
#[derive(Debug)]
pub(crate) struct PinMismatch(String);

impl std::fmt::Display for PinMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the broker's certificate {} does not match the pinned SHA-256", self.0)
    }
}

impl std::error::Error for PinMismatch {}

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
    fn fingerprints() {
        let want = [0x0f; 32];
        let colons = vec!["0F"; 32].join(":");
        for form in [
            "0f".repeat(32),
            colons.clone(),
            format!("sha256 Fingerprint={colons}"),
            format!("SHA256 Fingerprint={colons}\n"),
            vec!["0f"; 32].join(" "),
        ] {
            assert_eq!(normalize_fingerprint(&form), Some(want), "{form}");
        }
        for bad in ["", "0f", &"0f".repeat(31), &"0f".repeat(33), &"0g".repeat(32)] {
            assert_eq!(normalize_fingerprint(bad), None, "{bad}");
        }
    }

    fn cert(name: &str) -> CertificateDer<'static> {
        rcgen::generate_simple_self_signed(vec![name.to_string()]).unwrap().cert.der().clone()
    }

    #[test]
    fn pin_accepts_only_its_certificate() {
        let (broker, other) = (cert("broker.lan"), cert("broker.lan"));
        let pin = normalize_fingerprint(&fingerprint(&broker)).unwrap();
        let v = PinnedVerifier { pin, algorithms: provider().signature_verification_algorithms };
        let name = ServerName::try_from("anything.example").unwrap();
        assert!(
            v.verify_server_cert(&broker, &[], &name, &[], UnixTime::now()).is_ok(),
            "any name: the pin is the identity"
        );
        let err = v.verify_server_cert(&other, &[], &name, &[], UnixTime::now()).unwrap_err().to_string();
        assert!(err.contains(&fingerprint(&other)), "{err}");
        assert!(!v.supported_verify_schemes().is_empty());
    }

    #[test]
    fn configs() {
        assert!(client_config(Some([1; 32]), false).is_ok());
        assert!(client_config(None, true).is_ok());
    }

    #[test]
    fn constant_time_compare() {
        assert!(same(&[7; 32], &[7; 32]));
        assert!(!same(&[7; 32], &[7; 31]));
        let mut b = [7u8; 32];
        b[31] = 8;
        assert!(!same(&[7; 32], &b));
    }
}
