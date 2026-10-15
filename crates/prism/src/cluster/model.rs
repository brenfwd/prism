//! Replicated cluster documents: the cluster itself, members, invites and
//! tombstones.

use std::net::Ipv6Addr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CLUSTER: &str = "cluster";
pub const MEMBERS: &str = "member";
pub const INVITES: &str = "invite";
pub const TOMBSTONES: &str = "tombstone";

/// Cluster feature level; peers must be within one of each other.
pub const FEATURE_LEVEL: u32 = 1;

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
    pub cluster_id: Uuid,
    pub name: String,
    pub mode: ClusterMode,
    /// The ULA `/48`, as its first 48 bits (for example `fd12:3456:789a::`).
    pub mesh_prefix: Ipv6Addr,
    /// `wg-prism` MTU, the same on every member.
    pub wg_mtu: u16,
    pub feature_level: u32,
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
    pub node_id: Uuid,
    pub raft_id: u64,
    pub name: String,
    /// `WireGuard` public key, base64.
    pub wg_pubkey: String,
    pub mesh_addr: Ipv6Addr,
    /// Underlay `host:port` endpoints, preference-ordered; may be empty.
    pub endpoints: Vec<String>,
    pub state: MemberState,
    /// Votes in raft (the hub, or a consensus voter).
    pub voter: bool,
    pub version: String,
    pub joined: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteDoc {
    /// 128-bit random, hex. Names the reservation; not a secret.
    pub invite_id: String,
    pub raft_id: u64,
    pub assigned_addr: Ipv6Addr,
    pub exp: i64,
    pub created_by: Uuid,
    pub name_hint: Option<String>,
    /// The node that consumed it, making replays idempotent.
    pub consumed_by: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TombstoneDoc {
    pub node_id: Uuid,
    pub wg_pubkey: String,
    pub mesh_addr: Ipv6Addr,
    pub removed: i64,
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
