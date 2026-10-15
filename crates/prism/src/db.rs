//! Desired state in `data/state.db` (SQLite, WAL).

use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::spec::VmSpec;

const MIGRATIONS: &[&str] = &[
    // 1: initial schema
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
];

#[derive(Debug)]
pub struct Db {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone)]
pub struct PoolRow {
    pub name: String,
    pub path: String,
    pub format: String,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> anyhow::Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let version = usize::try_from(version).unwrap_or(0);
        if version > MIGRATIONS.len() {
            anyhow::bail!(
                "state.db schema version {version} is newer than this binary supports ({})",
                MIGRATIONS.len()
            );
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", i as i64 + 1)?;
            tx.commit()?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn meta(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    pub fn vms(&self) -> anyhow::Result<Vec<VmSpec>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT spec FROM vms ORDER BY name")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    pub fn vm(&self, id: Uuid) -> anyhow::Result<Option<VmSpec>> {
        let spec: Option<String> = self
            .conn()
            .query_row(
                "SELECT spec FROM vms WHERE id = ?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        spec.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }

    pub fn vm_by_name(&self, name: &str) -> anyhow::Result<Option<VmSpec>> {
        let spec: Option<String> = self
            .conn()
            .query_row("SELECT spec FROM vms WHERE name = ?1", [name], |r| r.get(0))
            .optional()?;
        spec.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }

    pub fn insert_vm(&self, spec: &VmSpec, now: i64) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO vms (id, name, spec, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                spec.id.to_string(),
                spec.name,
                serde_json::to_string(spec)?,
                now
            ],
        )?;
        Ok(())
    }

    pub fn update_vm(&self, spec: &VmSpec) -> anyhow::Result<()> {
        let n = self.conn().execute(
            "UPDATE vms SET name = ?2, spec = ?3 WHERE id = ?1",
            params![spec.id.to_string(), spec.name, serde_json::to_string(spec)?],
        )?;
        anyhow::ensure!(n == 1, "vm {} not found", spec.id);
        Ok(())
    }

    pub fn delete_vm(&self, id: Uuid) -> anyhow::Result<()> {
        self.conn()
            .execute("DELETE FROM vms WHERE id = ?1", [id.to_string()])?;
        Ok(())
    }

    pub fn pools(&self) -> anyhow::Result<Vec<PoolRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT name, path, format FROM pools ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            Ok(PoolRow {
                name: r.get(0)?,
                path: r.get(1)?,
                format: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn insert_pool(&self, row: &PoolRow) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO pools (name, path, format) VALUES (?1, ?2, ?3)",
            params![row.name, row.path, row.format],
        )?;
        Ok(())
    }

    pub fn delete_pool(&self, name: &str) -> anyhow::Result<()> {
        self.conn()
            .execute("DELETE FROM pools WHERE name = ?1", [name])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::*;

    #[test]
    fn vm_round_trip() {
        let db = Db::memory().unwrap();
        let spec = VmSpec {
            id: Uuid::new_v4(),
            name: "a".into(),
            description: String::new(),
            generation: 1,
            cpu: CpuSpec { vcpus: 1 },
            memory: MemorySpec {
                max_mib: 512,
                min_mib: 256,
                target_mib: 512,
                balloon: BalloonMode::Manual,
            },
            firmware: Firmware::Bios,
            machine: "q35".into(),
            disks: vec![],
            nics: vec![],
        };
        db.insert_vm(&spec, 0).unwrap();
        assert!(db.insert_vm(&spec, 0).is_err());
        assert_eq!(db.vm(spec.id).unwrap().unwrap(), spec);
        assert_eq!(db.vm_by_name("a").unwrap().unwrap().id, spec.id);
        db.delete_vm(spec.id).unwrap();
        assert_eq!(db.vms().unwrap().len(), 0);
    }

    #[test]
    fn meta() {
        let db = Db::memory().unwrap();
        assert_eq!(db.meta("x").unwrap(), None);
        db.set_meta("x", "1").unwrap();
        db.set_meta("x", "2").unwrap();
        assert_eq!(db.meta("x").unwrap().as_deref(), Some("2"));
    }
}
