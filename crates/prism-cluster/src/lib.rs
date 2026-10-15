//! Cluster membership and the replicated command log.
//!
//! Hub-spoke is a single-voter raft group whose other members are learners;
//! consensus mode has 3 or 5 voters. Commands are opaque JSON applied by the
//! application's [`Backend`].

// openraft's storage traits are async and return its large StorageError;
// the SQLite calls behind them are synchronous.
#![allow(clippy::unused_async_trait_impl, clippy::result_large_err)]

pub mod network;
pub mod sm;
pub mod store;
pub mod types;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use openraft::error::{ClientWriteError, RaftError};
use openraft::{ChangeMembers, Config, RaftMetrics, ServerState, SnapshotPolicy};
use tokio::sync::watch;

pub use network::{Channels, GrpcNetwork, RaftService};
pub use sm::{Backend, StateMachine};
pub use store::LogStore;
pub use types::{AppliedMeta, PrismNode, Raft, RaftId, TypeConfig};

/// How long a proposal may wait for commit before the cluster counts as degraded.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum ClusterError {
    /// This node is not the leader; propose there instead.
    #[error("not the leader; leader is {0:?}")]
    ForwardToLeader(Option<(RaftId, PrismNode)>),
    /// No leader or no quorum: configuration is frozen.
    #[error("cluster is degraded: {0}")]
    Degraded(String),
    #[error("{0}")]
    Other(String),
}

/// The local raft member.
#[derive(Clone)]
pub struct ClusterNode {
    pub id: RaftId,
    pub raft: Raft,
    applied: watch::Receiver<u64>,
}

impl std::fmt::Debug for ClusterNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterNode")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Timeouts tuned for `WireGuard` links: ~1 RTT heartbeats, elections after
/// a few missed beats.
pub fn raft_config(cluster_name: &str) -> anyhow::Result<Arc<Config>> {
    let cfg = Config {
        cluster_name: cluster_name.to_string(),
        heartbeat_interval: 250,
        election_timeout_min: 1500,
        election_timeout_max: 3000,
        snapshot_policy: SnapshotPolicy::LogsSinceLast(5000),
        max_in_snapshot_log_to_keep: 1000,
        install_snapshot_timeout: 10_000,
        ..Config::default()
    };
    Ok(Arc::new(cfg.validate()?))
}

impl ClusterNode {
    pub async fn start<N>(
        id: RaftId,
        config: Arc<Config>,
        network: N,
        log: LogStore,
        backend: Arc<dyn Backend>,
    ) -> anyhow::Result<Self>
    where
        N: openraft::RaftNetworkFactory<TypeConfig>,
    {
        let sm = StateMachine::new(backend)?;
        let applied = sm.applied_index();
        let raft = Raft::new(id, config, network, log, sm).await?;
        Ok(Self { id, raft, applied })
    }

    /// Creates a new single-voter cluster with this node as the hub.
    pub async fn initialize(&self, me: PrismNode) -> anyhow::Result<()> {
        let mut members = BTreeMap::new();
        members.insert(self.id, me);
        self.raft.initialize(members).await?;
        Ok(())
    }

    pub fn metrics(&self) -> RaftMetrics<RaftId, PrismNode> {
        self.raft.metrics().borrow().clone()
    }

    pub fn leader(&self) -> Option<(RaftId, PrismNode)> {
        let m = self.metrics();
        let id = m.current_leader?;
        let node = m.membership_config.membership().get_node(&id)?.clone();
        Some((id, node))
    }

    pub fn is_leader(&self) -> bool {
        self.metrics().state == ServerState::Leader
    }

    pub fn applied_index(&self) -> u64 {
        *self.applied.borrow()
    }

    /// Waits until the local state machine has applied `index`.
    pub async fn wait_applied(&self, index: u64, timeout: Duration) -> Result<(), ClusterError> {
        let mut rx = self.applied.clone();
        tokio::time::timeout(timeout, rx.wait_for(|a| *a >= index))
            .await
            .map_err(|_| ClusterError::Degraded(format!("not caught up to log index {index}")))?
            .map(drop)
            .map_err(|e| ClusterError::Other(e.to_string()))
    }

    /// Proposes a command on this node, which must be the leader. Returns
    /// the backend's result and the log index.
    pub async fn write(&self, command: String) -> Result<(String, u64), ClusterError> {
        match tokio::time::timeout(WRITE_TIMEOUT, self.raft.client_write(command)).await {
            Err(_) => Err(ClusterError::Degraded(
                "no quorum to commit the change".into(),
            )),
            Ok(Ok(resp)) => Ok((resp.data, resp.log_id.index)),
            Ok(Err(RaftError::APIError(ClientWriteError::ForwardToLeader(f)))) => Err(
                ClusterError::ForwardToLeader(f.leader_id.zip(f.leader_node)),
            ),
            Ok(Err(e)) => Err(ClusterError::Other(e.to_string())),
        }
    }

    /// Adds a learner without waiting for it to catch up.
    pub async fn add_learner(&self, id: RaftId, node: PrismNode) -> Result<(), ClusterError> {
        self.raft
            .add_learner(id, node, false)
            .await
            .map(drop)
            .map_err(|e| ClusterError::Other(e.to_string()))
    }

    /// Sets the voter set (consensus mode); others stay learners.
    pub async fn set_voters(&self, voters: BTreeSet<RaftId>) -> Result<(), ClusterError> {
        let change = self
            .raft
            .change_membership(ChangeMembers::ReplaceAllVoters(voters), true);
        membership_change(change).await
    }

    /// Removes members entirely (voters must be removed from the voter set first).
    pub async fn remove_nodes(&self, ids: BTreeSet<RaftId>) -> Result<(), ClusterError> {
        let change = self
            .raft
            .change_membership(ChangeMembers::RemoveNodes(ids), false);
        membership_change(change).await
    }
}

/// Runs a membership change with a deadline so a lost quorum cannot wedge
/// the caller.
async fn membership_change<F, T, E>(change: F) -> Result<(), ClusterError>
where
    F: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    match tokio::time::timeout(WRITE_TIMEOUT * 3, change).await {
        Err(_) => Err(ClusterError::Degraded(
            "no quorum to change raft membership".into(),
        )),
        Ok(r) => r.map(drop).map_err(|e| ClusterError::Other(e.to_string())),
    }
}

#[cfg(test)]
mod tests;
