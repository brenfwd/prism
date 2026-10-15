//! Disk images: qcow2 header reads and `qemu-img` copies, layers and resizes.

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use rustix::fs::AtFlags;
use tokio::process::Command;

use super::{Pool, StorageError, Volume, valid_name};

const MAX_CHAIN: usize = 64;

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

/// Reads the virtual size from a qcow2 header. Display only: image
/// metadata is untrusted and never used to make decisions.
pub(super) fn qcow2_virtual_size(fd: &OwnedFd) -> Option<u64> {
    let mut hdr = [0u8; 32];
    let n = rustix::io::pread(fd, &mut hdr, 0).ok()?;
    (n == hdr.len() && hdr[..4] == *b"QFI\xfb")
        .then(|| u64::from_be_bytes(hdr[24..32].try_into().unwrap_or_default()))
}

impl Pool {
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

    /// Creates an empty image of `size` bytes, optionally as a qcow2 overlay
    /// on `backing` (a volume in this pool).
    pub async fn create_image(
        &self,
        qemu_img: &Path,
        name: &str,
        format: &str,
        size: u64,
        backing: Option<&str>,
    ) -> Result<Volume, StorageError> {
        Self::check(name)?;
        drop(self.create_exclusive(name)?);
        let mut args: Vec<String> = vec!["create".into(), "-q".into(), "-f".into(), format.into()];
        if let Some(b) = backing {
            Self::check(b)?;
            let bfmt = self
                .image_format(b)
                .ok_or_else(|| StorageError::NoVolume(b.into()))?;
            args.extend([
                "-b".into(),
                self.path.join(b).to_string_lossy().into_owned(),
                "-F".into(),
                bfmt,
            ]);
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let res = run_qemu_img(
            qemu_img,
            &arg_refs,
            &[self.path.join(name), PathBuf::from(size.to_string())],
        )
        .await;
        if let Err(e) = res {
            let _ = rustix::fs::unlinkat(&self.root, name, AtFlags::empty());
            return Err(e);
        }
        self.volume(name)
    }

    /// Copies `src` with its whole backing chain flattened into a new
    /// qcow2 `dst` (our own VM disks; not for untrusted imports).
    pub async fn copy_volume_flatten(
        &self,
        qemu_img: &Path,
        src: &str,
        dst: &str,
    ) -> Result<(), StorageError> {
        let fmt = self
            .image_format(src)
            .ok_or_else(|| StorageError::NoVolume(src.into()))?;
        // Every backing layer must be inside this pool.
        self.backing_chain(src)?;
        drop(self.create_exclusive(dst)?);
        let res = run_qemu_img(
            qemu_img,
            &["convert", "-q", "-f", &fmt, "-O", "qcow2"],
            &[self.path.join(src), self.path.join(dst)],
        )
        .await;
        if let Err(e) = res {
            let _ = rustix::fs::unlinkat(&self.root, dst, AtFlags::empty());
            return Err(e);
        }
        Ok(())
    }

    /// Builds `dst` from backup layers (hidden partial files, base first):
    /// each incremental is linked onto the one before, then the top is
    /// flattened into a fresh image.
    pub async fn flatten_layers(
        &self,
        qemu_img: &Path,
        layers: &[String],
        dst: &str,
        format: &str,
    ) -> Result<Volume, StorageError> {
        Self::check(dst)?;
        let (first, rest) = layers
            .split_first()
            .ok_or_else(|| StorageError::Invalid("no backup layers".into()))?;
        let mut below = first.clone();
        for layer in rest {
            run_qemu_img(
                qemu_img,
                &[
                    "rebase",
                    "-q",
                    "-u",
                    "-f",
                    "qcow2",
                    "-F",
                    "qcow2",
                    "-b",
                    &self.path.join(&below).to_string_lossy(),
                ],
                &[self.path.join(layer)],
            )
            .await?;
            below.clone_from(layer);
        }
        drop(self.create_exclusive(dst)?);
        let res = run_qemu_img(
            qemu_img,
            &["convert", "-q", "-f", "qcow2", "-O", format],
            &[self.path.join(&below), self.path.join(dst)],
        )
        .await;
        if let Err(e) = res {
            let _ = rustix::fs::unlinkat(&self.root, dst, AtFlags::empty());
            return Err(e);
        }
        self.volume(dst)
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
}
