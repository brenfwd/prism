//! The raft log in SQLite (`data/raft.db`).
//!
//! Entries and the vote are stored as JSON text. Every write commits with
//! `synchronous=FULL` in WAL mode before the caller is acknowledged, and the
//! blocking SQLite calls run on tokio's blocking pool so they never stall an
//! async worker.
#![allow(clippy::result_large_err)]

use std::fmt::Debug;
use std::ops::{Bound, RangeBounds};
use std::path::Path;
use std::sync::{Arc, Mutex};

use openraft::storage::{LogFlushed, LogState, RaftLogStorage};
use openraft::{ErrorSubject, ErrorVerb, RaftLogReader};
use rusqlite::{Connection, OptionalExtension, params};

use crate::sync::lock_unpoisoned;
use crate::types::{Entry, LogId, RaftId, StorageError, TypeConfig, Vote};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS log (idx INTEGER PRIMARY KEY, entry TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

const KEY_VOTE: &str = "vote";
const KEY_COMMITTED: &str = "committed";
const KEY_LAST_PURGED: &str = "last_purged";

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn io_err(subject: ErrorSubject<RaftId>, verb: ErrorVerb, e: impl Into<BoxError>) -> StorageError {
    StorageError::from_io_error(subject, verb, std::io::Error::other(e))
}

fn read_err(e: impl Into<BoxError>) -> StorageError {
    io_err(ErrorSubject::Logs, ErrorVerb::Read, e)
}

fn write_err(e: impl Into<BoxError>) -> StorageError {
    io_err(ErrorSubject::Logs, ErrorVerb::Write, e)
}

/// Converts a log index to SQLite's signed integer, saturating at `i64::MAX`.
fn to_sql(index: u64) -> i64 {
    i64::try_from(index).unwrap_or(i64::MAX)
}

/// SQLite-backed log storage. Clones share one connection.
#[derive(Debug, Clone)]
pub struct LogStore {
    conn: Arc<Mutex<Connection>>,
}

impl LogStore {
    /// Opens or creates the log database at `path` in WAL mode with
    /// `synchronous=FULL`.
    ///
    /// # Errors
    ///
    /// Returns an error if SQLite cannot open the file, if it refuses WAL
    /// journal mode, or if the schema cannot be created.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Self::init(Connection::open(path)?, true)
    }

    /// Creates a log in a private in-memory database, for tests. Nothing is
    /// durable.
    ///
    /// # Errors
    ///
    /// Returns an error if SQLite cannot create the database.
    pub fn memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?, false)
    }

    fn init(conn: Connection, require_wal: bool) -> anyhow::Result<Self> {
        let mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
        if require_wal && !mode.eq_ignore_ascii_case("wal") {
            anyhow::bail!("raft log needs WAL journal mode, SQLite chose {mode}");
        }
        // Raft requires durable log writes before acknowledging.
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Runs `f` with the connection on the blocking pool.
    async fn run<T, F>(&self, f: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || f(&mut lock_unpoisoned(&conn)))
            .await
            .map_err(write_err)?
    }

    async fn get_kv<T>(
        &self,
        key: &'static str,
        subject: ErrorSubject<RaftId>,
    ) -> Result<Option<T>, StorageError>
    where
        T: serde::de::DeserializeOwned + Send + 'static,
    {
        self.run(move |conn| get_kv(conn, key, &subject)).await
    }

    async fn set_kv<T>(
        &self,
        key: &'static str,
        subject: ErrorSubject<RaftId>,
        v: T,
    ) -> Result<(), StorageError>
    where
        T: serde::Serialize + Send + 'static,
    {
        self.run(move |conn| set_kv(conn, key, &subject, &v)).await
    }
}

fn get_kv<T: serde::de::DeserializeOwned>(
    conn: &Connection,
    key: &str,
    subject: &ErrorSubject<RaftId>,
) -> Result<Option<T>, StorageError> {
    let err = |e: BoxError| io_err(subject.clone(), ErrorVerb::Read, e);
    let v: Option<String> = conn
        .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
        .optional()
        .map_err(|e| err(e.into()))?;
    v.map(|s| serde_json::from_str(&s).map_err(|e| err(e.into())))
        .transpose()
}

/// Upserts `key`. Takes a `Connection` so callers can run it inside a
/// transaction (`Transaction` derefs to `Connection`).
fn set_kv<T: serde::Serialize>(
    conn: &Connection,
    key: &str,
    subject: &ErrorSubject<RaftId>,
    v: &T,
) -> Result<(), StorageError> {
    let err = |e: BoxError| io_err(subject.clone(), ErrorVerb::Write, e);
    let s = serde_json::to_string(v).map_err(|e| err(e.into()))?;
    conn.execute(
        "INSERT INTO kv (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, s],
    )
    .map_err(|e| err(e.into()))?;
    Ok(())
}

fn last_entry_id(conn: &Connection) -> Result<Option<LogId>, StorageError> {
    let e: Option<String> = conn
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

/// Converts a range of log indexes to the inclusive `(low, high)` SQLite
/// bounds. Values above `i64::MAX` saturate. An empty range yields
/// `low > high`.
fn bounds<RB: RangeBounds<u64>>(range: &RB) -> (i64, i64) {
    let lo = match range.start_bound() {
        Bound::Included(v) => to_sql(*v),
        Bound::Excluded(v) => to_sql(*v).saturating_add(1),
        Bound::Unbounded => 0,
    };
    let hi = match range.end_bound() {
        Bound::Included(v) => to_sql(*v),
        Bound::Excluded(v) => to_sql(*v).saturating_sub(1),
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
        self.run(move |conn| {
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
        })
        .await
    }
}

impl RaftLogStorage<TypeConfig> for LogStore {
    type LogReader = Self;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, StorageError> {
        self.run(|conn| {
            let last_purged_log_id: Option<LogId> =
                get_kv(conn, KEY_LAST_PURGED, &ErrorSubject::Store)?;
            let last_log_id = last_entry_id(conn)?.or(last_purged_log_id);
            Ok(LogState {
                last_purged_log_id,
                last_log_id,
            })
        })
        .await
    }

    #[allow(clippy::unused_async_trait_impl)] // cloning the handle does no I/O
    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote) -> Result<(), StorageError> {
        self.set_kv(KEY_VOTE, ErrorSubject::Vote, *vote).await
    }

    async fn read_vote(&mut self) -> Result<Option<Vote>, StorageError> {
        self.get_kv(KEY_VOTE, ErrorSubject::Vote).await
    }

    async fn save_committed(&mut self, committed: Option<LogId>) -> Result<(), StorageError> {
        self.set_kv(KEY_COMMITTED, ErrorSubject::Store, committed)
            .await
    }

    async fn read_committed(&mut self) -> Result<Option<LogId>, StorageError> {
        Ok(self
            .get_kv::<Option<LogId>>(KEY_COMMITTED, ErrorSubject::Store)
            .await?
            .flatten())
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
        let entries: Vec<Entry> = entries.into_iter().collect();
        let res = self
            .run(move |conn| {
                let tx = conn.transaction().map_err(write_err)?;
                {
                    let mut stmt = tx
                        .prepare_cached("INSERT OR REPLACE INTO log (idx, entry) VALUES (?1, ?2)")
                        .map_err(write_err)?;
                    for e in &entries {
                        let s = serde_json::to_string(e).map_err(write_err)?;
                        stmt.execute(params![to_sql(e.log_id.index), s])
                            .map_err(write_err)?;
                    }
                }
                tx.commit().map_err(write_err)
            })
            .await;
        callback.log_io_completed(
            res.as_ref()
                .copied()
                .map_err(|e| std::io::Error::other(e.to_string())),
        );
        res
    }

    async fn truncate(&mut self, log_id: LogId) -> Result<(), StorageError> {
        self.run(move |conn| {
            conn.execute("DELETE FROM log WHERE idx >= ?1", [to_sql(log_id.index)])
                .map_err(write_err)?;
            Ok(())
        })
        .await
    }

    async fn purge(&mut self, log_id: LogId) -> Result<(), StorageError> {
        self.run(move |conn| {
            let tx = conn.transaction().map_err(write_err)?;
            set_kv(&tx, KEY_LAST_PURGED, &ErrorSubject::Store, &log_id)?;
            tx.execute("DELETE FROM log WHERE idx <= ?1", [to_sql(log_id.index)])
                .map_err(write_err)?;
            tx.commit().map_err(write_err)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Bound::{Excluded, Included, Unbounded};

    use super::*;

    #[test]
    fn bounds_table() {
        let max = i64::MAX;
        assert_eq!(bounds(&(..)), (0, max));
        assert_eq!(bounds(&(3..7)), (3, 6));
        assert_eq!(bounds(&(3..=7)), (3, 7));
        assert_eq!(bounds(&(3..)), (3, max));
        assert_eq!(bounds(&(..7)), (0, 6));
        assert_eq!(bounds(&(Excluded(3), Included(7))), (4, 7));
        assert_eq!(bounds(&(Excluded(3), Unbounded)), (4, max));
        // Excluded(0) as an end is an empty range.
        let (lo, hi) = bounds(&(0..0));
        assert!(lo > hi);
        // Values past i64::MAX saturate instead of wrapping.
        assert_eq!(bounds(&(u64::MAX..)).0, max);
        assert_eq!(bounds(&(Excluded(u64::MAX), Unbounded)).0, max);
        assert_eq!(bounds(&(..=u64::MAX)).1, max);
    }

    fn entry(index: u64) -> Entry {
        Entry {
            log_id: openraft::LogId::new(openraft::CommittedLeaderId::new(1, 1), index),
            payload: openraft::EntryPayload::Blank,
        }
    }

    fn insert(store: &LogStore, indexes: std::ops::RangeInclusive<u64>) {
        let conn = lock_unpoisoned(&store.conn);
        for i in indexes {
            conn.execute(
                "INSERT INTO log (idx, entry) VALUES (?1, ?2)",
                params![to_sql(i), serde_json::to_string(&entry(i)).unwrap()],
            )
            .unwrap();
        }
    }

    async fn indexes(store: &mut LogStore) -> Vec<u64> {
        store
            .try_get_log_entries(..)
            .await
            .unwrap()
            .iter()
            .map(|e| e.log_id.index)
            .collect()
    }

    #[tokio::test]
    async fn truncate_removes_from_index_up() {
        let mut s = LogStore::memory().unwrap();
        insert(&s, 1..=5);
        s.truncate(entry(3).log_id).await.unwrap();
        assert_eq!(indexes(&mut s).await, [1, 2]);
    }

    #[tokio::test]
    async fn purge_removes_up_to_index_and_records_it() {
        let mut s = LogStore::memory().unwrap();
        insert(&s, 1..=5);
        s.purge(entry(2).log_id).await.unwrap();
        assert_eq!(indexes(&mut s).await, [3, 4, 5]);
        let state = s.get_log_state().await.unwrap();
        assert_eq!(state.last_purged_log_id, Some(entry(2).log_id));
        assert_eq!(state.last_log_id, Some(entry(5).log_id));
    }

    #[tokio::test]
    async fn range_reads_respect_bounds() {
        let mut s = LogStore::memory().unwrap();
        insert(&s, 1..=5);
        let got: Vec<u64> = s
            .try_get_log_entries(2..4)
            .await
            .unwrap()
            .iter()
            .map(|e| e.log_id.index)
            .collect();
        assert_eq!(got, [2, 3]);
    }

    #[tokio::test]
    async fn entries_and_purge_marker_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raft.db");
        {
            let mut s = LogStore::open(&path).unwrap();
            insert(&s, 1..=4);
            s.purge(entry(1).log_id).await.unwrap();
        }
        let mut s = LogStore::open(&path).unwrap();
        assert_eq!(indexes(&mut s).await, [2, 3, 4]);
        let state = s.get_log_state().await.unwrap();
        assert_eq!(state.last_purged_log_id, Some(entry(1).log_id));
    }
}
