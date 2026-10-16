//! The rulesets Prism installs, built from typed configuration.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ipnet::{Ipv4Net, Ipv6Net};

use super::{Chain, ChainType, CtState, Family, Hook, Rule, Table, priority};
use crate::link::VXLAN_PORT;

const CHAIN_PREROUTING: &str = "prerouting";
const CHAIN_FORWARD: &str = "forward";
const CHAIN_INPUT: &str = "input";
const CHAIN_OUTPUT: &str = "output";
const CHAIN_POSTROUTING: &str = "postrouting";

/// `127.0.0.0/8`, the destinations a host-local forward matches when no host address is set.
const LOOPBACK_V4: Ipv4Net = Ipv4Net::new_assert(Ipv4Addr::new(127, 0, 0, 0), 8);

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
        CHAIN_PREROUTING,
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
        CHAIN_FORWARD,
        ChainType::Filter,
        Hook::Forward,
        priority::FILTER,
    ))
    .push(Rule::new(Family::Inet).iifname(wg).drop())
    .push(Rule::new(Family::Inet).oifname(wg).drop());

    let out = t.chain(Chain::base(
        CHAIN_OUTPUT,
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
    /// The node's `IPv6` address on the switch, where DNS also listens.
    pub vip6: Option<Ipv6Addr>,
}

/// An `input` chain for guest-facing bridges: from each bridge only DHCP, DNS to the VIP,
/// ICMP echo, IPv6 neighbour discovery and established traffic are accepted; the rest drops.
/// Add it to the table from [`boundary_ruleset`].
pub fn guest_input_chain(switches: &[SwitchServices]) -> Chain {
    let mut c = Chain::base(
        CHAIN_INPUT,
        ChainType::Filter,
        Hook::Input,
        priority::FILTER,
    );
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
        if let Some(vip6) = s.vip6 {
            let vip6 = Ipv6Net::from(vip6);
            c.push(
                Rule::new(f)
                    .iifname(br)
                    .ip6_daddr(vip6)
                    .udp_dport(53)
                    .accept(),
            )
            .push(
                Rule::new(f)
                    .iifname(br)
                    .ip6_daddr(vip6)
                    .tcp_dport(53)
                    .accept(),
            )
            .push(
                Rule::new(f)
                    .iifname(br)
                    .icmpv6_type(ICMPV6_ECHO_REQUEST)
                    .accept(),
            );
        }
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
    /// Guest source network to masquerade.
    pub source: Ipv4Net,
    /// Outgoing interface.
    pub egress: String,
}

/// NAT66: masquerade `IPv6` traffic from `source` leaving through `egress`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nat6Rule {
    /// Guest source network to masquerade.
    pub source: Ipv6Net,
    /// Outgoing interface.
    pub egress: String,
}

/// A host port forwarded to a guest by destination NAT.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DnatRule {
    /// UDP when set, TCP otherwise.
    pub udp: bool,
    /// Host address to match; any local address when absent.
    pub host_addr: Option<IpAddr>,
    /// Port on the host.
    pub host_port: u16,
    /// The guest's address.
    pub guest: IpAddr,
    /// Port on the guest.
    pub guest_port: u16,
    /// The guest's bridge. Hairpin connections are accepted only from it.
    pub bridge: String,
    /// The host's uplink interfaces for the guest's address family. External
    /// connections are forwarded only when they arrive on one of these.
    pub uplinks: Vec<String>,
    /// Also forward connections the host itself opens (to loopback, or to
    /// `host_addr`).
    pub local: bool,
    /// Also forward connections from guests on the same bridge.
    pub hairpin: bool,
    /// Source address for forwarded IPv6 connections; the IPv6 guest cannot
    /// be reached from the global source `masquerade` would pick.
    pub snat6: Option<Ipv6Addr>,
}

impl DnatRule {
    /// The prerouting rules: one per uplink for external clients, plus one
    /// for the guest's own bridge when hairpin is on. With no uplink, a
    /// forward bound to an explicit host address is still matched on any
    fn prerouting(&self, family: Family) -> Vec<Rule> {
        let dnat = |r: Rule| self.matches(r).dnat(self.guest, self.guest_port);
        let mut rules: Vec<Rule> = self
            .uplinks
            .iter()
            .map(|u| dnat(Rule::new(family).iifname(u)))
            .collect();
        if self.hairpin {
            rules.push(dnat(Rule::new(family).iifname(&self.bridge)));
        }
        rules
    }

    fn matches(&self, rule: Rule) -> Rule {
        let rule = match self.host_addr {
            Some(IpAddr::V4(a)) => rule.ip_daddr_eq(a),
            Some(IpAddr::V6(a)) => rule.ip6_daddr(Ipv6Net::from(a)),
            // Without an address the forward is for this host only, not for
            // transit traffic or a guest's connections to other hosts.
            None => rule.daddr_local(),
        };
        if self.udp {
            rule.udp_dport(self.host_port)
        } else {
            rule.tcp_dport(self.host_port)
        }
    }

    fn to_guest(&self, rule: Rule) -> Rule {
        let rule = match self.guest {
            IpAddr::V4(a) => rule.ip_daddr_eq(a),
            IpAddr::V6(a) => rule.ip6_daddr(Ipv6Net::from(a)),
        };
        if self.udp {
            rule.udp_dport(self.guest_port)
        } else {
            rule.tcp_dport(self.guest_port)
        }
    }

    /// Accepts a hairpinned connection, which enters and leaves the same bridge.
    pub fn hairpin_rule(&self, family: Family) -> Option<Rule> {
        self.hairpin.then(|| {
            let r = Rule::new(family)
                .iifname(&self.bridge)
                .oifname(&self.bridge);
            self.to_guest(r).accept()
        })
    }
}

/// The `ip prism-nat` table: port-forward DNAT in `prerouting` (and `output`
/// for the host's own connections), then in `postrouting` a masquerade for
/// each forward and one per entry in `rules`.
pub fn nat_ruleset(rules: &[NatRule], forwards: &[DnatRule]) -> Table {
    let mut t = Table::new(Family::Ip, NAT_TABLE);
    let c = forward_chains(&mut t, forwards.iter().filter(|f| f.guest.is_ipv4()));
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

/// The `ip6 prism-nat` table: the same forwards for IPv6 guests, and a masquerade for each
/// entry in `rules`.
pub fn nat6_ruleset(rules: &[Nat6Rule], forwards: &[DnatRule]) -> Table {
    let mut t = Table::new(Family::Ip6, NAT_TABLE);
    let c = forward_chains(&mut t, forwards.iter().filter(|f| f.guest.is_ipv6()));
    for r in rules {
        c.push(
            Rule::new(Family::Ip6)
                .oifname(&r.egress)
                .ip6_saddr(r.source.trunc())
                .masquerade(),
        );
    }
    t
}

/// Adds the DNAT chains for `forwards` to `t` and returns the `postrouting` chain, which
/// already holds the per-forward masquerades.
fn forward_chains<'t, 'a>(
    t: &'t mut Table,
    forwards: impl Iterator<Item = &'a DnatRule> + Clone,
) -> &'t mut Chain {
    let family = t.family;
    let pre = t.chain(Chain::base(
        CHAIN_PREROUTING,
        ChainType::Nat,
        Hook::Prerouting,
        priority::DSTNAT,
    ));
    for f in forwards.clone() {
        for rule in f.prerouting(family) {
            pre.push(rule);
        }
    }
    if forwards.clone().any(|f| f.local) {
        let out = t.chain(Chain::base(
            CHAIN_OUTPUT,
            ChainType::Nat,
            Hook::Output,
            priority::DSTNAT,
        ));
        for f in forwards.clone().filter(|f| f.local) {
            // Without an address a forward means loopback here: the host's
            // other addresses are reachable without a forward.
            let rule = Rule::new(family);
            let rule = match (f.host_addr, f.guest) {
                (None, IpAddr::V4(_)) => rule.ip_daddr(LOOPBACK_V4),
                // The kernel drops replies to `::1` that arrive on another
                // interface, so IPv6 has no loopback forward.
                (None, IpAddr::V6(_)) => continue,
                _ => rule,
            };
            out.push(f.matches(rule).dnat(f.guest, f.guest_port));
        }
    }
    let c = t.chain(Chain::base(
        CHAIN_POSTROUTING,
        ChainType::Nat,
        Hook::Postrouting,
        priority::SRCNAT,
    ));
    // A forwarded connection leaves with this node's address as its source. A
    // guest that migrated here still has the old node as its gateway, so
    // without this the replies would return through the wrong node. It also
    // makes hairpinned and loopback connections return through the host.
    let mut seen = std::collections::HashSet::new();
    for f in forwards {
        if seen.insert((f.udp, f.guest, f.guest_port)) {
            let rule = f.to_guest(Rule::new(family));
            c.push(match f.snat6 {
                Some(a) if f.guest.is_ipv6() => rule.snat(IpAddr::V6(a)),
                _ => rule.masquerade(),
            });
        }
    }
    c
}

#[cfg(test)]
mod dnat_tests {
    use super::*;

    fn rule(uplinks: &[&str], hairpin: bool) -> DnatRule {
        DnatRule {
            udp: false,
            host_addr: None,
            host_port: 80,
            guest: "10.0.0.5".parse().unwrap(),
            guest_port: 80,
            bridge: "ps1".into(),
            uplinks: uplinks.iter().map(|s| (*s).into()).collect(),
            local: false,
            hairpin,
            snat6: None,
        }
    }

    #[test]
    fn one_prerouting_rule_per_uplink() {
        assert_eq!(
            rule(&["eth0", "eth1"], false).prerouting(Family::Ip).len(),
            2
        );
    }

    #[test]
    fn a_forward_without_an_address_matches_local_destinations_only() {
        let r = rule(&["eth0"], true);
        for rule in r.prerouting(Family::Ip) {
            assert!(rule.exprs().contains(&crate::nft::Expr::FibDaddrType));
        }
        let mut bound = r;
        bound.host_addr = Some("192.0.2.1".parse().unwrap());
        for rule in bound.prerouting(Family::Ip) {
            assert!(!rule.exprs().contains(&crate::nft::Expr::FibDaddrType));
        }
    }

    #[test]
    fn no_uplink_and_no_hairpin_forwards_nothing() {
        assert_eq!(rule(&[], false).prerouting(Family::Ip).len(), 0);
    }

    #[test]
    fn hairpin_adds_a_rule_for_the_own_bridge_only() {
        assert_eq!(rule(&["eth0"], true).prerouting(Family::Ip).len(), 2);
        assert_eq!(rule(&[], true).prerouting(Family::Ip).len(), 1);
    }
}
