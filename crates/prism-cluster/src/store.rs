//! The raft log in SQLite (`data/raft.db`).

use std::fmt::Debug;
use std::ops::{Bound, RangeBounds};
use std::path::Path;
use std::sync::{Arc, Mutex};

use openraft::storage::{LogFlushed, LogState, RaftLogStorage};
use openraft::{ErrorSubject, ErrorVerb, RaftLogReader};
use rusqlite::{Connection, OptionalExtension, params};

use crate::types::{Entry, LogId, RaftId, StorageError, TypeConfig, Vote};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS log (idx INTEGER PRIMARY KEY, entry TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

fn io_err(
    subject: ErrorSubject<RaftId>,
    verb: ErrorVerb,
    e: impl std::error::Error + 'static,
) -> StorageError {
    StorageError::from_io_error(subject, verb, std::io::Error::other(e.to_string()))
}

fn read_err(e: impl std::error::Error + 'static) -> StorageError {
    io_err(ErrorSubject::Logs, ErrorVerb::Read, e)
}

fn write_err(e: impl std::error::Error + 'static) -> StorageError {
    io_err(ErrorSubject::Logs, ErrorVerb::Write, e)
}

/// SQLite-backed log storage. Clones share one connection.
#[derive(Debug, Clone)]
pub struct LogStore {
    conn: Arc<Mutex<Connection>>,
}

impl LogStore {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // Raft requires durable log writes before acknowledging.
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn get_kv<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>, StorageError> {
        let v: Option<String> = self
            .conn()
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .map_err(read_err)?;
        v.map(|s| serde_json::from_str(&s).map_err(read_err))
            .transpose()
    }

    fn set_kv<T: serde::Serialize>(&self, key: &str, v: &T) -> Result<(), StorageError> {
        let s = serde_json::to_string(v).map_err(write_err)?;
        self.conn()
            .execute(
                "INSERT INTO kv (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, s],
            )
            .map_err(write_err)?;
        Ok(())
    }

    fn last_entry_id(&self) -> Result<Option<LogId>, StorageError> {
        let e: Option<String> = self
            .conn()
            .query_row("SELECT entry FROM log ORDER BY idx DESC LIMIT 1", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(read_err)?;
        e.map(|s| {
            serde_json::from_str::<Entry>(&s)
                .map(|e| e.log_id)
                .map_err(read_err)
        })
        .transpose()
    }
}

fn bounds<RB: RangeBounds<u64>>(range: &RB) -> (i64, i64) {
    let lo = match range.start_bound() {
        Bound::Included(v) => *v as i64,
        Bound::Excluded(v) => *v as i64 + 1,
        Bound::Unbounded => 0,
    };
    let hi = match range.end_bound() {
        Bound::Included(v) => *v as i64,
        Bound::Excluded(v) => *v as i64 - 1,
        Bound::Unbounded => i64::MAX,
    };
    (lo, hi)
}

impl RaftLogReader<TypeConfig> for LogStore {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + Send>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry>, StorageError> {
        let (lo, hi) = bounds(&range);
        let conn = self.conn();
        let mut stmt = conn
            .prepare_cached("SELECT entry FROM log WHERE idx >= ?1 AND idx <= ?2 ORDER BY idx")
            .map_err(read_err)?;
        let rows = stmt
            .query_map(params![lo, hi], |r| r.get::<_, String>(0))
            .map_err(read_err)?;
        rows.map(|r| {
            let s = r.map_err(read_err)?;
            serde_json::from_str(&s).map_err(read_err)
        })
        .collect()
    }
}

impl RaftLogStorage<TypeConfig> for LogStore {
    type LogReader = Self;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, StorageError> {
        let last_purged_log_id: Option<LogId> = self.get_kv("last_purged")?;
        let last_log_id = self.last_entry_id()?.or(last_purged_log_id);
        Ok(LogState {
            last_purged_log_id,
            last_log_id,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote) -> Result<(), StorageError> {
        self.set_kv("vote", vote).map_err(|_| {
            io_err(
                ErrorSubject::Vote,
                ErrorVerb::Write,
                std::io::Error::other("save vote"),
            )
        })
    }

    async fn read_vote(&mut self) -> Result<Option<Vote>, StorageError> {
        self.get_kv("vote")
    }

    async fn save_committed(&mut self, committed: Option<LogId>) -> Result<(), StorageError> {
        self.set_kv("committed", &committed)
    }

    async fn read_committed(&mut self) -> Result<Option<LogId>, StorageError> {
        Ok(self.get_kv::<Option<LogId>>("committed")?.flatten())
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<TypeConfig>,
    ) -> Result<(), StorageError>
    where
        I: IntoIterator<Item = Entry> + Send,
        I::IntoIter: Send,
    {
        let res = (|| {
            let mut conn = self.conn();
            let tx = conn.transaction().map_err(write_err)?;
            {
                let mut stmt = tx
                    .prepare_cached("INSERT OR REPLACE INTO log (idx, entry) VALUES (?1, ?2)")
                    .map_err(write_err)?;
                for e in entries {
                    let s = serde_json::to_string(&e).map_err(write_err)?;
                    stmt.execute(params![e.log_id.index as i64, s])
                        .map_err(write_err)?;
                }
            }
            tx.commit().map_err(write_err)
        })();
        callback.log_io_completed(
            res.as_ref()
                .copied()
                .map_err(|e| std::io::Error::other(e.to_string())),
        );
        res
    }

    async fn truncate(&mut self, log_id: LogId) -> Result<(), StorageError> {
        self.conn()
            .execute("DELETE FROM log WHERE idx >= ?1", [log_id.index as i64])
            .map_err(write_err)?;
        Ok(())
    }

    async fn purge(&mut self, log_id: LogId) -> Result<(), StorageError> {
        self.set_kv("last_purged", &log_id)?;
        self.conn()
            .execute("DELETE FROM log WHERE idx <= ?1", [log_id.index as i64])
            .map_err(write_err)?;
        Ok(())
    }
}
