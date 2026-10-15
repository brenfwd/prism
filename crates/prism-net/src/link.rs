//! Links: bridges, VXLAN devices, bridge port VLANs, addresses and the VXLAN FDB.
//!
//! Every `ensure_*` call is idempotent: it creates what is missing and corrects what drifted.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv6Addr};

use futures::{StreamExt, TryStreamExt};
use ipnet::IpNet;
use netlink_packet_core::{
    NLM_F_ACK, NLM_F_APPEND, NLM_F_CREATE, NLM_F_REPLACE, NLM_F_REQUEST, NetlinkMessage,
    NetlinkPayload,
};
use netlink_packet_route::link::{
    AfSpecBridge, BridgeStpState, BridgeVlanInfoFlags, InfoData, InfoVxlan, LinkAttribute,
    LinkExtentMask, LinkFlags, LinkInfo as NlLinkInfo, LinkMessage,
};
use netlink_packet_route::neighbour::{
    NeighbourAddress, NeighbourAttribute, NeighbourFlags, NeighbourMessage, NeighbourState,
};
use netlink_packet_route::route::RouteType;
use netlink_packet_route::{AddressFamily, RouteNetlinkMessage};
use rtnetlink::{LinkBridge, LinkBridgeVlan, LinkUnspec, LinkVxlan};

use crate::error::{ENODEV, ENOENT, rtnl_errno};
use crate::{Error, MacAddr, Netlink, Result};

/// The IANA VXLAN port.
pub const VXLAN_PORT: u16 = 4789;

/// A snapshot of one interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkInfo {
    pub index: u32,
    pub name: String,
    /// `IFLA_INFO_KIND` (`bridge`, `vxlan`, `wireguard`, `veth`, ...); `None` for physical NICs.
    pub kind: Option<String>,
    pub mtu: Option<u32>,
    /// Administratively up (`IFF_UP`).
    pub up: bool,
    /// Index of the bridge (or other controller) this link is enslaved to.
    pub controller: Option<u32>,
    pub mac: Option<MacAddr>,
}

impl LinkInfo {
    fn from_message(msg: &LinkMessage) -> Self {
        let mut info = Self {
            index: msg.header.index,
            name: String::new(),
            kind: None,
            mtu: None,
            up: msg.header.flags.contains(LinkFlags::Up),
            controller: None,
            mac: None,
        };
        for attr in &msg.attributes {
            match attr {
                LinkAttribute::IfName(n) => info.name.clone_from(n),
                LinkAttribute::Mtu(m) => info.mtu = Some(*m),
                LinkAttribute::Controller(c) => info.controller = Some(*c),
                LinkAttribute::Address(a) => {
                    info.mac = <[u8; 6]>::try_from(a.as_slice()).ok().map(MacAddr);
                }
                LinkAttribute::LinkInfo(infos) => {
                    info.kind = infos.iter().find_map(|i| match i {
                        NlLinkInfo::Kind(k) => Some(k.to_string()),
                        _ => None,
                    });
                }
                _ => {}
            }
        }
        info
    }
}

/// Settings Prism enforces on a switch bridge (`ps<id>`). VLAN filtering is always on, STP and
/// multicast snooping are always off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgeConfig {
    pub mtu: u32,
}

/// A VXLAN device (`vx<id>`) with unicast-only forwarding (`nolearning`, no group).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VxlanConfig {
    pub vni: u32,
    /// Local VTEP address: the node's mesh address.
    pub local: Ipv6Addr,
    /// UDP destination port, normally [`VXLAN_PORT`].
    pub port: u16,
    pub mtu: u32,
    /// Bridge to enslave the device to.
    pub bridge: Option<String>,
}

/// The VLAN membership of a bridge port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortVlans {
    /// Access port: one VID, used as PVID and sent untagged.
    Access(u16),
    /// Trunk port: an optional untagged native VID plus tagged VIDs.
    Trunk {
        native: Option<u16>,
        tagged: BTreeSet<u16>,
    },
}

impl PortVlans {
    fn entries(&self) -> Result<BTreeMap<u16, BridgeVlanInfoFlags>> {
        let mut out = BTreeMap::new();
        match self {
            Self::Access(vid) => {
                out.insert(
                    *vid,
                    BridgeVlanInfoFlags::Pvid | BridgeVlanInfoFlags::Untagged,
                );
            }
            Self::Trunk { native, tagged } => {
                for vid in tagged {
                    out.insert(*vid, BridgeVlanInfoFlags::empty());
                }
                if let Some(vid) = native {
                    out.insert(
                        *vid,
                        BridgeVlanInfoFlags::Pvid | BridgeVlanInfoFlags::Untagged,
                    );
                }
            }
        }
        if let Some(bad) = out.keys().find(|v| !(1..=4094).contains(*v)) {
            return Err(Error::Invalid(format!("VLAN id {bad} out of range 1-4094")));
        }
        Ok(out)
    }
}

/// One VLAN on a bridge port, as read back from the kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortVlan {
    pub vid: u16,
    pub pvid: bool,
    pub untagged: bool,
}

/// The unicast FDB of a VXLAN device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VxlanFdb {
    /// Remote VTEPs that receive a copy of every broadcast, unknown-unicast and multicast frame
    /// (all-zero MAC entries, head-end replication).
    pub flood: BTreeSet<IpAddr>,
    /// Known remote MACs and the VTEP behind each.
    pub macs: BTreeMap<MacAddr, IpAddr>,
}

/// What [`Netlink::sync_vxlan_fdb`] changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FdbChanges {
    pub added: usize,
    pub replaced: usize,
    pub removed: usize,
}

impl Netlink {
    /// Looks up a link by name; `Ok(None)` when it does not exist.
    pub async fn link(&self, name: &str) -> Result<Option<LinkInfo>> {
        let mut stream = self
            .route
            .link()
            .get()
            .match_name(name.to_owned())
            .execute();
        match stream.try_next().await {
            Ok(Some(msg)) => Ok(Some(LinkInfo::from_message(&msg))),
            Ok(None) => Ok(None),
            Err(e) if matches!(rtnl_errno(&e), Some(ENODEV | ENOENT)) => Ok(None),
            Err(e) => Err(Error::rtnl(format!("get link {name}"), e)),
        }
    }

    /// All links in the namespace.
    pub async fn links(&self) -> Result<Vec<LinkInfo>> {
        self.route
            .link()
            .get()
            .execute()
            .map_ok(|m| LinkInfo::from_message(&m))
            .try_collect()
            .await
            .map_err(|e| Error::rtnl("dump links", e))
    }

    /// The index of an existing link.
    pub async fn link_index(&self, name: &str) -> Result<u32> {
        self.link(name)
            .await?
            .map(|l| l.index)
            .ok_or_else(|| Error::NoSuchLink(name.to_owned()))
    }

    async fn existing_of_kind(&self, name: &str, kind: &'static str) -> Result<Option<LinkInfo>> {
        match self.link(name).await? {
            Some(l) if l.kind.as_deref() == Some(kind) => Ok(Some(l)),
            Some(l) => Err(Error::WrongKind {
                name: name.to_owned(),
                expected: kind,
                found: l.kind.unwrap_or_else(|| "none".into()),
            }),
            None => Ok(None),
        }
    }

    /// Creates or corrects a switch bridge and brings it up. Returns its index.
    pub async fn ensure_bridge(&self, name: &str, cfg: &BridgeConfig) -> Result<u32> {
        let existing = self.existing_of_kind(name, "bridge").await?;
        let mut builder = LinkBridge::new(name)
            .vlan_filtering(true)
            .stp_state(BridgeStpState::Disabled)
            .mcast_snooping(false)
            .mtu(cfg.mtu);
        let op = format!("ensure bridge {name}");
        if let Some(l) = existing {
            builder = builder.index(l.index);
            self.route
                .link()
                .change(builder.build())
                .execute()
                .await
                .map_err(|e| Error::rtnl(op, e))?;
            Ok(l.index)
        } else {
            self.route
                .link()
                .add(builder.build())
                .execute()
                .await
                .map_err(|e| Error::rtnl(op, e))?;
            self.link_index(name).await
        }
    }

    /// Creates a VXLAN device, or recreates it when its VNI, local address, port or learning
    /// setting differ (the kernel cannot change those in place). Then sets MTU, bridge and up.
    pub async fn ensure_vxlan(&self, name: &str, cfg: &VxlanConfig) -> Result<u32> {
        if let Some(existing) = self.existing_of_kind(name, "vxlan").await?
            && !self.vxlan_matches(existing.index, cfg).await?
        {
            self.delete_link(name).await?;
        }
        if self.link(name).await?.is_none() {
            let msg = LinkVxlan::new(name, cfg.vni)
                .local6(cfg.local)
                .port(cfg.port)
                .learning(false)
                .mtu(cfg.mtu)
                .build();
            self.route
                .link()
                .add(msg)
                .execute()
                .await
                .map_err(|e| Error::rtnl(format!("create vxlan {name}"), e))?;
        }
        let index = self.link_index(name).await?;
        let mut builder = LinkUnspec::new_with_index(index).mtu(cfg.mtu).up();
        builder = match &cfg.bridge {
            Some(br) => builder.controller(self.link_index(br).await?),
            None => builder.nocontroller(),
        };
        self.route
            .link()
            .change(builder.build())
            .execute()
            .await
            .map_err(|e| Error::rtnl(format!("configure vxlan {name}"), e))?;
        Ok(index)
    }

    async fn vxlan_matches(&self, index: u32, cfg: &VxlanConfig) -> Result<bool> {
        let msg = self
            .route
            .link()
            .get()
            .match_index(index)
            .execute()
            .try_next()
            .await
            .map_err(|e| Error::rtnl("get vxlan", e))?;
        let Some(msg) = msg else { return Ok(false) };
        let data = msg.attributes.iter().find_map(|a| match a {
            LinkAttribute::LinkInfo(infos) => infos.iter().find_map(|i| match i {
                NlLinkInfo::Data(InfoData::Vxlan(v)) => Some(v.clone()),
                _ => None,
            }),
            _ => None,
        });
        let Some(data) = data else { return Ok(false) };
        let (mut vni, mut local, mut port, mut learning) = (None, None, None, None);
        for attr in data {
            match attr {
                InfoVxlan::Id(v) => vni = Some(v),
                InfoVxlan::Local6(a) => local = Some(a),
                InfoVxlan::Port(p) => port = Some(p),
                InfoVxlan::Learning(l) => learning = Some(l),
                _ => {}
            }
        }
        Ok(vni == Some(cfg.vni)
            && local == Some(cfg.local)
            && port == Some(cfg.port)
            && learning == Some(false))
    }

    /// Enslaves `port` to `bridge`, or releases it with `None`.
    pub async fn set_controller(&self, port: &str, bridge: Option<&str>) -> Result<()> {
        let index = self.link_index(port).await?;
        let builder = LinkUnspec::new_with_index(index);
        let builder = match bridge {
            Some(br) => builder.controller(self.link_index(br).await?),
            None => builder.nocontroller(),
        };
        self.route
            .link()
            .change(builder.build())
            .execute()
            .await
            .map_err(|e| Error::rtnl(format!("set controller of {port}"), e))
    }

    /// Sets a link administratively up or down.
    pub async fn set_link_up(&self, name: &str, up: bool) -> Result<()> {
        let index = self.link_index(name).await?;
        let builder = LinkUnspec::new_with_index(index);
        let builder = if up { builder.up() } else { builder.down() };
        self.route
            .link()
            .change(builder.build())
            .execute()
            .await
            .map_err(|e| Error::rtnl(format!("set {name} up={up}"), e))
    }

    /// Sets a link's MTU.
    pub async fn set_mtu(&self, name: &str, mtu: u32) -> Result<()> {
        let index = self.link_index(name).await?;
        self.route
            .link()
            .change(LinkUnspec::new_with_index(index).mtu(mtu).build())
            .execute()
            .await
            .map_err(|e| Error::rtnl(format!("set mtu of {name}"), e))
    }

    /// Deletes a link. Returns `false` when it did not exist.
    pub async fn delete_link(&self, name: &str) -> Result<bool> {
        let Some(link) = self.link(name).await? else {
            return Ok(false);
        };
        match self.route.link().del(link.index).execute().await {
            Ok(()) => Ok(true),
            Err(e) if matches!(rtnl_errno(&e), Some(ENODEV | ENOENT)) => Ok(false),
            Err(e) => Err(Error::rtnl(format!("delete link {name}"), e)),
        }
    }

    /// Adds an address to a link unless already present (replacing its flags otherwise).
    pub async fn ensure_address(&self, name: &str, addr: IpNet) -> Result<()> {
        let index = self.link_index(name).await?;
        self.route
            .address()
            .add(index, addr.addr(), addr.prefix_len())
            .replace()
            .execute()
            .await
            .map_err(|e| Error::rtnl(format!("add address {addr} to {name}"), e))
    }

    /// The VLANs configured on a bridge port.
    pub async fn port_vlans(&self, port: &str) -> Result<Vec<PortVlan>> {
        let index = self.link_index(port).await?;
        let mut stream = self
            .route
            .link()
            .get()
            .set_filter_mask(AddressFamily::Bridge, vec![LinkExtentMask::Brvlan])
            .execute();
        let mut out = Vec::new();
        while let Some(msg) = stream
            .try_next()
            .await
            .map_err(|e| Error::rtnl("dump bridge vlans", e))?
        {
            if msg.header.index != index {
                continue;
            }
            for attr in &msg.attributes {
                if let LinkAttribute::AfSpecBridge(specs) = attr {
                    for spec in specs {
                        if let AfSpecBridge::VlanInfo(v) = spec {
                            out.push(PortVlan {
                                vid: v.vid,
                                pvid: v.flags.contains(BridgeVlanInfoFlags::Pvid),
                                untagged: v.flags.contains(BridgeVlanInfoFlags::Untagged),
                            });
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Makes a bridge port's VLAN membership exactly `vlans` (like `bridge vlan add/del`).
    pub async fn set_port_vlans(&self, port: &str, vlans: &PortVlans) -> Result<()> {
        let desired = vlans.entries()?;
        let index = self.link_index(port).await?;
        let current = self.port_vlans(port).await?;
        let stale: Vec<u16> = current
            .iter()
            .map(|v| v.vid)
            .filter(|vid| !desired.contains_key(vid))
            .collect();
        if !stale.is_empty() {
            let mut builder = LinkBridgeVlan::new(index);
            for vid in stale {
                builder = builder.vlan(vid, BridgeVlanInfoFlags::empty());
            }
            self.route
                .link()
                .del_with_message(builder.build())
                .execute()
                .await
                .map_err(|e| Error::rtnl(format!("delete vlans on {port}"), e))?;
        }
        let mut builder = LinkBridgeVlan::new(index);
        for (vid, flags) in &desired {
            builder = builder.vlan(*vid, *flags);
        }
        self.route
            .link()
            .set(builder.build())
            .execute()
            .await
            .map_err(|e| Error::rtnl(format!("add vlans on {port}"), e))
    }

    /// Reads the unicast FDB (`self` entries with a remote) of a VXLAN device.
    pub async fn vxlan_fdb(&self, dev: &str) -> Result<VxlanFdb> {
        let index = self.link_index(dev).await?;
        let mut stream = self
            .route
            .neighbours()
            .get()
            .set_address_family(AddressFamily::Bridge)
            .execute();
        let mut fdb = VxlanFdb::default();
        while let Some(msg) = stream
            .try_next()
            .await
            .map_err(|e| Error::rtnl("dump fdb", e))?
        {
            if msg.header.ifindex != index {
                continue;
            }
            let (mut mac, mut dst) = (None, None);
            for attr in &msg.attributes {
                match attr {
                    NeighbourAttribute::LinkLayerAddress(a) => {
                        mac = <[u8; 6]>::try_from(a.as_slice()).ok().map(MacAddr);
                    }
                    NeighbourAttribute::Destination(NeighbourAddress::Inet6(a)) => {
                        dst = Some(IpAddr::V6(*a));
                    }
                    NeighbourAttribute::Destination(NeighbourAddress::Inet(a)) => {
                        dst = Some(IpAddr::V4(*a));
                    }
                    // AF_BRIDGE messages carry NDA_DST unparsed; its length gives the family.
                    NeighbourAttribute::Destination(NeighbourAddress::Other(raw)) => {
                        dst = <[u8; 16]>::try_from(raw.as_slice())
                            .map(|b| IpAddr::V6(b.into()))
                            .or_else(|_| {
                                <[u8; 4]>::try_from(raw.as_slice()).map(|b| IpAddr::V4(b.into()))
                            })
                            .ok();
                    }
                    _ => {}
                }
            }
            if let (Some(mac), Some(dst)) = (mac, dst) {
                if mac.is_zero() {
                    fdb.flood.insert(dst);
                } else {
                    fdb.macs.insert(mac, dst);
                }
            }
        }
        Ok(fdb)
    }

    /// Makes the VXLAN device's FDB exactly `desired` by applying the difference: one all-zero
    /// MAC entry per flood VTEP (head-end replication) and one entry per remote MAC.
    pub async fn sync_vxlan_fdb(&self, dev: &str, desired: &VxlanFdb) -> Result<FdbChanges> {
        let index = self.link_index(dev).await?;
        let current = self.vxlan_fdb(dev).await?;
        let mut changes = FdbChanges::default();

        for (mac, dst) in &current.macs {
            if !desired.macs.contains_key(mac) {
                self.fdb_request(index, *mac, *dst, FdbOp::Delete).await?;
                changes.removed += 1;
            }
        }
        for dst in current.flood.difference(&desired.flood) {
            self.fdb_request(index, MacAddr::ZERO, *dst, FdbOp::Delete)
                .await?;
            changes.removed += 1;
        }
        for dst in desired.flood.difference(&current.flood) {
            self.fdb_request(index, MacAddr::ZERO, *dst, FdbOp::Append)
                .await?;
            changes.added += 1;
        }
        for (mac, dst) in &desired.macs {
            if mac.is_zero() {
                return Err(Error::Invalid(
                    "all-zero MAC belongs in VxlanFdb::flood".into(),
                ));
            }
            match current.macs.get(mac) {
                Some(cur) if cur == dst => {}
                Some(_) => {
                    self.fdb_request(index, *mac, *dst, FdbOp::Replace).await?;
                    changes.replaced += 1;
                }
                None => {
                    self.fdb_request(index, *mac, *dst, FdbOp::Replace).await?;
                    changes.added += 1;
                }
            }
        }
        Ok(changes)
    }

    async fn fdb_request(&self, index: u32, mac: MacAddr, dst: IpAddr, op: FdbOp) -> Result<()> {
        let mut msg = NeighbourMessage::default();
        msg.header.family = AddressFamily::Bridge;
        msg.header.ifindex = index;
        msg.header.state = NeighbourState::Permanent;
        msg.header.flags = NeighbourFlags::Own;
        msg.header.kind = RouteType::Unspec;
        msg.attributes
            .push(NeighbourAttribute::LinkLayerAddress(mac.0.to_vec()));
        msg.attributes
            .push(NeighbourAttribute::Destination(match dst {
                IpAddr::V4(a) => NeighbourAddress::Inet(a),
                IpAddr::V6(a) => NeighbourAddress::Inet6(a),
            }));
        let (payload, flags) = match op {
            FdbOp::Append => (
                RouteNetlinkMessage::NewNeighbour(msg),
                NLM_F_CREATE | NLM_F_APPEND,
            ),
            FdbOp::Replace => (
                RouteNetlinkMessage::NewNeighbour(msg),
                NLM_F_CREATE | NLM_F_REPLACE,
            ),
            FdbOp::Delete => (RouteNetlinkMessage::DelNeighbour(msg), 0),
        };
        let mut req = NetlinkMessage::from(payload);
        req.header.flags = NLM_F_REQUEST | NLM_F_ACK | flags;
        let op_name = format!("fdb {op:?} {mac} dst {dst}");
        let mut handle = self.route.clone();
        let mut resp = handle.request(req).map_err(|e| Error::rtnl(&op_name, e))?;
        while let Some(m) = resp.next().await {
            if let NetlinkPayload::Error(err) = m.payload {
                let errno = err.raw_code().abs();
                if op == FdbOp::Delete && (errno == ENOENT || errno == ENODEV) {
                    return Ok(());
                }
                return Err(Error::kernel(op_name, err.to_io()));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FdbOp {
    Append,
    Replace,
    Delete,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlan_entries() {
        let access = PortVlans::Access(10).entries().unwrap();
        assert_eq!(
            access.get(&10),
            Some(&(BridgeVlanInfoFlags::Pvid | BridgeVlanInfoFlags::Untagged))
        );
        let trunk = PortVlans::Trunk {
            native: Some(1),
            tagged: [10, 20].into_iter().collect(),
        }
        .entries()
        .unwrap();
        assert_eq!(trunk.len(), 3);
        assert_eq!(trunk[&20], BridgeVlanInfoFlags::empty());
        assert!(PortVlans::Access(4095).entries().is_err());
    }
}
