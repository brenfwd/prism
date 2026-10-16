//! The `bridge prism` table: what a guest tap may send.

use std::net::{Ipv4Addr, Ipv6Addr};

use ipnet::Ipv6Net;

use super::rules::BRIDGE_TABLE;
use super::{Chain, ChainType, Family, Hook, Rule, Table, priority};
use crate::MacAddr;

const CHAIN_PREROUTING: &str = "prerouting";

/// `fe80::/10`, the `IPv6` link-local range.
const LINK_LOCAL_V6: Ipv6Net = Ipv6Net::new_assert(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10);

const ICMPV6_ROUTER_ADVERT: u8 = 134;
const ICMPV6_NEIGHBOR_SOLICIT: u8 = 135;
const ICMPV6_NEIGHBOR_ADVERT: u8 = 136;
const ICMPV6_REDIRECT: u8 = 137;
/// Link-layer address option types of neighbour discovery.
const OPT_SOURCE_LLADDR: u8 = 1;
const OPT_TARGET_LLADDR: u8 = 2;
/// The source ports of DHCP servers.
const DHCP4_SERVER_PORT: u16 = 67;
const DHCP6_SERVER_PORT: u16 = 547;
const ETH_P_8021Q: u16 = 0x8100;
const ETH_P_8021AD: u16 = 0x88a8;

/// The `IPv6` addresses a guest may send from.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Ipv6Source {
    /// No `IPv6` checks: the network does not hand out addresses.
    #[default]
    Any,
    /// Only the NIC's own addresses in `prefix`, link-local addresses and `::`.
    Only {
        /// The NIC's own addresses: its SLAAC address and any it was seen using.
        addrs: Vec<Ipv6Addr>,
        /// The network the addresses belong to; other addresses in it may not be claimed.
        prefix: Ipv6Net,
        /// Addresses of the network's routers (the nodes). No guest may send neighbour
        /// messages from them or claim them.
        gateways: Vec<Ipv6Addr>,
    },
}

/// A guest's tap on a switch bridge and the addresses it may use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestPort {
    /// The tap, `pn<nicid>`.
    pub tap: String,
    /// The NIC's MAC address; frames from any other source MAC are dropped.
    pub mac: MacAddr,
    /// The NIC's IPAM lease; `None` allows no IPv4 source other than `0.0.0.0`.
    pub ipv4: Option<Ipv4Addr>,
    /// Which `IPv6` sources the guest may use.
    pub ipv6: Ipv6Source,
    /// A trunk NIC may send VLAN-tagged frames; their sources are checked
    /// like untagged ones. Other NICs may not send tagged frames at all.
    pub trunk: bool,
}

/// The `IPv6` rules of one guest tap: the source address is one of the NIC's own, a link-local
/// address or `::`; neighbour messages may not claim another address of the network or name
/// another MAC; redirects are dropped. Neighbour solicitations and advertisements from the
/// network's prefix pass whatever their source, so the host can see which addresses the
/// guest uses; the checks before them have already tied those messages to the guest's MAC.
fn guard_ipv6(
    c: &mut Chain,
    port: &GuestPort,
    addrs: &[Ipv6Addr],
    prefix: Ipv6Net,
    gateways: &[Ipv6Addr],
) {
    let f = Family::Bridge;
    let tap = port.tap.as_str();
    let mut claim = Rule::new(f)
        .iifname(tap)
        .icmpv6_type(ICMPV6_NEIGHBOR_ADVERT)
        .icmpv6_target_in(prefix);
    for a in addrs {
        claim = claim.icmpv6_target_ne(*a);
    }
    c.push(claim.drop());
    for g in gateways.iter().filter(|g| !addrs.contains(g)) {
        // Gateway addresses include link-local ones, which the prefix check above misses.
        c.push(
            Rule::new(f)
                .iifname(tap)
                .icmpv6_type(ICMPV6_NEIGHBOR_ADVERT)
                .icmpv6_target_eq(*g)
                .drop(),
        );
        for kind in [ICMPV6_NEIGHBOR_SOLICIT, ICMPV6_NEIGHBOR_ADVERT] {
            c.push(
                Rule::new(f)
                    .iifname(tap)
                    .ip6_saddr(Ipv6Net::from(*g))
                    .icmpv6_type(kind)
                    .drop(),
            );
        }
    }
    // Duplicate address detection comes from `::`, carries no link-layer option and may
    // carry a nonce option, so it passes before the option checks.
    c.push(
        Rule::new(f)
            .iifname(tap)
            .ip6_saddr(Ipv6Net::from(Ipv6Addr::UNSPECIFIED))
            .icmpv6_type(ICMPV6_NEIGHBOR_SOLICIT)
            .accept(),
    );
    for (kind, option) in [
        (ICMPV6_NEIGHBOR_SOLICIT, OPT_SOURCE_LLADDR),
        (ICMPV6_NEIGHBOR_ADVERT, OPT_TARGET_LLADDR),
    ] {
        c.push(
            Rule::new(f)
                .iifname(tap)
                .icmpv6_type(kind)
                .icmpv6_first_option_ne(option)
                .drop(),
        );
    }
    c.push(
        Rule::new(f)
            .iifname(tap)
            .icmpv6_type(ICMPV6_NEIGHBOR_SOLICIT)
            .icmpv6_lladdr_ne(OPT_SOURCE_LLADDR, port.mac)
            .drop(),
    )
    .push(
        Rule::new(f)
            .iifname(tap)
            .icmpv6_type(ICMPV6_NEIGHBOR_ADVERT)
            .icmpv6_lladdr_ne(OPT_TARGET_LLADDR, port.mac)
            .drop(),
    )
    .push(
        Rule::new(f)
            .iifname(tap)
            .icmpv6_type(ICMPV6_REDIRECT)
            .drop(),
    );
    for kind in [ICMPV6_NEIGHBOR_SOLICIT, ICMPV6_NEIGHBOR_ADVERT] {
        c.push(
            Rule::new(f)
                .iifname(tap)
                .ip6_saddr(prefix)
                .icmpv6_type(kind)
                .accept(),
        );
    }
    let mut src = Rule::new(f).iifname(tap);
    for a in addrs {
        src = src.ip6_saddr_ne(*a);
    }
    c.push(
        src.ip6_saddr_ne(Ipv6Addr::UNSPECIFIED)
            .ip6_saddr_outside(LINK_LOCAL_V6)
            .drop(),
    );
}

/// The `IPv6` rules of a trunk tap for VLAN-tagged frames: the same claim checks, gateway
/// checks, option checks, redirect drop and source check as [`guard_ipv6`]. Learning is
/// untagged only.
fn guard_ipv6_tagged(
    c: &mut Chain,
    port: &GuestPort,
    addrs: &[Ipv6Addr],
    prefix: Ipv6Net,
    gateways: &[Ipv6Addr],
) {
    let f = Family::Bridge;
    let tap = port.tap.as_str();
    let mut claim = Rule::new(f)
        .iifname(tap)
        .tagged_icmpv6_type(ICMPV6_NEIGHBOR_ADVERT)
        .tagged_icmpv6_target_in(prefix);
    for a in addrs {
        claim = claim.tagged_icmpv6_target_ne(*a);
    }
    c.push(claim.drop());
    for g in gateways.iter().filter(|g| !addrs.contains(g)) {
        c.push(
            Rule::new(f)
                .iifname(tap)
                .tagged_icmpv6_type(ICMPV6_NEIGHBOR_ADVERT)
                .tagged_icmpv6_target_eq(*g)
                .drop(),
        );
        for kind in [ICMPV6_NEIGHBOR_SOLICIT, ICMPV6_NEIGHBOR_ADVERT] {
            c.push(
                Rule::new(f)
                    .iifname(tap)
                    .tagged_ip6_saddr_eq(*g)
                    .tagged_icmpv6_type(kind)
                    .drop(),
            );
        }
    }
    c.push(
        Rule::new(f)
            .iifname(tap)
            .tagged_ip6_saddr_eq(Ipv6Addr::UNSPECIFIED)
            .tagged_icmpv6_type(ICMPV6_NEIGHBOR_SOLICIT)
            .accept(),
    );
    for (kind, option) in [
        (ICMPV6_NEIGHBOR_SOLICIT, OPT_SOURCE_LLADDR),
        (ICMPV6_NEIGHBOR_ADVERT, OPT_TARGET_LLADDR),
    ] {
        c.push(
            Rule::new(f)
                .iifname(tap)
                .tagged_icmpv6_type(kind)
                .tagged_icmpv6_first_option_ne(option)
                .drop(),
        )
        .push(
            Rule::new(f)
                .iifname(tap)
                .tagged_icmpv6_type(kind)
                .tagged_icmpv6_lladdr_ne(option, port.mac)
                .drop(),
        );
    }
    c.push(
        Rule::new(f)
            .iifname(tap)
            .tagged_icmpv6_type(ICMPV6_REDIRECT)
            .drop(),
    );
    let mut src = Rule::new(f).iifname(tap);
    for a in addrs {
        src = src.tagged_ip6_saddr_ne(*a);
    }
    c.push(
        src.tagged_ip6_saddr_ne(Ipv6Addr::UNSPECIFIED)
            .tagged_ip6_saddr_outside(LINK_LOCAL_V6)
            .drop(),
    );
}

/// The `bridge prism` table: anti-spoofing per guest tap (ether source, IPv4 and ARP source,
/// `0.0.0.0` allowed for DHCPDISCOVER, `IPv6` source and neighbour messages on networks
/// with `IPv6`), no rogue DHCP servers (v4 or v6) or router advertisements, no VLAN-tagged
/// ingress except on trunks, whose tagged frames get the same checks.
pub fn bridge_guard_ruleset(ports: &[GuestPort]) -> Table {
    let f = Family::Bridge;
    let mut t = Table::new(f, BRIDGE_TABLE);
    let c = t.chain(Chain::base(
        CHAIN_PREROUTING,
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
            .push(
                Rule::new(f)
                    .iifname(tap)
                    .udp_sport(DHCP4_SERVER_PORT)
                    .drop(),
            )
            .push(
                Rule::new(f)
                    .iifname(tap)
                    .udp_sport(DHCP6_SERVER_PORT)
                    .drop(),
            )
            .push(
                Rule::new(f)
                    .iifname(tap)
                    .icmpv6_type(ICMPV6_ROUTER_ADVERT)
                    .drop(),
            )
            .push(Rule::new(f).iifname(tap).ether_type(ETH_P_8021AD).drop());
        if let Ipv6Source::Only {
            addrs,
            prefix,
            gateways,
        } = &p.ipv6
        {
            guard_ipv6(c, p, addrs, *prefix, gateways);
        }
        if p.trunk {
            c.push(
                Rule::new(f)
                    .iifname(tap)
                    .tagged_ip_saddr_ne(ip)
                    .tagged_ip_saddr_ne(Ipv4Addr::UNSPECIFIED)
                    .drop(),
            )
            .push(Rule::new(f).iifname(tap).tagged_arp_saddr_ip_ne(ip).drop())
            .push(
                Rule::new(f)
                    .iifname(tap)
                    .tagged_udp_sport_eq(DHCP4_SERVER_PORT)
                    .drop(),
            )
            .push(
                Rule::new(f)
                    .iifname(tap)
                    .tagged_udp6_sport_eq(DHCP6_SERVER_PORT)
                    .drop(),
            )
            .push(
                Rule::new(f)
                    .iifname(tap)
                    .tagged_icmpv6_type(ICMPV6_ROUTER_ADVERT)
                    .drop(),
            );
            if let Ipv6Source::Only {
                addrs,
                prefix,
                gateways,
            } = &p.ipv6
            {
                guard_ipv6_tagged(c, p, addrs, *prefix, gateways);
            }
        } else {
            c.push(Rule::new(f).iifname(tap).ether_type(ETH_P_8021Q).drop());
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(trunk: bool, ipv6: Ipv6Source) -> GuestPort {
        GuestPort {
            tap: "pn1".into(),
            mac: "52:54:00:00:00:01".parse().unwrap(),
            ipv4: None,
            ipv6,
            trunk,
        }
    }

    fn only(addrs: &[&str]) -> Ipv6Source {
        Ipv6Source::Only {
            addrs: addrs.iter().map(|a| a.parse().unwrap()).collect(),
            prefix: "fd00:1::/64".parse().unwrap(),
            gateways: vec!["fd00:1::ffff".parse().unwrap()],
        }
    }

    fn rules(p: &GuestPort) -> usize {
        bridge_guard_ruleset(std::slice::from_ref(p)).chains[0]
            .rules
            .len()
    }

    #[test]
    fn untagged_port_without_ipv6_has_the_base_rules() {
        // mac, ip, arp, dhcp4, dhcp6, ra, 8021ad, 8021q
        assert_eq!(rules(&port(false, Ipv6Source::Any)), 8);
    }

    #[test]
    fn ipv6_adds_claim_solicit_advert_redirect_learn_and_source_rules() {
        // claim, 2 lladdr, redirect, 2 learning accepts, source, then per gateway (one in
        // the test) a target claim and 2 source drops, a DAD accept and 2 option checks
        assert_eq!(rules(&port(false, only(&["fd00:1::1"]))), 8 + 7 + 6);
    }

    #[test]
    fn trunk_gets_tagged_ipv4_and_ipv6_rules() {
        // base 7 (no 8021q drop) + ipv4 tagged 5 + ipv6 untagged 13 + ipv6 tagged 11
        assert_eq!(rules(&port(true, only(&["fd00:1::1"]))), 7 + 5 + 13 + 11);
        assert_eq!(rules(&port(true, Ipv6Source::Any)), 7 + 5);
    }

    #[test]
    fn more_addresses_do_not_add_rules() {
        assert_eq!(
            rules(&port(false, only(&["fd00:1::1"]))),
            rules(&port(false, only(&["fd00:1::1", "fd00:1::2", "fd00:1::3"])))
        );
    }
}
