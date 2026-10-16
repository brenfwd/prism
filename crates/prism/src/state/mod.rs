//! Desired state, changed only by applying [`Command`]s.
//!
//! Standalone nodes apply commands directly. Clustered nodes propose them
//! through raft (see [`Propose`]), and every member applies them through
//! [`StateBackend`]. `apply_command` must stay deterministic: no clocks,
//! randomness or I/O besides the database; proposers stamp times into the
//! command itself.

mod apply;
mod backend;
mod command;
#[cfg(test)]
mod network_tests;
mod networks;
#[cfg(test)]
mod tests;
mod timers;

pub use backend::StateBackend;
pub use command::{Command, Outcome};

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::archive::{
    ARCHIVE_FILES, ARCHIVE_MEMBERS, ARCHIVE_POOLS, ArchiveFileDoc, ArchiveMemberDoc, ArchivePoolDoc,
};
use crate::cluster::model::{
    CLUSTER, ClusterDoc, INVITES, InviteDoc, MEMBERS, MemberDoc, TOMBSTONES, TombstoneDoc,
};
use crate::db::StateDb;
use crate::network::{NETWORKS, NetworkDoc, RELEASED_VNIS, ReleasedVni};
use crate::spec::VmSpec;
use apply::apply_command;
pub use timers::ArchiveTimers;
use timers::{ARCHIVE_TIMERS_ID, TIMERS};

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
        f.debug_struct("State")
            .field("node", &self.node)
            .finish_non_exhaustive()
    }
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
        self.check_level(&cmd)?;
        if let Some(p) = self.proposer.get() {
            let json = serde_json::to_string(&cmd).map_err(anyhow::Error::from)?;
            let outcome: Outcome =
                serde_json::from_str(&p.propose(json).await?).map_err(anyhow::Error::from)?;
            return outcome.into_result();
        }
        let (db, node) = (Arc::clone(&self.db), self.node);
        let cmd2 = cmd.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            db.transaction(|d| apply_command(d, node, 0, &cmd2))
        })
        .await
        .map_err(|e| StateError::Internal(e.into()))??;
        if outcome == Outcome::Ok {
            let _ = self.changes.send(Change::Applied(Arc::new(cmd)));
        }
        outcome.into_result()
    }

    /// Refuses commands newer than the cluster's feature level, so that
    /// nodes still on an older level never receive them.
    fn check_level(&self, cmd: &Command) -> Result<(), StateError> {
        let need = cmd.required_level();
        if matches!(
            cmd,
            Command::SetFeatureLevel { .. } | Command::SetMemberFeatureLevel { .. }
        ) {
            return Ok(());
        }
        let level = self
            .cluster()?
            .map_or(crate::cluster::model::FEATURE_LEVEL, |c| c.feature_level);
        if !crate::cluster::model::feature_enabled(level, need) {
            return Err(StateError::Conflict(format!(
                "the cluster is at feature level {level} but this needs {need}: \
                 upgrade every node to the same prism version, and the level rises automatically"
            )));
        }
        Ok(())
    }

    /// The raft state machine backend over this state.
    pub fn backend(&self) -> StateBackend {
        StateBackend {
            state: self.clone(),
        }
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
        Ok(self
            .db
            .get::<VmSpec>(VMS, &id.to_string())?
            .map(|v| self.resolve_vm(v)))
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

    /// The replicated archive timers; empty until a leader writes them.
    pub fn archive_timers(&self) -> anyhow::Result<ArchiveTimers> {
        Ok(self.db.get(TIMERS, ARCHIVE_TIMERS_ID)?.unwrap_or_default())
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

    pub fn networks(&self) -> anyhow::Result<Vec<NetworkDoc>> {
        let mut n: Vec<NetworkDoc> = self.db.list(NETWORKS)?;
        n.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(n)
    }

    /// Whether networks were ever created here (any exist, or one was
    /// deleted recently), so the default network is seeded only once.
    pub fn networks_used(&self) -> anyhow::Result<bool> {
        Ok(!self.db.list::<NetworkDoc>(NETWORKS)?.is_empty()
            || !self.db.list::<ReleasedVni>(RELEASED_VNIS)?.is_empty())
    }
}
