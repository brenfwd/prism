//! Partial files for downloads written by another process.

use std::path::PathBuf;

use super::{PARTIAL_PREFIX, Pool, StorageError};

impl Pool {
    /// Reserves a partial file; returns its name and path.
    pub fn new_partial(&self) -> Result<(String, PathBuf), StorageError> {
        let name = format!("{PARTIAL_PREFIX}{}", uuid::Uuid::new_v4().simple());
        drop(self.create_exclusive(&name)?);
        let path = self.path.join(&name);
        Ok((name, path))
    }
}
