//! Client addresses: whether one is on the home network, and the key failed sign-ins are counted under.
//!
//! The home network here is what can't be routed from the internet: loopback, the private IPv4 ranges, link-local,
//! and IPv6 unique local addresses. The web layer decides which address a request comes from (the socket's, or a
//! trusted proxy's `X-Forwarded-For`) and may count more ranges as home (a VPN, an office); it passes the answer in
//! [`crate::Client::local`], so this is the default, not the only rule.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// IPv4 addresses inside IPv6 (`::ffff:a.b.c.d`) as the IPv4 address they are, so one address has one spelling.
pub fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn local_v4(a: Ipv4Addr) -> bool {
    // 127/8 loopback, 10/8, 172.16/12, 192.168/16 private, 169.254/16 link-local
    a.is_loopback() || a.is_private() || a.is_link_local()
}

fn local_v6(a: Ipv6Addr) -> bool {
    let first = a.segments()[0];
    a.is_loopback()
        // fc00::/7: unique local addresses (the IPv6 private range)
        || (first & 0xfe00) == 0xfc00
        // fe80::/10: link-local
        || (first & 0xffc0) == 0xfe80
}

/// Whether an address is on the local network: loopback, private (10/8, 172.16/12, 192.168/16, fc00::/7) or
/// link-local (169.254/16, fe80::/10), for IPv4 and IPv6 (IPv4 written inside IPv6 included).
pub fn is_local(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(a) => local_v4(a),
        IpAddr::V6(a) => local_v6(a),
    }
}

/// An address as it arrives in a socket or a header: with or without a port (`192.0.2.1:443`, `[2001:db8::1]:443`),
/// with an IPv6 zone (`fe80::1%eth0`), IPv4 inside IPv6. `None`: not an address.
pub fn parse_address(text: &str) -> Option<IpAddr> {
    let s = text.trim();
    if s.is_empty() || s.len() > 100 {
        return None;
    }
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Some(canonical(ip));
    }
    if let Ok(sa) = s.parse::<std::net::SocketAddr>() {
        return Some(canonical(sa.ip()));
    }
    // a bracketed IPv6 without a port, or an address with a zone
    let s = s.strip_prefix('[').and_then(|r| r.split_once(']')).map_or(s, |(inner, _)| inner);
    let s = s.split_once('%').map_or(s, |(a, _)| a);
    s.parse::<IpAddr>().ok().map(canonical)
}

/// The key failed tries from an address are counted under: the address itself for IPv4, the /64 for IPv6 (one home
/// or one server gets a whole /64, so counting single IPv6 addresses would let it try 2^64 times).
pub(crate) fn throttle_key(ip: IpAddr) -> String {
    match canonical(ip) {
        IpAddr::V4(a) => a.to_string(),
        IpAddr::V6(a) => {
            let s = a.segments();
            format!("{}/64", Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    #[test]
    fn ipv6_counts_by_its_64() {
        let a = parse_address("2001:db8:1:2:aaaa::1").unwrap();
        let b = parse_address("2001:db8:1:2:bbbb::2").unwrap();
        let c = parse_address("2001:db8:1:3::1").unwrap();
        assert_eq!(throttle_key(a), "2001:db8:1:2::/64");
        assert_eq!(throttle_key(a), throttle_key(b));
        assert_ne!(throttle_key(a), throttle_key(c));
        assert_eq!(throttle_key(parse_address("::ffff:192.0.2.7").unwrap()), "192.0.2.7");
    }
}
