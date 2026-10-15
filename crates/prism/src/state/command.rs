//! The commands that change desired state, and their outcomes.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{PoolDoc, StateError};
use crate::archive::{ArchiveFileDoc, ArchiveMemberDoc, ArchivePoolDoc};
use crate::cluster::model::{ClusterDoc, InviteDoc, MemberDoc, MemberState};
use crate::network::NetworkDoc;
use crate::spec::VmSpec;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    /// Creates or replaces a VM. With `expect_generation`, fails unless the
    /// stored VM has that generation (`None` inside means "must not exist").
    PutVm {
        vm: Box<VmSpec>,
        #[allow(clippy::option_option)] // "don't check" vs. "must not exist"
        expect_generation: Option<Option<u64>>,
    },
    DeleteVm {
        id: Uuid,
    },
    PutPool {
        pool: PoolDoc,
    },
    DeletePool {
        node: Uuid,
        name: String,
    },
    /// The first entry of a new cluster: the cluster and its hub.
    InitCluster {
        cluster: ClusterDoc,
        hub: MemberDoc,
    },
    /// Reserves a raft ID and mesh address for a joining node.
    CreateInvite {
        invite: InviteDoc,
    },
    RevokeInvite {
        invite_id: String,
    },
    /// Admits a node and consumes its invite.
    AddMember {
        member: MemberDoc,
        invite_id: String,
        now: i64,
    },
    SetMemberState {
        node_id: Uuid,
        state: MemberState,
    },
    SetVoter {
        node_id: Uuid,
        voter: bool,
    },
    /// Removes a member and tombstones its identity.
    RemoveMember {
        node_id: Uuid,
        now: i64,
    },
    PutArchivePool {
        pool: ArchivePoolDoc,
    },
    /// Registers or updates a node's member directory (creates the pool on
    /// first use).
    PutArchiveMember {
        member: ArchiveMemberDoc,
    },
    /// Records a file. With `create`, fails if the path exists.
    PutArchiveFile {
        file: ArchiveFileDoc,
        create: bool,
    },
    /// Adds a verified replica to an existing file.
    AddArchiveReplica {
        pool: String,
        path: String,
        replica: crate::archive::Replica,
    },
    /// Forgets a replica (drained, missing or corrupt); the last one stays.
    RemoveArchiveReplica {
        pool: String,
        path: String,
        replica: crate::archive::Replica,
    },
    DeleteArchiveFile {
        pool: String,
        path: String,
    },
    PutBackup {
        backup: Box<crate::node::BackupDoc>,
    },
    DeleteBackup {
        id: Uuid,
    },
    /// Creates or updates a network. A new one gets a VNI and a gateway on
    /// every member.
    PutNetwork {
        network: Box<NetworkDoc>,
        now: i64,
    },
    /// Deletes an unused network; its VNI cools down before reuse.
    DeleteNetwork {
        id: Uuid,
        now: i64,
    },
}

/// A command's outcome as it travels through the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "detail", rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Conflict(String),
    NotFound(String),
}

impl Outcome {
    pub(super) fn into_result(self) -> Result<(), StateError> {
        match self {
            Self::Ok => Ok(()),
            Self::Conflict(m) => Err(StateError::Conflict(m)),
            Self::NotFound(m) => Err(StateError::NotFound(m)),
        }
    }
}
