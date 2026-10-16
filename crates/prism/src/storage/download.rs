//! Partial files for downloads written by the node's download task.

use std::path::PathBuf;

use super::{PARTIAL_PREFIX, Pool, StorageError};

impl Pool {
    /// Creates an empty `PARTIAL_PREFIX` file and returns its name and path.
    /// The caller writes to the path, then calls `commit_partial` on success
    /// or `abort_partial` on failure; `clean_partials` removes leftovers.
    pub fn new_partial(&self) -> Result<(String, PathBuf), StorageError> {
        let name = format!("{PARTIAL_PREFIX}{}", uuid::Uuid::new_v4().simple());
        drop(self.create_exclusive(&name)?);
        let path = self.path.join(&name);
        Ok((name, path))
    }
}
