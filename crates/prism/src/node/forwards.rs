//! Port forwarding on user-mode NICs.

use uuid::Uuid;

use super::{Node, NodeError, Result, VmView};
use crate::spec::{NicNetwork, PortForward, validate_forwards};

impl Node {
    /// Replaces the forwards of a user-mode NIC. They take effect at the
    /// next start, since the backend cannot change them under a guest.
    pub async fn set_forwards(
        &self,
        id: Uuid,
        nic_id: Uuid,
        forwards: Vec<PortForward>,
    ) -> Result<VmView> {
        validate_forwards(&forwards).map_err(NodeError::Invalid)?;
        let _guard = self.ops.lock().await;
        let old = self.local_spec(id)?;
        // A host listener can serve one VM only.
        for vm in self.state.vms()? {
            if vm.node != self.id {
                continue;
            }
            for nic in vm.nics.iter().filter(|n| n.id != nic_id) {
                if let NicNetwork::User { forwards: other } = &nic.network
                    && let Some(f) = forwards.iter().find(|f| {
                        other.iter().any(|o| {
                            o.proto == f.proto
                                && o.host_port == f.host_port
                                && o.listen_addr() == f.listen_addr()
                        })
                    })
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
        let mut spec = old.clone();
        let nic = spec
            .nics
            .iter_mut()
            .find(|n| n.id == nic_id)
            .ok_or_else(|| NodeError::NotFound(format!("nic {nic_id}")))?;
        match &mut nic.network {
            NicNetwork::User { forwards: slot } => *slot = forwards,
            _ => {
                return Err(NodeError::Invalid(
                    "port forwarding needs a user-mode NIC".into(),
                ));
            }
        }
        self.change_devices(&old, spec, |_, _| async { Ok(false) })
            .await
    }
}
