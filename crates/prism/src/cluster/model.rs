//! Replicated cluster documents: the cluster itself, members, invites and
//! tombstones.

use std::net::Ipv6Addr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Collection holding the single cluster document.
pub const CLUSTER: &str = "cluster";
/// Collection holding member documents, keyed by node id.
pub const MEMBERS: &str = "member";
/// Collection holding invite documents, keyed by invite id.
pub const INVITES: &str = "invite";
/// Collection holding tombstones of removed members.
pub const TOMBSTONES: &str = "tombstone";

/// Cluster feature level; peers must be within one of each other.
/// Level 2 adds networks (`PutNetwork`, `DeleteNetwork`, `SetFeatureLevel`).
/// Level 3 replicates the archive timers (`StampArchiveScrub`, `SetArchiveOffline`).
pub const FEATURE_LEVEL: u32 = 3;

/// Rolling-upgrade rule: nodes may differ by at most one feature level.
pub fn levels_compatible(a: u32, b: u32) -> bool {
    a.abs_diff(b) <= 1
}

/// A feature gated on `required` is usable once the cluster level (the
/// minimum all nodes support, raised explicitly) has reached it and this
/// node supports it too.
pub fn feature_enabled(cluster_level: u32, required: u32) -> bool {
    cluster_level >= required && FEATURE_LEVEL >= required
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterMode {
    /// One voter (the hub); every other member is a learner.
    HubSpoke,
    /// Three or five voters.
    Consensus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterDoc {
    /// Unique id of the cluster.
    pub cluster_id: Uuid,
    /// The cluster's name.
    pub name: String,
    /// Whether the cluster has a hub or elects a leader.
    pub mode: ClusterMode,
    /// The ULA `/48`, as its first 48 bits (for example `fd12:3456:789a::`).
    pub mesh_prefix: Ipv6Addr,
    /// `wg-prism` MTU, the same on every member.
    pub wg_mtu: u16,
    /// The level that every member's commands are limited to.
    pub feature_level: u32,
    /// Creation time, in Unix seconds.
    pub created: i64,
}

impl ClusterDoc {
    /// The mesh address of the member with raft ID `id`:
    /// `<prefix>:1::<id>`.
    pub fn mesh_addr(&self, id: u64) -> Ipv6Addr {
        let p = self.mesh_prefix.segments();
        Ipv6Addr::new(
            p[0],
            p[1],
            p[2],
            1,
            0,
            (id >> 32) as u16,
            (id >> 16) as u16,
            id as u16,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberState {
    /// Admitted; may only call `Cluster.Bootstrap` and `Cluster.Status`.
    Joining,
    /// Caught up; a full member.
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberDoc {
    /// Node id.
    pub node_id: Uuid,
    /// Raft id, also encoded in the mesh address.
    pub raft_id: u64,
    /// Display name.
    pub name: String,
    /// `WireGuard` public key, base64.
    pub wg_pubkey: String,
    /// The member's mesh address.
    pub mesh_addr: Ipv6Addr,
    /// Underlay `host:port` endpoints, preference-ordered; may be empty.
    pub endpoints: Vec<String>,
    /// Where the member is in its life cycle.
    pub state: MemberState,
    /// Votes in raft (the hub, or a consensus voter).
    pub voter: bool,
    /// prism version the member runs.
    pub version: String,
    /// Highest feature level this member's binary supports; documents from
    /// before levels were tracked read as 1.
    #[serde(default = "one")]
    pub feature_level: u32,
    /// Time the member was admitted, in Unix seconds.
    pub joined: i64,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteDoc {
    /// 128-bit random, hex. Names the reservation; not a secret.
    pub invite_id: String,
    /// Raft id reserved for the invited node.
    pub raft_id: u64,
    /// Mesh address reserved for the invited node.
    pub assigned_addr: Ipv6Addr,
    /// Expiry time, in Unix seconds.
    pub exp: i64,
    /// Node that created the invite.
    pub created_by: Uuid,
    /// Suggested name for the joining node.
    pub name_hint: Option<String>,
    /// The node that consumed it, making replays idempotent.
    pub consumed_by: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TombstoneDoc {
    /// Node id of the removed member.
    pub node_id: Uuid,
    /// The removed member's `WireGuard` public key, base64.
    pub wg_pubkey: String,
    /// The mesh address that stays reserved.
    pub mesh_addr: Ipv6Addr,
    /// Removal time, in Unix seconds.
    pub removed: i64,
    /// Log index of the removal; a member has applied it once its
    /// `applied_index` reaches this.
    #[serde(default)]
    pub removed_index: u64,
}

/// Reasons a member is not admitted; stable codes for the API.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmitError {
    #[error("invite_not_found: no such invite")]
    InviteNotFound,
    #[error("invite_expired: the invite expired; create a new one")]
    InviteExpired,
    #[error("invite_consumed: this invite was already used by another node")]
    InviteConsumed,
    #[error("address_mismatch: the reply does not match the invite's reserved address")]
    AddressMismatch,
    #[error("member_exists: this node or key is already a member")]
    MemberExists,
    #[error(
        "member_tombstoned: this node was removed; run `prism leave --force` on it, then join with a new invite"
    )]
    Tombstoned,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_addresses() {
        let c = ClusterDoc {
            cluster_id: Uuid::nil(),
            name: "c".into(),
            mode: ClusterMode::HubSpoke,
            mesh_prefix: "fd12:3456:789a::".parse().unwrap(),
            wg_mtu: 1420,
            feature_level: 1,
            created: 0,
        };
        assert_eq!(c.mesh_addr(1).to_string(), "fd12:3456:789a:1::1");
        assert_eq!(c.mesh_addr(0x1_0002).to_string(), "fd12:3456:789a:1::1:2");
    }
}

#[cfg(test)]
mod level_tests {
    use super::*;

    #[test]
    fn skew_rules() {
        assert!(levels_compatible(1, 2));
        assert!(!levels_compatible(1, 3));
        assert!(feature_enabled(1, 1));
        assert!(!feature_enabled(1, 2));
        assert!(levels_compatible(2, 2));
        assert!(levels_compatible(2, 1));
        assert!(!feature_enabled(0, 1));
        assert!(!feature_enabled(5, FEATURE_LEVEL + 1));
    }
}
