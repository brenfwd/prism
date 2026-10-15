//! Local `dir` pools. Every file operation is confined beneath the pool's
//! directory fd with `openat2(RESOLVE_BENEATH | NO_SYMLINKS | NO_MAGICLINKS | NO_XDEV)`.

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags, ResolveFlags};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

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
fn create_traversable(path: &Path) -> std::io::Result<()> {
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

const MAX_CHAIN: usize = 64;

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

/// The backing file name recorded in a qcow2 header, if any.
fn qcow2_backing_file(fd: &OwnedFd) -> Result<Option<String>, StorageError> {
    let mut hdr = [0u8; 20];
    let n = rustix::io::pread(fd, &mut hdr, 0)?;
    if n < hdr.len() || hdr[..4] != *b"QFI\xfb" {
        return Ok(None);
    }
    let offset = u64::from_be_bytes(hdr[8..16].try_into().unwrap_or_default());
    let size = u32::from_be_bytes(hdr[16..20].try_into().unwrap_or_default()) as usize;
    if offset == 0 || size == 0 {
        return Ok(None);
    }
    if size > 1023 {
        return Err(StorageError::Invalid(
            "qcow2 backing file name too long".into(),
        ));
    }
    let mut name = vec![0u8; size];
    let n = rustix::io::pread(fd, &mut name, offset)?;
    if n != size {
        return Err(StorageError::Invalid("truncated qcow2 header".into()));
    }
    String::from_utf8(name)
        .map(Some)
        .map_err(|_| StorageError::Invalid("qcow2 backing file name is not UTF-8".into()))
}

/// Runs `qemu-img` with fixed arguments, `--`, then paths.
async fn run_qemu_img(
    qemu_img: &Path,
    args: &[&str],
    paths: &[PathBuf],
) -> Result<(), StorageError> {
    let out = Command::new(qemu_img)
        .args(args)
        .arg("--")
        .args(paths)
        .output()
        .await
        .map_err(|e| StorageError::QemuImg(format!("{}: {e}", qemu_img.display())))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(StorageError::QemuImg(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}

/// Builds a `NoCloud` seed image (volume ID `CIDATA`).
fn build_seed_iso(
    user_data: &str,
    meta_data: &str,
    network_config: Option<&str>,
) -> Result<Vec<u8>, StorageError> {
    let mut b = prism_iso::IsoBuilder::new("CIDATA")
        .file("user-data", user_data.as_bytes())
        .file("meta-data", meta_data.as_bytes());
    if let Some(n) = network_config {
        b = b.file("network-config", n.as_bytes());
    }
    b.build()
        .map_err(|e| StorageError::Invalid(format!("building seed ISO: {e}")))
}

/// Reads the virtual size from a qcow2 header. Display only: image
/// metadata is untrusted and never used to make decisions.
fn qcow2_virtual_size(fd: &OwnedFd) -> Option<u64> {
    let mut hdr = [0u8; 32];
    let n = rustix::io::pread(fd, &mut hdr, 0).ok()?;
    (n == hdr.len() && hdr[..4] == *b"QFI\xfb")
        .then(|| u64::from_be_bytes(hdr[24..32].try_into().unwrap_or_default()))
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

    /// `qcow2` when the file starts with the qcow2 magic, else `raw`.
    /// Never runs a format probe in qemu.
    pub fn image_format(&self, name: &str) -> Option<String> {
        let fd = self.open_existing(name).ok()?;
        let mut magic = [0u8; 4];
        let n = rustix::io::pread(&fd, &mut magic, 0).ok()?;
        Some(
            if n == 4 && magic == *b"QFI\xfb" {
                "qcow2"
            } else {
                "raw"
            }
            .to_string(),
        )
    }

    /// The backing files below `name`, nearest first, read from qcow2
    /// headers. Every layer must be a plain volume in this pool.
    pub fn backing_chain(&self, name: &str) -> Result<Vec<String>, StorageError> {
        let mut chain = Vec::new();
        let mut current = name.to_string();
        for _ in 0..MAX_CHAIN {
            let fd = self.open_existing(&current)?;
            let Some(backing) = qcow2_backing_file(&fd)? else {
                return Ok(chain);
            };
            let rel = Path::new(&backing)
                .strip_prefix(&self.path)
                .map_or_else(|_| PathBuf::from(&backing), Path::to_path_buf);
            let rel = rel.to_str().unwrap_or_default().to_string();
            if !valid_name(&rel) {
                return Err(StorageError::Invalid(format!(
                    "{current} has a backing file outside its pool: {backing}"
                )));
            }
            chain.push(rel.clone());
            current = rel;
        }
        Err(StorageError::Invalid(format!(
            "{name}: backing chain is too deep"
        )))
    }

    /// Copies (and flattens) `src` into a new volume `dst`, optionally grown
    /// to `size` bytes.
    pub async fn copy_volume(
        &self,
        qemu_img: &Path,
        src: &str,
        dst: &str,
        format: &str,
        size: Option<u64>,
    ) -> Result<Volume, StorageError> {
        Self::check(src)?;
        Self::check(dst)?;
        let src_fmt = self
            .image_format(src)
            .ok_or_else(|| StorageError::NoVolume(src.into()))?;
        if !self.backing_chain(src)?.is_empty() {
            return Err(StorageError::Invalid(format!(
                "{src} has a backing file; flatten it first"
            )));
        }
        drop(self.create_exclusive(dst)?);
        let res = async {
            run_qemu_img(
                qemu_img,
                &["convert", "-q", "-f", &src_fmt, "-O", format],
                &[self.path.join(src), self.path.join(dst)],
            )
            .await?;
            if let Some(size) = size
                && self.volume(dst)?.size_bytes < size
            {
                self.resize_volume(qemu_img, dst, format, size).await?;
            }
            self.volume(dst)
        }
        .await;
        if res.is_err() {
            let _ = rustix::fs::unlinkat(&self.root, dst, AtFlags::empty());
        }
        res
    }

    /// Grows a volume to `size` bytes. Shrinking is refused.
    pub async fn resize_volume(
        &self,
        qemu_img: &Path,
        name: &str,
        format: &str,
        size: u64,
    ) -> Result<Volume, StorageError> {
        let current = self.volume(name)?;
        if size < current.size_bytes {
            return Err(StorageError::Invalid("disks can only grow".into()));
        }
        run_qemu_img(
            qemu_img,
            &["resize", "-q", "-f", format],
            &[self.path.join(name), PathBuf::from(size.to_string())],
        )
        .await?;
        self.volume(name)
    }

    fn seed_name(vm: &uuid::Uuid) -> String {
        format!(".prism-seed-{}.iso", vm.simple())
    }

    /// Writes the cloud-init `NoCloud` seed ISO for `vm` (replacing any old
    /// one) and returns its path. Hidden from volume listings.
    pub fn write_seed_iso(
        &self,
        vm: &uuid::Uuid,
        user_data: &str,
        meta_data: &str,
        network_config: Option<&str>,
    ) -> Result<PathBuf, StorageError> {
        let iso = build_seed_iso(user_data, meta_data, network_config)?;
        let name = Self::seed_name(vm);
        let partial = format!("{PARTIAL_PREFIX}{}", uuid::Uuid::new_v4().simple());
        let fd = self.create_exclusive(&partial)?;
        let mut file = std::fs::File::from(fd);
        let res = std::io::Write::write_all(&mut file, &iso).and_then(|()| file.sync_all());
        drop(file);
        if let Err(e) = res {
            let _ = rustix::fs::unlinkat(&self.root, &partial, AtFlags::empty());
            return Err(e.into());
        }
        rustix::fs::renameat(&self.root, &partial, &self.root, &name)?;
        Ok(self.path.join(name))
    }

    pub fn remove_seed_iso(&self, vm: &uuid::Uuid) {
        let _ = rustix::fs::unlinkat(&self.root, Self::seed_name(vm), AtFlags::empty());
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

    /// Writes `body` to a partial file and atomically renames it to `name`.
    pub async fn upload<S, E>(&self, name: &str, mut body: S) -> Result<Volume, StorageError>
    where
        S: futures::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        use futures::StreamExt;

        Self::check(name)?;
        if self.open_existing(name).is_ok() {
            return Err(StorageError::Exists(name.into()));
        }
        let partial = format!("{PARTIAL_PREFIX}{}", uuid::Uuid::new_v4().simple());
        let fd = self.create_exclusive(&partial)?;
        let mut file = tokio::fs::File::from_std(std::fs::File::from(fd));
        let res: Result<(), StorageError> = async {
            while let Some(chunk) = body.next().await {
                let chunk =
                    chunk.map_err(|e| StorageError::Invalid(format!("upload aborted: {e}")))?;
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
            name,
            RenameFlags::NOREPLACE,
        ) {
            let _ = rustix::fs::unlinkat(&self.root, &partial, AtFlags::empty());
            return Err(match e {
                rustix::io::Errno::EXIST => StorageError::Exists(name.into()),
                e => e.into(),
            });
        }
        self.volume(name)
    }

    /// Opens a volume for reading (confined).
    pub fn open_read(&self, name: &str) -> Result<std::fs::File, StorageError> {
        Self::check(name)?;
        Ok(std::fs::File::from(self.open_existing(name)?))
    }

    /// Starts receiving `name`: returns a hidden partial file to fill, then
    /// [`Self::commit_partial`] or [`Self::abort_partial`].
    pub fn begin_partial(&self, name: &str) -> Result<(String, std::fs::File), StorageError> {
        Self::check(name)?;
        if self.open_existing(name).is_ok() {
            return Err(StorageError::Exists(name.into()));
        }
        let partial = format!("{PARTIAL_PREFIX}{}", uuid::Uuid::new_v4().simple());
        let fd = rustix::fs::openat2(
            &self.root,
            &partial,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
            resolve(),
        )?;
        Ok((partial, std::fs::File::from(fd)))
    }

    pub fn commit_partial(&self, partial: &str, name: &str) -> Result<(), StorageError> {
        rustix::fs::renameat_with(&self.root, partial, &self.root, name, RenameFlags::NOREPLACE).map_err(|e| {
            let _ = rustix::fs::unlinkat(&self.root, partial, AtFlags::empty());
            match e {
                rustix::io::Errno::EXIST => StorageError::Exists(name.into()),
                e => e.into(),
            }
        })
    }

    pub fn abort_partial(&self, partial: &str) {
        let _ = rustix::fs::unlinkat(&self.root, partial, AtFlags::empty());
    }

    /// Removes partial uploads left by a crash.
    pub fn clean_partials(&self) {
        let Ok(vols) = rustix::fs::openat2(
            &self.root,
            ".",
            OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
            resolve(),
        ) else {
            return;
        };
        let Ok(dir) = rustix::fs::Dir::read_from(&vols) else {
            return;
        };
        for entry in dir.flatten() {
            if let Ok(name) = entry.file_name().to_str()
                && name.starts_with(PARTIAL_PREFIX)
            {
                let _ = rustix::fs::unlinkat(&self.root, name, AtFlags::empty());
            }
        }
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
