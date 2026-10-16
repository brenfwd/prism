//! `WireGuard`: keys, the `wg-prism` interface and its peer set (generic netlink).

mod keys;

pub use keys::{KeyPair, PrivateKey, PublicKey};

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::time::{Duration, SystemTime};

use futures::StreamExt;
use ipnet::{IpNet, Ipv6Net};
use netlink_packet_core::{NLM_F_ACK, NLM_F_DUMP, NLM_F_REQUEST, NetlinkMessage, NetlinkPayload};
use netlink_packet_generic::GenlMessage;
use netlink_packet_wireguard::{
    WireguardAddressFamily, WireguardAllowedIp, WireguardAllowedIpAttr, WireguardAttribute,
    WireguardCmd, WireguardMessage, WireguardPeer, WireguardPeerAttribute, WireguardPeerFlags,
};
use rtnetlink::LinkWireguard;

use crate::{Error, Netlink, Result};

/// The mesh interface name.
pub const INTERFACE: &str = "wg-prism";
/// Default UDP listen port.
pub const DEFAULT_PORT: u16 = 51820;
/// Default `wg-prism` MTU (fits a 1500-byte underlay of either family).
pub const DEFAULT_MTU: u32 = 1420;
/// Persistent keepalive Prism uses for every peer.
pub const KEEPALIVE_SECS: u16 = 25;

/// Configuration of the mesh interface.
#[derive(Clone, Debug)]
pub struct InterfaceConfig {
    pub name: String,
    pub private_key: PrivateKey,
    pub listen_port: u16,
    /// Firewall mark for outer packets; `None` clears it.
    pub fwmark: Option<u32>,
    pub mtu: u32,
    /// The node's mesh address with the mesh subnet's prefix, e.g. `fd12:3456:789a:1::3/64`.
    pub address: Ipv6Net,
}

/// A desired peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerConfig {
    pub public_key: PublicKey,
    /// Underlay endpoint. `None` keeps whatever the kernel has (possibly a roamed address).
    pub endpoint: Option<SocketAddr>,
    /// The peer's mesh address; its only allowed IP, as a /128.
    pub mesh_address: Ipv6Addr,
    /// Persistent keepalive interval in seconds (`None` or 0 disables it).
    pub keepalive: Option<u16>,
}

/// A peer as reported by the kernel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerStatus {
    pub public_key: PublicKey,
    pub endpoint: Option<SocketAddr>,
    pub allowed_ips: Vec<IpNet>,
    /// `None` when no handshake has completed yet.
    pub last_handshake: Option<SystemTime>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub keepalive: u16,
}

/// Interface state as reported by the kernel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceStatus {
    pub ifindex: u32,
    pub public_key: Option<PublicKey>,
    pub listen_port: u16,
    pub fwmark: u32,
    pub peers: Vec<PeerStatus>,
}

/// What [`Netlink::set_wireguard_peers`] changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerChanges {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
}

fn allowed_ip(addr: Ipv6Addr) -> WireguardAllowedIp {
    WireguardAllowedIp(vec![
        WireguardAllowedIpAttr::Family(WireguardAddressFamily::Ipv6),
        WireguardAllowedIpAttr::IpAddr(IpAddr::V6(addr)),
        WireguardAllowedIpAttr::Cidr(128),
    ])
}

/// A handshake newer than this means the peer's current endpoint works.
const LIVE_HANDSHAKE: std::time::Duration = std::time::Duration::from_secs(180);

/// Whether the peer's endpoint needs no write. `WireGuard` follows a peer that
/// roams to the source of its latest authenticated packet, so while the session
/// is live a differing endpoint is the learned one and resetting it to the
/// configured address would break the tunnel until the next handshake.
fn endpoint_settled(want: Option<std::net::SocketAddr>, cur: &PeerStatus) -> bool {
    let Some(want) = want else { return true };
    if cur.endpoint == Some(want) {
        return true;
    }
    cur.endpoint.is_some()
        && cur
            .last_handshake
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < LIVE_HANDSHAKE)
}

impl Netlink {
    /// Creates the `WireGuard` link if missing, sets key, port, fwmark, MTU and address, and
    /// brings it up. Existing peers are left alone. Returns the interface index.
    pub async fn ensure_wireguard(&self, cfg: &InterfaceConfig) -> Result<u32> {
        let name = cfg.name.as_str();
        match self.link(name).await? {
            Some(l) if l.kind.as_deref() == Some("wireguard") => {}
            Some(l) => {
                return Err(Error::WrongKind {
                    name: name.to_owned(),
                    expected: "wireguard",
                    found: l.kind.unwrap_or_else(|| "none".into()),
                });
            }
            None => self
                .route
                .link()
                .add(LinkWireguard::new(name).mtu(cfg.mtu).build())
                .execute()
                .await
                .map_err(|e| Error::rtnl(format!("create {name}"), e))?,
        }
        self.wg_set(
            name,
            vec![
                WireguardAttribute::PrivateKey(*cfg.private_key.as_bytes()),
                WireguardAttribute::ListenPort(cfg.listen_port),
                WireguardAttribute::Fwmark(cfg.fwmark.unwrap_or(0)),
            ],
        )
        .await?;
        self.set_mtu(name, cfg.mtu).await?;
        self.ensure_address(name, IpNet::V6(cfg.address)).await?;
        self.set_link_up(name).await?;
        self.link_index(name).await
    }

    /// Makes the peer set exactly `peers` in one generic netlink message: unknown peers are
    /// removed, new ones added, existing ones updated in place (sessions survive). Each peer's
    /// allowed IPs are replaced by its single mesh /128.
    pub async fn set_wireguard_peers(
        &self,
        ifname: &str,
        peers: &[PeerConfig],
    ) -> Result<PeerChanges> {
        let mut desired = BTreeMap::new();
        for p in peers {
            if desired.insert(p.public_key, p).is_some() {
                return Err(Error::Invalid(format!("duplicate peer {}", p.public_key)));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for p in peers {
            if !seen.insert(p.mesh_address) {
                return Err(Error::Invalid(format!(
                    "mesh address {} assigned to two peers",
                    p.mesh_address
                )));
            }
        }
        let current = self.wireguard_status(ifname).await?;
        let mut changes = PeerChanges::default();
        let mut list = Vec::new();
        for cur in &current.peers {
            if !desired.contains_key(&cur.public_key) {
                list.push(WireguardPeer(vec![
                    WireguardPeerAttribute::PublicKey(*cur.public_key.as_bytes()),
                    WireguardPeerAttribute::Flags(WireguardPeerFlags::RemoveMe),
                ]));
                changes.removed += 1;
            }
        }
        for (key, p) in &desired {
            let existing = current.peers.iter().find(|c| c.public_key == *key);
            let want_allowed = vec![IpNet::V6(Ipv6Net::from(p.mesh_address))];
            let keepalive = p.keepalive.unwrap_or(0);
            match existing {
                Some(c)
                    if c.allowed_ips == want_allowed
                        && c.keepalive == keepalive
                        && endpoint_settled(p.endpoint, c) =>
                {
                    continue;
                }
                Some(_) => changes.updated += 1,
                None => changes.added += 1,
            }
            let mut attrs = vec![
                WireguardPeerAttribute::PublicKey(*key.as_bytes()),
                WireguardPeerAttribute::Flags(WireguardPeerFlags::ReplaceAllowedIps),
                WireguardPeerAttribute::PersistentKeepalive(keepalive),
                WireguardPeerAttribute::AllowedIps(vec![allowed_ip(p.mesh_address)]),
            ];
            if let Some(ep) = p.endpoint {
                attrs.push(WireguardPeerAttribute::Endpoint(ep));
            }
            list.push(WireguardPeer(attrs));
        }
        if !list.is_empty() {
            self.wg_set(ifname, vec![WireguardAttribute::Peers(list)])
                .await?;
        }
        Ok(changes)
    }

    /// Reads the interface's public key, port and peers with their statistics.
    pub async fn wireguard_status(&self, ifname: &str) -> Result<DeviceStatus> {
        let msg = GenlMessage::from_payload(WireguardMessage {
            cmd: WireguardCmd::GetDevice,
            attributes: vec![WireguardAttribute::IfName(ifname.to_owned())],
        });
        let mut req = NetlinkMessage::from(msg);
        req.header.flags = NLM_F_REQUEST | NLM_F_DUMP;
        let op = format!("get wireguard device {ifname}");
        let mut genl = self.genl.clone();
        let mut stream = genl.request(req).await.map_err(|e| Error::genl(&op, e))?;
        let mut status = DeviceStatus {
            ifindex: 0,
            public_key: None,
            listen_port: 0,
            fwmark: 0,
            peers: Vec::new(),
        };
        while let Some(item) = stream.next().await {
            let item = item.map_err(|e| Error::Protocol(format!("{op}: {e}")))?;
            match item.payload {
                NetlinkPayload::InnerMessage(genl) => {
                    for attr in genl.payload.attributes {
                        match attr {
                            WireguardAttribute::IfIndex(i) => status.ifindex = i,
                            WireguardAttribute::PublicKey(k) => {
                                status.public_key = Some(PublicKey::from_bytes(k));
                            }
                            WireguardAttribute::ListenPort(p) => status.listen_port = p,
                            WireguardAttribute::Fwmark(m) => status.fwmark = m,
                            WireguardAttribute::Peers(peers) => {
                                // Large peer lists are split across messages; a peer may
                                // continue in the next one with only more allowed IPs.
                                for peer in peers {
                                    merge_peer(&mut status.peers, peer);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                NetlinkPayload::Error(e) => {
                    let errno = e.raw_code().abs();
                    if errno == crate::error::ENODEV {
                        return Err(Error::NoSuchLink(ifname.to_owned()));
                    }
                    return Err(Error::kernel(op, e.to_io()));
                }
                _ => {}
            }
        }
        Ok(status)
    }

    async fn wg_set(&self, ifname: &str, mut attributes: Vec<WireguardAttribute>) -> Result<()> {
        attributes.insert(0, WireguardAttribute::IfName(ifname.to_owned()));
        let msg = GenlMessage::from_payload(WireguardMessage {
            cmd: WireguardCmd::SetDevice,
            attributes,
        });
        let mut req = NetlinkMessage::from(msg);
        req.header.flags = NLM_F_REQUEST | NLM_F_ACK;
        let op = format!("set wireguard device {ifname}");
        let mut genl = self.genl.clone();
        let mut stream = genl.request(req).await.map_err(|e| Error::genl(&op, e))?;
        while let Some(item) = stream.next().await {
            let item = item.map_err(|e| Error::Protocol(format!("{op}: {e}")))?;
            if let NetlinkPayload::Error(e) = item.payload {
                return Err(Error::kernel(op, e.to_io()));
            }
        }
        Ok(())
    }
}

fn merge_peer(peers: &mut Vec<PeerStatus>, peer: WireguardPeer) {
    let mut st = PeerStatus {
        public_key: PublicKey::from_bytes([0; 32]),
        endpoint: None,
        allowed_ips: Vec::new(),
        last_handshake: None,
        rx_bytes: 0,
        tx_bytes: 0,
        keepalive: 0,
    };
    for attr in peer.0 {
        match attr {
            WireguardPeerAttribute::PublicKey(k) => st.public_key = PublicKey::from_bytes(k),
            WireguardPeerAttribute::Endpoint(e) => st.endpoint = Some(e),
            WireguardPeerAttribute::PersistentKeepalive(k) => st.keepalive = k,
            WireguardPeerAttribute::RxBytes(b) => st.rx_bytes = b,
            WireguardPeerAttribute::TxBytes(b) => st.tx_bytes = b,
            WireguardPeerAttribute::LastHandshake(ts) if ts.seconds > 0 || ts.nano_seconds > 0 => {
                st.last_handshake = Some(
                    SystemTime::UNIX_EPOCH
                        + Duration::new(ts.seconds as u64, ts.nano_seconds as u32),
                );
            }
            WireguardPeerAttribute::AllowedIps(ips) => {
                for ip in ips {
                    let (mut addr, mut cidr) = (None, None);
                    for a in ip.0 {
                        match a {
                            WireguardAllowedIpAttr::IpAddr(x) => addr = Some(x),
                            WireguardAllowedIpAttr::Cidr(c) => cidr = Some(c),
                            _ => {}
                        }
                    }
                    if let (Some(addr), Some(cidr)) = (addr, cidr)
                        && let Ok(net) = IpNet::new(addr, cidr)
                    {
                        st.allowed_ips.push(net);
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(existing) = peers.iter_mut().find(|p| p.public_key == st.public_key) {
        existing.allowed_ips.extend(st.allowed_ips);
    } else {
        peers.push(st);
    }
}
