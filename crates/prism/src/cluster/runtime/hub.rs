//! Planned hub transfer: make another member the hub (the single raft
//! voter) and demote the old one to a learner, so the old hub can leave
//! without removing everyone.

use std::time::Duration;

use uuid::Uuid;

use super::{Cluster, ClusterOpError};
use crate::cluster::model::{ClusterMode, MemberDoc, MemberState};
use crate::state::Command;

/// How long to wait for the new voter to catch up with the log.
const CATCH_UP_TIMEOUT: Duration = Duration::from_secs(15);
/// How many log entries the new voter may trail by when the old hub steps down.
const CATCH_UP_SLACK: u64 = 2;
/// How long to wait for the new hub to win leadership.
const HANDOVER_TIMEOUT: Duration = Duration::from_secs(30);

/// Whether `to` can take the hub role from `from`. Returns the reason when not.
pub fn check_transfer(
    mode: ClusterMode,
    members: &[MemberDoc],
    from: Uuid,
    to: Uuid,
) -> Result<(), String> {
    if mode != ClusterMode::HubSpoke {
        return Err("only hub-spoke clusters have a hub; consensus clusters use voters".into());
    }
    if from == to {
        return Err("choose a different node to become the hub".into());
    }
    if !members.iter().any(|m| m.node_id == from && m.voter) {
        return Err("only the current hub can hand over the hub role".into());
    }
    match members.iter().find(|m| m.node_id == to) {
        None => Err("no such member".into()),
        Some(m) if m.state != MemberState::Active => {
            Err(format!("{} has not finished joining yet", m.name))
        }
        Some(_) => Ok(()),
    }
}

impl Cluster {
    async fn set_voter(&self, node_id: Uuid, voter: bool) -> Result<(), ClusterOpError> {
        self.state
            .apply(Command::SetVoter { node_id, voter })
            .await?;
        Ok(())
    }

    /// Waits until the leader's replication of raft member `raft_id` is within
    /// [`CATCH_UP_SLACK`] entries of the last log index.
    async fn wait_caught_up(&self, raft_id: u64) -> bool {
        let deadline = tokio::time::Instant::now() + CATCH_UP_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            if let Some(m) = self.raft().map(|r| r.metrics()) {
                let last = m.last_log_index.unwrap_or(0);
                let matched = m
                    .replication
                    .as_ref()
                    .and_then(|r| r.get(&raft_id))
                    .and_then(|l| l.as_ref())
                    .map(|l| l.index);
                if matched.is_some_and(|i| i + CATCH_UP_SLACK >= last) {
                    return true;
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        false
    }

    /// Promotes `to` and demotes this node, then waits for `to` to lead.
    pub async fn transfer_hub(&self, to: Uuid) -> Result<(), ClusterOpError> {
        let doc = self
            .state
            .cluster()?
            .ok_or_else(|| ClusterOpError::Invalid("this node is not in a cluster".into()))?;
        let members = self.state.members()?;
        check_transfer(doc.mode, &members, self.me, to).map_err(ClusterOpError::Invalid)?;
        if self.status()?.degraded {
            return Err(ClusterOpError::Conflict(
                "the cluster is degraded; wait until it is healthy".into(),
            ));
        }
        let target = members
            .iter()
            .find(|m| m.node_id == to)
            .map(|m| m.raft_id)
            .ok_or_else(|| ClusterOpError::Invalid("no such member".into()))?;
        // Two committed changes, never zero voters: the reconciler first
        // makes {old, new} voters, then {new}. The old hub steps down only
        // after the new voter has caught up, and a failure puts it back.
        self.set_voter(to, true).await?;
        if !self.wait_caught_up(target).await {
            let _ = self.set_voter(to, false).await;
            return Err(ClusterOpError::Conflict(
                "the new hub is not reachable or is behind on the log; it stays a learner".into(),
            ));
        }
        if let Err(e) = self.set_voter(self.me, false).await {
            let _ = self.set_voter(to, false).await;
            return Err(e);
        }
        let deadline = tokio::time::Instant::now() + HANDOVER_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            if self.status()?.leader == Some(to) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err(ClusterOpError::Conflict(
            "the new hub has not taken over yet; it will finish in the background".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: u128, voter: bool, state: MemberState) -> MemberDoc {
        MemberDoc {
            node_id: Uuid::from_u128(id),
            raft_id: id as u64,
            name: format!("n{id}"),
            wg_pubkey: String::new(),
            mesh_addr: "fd00::1".parse().unwrap(),
            endpoints: vec![],
            state,
            voter,
            version: String::new(),
            feature_level: 1,
            joined: 0,
        }
    }

    #[test]
    fn transfer_rules() {
        let m = [
            member(1, true, MemberState::Active),
            member(2, false, MemberState::Active),
            member(3, false, MemberState::Joining),
        ];
        let id = |n: u128| Uuid::from_u128(n);
        let hs = ClusterMode::HubSpoke;
        assert!(check_transfer(hs, &m, id(1), id(2)).is_ok());
        assert!(check_transfer(hs, &m, id(1), id(3)).is_err());
        assert!(check_transfer(hs, &m, id(1), id(1)).is_err());
        assert!(check_transfer(hs, &m, id(2), id(1)).is_err());
        assert!(check_transfer(hs, &m, id(1), id(9)).is_err());
        assert!(check_transfer(ClusterMode::Consensus, &m, id(1), id(2)).is_err());
    }
}
