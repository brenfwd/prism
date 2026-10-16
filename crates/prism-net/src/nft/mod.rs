//! A minimal nftables layer over `NETLINK_NETFILTER`.
//!
//! Prism owns only its own tables (`inet prism`, `ip prism-nat`, `bridge prism`) and always
//! replaces a whole table in one transaction ([`Nft::replace_table`]): the kernel either
//! commits the complete new table or keeps the old one.
//!
//! [`Rule`] is a typed builder for the matches Prism needs. It adds the same implicit
//! protocol dependencies the `nft` CLI adds (for example `meta nfproto ipv6` before an
//! `ip6 saddr` match in an `inet` table), so `nft list` shows the rules exactly as written.

mod expr;
mod rule;
mod rules;
mod wire;

pub use expr::{CmpOp, CtState, Expr, MetaKey, PayloadBase, Verdict};
pub use rule::Rule;
pub use rules::{
    BoundaryConfig, DnatRule, GuestPort, Ipv6Source, Nat6Rule, NatRule, SwitchServices,
    boundary_ruleset, bridge_guard_ruleset, guest_input_chain, nat_ruleset, nat6_ruleset,
};
pub use wire::Nft;

/// An nftables address family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    /// `IPv4` and `IPv6` together.
    Inet,
    /// `IPv4` only.
    Ip,
    /// `IPv6` only.
    Ip6,
    /// Frames crossing a Linux bridge.
    Bridge,
    /// Ingress on one device.
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

    /// The family keyword the `nft` CLI uses.
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
    /// Family the table belongs to.
    pub family: Family,
    /// Table name, unique within the family.
    pub name: String,
    /// Chains in creation order.
    pub chains: Vec<Chain>,
}

impl Table {
    /// An empty table.
    pub fn new(family: Family, name: impl Into<String>) -> Self {
        Self {
            family,
            name: name.into(),
            chains: Vec::new(),
        }
    }

    /// Adds a chain and returns it for appending rules.
    pub fn chain(&mut self, chain: Chain) -> &mut Chain {
        let index = self.chains.len();
        self.chains.push(chain);
        &mut self.chains[index]
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
    /// Chain name, unique within the table.
    pub name: String,
    /// Hook attachment; `None` for a regular chain that rules jump to.
    pub base: Option<BaseChain>,
    /// Rules in evaluation order.
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

    /// Sets the policy of a base chain; has no effect on a regular chain.
    pub fn policy(mut self, policy: Policy) -> Self {
        if let Some(b) = &mut self.base {
            b.policy = policy;
        }
        self
    }

    /// Appends a rule.
    pub fn push(&mut self, rule: Rule) -> &mut Self {
        self.rules.push(rule);
        self
    }
}

/// How a base chain attaches to the packet path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BaseChain {
    /// What the chain may do.
    pub kind: ChainType,
    /// Where in the packet path it runs.
    pub hook: Hook,
    /// Order among chains on the same hook; lower runs first. See [`priority`].
    pub priority: i32,
    /// Verdict for packets no rule decides.
    pub policy: Policy,
}

/// The kind of a base chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainType {
    /// Accept or drop packets.
    Filter,
    /// Rewrite addresses; only the first packet of a connection is seen.
    Nat,
    /// Re-route a packet after changing its mark.
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

/// A netfilter hook point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hook {
    /// Packets just received, before the routing decision.
    Prerouting,
    /// Packets addressed to this host.
    Input,
    /// Packets routed through this host.
    Forward,
    /// Packets this host sends.
    Output,
    /// Packets about to leave, after routing.
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
    /// `raw`: before connection tracking.
    pub const RAW: i32 = -300;
    /// `mangle`.
    pub const MANGLE: i32 = -150;
    /// `dstnat`: destination NAT.
    pub const DSTNAT: i32 = -100;
    /// `filter`.
    pub const FILTER: i32 = 0;
    /// `security`.
    pub const SECURITY: i32 = 50;
    /// `srcnat`: source NAT.
    pub const SRCNAT: i32 = 100;
    /// `filter` in the bridge family.
    pub const BRIDGE_FILTER: i32 = -200;
}

/// The default verdict of a base chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Let the packet through.
    Accept,
    /// Discard the packet.
    Drop,
}
