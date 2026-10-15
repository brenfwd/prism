//! nftables expressions and the values they compare against.

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
