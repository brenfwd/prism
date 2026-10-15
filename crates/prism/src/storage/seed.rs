//! cloud-init `NoCloud` seed ISOs, kept hidden inside a pool.

use std::path::PathBuf;

use rustix::fs::AtFlags;

use super::{PARTIAL_PREFIX, Pool, StorageError};

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

impl Pool {
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
}
