//! nftables expressions and the values they compare against.

/// What happens to a packet that reaches the end of a rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Stop evaluating and let the packet through.
    Accept,
    /// Discard the packet.
    Drop,
    /// Go back to the calling chain, or apply the base chain policy.
    Return,
    /// Evaluate the named chain, then continue after this rule.
    Jump(String),
    /// Evaluate the named chain and do not come back.
    Goto(String),
}

/// `meta` keys this layer supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetaKey {
    /// `EtherType` of the packet (`meta protocol`), 2 bytes, network order.
    Protocol,
    /// Packet mark (`meta mark`), 4 bytes.
    Mark,
    /// Name of the incoming interface (`meta iifname`), 16 bytes, NUL padded.
    Iifname,
    /// Name of the outgoing interface (`meta oifname`), 16 bytes, NUL padded.
    Oifname,
    /// uid of the process that owns the local socket (`meta skuid`), 4 bytes.
    Skuid,
    /// gid of the process that owns the local socket (`meta skgid`), 4 bytes.
    Skgid,
    /// Address family of the packet (`meta nfproto`), 1 byte.
    Nfproto,
    /// Layer 4 protocol number (`meta l4proto`), 1 byte.
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

/// The header a [`Expr::Payload`] offset counts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadBase {
    /// The Ethernet header (bridge family).
    Link,
    /// The `IPv4`, `IPv6` or ARP header.
    Network,
    /// The TCP, UDP or ICMP header.
    Transport,
}

/// Comparison of a register with a constant, as bytes in network order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    /// Equal.
    Eq,
    /// Not equal.
    Neq,
    /// Less than.
    Lt,
    /// Less than or equal.
    Lte,
    /// Greater than.
    Gt,
    /// Greater than or equal.
    Gte,
}

/// Conntrack states for [`Rule::ct_state`]; combine with `|` to match any of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CtState(u32);

impl CtState {
    /// The packet belongs to no known connection and cannot start one.
    pub const INVALID: Self = Self(1);
    /// The packet belongs to a connection that has seen traffic both ways.
    pub const ESTABLISHED: Self = Self(2);
    /// The packet starts a connection expected by an existing one, such as FTP data.
    pub const RELATED: Self = Self(4);
    /// The packet starts a new connection.
    pub const NEW: Self = Self(8);
    /// Connection tracking was bypassed for the packet.
    pub const UNTRACKED: Self = Self(64);

    /// The state mask as the kernel's `NF_CT_STATE_*` bit set.
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
    /// Loads a `meta` value into the register.
    Meta {
        /// The value to load.
        key: MetaKey,
    },
    /// Loads `len` bytes at `offset` from a packet header into the register.
    Payload {
        /// Header the offset counts from.
        base: PayloadBase,
        /// Byte offset into the header.
        offset: u32,
        /// Number of bytes to load.
        len: u32,
    },
    /// Compares the register with `data`; the rule stops here when the comparison is false.
    Cmp {
        /// The comparison.
        op: CmpOp,
        /// The constant, as long as the loaded value.
        data: Vec<u8>,
    },
    /// Replaces the register with `(register & mask) ^ xor`.
    Bitwise {
        /// Bits to keep.
        mask: Vec<u8>,
        /// Value combined with exclusive or after masking.
        xor: Vec<u8>,
    },
    /// `ct state` load.
    CtState,
    /// Counts packets and bytes that reach this point.
    Counter,
    /// Source NAT to the address of the outgoing interface.
    Masquerade,
    /// `dnat to <addr>:<port>` (nat prerouting and output chains).
    Dnat {
        /// New destination address.
        addr: std::net::IpAddr,
        /// New destination port.
        port: u16,
    },
    /// `snat to <addr>` (nat postrouting chains), keeping the port.
    Snat {
        /// New source address.
        addr: std::net::IpAddr,
    },
    /// Ends the rule with a verdict.
    Verdict(Verdict),
}
