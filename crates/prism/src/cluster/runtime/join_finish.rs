//! The second half of joining: waiting for admission, adopting this node's
//! own documents and serving the bootstrap member list.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::reconcile::member_peer;
use super::{Cluster, Duration, LocalMember, META_LOCAL, META_PENDING, PendingJoin, grpc_addr};
use crate::cluster::model::{ClusterDoc, MemberDoc};
use crate::spec::VmSpec;
use crate::state::{Command, PoolDoc};
use prism_proto::v1::cluster_client::ClusterClient;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BootstrapMembers {
    cluster: ClusterDoc,
    members: Vec<MemberDoc>,
}

impl Cluster {
    /// Waits for admission, installs all peers, then re-proposes this node's
    /// own VMs and pools into the cluster once the snapshot has arrived.
    pub(super) async fn finish_join(self: Arc<Self>, pending: PendingJoin) {
        // Documents this node owned before joining; the snapshot replaces them.
        let own_vms: Vec<VmSpec> = self.state.vms().unwrap_or_default();
        let own_pools: Vec<PoolDoc> = self.state.pools(self.me).unwrap_or_default();
        let sponsor = grpc_addr(pending.sponsor_addr);
        let mut delay = Duration::from_secs(1);
        let boot = loop {
            match self.bootstrap(&sponsor).await {
                Ok(b) => break b,
                Err(e) => {
                    tracing::info!("waiting to be admitted by {sponsor}: {e}");
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(15));
                }
            }
        };
        let peers = boot
            .members
            .iter()
            .filter(|m| m.node_id != self.me)
            .filter_map(member_peer)
            .collect();
        if let Err(e) = self.mesh.set_peers(peers).await {
            tracing::error!("installing mesh peers: {e:#}");
        }
        // Wait for the snapshot from the leader.
        loop {
            if self.state.cluster().ok().flatten().is_some()
                && self
                    .state
                    .members()
                    .is_ok_and(|m| m.iter().any(|m| m.node_id == self.me))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let local = LocalMember {
            cluster_id: boot.cluster.cluster_id,
            raft_id: pending.raft_id,
            mesh_addr: boot.cluster.mesh_addr(pending.raft_id),
            wg_mtu: boot.cluster.wg_mtu,
        };
        if let Err(e) = (|| -> anyhow::Result<()> {
            self.state
                .db()
                .set_meta(META_LOCAL, &serde_json::to_string(&local)?)?;
            self.state.db().delete_meta(META_PENDING)
        })() {
            tracing::error!("recording membership: {e:#}");
        }
        *self
            .local
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(local);
        self.adopt_local_documents(own_vms, own_pools).await;
        tracing::info!("joined the cluster");
    }

    /// Re-proposes the VMs and pools this node owned before joining, which
    /// the leader's snapshot replaced. Failures are logged per document.
    async fn adopt_local_documents(&self, vms: Vec<VmSpec>, pools: Vec<PoolDoc>) {
        for mut vm in vms {
            vm.node = self.me;
            if let Err(e) = self
                .state
                .apply(Command::PutVm {
                    vm: Box::new(vm.clone()),
                    expect_generation: Some(None),
                })
                .await
            {
                tracing::warn!(vm = %vm.name, "could not bring this VM into the cluster: {e}");
            }
        }
        for mut pool in pools {
            pool.node = self.me;
            if let Err(e) = self
                .state
                .apply(Command::PutPool { pool: pool.clone() })
                .await
            {
                tracing::warn!(pool = %pool.name, "could not bring this pool into the cluster: {e}");
            }
        }
    }

    async fn bootstrap(&self, addr: &str) -> anyhow::Result<BootstrapMembers> {
        let ch = self.channels.get(addr)?;
        let reply = ClusterClient::new(ch)
            .bootstrap(prism_proto::v1::Empty {})
            .await?
            .into_inner();
        Ok(serde_json::from_slice(&reply.members)?)
    }

    pub(crate) fn bootstrap_members(&self) -> anyhow::Result<Vec<u8>> {
        let cluster = self
            .state
            .cluster()?
            .ok_or_else(|| anyhow::anyhow!("not clustered"))?;
        Ok(serde_json::to_vec(&BootstrapMembers {
            cluster,
            members: self.state.members()?,
        })?)
    }
}
