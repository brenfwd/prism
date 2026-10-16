//! openraft type configuration and the aliases the rest of the crate uses.

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
    /// The member's permanent identity, which survives a change of raft ID.
    pub node_id: Uuid,
    /// The member's display name.
    pub name: String,
    /// `[mesh address]:port` of the member's gRPC server, for example
    /// `[fd00:1::2]:7000`. Used verbatim as the host part of an `http://` URL.
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

/// A log position: term, node and index.
pub type LogId = openraft::LogId<RaftId>;
/// A persisted vote: term and the candidate voted for.
pub type Vote = openraft::Vote<RaftId>;
/// A log entry carrying a blank, a JSON command or a membership change.
pub type Entry = openraft::Entry<TypeConfig>;
/// A membership configuration with the log position that set it.
pub type StoredMembership = openraft::StoredMembership<RaftId, PrismNode>;
/// Snapshot metadata: last log position, membership and snapshot ID.
pub type SnapshotMeta = openraft::SnapshotMeta<RaftId, PrismNode>;
/// A snapshot with its metadata and data cursor.
pub type Snapshot = openraft::Snapshot<TypeConfig>;
/// The openraft handle, specialised to Prism's types.
pub type Raft = openraft::Raft<TypeConfig>;
/// openraft's storage error, specialised to [`RaftId`].
pub type StorageError = openraft::StorageError<RaftId>;

/// What the state machine has applied: stored by the application in the same
/// transaction as the command itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedMeta {
    /// The last log entry applied, or `None` before the first.
    pub last_applied: Option<LogId>,
    /// The membership as of the last membership entry applied.
    pub membership: StoredMembership,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prism_node_round_trips_through_json() {
        let node = PrismNode {
            node_id: Uuid::from_u128(7),
            name: "hub".into(),
            addr: "[fd00::1]:7000".into(),
        };
        let json = serde_json::to_string(&node).unwrap();
        assert_eq!(serde_json::from_str::<PrismNode>(&json).unwrap(), node);
    }

    #[test]
    fn applied_meta_round_trips_through_json() {
        let meta = AppliedMeta {
            last_applied: Some(openraft::LogId::new(
                openraft::CommittedLeaderId::new(2, 1),
                9,
            )),
            membership: StoredMembership::default(),
        };
        let json = serde_json::to_string(&meta).unwrap();
        assert_eq!(serde_json::from_str::<AppliedMeta>(&json).unwrap(), meta);
    }
}
