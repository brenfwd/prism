//! Giving the VLANs of a network their own subnets.

use ipnet::Ipv4Net;

use super::{NetworkView, Node, NodeError, Result};
use crate::network::{AddVlanSubnet, NetworkDoc, VLAN_RANGE, VlanSubnet, host, ipam};
use crate::state::Command;

use super::backups::unix_now;

impl Node {
    /// Subnets that no network (VLAN subnets included) or host route uses.
    fn taken_subnets(&self) -> Result<Vec<Ipv4Net>> {
        let mut taken: Vec<_> = self
            .state
            .networks()?
            .iter()
            .flat_map(NetworkDoc::subnets)
            .collect();
        taken.extend(host::foreign_subnets(&host::routes()));
        taken.push(ipam::CGNAT);
        Ok(taken)
    }

    /// The subnet a new VLAN would get on network `id`, for the UI to offer with one click.
    pub fn suggest_vlan_subnet(&self, id: &str) -> Result<Ipv4Net> {
        self.network(id)?;
        ipam::pick_subnet(&self.taken_subnets()?)
            .ok_or_else(|| NodeError::Conflict("no free subnet left".into()))
    }

    /// Gives VLAN `req.vlan` of network `id` a subnet of its own, picking a free one when the
    /// request names none. NICs already on the VLAN move into it.
    pub async fn add_vlan_subnet(&self, id: &str, req: AddVlanSubnet) -> Result<NetworkView> {
        let mut doc = self.network(id)?.network;
        if !VLAN_RANGE.contains(&req.vlan) {
            return Err(NodeError::Invalid(format!(
                "VLAN {} is out of range 2 to 4094",
                req.vlan
            )));
        }
        if doc.vlan_subnets.contains_key(&req.vlan) {
            return Err(NodeError::Conflict(format!(
                "VLAN {} already has a subnet on {}",
                req.vlan, doc.name
            )));
        }
        let taken = self.taken_subnets()?;
        let subnet = match req.subnet {
            Some(s) => {
                if let Some(hit) = ipam::conflicts(s, &taken) {
                    return Err(NodeError::Conflict(format!(
                        "{s} overlaps {hit}, which a network or this host already uses"
                    )));
                }
                s
            }
            None => ipam::pick_subnet(&taken)
                .ok_or_else(|| NodeError::Conflict("no free subnet left".into()))?,
        };
        doc.vlan_subnets.insert(
            req.vlan,
            VlanSubnet {
                subnet,
                gateways: std::collections::BTreeMap::new(),
            },
        );
        self.put_network_doc(doc).await
    }

    /// Removes the subnet of VLAN `vlan`. Refused while a NIC is on the VLAN.
    pub async fn remove_vlan_subnet(&self, id: &str, vlan: u16) -> Result<NetworkView> {
        let mut doc = self.network(id)?.network;
        if doc.vlan_subnets.remove(&vlan).is_none() {
            return Err(NodeError::NotFound(format!(
                "a subnet for VLAN {vlan} on {}",
                doc.name
            )));
        }
        self.put_network_doc(doc).await
    }

    async fn put_network_doc(&self, doc: NetworkDoc) -> Result<NetworkView> {
        let id = doc.id;
        self.state
            .apply(Command::PutNetwork {
                network: Box::new(doc),
                now: unix_now(),
            })
            .await?;
        self.network(&id.to_string())
    }
}
