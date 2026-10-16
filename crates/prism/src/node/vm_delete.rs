//! Deleting a VM and the files it owns.

use serde::Serialize;
use uuid::Uuid;

use super::snapshot_chain::chain_files;
use super::{Node, Result, domain_ref, lock, not_running, undefine_flags};

/// What a file is to the VM that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteKind {
    /// A disk image, a snapshot overlay or a backing layer.
    Disk,
    /// A saved memory image from a live snapshot.
    Snapshot,
}

/// One file that deleting a VM would remove.
#[derive(Debug, Clone, Serialize)]
pub struct DeleteItem {
    pub pool: String,
    pub volume: String,
    pub kind: DeleteKind,
    pub size_bytes: u64,
    /// Other VMs that use the file. A shared file is never deleted.
    pub shared_with: Vec<String>,
}

/// The files a VM delete with disks would remove.
#[derive(Debug, Clone, Serialize)]
pub struct DeletePlan {
    pub items: Vec<DeleteItem>,
}

impl Node {
    /// The files that `delete_vm` removes when asked to delete disks.
    /// ISOs, archive files and empty CD-ROM trays are never listed.
    pub async fn delete_plan(&self, id: Uuid) -> Result<DeletePlan> {
        let spec = self.local_spec(id)?;
        let chain = chain_files(&spec);
        let mut files: Vec<(String, String, DeleteKind)> = chain
            .iter()
            .cloned()
            .map(|(p, v)| (p, v, DeleteKind::Disk))
            .collect();
        for (p, v) in self.snapshot_files(&spec).await.unwrap_or_default() {
            if !chain.contains(&(p.clone(), v.clone())) {
                files.push((p, v, DeleteKind::Snapshot));
            }
        }
        files.sort();
        files.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        let own: Vec<(String, String)> = files
            .iter()
            .map(|(p, v, _)| (p.clone(), v.clone()))
            .collect();
        let mut items = Vec::with_capacity(files.len());
        for (pool, volume, kind) in files {
            let size_bytes = self
                .storage
                .get(&pool)
                .and_then(|p| p.volume(&volume))
                .map_or(0, |v| v.file_bytes);
            let mut shared_with: Vec<String> = self
                .volume_users(&pool, &volume)?
                .into_iter()
                .filter(|n| *n != spec.name)
                .collect();
            shared_with.extend(self.backing_dependents(&pool, &volume, &own));
            items.push(DeleteItem {
                pool,
                volume,
                kind,
                size_bytes,
                shared_with,
            });
        }
        Ok(DeletePlan { items })
    }

    /// Volumes in `pool` outside this VM's own files that have `volume` in
    /// their qcow2 backing chain, such as a standalone clone. Deleting
    /// `volume` would break them.
    fn backing_dependents(
        &self,
        pool: &str,
        volume: &str,
        own: &[(String, String)],
    ) -> Vec<String> {
        let Ok(p) = self.storage.get(pool) else {
            return Vec::new();
        };
        let Ok(vols) = p.volumes() else {
            return Vec::new();
        };
        vols.into_iter()
            .map(|v| v.name)
            .filter(|n| n != volume && !own.iter().any(|(op, ov)| op == pool && ov == n))
            .filter(|n| {
                p.backing_chain(n)
                    .is_ok_and(|c| c.iter().any(|b| b == volume))
            })
            .map(|n| format!("volume {n}"))
            .collect()
    }

    /// Powers the VM off, forgets it (libvirt drops its snapshot metadata,
    /// managed-save image and NVRAM) and removes its seed ISO. With
    /// `delete_disks` it also deletes the disk files and snapshot files
    /// that no other VM uses.
    pub async fn delete_vm(&self, id: Uuid, delete_disks: bool) -> Result<()> {
        let c = self.client()?;
        let _guard = self.ops.lock().await;
        let plan = self.delete_plan(id).await?;
        let spec = self.local_spec(id)?;
        let dom = domain_ref(&spec);
        match c.destroy(&dom, 0).await {
            Ok(()) => {}
            Err(e) if not_running(&e) => {}
            Err(e) => return Err(e.into()),
        }
        match c.undefine(&dom, undefine_flags()).await {
            Ok(()) => {}
            Err(e) if e.remote_code() == Some(prism_libvirt::proto::error_codes::NO_DOMAIN) => {}
            Err(e) => return Err(e.into()),
        }
        self.state
            .apply(crate::state::Command::DeleteVm { id })
            .await?;
        lock(&self.runtime).remove(&id);
        self.remove_seed_iso(&spec);
        if delete_disks {
            for item in plan.items {
                if !item.shared_with.is_empty() {
                    tracing::info!(
                        vm = %spec.name,
                        "keeping {}/{}: also used by {}",
                        item.pool,
                        item.volume,
                        item.shared_with.join(", ")
                    );
                    continue;
                }
                let res = self
                    .storage
                    .get(&item.pool)
                    .and_then(|p| p.delete_volume(&item.volume));
                if let Err(e) = res {
                    tracing::warn!(vm = %spec.name, "deleting {}/{}: {e}", item.pool, item.volume);
                }
            }
        }
        tracing::info!(vm = %spec.name, "deleted VM");
        Ok(())
    }
}
