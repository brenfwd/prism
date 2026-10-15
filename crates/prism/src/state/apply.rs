//! The deterministic state machine: how each command changes the documents.

use uuid::Uuid;

use super::{CLUSTER_ID, Command, Outcome, POOLS, PoolDoc, VMS};
use crate::archive::{
    ARCHIVE_FILES, ARCHIVE_MEMBERS, ARCHIVE_POOLS, ArchiveFileDoc, ArchivePoolDoc,
};
use crate::cluster::model::{
    AdmitError, CLUSTER, ClusterDoc, INVITES, InviteDoc, MEMBERS, MemberDoc, TOMBSTONES,
    TombstoneDoc,
};
use crate::db::Docs;
use crate::spec::VmSpec;

/// Applies one command inside a transaction. Deterministic.
pub(super) fn apply_command(d: &Docs<'_>, local: Uuid, cmd: &Command) -> anyhow::Result<Outcome> {
    match cmd {
        Command::PutVm {
            vm,
            expect_generation,
        } => {
            let current: Option<VmSpec> = d.get(VMS, &vm.id.to_string())?;
            if let Some(expected) = expect_generation {
                let actual = current.as_ref().map(|c| c.generation);
                if actual != *expected {
                    return Ok(Outcome::Conflict(format!(
                        "vm {} changed concurrently (generation {actual:?}, expected {expected:?})",
                        vm.name
                    )));
                }
            }
            if d.list::<VmSpec>(VMS)?
                .iter()
                .any(|o| o.name == vm.name && o.id != vm.id)
            {
                return Ok(Outcome::Conflict(format!(
                    "a VM named {} already exists",
                    vm.name
                )));
            }
            d.put(VMS, &vm.id.to_string(), vm)?;
        }
        Command::DeleteVm { id } => {
            if !d.delete(VMS, &id.to_string())? {
                return Ok(Outcome::NotFound(format!("vm {id}")));
            }
        }
        Command::PutPool { pool } => {
            d.put(POOLS, &PoolDoc::key(pool.node, &pool.name), pool)?;
        }
        Command::DeletePool { node, name } => {
            let removed = d.delete(POOLS, &PoolDoc::key(*node, name))?
                || (*node == local && d.delete(POOLS, name)?);
            if !removed {
                return Ok(Outcome::NotFound(format!("pool {name}")));
            }
        }
        Command::InitCluster { cluster, hub } => {
            if d.get::<ClusterDoc>(CLUSTER, CLUSTER_ID)?.is_some() {
                return Ok(Outcome::Conflict(
                    "the cluster is already initialised".into(),
                ));
            }
            d.put(CLUSTER, CLUSTER_ID, cluster)?;
            d.put(MEMBERS, &hub.node_id.to_string(), hub)?;
            // VMs and pools created before clustering belong to the hub.
            for mut vm in d.list::<VmSpec>(VMS)? {
                if vm.node.is_nil() {
                    vm.node = hub.node_id;
                    d.put(VMS, &vm.id.to_string(), &vm)?;
                }
            }
            for (key, mut pool) in d
                .list::<PoolDoc>(POOLS)?
                .into_iter()
                .map(|p| (p.name.clone(), p))
            {
                if pool.node.is_nil() {
                    d.delete(POOLS, &key)?;
                    pool.node = hub.node_id;
                    d.put(POOLS, &PoolDoc::key(pool.node, &pool.name), &pool)?;
                }
            }
        }
        Command::CreateInvite { invite } => {
            let taken = d
                .list::<MemberDoc>(MEMBERS)?
                .iter()
                .any(|m| m.raft_id == invite.raft_id || m.mesh_addr == invite.assigned_addr)
                || d.list::<InviteDoc>(INVITES)?.iter().any(|i| {
                    i.raft_id == invite.raft_id || i.assigned_addr == invite.assigned_addr
                })
                || d.list::<TombstoneDoc>(TOMBSTONES)?
                    .iter()
                    .any(|t| t.mesh_addr == invite.assigned_addr);
            if taken {
                return Ok(Outcome::Conflict(
                    "that mesh address is already reserved; retry".into(),
                ));
            }
            d.put(INVITES, &invite.invite_id, invite)?;
        }
        Command::RevokeInvite { invite_id } => {
            if !d.delete(INVITES, invite_id)? {
                return Ok(Outcome::NotFound(format!("invite {invite_id}")));
            }
        }
        Command::AddMember {
            member,
            invite_id,
            now,
        } => {
            if let Err(e) = admit(d, member, invite_id, *now)? {
                return Ok(Outcome::Conflict(e.to_string()));
            }
        }
        Command::SetMemberState { node_id, state } => {
            let Some(mut m) = d.get::<MemberDoc>(MEMBERS, &node_id.to_string())? else {
                return Ok(Outcome::NotFound(format!("member {node_id}")));
            };
            m.state = *state;
            d.put(MEMBERS, &node_id.to_string(), &m)?;
        }
        Command::SetVoter { node_id, voter } => {
            let Some(mut m) = d.get::<MemberDoc>(MEMBERS, &node_id.to_string())? else {
                return Ok(Outcome::NotFound(format!("member {node_id}")));
            };
            m.voter = *voter;
            d.put(MEMBERS, &node_id.to_string(), &m)?;
        }
        Command::RemoveMember { node_id, now } => {
            let Some(m) = d.get::<MemberDoc>(MEMBERS, &node_id.to_string())? else {
                return Ok(Outcome::NotFound(format!("member {node_id}")));
            };
            if d.list::<VmSpec>(VMS)?.iter().any(|v| v.node == *node_id) {
                return Ok(Outcome::Conflict(format!(
                    "{} still owns VMs; move or delete them first",
                    m.name
                )));
            }
            d.delete(MEMBERS, &node_id.to_string())?;
            d.put(
                TOMBSTONES,
                &node_id.to_string(),
                &TombstoneDoc {
                    node_id: *node_id,
                    wg_pubkey: m.wg_pubkey,
                    mesh_addr: m.mesh_addr,
                    removed: *now,
                },
            )?;
            for pool in d
                .list::<PoolDoc>(POOLS)?
                .into_iter()
                .filter(|p| p.node == *node_id)
            {
                d.delete(POOLS, &PoolDoc::key(pool.node, &pool.name))?;
            }
        }
        Command::PutArchivePool { pool } => {
            d.put(ARCHIVE_POOLS, &pool.name, pool)?;
        }
        Command::PutArchiveMember { member } => {
            if d.get::<ArchivePoolDoc>(ARCHIVE_POOLS, &member.pool)?
                .is_none()
            {
                d.put(
                    ARCHIVE_POOLS,
                    &member.pool,
                    &ArchivePoolDoc {
                        name: member.pool.clone(),
                        replicas: 1,
                    },
                )?;
            }
            d.put(ARCHIVE_MEMBERS, &member.key(), member)?;
        }
        Command::PutArchiveFile { file, create } => {
            if d.get::<ArchivePoolDoc>(ARCHIVE_POOLS, &file.pool)?
                .is_none()
            {
                return Ok(Outcome::NotFound(format!("archive pool {}", file.pool)));
            }
            let key = ArchiveFileDoc::key(&file.pool, &file.path);
            if *create && d.get::<ArchiveFileDoc>(ARCHIVE_FILES, &key)?.is_some() {
                return Ok(Outcome::Conflict(format!(
                    "{} already exists in {}",
                    file.path, file.pool
                )));
            }
            d.put(ARCHIVE_FILES, &key, file)?;
        }
        Command::DeleteArchiveFile { pool, path } => {
            if !d.delete(ARCHIVE_FILES, &ArchiveFileDoc::key(pool, path))? {
                return Ok(Outcome::NotFound(format!("{pool}/{path}")));
            }
        }
        Command::PutBackup { backup } => {
            d.put(crate::node::BACKUPS, &backup.id.to_string(), backup)?;
        }
        Command::DeleteBackup { id } => {
            if !d.delete(crate::node::BACKUPS, &id.to_string())? {
                return Ok(Outcome::NotFound(format!("backup {id}")));
            }
        }
    }
    Ok(Outcome::Ok)
}

/// The admission checks from the join protocol. Replaying the same
/// `AddMember` is a no-op success.
fn admit(
    d: &Docs<'_>,
    member: &MemberDoc,
    invite_id: &str,
    now: i64,
) -> anyhow::Result<Result<(), AdmitError>> {
    let Some(mut invite) = d.get::<InviteDoc>(INVITES, invite_id)? else {
        return Ok(Err(AdmitError::InviteNotFound));
    };
    match invite.consumed_by {
        Some(n) if n == member.node_id => return Ok(Ok(())),
        Some(_) => return Ok(Err(AdmitError::InviteConsumed)),
        None => {}
    }
    if now > invite.exp {
        return Ok(Err(AdmitError::InviteExpired));
    }
    if member.mesh_addr != invite.assigned_addr || member.raft_id != invite.raft_id {
        return Ok(Err(AdmitError::AddressMismatch));
    }
    let tombstoned = d
        .list::<TombstoneDoc>(TOMBSTONES)?
        .iter()
        .any(|t| t.node_id == member.node_id || t.wg_pubkey == member.wg_pubkey);
    if tombstoned {
        return Ok(Err(AdmitError::Tombstoned));
    }
    let exists = d
        .list::<MemberDoc>(MEMBERS)?
        .iter()
        .any(|m| m.node_id == member.node_id || m.wg_pubkey == member.wg_pubkey);
    if exists {
        return Ok(Err(AdmitError::MemberExists));
    }
    invite.consumed_by = Some(member.node_id);
    d.put(INVITES, invite_id, &invite)?;
    d.put(MEMBERS, &member.node_id.to_string(), member)?;
    Ok(Ok(()))
}
