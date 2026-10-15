//! Networking primitives for Prism nodes.
//!
//! Everything here is a correct, idempotent building block with typed errors; policy
//! (which switches exist, which peers belong to the mesh, ...) lives in the node.
//!
//! - [`Netlink`]: rtnetlink + generic netlink handles bound to the current network namespace.
//! - [`link`]: bridges, VXLAN devices, bridge port VLANs, addresses and VXLAN FDB sync.
//! - [`wireguard`]: key generation, the `wg-prism` interface and its peer set.
//! - [`nft`]: a minimal nftables netlink layer that replaces whole tables atomically.
//! - [`dhcp`]: an embedded static-lease `DHCPv4` server bound to one interface.
//! - [`dns`]: an authoritative responder for a switch zone that forwards everything else.

mod error;
mod handle;

pub mod dhcp;
pub mod dns;
pub mod link;
pub mod nft;
pub mod wireguard;

pub use error::{Error, Result};
pub use handle::Netlink;

/// A 48-bit Ethernet MAC address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    /// The all-zero address, used for VXLAN head-end replication FDB entries.
    pub const ZERO: Self = Self([0; 6]);

    pub fn octets(self) -> [u8; 6] {
        self.0
    }

    pub fn is_zero(self) -> bool {
        self == Self::ZERO
    }
}

impl std::fmt::Display for MacAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, byte) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(":")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for MacAddr {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let mut out = [0u8; 6];
        let mut parts = s.split(':');
        for byte in &mut out {
            let part = parts
                .next()
                .filter(|p| p.len() == 2)
                .ok_or_else(|| Error::Invalid(format!("bad MAC address {s:?}")))?;
            *byte = u8::from_str_radix(part, 16)
                .map_err(|_| Error::Invalid(format!("bad MAC address {s:?}")))?;
        }
        if parts.next().is_some() {
            return Err(Error::Invalid(format!("bad MAC address {s:?}")));
        }
        Ok(Self(out))
    }
}

impl From<[u8; 6]> for MacAddr {
    fn from(b: [u8; 6]) -> Self {
        Self(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_roundtrip() {
        let m: MacAddr = "52:54:00:ab:cd:0e".parse().unwrap();
        assert_eq!(m.0, [0x52, 0x54, 0, 0xab, 0xcd, 0x0e]);
        assert_eq!(m.to_string(), "52:54:00:ab:cd:0e");
        assert!("52:54:00:ab:cd".parse::<MacAddr>().is_err());
        assert!("52:54:00:ab:cd:0e:00".parse::<MacAddr>().is_err());
        assert!("5:54:00:ab:cd:0e".parse::<MacAddr>().is_err());
    }
}
