//! The only places this crate ever connects to. Every request is built from an [`Endpoints`], and an `Endpoints`
//! only comes from [`Endpoints::for_region`]: no host is ever read from settings, the environment or an answer.

#[cfg(feature = "test-endpoints")]
use rustls::pki_types::CertificateDer;
use serde::{Deserialize, Serialize};
#[cfg(feature = "test-endpoints")]
use std::sync::Arc;

/// Which of Tesla's sign-in and API hosts an account uses. Fixed per account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    #[default]
    Global,
    China,
}

impl Region {
    pub fn word(self) -> &'static str {
        match self {
            Region::Global => "global",
            Region::China => "china",
        }
    }

    pub fn parse(s: &str) -> Option<Region> {
        match s {
            "global" => Some(Region::Global),
            "china" => Some(Region::China),
            _ => None,
        }
    }
}

/// Tesla's hosts, global: sign-in, REST API, streaming.
pub const GLOBAL_HOSTS: [&str; 3] = ["auth.tesla.com", "owner-api.teslamotors.com", "streaming.vn.teslamotors.com"];
/// Tesla's hosts, China: sign-in and REST API. No public source names a streaming host there, so China accounts
/// are polled only.
pub const CHINA_HOSTS: [&str; 2] = ["auth.tesla.cn", "owner-api.vn.cloud.tesla.cn"];

/// One HTTPS (or WSS) server: host and port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub host: String,
    pub port: u16,
}

impl Origin {
    fn tesla(host: &str) -> Origin {
        Origin { host: host.to_string(), port: 443 }
    }

    /// `host` or `host:port` when the port is not 443: what goes after `https://`.
    pub fn authority(&self) -> String {
        if self.port == 443 { self.host.clone() } else { format!("{}:{}", self.host, self.port) }
    }

    pub fn https(&self) -> String {
        format!("https://{}", self.authority())
    }
}

/// The certificates a connection trusts.
#[derive(Clone)]
pub(crate) enum Roots {
    /// The public web roots (webpki-roots), as every browser.
    Public,
    /// Only these (the fake server of the tests).
    #[cfg(feature = "test-endpoints")]
    Only(Arc<Vec<CertificateDer<'static>>>),
}

/// Where one account's requests go.
#[derive(Clone)]
pub struct Endpoints {
    pub region: Region,
    /// The sign-in server: the authorize page (opened in the person's browser) and the token endpoint.
    pub auth: Origin,
    /// The REST API.
    pub api: Origin,
    /// The streaming websocket (`/streaming/`), where there is one.
    pub stream: Option<Origin>,
    pub(crate) roots: Roots,
}

impl std::fmt::Debug for Endpoints {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoints")
            .field("region", &self.region)
            .field("auth", &self.auth.authority())
            .field("api", &self.api.authority())
            .field("stream", &self.stream.as_ref().map(Origin::authority))
            .finish()
    }
}

impl Endpoints {
    /// Tesla's own hosts for a region. The only way to make an `Endpoints` in a normal build.
    pub fn for_region(region: Region) -> Endpoints {
        match region {
            Region::Global => Endpoints {
                region,
                auth: Origin::tesla(GLOBAL_HOSTS[0]),
                api: Origin::tesla(GLOBAL_HOSTS[1]),
                stream: Some(Origin::tesla(GLOBAL_HOSTS[2])),
                roots: Roots::Public,
            },
            Region::China => Endpoints {
                region,
                auth: Origin::tesla(CHINA_HOSTS[0]),
                api: Origin::tesla(CHINA_HOSTS[1]),
                stream: None,
                roots: Roots::Public,
            },
        }
    }

    /// A fake Tesla on this machine for tests: sign-in and REST on `api`, streaming on `stream`, trusting only the
    /// certificate(s) in `trust_pem`. Not in a normal build.
    #[cfg(feature = "test-endpoints")]
    pub fn fake(host: &str, api_port: u16, stream_port: Option<u16>, trust_pem: &str) -> Endpoints {
        let certs = pem_certs(trust_pem);
        let api = Origin { host: host.to_string(), port: api_port };
        Endpoints {
            region: Region::Global,
            auth: api.clone(),
            api,
            stream: stream_port.map(|port| Origin { host: host.to_string(), port }),
            roots: Roots::Only(Arc::new(certs)),
        }
    }

    /// Every host this account may connect to, for the settings page ("connects only to ...").
    pub fn hosts(&self) -> Vec<String> {
        let mut h = vec![self.auth.host.clone(), self.api.host.clone()];
        h.extend(self.stream.as_ref().map(|s| s.host.clone()));
        h.dedup();
        h
    }

    /// Whether `origin` is one of this account's servers (checked again before every connection).
    pub(crate) fn allows(&self, origin: &Origin) -> bool {
        *origin == self.auth || *origin == self.api || self.stream.as_ref() == Some(origin)
    }

    /// The `issuer` Tesla names in the sign-in callback.
    pub fn issuer(&self) -> String {
        format!("{}/oauth2/v3", self.auth.https())
    }
}

#[cfg(feature = "test-endpoints")]
fn pem_certs(pem: &str) -> Vec<CertificateDer<'static>> {
    use base64::Engine as _;
    let mut out = Vec::new();
    let mut body = String::new();
    let mut inside = false;
    for line in pem.lines() {
        let line = line.trim();
        if line == "-----BEGIN CERTIFICATE-----" {
            inside = true;
            body.clear();
        } else if line == "-----END CERTIFICATE-----" {
            inside = false;
            if let Ok(der) = base64::engine::general_purpose::STANDARD.decode(&body) {
                out.push(CertificateDer::from(der));
            }
        } else if inside {
            body.push_str(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_teslas_hosts() {
        let g = Endpoints::for_region(Region::Global);
        assert_eq!(g.hosts(), GLOBAL_HOSTS.map(String::from).to_vec());
        assert_eq!(g.issuer(), "https://auth.tesla.com/oauth2/v3");
        let c = Endpoints::for_region(Region::China);
        assert_eq!(c.hosts(), CHINA_HOSTS.map(String::from).to_vec());
        assert!(c.stream.is_none());
        assert!(!g.allows(&Origin { host: "example.com".into(), port: 443 }));
        assert!(!g.allows(&Origin { host: "auth.tesla.com".into(), port: 8443 }));
        assert!(g.allows(&Origin { host: "owner-api.teslamotors.com".into(), port: 443 }));
    }
}
