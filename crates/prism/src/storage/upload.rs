//! Streaming writes into a pool through partial files that are renamed
//! into place on success.

use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags};
use tokio::io::AsyncWriteExt;

use super::{PARTIAL_PREFIX, Pool, StorageError, Volume, resolve};

impl Pool {
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
        rustix::fs::renameat_with(
            &self.root,
            partial,
            &self.root,
            name,
            RenameFlags::NOREPLACE,
        )
        .map_err(|e| {
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
