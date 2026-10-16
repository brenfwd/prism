//! Links: bridges, VXLAN devices, bridge port VLANs, addresses and the VXLAN FDB.
//!
//! Every `ensure_*` call is idempotent: it creates what is missing and corrects what drifted.

mod fdb;
mod vlan;

use std::net::Ipv6Addr;

use futures::TryStreamExt;
use ipnet::IpNet;
use netlink_packet_route::link::{
    BridgeStpState, InfoData, InfoVxlan, LinkAttribute, LinkFlags, LinkInfo as NlLinkInfo,
    LinkMessage,
};
use rtnetlink::{LinkBridge, LinkUnspec, LinkVxlan};

use crate::error::{ENODEV, ENOENT, rtnl_errno};
use crate::{Error, MacAddr, Netlink, Result};

pub use fdb::{FdbChanges, VxlanFdb};
pub use vlan::{PortVlan, PortVlans};

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

    /// Routes the host address `addr` (a /128) directly out of `name`, which
    /// lets the host forward to a guest whose network has no IPv6 prefix.
    pub async fn ensure_host_route6(&self, name: &str, addr: Ipv6Addr) -> Result<()> {
        use rtnetlink::RouteMessageBuilder;
        let index = self.link_index(name).await?;
        let msg = RouteMessageBuilder::<Ipv6Addr>::new()
            .destination_prefix(addr, 128)
            .output_interface(index)
            .build();
        self.route
            .route()
            .add(msg)
            .replace()
            .execute()
            .await
            .map_err(|e| Error::rtnl(format!("route {addr} via {name}"), e))
    }

    /// Removes every IPv4 address on a link except `keep`, so a re-addressed bridge drops its old gateway.
    pub async fn prune_v4_addresses(&self, name: &str, keep: Option<IpNet>) -> Result<()> {
        use netlink_packet_route::address::AddressAttribute;
        let index = self.link_index(name).await?;
        let msgs: Vec<_> = self
            .route
            .address()
            .get()
            .set_link_index_filter(index)
            .execute()
            .try_collect()
            .await
            .map_err(|e| Error::rtnl(format!("list addresses of {name}"), e))?;
        for msg in msgs {
            let addr = msg.attributes.iter().find_map(|a| match a {
                AddressAttribute::Address(ip) if ip.is_ipv4() => Some(*ip),
                _ => None,
            });
            let Some(ip) = addr else { continue };
            let wanted =
                keep.is_some_and(|k| k.addr() == ip && k.prefix_len() == msg.header.prefix_len);
            if wanted {
                continue;
            }
            self.route
                .address()
                .del(msg)
                .execute()
                .await
                .map_err(|e| Error::rtnl(format!("delete address {ip} from {name}"), e))?;
        }
        Ok(())
    }
}
