//! The only place this crate ever connects to: TeslaFi's API host, `www.teslafi.com`, on 443. Every request is built
//! from an [`Endpoint`], and an `Endpoint` only comes from [`Endpoint::teslafi`]: no host is ever read from settings,
//! the environment or an answer.

#[cfg(feature = "test-endpoints")]
use rustls::pki_types::CertificateDer;
#[cfg(feature = "test-endpoints")]
use std::sync::Arc;
pub use tesla::hosts::Origin;

/// TeslaFi's API host (`feed.php`). `http://` and the bare `teslafi.com` are never used.
pub const HOST: &str = "www.teslafi.com";

/// The certificates a connection trusts.
#[derive(Clone)]
pub(crate) enum Roots {
    /// The public web roots (webpki-roots), as every browser.
    Public,
    /// Only these (the fake server of the tests).
    #[cfg(feature = "test-endpoints")]
    Only(Arc<Vec<CertificateDer<'static>>>),
}

/// Where the requests go.
#[derive(Clone)]
pub struct Endpoint {
    pub origin: Origin,
    pub(crate) roots: Roots,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint").field("origin", &self.origin.authority()).finish()
    }
}

impl Endpoint {
    /// TeslaFi's own host. The only way to make an `Endpoint` in a normal build.
    pub fn teslafi() -> Endpoint {
        Endpoint { origin: Origin { host: HOST.to_string(), port: 443 }, roots: Roots::Public }
    }

    /// A fake TeslaFi on this machine for tests, trusting only the certificate(s) in `trust_pem`. Not in a normal
    /// build.
    #[cfg(feature = "test-endpoints")]
    pub fn fake(host: &str, port: u16, trust_pem: &str) -> Endpoint {
        use rustls::pki_types::pem::PemObject;
        let certs: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(trust_pem.as_bytes()).filter_map(Result::ok).collect();
        Endpoint { origin: Origin { host: host.to_string(), port }, roots: Roots::Only(Arc::new(certs)) }
    }

    /// The host, for the settings page ("connects only to ...").
    pub fn host(&self) -> &str {
        &self.origin.host
    }

    /// Whether `origin` is this endpoint (checked again before every connection).
    pub(crate) fn allows(&self, origin: &Origin) -> bool {
        *origin == self.origin
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_teslafis_host() {
        let e = Endpoint::teslafi();
        assert_eq!(e.host(), "www.teslafi.com");
        assert_eq!(e.origin.https(), "https://www.teslafi.com");
        assert!(e.allows(&Origin { host: "www.teslafi.com".into(), port: 443 }));
        assert!(!e.allows(&Origin { host: "teslafi.com".into(), port: 443 }));
        assert!(!e.allows(&Origin { host: "www.teslafi.com".into(), port: 80 }));
        assert!(!e.allows(&Origin { host: "owner-api.teslamotors.com".into(), port: 443 }));
    }
}
