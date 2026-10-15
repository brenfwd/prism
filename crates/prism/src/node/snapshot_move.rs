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
use crate::spec::VmSpec;

impl Node {
    /// Snapshot XML for `spec`, parents first. Fails for memory snapshots,
    /// whose RAM image is not part of the disk chain.
    pub(super) async fn export_snapshots(&self, spec: &VmSpec) -> anyhow::Result<Vec<String>> {
        let c = self.client()?;
        let mut all = Vec::new();
        for snap in c.snapshots(&domain_ref(spec)).await? {
            let xml = c.snapshot_xml(&snap, op::SNAPSHOT_XML_SECURE).await?;
            if has_memory(&xml) {
                anyhow::bail!(
                    "{} has a snapshot with saved memory; delete it before moving",
                    spec.name
                );
            }
            all.push((created(&xml), xml));
        }
        all.sort_by_key(|(t, _)| *t);
        Ok(all.into_iter().map(|(_, x)| x).collect())
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
        let tops: Vec<&str> = spec
            .disks
            .iter()
            .filter_map(|d| d.volume.as_deref())
            .collect();
        for xml in xmls {
            let xml = rewrite_files(xml, pool_dir);
            let mut fl = op::SNAPSHOT_REDEFINE_CURRENT_FLAGS;
            fl &= !op::SNAPSHOT_CREATE_CURRENT;
            if is_current(&xml, &tops) {
                fl |= op::SNAPSHOT_CREATE_CURRENT;
            }
            c.snapshot_create(&dom, &xml, fl).await?;
        }
        Ok(())
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

fn name(xml: &str) -> &str {
    xml.split("<name>")
        .nth(1)
        .and_then(|s| s.split('<').next())
        .unwrap_or("")
}

/// The current snapshot is the one whose overlays are the active layers.
fn is_current(xml: &str, tops: &[&str]) -> bool {
    let n = name(xml);
    !n.is_empty() && tops.iter().any(|t| t.contains(&format!(".{n}.")))
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
    fn rewrites_directories() {
        let x = "<source file='/a/b/x.s1.qcow2'/><source file=\"/c/y.qcow2\"/>";
        assert_eq!(
            rewrite_files(x, Path::new("/pool")),
            "<source file='/pool/x.s1.qcow2'/><source file=\"/pool/y.qcow2\"/>"
        );
    }
}
