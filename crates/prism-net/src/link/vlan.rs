//! Bridge port VLAN membership.

use std::collections::{BTreeMap, BTreeSet};

use futures::TryStreamExt;
use netlink_packet_route::AddressFamily;
use netlink_packet_route::link::{
    AfSpecBridge, BridgeVlanInfoFlags, LinkAttribute, LinkExtentMask,
};
use rtnetlink::LinkBridgeVlan;

use crate::{Error, Netlink, Result};

/// The VLAN membership of a bridge port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortVlans {
    /// Access port: one VID, used as PVID and sent untagged.
    Access(u16),
    /// Trunk port: an optional untagged native VID plus tagged VIDs.
    Trunk {
        /// The VLAN carried untagged, used as PVID. `None` for a trunk with no native VLAN.
        native: Option<u16>,
        /// VIDs carried tagged.
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
    /// VLAN id, 1 to 4094.
    pub vid: u16,
    /// Whether untagged ingress frames on the port are assigned to this VLAN.
    pub pvid: bool,
    /// Whether frames of this VLAN leave the port without a tag.
    pub untagged: bool,
}

impl Netlink {
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
