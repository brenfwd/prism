//! The raft state machine over `state.db`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::apply::apply_command;
use super::{APPLIED_META, Change, Command, Outcome, State};

/// Applies replicated commands to `state.db`; the raft state machine.
#[derive(Clone)]
pub struct StateBackend {
    pub(super) state: State,
}

#[derive(Serialize, Deserialize)]
struct SnapshotDoc {
    collection: String,
    id: String,
    body: String,
}

impl prism_cluster::Backend for StateBackend {
    fn apply(
        &self,
        command: Option<&str>,
        meta: &prism_cluster::AppliedMeta,
    ) -> anyhow::Result<String> {
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
                return Ok(serde_json::to_string(&Outcome::Conflict(format!(
                    "unreadable command: {e}"
                )))?);
            }
        };
        let outcome = s.db.transaction(|d| {
            let o = apply_command(d, s.node, applied_index(meta), &cmd)?;
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
                    tracing::warn!(
                        index = applied_index(meta),
                        "replicated command rejected: {o:?}"
                    );
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
            .map(|(collection, id, body)| SnapshotDoc {
                collection,
                id,
                body,
            })
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

/// The log index of the entry being applied.
fn applied_index(meta: &prism_cluster::AppliedMeta) -> u64 {
    meta.last_applied.map_or(0, |l| l.index)
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
