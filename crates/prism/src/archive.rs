//! Archive pools: a cluster-wide namespace for ISOs, backups and templates,
//! stored whole-file across member directories on any nodes.

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::Arc;

use futures::StreamExt;
use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags, ResolveFlags};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::storage::{StorageError, valid_name};

pub const ARCHIVE_POOLS: &str = "archive_pool";
pub const ARCHIVE_MEMBERS: &str = "archive_member";
pub const ARCHIVE_FILES: &str = "archive_file";
const PARTIAL_DIR: &str = ".prism-partial";
const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivePoolDoc {
    pub name: String,
    /// Copies of each file, on distinct nodes.
    pub replicas: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberState {
    Active,
    Draining,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveMemberDoc {
    pub node: Uuid,
    pub name: String,
    pub pool: String,
    pub weight: f64,
    pub reserve: u64,
    pub state: MemberState,
}

impl ArchiveMemberDoc {
    pub fn key(&self) -> String {
        format!("{}/{}", self.node, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replica {
    pub node: Uuid,
    pub member: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Iso,
    Disk,
    Backup,
    Other,
}

impl FileKind {
    pub fn guess(path: &str) -> Self {
        let ext = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("iso") => Self::Iso,
            Some("qcow2" | "img" | "raw") => Self::Disk,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveFileDoc {
    pub pool: String,
    pub path: String,
    pub size: u64,
    /// BLAKE3 of the content, hex; the integrity anchor.
    pub blake3: String,
    pub kind: FileKind,
    pub created: i64,
    pub replicas: Vec<Replica>,
}

impl ArchiveFileDoc {
    pub fn key(pool: &str, path: &str) -> String {
        format!("{pool}/{path}")
    }
}

/// Free space of a member, reported by its node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberCapacity {
    pub name: String,
    pub pool: String,
    pub total: u64,
    pub free: u64,
}

/// Validates an archive path: `/`-separated valid names, no `.` or `..`.
pub fn valid_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    !path.is_empty()
        && path.len() <= 1024
        && parts.len() <= MAX_DEPTH
        && parts.iter().all(|p| valid_name(p))
}

/// Picks a member for a new file: two weighted random choices, keeping the
/// one with more weighted free space ("power of two choices"). Members at
/// or below their reserve are skipped.
pub fn place<'a>(
    candidates: &[(&'a ArchiveMemberDoc, u64)],
    size: u64,
    seed: u128,
) -> Option<&'a ArchiveMemberDoc> {
    let ok: Vec<(&ArchiveMemberDoc, f64)> = candidates
        .iter()
        .filter(|(m, free)| {
            m.state == MemberState::Active && *free > m.reserve.saturating_add(size)
        })
        .map(|(m, free)| (*m, m.weight * (*free - m.reserve) as f64))
        .collect();
    if ok.is_empty() {
        return None;
    }
    let total: f64 = ok.iter().map(|(_, w)| w).sum();
    let pick = |r: u64| {
        let mut x = (r as f64 / u64::MAX as f64) * total;
        for (m, w) in &ok {
            if x < *w {
                return *m;
            }
            x -= w;
        }
        ok[ok.len() - 1].0
    };
    let (a, b) = (pick(seed as u64), pick((seed >> 64) as u64));
    let score = |m: &ArchiveMemberDoc| {
        ok.iter()
            .find(|(x, _)| x.key() == m.key())
            .map_or(0.0, |(_, w)| *w)
    };
    Some(if score(a) >= score(b) { a } else { b })
}

fn resolve() -> ResolveFlags {
    ResolveFlags::BENEATH
        | ResolveFlags::NO_SYMLINKS
        | ResolveFlags::NO_MAGICLINKS
        | ResolveFlags::NO_XDEV
}

/// A member directory on this node, with confined file access.
#[derive(Debug)]
pub struct LocalMember {
    pub name: String,
    pub pool: String,
    root: OwnedFd,
}

impl LocalMember {
    pub fn open(name: &str, pool: &str, path: &Path) -> Result<Self, StorageError> {
        std::fs::create_dir_all(path)?;
        let root = rustix::fs::open(
            path,
            OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let m = Self {
            name: name.into(),
            pool: pool.into(),
            root,
        };
        let _ = rustix::fs::mkdirat(&m.root, PARTIAL_DIR, Mode::from_raw_mode(0o700));
        // Partial uploads left by a crash.
        if let Ok(dir) = rustix::fs::openat2(
            &m.root,
            PARTIAL_DIR,
            OFlags::DIRECTORY | OFlags::RDONLY,
            Mode::empty(),
            resolve(),
        ) && let Ok(entries) = rustix::fs::Dir::read_from(&dir)
        {
            for e in entries.flatten() {
                if let Ok(n) = e.file_name().to_str()
                    && n.starts_with("up-")
                {
                    let _ = rustix::fs::unlinkat(&dir, n, AtFlags::empty());
                }
            }
        }
        Ok(m)
    }

    pub fn capacity(&self) -> MemberCapacity {
        let (total, free) = rustix::fs::fstatvfs(&self.root).map_or((0, 0), |s| {
            (s.f_blocks * s.f_frsize, s.f_bavail * s.f_frsize)
        });
        MemberCapacity {
            name: self.name.clone(),
            pool: self.pool.clone(),
            total,
            free,
        }
    }

    fn check(path: &str) -> Result<(), StorageError> {
        if valid_path(path) {
            Ok(())
        } else {
            Err(StorageError::InvalidName(path.into()))
        }
    }

    /// Creates the parent directories of `path`.
    fn ensure_parent(&self, path: &str) -> Result<(), StorageError> {
        let mut dir = String::new();
        let parts: Vec<&str> = path.split('/').collect();
        for p in &parts[..parts.len() - 1] {
            if !dir.is_empty() {
                dir.push('/');
            }
            dir.push_str(p);
            match rustix::fs::openat2(
                &self.root,
                &dir,
                OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
                Mode::empty(),
                resolve(),
            ) {
                Ok(_) => {}
                Err(rustix::io::Errno::NOENT) => {
                    match rustix::fs::mkdirat(&self.root, &dir, Mode::from_raw_mode(0o700)) {
                        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Streams `body` into `path`, returning `(size, blake3 hex)`. Refuses
    /// to overwrite.
    pub async fn write<S, E>(&self, path: &str, mut body: S) -> Result<(u64, String), StorageError>
    where
        S: futures::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        Self::check(path)?;
        if self.open_read(path).is_ok() {
            return Err(StorageError::Exists(path.into()));
        }
        self.ensure_parent(path)?;
        let partial = format!("{PARTIAL_DIR}/up-{}", Uuid::new_v4().simple());
        let fd = rustix::fs::openat2(
            &self.root,
            &partial,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
            resolve(),
        )?;
        let mut file = tokio::fs::File::from_std(std::fs::File::from(fd));
        let mut hasher = blake3::Hasher::new();
        let mut size = 0u64;
        let res: Result<(), StorageError> = async {
            while let Some(chunk) = body.next().await {
                let chunk =
                    chunk.map_err(|e| StorageError::Invalid(format!("upload aborted: {e}")))?;
                hasher.update(&chunk);
                size += chunk.len() as u64;
                file.write_all(&chunk).await?;
            }
            file.sync_all().await?;
            Ok(())
        }
        .await;
        drop(file);
        if let Err(e) = res {
            let _ = rustix::fs::unlinkat(&self.root, &partial, AtFlags::empty());
            return Err(e);
        }
        if let Err(e) = rustix::fs::renameat_with(
            &self.root,
            &partial,
            &self.root,
            path,
            RenameFlags::NOREPLACE,
        ) {
            let _ = rustix::fs::unlinkat(&self.root, &partial, AtFlags::empty());
            return Err(match e {
                rustix::io::Errno::EXIST => StorageError::Exists(path.into()),
                e => e.into(),
            });
        }
        Ok((size, hasher.finalize().to_hex().to_string()))
    }

    pub fn open_read(&self, path: &str) -> Result<std::fs::File, StorageError> {
        Self::check(path)?;
        let fd = rustix::fs::openat2(
            &self.root,
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            resolve(),
        )
        .map_err(|e| match e {
            rustix::io::Errno::NOENT => StorageError::NoVolume(path.into()),
            e => e.into(),
        })?;
        Ok(std::fs::File::from(fd))
    }

    /// BLAKE3 of the stored file, hex (for scrubbing).
    pub async fn hash(&self, path: &str) -> Result<String, StorageError> {
        let file = self.open_read(path)?;
        tokio::task::spawn_blocking(move || {
            let mut h = blake3::Hasher::new();
            h.update_reader(file)?;
            Ok(h.finalize().to_hex().to_string())
        })
        .await
        .map_err(|e| StorageError::Invalid(e.to_string()))?
    }

    pub fn delete(&self, path: &str) -> Result<(), StorageError> {
        Self::check(path)?;
        rustix::fs::unlinkat(&self.root, path, AtFlags::empty()).map_err(|e| match e {
            rustix::io::Errno::NOENT => StorageError::NoVolume(path.into()),
            e => e.into(),
        })
    }
}

/// This node's archive members, by name.
#[derive(Debug, Default)]
pub struct Archive {
    pub members: BTreeMap<String, Arc<LocalMember>>,
}

impl Archive {
    pub fn member(&self, name: &str) -> Option<Arc<LocalMember>> {
        self.members.get(name).cloned()
    }

    pub fn capacity(&self) -> Vec<MemberCapacity> {
        self.members.values().map(|m| m.capacity()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(node: u128, name: &str, weight: f64) -> ArchiveMemberDoc {
        ArchiveMemberDoc {
            node: Uuid::from_u128(node),
            name: name.into(),
            pool: "archive".into(),
            weight,
            reserve: 10,
            state: MemberState::Active,
        }
    }

    #[test]
    fn paths() {
        assert!(valid_path("isos/alpine.iso"));
        assert!(valid_path("backups/web/2026-10-15.qcow2"));
        assert!(!valid_path("../etc/passwd"));
        assert!(!valid_path("a//b"));
        assert!(!valid_path("/abs"));
        assert!(!valid_path(".prism-partial/x"));
        assert!(!valid_path(""));
    }

    #[test]
    fn placement_respects_reserve_and_prefers_free_space() {
        let (a, b, c) = (
            member(1, "a", 1.0),
            member(2, "b", 1.0),
            member(3, "c", 1.0),
        );
        let cands = [(&a, 15u64), (&b, 1_000_000), (&c, 5)];
        // `a` would drop below its reserve with a 10-byte file; `c` already is.
        for seed in 0..50u128 {
            assert_eq!(
                place(&cands, 10, seed * 7919 + (seed << 64)).unwrap().name,
                "b"
            );
        }
        assert!(place(&[(&c, 5)], 1, 0).is_none());
    }

    #[test]
    fn placement_spreads_by_weight() {
        let (a, b) = (member(1, "a", 1.0), member(2, "b", 1.0));
        let cands = [(&a, 1000u64), (&b, 1000)];
        let mut hits = 0;
        for seed in 0..200u128 {
            let s = (seed * 0x9E37_79B9_7F4A_7C15) | ((seed * 0xD1B5_4A32_D192_ED03) << 64);
            if place(&cands, 1, s).unwrap().name == "a" {
                hits += 1;
            }
        }
        assert!((40..160).contains(&hits), "a got {hits}/200");
    }

    #[tokio::test]
    async fn write_read_delete_nested() {
        let dir = tempfile::tempdir().unwrap();
        let m = LocalMember::open("m", "archive", dir.path()).unwrap();
        let body = futures::stream::iter(vec![Ok::<_, std::io::Error>(bytes::Bytes::from_static(
            b"hello",
        ))]);
        let (size, hash) = m.write("isos/a/b.iso", body).await.unwrap();
        assert_eq!(size, 5);
        assert_eq!(hash, blake3::hash(b"hello").to_hex().to_string());
        assert!(m.open_read("isos/a/b.iso").is_ok());
        let again = futures::stream::iter(vec![Ok::<_, std::io::Error>(bytes::Bytes::new())]);
        assert!(matches!(
            m.write("isos/a/b.iso", again).await,
            Err(StorageError::Exists(_))
        ));
        m.delete("isos/a/b.iso").unwrap();
        assert!(m.open_read("isos/a/b.iso").is_err());
        assert!(m.open_read("../x").is_err());
    }
}
