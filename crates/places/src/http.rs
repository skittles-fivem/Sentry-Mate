//! The one way this crate talks to the internet: HTTPS only (rustls, certificates checked), a generic User-Agent
//! with nothing about the owner, no cookies, no Referer, no proxy unless the owner sets one, and every body read
//! with a size limit. Used for the GeoNames download and for Nominatim.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};

/// Sent with every request: names the program, never the owner, the car or the server's address.
pub const USER_AGENT: &str = "Sentry Mate (self-hosted)";

/// How to reach servers on the internet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpOptions {
    /// An HTTP proxy (`http://host:port`), when the server's network needs one. None: connect directly (the
    /// environment's proxy variables are not used, so nothing goes through a proxy nobody chose).
    pub proxy: Option<String>,
    /// PEM certificate(s) to trust instead of the public roots: for a self-hosted server with its own CA.
    pub trust_pem: Option<String>,
    /// Longest a whole request may take, ms.
    pub timeout_ms: u64,
}

impl Default for HttpOptions {
    fn default() -> Self {
        HttpOptions { proxy: None, trust_pem: None, timeout_ms: 30_000 }
    }
}

/// The address is `https://` with a real host (see [`host_ok`]), no user name or password in it, no fragment.
pub(crate) fn check_https_url(url: &str) -> Result<(), String> {
    let Some(rest) = url.strip_prefix("https://") else {
        return Err("the address must start with https://".into());
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if !host_ok(host) || url.contains('#') || url.chars().any(|c| c.is_whitespace()) {
        return Err(
            "the address needs a host name (letters, digits, -, _ and .) or [IPv6 address], an optional port, \
                    and no user name, spaces or #"
                .into(),
        );
    }
    Ok(())
}

/// A host as an address may name it: letters, digits, `-`, `_` and `.`, or an IPv6 address in brackets, then an
/// optional port 1 to 65535. The tile server's origin goes into the page's Content-Security-Policy as it is, so
/// anything that would widen or end a CSP source (`*`, `;`, `,`, quotes, braces) must never pass as a host.
fn host_ok(host: &str) -> bool {
    let (name, port) = if host.starts_with('[') {
        match host.find(']') {
            Some(i) => host.split_at(i + 1),
            None => return false,
        }
    } else {
        host.split_at(host.find(':').unwrap_or(host.len()))
    };
    let name_ok = match name.strip_prefix('[').and_then(|n| n.strip_suffix(']')) {
        Some(v6) => v6.contains(':') && v6.bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.'),
        None => {
            name.bytes().any(|b| b.is_ascii_alphanumeric())
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        }
    };
    let port_ok = match port.strip_prefix(':') {
        None => port.is_empty(),
        Some(p) => {
            p.len() <= 5
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u32>().is_ok_and(|n| (1..=65_535).contains(&n))
        }
    };
    name_ok && port_ok
}

/// `https://host[:port]` of an address: lower case, the default port and a trailing dot left out, so one server
/// always has one spelling (the rate limit is kept per origin).
pub(crate) fn origin(url: &str) -> String {
    let rest = url.strip_prefix("https://").unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("").to_ascii_lowercase();
    let host = host.strip_suffix(":443").unwrap_or(&host);
    let host = match host.rsplit_once(':') {
        Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) => {
            format!("{}:{port}", h.trim_end_matches('.'))
        }
        _ => host.trim_end_matches('.').to_string(),
    };
    format!("https://{host}")
}

/// What went wrong with a request, in words that never include the address asked for (it holds the position).
pub(crate) fn describe(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Timeout(_) => "timed out".into(),
        ureq::Error::HostNotFound => "host not found".into(),
        ureq::Error::ConnectionFailed => "could not connect".into(),
        ureq::Error::Io(e) => match e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) {
            Some(tls) => format!("TLS: {tls}"),
            None => format!("network error ({})", e.kind()),
        },
        ureq::Error::Tls(m) => format!("TLS: {m}"),
        ureq::Error::Rustls(e) => format!("TLS: {e}"),
        ureq::Error::BodyExceedsLimit(n) => format!("answer larger than {n} bytes"),
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => "redirect refused".into(),
        ureq::Error::RequireHttpsOnly(_) => "the server pointed to a plain http:// address".into(),
        ureq::Error::ConnectProxyFailed(_) | ureq::Error::InvalidProxyUrl => "the proxy refused the connection".into(),
        _ => "request failed".into(),
    }
}

pub(crate) fn agent(opts: &HttpOptions, max_redirects: u32) -> Result<ureq::Agent, String> {
    let roots = match &opts.trust_pem {
        None => RootCerts::WebPki,
        Some(pem) => {
            let certs: Vec<Certificate<'static>> = ureq::tls::parse_pem(pem.as_bytes())
                .filter_map(|item| match item {
                    Ok(ureq::tls::PemItem::Certificate(c)) => Some(Ok(c)),
                    Ok(_) => None,
                    Err(e) => Some(Err(e)),
                })
                .collect::<Result<_, _>>()
                .map_err(|e| format!("trusted certificate: {e}"))?;
            if certs.is_empty() {
                return Err("trusted certificate: no certificate in the PEM text".into());
            }
            RootCerts::new_with_certs(&certs)
        }
    };
    let proxy = match &opts.proxy {
        None => None,
        Some(p) => Some(ureq::Proxy::new(p).map_err(|e| format!("proxy: {e}"))?),
    };
    let tls = TlsConfig::builder().provider(TlsProvider::Rustls).root_certs(roots).build();
    let timeout = Duration::from_millis(opts.timeout_ms.clamp(1_000, 600_000));
    Ok(ureq::Agent::config_builder()
        .tls_config(tls)
        .https_only(true)
        .http_status_as_error(false)
        .proxy(proxy)
        .user_agent(USER_AGENT)
        .max_redirects(max_redirects)
        .timeout_global(Some(timeout))
        .build()
        .new_agent())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    #[test]
    fn only_plain_https_addresses() {
        assert!(check_https_url("https://nominatim.openstreetmap.org").is_ok());
        assert!(check_https_url("https://geo.example:8443/nominatim/").is_ok());
        for bad in [
            "http://nominatim.openstreetmap.org",
            "https://",
            "https://user:pw@example.org",
            "https://example.org/#x",
            "https://exa mple.org",
            "ftp://example.org",
            // Hosts that would widen or break a Content-Security-Policy source.
            "https://*/{z}/{x}/{y}.png",
            "https://*.example.org/x",
            "https://a;sandbox;/x",
            "https://a,b/x",
            "https://{z}.tile.example/{x}/{y}.png",
            "https://exa'mple.org",
            "https://example.org:/x",
            "https://example.org:0/x",
            "https://example.org:65536/x",
            "https://example.org:443:443/x",
            "https://example.org:80a/x",
            "https://[::1/x",
            "https://[nothex]/x",
            "https://.../x",
        ] {
            assert!(check_https_url(bad).is_err(), "{bad}");
        }
        for good in ["https://127.0.0.1:8443/x", "https://[::1]:8443/", "https://[fe80::1]", "https://geo_lan.local./"]
        {
            assert!(check_https_url(good).is_ok(), "{good}");
        }
        assert_eq!(origin("https://Geo.Example:8443/x?y"), "https://geo.example:8443");
        assert_eq!(origin("https://nominatim.openstreetmap.org.:443/"), "https://nominatim.openstreetmap.org");
        assert_eq!(origin("https://[::1]:8443/"), "https://[::1]:8443");
    }

    #[test]
    fn bad_trust_or_proxy_settings_are_refused() {
        let o = HttpOptions { trust_pem: Some("not a certificate".into()), ..HttpOptions::default() };
        assert!(agent(&o, 0).is_err());
        let o = HttpOptions { proxy: Some("::::".into()), ..HttpOptions::default() };
        assert!(agent(&o, 0).is_err());
        assert!(agent(&HttpOptions::default(), 0).is_ok());
    }
}
