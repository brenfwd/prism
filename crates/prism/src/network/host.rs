//! What this host's routing table says: the uplink to masquerade out of
//! and the subnets guests must not reuse.

use std::net::{Ipv4Addr, Ipv6Addr};

use ipnet::{Ipv4Net, Ipv6Net};

/// One IPv4 route from `/proc/net/route`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub iface: String,
    pub dest: Ipv4Net,
    pub metric: u32,
}

fn hex_addr(s: &str) -> Option<Ipv4Addr> {
    // The kernel prints the address as a native-endian (little-endian) u32.
    u32::from_str_radix(s, 16)
        .ok()
        .map(|v| Ipv4Addr::from(v.swap_bytes()))
}

pub fn parse_routes(text: &str) -> Vec<Route> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 8 {
                return None;
            }
            let dest = hex_addr(f[1])?;
            let mask = hex_addr(f[7])?;
            let prefix = u32::from(mask).leading_ones() as u8;
            Some(Route {
                iface: f[0].to_string(),
                dest: Ipv4Net::new(dest, prefix).ok()?,
                metric: f[6].parse().ok()?,
            })
        })
        .collect()
}

pub fn routes() -> Vec<Route> {
    std::fs::read_to_string("/proc/net/route")
        .map(|t| parse_routes(&t))
        .unwrap_or_default()
}

/// The interface of the best default route.
pub fn uplink_of(routes: &[Route]) -> Option<String> {
    routes
        .iter()
        .filter(|r| r.dest.prefix_len() == 0)
        .min_by_key(|r| r.metric)
        .map(|r| r.iface.clone())
}

/// Routed subnets that are not Prism's own and not the default route.
pub fn foreign_subnets(routes: &[Route]) -> Vec<Ipv4Net> {
    routes
        .iter()
        .filter(|r| r.dest.prefix_len() > 0 && super::vni_of(&r.iface).is_none())
        .map(|r| r.dest)
        .collect()
}

/// Turns IPv4 forwarding on, which masquerade needs.
pub fn enable_forwarding() {
    write_sysctl("/proc/sys/net/ipv4/ip_forward", "1", "IPv4 forwarding");
}

/// One IPv6 route from `/proc/net/ipv6_route`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route6 {
    pub iface: String,
    pub dest: Ipv6Net,
    pub metric: u32,
}

fn parse_hex_addr6(s: &str) -> Option<Ipv6Addr> {
    u128::from_str_radix(s, 16).ok().map(Ipv6Addr::from)
}

pub fn parse_routes6(text: &str) -> Vec<Route6> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 10 {
                return None;
            }
            let prefix = u8::from_str_radix(f[1], 16).ok()?;
            Some(Route6 {
                iface: f[9].to_string(),
                dest: Ipv6Net::new(parse_hex_addr6(f[0])?, prefix).ok()?,
                metric: u32::from_str_radix(f[5], 16).ok()?,
            })
        })
        .collect()
}

pub fn routes6() -> Vec<Route6> {
    std::fs::read_to_string("/proc/net/ipv6_route")
        .map(|t| parse_routes6(&t))
        .unwrap_or_default()
}

/// The interface of the best IPv6 default route, if the host has one.
pub fn uplink6_of(routes: &[Route6]) -> Option<String> {
    routes
        .iter()
        .filter(|r| r.dest.prefix_len() == 0 && r.iface != "lo")
        .min_by_key(|r| r.metric)
        .map(|r| r.iface.clone())
}

/// IPv6 routes that are not Prism's own, not the default route, not link-local, multicast
/// or loopback: prefixes a guest network must not reuse.
pub fn foreign_subnets6(routes: &[Route6]) -> Vec<Ipv6Net> {
    let skip: [Ipv6Net; 3] = [
        "fe80::/10".parse().expect("literal"),
        "ff00::/8".parse().expect("literal"),
        "::1/128".parse().expect("literal"),
    ];
    routes
        .iter()
        .filter(|r| {
            r.dest.prefix_len() > 0
                && super::vni_of(&r.iface).is_none()
                && !skip.iter().any(|s| s.contains(&r.dest.network()))
        })
        .map(|r| r.dest)
        .collect()
}

/// Turns IPv6 forwarding on, which IPv6 port forwards and NAT66 need. Turning it on makes
/// the kernel ignore router advertisements, which would drop the host's own default route,
/// so `uplink` first gets `accept_ra=2` (accept even when forwarding).
pub fn enable_forwarding6(uplink: Option<&str>) {
    if let Some(u) = uplink {
        write_sysctl(
            &format!("/proc/sys/net/ipv6/conf/{u}/accept_ra"),
            "2",
            "accept_ra",
        );
    }
    write_sysctl(
        "/proc/sys/net/ipv6/conf/all/forwarding",
        "1",
        "IPv6 forwarding",
    );
}

/// Stops the host from configuring itself from router advertisements on `bridge`. The
/// bridge carries Prism's own addresses; other nodes' advertisements must not add more.
pub fn lock_bridge6(bridge: &str) {
    for knob in ["accept_ra", "autoconf"] {
        write_sysctl(
            &format!("/proc/sys/net/ipv6/conf/{bridge}/{knob}"),
            "0",
            knob,
        );
    }
}

/// Lets `bridge` route packets that came from loopback, so a forward from
/// `127.0.0.1` can reach a guest.
pub fn enable_route_localnet(bridge: &str) {
    write_sysctl(
        &format!("/proc/sys/net/ipv4/conf/{bridge}/route_localnet"),
        "1",
        "route_localnet",
    );
}

fn write_sysctl(path: &str, value: &str, what: &str) {
    if std::fs::read_to_string(path).is_ok_and(|v| v.trim() == value) {
        return;
    }
    if let Err(e) = std::fs::write(path, value) {
        tracing::warn!("enabling {what}: {e}");
    }
}

/// DNS servers from `/etc/resolv.conf`, skipping `skip` (a switch VIP).
pub fn nameservers(skip: &[Ipv4Addr]) -> Vec<std::net::SocketAddr> {
    let text = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
    let mut out: Vec<std::net::SocketAddr> = text
        .lines()
        .filter_map(|l| l.strip_prefix("nameserver"))
        .filter_map(|a| a.trim().parse::<Ipv4Addr>().ok())
        .filter(|a| !skip.contains(a))
        .map(|a| std::net::SocketAddr::from((a, 53)))
        .collect();
    if out.is_empty() {
        out.push(std::net::SocketAddr::from(([1, 1, 1, 1], 53)));
        out.push(std::net::SocketAddr::from(([9, 9, 9, 9], 53)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str =
        "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0
eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0
ps100000\t0058580A\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0
";

    const TABLE6: &str = "\
fd000000000000000000000000000000 08 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 eth0
00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000001 00000000 00000003 eth0
fe800000000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 eth0
fd010203040500070000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 ps100000
";

    #[test]
    fn parses_the_ipv6_table() {
        let r = parse_routes6(TABLE6);
        assert_eq!(r.len(), 4);
        assert_eq!(uplink6_of(&r).as_deref(), Some("eth0"));
        assert_eq!(foreign_subnets6(&r), vec!["fd00::/8".parse().unwrap()]);
    }

    #[test]
    fn parses_the_table() {
        let r = parse_routes(TABLE);
        assert_eq!(r.len(), 3);
        assert_eq!(uplink_of(&r).as_deref(), Some("eth0"));
        assert_eq!(foreign_subnets(&r), vec!["192.168.1.0/24".parse().unwrap()]);
    }
}
