//! A minimal nftables layer over `NETLINK_NETFILTER`.
//!
//! Prism owns only its own tables (`inet prism`, `ip prism-nat`, `bridge prism`) and always
//! replaces a whole table in one transaction ([`Nft::replace_table`]): the kernel either
//! commits the complete new table or keeps the old one.
//!
//! [`Rule`] is a typed builder for the matches Prism needs. It adds the same implicit
//! protocol dependencies the `nft` CLI adds (for example `meta nfproto ipv6` before an
//! `ip6 saddr` match in an `inet` table), so `nft list` shows the rules exactly as written.

mod rules;
mod wire;

use std::net::{Ipv4Addr, Ipv6Addr};

use ipnet::{Ipv4Net, Ipv6Net};

pub use rules::{
    BoundaryConfig, GuestPort, NatRule, SwitchServices, boundary_ruleset, bridge_guard_ruleset,
    guest_input_chain, nat_ruleset,
};
pub use wire::Nft;

use crate::MacAddr;

/// An nftables address family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    Inet,
    Ip,
    Ip6,
    Bridge,
    Netdev,
}

impl Family {
    pub(crate) fn nfproto(self) -> u8 {
        match self {
            Self::Inet => 1,
            Self::Ip => 2,
            Self::Netdev => 5,
            Self::Bridge => 7,
            Self::Ip6 => 10,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inet => "inet",
            Self::Ip => "ip",
            Self::Ip6 => "ip6",
            Self::Bridge => "bridge",
            Self::Netdev => "netdev",
        }
    }
}

/// A table and everything in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub family: Family,
    pub name: String,
    pub chains: Vec<Chain>,
}

impl Table {
    pub fn new(family: Family, name: impl Into<String>) -> Self {
        Self {
            family,
            name: name.into(),
            chains: Vec::new(),
        }
    }

    /// Adds a chain and returns it for appending rules.
    pub fn chain(&mut self, chain: Chain) -> &mut Chain {
        self.chains.push(chain);
        self.chains.last_mut().expect("just pushed")
    }

    /// The chain with this name, if any.
    pub fn chain_mut(&mut self, name: &str) -> Option<&mut Chain> {
        self.chains.iter_mut().find(|c| c.name == name)
    }

    /// A rule builder for this table's family.
    pub fn rule(&self) -> Rule {
        Rule::new(self.family)
    }
}

/// A chain; base chains are attached to a netfilter hook.
#[must_use]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chain {
    pub name: String,
    pub base: Option<BaseChain>,
    pub rules: Vec<Rule>,
}

impl Chain {
    /// A base chain with policy accept.
    pub fn base(name: impl Into<String>, kind: ChainType, hook: Hook, priority: i32) -> Self {
        Self {
            name: name.into(),
            base: Some(BaseChain {
                kind,
                hook,
                priority,
                policy: Policy::Accept,
            }),
            rules: Vec::new(),
        }
    }

    /// A regular chain (jump target).
    pub fn regular(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base: None,
            rules: Vec::new(),
        }
    }

    pub fn policy(mut self, policy: Policy) -> Self {
        if let Some(b) = &mut self.base {
            b.policy = policy;
        }
        self
    }

    pub fn push(&mut self, rule: Rule) -> &mut Self {
        self.rules.push(rule);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BaseChain {
    pub kind: ChainType,
    pub hook: Hook,
    pub priority: i32,
    pub policy: Policy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainType {
    Filter,
    Nat,
    Route,
}

impl ChainType {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Filter => "filter",
            Self::Nat => "nat",
            Self::Route => "route",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hook {
    Prerouting,
    Input,
    Forward,
    Output,
    Postrouting,
}

impl Hook {
    pub(crate) fn number(self) -> u32 {
        match self {
            Self::Prerouting => 0,
            Self::Input => 1,
            Self::Forward => 2,
            Self::Output => 3,
            Self::Postrouting => 4,
        }
    }
}

/// Standard priorities (`nft` keywords) for the ip/ip6/inet families.
pub mod priority {
    pub const RAW: i32 = -300;
    pub const MANGLE: i32 = -150;
    pub const DSTNAT: i32 = -100;
    pub const FILTER: i32 = 0;
    pub const SECURITY: i32 = 50;
    pub const SRCNAT: i32 = 100;
    /// `filter` in the bridge family.
    pub const BRIDGE_FILTER: i32 = -200;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Accept,
    Drop,
}

/// Rule verdicts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Accept,
    Drop,
    Return,
    Jump(String),
    Goto(String),
}

/// `meta` keys this layer supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetaKey {
    /// `EtherType` of the packet (`meta protocol`), 2 bytes, network order.
    Protocol,
    Mark,
    Iifname,
    Oifname,
    Skuid,
    Skgid,
    Nfproto,
    L4proto,
}

impl MetaKey {
    pub(crate) fn number(self) -> u32 {
        match self {
            Self::Protocol => 1,
            Self::Mark => 3,
            Self::Iifname => 6,
            Self::Oifname => 7,
            Self::Skuid => 10,
            Self::Skgid => 11,
            Self::Nfproto => 15,
            Self::L4proto => 16,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadBase {
    Link,
    Network,
    Transport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
}

/// Conntrack states for [`Rule::ct_state`]; combine with `|` to match any of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CtState(u32);

impl CtState {
    pub const INVALID: Self = Self(1);
    pub const ESTABLISHED: Self = Self(2);
    pub const RELATED: Self = Self(4);
    pub const NEW: Self = Self(8);
    pub const UNTRACKED: Self = Self(64);

    pub fn bits(self) -> u32 {
        self.0
    }
}

impl std::ops::BitOr for CtState {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// One nftables expression. Registers use the 16-byte `NFT_REG_1`; the verdict register is 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Meta {
        key: MetaKey,
    },
    Payload {
        base: PayloadBase,
        offset: u32,
        len: u32,
    },
    Cmp {
        op: CmpOp,
        data: Vec<u8>,
    },
    Bitwise {
        mask: Vec<u8>,
        xor: Vec<u8>,
    },
    /// `ct state` load.
    CtState,
    Counter,
    Masquerade,
    Verdict(Verdict),
}

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
    pub fn new(family: Family) -> Self {
        Self {
            family,
            exprs: Vec::new(),
            deps: Vec::new(),
        }
    }

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

    /// `ether saddr != <mac>`
    pub fn ether_saddr_ne(self, mac: MacAddr) -> Self {
        self.payload(PayloadBase::Link, 6, 6)
            .cmp(CmpOp::Neq, mac.0.to_vec())
    }

    /// Matches the first `prefix_len` bits of an address field, the way `nft` encodes it:
    /// byte-aligned prefixes load only the covered bytes; others load the whole field and mask.
    fn prefix(self, base: PayloadBase, offset: u32, addr: &[u8], prefix_len: u8) -> Self {
        let prefix_len = usize::from(prefix_len).min(addr.len() * 8);
        if prefix_len == 0 {
            return self;
        }
        if prefix_len.is_multiple_of(8) {
            let bytes = prefix_len / 8;
            return self
                .payload(base, offset, bytes as u32)
                .cmp(CmpOp::Eq, addr[..bytes].to_vec());
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
            .cmp(CmpOp::Eq, want)
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
                mask: states.0.to_ne_bytes().to_vec(),
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

    /// Ends the rule with `masquerade` (nat chains only).
    pub fn masquerade(self) -> Self {
        self.expr(Expr::Masquerade)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_are_encoded_like_nft() {
        let r = Rule::new(Family::Ip).ip_saddr("10.21.0.0/23".parse().unwrap());
        assert_eq!(
            r.exprs(),
            &[
                Expr::Payload {
                    base: PayloadBase::Network,
                    offset: 12,
                    len: 4
                },
                Expr::Bitwise {
                    mask: vec![0xff, 0xff, 0xfe, 0],
                    xor: vec![0; 4]
                },
                Expr::Cmp {
                    op: CmpOp::Eq,
                    data: vec![10, 21, 0, 0]
                },
            ]
        );
        let r = Rule::new(Family::Inet).ip6_saddr("fd12:3456:789a::/48".parse().unwrap());
        assert_eq!(r.exprs().len(), 4); // nfproto dependency + payload + cmp
        assert_eq!(
            r.exprs()[2],
            Expr::Payload {
                base: PayloadBase::Network,
                offset: 8,
                len: 6
            }
        );
        let r = Rule::new(Family::Ip).ip_saddr("0.0.0.0/0".parse().unwrap());
        assert_eq!(r.exprs(), &[]);
    }

    #[test]
    fn dependencies_are_added_once() {
        let r = Rule::new(Family::Bridge)
            .ip_saddr_ne(Ipv4Addr::new(10, 0, 0, 1))
            .ip_saddr_ne(Ipv4Addr::UNSPECIFIED);
        // meta protocol + cmp, then two payload + cmp pairs.
        assert_eq!(r.exprs().len(), 6);
    }
}
