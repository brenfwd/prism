//! Carrying snapshot metadata across a cold move or live migration.
//!
//! The overlay files travel with the disk chain; libvirt's snapshot
//! metadata does not, so it is exported as XML before the hand-over and
//! redefined on the destination with disk paths pointed at its pool.

use std::path::Path;

use prism_libvirt::ops::flags as op;
use quick_xml::escape::escape;
use uuid::Uuid;

use super::{Node, Result, domain_ref};
use crate::spec::{DiskDevice, VmSpec};

impl Node {
    /// Snapshot XML for `spec`, parents first, plus the saved-memory images
    /// (`(pool, volume)`) that must travel with the disk chain.
    pub(super) async fn export_snapshots(
        &self,
        spec: &VmSpec,
    ) -> anyhow::Result<(Vec<String>, Vec<(String, String)>)> {
        let c = self.client()?;
        let mut all = Vec::new();
        let mut memory = Vec::new();
        for snap in c.snapshots(&domain_ref(spec)).await? {
            let xml = c.snapshot_xml(&snap, op::SNAPSHOT_XML_SECURE).await?;
            if let Some(file) = super::snapshot_scan::memory_file(&xml) {
                let base = file.file_name().and_then(|f| f.to_str());
                let pool = spec.disks.iter().find(|d| d.device == DiskDevice::Disk);
                let (Some(base), Some(pool)) = (base, pool) else {
                    anyhow::bail!("{}: cannot locate a snapshot memory image", spec.name);
                };
                memory.push((pool.pool.clone(), base.to_string()));
            } else if has_memory(&xml) {
                anyhow::bail!("{}: snapshot memory image has no file", spec.name);
            }
            let mut xml = xml;
            if c.snapshot_is_current(&snap).await.unwrap_or(false) {
                // libvirt keeps "current" outside the XML; carry it along.
                xml.push_str(CURRENT_MARK);
            }
            all.push((created(&xml), xml));
        }
        all.sort_by_key(|(t, _)| *t);
        self.snapshot_layer_files(spec, &all, &mut memory)?;
        Ok((all.into_iter().map(|(_, x)| x).collect(), memory))
    }

    /// Adds the disk layers the snapshots refer to but the active chain no
    /// longer contains (after reverting to an earlier snapshot), with their
    /// backing files, so a later revert on the destination finds them.
    fn snapshot_layer_files(
        &self,
        spec: &VmSpec,
        snapshots: &[(i64, String)],
        extra: &mut Vec<(String, String)>,
    ) -> anyhow::Result<()> {
        let Some(disk) = spec.disks.iter().find(|d| d.device == DiskDevice::Disk) else {
            return Ok(());
        };
        let pool = self.storage.get(&disk.pool)?;
        let active: Vec<&String> = spec
            .disks
            .iter()
            .filter(|d| d.device == DiskDevice::Disk)
            .flat_map(|d| d.volume.iter().chain(d.backing.iter()))
            .collect();
        for (_, xml) in snapshots {
            for file in referenced_files(xml) {
                let Some(name) = file
                    .strip_prefix(&pool.path)
                    .ok()
                    .and_then(|r| r.to_str())
                    .filter(|n| !n.starts_with('.'))
                else {
                    continue;
                };
                if pool.volume(name).is_err() {
                    continue;
                }
                let chain = pool.backing_chain(name).unwrap_or_default();
                // Backing files first, like the active chain.
                for n in chain.iter().rev().map(String::as_str).chain([name]) {
                    let item = (disk.pool.clone(), n.to_string());
                    if !active.contains(&&item.1) && !extra.contains(&item) {
                        extra.push(item);
                    }
                }
            }
        }
        Ok(())
    }

    /// Redefines `xmls` on this node's (already defined) domain for `spec`,
    /// with files under `pool_dir`.
    pub(crate) async fn import_snapshots(
        &self,
        spec: &VmSpec,
        xmls: &[String],
        pool_dir: &Path,
    ) -> Result<()> {
        if xmls.is_empty() {
            return Ok(());
        }
        let c = self.client()?;
        let dom = domain_ref(spec);
        let mut current = None;
        for xml in xmls {
            let marked = xml.ends_with(CURRENT_MARK);
            let xml = rewrite_files(xml.trim_end_matches(CURRENT_MARK), pool_dir);
            let fl = op::SNAPSHOT_CREATE_REDEFINE;
            c.snapshot_create(&dom, &xml, fl).await?;
            if marked {
                current = Some(xml);
            }
        }
        // Redefining the current one last makes it current again; a wrong
        // current snapshot would make a later revert delete live layers.
        if let Some(xml) = current {
            c.snapshot_create(&dom, &xml, op::SNAPSHOT_REDEFINE_CURRENT_FLAGS)
                .await?;
        }
        Ok(())
    }

    /// Drops the snapshot metadata of `spec`'s domain (files stay), which
    /// libvirt requires before a live migration.
    pub(super) async fn detach_snapshots(&self, spec: &VmSpec) -> anyhow::Result<()> {
        let c = self.client()?;
        for snap in c.snapshots(&domain_ref(spec)).await? {
            c.snapshot_delete(&snap, op::SNAPSHOT_DELETE_METADATA_ONLY)
                .await?;
        }
        Ok(())
    }

    /// Redefines metadata removed by [`Self::detach_snapshots`] after a
    /// migration that did not happen.
    pub(super) async fn restore_snapshots(&self, spec: &VmSpec, xmls: &[String]) {
        let Some(dir) = spec.disks.first().and_then(|d| self.pool_dir(&d.pool)) else {
            return;
        };
        if let Err(e) = self.import_snapshots(spec, xmls, &dir).await {
            tracing::error!(vm = %spec.name, "restoring snapshot metadata: {e}");
        }
    }

    /// Redefines snapshots carried in a freshly adopted spec, then clears them.
    pub(super) async fn adopt_snapshots(&self, spec: &VmSpec) {
        if spec.snapshot_xml.is_empty() {
            return;
        }
        let Some(dir) = spec.disks.first().and_then(|d| self.pool_loc(&d.pool)) else {
            return;
        };
        if let Err(e) = self
            .import_snapshots(spec, &spec.snapshot_xml, &dir.path)
            .await
        {
            tracing::error!(vm = %spec.name, "redefining snapshots: {e}");
            return;
        }
        let mut done = spec.clone();
        done.snapshot_xml.clear();
        done.generation += 1;
        if let Err(e) = self.put_spec(&done, Some(spec.generation)).await {
            tracing::warn!(vm = %spec.name, "clearing carried snapshots: {e}");
        }
    }

    /// Looks up the destination pool directory for a migrated VM.
    pub(crate) fn pool_dir(&self, pool: &str) -> Option<std::path::PathBuf> {
        self.pool_loc(pool).map(|p| p.path)
    }

    /// Used by migration finish.
    pub(crate) async fn import_migrated_snapshots(
        &self,
        id: Uuid,
        pool: &str,
        spec: &VmSpec,
        xmls: &[String],
    ) {
        let Some(dir) = self.pool_dir(pool) else {
            return;
        };
        if let Err(e) = self.import_snapshots(spec, xmls, &dir).await {
            tracing::error!(vm = %id, "redefining snapshots after migration: {e}");
        }
    }
}

/// Every `file='…'` path in `xml`.
fn referenced_files(xml: &str) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("file=") {
        let tail = &rest[i + 5..];
        let Some(q) = tail.chars().next().filter(|c| *c == '\'' || *c == '"') else {
            rest = tail;
            continue;
        };
        let body = &tail[1..];
        let Some(end) = body.find(q) else { break };
        out.push(std::path::PathBuf::from(&body[..end]));
        rest = &body[end + 1..];
    }
    out
}

/// Trailing marker on the XML of the snapshot that was current.
const CURRENT_MARK: &str = "\n<!-- prism:current -->";

fn has_memory(xml: &str) -> bool {
    xml.contains("<memory snapshot='external'")
        || xml.contains("<memory snapshot=\"external\"")
        || xml.contains("<memory snapshot='internal'")
}

fn created(xml: &str) -> i64 {
    xml.split("<creationTime>")
        .nth(1)
        .and_then(|s| s.split('<').next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Points every `file='…'` attribute at `dir`, keeping the file name.
fn rewrite_files(xml: &str, dir: &Path) -> String {
    let dir = escape(dir.to_string_lossy().as_ref()).into_owned();
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find("file=") {
        let (head, tail) = rest.split_at(i + 5);
        out.push_str(head);
        let Some(q) = tail.chars().next().filter(|c| *c == '\'' || *c == '"') else {
            rest = tail;
            continue;
        };
        let body = &tail[1..];
        let Some(end) = body.find(q) else {
            rest = tail;
            continue;
        };
        let value = &body[..end];
        let base = value.rsplit('/').next().unwrap_or(value);
        out.push(q);
        out.push_str(&dir);
        out.push('/');
        out.push_str(base);
        out.push(q);
        rest = &body[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_referenced_files() {
        let x = "<a file='/p/x.s1.qcow2'/><b file=\"/p/y.mem\"/><c file=/>";
        assert_eq!(
            referenced_files(x),
            vec![
                std::path::PathBuf::from("/p/x.s1.qcow2"),
                std::path::PathBuf::from("/p/y.mem")
            ]
        );
    }

    #[test]
    fn rewrites_directories() {
        let x = "<source file='/a/b/x.s1.qcow2'/><source file=\"/c/y.qcow2\"/>";
        assert_eq!(
            rewrite_files(x, Path::new("/pool")),
            "<source file='/pool/x.s1.qcow2'/><source file=\"/pool/y.qcow2\"/>"
        );
    }
}
