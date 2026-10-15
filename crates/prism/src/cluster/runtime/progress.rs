//! Removal status: which members have not yet applied each removal.

use std::time::Duration;

use futures::future::join_all;
use prism_proto::v1::cluster_client::ClusterClient;
use serde::Serialize;
use uuid::Uuid;

use super::{Cluster, ClusterStatus, grpc_addr};
use crate::cluster::model::{MemberDoc, TombstoneDoc};

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// How far one member has applied the replicated log.
#[derive(Debug, Clone, Serialize)]
pub struct MemberProgress {
    pub node_id: Uuid,
    pub name: String,
    /// `None` when the member did not answer.
    pub applied_index: Option<u64>,
}

/// A removal and the members that have not applied it yet.
#[derive(Debug, Clone, Serialize)]
pub struct RemovalStatus {
    #[serde(flatten)]
    pub tombstone: TombstoneDoc,
    /// Members whose `applied_index` is below the removal's log index, or
    /// that could not be reached. Catch them up or isolate them.
    pub pending: Vec<MemberProgress>,
    pub complete: bool,
}

/// Members behind (or silent on) a removal at `removed_index`.
pub fn pending(progress: &[MemberProgress], removed_index: u64) -> Vec<MemberProgress> {
    progress
        .iter()
        .filter(|p| p.applied_index.is_none_or(|a| a < removed_index))
        .cloned()
        .collect()
}

impl Cluster {
    async fn probe(&self, m: &MemberDoc) -> MemberProgress {
        let applied_index = if m.node_id == self.me {
            self.status().ok().map(|s| s.applied_index)
        } else {
            self.probe_remote(m).await
        };
        MemberProgress {
            node_id: m.node_id,
            name: m.name.clone(),
            applied_index,
        }
    }

    async fn probe_remote(&self, m: &MemberDoc) -> Option<u64> {
        let ch = self.channels.get(&grpc_addr(m.mesh_addr)).ok()?;
        let mut req = tonic::Request::new(prism_proto::v1::Empty {});
        req.set_timeout(PROBE_TIMEOUT);
        let reply = ClusterClient::new(ch).status(req).await.ok()?.into_inner();
        let s: ClusterStatus = serde_json::from_slice(&reply.status).ok()?;
        Some(s.applied_index)
    }

    /// Each member's applied log index, asked of every member at once.
    pub async fn member_progress(&self) -> anyhow::Result<Vec<MemberProgress>> {
        let members = self.state.members()?;
        Ok(join_all(members.iter().map(|m| self.probe(m))).await)
    }

    /// Every removal with the members that have not applied it.
    pub async fn removal_status(&self) -> anyhow::Result<Vec<RemovalStatus>> {
        let tombstones = self.state.tombstones()?;
        let progress = self.member_progress().await?;
        let mut out: Vec<RemovalStatus> = tombstones
            .into_iter()
            .map(|t| {
                let pending = pending(&progress, t.removed_index);
                RemovalStatus {
                    complete: pending.is_empty(),
                    pending,
                    tombstone: t,
                }
            })
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.tombstone.removed));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(applied: Option<u64>) -> MemberProgress {
        MemberProgress {
            node_id: Uuid::new_v4(),
            name: "n".into(),
            applied_index: applied,
        }
    }

    #[test]
    fn lagging_and_silent_members_are_pending() {
        let all = [p(Some(10)), p(Some(7)), p(None)];
        assert_eq!(pending(&all, 8).len(), 2);
        assert_eq!(pending(&all, 7).len(), 1);
    }
}
