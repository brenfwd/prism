//! An `IPv6` router advertisement daemon for one switch bridge.
//!
//! It announces a /64 for SLAAC (RFC 4861), the node's DNS server (RDNSS, RFC 8106) and the
//! search domain (DNSSL), and answers router solicitations. A node that should not be a
//! default router advertises a router lifetime of zero: guests still autoconfigure but send
//! no off-link traffic to it.

use std::io;
use std::net::{Ipv6Addr, SocketAddrV6};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use ipnet::Ipv6Net;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::time::Instant;

use crate::{Error, MacAddr, Result};

const ICMPV6_ROUTER_SOLICITATION: u8 = 133;
const ICMPV6_ROUTER_ADVERT: u8 = 134;

const OPT_SOURCE_LLADDR: u8 = 1;
const OPT_PREFIX_INFO: u8 = 3;
const OPT_MTU: u8 = 5;
const OPT_RDNSS: u8 = 25;
const OPT_DNSSL: u8 = 31;

const FLAG_ON_LINK: u8 = 0x80;
const FLAG_AUTONOMOUS: u8 = 0x40;

/// Hop limit the guests put on their packets.
const CUR_HOP_LIMIT: u8 = 64;
/// Neighbour discovery messages are only valid with this hop limit.
const ND_HOP_LIMIT: u32 = 255;
const ALL_NODES: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1);
const ALL_ROUTERS: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 2);
/// Time between unsolicited advertisements.
pub const INTERVAL: Duration = Duration::from_secs(120);
/// The shortest time between two advertisements, however many solicitations arrive.
const MIN_GAP: Duration = Duration::from_secs(3);
/// Lifetime of the prefix: guests keep an address for this long after the last advertisement.
const VALID_LIFETIME: u32 = 24 * 3600;
const PREFERRED_LIFETIME: u32 = 4 * 3600;
/// RDNSS and DNSSL entries live three times as long as the interval between advertisements.
const DNS_LIFETIME: u32 = 3 * INTERVAL.as_secs() as u32;
/// How long to wait for the bridge's link-local address to leave the tentative state.
const BIND_ATTEMPTS: u32 = 40;
const BIND_RETRY: Duration = Duration::from_millis(500);

/// What to announce on one bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaConfig {
    /// The switch bridge to send on.
    pub interface: String,
    /// The bridge's link-local address; advertisements come from it.
    pub source: Ipv6Addr,
    /// The /64 guests autoconfigure from.
    pub prefix: Ipv6Net,
    /// How long guests keep this node as a default router; zero for none.
    pub router_lifetime: Duration,
    pub mtu: Option<u32>,
    /// Recursive DNS servers (RDNSS).
    pub dns: Vec<Ipv6Addr>,
    /// Search domain (DNSSL).
    pub domain: Option<String>,
}

/// A configuration the daemon re-reads before every advertisement.
pub type SharedRa = Arc<RwLock<RaConfig>>;

/// The advertisement for `cfg`: an `ICMPv6` message without its checksum, which the kernel
/// fills in on a raw `ICMPv6` socket. `lladdr` is the bridge's MAC address, if known.
pub fn build_advert(cfg: &RaConfig, lladdr: Option<MacAddr>) -> Vec<u8> {
    let lifetime = u16::try_from(cfg.router_lifetime.as_secs()).unwrap_or(u16::MAX);
    let mut p = vec![ICMPV6_ROUTER_ADVERT, 0, 0, 0, CUR_HOP_LIMIT, 0];
    p.extend_from_slice(&lifetime.to_be_bytes());
    p.extend_from_slice(&[0; 8]); // reachable time and retransmit timer: unspecified
    if let Some(mac) = lladdr {
        p.extend_from_slice(&[OPT_SOURCE_LLADDR, 1]);
        p.extend_from_slice(&mac.octets());
    }
    if let Some(mtu) = cfg.mtu {
        p.extend_from_slice(&[OPT_MTU, 1, 0, 0]);
        p.extend_from_slice(&mtu.to_be_bytes());
    }
    p.extend_from_slice(&[OPT_PREFIX_INFO, 4, cfg.prefix.prefix_len()]);
    p.push(FLAG_ON_LINK | FLAG_AUTONOMOUS);
    p.extend_from_slice(&VALID_LIFETIME.to_be_bytes());
    p.extend_from_slice(&PREFERRED_LIFETIME.to_be_bytes());
    p.extend_from_slice(&[0; 4]);
    p.extend_from_slice(&cfg.prefix.network().octets());
    if !cfg.dns.is_empty() {
        // Length counts 8-byte units: the 8-byte header and 16 bytes per server.
        let units = u8::try_from(1 + 2 * cfg.dns.len()).unwrap_or(u8::MAX);
        p.extend_from_slice(&[OPT_RDNSS, units, 0, 0]);
        p.extend_from_slice(&DNS_LIFETIME.to_be_bytes());
        for a in &cfg.dns {
            p.extend_from_slice(&a.octets());
        }
    }
    if let Some(names) = cfg.domain.as_deref().and_then(domain_wire) {
        let units = u8::try_from(1 + names.len() / 8).unwrap_or(u8::MAX);
        p.extend_from_slice(&[OPT_DNSSL, units, 0, 0]);
        p.extend_from_slice(&DNS_LIFETIME.to_be_bytes());
        p.extend_from_slice(&names);
    }
    p
}

/// A domain name in DNS wire format, zero-padded to a multiple of 8 bytes.
fn domain_wire(domain: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for label in domain.trim_end_matches('.').split('.') {
        let len = u8::try_from(label.len())
            .ok()
            .filter(|l| (1..=63).contains(l))?;
        out.push(len);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.resize(out.len().next_multiple_of(8), 0);
    Some(out)
}

/// Whether `packet` is a router solicitation.
pub fn is_router_solicitation(packet: &[u8]) -> bool {
    packet.first() == Some(&ICMPV6_ROUTER_SOLICITATION) && packet.len() >= 8
}

/// The kernel's interface index, read from sysfs.
async fn ifindex(interface: &str) -> io::Result<u32> {
    let path = format!("/sys/class/net/{interface}/ifindex");
    let text = tokio::fs::read_to_string(&path).await?;
    text.trim()
        .parse()
        .map_err(|e| io::Error::other(format!("{path}: {e}")))
}

/// The bridge's MAC address, or `None` (logged) when sysfs has no usable value. The
/// advertisement then goes out without a source link-layer option.
async fn bridge_mac(interface: &str) -> Option<MacAddr> {
    let path = format!("/sys/class/net/{interface}/address");
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(text) => text,
        Err(e) => {
            tracing::warn!(error = %e, %path, "ra: cannot read bridge MAC");
            return None;
        }
    };
    match text.trim().parse() {
        Ok(mac) => Some(mac),
        Err(e) => {
            tracing::warn!(error = %e, %path, "ra: unparsable bridge MAC");
            None
        }
    }
}

/// An `ICMPv6` raw socket. Tokio's datagram wrapper drives it: raw sockets use the same
/// `recvfrom` and `sendto` calls.
fn open(cfg: &RaConfig, index: u32) -> io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV6, Type::RAW, Some(Protocol::ICMPV6))?;
    s.bind_device(Some(cfg.interface.as_bytes()))?;
    s.set_unicast_hops_v6(ND_HOP_LIMIT)?;
    s.set_multicast_hops_v6(ND_HOP_LIMIT)?;
    s.set_multicast_loop_v6(false)?;
    s.set_nonblocking(true)?;
    s.bind(&SocketAddrV6::new(cfg.source, 0, 0, index).into())?;
    s.join_multicast_v6(&ALL_ROUTERS, index)?;
    UdpSocket::from_std(s.into())
}

async fn bind(shared: &SharedRa) -> Result<UdpSocket> {
    let mut attempt = 1;
    loop {
        let cfg = read(shared);
        let opened = match ifindex(&cfg.interface).await {
            Ok(index) => open(&cfg, index),
            Err(e) => Err(e),
        };
        match opened {
            Ok(s) => return Ok(s),
            Err(e) if attempt >= BIND_ATTEMPTS => {
                return Err(Error::io(format!("ra on {}: bind", cfg.interface), e));
            }
            // The link-local address is tentative until duplicate address detection ends.
            Err(_) => {}
        }
        attempt += 1;
        tokio::time::sleep(BIND_RETRY).await;
    }
}

fn read(shared: &SharedRa) -> RaConfig {
    shared
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

async fn send(sock: &UdpSocket, shared: &SharedRa) -> io::Result<()> {
    let cfg = read(shared);
    let packet = build_advert(&cfg, bridge_mac(&cfg.interface).await);
    let to = SocketAddrV6::new(ALL_NODES, 0, 0, ifindex(&cfg.interface).await?);
    sock.send_to(&packet, to).await.map(drop)
}

/// Advertises on the bridge every [`INTERVAL`] and whenever a guest solicits. It returns an
/// error only when the socket cannot be bound within about 20 seconds; later send and receive
/// errors (the bridge going down, its address changing) are logged and retried. Must be
/// called inside a tokio runtime; the socket belongs to the calling thread's network
/// namespace.
pub async fn run(shared: SharedRa) -> Result<()> {
    let sock = bind(&shared).await?;
    let interface = read(&shared).interface;
    let mut ticker = tokio::time::interval(INTERVAL);
    let mut last: Option<Instant> = None;
    let mut buf = [0u8; 1500];
    loop {
        let due = tokio::select! {
            _ = ticker.tick() => true,
            r = sock.recv_from(&mut buf) => {
                match r {
                    Ok((n, _)) => is_router_solicitation(&buf[..n]),
                    Err(e) => {
                        tracing::warn!(error = %e, %interface, "ra: receive failed");
                        tokio::time::sleep(BIND_RETRY).await;
                        false
                    }
                }
            }
        };
        let paced = last.is_none_or(|t| t.elapsed() >= MIN_GAP);
        if due && paced {
            if let Err(e) = send(&sock, &shared).await {
                tracing::warn!(error = %e, %interface, "ra: send failed");
            }
            last = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RaConfig {
        RaConfig {
            interface: "ps100000".into(),
            source: "fe80::1".parse().unwrap(),
            prefix: "fd12:3456:789a:1::/64".parse().unwrap(),
            router_lifetime: Duration::from_secs(1800),
            mtu: Some(1450),
            dns: vec!["fd12:3456:789a:1::1".parse().unwrap()],
            domain: Some("lab.prism".into()),
        }
    }

    #[test]
    fn advert_layout() {
        let mac: MacAddr = "52:54:00:aa:bb:cc".parse().unwrap();
        let p = build_advert(&cfg(), Some(mac));
        assert_eq!(&p[..4], &[134, 0, 0, 0]);
        assert_eq!(p[4], 64);
        assert_eq!(u16::from_be_bytes([p[6], p[7]]), 1800);
        // Header 16, lladdr 8, mtu 8, prefix 32, rdnss 24, dnssl 8 + 16.
        assert_eq!(p.len(), 16 + 8 + 8 + 32 + 24 + 24);
        assert_eq!(&p[16..24], &[1, 1, 0x52, 0x54, 0, 0xaa, 0xbb, 0xcc]);
        assert_eq!(&p[24..28], &[5, 1, 0, 0]);
        assert_eq!(u32::from_be_bytes(p[28..32].try_into().unwrap()), 1450);
        assert_eq!(&p[32..36], &[3, 4, 64, 0xc0]);
        assert_eq!(&p[48..64], &cfg().prefix.network().octets());
        assert_eq!(&p[64..66], &[25, 3]);
        assert_eq!(&p[88..90], &[31, 3]);
        assert_eq!(&p[96..], b"\x03lab\x05prism\0\0\0\0\0\0");
    }

    #[test]
    fn a_node_that_is_not_a_router_says_so() {
        let mut c = cfg();
        c.router_lifetime = Duration::ZERO;
        c.mtu = None;
        c.dns.clear();
        c.domain = None;
        let p = build_advert(&c, None);
        assert_eq!(&p[6..8], &[0, 0]);
        assert_eq!(p.len(), 16 + 32);
    }

    #[test]
    fn solicitations() {
        assert!(is_router_solicitation(&[133, 0, 0, 0, 0, 0, 0, 0]));
        assert!(!is_router_solicitation(&[133, 0, 0]));
        assert!(!is_router_solicitation(&[134, 0, 0, 0, 0, 0, 0, 0]));
    }

    #[test]
    fn domains_are_padded() {
        assert_eq!(domain_wire("a.prism").unwrap().len() % 8, 0);
        assert!(domain_wire("").is_none());
        assert!(domain_wire(&"x".repeat(64)).is_none());
    }

    #[test]
    fn domain_label_limits() {
        assert!(domain_wire(&"x".repeat(63)).is_some());
        assert!(domain_wire("a..b").is_none());
        assert_eq!(domain_wire("a.b."), domain_wire("a.b"));
    }
}
