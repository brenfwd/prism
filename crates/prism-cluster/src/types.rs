//! openraft type configuration.

use std::fmt;
use std::io::Cursor;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Raft node IDs are small integers assigned at join; the stable identity is
/// [`PrismNode::node_id`].
pub type RaftId = u64;

/// A cluster member as raft sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrismNode {
    pub node_id: Uuid,
    pub name: String,
    /// `[mesh address]:port` of the member's gRPC server.
    pub addr: String,
}

impl fmt::Display for PrismNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name, self.addr)
    }
}

openraft::declare_raft_types!(
    /// Prism's raft types: commands and results are JSON strings owned by
    /// the application's state machine.
    pub TypeConfig:
        D = String,
        R = String,
        NodeId = RaftId,
        Node = PrismNode,
        SnapshotData = Cursor<Vec<u8>>,
);

pub type LogId = openraft::LogId<RaftId>;
pub type Vote = openraft::Vote<RaftId>;
pub type Entry = openraft::Entry<TypeConfig>;
pub type StoredMembership = openraft::StoredMembership<RaftId, PrismNode>;
pub type SnapshotMeta = openraft::SnapshotMeta<RaftId, PrismNode>;
pub type Snapshot = openraft::Snapshot<TypeConfig>;
pub type Raft = openraft::Raft<TypeConfig>;
pub type StorageError = openraft::StorageError<RaftId>;

/// What the state machine has applied: stored by the application in the same
/// transaction as the command itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedMeta {
    pub last_applied: Option<LogId>,
    pub membership: StoredMembership,
}
