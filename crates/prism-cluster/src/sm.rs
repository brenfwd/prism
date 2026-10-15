//! The raft state machine: delegates to the application's [`Backend`].

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use openraft::storage::{RaftStateMachine, Snapshot};
use openraft::{EntryPayload, ErrorSubject, ErrorVerb, RaftSnapshotBuilder};
use tokio::sync::watch;

use crate::types::{
    AppliedMeta, Entry, LogId, SnapshotMeta, StorageError, StoredMembership, TypeConfig,
};

/// The application state that commands apply to.
///
/// Every method must be deterministic and persist `meta` in the same
/// transaction as the change it describes.
pub trait Backend: Send + Sync + 'static {
    /// Applies a JSON command (`None` for blank and membership entries) and
    /// records `meta`. Returns the JSON result handed back to the proposer.
    fn apply(&self, command: Option<&str>, meta: &AppliedMeta) -> anyhow::Result<String>;
    /// The meta recorded by the last apply or restore.
    fn applied_meta(&self) -> anyhow::Result<Option<AppliedMeta>>;
    /// A consistent copy of the whole state.
    fn snapshot(&self) -> anyhow::Result<Vec<u8>>;
    /// Replaces the whole state with a snapshot and records `meta`.
    fn restore(&self, data: &[u8], meta: &AppliedMeta) -> anyhow::Result<()>;
}

fn sm_err(verb: ErrorVerb, e: impl std::fmt::Display) -> StorageError {
    StorageError::from_io_error(
        ErrorSubject::StateMachine,
        verb,
        std::io::Error::other(e.to_string()),
    )
}

/// The most recent snapshot: its metadata and data.
type CurrentSnapshot = (SnapshotMeta, Vec<u8>);

#[derive(Clone)]
pub struct StateMachine {
    backend: Arc<dyn Backend>,
    current: Arc<std::sync::Mutex<Option<CurrentSnapshot>>>,
    snapshot_seq: Arc<AtomicU64>,
    /// The last applied index, for read-your-writes waits.
    applied: watch::Sender<u64>,
}

impl std::fmt::Debug for StateMachine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateMachine").finish_non_exhaustive()
    }
}

impl StateMachine {
    pub fn new(backend: Arc<dyn Backend>) -> anyhow::Result<Self> {
        let applied = backend
            .applied_meta()?
            .and_then(|m| m.last_applied)
            .map_or(0, |l| l.index);
        Ok(Self {
            backend,
            current: Arc::default(),
            snapshot_seq: Arc::default(),
            applied: watch::channel(applied).0,
        })
    }

    pub fn applied_index(&self) -> watch::Receiver<u64> {
        self.applied.subscribe()
    }

    fn meta(&self) -> Result<AppliedMeta, StorageError> {
        Ok(self
            .backend
            .applied_meta()
            .map_err(|e| sm_err(ErrorVerb::Read, e))?
            .unwrap_or_default())
    }
}

impl RaftSnapshotBuilder<TypeConfig> for StateMachine {
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, StorageError> {
        // The backend's snapshot and meta are read under one lock on its side
        // only if it is consistent; apply runs on this same task, so no
        // command can interleave here.
        let meta = self.meta()?;
        let data = self
            .backend
            .snapshot()
            .map_err(|e| sm_err(ErrorVerb::Read, e))?;
        let seq = self.snapshot_seq.fetch_add(1, Ordering::Relaxed);
        let snapshot_id = format!("{}-{seq}", meta.last_applied.map_or(0, |l| l.index));
        let snap_meta = SnapshotMeta {
            last_log_id: meta.last_applied,
            last_membership: meta.membership,
            snapshot_id,
        };
        *self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((snap_meta.clone(), data.clone()));
        Ok(Snapshot {
            meta: snap_meta,
            snapshot: Box::new(Cursor::new(data)),
        })
    }
}

impl RaftStateMachine<TypeConfig> for StateMachine {
    type SnapshotBuilder = Self;

    async fn applied_state(&mut self) -> Result<(Option<LogId>, StoredMembership), StorageError> {
        let m = self.meta()?;
        Ok((m.last_applied, m.membership))
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<String>, StorageError>
    where
        I: IntoIterator<Item = Entry> + Send,
        I::IntoIter: Send,
    {
        let mut meta = self.meta()?;
        let mut out = Vec::new();
        for entry in entries {
            meta.last_applied = Some(entry.log_id);
            let cmd = match &entry.payload {
                EntryPayload::Blank => None,
                EntryPayload::Normal(c) => Some(c.as_str()),
                EntryPayload::Membership(m) => {
                    meta.membership = StoredMembership::new(Some(entry.log_id), m.clone());
                    None
                }
            };
            let result = self
                .backend
                .apply(cmd, &meta)
                .map_err(|e| sm_err(ErrorVerb::Write, e))?;
            self.applied.send_replace(entry.log_id.index);
            out.push(result);
        }
        Ok(out)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn begin_receiving_snapshot(&mut self) -> Result<Box<Cursor<Vec<u8>>>, StorageError> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta,
        snapshot: Box<Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError> {
        let data = snapshot.into_inner();
        let applied = AppliedMeta {
            last_applied: meta.last_log_id,
            membership: meta.last_membership.clone(),
        };
        self.backend
            .restore(&data, &applied)
            .map_err(|e| sm_err(ErrorVerb::Write, e))?;
        self.applied
            .send_replace(meta.last_log_id.map_or(0, |l| l.index));
        *self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((meta.clone(), data));
        Ok(())
    }

    async fn get_current_snapshot(&mut self) -> Result<Option<Snapshot<TypeConfig>>, StorageError> {
        let cur = self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Ok(cur.map(|(meta, data)| Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        }))
    }
}
