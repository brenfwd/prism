//! [`Rule`]: a typed rule builder that adds the implicit protocol
//! dependencies the `nft` CLI adds.

use std::net::{Ipv4Addr, Ipv6Addr};

use ipnet::{Ipv4Net, Ipv6Net};

use super::{CmpOp, CtState, Expr, Family, MetaKey, PayloadBase, Verdict};
use crate::MacAddr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Dep {
    Ipv4,
    Ipv6,
    Arp,
    Icmpv6Bridge,
    L4(u8),
}

const IPPROTO_ICMP: u8 = 1;
const IPPROTO_TCP: u8 = 6;
const IPPROTO_UDP: u8 = 17;
const IPPROTO_ICMPV6: u8 = 58;
const ETH_P_8021Q: u16 = 0x8100;
const ETH_P_IP: u16 = 0x0800;
const ETH_P_ARP: u16 = 0x0806;
const ETH_P_IPV6: u16 = 0x86dd;

/// A rule: a list of expressions built with the typed matchers below.
#[must_use]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    family: Family,
    exprs: Vec<Expr>,
    deps: Vec<Dep>,
}

impl Rule {
    /// An empty rule for `family`. Matches added with the builder methods also insert the
    /// protocol checks the `nft` CLI would.
    pub fn new(family: Family) -> Self {
        Self {
            family,
            exprs: Vec::new(),
            deps: Vec::new(),
        }
    }

    /// The expressions in evaluation order, including inserted protocol checks.
    pub fn exprs(&self) -> &[Expr] {
        &self.exprs
    }

    /// Appends a raw expression.
    pub fn expr(mut self, e: Expr) -> Self {
        self.exprs.push(e);
        self
    }

    fn cmp(self, op: CmpOp, data: Vec<u8>) -> Self {
        self.expr(Expr::Cmp { op, data })
    }

    fn meta_cmp(self, key: MetaKey, op: CmpOp, data: Vec<u8>) -> Self {
        self.expr(Expr::Meta { key }).cmp(op, data)
    }

    fn payload(self, base: PayloadBase, offset: u32, len: u32) -> Self {
        self.expr(Expr::Payload { base, offset, len })
    }

    fn dep(mut self, dep: Dep) -> Self {
        if self.deps.contains(&dep) {
            return self;
        }
        self.deps.push(dep);
        let fam = self.family;
        match (dep, fam) {
            (Dep::Ipv4, Family::Inet) => self.meta_cmp(MetaKey::Nfproto, CmpOp::Eq, vec![2]),
            (Dep::Ipv6, Family::Inet) => self.meta_cmp(MetaKey::Nfproto, CmpOp::Eq, vec![10]),
            (Dep::Ipv4, Family::Bridge | Family::Netdev) => self.meta_cmp(
                MetaKey::Protocol,
                CmpOp::Eq,
                ETH_P_IP.to_be_bytes().to_vec(),
            ),
            (Dep::Ipv6, Family::Bridge | Family::Netdev) => self.meta_cmp(
                MetaKey::Protocol,
                CmpOp::Eq,
                ETH_P_IPV6.to_be_bytes().to_vec(),
            ),
            (Dep::Arp, _) => self.ether_type(ETH_P_ARP),
            (Dep::Icmpv6Bridge, Family::Bridge | Family::Netdev) => self.ether_type(ETH_P_IPV6),
            (Dep::L4(proto), Family::Ip) => self
                .payload(PayloadBase::Network, 9, 1)
                .cmp(CmpOp::Eq, vec![proto]),
            (Dep::L4(proto), _) => self.meta_cmp(MetaKey::L4proto, CmpOp::Eq, vec![proto]),
            _ => self,
        }
    }

    fn ifname(self, key: MetaKey, op: CmpOp, name: &str) -> Self {
        let mut data = [0u8; 16];
        let n = name.len().min(15);
        data[..n].copy_from_slice(&name.as_bytes()[..n]);
        self.meta_cmp(key, op, data.to_vec())
    }

    /// `iifname "<name>"`
    pub fn iifname(self, name: &str) -> Self {
        self.ifname(MetaKey::Iifname, CmpOp::Eq, name)
    }

    /// `iifname != "<name>"`
    pub fn iifname_ne(self, name: &str) -> Self {
        self.ifname(MetaKey::Iifname, CmpOp::Neq, name)
    }

    /// `oifname "<name>"`
    pub fn oifname(self, name: &str) -> Self {
        self.ifname(MetaKey::Oifname, CmpOp::Eq, name)
    }

    /// `oifname != "<name>"`
    pub fn oifname_ne(self, name: &str) -> Self {
        self.ifname(MetaKey::Oifname, CmpOp::Neq, name)
    }

    /// `meta skuid <uid>`
    pub fn skuid(self, uid: u32) -> Self {
        self.meta_cmp(MetaKey::Skuid, CmpOp::Eq, uid.to_ne_bytes().to_vec())
    }

    /// `meta skuid != <uid>`
    pub fn skuid_ne(self, uid: u32) -> Self {
        self.meta_cmp(MetaKey::Skuid, CmpOp::Neq, uid.to_ne_bytes().to_vec())
    }

    /// `meta nfproto ipv4|ipv6` (pass 2 or 10).
    pub fn nfproto(self, proto: u8) -> Self {
        self.meta_cmp(MetaKey::Nfproto, CmpOp::Eq, vec![proto])
    }

    /// `meta l4proto <proto>`
    pub fn l4proto(self, proto: u8) -> Self {
        self.dep(Dep::L4(proto))
    }

    /// `ether type <type>`
    pub fn ether_type(self, ethertype: u16) -> Self {
        self.payload(PayloadBase::Link, 12, 2)
            .cmp(CmpOp::Eq, ethertype.to_be_bytes().to_vec())
    }

    /// Inside an 802.1Q frame: carries `ethertype` and has the byte run
    /// `[offset, offset + len)` of its payload (counted from the 802.1Q
    /// header's end) differ from `data`.
    fn tagged_payload_ne(self, ethertype: u16, offset: u32, data: Vec<u8>) -> Self {
        self.ether_type(ETH_P_8021Q)
            .payload(PayloadBase::Link, 16, 2)
            .cmp(CmpOp::Eq, ethertype.to_be_bytes().to_vec())
            .payload(PayloadBase::Link, 18 + offset, data.len() as u32)
            .cmp(CmpOp::Neq, data)
    }

    /// `ip saddr != <addr>` for a VLAN-tagged frame.
    pub fn tagged_ip_saddr_ne(self, addr: Ipv4Addr) -> Self {
        self.tagged_payload_ne(ETH_P_IP, 12, addr.octets().to_vec())
    }

    /// `arp saddr ip != <addr>` for a VLAN-tagged frame.
    pub fn tagged_arp_saddr_ip_ne(self, addr: Ipv4Addr) -> Self {
        self.tagged_payload_ne(ETH_P_ARP, 14, addr.octets().to_vec())
    }

    /// `udp sport != <port>` for a VLAN-tagged IPv4 frame without options.
    pub fn tagged_udp_sport_eq(self, port: u16) -> Self {
        self.ether_type(ETH_P_8021Q)
            .payload(PayloadBase::Link, 16, 2)
            .cmp(CmpOp::Eq, ETH_P_IP.to_be_bytes().to_vec())
            .payload(PayloadBase::Link, 18 + 9, 1)
            .cmp(CmpOp::Eq, vec![IPPROTO_UDP])
            .payload(PayloadBase::Link, 18 + 20, 2)
            .cmp(CmpOp::Eq, port.to_be_bytes().to_vec())
    }

    /// The start of a match on an IPv6 packet inside an 802.1Q frame.
    fn tagged_ipv6(self) -> Self {
        self.ether_type(ETH_P_8021Q)
            .payload(PayloadBase::Link, 16, 2)
            .cmp(CmpOp::Eq, ETH_P_IPV6.to_be_bytes().to_vec())
    }

    /// An IPv6 packet inside an 802.1Q frame whose next header is `proto` (extension headers
    /// are not followed).
    fn tagged_ip6_l4(self, proto: u8) -> Self {
        self.tagged_ipv6()
            .payload(PayloadBase::Link, 18 + 6, 1)
            .cmp(CmpOp::Eq, vec![proto])
    }

    /// `ip6 saddr != <addr>` for a VLAN-tagged frame.
    pub fn tagged_ip6_saddr_ne(self, addr: Ipv6Addr) -> Self {
        self.tagged_payload_ne(ETH_P_IPV6, 8, addr.octets().to_vec())
    }

    /// `ip6 saddr != <prefix>` for a VLAN-tagged frame.
    pub fn tagged_ip6_saddr_outside(self, net: Ipv6Net) -> Self {
        self.tagged_ipv6().prefix_cmp(
            PayloadBase::Link,
            18 + 8,
            &net.network().octets(),
            net.prefix_len(),
            CmpOp::Neq,
        )
    }

    /// `icmpv6 type <type>` for a VLAN-tagged IPv6 frame without extension headers.
    pub fn tagged_icmpv6_type(self, kind: u8) -> Self {
        self.tagged_ip6_l4(IPPROTO_ICMPV6)
            .payload(PayloadBase::Link, 18 + 40, 1)
            .cmp(CmpOp::Eq, vec![kind])
    }

    /// The target of a tagged neighbour message lies inside `net`.
    pub fn tagged_icmpv6_target_in(self, net: Ipv6Net) -> Self {
        self.prefix(
            PayloadBase::Link,
            18 + 40 + 8,
            &net.network().octets(),
            net.prefix_len(),
        )
    }

    /// The target of a tagged neighbour message is not `addr`.
    pub fn tagged_icmpv6_target_ne(self, addr: Ipv6Addr) -> Self {
        self.payload(PayloadBase::Link, 18 + 40 + 8, 16)
            .cmp(CmpOp::Neq, addr.octets().to_vec())
    }

    /// `udp sport <port>` for a VLAN-tagged IPv6 frame without extension headers.
    pub fn tagged_udp6_sport_eq(self, port: u16) -> Self {
        self.tagged_ip6_l4(IPPROTO_UDP)
            .payload(PayloadBase::Link, 18 + 40, 2)
            .cmp(CmpOp::Eq, port.to_be_bytes().to_vec())
    }

    /// `ether saddr != <mac>`
    pub fn ether_saddr_ne(self, mac: MacAddr) -> Self {
        self.payload(PayloadBase::Link, 6, 6)
            .cmp(CmpOp::Neq, mac.0.to_vec())
    }

    /// Matches the first `prefix_len` bits of an address field, the way `nft` encodes it:
    /// byte-aligned prefixes load only the covered bytes; others load the whole field and mask.
    fn prefix(self, base: PayloadBase, offset: u32, addr: &[u8], prefix_len: u8) -> Self {
        self.prefix_cmp(base, offset, addr, prefix_len, CmpOp::Eq)
    }

    /// [`Rule::prefix`] with a choice between "inside" ([`CmpOp::Eq`]) and "outside"
    /// ([`CmpOp::Neq`]) the prefix.
    fn prefix_cmp(
        self,
        base: PayloadBase,
        offset: u32,
        addr: &[u8],
        prefix_len: u8,
        op: CmpOp,
    ) -> Self {
        let prefix_len = usize::from(prefix_len).min(addr.len() * 8);
        if prefix_len == 0 {
            return self;
        }
        if prefix_len.is_multiple_of(8) {
            let bytes = prefix_len / 8;
            return self
                .payload(base, offset, bytes as u32)
                .cmp(op, addr[..bytes].to_vec());
        }
        let mask: Vec<u8> = (0..addr.len())
            .map(|i| {
                let bits = prefix_len.saturating_sub(i * 8).min(8);
                (0xff_u16 << (8 - bits)) as u8
            })
            .collect();
        let want = addr.iter().zip(&mask).map(|(a, m)| a & m).collect();
        self.payload(base, offset, addr.len() as u32)
            .expr(Expr::Bitwise {
                xor: vec![0; addr.len()],
                mask,
            })
            .cmp(op, want)
    }

    /// `ip saddr <prefix>`
    pub fn ip_saddr(self, net: Ipv4Net) -> Self {
        self.dep(Dep::Ipv4).prefix(
            PayloadBase::Network,
            12,
            &net.network().octets(),
            net.prefix_len(),
        )
    }

    /// `ip daddr <prefix>`
    pub fn ip_daddr(self, net: Ipv4Net) -> Self {
        self.dep(Dep::Ipv4).prefix(
            PayloadBase::Network,
            16,
            &net.network().octets(),
            net.prefix_len(),
        )
    }

    /// `ip saddr != <addr>`
    pub fn ip_saddr_ne(self, addr: Ipv4Addr) -> Self {
        self.dep(Dep::Ipv4)
            .payload(PayloadBase::Network, 12, 4)
            .cmp(CmpOp::Neq, addr.octets().to_vec())
    }

    /// `ip daddr <addr>`
    pub fn ip_daddr_eq(self, addr: Ipv4Addr) -> Self {
        self.ip_daddr(Ipv4Net::from(addr))
    }

    /// `ip6 saddr <prefix>`
    pub fn ip6_saddr(self, net: Ipv6Net) -> Self {
        self.dep(Dep::Ipv6).prefix(
            PayloadBase::Network,
            8,
            &net.network().octets(),
            net.prefix_len(),
        )
    }

    /// `ip6 daddr <prefix>`
    pub fn ip6_daddr(self, net: Ipv6Net) -> Self {
        self.dep(Dep::Ipv6).prefix(
            PayloadBase::Network,
            24,
            &net.network().octets(),
            net.prefix_len(),
        )
    }

    /// `ip6 saddr != <addr>`
    pub fn ip6_saddr_ne(self, addr: Ipv6Addr) -> Self {
        self.dep(Dep::Ipv6)
            .payload(PayloadBase::Network, 8, 16)
            .cmp(CmpOp::Neq, addr.octets().to_vec())
    }

    /// `ip6 saddr != <prefix>`: the source lies outside `net`.
    pub fn ip6_saddr_outside(self, net: Ipv6Net) -> Self {
        self.dep(Dep::Ipv6).prefix_cmp(
            PayloadBase::Network,
            8,
            &net.network().octets(),
            net.prefix_len(),
            CmpOp::Neq,
        )
    }

    /// `icmpv6` neighbour message whose target address lies inside `net` (the target is the
    /// 16 bytes after the 4-byte header and 4 reserved bytes).
    pub fn icmpv6_target_in(self, net: Ipv6Net) -> Self {
        self.prefix(
            PayloadBase::Transport,
            8,
            &net.network().octets(),
            net.prefix_len(),
        )
    }

    /// `icmpv6` neighbour message whose target address is not `addr`.
    pub fn icmpv6_target_ne(self, addr: Ipv6Addr) -> Self {
        self.payload(PayloadBase::Transport, 8, 16)
            .cmp(CmpOp::Neq, addr.octets().to_vec())
    }

    /// A neighbour solicitation or advertisement whose first option is a link-layer address
    /// option of type `option` (1 for source, 2 for target) naming a MAC other than `mac`.
    pub fn icmpv6_lladdr_ne(self, option: u8, mac: MacAddr) -> Self {
        self.payload(PayloadBase::Transport, 24, 1)
            .cmp(CmpOp::Eq, vec![option])
            .payload(PayloadBase::Transport, 26, 6)
            .cmp(CmpOp::Neq, mac.0.to_vec())
    }

    /// `arp saddr ip != <addr>` (bridge family).
    pub fn arp_saddr_ip_ne(self, addr: Ipv4Addr) -> Self {
        self.dep(Dep::Arp)
            .payload(PayloadBase::Network, 14, 4)
            .cmp(CmpOp::Neq, addr.octets().to_vec())
    }

    fn port(self, proto: u8, offset: u32, port: u16) -> Self {
        self.dep(Dep::L4(proto))
            .payload(PayloadBase::Transport, offset, 2)
            .cmp(CmpOp::Eq, port.to_be_bytes().to_vec())
    }

    /// `udp sport <port>`
    pub fn udp_sport(self, port: u16) -> Self {
        self.port(IPPROTO_UDP, 0, port)
    }

    /// `udp dport <port>`
    pub fn udp_dport(self, port: u16) -> Self {
        self.port(IPPROTO_UDP, 2, port)
    }

    /// `tcp dport <port>`
    pub fn tcp_dport(self, port: u16) -> Self {
        self.port(IPPROTO_TCP, 2, port)
    }

    /// `icmp type <type>`
    pub fn icmp_type(self, kind: u8) -> Self {
        self.dep(Dep::Ipv4)
            .dep(Dep::L4(IPPROTO_ICMP))
            .payload(PayloadBase::Transport, 0, 1)
            .cmp(CmpOp::Eq, vec![kind])
    }

    /// `icmpv6 type <type>`
    pub fn icmpv6_type(self, kind: u8) -> Self {
        let s = if matches!(self.family, Family::Bridge | Family::Netdev) {
            self.dep(Dep::Icmpv6Bridge)
        } else {
            self.dep(Dep::Ipv6)
        };
        s.dep(Dep::L4(IPPROTO_ICMPV6))
            .payload(PayloadBase::Transport, 0, 1)
            .cmp(CmpOp::Eq, vec![kind])
    }

    /// `ct state <states>` (matches any of them).
    pub fn ct_state(self, states: CtState) -> Self {
        self.expr(Expr::CtState)
            .expr(Expr::Bitwise {
                mask: states.bits().to_ne_bytes().to_vec(),
                xor: vec![0; 4],
            })
            .cmp(CmpOp::Neq, vec![0; 4])
    }

    /// `counter`
    pub fn counter(self) -> Self {
        self.expr(Expr::Counter)
    }

    /// Ends the rule with `accept`.
    pub fn accept(self) -> Self {
        self.expr(Expr::Verdict(Verdict::Accept))
    }

    /// Ends the rule with `drop`.
    pub fn drop(self) -> Self {
        self.expr(Expr::Verdict(Verdict::Drop))
    }

    /// Ends the rule with `dnat to <addr>:<port>`.
    pub fn dnat(self, addr: std::net::IpAddr, port: u16) -> Self {
        self.expr(Expr::Dnat { addr, port })
    }

    /// Ends the rule with `snat to <addr>` (nat chains only).
    pub fn snat(self, addr: std::net::IpAddr) -> Self {
        self.expr(Expr::Snat { addr })
    }

    /// Ends the rule with `masquerade` (nat chains only).
    pub fn masquerade(self) -> Self {
        self.expr(Expr::Masquerade)
    }
}

#[cfg(test)]
mod tests;
