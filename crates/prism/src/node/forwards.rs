//! Port forwarding: nftables DNAT on Prism networks, passt on user-mode NICs.

use uuid::Uuid;

use super::{Node, NodeError, Result, VmView};
use crate::spec::{NicNetwork, PortForward, validate_forwards};

/// Whether two forwards would listen on the same host port. A wildcard
/// forward covers every address.
fn overlaps(a: &PortForward, b: &PortForward) -> bool {
    a.proto == b.proto
        && a.host_port == b.host_port
        && match (a.host_addr, b.host_addr) {
            (Some(x), Some(y)) => x == y,
            _ => true,
        }
}

/// Checks the forwards of a NIC being created or edited.
pub(super) fn check_nic_forwards(network: &NicNetwork) -> Result<()> {
    let forwards = network.forwards();
    validate_forwards(forwards).map_err(NodeError::Invalid)?;
    match network {
        NicNetwork::User { .. } => require_passt(forwards),
        NicNetwork::Network { .. } => {
            let has_v6 = matches!(network, NicNetwork::Network { ipv6: Some(_), .. });
            if !has_v6
                && forwards
                    .iter()
                    .any(|f| f.host_addr.is_some_and(|a| a.is_ipv6()))
            {
                return Err(NodeError::Invalid(
                    "IPv6 port forwards need the NIC to have an IPv6 address".into(),
                ));
            }
            Ok(())
        }
        NicNetwork::Bridge { .. } => Ok(()),
    }
}

impl Node {
    /// Replaces the forwards of a NIC. On a Prism network they apply at once
    /// (the network agent installs the DNAT rules); on a user-mode NIC they
    /// take effect at the next start, since passt cannot change them live.
    pub async fn set_forwards(
        &self,
        id: Uuid,
        nic_id: Uuid,
        forwards: Vec<PortForward>,
    ) -> Result<VmView> {
        let _guard = self.ops.lock().await;
        let old = self.local_spec(id)?;
        let mut spec = old.clone();
        let nic = spec
            .nics
            .iter_mut()
            .find(|n| n.id == nic_id)
            .ok_or_else(|| NodeError::NotFound(format!("nic {nic_id}")))?;
        let live = matches!(nic.network, NicNetwork::Network { .. });
        match nic.network.forwards_mut() {
            Some(slot) => *slot = forwards,
            None => {
                return Err(NodeError::Invalid(
                    "port forwarding needs a Prism network or user-mode NIC".into(),
                ));
            }
        }
        check_nic_forwards(&nic.network)?;
        if live && !matches!(&nic.network, NicNetwork::Network { ipv4: Some(_), .. }) {
            return Err(NodeError::Invalid(
                "the NIC has no address lease to forward to yet".into(),
            ));
        }
        let mine = nic.network.clone();
        // A host port can serve one guest only, per node.
        for vm in self.state.vms()? {
            if vm.node != self.id {
                continue;
            }
            for other in vm.nics.iter().filter(|n| n.id != nic_id) {
                if std::mem::discriminant(&other.network) != std::mem::discriminant(&mine) {
                    continue;
                }
                if let Some(f) = mine
                    .forwards()
                    .iter()
                    .find(|f| other.network.forwards().iter().any(|o| overlaps(f, o)))
                {
                    return Err(NodeError::Conflict(format!(
                        "{}/{} is already forwarded to {}",
                        f.proto.as_str(),
                        f.host_port,
                        vm.name
                    )));
                }
            }
        }
        self.change_devices(&old, spec, move |_, _| async move { Ok(live) })
            .await
    }
}

/// Refuses passt forwards when the host cannot run passt.
pub(super) fn require_passt(forwards: &[PortForward]) -> Result<()> {
    if forwards.is_empty() {
        return Ok(());
    }
    let p = crate::prereqs::passt();
    if p.ok {
        return Ok(());
    }
    Err(NodeError::Invalid(format!(
        "passt port forwarding is unavailable: {}. {} Forwards on a Prism network need no passt.",
        p.detail,
        p.hint.unwrap_or_default()
    )))
}
