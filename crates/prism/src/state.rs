//! Desired state, changed only by applying [`Command`]s.
//!
//! Standalone nodes apply commands directly. Clustered nodes propose them
//! through raft (see [`Propose`]), and every member applies them through
//! [`StateBackend`]. `apply_command` must stay deterministic: no clocks,
//! randomness or I/O besides the database; proposers stamp times into the
//! command itself.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::archive::{ARCHIVE_FILES, ARCHIVE_MEMBERS, ARCHIVE_POOLS, ArchiveFileDoc, ArchiveMemberDoc, ArchivePoolDoc};
use crate::cluster::model::{
    AdmitError, CLUSTER, ClusterDoc, INVITES, InviteDoc, MEMBERS, MemberDoc, MemberState, TOMBSTONES, TombstoneDoc,
};
use crate::db::{Docs, StateDb};
use crate::spec::VmSpec;

pub const VMS: &str = "vm";
pub const POOLS: &str = "pool";
const CLUSTER_ID: &str = "cluster";
const APPLIED_META: &str = "raft.applied";

/// An API-created directory pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolDoc {
    pub name: String,
    pub path: PathBuf,
    pub format: String,
    /// Owning node; nil in documents written before clustering existed.
    #[serde(default)]
    pub node: Uuid,
}

impl PoolDoc {
    fn key(node: Uuid, name: &str) -> String {
        if node.is_nil() {
            name.to_string()
        } else {
            format!("{node}/{name}")
        }
    }
}

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
    DeleteArchiveFile {
        pool: String,
        path: String,
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
    fn into_result(self) -> Result<(), StateError> {
        match self {
            Self::Ok => Ok(()),
            Self::Conflict(m) => Err(StateError::Conflict(m)),
            Self::NotFound(m) => Err(StateError::NotFound(m)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("{0}")]
    Conflict(String),
    #[error("{0} not found")]
    NotFound(String),
    /// No hub/leader or no quorum: configuration is read-only.
    #[error("cluster is degraded: {0}")]
    Degraded(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

/// A state change, as seen by subscribers.
#[derive(Debug, Clone)]
pub enum Change {
    Applied(Arc<Command>),
    /// The whole state was replaced (raft snapshot install).
    Reset,
}

/// Sends a JSON command through the replicated log and returns the JSON
/// [`Outcome`] once the local state machine has applied it.
pub trait Propose: Send + Sync + 'static {
    fn propose(&self, command: String) -> BoxFuture<'_, Result<String, StateError>>;
}

#[derive(Clone)]
pub struct State {
    db: Arc<StateDb>,
    /// The local node, used to resolve nil owners in old documents.
    node: Uuid,
    changes: broadcast::Sender<Change>,
    proposer: Arc<OnceLock<Arc<dyn Propose>>>,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State").field("node", &self.node).finish_non_exhaustive()
    }
}

fn reject(outcome: Outcome) -> anyhow::Result<Outcome> {
    Ok(outcome)
}

/// Applies one command inside a transaction. Deterministic.
fn apply_command(d: &Docs<'_>, local: Uuid, cmd: &Command) -> anyhow::Result<Outcome> {
    match cmd {
        Command::PutVm { vm, expect_generation } => {
            let current: Option<VmSpec> = d.get(VMS, &vm.id.to_string())?;
            if let Some(expected) = expect_generation {
                let actual = current.as_ref().map(|c| c.generation);
                if actual != *expected {
                    return reject(Outcome::Conflict(format!(
                        "vm {} changed concurrently (generation {actual:?}, expected {expected:?})",
                        vm.name
                    )));
                }
            }
            if d.list::<VmSpec>(VMS)?.iter().any(|o| o.name == vm.name && o.id != vm.id) {
                return reject(Outcome::Conflict(format!("a VM named {} already exists", vm.name)));
            }
            d.put(VMS, &vm.id.to_string(), vm)?;
        }
        Command::DeleteVm { id } => {
            if !d.delete(VMS, &id.to_string())? {
                return reject(Outcome::NotFound(format!("vm {id}")));
            }
        }
        Command::PutPool { pool } => {
            d.put(POOLS, &PoolDoc::key(pool.node, &pool.name), pool)?;
        }
        Command::DeletePool { node, name } => {
            let removed = d.delete(POOLS, &PoolDoc::key(*node, name))? || (*node == local && d.delete(POOLS, name)?);
            if !removed {
                return reject(Outcome::NotFound(format!("pool {name}")));
            }
        }
        Command::InitCluster { cluster, hub } => {
            if d.get::<ClusterDoc>(CLUSTER, CLUSTER_ID)?.is_some() {
                return reject(Outcome::Conflict("the cluster is already initialised".into()));
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
            for (key, mut pool) in d.list::<PoolDoc>(POOLS)?.into_iter().map(|p| (p.name.clone(), p)) {
                if pool.node.is_nil() {
                    d.delete(POOLS, &key)?;
                    pool.node = hub.node_id;
                    d.put(POOLS, &PoolDoc::key(pool.node, &pool.name), &pool)?;
                }
            }
        }
        Command::CreateInvite { invite } => {
            let taken = d.list::<MemberDoc>(MEMBERS)?.iter().any(|m| m.raft_id == invite.raft_id || m.mesh_addr == invite.assigned_addr)
                || d.list::<InviteDoc>(INVITES)?.iter().any(|i| i.raft_id == invite.raft_id || i.assigned_addr == invite.assigned_addr)
                || d.list::<TombstoneDoc>(TOMBSTONES)?.iter().any(|t| t.mesh_addr == invite.assigned_addr);
            if taken {
                return reject(Outcome::Conflict("that mesh address is already reserved; retry".into()));
            }
            d.put(INVITES, &invite.invite_id, invite)?;
        }
        Command::RevokeInvite { invite_id } => {
            if !d.delete(INVITES, invite_id)? {
                return reject(Outcome::NotFound(format!("invite {invite_id}")));
            }
        }
        Command::AddMember { member, invite_id, now } => {
            if let Err(e) = admit(d, member, invite_id, *now)? {
                return reject(Outcome::Conflict(e.to_string()));
            }
        }
        Command::SetMemberState { node_id, state } => {
            let Some(mut m) = d.get::<MemberDoc>(MEMBERS, &node_id.to_string())? else {
                return reject(Outcome::NotFound(format!("member {node_id}")));
            };
            m.state = *state;
            d.put(MEMBERS, &node_id.to_string(), &m)?;
        }
        Command::SetVoter { node_id, voter } => {
            let Some(mut m) = d.get::<MemberDoc>(MEMBERS, &node_id.to_string())? else {
                return reject(Outcome::NotFound(format!("member {node_id}")));
            };
            m.voter = *voter;
            d.put(MEMBERS, &node_id.to_string(), &m)?;
        }
        Command::RemoveMember { node_id, now } => {
            let Some(m) = d.get::<MemberDoc>(MEMBERS, &node_id.to_string())? else {
                return reject(Outcome::NotFound(format!("member {node_id}")));
            };
            if d.list::<VmSpec>(VMS)?.iter().any(|v| v.node == *node_id) {
                return reject(Outcome::Conflict(format!("{} still owns VMs; move or delete them first", m.name)));
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
            for pool in d.list::<PoolDoc>(POOLS)?.into_iter().filter(|p| p.node == *node_id) {
                d.delete(POOLS, &PoolDoc::key(pool.node, &pool.name))?;
            }
        }
        Command::PutArchivePool { pool } => {
            d.put(ARCHIVE_POOLS, &pool.name, pool)?;
        }
        Command::PutArchiveMember { member } => {
            if d.get::<ArchivePoolDoc>(ARCHIVE_POOLS, &member.pool)?.is_none() {
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
            if d.get::<ArchivePoolDoc>(ARCHIVE_POOLS, &file.pool)?.is_none() {
                return reject(Outcome::NotFound(format!("archive pool {}", file.pool)));
            }
            let key = ArchiveFileDoc::key(&file.pool, &file.path);
            if *create && d.get::<ArchiveFileDoc>(ARCHIVE_FILES, &key)?.is_some() {
                return reject(Outcome::Conflict(format!("{} already exists in {}", file.path, file.pool)));
            }
            d.put(ARCHIVE_FILES, &key, file)?;
        }
        Command::DeleteArchiveFile { pool, path } => {
            if !d.delete(ARCHIVE_FILES, &ArchiveFileDoc::key(pool, path))? {
                return reject(Outcome::NotFound(format!("{pool}/{path}")));
            }
        }
    }
    Ok(Outcome::Ok)
}

/// The admission checks from the join protocol. Replaying the same
/// `AddMember` is a no-op success.
fn admit(d: &Docs<'_>, member: &MemberDoc, invite_id: &str, now: i64) -> anyhow::Result<Result<(), AdmitError>> {
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

impl State {
    pub fn new(db: StateDb, node: Uuid) -> Self {
        Self {
            db: Arc::new(db),
            node,
            changes: broadcast::channel(1024).0,
            proposer: Arc::default(),
        }
    }

    pub fn db(&self) -> &StateDb {
        &self.db
    }

    /// State changes as they are applied, from any source.
    pub fn subscribe(&self) -> broadcast::Receiver<Change> {
        self.changes.subscribe()
    }

    /// Routes future commands through the replicated log.
    pub fn set_proposer(&self, p: Arc<dyn Propose>) {
        let _ = self.proposer.set(p);
    }

    pub fn clustered(&self) -> bool {
        self.proposer.get().is_some()
    }

    /// Applies a command: directly when standalone, through raft when
    /// clustered (returning once the local state reflects it).
    pub async fn apply(&self, cmd: Command) -> Result<(), StateError> {
        if let Some(p) = self.proposer.get() {
            let json = serde_json::to_string(&cmd).map_err(anyhow::Error::from)?;
            let outcome: Outcome = serde_json::from_str(&p.propose(json).await?).map_err(anyhow::Error::from)?;
            return outcome.into_result();
        }
        let (db, node) = (Arc::clone(&self.db), self.node);
        let cmd2 = cmd.clone();
        let outcome = tokio::task::spawn_blocking(move || db.transaction(|d| apply_command(d, node, &cmd2)))
            .await
            .map_err(|e| StateError::Internal(e.into()))??;
        if outcome == Outcome::Ok {
            let _ = self.changes.send(Change::Applied(Arc::new(cmd)));
        }
        outcome.into_result()
    }

    /// The raft state machine backend over this state.
    pub fn backend(&self) -> StateBackend {
        StateBackend { state: self.clone() }
    }

    fn resolve_vm(&self, mut vm: VmSpec) -> VmSpec {
        if vm.node.is_nil() {
            vm.node = self.node;
        }
        vm
    }

    pub fn vms(&self) -> anyhow::Result<Vec<VmSpec>> {
        let mut vms: Vec<VmSpec> = self.db.list(VMS)?;
        vms.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(vms.into_iter().map(|v| self.resolve_vm(v)).collect())
    }

    pub fn vm(&self, id: Uuid) -> anyhow::Result<Option<VmSpec>> {
        Ok(self.db.get::<VmSpec>(VMS, &id.to_string())?.map(|v| self.resolve_vm(v)))
    }

    pub fn vm_by_name(&self, name: &str) -> anyhow::Result<Option<VmSpec>> {
        Ok(self.vms()?.into_iter().find(|v| v.name == name))
    }

    /// API-created pools on `node`.
    pub fn pools(&self, node: Uuid) -> anyhow::Result<Vec<PoolDoc>> {
        Ok(self
            .db
            .list::<PoolDoc>(POOLS)?
            .into_iter()
            .map(|mut p| {
                if p.node.is_nil() {
                    p.node = self.node;
                }
                p
            })
            .filter(|p| p.node == node)
            .collect())
    }

    pub fn cluster(&self) -> anyhow::Result<Option<ClusterDoc>> {
        self.db.get(CLUSTER, CLUSTER_ID)
    }

    pub fn members(&self) -> anyhow::Result<Vec<MemberDoc>> {
        let mut m: Vec<MemberDoc> = self.db.list(MEMBERS)?;
        m.sort_by_key(|m| m.raft_id);
        Ok(m)
    }

    pub fn invites(&self) -> anyhow::Result<Vec<InviteDoc>> {
        self.db.list(INVITES)
    }

    pub fn tombstones(&self) -> anyhow::Result<Vec<TombstoneDoc>> {
        self.db.list(TOMBSTONES)
    }

    pub fn archive_pools(&self) -> anyhow::Result<Vec<ArchivePoolDoc>> {
        self.db.list(ARCHIVE_POOLS)
    }

    pub fn archive_members(&self) -> anyhow::Result<Vec<ArchiveMemberDoc>> {
        self.db.list(ARCHIVE_MEMBERS)
    }

    pub fn archive_file(&self, pool: &str, path: &str) -> anyhow::Result<Option<ArchiveFileDoc>> {
        self.db.get(ARCHIVE_FILES, &ArchiveFileDoc::key(pool, path))
    }

    /// Files in `pool` whose path starts with `prefix`.
    pub fn archive_files(&self, pool: &str, prefix: &str) -> anyhow::Result<Vec<ArchiveFileDoc>> {
        Ok(self
            .db
            .list::<ArchiveFileDoc>(ARCHIVE_FILES)?
            .into_iter()
            .filter(|f| f.pool == pool && f.path.starts_with(prefix))
            .collect())
    }
}

/// Applies replicated commands to `state.db`; the raft state machine.
#[derive(Clone)]
pub struct StateBackend {
    state: State,
}

#[derive(Serialize, Deserialize)]
struct SnapshotDoc {
    collection: String,
    id: String,
    body: String,
}

impl prism_cluster::Backend for StateBackend {
    fn apply(&self, command: Option<&str>, meta: &prism_cluster::AppliedMeta) -> anyhow::Result<String> {
        let meta_json = serde_json::to_string(meta)?;
        let s = &self.state;
        let Some(json) = command else {
            s.db.set_meta(APPLIED_META, &meta_json)?;
            return Ok(serde_json::to_string(&Outcome::Ok)?);
        };
        let cmd: Command = match serde_json::from_str(json) {
            Ok(c) => c,
            Err(e) => {
                // A newer peer's command this binary cannot read: record
                // progress so the log keeps moving, and report it.
                tracing::error!("unreadable replicated command (upgrade prism): {e}");
                s.db.set_meta(APPLIED_META, &meta_json)?;
                return Ok(serde_json::to_string(&Outcome::Conflict(format!("unreadable command: {e}")))?);
            }
        };
        let outcome = s.db.transaction(|d| {
            let o = apply_command(d, s.node, &cmd)?;
            if o != Outcome::Ok {
                anyhow::bail!(Rejected(o));
            }
            d.set_meta(APPLIED_META, &meta_json)?;
            Ok(Outcome::Ok)
        });
        let outcome = match outcome {
            Ok(o) => {
                let _ = s.changes.send(Change::Applied(Arc::new(cmd)));
                o
            }
            Err(e) => match e.downcast::<Rejected>() {
                // Rolled back; still record that the entry was applied.
                Ok(Rejected(o)) => {
                    s.db.set_meta(APPLIED_META, &meta_json)?;
                    o
                }
                Err(e) => return Err(e),
            },
        };
        Ok(serde_json::to_string(&outcome)?)
    }

    fn applied_meta(&self) -> anyhow::Result<Option<prism_cluster::AppliedMeta>> {
        self.state
            .db
            .meta(APPLIED_META)?
            .map(|m| Ok(serde_json::from_str(&m)?))
            .transpose()
    }

    fn snapshot(&self) -> anyhow::Result<Vec<u8>> {
        let docs: Vec<SnapshotDoc> = self
            .state
            .db
            .export()?
            .into_iter()
            .map(|(collection, id, body)| SnapshotDoc { collection, id, body })
            .collect();
        Ok(serde_json::to_vec(&docs)?)
    }

    fn restore(&self, data: &[u8], meta: &prism_cluster::AppliedMeta) -> anyhow::Result<()> {
        let docs: Vec<SnapshotDoc> = serde_json::from_slice(data)?;
        let meta_json = serde_json::to_string(meta)?;
        self.state.db.transaction(|d| {
            d.clear()?;
            for doc in &docs {
                d.put_raw(&doc.collection, &doc.id, &doc.body)?;
            }
            d.set_meta(APPLIED_META, &meta_json)
        })?;
        let _ = self.state.changes.send(Change::Reset);
        Ok(())
    }
}

/// A command rejected by validation; its transaction is rolled back.
#[derive(Debug)]
struct Rejected(Outcome);

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rejected: {:?}", self.0)
    }
}

impl std::error::Error for Rejected {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::model::ClusterMode;
    use crate::spec::tests::sample_spec;
    use prism_cluster::Backend;

    fn state() -> State {
        State::new(StateDb::memory().unwrap(), Uuid::from_u128(1))
    }

    #[tokio::test]
    async fn put_and_conflicts() {
        let s = state();
        let vm = sample_spec("a");
        s.apply(Command::PutVm {
            vm: Box::new(vm.clone()),
            expect_generation: Some(None),
        })
        .await
        .unwrap();
        // Creating it again with "must not exist" conflicts.
        let again = s
            .apply(Command::PutVm {
                vm: Box::new(vm.clone()),
                expect_generation: Some(None),
            })
            .await;
        assert!(matches!(again, Err(StateError::Conflict(_))));
        // A different VM with the same name conflicts.
        let mut other = sample_spec("a");
        other.id = Uuid::new_v4();
        let dup = s
            .apply(Command::PutVm {
                vm: Box::new(other),
                expect_generation: None,
            })
            .await;
        assert!(matches!(dup, Err(StateError::Conflict(_))));
        assert_eq!(s.vm(vm.id).unwrap().unwrap().node, Uuid::from_u128(1));
        s.apply(Command::DeleteVm { id: vm.id }).await.unwrap();
        assert!(matches!(
            s.apply(Command::DeleteVm { id: vm.id }).await,
            Err(StateError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn subscribers_see_commands() {
        let s = state();
        let mut rx = s.subscribe();
        s.apply(Command::DeleteVm { id: Uuid::nil() }).await.ok();
        assert!(rx.try_recv().is_err(), "failed commands are not broadcast");
        let pool = PoolDoc {
            name: "p".into(),
            path: "/srv/p".into(),
            format: "qcow2".into(),
            node: Uuid::from_u128(1),
        };
        s.apply(Command::PutPool { pool }).await.unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Change::Applied(c) if matches!(*c, Command::PutPool { .. })));
        assert_eq!(s.pools(Uuid::from_u128(1)).unwrap().len(), 1);
        assert_eq!(s.pools(Uuid::from_u128(2)).unwrap().len(), 0);
    }

    fn cluster() -> ClusterDoc {
        ClusterDoc {
            cluster_id: Uuid::from_u128(9),
            name: "c".into(),
            mode: ClusterMode::HubSpoke,
            mesh_prefix: "fd12:3456:789a::".parse().unwrap(),
            wg_mtu: 1420,
            feature_level: 1,
            created: 0,
        }
    }

    fn member(c: &ClusterDoc, n: u128, raft_id: u64) -> MemberDoc {
        MemberDoc {
            node_id: Uuid::from_u128(n),
            raft_id,
            name: format!("n{n}"),
            wg_pubkey: format!("key{n}"),
            mesh_addr: c.mesh_addr(raft_id),
            endpoints: vec![],
            state: MemberState::Joining,
            voter: false,
            version: "0.1.0".into(),
            joined: 0,
        }
    }

    #[tokio::test]
    async fn join_protocol_checks() {
        let s = state();
        let c = cluster();
        let mut vm = sample_spec("legacy");
        vm.node = Uuid::nil();
        s.apply(Command::PutVm { vm: Box::new(vm.clone()), expect_generation: None }).await.unwrap();
        s.apply(Command::InitCluster { cluster: c.clone(), hub: member(&c, 1, 1) }).await.unwrap();
        // Pre-cluster VMs now explicitly belong to the hub.
        assert_eq!(s.db().get::<VmSpec>(VMS, &vm.id.to_string()).unwrap().unwrap().node, Uuid::from_u128(1));

        let invite = InviteDoc {
            invite_id: "aa".into(),
            raft_id: 2,
            assigned_addr: c.mesh_addr(2),
            exp: 100,
            created_by: Uuid::from_u128(1),
            name_hint: None,
            consumed_by: None,
        };
        s.apply(Command::CreateInvite { invite: invite.clone() }).await.unwrap();
        let dup = InviteDoc { invite_id: "bb".into(), ..invite.clone() };
        assert!(matches!(s.apply(Command::CreateInvite { invite: dup }).await, Err(StateError::Conflict(_))));

        let new = member(&c, 2, 2);
        let late = s.apply(Command::AddMember { member: new.clone(), invite_id: "aa".into(), now: 101 }).await;
        assert!(matches!(late, Err(StateError::Conflict(m)) if m.starts_with("invite_expired")));
        let wrong = MemberDoc { mesh_addr: c.mesh_addr(7), ..new.clone() };
        let r = s.apply(Command::AddMember { member: wrong, invite_id: "aa".into(), now: 50 }).await;
        assert!(matches!(r, Err(StateError::Conflict(m)) if m.starts_with("address_mismatch")));
        s.apply(Command::AddMember { member: new.clone(), invite_id: "aa".into(), now: 50 }).await.unwrap();
        // Replaying the same reply is a no-op; another node is refused.
        s.apply(Command::AddMember { member: new.clone(), invite_id: "aa".into(), now: 50 }).await.unwrap();
        let other = member(&c, 3, 2);
        let r = s.apply(Command::AddMember { member: other, invite_id: "aa".into(), now: 50 }).await;
        assert!(matches!(r, Err(StateError::Conflict(m)) if m.starts_with("invite_consumed")));

        s.apply(Command::RemoveMember { node_id: new.node_id, now: 60 }).await.unwrap();
        assert_eq!(s.tombstones().unwrap().len(), 1);
        assert_eq!(s.members().unwrap().len(), 1);
    }

    #[test]
    fn backend_records_progress_for_rejected_commands_and_restores() {
        let s = state();
        let b = s.backend();
        let mut meta = prism_cluster::AppliedMeta::default();
        meta.last_applied = Some(openraft::LogId::new(openraft::CommittedLeaderId::new(1, 1), 5));
        let bad = serde_json::to_string(&Command::DeleteVm { id: Uuid::nil() }).unwrap();
        let out: Outcome = serde_json::from_str(&b.apply(Some(&bad), &meta).unwrap()).unwrap();
        assert!(matches!(out, Outcome::NotFound(_)));
        assert_eq!(b.applied_meta().unwrap().unwrap().last_applied.unwrap().index, 5);

        let vm = sample_spec("x");
        let put = serde_json::to_string(&Command::PutVm { vm: Box::new(vm.clone()), expect_generation: None }).unwrap();
        meta.last_applied = Some(openraft::LogId::new(openraft::CommittedLeaderId::new(1, 1), 6));
        b.apply(Some(&put), &meta).unwrap();
        let snap = b.snapshot().unwrap();

        let s2 = state();
        let b2 = s2.backend();
        b2.restore(&snap, &meta).unwrap();
        assert_eq!(s2.vm(vm.id).unwrap().unwrap().name, "x");
        assert_eq!(b2.applied_meta().unwrap().unwrap().last_applied.unwrap().index, 6);
    }
}
