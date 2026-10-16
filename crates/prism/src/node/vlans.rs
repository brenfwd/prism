//! Editing the VLAN membership of an existing NIC.

use uuid::Uuid;

use super::{Node, NodeError, Result, VmView};
use crate::spec::NicNetwork;

/// New VLAN settings for a Prism network NIC.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct SetVlans {
    /// Access (or native) VLAN; `None` for the untagged default segment.
    #[serde(default)]
    pub vlan: Option<u16>,
    /// Tagged VLANs; non-empty makes the NIC a trunk.
    #[serde(default)]
    pub trunk: Vec<u16>,
}

impl Node {
    /// Changes a NIC's VLANs. No device changes: the network agent
    /// reprograms the bridge port from the stored spec, so it applies to a
    /// running guest at once.
    pub async fn set_vlans(&self, id: Uuid, nic_id: Uuid, req: SetVlans) -> Result<VmView> {
        let _guard = self.ops.lock().await;
        let old = self.local_spec(id)?;
        let mut spec = old.clone();
        let nic = spec
            .nics
            .iter_mut()
            .find(|n| n.id == nic_id)
            .ok_or_else(|| NodeError::NotFound(format!("nic {nic_id}")))?;
        let NicNetwork::Network {
            network,
            ipv4,
            vlan,
            trunk,
            ..
        } = &mut nic.network
        else {
            return Err(NodeError::Invalid(
                "VLANs need a NIC on a Prism network".into(),
            ));
        };
        // A VLAN with its own subnet needs an address from it; the state machine leases one.
        let net = self
            .state
            .networks()?
            .into_iter()
            .find(|n| n.id == *network);
        if net.is_some_and(|n| n.segment(*vlan) != n.segment(req.vlan)) {
            *ipv4 = None;
        }
        *vlan = req.vlan;
        *trunk = req.trunk;
        trunk.sort_unstable();
        nic.network.validate_vlans().map_err(NodeError::Invalid)?;
        self.change_devices(&old, spec, move |_, _| async move { Ok(true) })
            .await
    }
}
