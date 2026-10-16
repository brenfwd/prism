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

/// Replicated-state collection holding [`ArchivePoolDoc`] records.
pub const ARCHIVE_POOLS: &str = "archive_pool";
/// Replicated-state collection holding [`ArchiveMemberDoc`] records.
pub const ARCHIVE_MEMBERS: &str = "archive_member";
/// Replicated-state collection holding the file records and their replicas.
pub const ARCHIVE_FILES: &str = "archive_file";
const PARTIAL_DIR: &str = ".prism-partial";
const MAX_DEPTH: usize = 16;

/// An archive pool as stored in replicated state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivePoolDoc {
    /// Pool name, unique in the cluster.
    pub name: String,
    /// Copies of each file, on distinct nodes.
    pub replicas: u8,
}

/// Whether a member accepts new files and keeps its existing ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberState {
    /// Takes new files and keeps its replicas.
    Active,
    /// Takes no new files; a drain is moving its replicas elsewhere.
    Draining,
    /// Drained: holds no replicas and takes no new files.
    Disabled,
}

/// A member directory that one node contributes to an archive pool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveMemberDoc {
    /// Node that holds the directory.
    pub node: Uuid,
    /// Member name, unique on that node.
    pub name: String,
    /// Pool the member belongs to.
    pub pool: String,
    /// Relative share of new files, used by [`place`].
    pub weight: f64,
    /// Bytes that placement leaves free on the filesystem.
    pub reserve: u64,
    /// Current state.
    pub state: MemberState,
}

impl ArchiveMemberDoc {
    /// Replicated-state key, `<node id>/<member name>`.
    pub fn key(&self) -> String {
        format!("{}/{}", self.node, self.name)
    }
}

/// One stored copy of a file: the node and the member directory holding it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replica {
    /// Node holding the copy.
    pub node: Uuid,
    /// Name of the member directory on that node.
    pub member: String,
}

/// What an archive file is, guessed from its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    /// An installation image (`.iso`).
    Iso,
    /// A disk image (`.qcow2`, `.img` or `.raw`).
    Disk,
    /// A VM backup.
    Backup,
    /// Anything else.
    Other,
}

impl FileKind {
    /// Kind for `path`, by case-insensitive extension. Never returns `Backup`;
    /// backups are labelled when they are created.
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

/// A file in an archive pool, with its replicas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveFileDoc {
    /// Pool holding the file.
    pub pool: String,
    /// Relative path inside the pool, validated by [`valid_path`].
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// BLAKE3 of the content, hex; the integrity anchor.
    pub blake3: String,
    /// Kind of file.
    pub kind: FileKind,
    /// Creation time, Unix seconds.
    pub created: i64,
    /// Where the copies are.
    pub replicas: Vec<Replica>,
}

impl ArchiveFileDoc {
    /// Replicated-state key, `<pool>/<path>`.
    pub fn key(pool: &str, path: &str) -> String {
        format!("{pool}/{path}")
    }
}

/// Free space of a member, reported by its node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberCapacity {
    /// Member name.
    pub name: String,
    /// Pool the member belongs to.
    pub pool: String,
    /// Filesystem size in bytes.
    pub total: u64,
    /// Bytes available to unprivileged writers.
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
    /// Member name.
    pub name: String,
    /// Pool the member belongs to.
    pub pool: String,
    root: OwnedFd,
}

impl LocalMember {
    /// Opens the member directory at `path`, creating it with its partial
    /// upload directory if missing, and removes uploads a crash left behind.
    ///
    /// Fails if the directory or the partial directory cannot be created or opened.
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
        match rustix::fs::mkdirat(&m.root, PARTIAL_DIR, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(e.into()),
        }
        m.remove_crashed_partials();
        Ok(m)
    }

    /// Deletes `up-*` files that a crash left in the partial directory.
    /// Failures are logged, since a leftover file only wastes space.
    fn remove_crashed_partials(&self) {
        let dir = match rustix::fs::openat2(
            &self.root,
            PARTIAL_DIR,
            OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
            resolve(),
        ) {
            Ok(dir) => dir,
            Err(e) => {
                tracing::warn!(member = %self.name, "could not open the partial upload directory: {e}");
                return;
            }
        };
        let entries = match rustix::fs::Dir::read_from(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!(member = %self.name, "could not list the partial upload directory: {e}");
                return;
            }
        };
        for e in entries.flatten() {
            if let Ok(n) = e.file_name().to_str()
                && n.starts_with("up-")
                && let Err(e) = rustix::fs::unlinkat(&dir, n, AtFlags::empty())
            {
                tracing::warn!(member = %self.name, file = n, "could not remove a partial upload: {e}");
            }
        }
    }

    /// Deletes a partial upload after a failed write, logging on failure.
    fn remove_partial(&self, partial: &str) {
        if let Err(e) = rustix::fs::unlinkat(&self.root, partial, AtFlags::empty()) {
            tracing::warn!(member = %self.name, partial, "could not remove a partial upload: {e}");
        }
    }

    /// Total and available bytes of the filesystem under the member; both are 0 if `fstatvfs` fails.
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
    ///
    /// The data goes to a temporary file in the partial directory, is synced,
    /// and is renamed into place, so readers never see a half-written file.
    /// Fails with `Exists` if the path is taken and `Invalid` if the stream errors.
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
            self.remove_partial(&partial);
            return Err(e);
        }
        if let Err(e) = rustix::fs::renameat_with(
            &self.root,
            &partial,
            &self.root,
            path,
            RenameFlags::NOREPLACE,
        ) {
            self.remove_partial(&partial);
            return Err(match e {
                rustix::io::Errno::EXIST => StorageError::Exists(path.into()),
                e => e.into(),
            });
        }
        Ok((size, hasher.finalize().to_hex().to_string()))
    }

    /// Opens the stored file read-only. Fails with `NoVolume` if it is missing
    /// and `InvalidName` if `path` is not a valid relative path.
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

    /// Deletes `path`, logging a warning on failure. Returns whether the
    /// file is gone. Used where a failed delete must not mask the error that
    /// led to it.
    pub fn delete_best_effort(&self, path: &str) -> bool {
        match self.delete(path) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(member = %self.name, pool = %self.pool, path, "could not delete archive file: {e}");
                false
            }
        }
    }

    /// Removes the stored file. Fails with `NoVolume` if it is missing.
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
    /// Members keyed by name.
    pub members: BTreeMap<String, Arc<LocalMember>>,
}

impl Archive {
    /// The member named `name`, if this node has one.
    pub fn member(&self, name: &str) -> Option<Arc<LocalMember>> {
        self.members.get(name).cloned()
    }

    /// Capacity of every member on this node.
    pub fn capacity(&self) -> Vec<MemberCapacity> {
        self.members.values().map(|m| m.capacity()).collect()
    }
}

#[cfg(test)]
mod tests;
