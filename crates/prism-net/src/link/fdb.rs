//! The VXLAN forwarding database: flood list and remote MACs.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use futures::{StreamExt, TryStreamExt};
use netlink_packet_core::{
    NLM_F_ACK, NLM_F_APPEND, NLM_F_CREATE, NLM_F_REPLACE, NLM_F_REQUEST, NetlinkMessage,
    NetlinkPayload,
};
use netlink_packet_route::neighbour::{
    NeighbourAddress, NeighbourAttribute, NeighbourFlags, NeighbourMessage, NeighbourState,
};
use netlink_packet_route::route::RouteType;
use netlink_packet_route::{AddressFamily, RouteNetlinkMessage};

use crate::error::{ENODEV, ENOENT};
use crate::{Error, MacAddr, Netlink, Result};

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
