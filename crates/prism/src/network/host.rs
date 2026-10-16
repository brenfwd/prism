//! What this host's routing table says: the uplink to masquerade out of
//! and the subnets guests must not reuse.

use std::net::Ipv4Addr;

use ipnet::Ipv4Net;

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
    write_sysctl("/proc/sys/net/ipv4/ip_forward", "IPv4 forwarding");
}

/// Turns IPv6 forwarding on, which IPv6 port forwards need.
pub fn enable_forwarding6() {
    write_sysctl("/proc/sys/net/ipv6/conf/all/forwarding", "IPv6 forwarding");
}

/// Lets `bridge` route packets that came from loopback, so a forward from
/// `127.0.0.1` can reach a guest.
pub fn enable_route_localnet(bridge: &str) {
    write_sysctl(
        &format!("/proc/sys/net/ipv4/conf/{bridge}/route_localnet"),
        "route_localnet",
    );
}

fn write_sysctl(path: &str, what: &str) {
    if std::fs::read_to_string(path).is_ok_and(|v| v.trim() == "1") {
        return;
    }
    if let Err(e) = std::fs::write(path, "1") {
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

    #[test]
    fn parses_the_table() {
        let r = parse_routes(TABLE);
        assert_eq!(r.len(), 3);
        assert_eq!(uplink_of(&r).as_deref(), Some("eth0"));
        assert_eq!(foreign_subnets(&r), vec!["192.168.1.0/24".parse().unwrap()]);
    }
}
