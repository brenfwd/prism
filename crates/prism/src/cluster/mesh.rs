//! The `WireGuard` mesh: the single trust boundary between members.

use std::net::Ipv6Addr;

use futures::future::BoxFuture;

/// Interface name of the mesh.
pub const INTERFACE: &str = "wg-prism";
pub const DEFAULT_PORT: u16 = 51820;
/// Default `wg-prism` MTU: 1420 suits a 1500-byte underlay over IPv6.
pub const DEFAULT_MTU: u16 = 1420;

/// A peer as configured on `wg-prism`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshPeer {
    pub pubkey: [u8; 32],
    pub mesh_addr: Ipv6Addr,
    /// First reachable underlay endpoint, if known; peers learn the rest by
    /// roaming.
    pub endpoint: Option<String>,
}

/// Live state of one peer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PeerStatus {
    pub pubkey: String,
    pub endpoint: Option<String>,
    /// Seconds since the last handshake.
    pub handshake_age_secs: Option<u64>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// Mesh operations, implemented over netlink by [`super::wg`].
pub trait Mesh: Send + Sync + 'static {
    /// Creates or updates `wg-prism` with `addr/64`, `mtu` and `port`, using
    /// the node's private key (generated on first use). Returns the public key.
    fn ensure(&self, addr: Ipv6Addr, mtu: u16, port: u16) -> BoxFuture<'_, anyhow::Result<[u8; 32]>>;
    /// Replaces the peer set atomically.
    fn set_peers(&self, peers: Vec<MeshPeer>) -> BoxFuture<'_, anyhow::Result<()>>;
    fn status(&self) -> BoxFuture<'_, anyhow::Result<Vec<PeerStatus>>>;
    /// Removes `wg-prism` (leaving a cluster). The key is kept.
    fn remove(&self) -> BoxFuture<'_, anyhow::Result<()>>;
}
