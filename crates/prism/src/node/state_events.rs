//! Turns applied state changes into SSE events.

use std::sync::Arc;

use tokio::sync::broadcast;

use super::{Node, VmState, lock};
use crate::events::Op;
use crate::state::{Change, Command};

impl Node {
    /// Publishes SSE events for every applied state change.
    pub(super) async fn watch_state(self: Arc<Self>) {
        let mut rx = self.state.subscribe();
        loop {
            let cmd = match rx.recv().await {
                Ok(Change::Applied(c)) => c,
                // Everything may have changed: re-snapshot clients and
                // reconcile this node's VMs.
                Ok(Change::Reset) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    self.bus
                        .publish("reset", "/".into(), Op::Upsert, serde_json::Value::Null);
                    if let Ok(c) = self.client() {
                        self.reconcile(&c).await;
                    }
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            };
            match &*cmd {
                Command::PutVm { vm, .. } => {
                    if vm.node == self.id && self.runtime(vm.id).state == VmState::Unknown {
                        // Moved here: define it.
                        let node = Arc::clone(&self);
                        let id = vm.id;
                        tokio::spawn(async move { node.adopt(id).await });
                    }
                    let view = self.vm_view((**vm).clone());
                    self.bus.publish(
                        "vm.config",
                        format!("/vms/{}", vm.id),
                        Op::Upsert,
                        serde_json::to_value(view).unwrap_or_default(),
                    );
                }
                Command::DeleteVm { id } => {
                    lock(&self.runtime).remove(id);
                    self.bus.publish(
                        "vm.config",
                        format!("/vms/{id}"),
                        Op::Delete,
                        serde_json::Value::Null,
                    );
                }
                Command::PutPool { pool } => {
                    self.bus.publish(
                        "pool",
                        format!("/nodes/{}/pools/{}", pool.node, pool.name),
                        Op::Upsert,
                        serde_json::Value::Null,
                    );
                }
                Command::DeletePool { node, name } => {
                    self.bus.publish(
                        "pool",
                        format!("/nodes/{node}/pools/{name}"),
                        Op::Delete,
                        serde_json::Value::Null,
                    );
                }
                Command::InitCluster { .. }
                | Command::CreateInvite { .. }
                | Command::RevokeInvite { .. }
                | Command::AddMember { .. }
                | Command::SetMemberState { .. }
                | Command::SetVoter { .. }
                | Command::RemoveMember { .. } => {
                    self.bus.publish(
                        "cluster",
                        "/cluster".into(),
                        Op::Upsert,
                        serde_json::Value::Null,
                    );
                }
                Command::PutBackup { backup } => {
                    self.bus.publish(
                        "backup",
                        format!("/backups/{}", backup.id),
                        Op::Upsert,
                        serde_json::to_value(backup).unwrap_or_default(),
                    );
                }
                Command::DeleteBackup { id } => {
                    self.bus.publish(
                        "backup",
                        format!("/backups/{id}"),
                        Op::Delete,
                        serde_json::Value::Null,
                    );
                }
                Command::PutNetwork { network, .. } => {
                    self.bus.publish(
                        "network",
                        format!("/networks/{}", network.id),
                        Op::Upsert,
                        serde_json::Value::Null,
                    );
                }
                Command::DeleteNetwork { id, .. } => {
                    self.bus.publish(
                        "network",
                        format!("/networks/{id}"),
                        Op::Delete,
                        serde_json::Value::Null,
                    );
                }
                Command::PutArchivePool { .. } | Command::PutArchiveMember { .. } => {
                    self.bus.publish(
                        "archive.pool",
                        "/archive/pools".into(),
                        Op::Upsert,
                        serde_json::Value::Null,
                    );
                }
                Command::PutArchiveFile { file, .. } => {
                    self.bus.publish(
                        "archive.file",
                        format!("/archive/pools/{}/files/{}", file.pool, file.path),
                        Op::Upsert,
                        serde_json::to_value(file).unwrap_or_default(),
                    );
                }
                Command::DeleteArchiveFile { pool, path } => {
                    self.bus.publish(
                        "archive.file",
                        format!("/archive/pools/{pool}/files/{path}"),
                        Op::Delete,
                        serde_json::Value::Null,
                    );
                }
            }
        }
    }
}
