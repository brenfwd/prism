//! Local `dir` pools. Every file operation is confined beneath the pool's
//! directory fd with `openat2(RESOLVE_BENEATH | NO_SYMLINKS | NO_MAGICLINKS | NO_XDEV)`.

pub mod cache;
mod download;
mod image;
mod seed;
mod upload;

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use rustix::fs::{AtFlags, Mode, OFlags, ResolveFlags};
use serde::Serialize;
use tokio::process::Command;

use image::qcow2_virtual_size;

pub const PARTIAL_PREFIX: &str = ".prism-partial-";

/// `[A-Za-z0-9._-]{1,128}` with no leading `-` or `.`.
pub fn valid_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && !name.starts_with(['-', '.'])
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("pool {0} not found")]
    NoPool(String),
    #[error("volume {0} not found")]
    NoVolume(String),
    #[error("{0} already exists")]
    Exists(String),
    #[error("invalid name {0:?}")]
    InvalidName(String),
    #[error("{0}")]
    Invalid(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("qemu-img failed: {0}")]
    QemuImg(String),
}

impl From<rustix::io::Errno> for StorageError {
    fn from(e: rustix::io::Errno) -> Self {
        Self::Io(e.into())
    }
}

#[derive(Debug)]
pub struct Pool {
    pub name: String,
    pub path: PathBuf,
    pub format: String,
    /// `true` for pools declared in `prism.toml`.
    pub static_config: bool,
    /// The filesystem supports `O_DIRECT` (tmpfs and some FUSE filesystems
    /// don't), so disks can use `cache='none'`.
    pub direct_io: bool,
    root: OwnedFd,
}

#[derive(Debug, Clone, Serialize)]
pub struct PoolInfo {
    pub name: String,
    pub path: PathBuf,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub format: String,
    pub static_config: bool,
    pub capacity_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Volume {
    pub name: String,
    /// Guest-visible size: the qcow2 virtual size, else the file size.
    pub size_bytes: u64,
    pub file_bytes: u64,
    pub allocated_bytes: u64,
    pub kind: &'static str,
}

fn resolve() -> ResolveFlags {
    ResolveFlags::BENEATH
        | ResolveFlags::NO_SYMLINKS
        | ResolveFlags::NO_MAGICLINKS
        | ResolveFlags::NO_XDEV
}

fn volume_kind(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("iso") => "iso",
        Some("qcow2" | "raw" | "img") => "disk",
        _ => "file",
    }
}

/// Creates `path` and any missing parents as root-owned `0711`: the qemu
/// uid can reach image files (which libvirt relabels) but cannot list or
/// write the directories. Explicit because Prism runs with `umask 077`.
pub(crate) fn create_traversable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let missing: Vec<&Path> = path.ancestors().take_while(|p| !p.exists()).collect();
    for dir in missing.into_iter().rev() {
        match std::fs::create_dir(dir) {
            Ok(()) => std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o711))?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Whether files under `root` can be opened with `O_DIRECT`.
fn probe_direct_io(root: &OwnedFd) -> bool {
    let name = format!("{PARTIAL_PREFIX}odirect-{}", uuid::Uuid::new_v4().simple());
    let res = rustix::fs::openat2(
        root,
        &name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::DIRECT,
        Mode::from_raw_mode(0o600),
        resolve(),
    );
    let _ = rustix::fs::unlinkat(root, &name, AtFlags::empty());
    res.is_ok()
}

impl Pool {
    pub fn open(
        name: &str,
        path: &Path,
        format: &str,
        static_config: bool,
    ) -> Result<Self, StorageError> {
        create_traversable(path)?;
        let root = rustix::fs::open(
            path,
            OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let direct_io = probe_direct_io(&root);
        if !direct_io {
            tracing::warn!(
                pool = name,
                "no O_DIRECT support; disks will use cache=writeback"
            );
        }
        Ok(Self {
            name: name.into(),
            path: path.into(),
            format: format.into(),
            static_config,
            direct_io,
            root,
        })
    }

    pub fn info(&self) -> PoolInfo {
        let (capacity_bytes, available_bytes) = rustix::fs::fstatvfs(&self.root)
            .map_or((0, 0), |s| {
                (s.f_blocks * s.f_frsize, s.f_bavail * s.f_frsize)
            });
        PoolInfo {
            name: self.name.clone(),
            path: self.path.clone(),
            kind: "dir",
            format: self.format.clone(),
            static_config: self.static_config,
            capacity_bytes,
            available_bytes,
        }
    }

    fn check(name: &str) -> Result<(), StorageError> {
        if valid_name(name) {
            Ok(())
        } else {
            Err(StorageError::InvalidName(name.into()))
        }
    }

    pub fn volumes(&self) -> Result<Vec<Volume>, StorageError> {
        let dir = rustix::fs::openat2(
            &self.root,
            ".",
            OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
            resolve(),
        )?;
        let mut out = Vec::new();
        for entry in rustix::fs::Dir::read_from(&dir)? {
            let entry = entry?;
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            if !valid_name(name) {
                continue;
            }
            // Symlinks, directories and devices are skipped.
            if let Ok(v) = self.volume(name) {
                out.push(v);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn volume(&self, name: &str) -> Result<Volume, StorageError> {
        Self::check(name)?;
        let fd = self.open_existing(name)?;
        let st = rustix::fs::fstat(&fd)?;
        if rustix::fs::FileType::from_raw_mode(st.st_mode) != rustix::fs::FileType::RegularFile {
            return Err(StorageError::NoVolume(name.into()));
        }
        let file_bytes = st.st_size as u64;
        Ok(Volume {
            name: name.into(),
            size_bytes: qcow2_virtual_size(&fd).unwrap_or(file_bytes),
            file_bytes,
            allocated_bytes: st.st_blocks as u64 * 512,
            kind: volume_kind(name),
        })
    }

    fn open_existing(&self, name: &str) -> Result<OwnedFd, StorageError> {
        rustix::fs::openat2(
            &self.root,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            resolve(),
        )
        .map_err(|e| match e {
            rustix::io::Errno::NOENT => StorageError::NoVolume(name.into()),
            e => e.into(),
        })
    }

    fn create_exclusive(&self, name: &str) -> Result<OwnedFd, StorageError> {
        rustix::fs::openat2(
            &self.root,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
            resolve(),
        )
        .map_err(|e| match e {
            rustix::io::Errno::EXIST => StorageError::Exists(name.into()),
            e => e.into(),
        })
    }

    /// Creates an empty disk image of `size_bytes`.
    pub async fn create_volume(
        &self,
        qemu_img: &Path,
        name: &str,
        size_bytes: u64,
        format: &str,
    ) -> Result<Volume, StorageError> {
        Self::check(name)?;
        if !matches!(format, "qcow2" | "raw") {
            return Err(StorageError::Invalid(format!(
                "unsupported format {format}"
            )));
        }
        // Reserve the name atomically, then let qemu-img fill it in.
        drop(self.create_exclusive(name)?);
        let out = Command::new(qemu_img)
            .args(["create", "-q", "-f", format])
            .args(if format == "qcow2" {
                &["-o", "preallocation=off"][..]
            } else {
                &[]
            })
            .arg("--")
            .arg(self.path.join(name))
            .arg(size_bytes.to_string())
            .output()
            .await;
        let failure = match out {
            Ok(o) if o.status.success() => None,
            Ok(o) => Some(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            Err(e) => Some(format!("{}: {e}", qemu_img.display())),
        };
        if let Some(msg) = failure {
            let _ = rustix::fs::unlinkat(&self.root, name, AtFlags::empty());
            return Err(StorageError::QemuImg(msg));
        }
        self.volume(name)
    }

    pub fn delete_volume(&self, name: &str) -> Result<(), StorageError> {
        Self::check(name)?;
        rustix::fs::unlinkat(&self.root, name, AtFlags::empty()).map_err(|e| match e {
            rustix::io::Errno::NOENT => StorageError::NoVolume(name.into()),
            e => e.into(),
        })
    }
}

/// All pools on this node.
#[derive(Debug, Default)]
pub struct Storage {
    pools: RwLock<BTreeMap<String, Arc<Pool>>>,
    pub pool_roots: Vec<PathBuf>,
}

impl Storage {
    pub fn new(pool_roots: Vec<PathBuf>) -> Self {
        Self {
            pools: RwLock::default(),
            pool_roots,
        }
    }

    pub fn insert(&self, pool: Pool) {
        pool.clean_partials();
        self.pools
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(pool.name.clone(), Arc::new(pool));
    }

    pub fn remove(&self, name: &str) -> Option<Arc<Pool>> {
        self.pools
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(name)
    }

    pub fn get(&self, name: &str) -> Result<Arc<Pool>, StorageError> {
        self.pools
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .cloned()
            .ok_or_else(|| StorageError::NoPool(name.into()))
    }

    pub fn list(&self) -> Vec<Arc<Pool>> {
        self.pools
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }

    /// The directory for a new API-created pool: `<root>/<name>`, where
    /// `root` must be one of the allowlisted `pool_roots`.
    pub fn new_pool_path(&self, name: &str, root: Option<&Path>) -> Result<PathBuf, StorageError> {
        if !valid_name(name) {
            return Err(StorageError::InvalidName(name.into()));
        }
        let root = match root {
            Some(r) => self
                .pool_roots
                .iter()
                .find(|p| p.as_path() == r)
                .ok_or_else(|| {
                    StorageError::Invalid(format!("{} is not in storage.pool_roots", r.display()))
                })?,
            None => self
                .pool_roots
                .first()
                .ok_or_else(|| StorageError::Invalid("no storage.pool_roots configured".into()))?,
        };
        Ok(root.join(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("disk-1.qcow2"));
        assert!(!valid_name(".hidden"));
        assert!(!valid_name("-rf"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(".."));
        assert!(!valid_name(""));
        assert!(!valid_name(&"a".repeat(129)));
    }

    #[tokio::test]
    async fn upload_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open("p", dir.path(), "qcow2", true).unwrap();
        let body = futures::stream::iter(vec![
            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"hello ")),
            Ok(bytes::Bytes::from_static(b"world")),
        ]);
        let v = pool.upload("a.iso", body).await.unwrap();
        assert_eq!(v.size_bytes, 11);
        assert_eq!(v.kind, "iso");
        let again = futures::stream::iter(vec![Ok::<_, std::io::Error>(bytes::Bytes::new())]);
        assert!(matches!(
            pool.upload("a.iso", again).await,
            Err(StorageError::Exists(_))
        ));
        assert_eq!(pool.volumes().unwrap().len(), 1);
        pool.delete_volume("a.iso").unwrap();
        assert!(pool.volumes().unwrap().is_empty());
    }

    #[test]
    fn new_pool_dirs_are_traversable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b");
        Pool::open("p", &path, "qcow2", true).unwrap();
        for p in [dir.path().join("a"), path] {
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o711, "{}", p.display());
        }
    }

    #[test]
    fn qcow2_header() {
        let dir = tempfile::tempdir().unwrap();
        let mut hdr = vec![0u8; 512];
        hdr[..4].copy_from_slice(b"QFI\xfb");
        hdr[24..32].copy_from_slice(&(2u64 << 30).to_be_bytes());
        std::fs::write(dir.path().join("a.qcow2"), &hdr).unwrap();
        std::fs::write(dir.path().join("b.raw"), [0u8; 100]).unwrap();
        let pool = Pool::open("p", dir.path(), "qcow2", true).unwrap();
        let a = pool.volume("a.qcow2").unwrap();
        assert_eq!((a.size_bytes, a.file_bytes), (2 << 30, 512));
        assert_eq!(pool.volume("b.raw").unwrap().size_bytes, 100);
    }

    #[test]
    fn refuses_symlink() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("evil")).unwrap();
        let pool = Pool::open("p", dir.path(), "qcow2", true).unwrap();
        assert!(pool.volume("evil").is_err());
        assert!(pool.volumes().unwrap().is_empty());
    }

    #[test]
    fn pool_roots_enforced() {
        let s = Storage::new(vec![PathBuf::from("/srv/pools")]);
        assert_eq!(
            s.new_pool_path("fast", None).unwrap(),
            PathBuf::from("/srv/pools/fast")
        );
        assert!(s.new_pool_path("fast", Some(Path::new("/etc"))).is_err());
        assert!(s.new_pool_path("../x", None).is_err());
    }
}
