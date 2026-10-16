//! Cluster membership and the replicated command log.
//!
//! Hub-spoke is a single-voter raft group whose other members are learners;
//! consensus mode has 3 or 5 voters. Commands are opaque JSON strings applied
//! by the application's [`Backend`]. The crate provides the SQLite log
//! ([`LogStore`]), the state machine ([`StateMachine`]), the gRPC transport
//! ([`GrpcNetwork`], [`RaftService`]) and [`ClusterNode`], which wraps the
//! openraft handle with deadlines on writes and membership changes.
#![warn(missing_docs)]

pub mod network;
pub mod sm;
pub mod store;
mod sync;
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

/// How long a membership change may take. A change commits two log entries
/// (joint and final configuration), so it gets three write timeouts.
pub const MEMBERSHIP_TIMEOUT: Duration = Duration::from_secs(30);

/// Interval between leader heartbeats, in milliseconds. A `WireGuard` hop
/// between members is a few milliseconds, so 250 ms is many round trips.
const HEARTBEAT_MS: u64 = 250;
/// Shortest election timeout, in milliseconds: six missed heartbeats.
const ELECTION_TIMEOUT_MIN_MS: u64 = 1500;
/// Longest election timeout, in milliseconds. The random spread between the
/// two limits keeps followers from calling elections together.
const ELECTION_TIMEOUT_MAX_MS: u64 = 3000;
/// Log entries since the last snapshot that trigger a new one.
const SNAPSHOT_EVERY_ENTRIES: u64 = 5000;
/// Log entries kept after a snapshot so slightly lagging followers can catch
/// up from the log instead of a snapshot.
const LOGS_KEPT_AFTER_SNAPSHOT: u64 = 1000;
/// Time a follower gets to receive a whole snapshot, in milliseconds.
const INSTALL_SNAPSHOT_TIMEOUT_MS: u64 = 10_000;

/// Why the cluster cannot accept a change right now.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DegradedReason {
    /// A proposal did not commit within [`WRITE_TIMEOUT`].
    #[error("no quorum to commit the change")]
    WriteNoQuorum,
    /// A membership change did not commit within [`MEMBERSHIP_TIMEOUT`].
    #[error("no quorum to change raft membership")]
    MembershipNoQuorum,
    /// The local state machine had not applied `index` when the wait ended.
    #[error("not caught up to log index {index}")]
    NotCaughtUp {
        /// The log index that was awaited.
        index: u64,
    },
    /// This process has no raft instance.
    #[error("raft is not running")]
    NotRunning,
}

/// Why a [`ClusterNode`] operation failed.
#[derive(Debug, thiserror::Error)]
pub enum ClusterError {
    /// This node is not the leader; propose there instead. Carries the
    /// leader's ID and address when this node knows them.
    #[error("not the leader; leader is {0:?}")]
    ForwardToLeader(Option<(RaftId, PrismNode)>),
    /// No leader or no quorum: configuration is frozen.
    #[error("cluster is degraded: {0}")]
    Degraded(DegradedReason),
    /// openraft rejected the request or stopped. The source is the openraft
    /// error, which tells fatal errors from rejected requests.
    #[error(transparent)]
    Raft(Box<dyn std::error::Error + Send + Sync>),
}

impl ClusterError {
    fn raft(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Raft(Box::new(e))
    }
}

/// The result of a committed write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutcome {
    /// The JSON string the [`Backend`] returned for the command.
    pub result: String,
    /// The log index the command was committed at.
    pub index: u64,
}

/// The local raft member.
///
/// `raft` is the underlying openraft handle. Calling it directly skips the
/// deadlines that [`write`](Self::write) and the membership methods apply.
#[derive(Clone)]
pub struct ClusterNode {
    /// This node's raft ID.
    pub id: RaftId,
    /// The openraft handle.
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

/// Builds the raft configuration for `cluster_name`, with timings suited to
/// `WireGuard` links: 250 ms heartbeats, elections after 1.5 to 3 seconds,
/// a snapshot every 5000 entries and 1000 entries kept after it.
///
/// # Errors
///
/// Returns an error if openraft rejects the configuration,
/// which does not happen with the constants above.
pub fn raft_config(cluster_name: &str) -> anyhow::Result<Arc<Config>> {
    let cfg = Config {
        cluster_name: cluster_name.to_string(),
        heartbeat_interval: HEARTBEAT_MS,
        election_timeout_min: ELECTION_TIMEOUT_MIN_MS,
        election_timeout_max: ELECTION_TIMEOUT_MAX_MS,
        snapshot_policy: SnapshotPolicy::LogsSinceLast(SNAPSHOT_EVERY_ENTRIES),
        max_in_snapshot_log_to_keep: LOGS_KEPT_AFTER_SNAPSHOT,
        install_snapshot_timeout: INSTALL_SNAPSHOT_TIMEOUT_MS,
        ..Config::default()
    };
    Ok(Arc::new(cfg.validate()?))
}

impl ClusterNode {
    /// Starts the raft instance for `id` over `log`, applying commands
    /// through `backend`.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend cannot report its applied meta or
    /// openraft cannot start.
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

    /// Creates a new single-voter cluster with this node as the hub. Call it
    /// once, on a node whose log is empty.
    ///
    /// # Errors
    ///
    /// Returns openraft's error if the node is already initialized.
    pub async fn initialize(&self, me: PrismNode) -> anyhow::Result<()> {
        self.raft
            .initialize(BTreeMap::from([(self.id, me)]))
            .await?;
        Ok(())
    }

    /// A snapshot of openraft's metrics for this node.
    pub fn metrics(&self) -> RaftMetrics<RaftId, PrismNode> {
        self.raft.metrics().borrow().clone()
    }

    /// The current leader's ID and address, or `None` while there is no
    /// leader or its address is not in the membership.
    pub fn leader(&self) -> Option<(RaftId, PrismNode)> {
        let m = self.metrics();
        let id = m.current_leader?;
        let node = m.membership_config.membership().get_node(&id)?.clone();
        Some((id, node))
    }

    /// Whether this node is the leader right now.
    pub fn is_leader(&self) -> bool {
        self.metrics().state == ServerState::Leader
    }

    /// The index of the last log entry the local state machine has applied.
    pub fn applied_index(&self) -> u64 {
        *self.applied.borrow()
    }

    /// Waits until the local state machine has applied `index`.
    ///
    /// # Errors
    ///
    /// Returns [`DegradedReason::NotCaughtUp`] after `timeout`, or
    /// [`ClusterError::Raft`] if the state machine has shut down.
    pub async fn wait_applied(&self, index: u64, timeout: Duration) -> Result<(), ClusterError> {
        let mut rx = self.applied.clone();
        match tokio::time::timeout(timeout, rx.wait_for(|a| *a >= index)).await {
            Err(_) => Err(ClusterError::Degraded(DegradedReason::NotCaughtUp {
                index,
            })),
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(ClusterError::raft(e)),
        }
    }

    /// Proposes a JSON command on this node, which must be the leader, and
    /// waits up to [`WRITE_TIMEOUT`] for it to commit and apply.
    ///
    /// # Errors
    ///
    /// Returns [`ClusterError::ForwardToLeader`] on a follower,
    /// [`DegradedReason::WriteNoQuorum`] on timeout, and
    /// [`ClusterError::Raft`] for any other openraft error.
    pub async fn write(&self, command: String) -> Result<WriteOutcome, ClusterError> {
        match tokio::time::timeout(WRITE_TIMEOUT, self.raft.client_write(command)).await {
            Err(_) => Err(ClusterError::Degraded(DegradedReason::WriteNoQuorum)),
            Ok(Ok(resp)) => Ok(WriteOutcome {
                result: resp.data,
                index: resp.log_id.index,
            }),
            Ok(Err(RaftError::APIError(ClientWriteError::ForwardToLeader(f)))) => Err(
                ClusterError::ForwardToLeader(f.leader_id.zip(f.leader_node)),
            ),
            Ok(Err(e)) => Err(ClusterError::raft(e)),
        }
    }

    /// Adds a learner without waiting for it to catch up.
    ///
    /// # Errors
    ///
    /// Returns [`ClusterError::Raft`] if openraft rejects the change, for
    /// example on a follower.
    pub async fn add_learner(&self, id: RaftId, node: PrismNode) -> Result<(), ClusterError> {
        self.raft
            .add_learner(id, node, false)
            .await
            .map(drop)
            .map_err(ClusterError::raft)
    }

    /// Sets the voter set (consensus mode); others stay learners.
    ///
    /// # Errors
    ///
    /// Returns [`DegradedReason::MembershipNoQuorum`] after
    /// [`MEMBERSHIP_TIMEOUT`] and [`ClusterError::Raft`] if openraft rejects
    /// the change.
    pub async fn set_voters(&self, voters: BTreeSet<RaftId>) -> Result<(), ClusterError> {
        let change = self
            .raft
            .change_membership(ChangeMembers::ReplaceAllVoters(voters), true);
        membership_change(change).await
    }

    /// Removes members entirely (voters must be removed from the voter set first).
    ///
    /// # Errors
    ///
    /// Same as [`set_voters`](Self::set_voters).
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
    E: std::error::Error + Send + Sync + 'static,
{
    match tokio::time::timeout(MEMBERSHIP_TIMEOUT, change).await {
        Err(_) => Err(ClusterError::Degraded(DegradedReason::MembershipNoQuorum)),
        Ok(r) => r.map(drop).map_err(ClusterError::raft),
    }
}

#[cfg(test)]
mod tests;
