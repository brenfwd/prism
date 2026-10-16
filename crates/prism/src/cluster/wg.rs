//! The `WireGuard` mesh over netlink (`prism-net`): `wg-prism`, its peers and
//! the nftables rules that make it the only way into the mesh.

use std::net::{Ipv6Addr, SocketAddr};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use futures::future::BoxFuture;
use prism_net::Netlink;
use prism_net::nft::{BoundaryConfig, Nft, boundary_ruleset};
use prism_net::wireguard::{InterfaceConfig, PeerConfig, PrivateKey, PublicKey};
use tokio::sync::Mutex;

use super::mesh::{INTERFACE, Mesh, MeshPeer, PeerStatus};

const KEEPALIVE: u16 = 25;

/// A [`Mesh`] backed by a kernel `WireGuard` interface, configured over netlink.
pub struct NetlinkMesh {
    key_path: PathBuf,
    nl: Mutex<Option<Netlink>>,
}

/// The mesh implementation for this node; `dir` is the config directory.
pub fn mesh(dir: &Path) -> Arc<dyn Mesh> {
    Arc::new(NetlinkMesh {
        key_path: dir.join("wg/wg.key"),
        nl: Mutex::new(None),
    })
}

/// The uid QEMU runs as, so the boundary rules can stop it opening
/// connections into the mesh.
pub(crate) fn qemu_uid() -> Option<u32> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() > 2 && matches!(f[0], "libvirt-qemu" | "qemu")).then(|| f[2].parse().ok())?
    })
}

impl NetlinkMesh {
    async fn nl(&self) -> anyhow::Result<tokio::sync::MappedMutexGuard<'_, Netlink>> {
        let mut g = self.nl.lock().await;
        if g.is_none() {
            *g = Some(Netlink::connect()?);
        }
        Ok(tokio::sync::MutexGuard::map(g, |n| {
            n.as_mut().expect("just connected")
        }))
    }

    /// Loads `wg/wg.key`, creating it (0600, in a 0700 directory) on first use.
    fn private_key(&self) -> anyhow::Result<PrivateKey> {
        if let Ok(text) = std::fs::read_to_string(&self.key_path) {
            return Ok(text.trim().parse()?);
        }
        if let Some(dir) = self.key_path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        let key = PrivateKey::generate();
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&self.key_path)?;
        std::io::Write::write_all(&mut f, key.to_base64().as_bytes())?;
        f.sync_all()?;
        Ok(key)
    }

    async fn ensure_inner(&self, addr: Ipv6Addr, mtu: u16, port: u16) -> anyhow::Result<[u8; 32]> {
        let key = self.private_key()?;
        let public = *key.public_key().as_bytes();
        self.nl()
            .await?
            .ensure_wireguard(&InterfaceConfig {
                name: INTERFACE.into(),
                private_key: key,
                listen_port: port,
                fwmark: None,
                mtu: u32::from(mtu),
                address: ipnet::Ipv6Net::new(addr, 64)?,
            })
            .await?;
        // The mesh /48 is reachable only through wg-prism.
        let s = addr.segments();
        let prefix = ipnet::Ipv6Net::new(Ipv6Addr::new(s[0], s[1], s[2], 0, 0, 0, 0, 0), 48)?;
        let table = boundary_ruleset(&BoundaryConfig {
            mesh_prefix: prefix,
            wg_interface: INTERFACE.into(),
            qemu_uid: qemu_uid(),
        });
        let nft = Nft::new()?;
        let (_, res) = nft.replace_tables_async(vec![table]).await;
        res?;
        Ok(public)
    }

    async fn set_peers_inner(&self, peers: Vec<MeshPeer>) -> anyhow::Result<()> {
        let mut out = Vec::with_capacity(peers.len());
        for p in peers {
            let endpoint = match &p.endpoint {
                Some(e) => tokio::net::lookup_host(e.as_str())
                    .await
                    .ok()
                    .and_then(|mut a| a.next()),
                None => None,
            };
            out.push(PeerConfig {
                public_key: PublicKey::from_bytes(p.pubkey),
                endpoint,
                mesh_address: p.mesh_addr,
                keepalive: Some(KEEPALIVE),
            });
        }
        self.nl()
            .await?
            .set_wireguard_peers(INTERFACE, &out)
            .await?;
        Ok(())
    }

    async fn status_inner(&self) -> anyhow::Result<Vec<PeerStatus>> {
        let st = self.nl().await?.wireguard_status(INTERFACE).await?;
        let now = SystemTime::now();
        Ok(st
            .peers
            .into_iter()
            .map(|p| PeerStatus {
                pubkey: p.public_key.to_base64(),
                endpoint: p.endpoint.map(|e: SocketAddr| e.to_string()),
                handshake_age_secs: p
                    .last_handshake
                    .and_then(|t| now.duration_since(t).ok())
                    .map(|d| d.as_secs()),
                rx_bytes: p.rx_bytes,
                tx_bytes: p.tx_bytes,
            })
            .collect())
    }
}

impl Mesh for NetlinkMesh {
    fn ensure(
        &self,
        addr: Ipv6Addr,
        mtu: u16,
        port: u16,
    ) -> BoxFuture<'_, anyhow::Result<[u8; 32]>> {
        Box::pin(self.ensure_inner(addr, mtu, port))
    }

    fn set_peers(&self, peers: Vec<MeshPeer>) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(self.set_peers_inner(peers))
    }

    fn status(&self) -> BoxFuture<'_, anyhow::Result<Vec<PeerStatus>>> {
        Box::pin(self.status_inner())
    }
}
