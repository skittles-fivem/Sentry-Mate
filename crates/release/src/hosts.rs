//! The only place this crate ever connects to: GitHub's API host, `api.github.com`, on 443. Every request is built
//! from an [`Endpoint`], and an `Endpoint` only comes from [`Endpoint::github`]: no host is ever read from settings,
//! the environment or an answer.

#[cfg(feature = "test-endpoints")]
use rustls::pki_types::CertificateDer;
#[cfg(feature = "test-endpoints")]
use std::sync::Arc;
pub use tesla::hosts::Origin;

/// GitHub's API host. `http://` is never used.
pub const HOST: &str = "api.github.com";

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
    /// GitHub's API host. The only way to make an `Endpoint` in a normal build.
    pub fn github() -> Endpoint {
        Endpoint { origin: Origin { host: HOST.to_string(), port: 443 }, roots: Roots::Public }
    }

    /// A fake GitHub API on this machine for tests, trusting only the certificate(s) in `trust_pem`. Not in a normal
    /// build.
    #[cfg(feature = "test-endpoints")]
    pub fn fake(host: &str, port: u16, trust_pem: &str) -> Endpoint {
        use rustls::pki_types::pem::PemObject;
        let certs: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(trust_pem.as_bytes()).filter_map(Result::ok).collect();
        Endpoint { origin: Origin { host: host.to_string(), port }, roots: Roots::Only(Arc::new(certs)) }
    }

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
    fn only_githubs_api_host() {
        let e = Endpoint::github();
        assert_eq!(e.host(), "api.github.com");
        assert_eq!(e.origin.https(), "https://api.github.com");
        assert!(e.allows(&Origin { host: "api.github.com".into(), port: 443 }));
        assert!(!e.allows(&Origin { host: "github.com".into(), port: 443 }));
        assert!(!e.allows(&Origin { host: "api.github.com".into(), port: 80 }));
    }
}
