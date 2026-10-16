//! Which addresses a URL download may connect to.
//!
//! A download runs with the node's network position, so a link such as
//! `http://127.0.0.1:8080/...` or `http://169.254.169.254/...` would let an
//! API caller read services that only the node can reach. The policy blocks
//! loopback, link-local, unspecified, multicast, reserved and private
//! addresses, the `WireGuard` mesh prefix and the node's own addresses. It
//! applies to the addresses a name resolves to and to every redirect hop.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ipnet::{IpNet, Ipv6Net};

use crate::config::DownloadConfig;

/// Why an address is blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    Unspecified,
    Loopback,
    LinkLocal,
    Multicast,
    Reserved,
    /// RFC 1918, unique local (`fc00::/7`) or carrier-grade NAT (100.64.0.0/10).
    Private,
    Mesh,
    ThisNode,
}

impl Blocked {
    fn label(self) -> &'static str {
        match self {
            Blocked::Unspecified => "an unspecified address",
            Blocked::Loopback => "a loopback address",
            Blocked::LinkLocal => "a link-local address",
            Blocked::Multicast => "a multicast address",
            Blocked::Reserved => "a reserved address",
            Blocked::Private => "a private network address",
            Blocked::Mesh => "an address on the cluster mesh",
            Blocked::ThisNode => "an address of this node",
        }
    }
}

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// The blocklist for one download.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    allow_private: bool,
    allow: Vec<IpNet>,
    mesh: Option<Ipv6Net>,
    own: Vec<IpAddr>,
}

impl Policy {
    pub fn new(cfg: &DownloadConfig, mesh: Option<Ipv6Net>, own: Vec<IpAddr>) -> Self {
        Self {
            allow_private: cfg.allow_private,
            allow: cfg.allow.clone(),
            mesh,
            own,
        }
    }

    /// Checks one address. An address inside `allow` always passes. With
    /// `allow_private` the private ranges pass, but loopback, link-local, the
    /// mesh and the node's own addresses stay blocked.
    pub fn check(&self, ip: IpAddr) -> Result<(), Blocked> {
        let ip = canonical(ip);
        if self.allow.iter().any(|n| n.contains(&ip)) {
            return Ok(());
        }
        if self.own.iter().any(|o| canonical(*o) == ip) {
            return Err(Blocked::ThisNode);
        }
        if let IpAddr::V6(v6) = ip
            && self.mesh.is_some_and(|m| m.contains(&v6))
        {
            return Err(Blocked::Mesh);
        }
        match classify(ip) {
            None => Ok(()),
            Some(Blocked::Private) if self.allow_private => Ok(()),
            Some(b) => Err(b),
        }
    }

    /// Checks every address a name resolved to.
    pub fn check_all(&self, host: &str, ips: &[IpAddr]) -> Result<(), String> {
        for ip in ips {
            if let Err(b) = self.check(*ip) {
                return Err(blocked_message(host, *ip, b));
            }
        }
        Ok(())
    }
}

fn blocked_message(host: &str, ip: IpAddr, why: Blocked) -> String {
    let via = if host.parse::<IpAddr>().is_ok() {
        String::new()
    } else {
        format!(" ({host} resolves to {ip})")
    };
    let hint = if why == Blocked::Private {
        "set download.allow_private = true or add the range to download.allow"
    } else {
        "add the range to download.allow"
    };
    format!("downloads from {why} are blocked{via}; to allow it, {hint} in prism.toml")
}

/// Maps IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) to IPv4, and strips the
/// NAT64 well-known prefix (`64:ff9b::/96`), so they are judged by the IPv4
/// address they carry.
fn canonical(ip: IpAddr) -> IpAddr {
    if let IpAddr::V6(v6) = ip {
        let s = v6.segments();
        if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
            let o = v6.octets();
            return IpAddr::V4(Ipv4Addr::new(o[12], o[13], o[14], o[15]));
        }
    }
    ip.to_canonical()
}

/// The category of a special-purpose address, or `None` for a public one.
/// The mesh prefix and the node's own addresses are handled by `Policy`.
pub fn classify(ip: IpAddr) -> Option<Blocked> {
    match canonical(ip) {
        IpAddr::V4(a) => classify_v4(a),
        IpAddr::V6(a) => classify_v6(a),
    }
}

fn classify_v4(a: Ipv4Addr) -> Option<Blocked> {
    let o = a.octets();
    if a.is_unspecified() || o[0] == 0 {
        Some(Blocked::Unspecified)
    } else if a.is_loopback() {
        Some(Blocked::Loopback)
    } else if a.is_link_local() {
        Some(Blocked::LinkLocal)
    } else if a.is_multicast() {
        Some(Blocked::Multicast)
    } else if a.is_private() || (o[0] == 100 && (o[1] & 0xc0) == 64) {
        Some(Blocked::Private)
    } else if a.is_broadcast()
        || o[0] >= 240
        || (o[..3] == [192, 0, 0])
        || (o[..3] == [192, 0, 2])
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
        || (o[..3] == [198, 51, 100])
        || (o[..3] == [203, 0, 113])
    {
        Some(Blocked::Reserved)
    } else {
        None
    }
}

fn classify_v6(a: Ipv6Addr) -> Option<Blocked> {
    let s = a.segments();
    if a.is_unspecified() {
        Some(Blocked::Unspecified)
    } else if a.is_loopback() {
        Some(Blocked::Loopback)
    } else if (s[0] & 0xffc0) == 0xfe80 {
        Some(Blocked::LinkLocal)
    } else if a.is_multicast() {
        Some(Blocked::Multicast)
    } else if (s[0] & 0xfe00) == 0xfc00 {
        Some(Blocked::Private)
    } else if (s[0] & 0xffc0) == 0xfec0 // deprecated site-local
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0 // discard-only
        || (s[..5] == [0, 0, 0, 0, 0] && s[5] != 0xffff)
    // ::/96 compatible and other low space
    {
        Some(Blocked::Reserved)
    } else {
        None
    }
}

/// Addresses assigned to this host, read from `/proc`. Failures give an
/// empty list; the loopback and link-local rules still apply.
pub fn own_addresses() -> Vec<IpAddr> {
    let mut out = Vec::new();
    if let Ok(t) = std::fs::read_to_string("/proc/net/if_inet6") {
        out.extend(parse_if_inet6(&t).into_iter().map(IpAddr::V6));
    }
    if let Ok(t) = std::fs::read_to_string("/proc/net/fib_trie") {
        out.extend(parse_fib_trie(&t).into_iter().map(IpAddr::V4));
    }
    out
}

/// Parses `/proc/net/if_inet6`: the first field is 32 hex digits.
pub fn parse_if_inet6(text: &str) -> Vec<Ipv6Addr> {
    text.lines()
        .filter_map(|l| {
            let h = l.split_whitespace().next()?;
            u128::from_str_radix(h, 16)
                .ok()
                .filter(|_| h.len() == 32)
                .map(Ipv6Addr::from)
        })
        .collect()
}

/// Parses `/proc/net/fib_trie`: a `/32 host LOCAL` line follows the line
/// that names each local address (`|-- 10.0.0.5`).
pub fn parse_fib_trie(text: &str) -> Vec<Ipv4Addr> {
    let mut last: Option<Ipv4Addr> = None;
    let mut out = Vec::new();
    for l in text.lines() {
        let l = l.trim();
        if let Some(rest) = l
            .strip_prefix("|--")
            .or_else(|| l.strip_prefix("+--"))
            .map(str::trim)
        {
            last = rest.split_whitespace().next().and_then(|a| a.parse().ok());
        } else if l.starts_with("/32 host LOCAL")
            && let Some(a) = last
            && !out.contains(&a)
        {
            out.push(a);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn policy(allow_private: bool, allow: &[&str]) -> Policy {
        Policy {
            allow_private,
            allow: allow.iter().map(|s| s.parse().unwrap()).collect(),
            mesh: Some("fd12:3456:789a::/48".parse().unwrap()),
            own: vec![ip("203.0.113.9"), ip("2001:db8::9")],
        }
    }

    #[test]
    fn public_addresses_pass() {
        let p = Policy::default();
        for a in [
            "1.1.1.1",
            "93.184.216.34",
            "172.32.0.1",
            "100.128.0.1",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert_eq!(p.check(ip(a)), Ok(()), "{a}");
        }
    }

    #[test]
    fn special_addresses_are_blocked() {
        let cases = [
            ("0.0.0.0", Blocked::Unspecified),
            ("0.1.2.3", Blocked::Unspecified),
            ("::", Blocked::Unspecified),
            ("127.0.0.1", Blocked::Loopback),
            ("127.255.0.9", Blocked::Loopback),
            ("::1", Blocked::Loopback),
            ("169.254.169.254", Blocked::LinkLocal),
            ("fe80::1", Blocked::LinkLocal),
            ("224.0.0.1", Blocked::Multicast),
            ("239.255.255.250", Blocked::Multicast),
            ("ff02::1", Blocked::Multicast),
            ("10.1.2.3", Blocked::Private),
            ("172.16.0.1", Blocked::Private),
            ("172.31.255.255", Blocked::Private),
            ("192.168.1.1", Blocked::Private),
            ("100.64.0.1", Blocked::Private),
            ("100.127.255.255", Blocked::Private),
            ("fc00::1", Blocked::Private),
            ("fd00::1", Blocked::Private),
            ("255.255.255.255", Blocked::Reserved),
            ("240.0.0.1", Blocked::Reserved),
            ("192.0.0.1", Blocked::Reserved),
            ("198.18.0.1", Blocked::Reserved),
            ("198.19.255.255", Blocked::Reserved),
            ("2001:db8::1", Blocked::Reserved),
        ];
        let p = Policy::default();
        for (a, why) in cases {
            assert_eq!(p.check(ip(a)), Err(why), "{a}");
        }
    }

    #[test]
    fn embedded_ipv4_is_judged_as_ipv4() {
        let p = Policy::default();
        assert_eq!(p.check(ip("::ffff:127.0.0.1")), Err(Blocked::Loopback));
        assert_eq!(p.check(ip("::ffff:10.0.0.1")), Err(Blocked::Private));
        assert_eq!(p.check(ip("64:ff9b::a9fe:a9fe")), Err(Blocked::LinkLocal));
        assert_eq!(p.check(ip("64:ff9b::808:808")), Ok(()));
        assert_eq!(p.check(ip("::7f00:1")), Err(Blocked::Reserved));
    }

    #[test]
    fn mesh_and_own_addresses_are_blocked() {
        let p = policy(false, &[]);
        assert_eq!(p.check(ip("fd12:3456:789a::5")), Err(Blocked::Mesh));
        assert_eq!(p.check(ip("203.0.113.9")), Err(Blocked::ThisNode));
        assert_eq!(p.check(ip("::ffff:203.0.113.9")), Err(Blocked::ThisNode));
        // allow_private does not open the mesh or this node.
        let p = policy(true, &[]);
        assert_eq!(p.check(ip("fd12:3456:789a::5")), Err(Blocked::Mesh));
        assert_eq!(p.check(ip("203.0.113.9")), Err(Blocked::ThisNode));
    }

    #[test]
    fn allow_private_opens_only_private_ranges() {
        let p = policy(true, &[]);
        for a in ["10.0.0.1", "192.168.0.1", "100.64.0.1", "fd00::1"] {
            assert_eq!(p.check(ip(a)), Ok(()), "{a}");
        }
        for a in [
            "127.0.0.1",
            "::1",
            "169.254.169.254",
            "0.0.0.0",
            "224.0.0.1",
        ] {
            assert!(p.check(ip(a)).is_err(), "{a}");
        }
    }

    #[test]
    fn allow_list_overrides_every_rule() {
        let p = policy(
            false,
            &["10.5.0.0/16", "127.0.0.1/32", "fd12:3456:789a::/64"],
        );
        assert_eq!(p.check(ip("10.5.9.9")), Ok(()));
        assert_eq!(p.check(ip("127.0.0.1")), Ok(()));
        assert_eq!(p.check(ip("fd12:3456:789a::5")), Ok(()));
        assert!(p.check(ip("10.6.0.1")).is_err());
        assert!(p.check(ip("127.0.0.2")).is_err());
    }

    #[test]
    fn one_bad_answer_blocks_the_name() {
        let p = Policy::default();
        let ips = [ip("93.184.216.34"), ip("127.0.0.1")];
        let e = p.check_all("example.test", &ips).unwrap_err();
        assert!(e.contains("example.test resolves to 127.0.0.1"), "{e}");
        assert!(e.contains("download.allow"), "{e}");
        assert!(!e.contains("allow_private"), "{e}");
        let e = p.check_all("example.test", &[ip("10.0.0.1")]).unwrap_err();
        assert!(e.contains("download.allow_private"), "{e}");
        assert!(p.check_all("example.test", &ips[..1]).is_ok());
        let e = p.check_all("127.0.0.1", &ips[1..]).unwrap_err();
        assert!(!e.contains("resolves"), "{e}");
    }

    #[test]
    fn parses_proc_files() {
        let v6 = "fe800000000000000000000000000001 02 40 20 80 eth0\n\
                  00000000000000000000000000000001 01 80 10 80 lo\n\
                  short 01\n";
        assert_eq!(
            parse_if_inet6(v6),
            vec!["fe80::1".parse::<Ipv6Addr>().unwrap(), Ipv6Addr::LOCALHOST]
        );
        let trie = "Main:\n  +-- 0.0.0.0/0 3 0 5\n     |-- 0.0.0.0\n        /0 universe UNICAST\n     +-- 10.0.0.0/24 2 0 2\n        |-- 10.0.0.0\n           /32 link BROADCAST\n        |-- 10.0.0.5\n           /32 host LOCAL\n     |-- 127.0.0.1\n           /32 host LOCAL\n";
        assert_eq!(
            parse_fib_trie(trie),
            vec![Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::LOCALHOST]
        );
    }
}
