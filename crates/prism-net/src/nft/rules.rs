//! The rulesets Prism installs, built from typed configuration.

use std::net::Ipv4Addr;

use ipnet::{Ipv4Net, Ipv6Net};

use super::{Chain, ChainType, CtState, Family, Hook, Rule, Table, priority};
use crate::MacAddr;
use crate::link::VXLAN_PORT;

const ICMPV6_ECHO_REQUEST: u8 = 128;

/// Name of the `inet` filter table.
pub const INET_TABLE: &str = "prism";
/// Name of the `ip` NAT table.
pub const NAT_TABLE: &str = "prism-nat";
/// Name of the `bridge` table.
pub const BRIDGE_TABLE: &str = "prism";

/// Inputs of the mesh boundary rules (see DESIGN.md, "What makes `WireGuard` the boundary").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundaryConfig {
    /// The cluster's ULA /48.
    pub mesh_prefix: Ipv6Net,
    /// The `WireGuard` interface, normally `wg-prism`.
    pub wg_interface: String,
    /// The uid QEMU runs as; it may not open new connections.
    pub qemu_uid: Option<u32>,
}

/// The `inet prism` table: interface rule (prerouting), forward isolation of the mesh and the
/// process rule (output). Guest input rules come from
/// [`guest_input_chain`].
pub fn boundary_ruleset(cfg: &BoundaryConfig) -> Table {
    let mut t = Table::new(Family::Inet, INET_TABLE);
    let wg = cfg.wg_interface.as_str();
    let mesh = cfg.mesh_prefix.trunc();

    let pre = t.chain(Chain::base(
        "prerouting",
        ChainType::Filter,
        Hook::Prerouting,
        priority::RAW,
    ));
    pre.push(
        Rule::new(Family::Inet)
            .iifname_ne(wg)
            .iifname_ne("lo")
            .ip6_saddr(mesh)
            .drop(),
    )
    .push(
        Rule::new(Family::Inet)
            .iifname_ne(wg)
            .iifname_ne("lo")
            .ip6_daddr(mesh)
            .drop(),
    )
    .push(
        Rule::new(Family::Inet)
            .iifname_ne(wg)
            .udp_dport(VXLAN_PORT)
            .drop(),
    );

    t.chain(Chain::base(
        "forward",
        ChainType::Filter,
        Hook::Forward,
        priority::FILTER,
    ))
    .push(Rule::new(Family::Inet).iifname(wg).drop())
    .push(Rule::new(Family::Inet).oifname(wg).drop());

    let out = t.chain(Chain::base(
        "output",
        ChainType::Filter,
        Hook::Output,
        priority::FILTER,
    ));
    if let Some(uid) = cfg.qemu_uid {
        out.push(
            Rule::new(Family::Inet)
                .skuid(uid)
                .ct_state(CtState::NEW)
                .drop(),
        );
    }
    // Echo requests to the mesh are harmless and make `ping6` work for
    // operators; everything else from non-root processes still drops. Only
    // on wg-prism (it is NOARP, so there is no neighbour discovery to allow).
    out.push(
        Rule::new(Family::Inet)
            .oifname(wg)
            .ip6_daddr(mesh)
            .icmpv6_type(ICMPV6_ECHO_REQUEST)
            .accept(),
    );
    out.push(Rule::new(Family::Inet).ip6_daddr(mesh).skuid_ne(0).drop());
    t
}

/// Host services a switch bridge may reach (DHCP, DNS on the VIP, ping).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchServices {
    /// The switch bridge, `ps<id>`.
    pub bridge: String,
    /// The node's gateway VIP on this switch, where DNS listens.
    pub vip: Ipv4Addr,
}

/// An `input` chain for guest-facing bridges: from each bridge only DHCP, DNS to the VIP,
/// ICMP echo, IPv6 neighbour discovery and established traffic are accepted; the rest drops.
/// Add it to the table from [`boundary_ruleset`].
pub fn guest_input_chain(switches: &[SwitchServices]) -> Chain {
    let mut c = Chain::base("input", ChainType::Filter, Hook::Input, priority::FILTER);
    let f = Family::Inet;
    for s in switches {
        let br = s.bridge.as_str();
        let vip = Ipv4Net::from(s.vip);
        c.push(
            Rule::new(f)
                .iifname(br)
                .ct_state(CtState::ESTABLISHED | CtState::RELATED)
                .accept(),
        )
        .push(Rule::new(f).iifname(br).udp_dport(67).accept())
        .push(
            Rule::new(f)
                .iifname(br)
                .ip_daddr(vip)
                .udp_dport(53)
                .accept(),
        )
        .push(
            Rule::new(f)
                .iifname(br)
                .ip_daddr(vip)
                .tcp_dport(53)
                .accept(),
        )
        .push(Rule::new(f).iifname(br).icmp_type(8).accept());
        for nd in 133..=136 {
            c.push(Rule::new(f).iifname(br).icmpv6_type(nd).accept());
        }
        c.push(Rule::new(f).iifname(br).drop());
    }
    c
}

/// Masquerade traffic from `source` leaving through `egress`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NatRule {
    pub source: Ipv4Net,
    pub egress: String,
}

/// A host port forwarded to a guest by destination NAT.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DnatRule {
    /// UDP when set, TCP otherwise.
    pub udp: bool,
    /// Host address to match; any local address when absent.
    pub host_addr: Option<Ipv4Addr>,
    pub host_port: u16,
    pub guest: Ipv4Addr,
    pub guest_port: u16,
}

/// The `ip prism-nat` table: port-forward DNAT in `prerouting`, then in `postrouting` a
/// masquerade for each forward and one per entry in `rules`.
pub fn nat_ruleset(rules: &[NatRule], forwards: &[DnatRule]) -> Table {
    let mut t = Table::new(Family::Ip, NAT_TABLE);
    let pre = t.chain(Chain::base(
        "prerouting",
        ChainType::Nat,
        Hook::Prerouting,
        priority::DSTNAT,
    ));
    for f in forwards {
        let mut r = Rule::new(Family::Ip);
        if let Some(a) = f.host_addr {
            r = r.ip_daddr_eq(a);
        }
        r = if f.udp {
            r.udp_dport(f.host_port)
        } else {
            r.tcp_dport(f.host_port)
        };
        pre.push(r.dnat(f.guest, f.guest_port));
    }
    let c = t.chain(Chain::base(
        "postrouting",
        ChainType::Nat,
        Hook::Postrouting,
        priority::SRCNAT,
    ));
    // A forwarded connection leaves with this node's address as its source. A
    // guest that migrated here still has the old node as its gateway, so
    // without this the replies would return through the wrong node.
    let mut seen = std::collections::HashSet::new();
    for f in forwards {
        if !seen.insert((f.udp, f.guest, f.guest_port)) {
            continue;
        }
        let r = Rule::new(Family::Ip).ip_daddr_eq(f.guest);
        c.push(
            if f.udp {
                r.udp_dport(f.guest_port)
            } else {
                r.tcp_dport(f.guest_port)
            }
            .masquerade(),
        );
    }
    for r in rules {
        c.push(
            Rule::new(Family::Ip)
                .oifname(&r.egress)
                .ip_saddr(r.source.trunc())
                .masquerade(),
        );
    }
    t
}

/// A guest's tap on a switch bridge and the addresses it may use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestPort {
    /// The tap, `pn<nicid>`.
    pub tap: String,
    pub mac: MacAddr,
    /// The NIC's IPAM lease; `None` allows no IPv4 source other than `0.0.0.0`.
    pub ipv4: Option<Ipv4Addr>,
}

const ETH_P_8021Q: u16 = 0x8100;
const ETH_P_8021AD: u16 = 0x88a8;

/// The `bridge prism` table: anti-spoofing per guest tap (ether source, IPv4 and ARP source,
/// `0.0.0.0` allowed for DHCPDISCOVER), no rogue DHCP servers or router advertisements, no
/// VLAN-tagged ingress.
pub fn bridge_guard_ruleset(ports: &[GuestPort]) -> Table {
    let f = Family::Bridge;
    let mut t = Table::new(f, BRIDGE_TABLE);
    let c = t.chain(Chain::base(
        "prerouting",
        ChainType::Filter,
        Hook::Prerouting,
        priority::BRIDGE_FILTER,
    ));
    for p in ports {
        let tap = p.tap.as_str();
        let ip = p.ipv4.unwrap_or(Ipv4Addr::UNSPECIFIED);
        c.push(Rule::new(f).iifname(tap).ether_saddr_ne(p.mac).drop())
            .push(
                Rule::new(f)
                    .iifname(tap)
                    .ip_saddr_ne(ip)
                    .ip_saddr_ne(Ipv4Addr::UNSPECIFIED)
                    .drop(),
            )
            .push(Rule::new(f).iifname(tap).arp_saddr_ip_ne(ip).drop())
            .push(Rule::new(f).iifname(tap).udp_sport(67).drop())
            .push(Rule::new(f).iifname(tap).icmpv6_type(134).drop())
            .push(Rule::new(f).iifname(tap).ether_type(ETH_P_8021Q).drop())
            .push(Rule::new(f).iifname(tap).ether_type(ETH_P_8021AD).drop());
    }
    t
}
