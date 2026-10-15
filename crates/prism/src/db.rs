//! SQLite stores: `data/state.db` (desired state, a document store that the
//! replicated command log applies to) and `data/node.db` (node-local: tasks).

use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

const STATE_MIGRATIONS: &[&str] = &[
    // 1: initial schema (MVP)
    "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
     CREATE TABLE vms (
         id TEXT PRIMARY KEY,
         name TEXT NOT NULL UNIQUE,
         spec TEXT NOT NULL,
         created_at INTEGER NOT NULL
     );
     CREATE TABLE pools (
         name TEXT PRIMARY KEY,
         path TEXT NOT NULL,
         format TEXT NOT NULL
     );",
    // 2: generic document store; carry MVP rows over
    "CREATE TABLE docs (
         collection TEXT NOT NULL,
         id TEXT NOT NULL,
         body TEXT NOT NULL,
         PRIMARY KEY (collection, id)
     ) WITHOUT ROWID;
     INSERT INTO docs (collection, id, body) SELECT 'vm', id, spec FROM vms;
     INSERT INTO docs (collection, id, body)
         SELECT 'pool', name, json_object('name', name, 'path', path, 'format', format) FROM pools;
     DROP TABLE vms;
     DROP TABLE pools;",
];

const NODE_MIGRATIONS: &[&str] = &[
    // 1: tasks
    "CREATE TABLE tasks (
         id TEXT PRIMARY KEY,
         body TEXT NOT NULL,
         created_at INTEGER NOT NULL
     );
     CREATE INDEX tasks_created ON tasks (created_at);",
];

/// A SQLite connection with forward-only migrations keyed by `user_version`.
#[derive(Debug)]
pub struct Sqlite {
    conn: Mutex<Connection>,
}

impl Sqlite {
    fn open(path: &Path, migrations: &[&str]) -> anyhow::Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn, migrations).with_context(|| format!("migrating {}", path.display()))
    }

    fn init(mut conn: Connection, migrations: &[&str]) -> anyhow::Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let version = usize::try_from(version).unwrap_or(0);
        if version > migrations.len() {
            anyhow::bail!(
                "schema version {version} is newer than this binary supports ({})",
                migrations.len()
            );
        }
        for (i, sql) in migrations.iter().enumerate().skip(version) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", i as i64 + 1)?;
            tx.commit()?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Desired state: documents grouped into collections, plus `meta`.
#[derive(Debug)]
pub struct StateDb {
    db: Sqlite,
}

/// Read and write documents inside one transaction.
pub struct Docs<'a> {
    tx: &'a Transaction<'a>,
}

impl Docs<'_> {
    pub fn get<T: DeserializeOwned>(
        &self,
        collection: &str,
        id: &str,
    ) -> anyhow::Result<Option<T>> {
        get(self.tx, collection, id)
    }

    pub fn list<T: DeserializeOwned>(&self, collection: &str) -> anyhow::Result<Vec<T>> {
        list(self.tx, collection)
    }

    pub fn put<T: Serialize>(&self, collection: &str, id: &str, body: &T) -> anyhow::Result<()> {
        self.tx.execute(
            "INSERT INTO docs (collection, id, body) VALUES (?1, ?2, ?3)
             ON CONFLICT (collection, id) DO UPDATE SET body = excluded.body",
            params![collection, id, serde_json::to_string(body)?],
        )?;
        Ok(())
    }

    /// Returns whether a document was removed.
    pub fn delete(&self, collection: &str, id: &str) -> anyhow::Result<bool> {
        Ok(self.tx.execute(
            "DELETE FROM docs WHERE collection = ?1 AND id = ?2",
            params![collection, id],
        )? > 0)
    }
}

fn get<T: DeserializeOwned>(
    conn: &Connection,
    collection: &str,
    id: &str,
) -> anyhow::Result<Option<T>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM docs WHERE collection = ?1 AND id = ?2",
            params![collection, id],
            |r| r.get(0),
        )
        .optional()?;
    body.map(|b| serde_json::from_str(&b).with_context(|| format!("decoding {collection}/{id}")))
        .transpose()
}

fn list<T: DeserializeOwned>(conn: &Connection, collection: &str) -> anyhow::Result<Vec<T>> {
    let mut stmt =
        conn.prepare_cached("SELECT id, body FROM docs WHERE collection = ?1 ORDER BY id")?;
    let rows = stmt.query_map([collection], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    rows.map(|r| {
        let (id, body) = r?;
        serde_json::from_str(&body).with_context(|| format!("decoding {collection}/{id}"))
    })
    .collect()
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

impl StateDb {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            db: Sqlite::open(path, STATE_MIGRATIONS)?,
        })
    }

    #[cfg(test)]
    pub fn memory() -> anyhow::Result<Self> {
        Ok(Self {
            db: Sqlite::init(Connection::open_in_memory()?, STATE_MIGRATIONS)?,
        })
    }

    pub fn meta(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .db
            .conn()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> anyhow::Result<()> {
        set_meta(&self.db.conn(), key, value)
    }

    pub fn get<T: DeserializeOwned>(
        &self,
        collection: &str,
        id: &str,
    ) -> anyhow::Result<Option<T>> {
        get(&self.db.conn(), collection, id)
    }

    pub fn list<T: DeserializeOwned>(&self, collection: &str) -> anyhow::Result<Vec<T>> {
        list(&self.db.conn(), collection)
    }

    /// Runs `f` in one transaction, committing only if it succeeds.
    pub fn transaction<R>(
        &self,
        f: impl FnOnce(&Docs<'_>) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        let mut conn = self.db.conn();
        let tx = conn.transaction()?;
        let r = f(&Docs { tx: &tx })?;
        tx.commit()?;
        Ok(r)
    }
}

/// Node-local store: tasks.
#[derive(Debug)]
pub struct NodeDb {
    db: Sqlite,
}

impl NodeDb {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            db: Sqlite::open(path, NODE_MIGRATIONS)?,
        })
    }

    #[cfg(test)]
    pub fn memory() -> anyhow::Result<Self> {
        Ok(Self {
            db: Sqlite::init(Connection::open_in_memory()?, NODE_MIGRATIONS)?,
        })
    }

    pub fn put_task<T: Serialize>(
        &self,
        id: &str,
        created_at: i64,
        body: &T,
    ) -> anyhow::Result<()> {
        self.db.conn().execute(
            "INSERT INTO tasks (id, body, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET body = excluded.body",
            params![id, serde_json::to_string(body)?, created_at],
        )?;
        Ok(())
    }

    pub fn task<T: DeserializeOwned>(&self, id: &str) -> anyhow::Result<Option<T>> {
        let body: Option<String> = self
            .db
            .conn()
            .query_row("SELECT body FROM tasks WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        body.map(|b| Ok(serde_json::from_str(&b)?)).transpose()
    }

    /// Most recent first.
    pub fn tasks<T: DeserializeOwned>(&self, limit: usize) -> anyhow::Result<Vec<T>> {
        let conn = self.db.conn();
        let mut stmt = conn
            .prepare_cached("SELECT body FROM tasks ORDER BY created_at DESC, id DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit as i64], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    /// Deletes finished tasks older than `before` (unix seconds).
    pub fn prune_tasks(&self, before: i64) -> anyhow::Result<usize> {
        Ok(self.db.conn().execute(
            "DELETE FROM tasks WHERE created_at < ?1 AND json_extract(body, '$.state') != 'running'",
            [before],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docs_round_trip() {
        let db = StateDb::memory().unwrap();
        db.transaction(|d| {
            d.put("vm", "a", &serde_json::json!({"name": "a"}))?;
            d.put("vm", "b", &serde_json::json!({"name": "b"}))?;
            Ok(())
        })
        .unwrap();
        let all: Vec<serde_json::Value> = db.list("vm").unwrap();
        assert_eq!(all.len(), 2);
        let removed = db.transaction(|d| d.delete("vm", "a")).unwrap();
        assert!(removed);
        assert_eq!(db.get::<serde_json::Value>("vm", "a").unwrap(), None);
    }

    #[test]
    fn failed_transaction_rolls_back() {
        let db = StateDb::memory().unwrap();
        let r: anyhow::Result<()> = db.transaction(|d| {
            d.put("vm", "a", &1)?;
            anyhow::bail!("no")
        });
        assert!(r.is_err());
        assert_eq!(db.list::<i32>("vm").unwrap().len(), 0);
    }

    #[test]
    fn migrates_mvp_rows() {
        let conn = Connection::open_in_memory().unwrap();
        let db = Sqlite::init(conn, &STATE_MIGRATIONS[..1]).unwrap();
        db.conn()
            .execute_batch(
                "INSERT INTO vms VALUES ('id1', 'web', '{\"name\":\"web\"}', 0);
                 INSERT INTO pools VALUES ('fast', '/srv/fast', 'raw');",
            )
            .unwrap();
        let conn = db.conn.into_inner().unwrap();
        let db = StateDb {
            db: Sqlite::init(conn, STATE_MIGRATIONS).unwrap(),
        };
        let vm: serde_json::Value = db.get("vm", "id1").unwrap().unwrap();
        assert_eq!(vm["name"], "web");
        let pool: serde_json::Value = db.get("pool", "fast").unwrap().unwrap();
        assert_eq!(pool["path"], "/srv/fast");
    }

    #[test]
    fn meta() {
        let db = StateDb::memory().unwrap();
        db.set_meta("x", "1").unwrap();
        db.set_meta("x", "2").unwrap();
        assert_eq!(db.meta("x").unwrap().as_deref(), Some("2"));
    }

    #[test]
    fn tasks() {
        let db = NodeDb::memory().unwrap();
        db.put_task("a", 1, &serde_json::json!({"state": "succeeded"}))
            .unwrap();
        db.put_task("b", 2, &serde_json::json!({"state": "running"}))
            .unwrap();
        let t: Vec<serde_json::Value> = db.tasks(10).unwrap();
        assert_eq!(t[0]["state"], "running");
        assert_eq!(db.prune_tasks(10).unwrap(), 1);
    }
}
