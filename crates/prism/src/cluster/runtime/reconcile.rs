//! Keeps the mesh and raft membership in line with committed members.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use prism_cluster::{PrismNode, RaftId};
use tokio::sync::broadcast;

use super::{Cluster, RECONCILE_EVERY, decode_key, grpc_addr, membership};
use crate::cluster::blob::Sponsor;
use crate::cluster::mesh::MeshPeer;
use crate::cluster::model::{MemberDoc, MemberState};
use crate::state::{Change, Command};

impl Cluster {
    /// Mesh peers from committed membership.
    pub(super) async fn reconcile_mesh(&self) {
        let Ok(members) = self.state.members() else {
            return;
        };
        let peers: Vec<MeshPeer> = members
            .iter()
            .filter(|m| m.node_id != self.me)
            .filter_map(member_peer)
            .collect();
        if peers.is_empty() && self.pending_join() {
            return;
        }
        if let Err(e) = self.mesh.set_peers(peers).await {
            tracing::warn!("updating mesh peers: {e:#}");
        }
    }

    /// The leader drives raft membership from committed member documents.
    async fn reconcile_membership(&self) -> anyhow::Result<()> {
        let Some(raft) = self.raft() else {
            return Ok(());
        };
        if !raft.is_leader() {
            return Ok(());
        }
        let metrics = raft.metrics();
        let membership = metrics.membership_config.membership().clone();
        let in_raft: BTreeSet<RaftId> = membership.nodes().map(|(id, _)| *id).collect();
        let members = self.state.members()?;
        let cluster = self.state.cluster()?;
        for m in &members {
            if !in_raft.contains(&m.raft_id) {
                tracing::info!(member = %m.name, "adding raft learner");
                raft.add_learner(
                    m.raft_id,
                    PrismNode {
                        node_id: m.node_id,
                        name: m.name.clone(),
                        addr: grpc_addr(m.mesh_addr),
                    },
                )
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
        }
        // Joining members become active once caught up with the leader.
        let last = metrics.last_log_index.unwrap_or(0);
        let replication: BTreeMap<RaftId, u64> = metrics
            .replication
            .as_ref()
            .map(|r| {
                r.iter()
                    .map(|(id, l)| (*id, l.map_or(0, |l| l.index)))
                    .collect()
            })
            .unwrap_or_default();
        for m in members.iter().filter(|m| m.state == MemberState::Joining) {
            if replication.get(&m.raft_id).is_some_and(|i| *i + 2 >= last) {
                self.state
                    .apply(Command::SetMemberState {
                        node_id: m.node_id,
                        state: MemberState::Active,
                    })
                    .await?;
                tracing::info!(member = %m.name, "member is active");
            }
        }
        // Raise the cluster feature level once every member runs this binary.
        if let Some(c) = &cluster
            && c.feature_level < crate::cluster::model::FEATURE_LEVEL
            && members
                .iter()
                .all(|m| m.feature_level >= crate::cluster::model::FEATURE_LEVEL)
        {
            self.state
                .apply(Command::SetFeatureLevel {
                    level: crate::cluster::model::FEATURE_LEVEL,
                })
                .await?;
            tracing::info!("cluster feature level raised");
        }
        // Demote removed voters first, then drop them. A removed leader
        // steps down once the new voter set commits; the next leader drops it.
        let plan = membership::plan(&in_raft, &membership.voter_ids().collect(), &members);
        if let Some(voters) = plan.set_voters.filter(|_| cluster.is_some()) {
            tracing::info!(?voters, "changing raft voters");
            raft.set_voters(voters)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        if !plan.remove.is_empty() && raft.is_leader() {
            raft.remove_nodes(plan.remove)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Ok(())
    }

    /// Reports this node's feature level once its binary was upgraded.
    async fn report_feature_level(&self) {
        let Ok(members) = self.state.members() else {
            return;
        };
        let level = crate::cluster::model::FEATURE_LEVEL;
        if members
            .iter()
            .any(|m| m.node_id == self.me && m.feature_level < level)
            && let Err(e) = self
                .state
                .apply(Command::SetMemberFeatureLevel {
                    node_id: self.me,
                    level,
                })
                .await
        {
            tracing::debug!("reporting feature level: {e:#}");
        }
    }

    pub(super) async fn reconcile_loop(self: Arc<Self>) {
        let mut changes = self.state.subscribe();
        loop {
            tokio::select! {
                c = changes.recv() => {
                    if matches!(c, Err(broadcast::error::RecvError::Closed)) {
                        return;
                    }
                    if matches!(c, Ok(Change::Applied(_) | Change::Reset) | Err(_)) {
                        self.reconcile_mesh().await;
                    }
                }
                () = tokio::time::sleep(RECONCILE_EVERY) => {}
            }
            self.report_feature_level().await;
            if let Err(e) = self.reconcile_membership().await {
                tracing::debug!("membership reconcile: {e:#}");
            }
        }
    }
}

pub(super) fn sponsor_peer(s: &Sponsor) -> MeshPeer {
    MeshPeer {
        pubkey: s.wg_pubkey,
        mesh_addr: s.mesh_addr,
        endpoint: s.endpoints.first().cloned(),
    }
}

pub(super) fn member_peer(m: &MemberDoc) -> Option<MeshPeer> {
    Some(MeshPeer {
        pubkey: decode_key(&m.wg_pubkey).ok()?,
        mesh_addr: m.mesh_addr,
        endpoint: m.endpoints.first().cloned(),
    })
}
