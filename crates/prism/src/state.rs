//! Desired state, changed only by applying [`Command`]s.
//!
//! Every write goes through [`State::apply`] so that clustering can later
//! replicate the same commands through raft; `apply_local` must stay
//! deterministic (no clocks, randomness or I/O besides the database).

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::db::StateDb;
use crate::spec::VmSpec;

pub const VMS: &str = "vm";
pub const POOLS: &str = "pool";

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
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("{0}")]
    Conflict(String),
    #[error("{0} not found")]
    NotFound(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

#[derive(Debug)]
pub struct State {
    db: Arc<StateDb>,
    /// The local node, used to resolve nil owners in old documents.
    node: Uuid,
    applied: broadcast::Sender<Arc<Command>>,
}

impl State {
    pub fn new(db: StateDb, node: Uuid) -> Self {
        Self {
            db: Arc::new(db),
            node,
            applied: broadcast::channel(1024).0,
        }
    }

    /// Commands as they are applied, from any source.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Command>> {
        self.applied.subscribe()
    }

    /// Applies a command. Standalone nodes apply directly; clustered nodes
    /// will route this through the replicated log.
    pub async fn apply(&self, cmd: Command) -> Result<(), StateError> {
        let (db, node) = (Arc::clone(&self.db), self.node);
        let cmd = tokio::task::spawn_blocking(move || apply_local(&db, node, &cmd).map(|()| cmd))
            .await
            .map_err(|e| StateError::Internal(e.into()))??;
        let _ = self.applied.send(Arc::new(cmd));
        Ok(())
    }
}

/// Applies one command in a single transaction. Deterministic.
fn apply_local(db: &StateDb, local: Uuid, cmd: &Command) -> Result<(), StateError> {
    db.transaction(|d| {
            match cmd {
                Command::PutVm { vm, expect_generation } => {
                    let current: Option<VmSpec> = d.get(VMS, &vm.id.to_string())?;
                    if let Some(expected) = expect_generation {
                        let actual = current.as_ref().map(|c| c.generation);
                        if actual != *expected {
                            return Ok(Err(StateError::Conflict(format!(
                                "vm {} changed concurrently (generation {actual:?}, expected {expected:?})",
                                vm.name
                            ))));
                        }
                    }
                    let clash = d
                        .list::<VmSpec>(VMS)?
                        .into_iter()
                        .any(|o| o.name == vm.name && o.id != vm.id);
                    if clash {
                        return Ok(Err(StateError::Conflict(format!(
                            "a VM named {} already exists",
                            vm.name
                        ))));
                    }
                    d.put(VMS, &vm.id.to_string(), vm)?;
                }
                Command::DeleteVm { id } => {
                    if !d.delete(VMS, &id.to_string())? {
                        return Ok(Err(StateError::NotFound(format!("vm {id}"))));
                    }
                }
                Command::PutPool { pool } => {
                    d.put(POOLS, &PoolDoc::key(pool.node, &pool.name), pool)?;
                }
                Command::DeletePool { node, name } => {
                    let removed = d.delete(POOLS, &PoolDoc::key(*node, name))?
                        || (*node == local && d.delete(POOLS, name)?);
                    if !removed {
                        return Ok(Err(StateError::NotFound(format!("pool {name}"))));
                    }
                }
            }
            Ok(Ok(()))
        })?
}

impl State {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::tests::sample_spec;

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
        assert!(matches!(*rx.try_recv().unwrap(), Command::PutPool { .. }));
        assert_eq!(s.pools(Uuid::from_u128(1)).unwrap().len(), 1);
        assert_eq!(s.pools(Uuid::from_u128(2)).unwrap().len(), 0);
    }
}
