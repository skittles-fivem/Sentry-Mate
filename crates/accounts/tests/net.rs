#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use accounts::{Client, is_local, parse_address};

fn ip(s: &str) -> std::net::IpAddr {
    parse_address(s).unwrap_or_else(|| panic!("{s} should parse"))
}

#[test]
fn private_loopback_and_link_local_addresses_are_home() {
    for a in [
        "127.0.0.1",
        "127.255.255.254",
        "10.0.0.1",
        "10.255.255.255",
        "172.16.0.1",
        "172.31.255.254",
        "192.168.0.1",
        "192.168.255.255",
        "169.254.1.1",
        "::1",
        "fc00::1",
        "fd12:3456:789a::1",
        "fe80::1",
        "febf:ffff::1",
        "::ffff:192.168.1.2",
        "::ffff:127.0.0.1",
    ] {
        assert!(is_local(ip(a)), "{a} is local");
    }
}

#[test]
fn everything_else_is_outside() {
    for a in [
        "8.8.8.8",
        "1.1.1.1",
        "172.15.255.255",
        "172.32.0.1",
        "192.169.0.1",
        "11.0.0.1",
        "100.64.0.1",
        "169.253.255.255",
        "203.0.113.5",
        "0.0.0.0",
        "255.255.255.255",
        "2001:db8::1",
        "2606:4700::1111",
        "fec0::1",
        "fbff::1",
        "::",
        "::ffff:8.8.8.8",
        "64:ff9b::c0a8:0102",
    ] {
        assert!(!is_local(ip(a)), "{a} is outside");
    }
}

#[test]
fn addresses_are_read_however_they_arrive() {
    assert_eq!(ip("192.0.2.1:443"), ip("192.0.2.1"));
    assert_eq!(ip("[2001:db8::1]:443"), ip("2001:db8::1"));
    assert_eq!(ip("[2001:db8::1]"), ip("2001:db8::1"));
    assert_eq!(ip("fe80::1%eth0"), ip("fe80::1"));
    assert_eq!(ip(" ::FFFF:192.0.2.1 "), ip("192.0.2.1"), "IPv4 inside IPv6 is the IPv4 address");
    assert_eq!(ip("[fe80::1%eth0]:443"), ip("fe80::1"));
    assert_eq!(ip("[fe80::1%3]"), ip("fe80::1"));
    for bad in ["", "localhost", "192.168.1", "1.2.3.4.5", "[::1", "::g", &"1".repeat(200)] {
        assert_eq!(parse_address(bad), None, "{bad}");
    }
    // nothing but a port after a bracket, zones only on IPv6, brackets only around IPv6
    for bad in
        ["[::1]junk", "[::1]:", "[::1]:99999", "[::1]:443x", "1.2.3.4%eth0", "fe80::1%", "fe80::1%a b", "[1.2.3.4]"]
    {
        assert_eq!(parse_address(bad), None, "{bad}");
    }
    let c = Client::new(ip("::ffff:10.0.0.9"), "Phone\nwith a newline");
    assert!(c.local);
    assert_eq!(c.address, ip("10.0.0.9"));
    assert_eq!(c.device, "Phone with a newline");
}
