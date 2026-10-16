//! The `WireGuard` mesh: the single trust boundary between members.

use std::net::Ipv6Addr;

use futures::future::BoxFuture;

/// Interface name of the mesh.
pub const INTERFACE: &str = "wg-prism";
/// UDP port `WireGuard` listens on unless `[cluster]` overrides it.
pub const DEFAULT_PORT: u16 = 51820;
/// Default `wg-prism` MTU: 1420 suits a 1500-byte underlay over IPv6.
pub const DEFAULT_MTU: u16 = 1420;

/// A peer as configured on `wg-prism`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshPeer {
    /// The node's `WireGuard` public key.
    pub pubkey: [u8; 32],
    /// The node's address inside the mesh.
    pub mesh_addr: Ipv6Addr,
    /// First reachable underlay endpoint, if known; peers learn the rest by
    /// roaming.
    pub endpoint: Option<String>,
}

/// Live state of one peer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PeerStatus {
    /// The peer's `WireGuard` public key, base64.
    pub pubkey: String,
    /// The peer's last known UDP endpoint, if it has one.
    pub endpoint: Option<String>,
    /// Seconds since the last handshake.
    pub handshake_age_secs: Option<u64>,
    /// Bytes received from the peer.
    pub rx_bytes: u64,
    /// Bytes sent to the peer.
    pub tx_bytes: u64,
}

/// Mesh operations, implemented over netlink by [`super::wg`].
pub trait Mesh: Send + Sync + 'static {
    /// Creates or updates `wg-prism` with `addr/64`, `mtu` and `port`, using
    /// the node's private key (generated on first use). Returns the public key.
    fn ensure(
        &self,
        addr: Ipv6Addr,
        mtu: u16,
        port: u16,
    ) -> BoxFuture<'_, anyhow::Result<[u8; 32]>>;
    /// Replaces the peer set atomically.
    fn set_peers(&self, peers: Vec<MeshPeer>) -> BoxFuture<'_, anyhow::Result<()>>;
    fn status(&self) -> BoxFuture<'_, anyhow::Result<Vec<PeerStatus>>>;
}
