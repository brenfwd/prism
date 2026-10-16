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
