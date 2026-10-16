//! Disk-chain bookkeeping for snapshots: which files a VM's chains use and
//! keeping the spec's active layer in step with libvirt.

use std::collections::BTreeMap;
use std::path::Path;

use prism_libvirt::proto::flags;

use uuid::Uuid;

use super::snapshot_scan::{memory_file, snapshot_overlays};
use super::{Node, Result, domain_ref};
use crate::render;
use crate::spec::{DiskDevice, DiskSpec, VmSpec};
use crate::storage::valid_name;

impl Node {
    /// All files in every disk chain plus snapshot memory images, as
    /// `(pool, volume)`; used to clean up when the VM is deleted.
    pub(crate) async fn snapshot_files(&self, spec: &VmSpec) -> Result<Vec<(String, String)>> {
        let mut files = chain_files(spec);
        let c = self.client()?;
        for snap in c.snapshots(&domain_ref(spec)).await.unwrap_or_default() {
            let Ok(xml) = c.snapshot_xml(&snap, 0).await else {
                continue;
            };
            // The overlays a snapshot created stay behind when a revert drops the disk that
            // used them, and only the snapshot still names them.
            for path in memory_file(&xml).into_iter().chain(snapshot_overlays(&xml)) {
                for pool in self.storage.list() {
                    if let Ok(rest) = path.strip_prefix(&pool.path)
                        && let Some(f) = rest.to_str()
                    {
                        files.push((pool.name.clone(), f.to_string()));
                    }
                }
            }
        }
        Ok(files)
    }

    /// The file libvirt currently uses for `disk`, as a volume name, when it
    /// differs from the spec (for example right after a snapshot).
    pub(crate) async fn active_volume(
        &self,
        spec: &VmSpec,
        disk: &DiskSpec,
    ) -> Result<Option<String>> {
        let c = self.client()?;
        let xml = c
            .get_xml_desc(&domain_ref(spec), flags::DOMAIN_XML_INACTIVE)
            .await?;
        let pool = self.storage.get(&disk.pool)?;
        Ok(render::disk_sources(&xml)
            .into_iter()
            .find(|(t, _)| *t == disk.target)
            .and_then(|(_, p)| p)
            .and_then(|p| relative_to(&p, &pool.path)))
    }

    /// Re-reads each disk's active file from libvirt's persistent definition
    /// and its backing chain from the qcow2 headers, and stores both.
    pub(crate) async fn sync_disk_chains(&self, id: Uuid) -> Result<()> {
        let c = self.client()?;
        let old = self.local_spec(id)?;
        let xml = c
            .get_xml_desc(&domain_ref(&old), flags::DOMAIN_XML_INACTIVE)
            .await?;
        let sources: BTreeMap<String, std::path::PathBuf> = render::disk_sources(&xml)
            .into_iter()
            .filter_map(|(t, p)| p.map(|p| (t, p)))
            .collect();
        let mut spec = old.clone();
        for disk in spec
            .disks
            .iter_mut()
            .filter(|d| d.device == DiskDevice::Disk)
        {
            let pool = self.storage.get(&disk.pool)?;
            let Some(active) = sources
                .get(&disk.target)
                .and_then(|p| relative_to(p, &pool.path))
            else {
                tracing::warn!(vm = %old.name, disk = %disk.target, "active layer is outside its pool; leaving spec unchanged");
                continue;
            };
            let chain = pool.backing_chain(&active)?;
            disk.format = pool
                .image_format(&active)
                .unwrap_or_else(|| disk.format.clone());
            disk.volume = Some(active);
            disk.backing = chain.into_iter().rev().collect();
        }
        if spec != old {
            spec.generation += 1;
            self.put_spec(&spec, Some(old.generation)).await?;
        }
        Ok(())
    }
}

pub(super) fn chain_files(spec: &VmSpec) -> Vec<(String, String)> {
    spec.disks
        .iter()
        .filter(|d| d.device == DiskDevice::Disk)
        .flat_map(|d| {
            d.volume
                .iter()
                .chain(d.backing.iter())
                .map(|v| (d.pool.clone(), v.clone()))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn relative_to(path: &Path, dir: &Path) -> Option<String> {
    let rest = path.strip_prefix(dir).ok()?;
    let name = rest.to_str()?;
    valid_name(name).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative_to(Path::new("/p/a.qcow2"), Path::new("/p")).as_deref(),
            Some("a.qcow2")
        );
        assert_eq!(relative_to(Path::new("/q/a.qcow2"), Path::new("/p")), None);
        assert_eq!(relative_to(Path::new("/p/sub/a"), Path::new("/p")), None);
    }
}
