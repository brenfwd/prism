//! Live external snapshots: create, list, revert, delete.
//!
//! libvirt switches each disk to a new overlay on snapshot, revert and
//! delete, so after every operation the spec's active layer and backing
//! chain are re-read ([`Node::sync_disk_chains`]); otherwise the next
//! definition from the spec would point at a stale layer.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

use prism_libvirt::DomainChild;
use prism_libvirt::ops::flags as op;
use prism_libvirt::proto::flags;
use quick_xml::escape::escape;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Node, NodeError, Result, domain_ref};
use crate::render;
use crate::spec::{DiskDevice, DiskSpec, VmSpec};
use crate::storage::valid_name;
use crate::tasks::Task;

/// libvirt 9.9 reverts and deletes external snapshots natively.
const MIN_LIBVIRT_EXTERNAL: u64 = 9_009_000;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSnapshot {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: String,
    /// Also save guest RAM, so a revert resumes exactly where it was.
    #[serde(default)]
    pub memory: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotView {
    pub name: String,
    pub description: String,
    pub created: Option<i64>,
    /// VM state when taken (`running`, `shutoff`, `disk-snapshot`, …).
    pub state: String,
    pub parent: Option<String>,
    pub memory: bool,
    pub current: bool,
}

/// Text of the first `<tag>` directly or indirectly under the root, and
/// attributes of `<memory>`.
fn parse_snapshot(xml: &str) -> SnapshotView {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    let mut path: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut v = SnapshotView {
        name: String::new(),
        description: String::new(),
        created: None,
        state: String::new(),
        parent: None,
        memory: false,
        current: false,
    };
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => {
                path.push(e.name().as_ref().to_string());
                text.clear();
            }
            Event::Empty(e) => {
                if e.name().as_ref() == "memory" && path.len() == 1 {
                    v.memory = e
                        .attributes()
                        .flatten()
                        .any(|a| a.key.as_ref() == "snapshot" && a.value.as_ref() != "no");
                }
            }
            Event::Text(t) => text.push_str(&t.xml_content(quick_xml::XmlVersion::Implicit1_0)),
            Event::GeneralRef(r) => match r.resolve_char_ref() {
                Ok(Some(c)) => text.push(c),
                _ => {
                    if let Some(v) = quick_xml::escape::resolve_predefined_entity(r.as_ref()) {
                        text.push_str(v);
                    }
                }
            },
            Event::End(_) => {
                let p: Vec<&str> = path.iter().map(String::as_str).collect();
                let t = std::mem::take(&mut text);
                match p.as_slice() {
                    ["domainsnapshot", "name"] => v.name = t,
                    ["domainsnapshot", "description"] => v.description = t,
                    ["domainsnapshot", "creationTime"] => v.created = t.trim().parse().ok(),
                    ["domainsnapshot", "state"] => v.state = t,
                    ["domainsnapshot", "parent", "name"] => v.parent = Some(t),
                    _ => {}
                }
                path.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    v
}

fn default_name() -> String {
    // Seconds since the epoch keep names sortable and unique enough.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format!("snap-{secs}")
}

/// Overlay file for `disk` in snapshot `snap`: `<stem>.<snap>.qcow2`.
fn overlay_name(disk: &DiskSpec, snap: &str) -> String {
    let base = disk.volume.as_deref().unwrap_or("disk");
    let stem = base.rsplit_once('.').map_or(base, |(s, _)| s);
    // Drop an older snapshot suffix so names don't grow without bound.
    let stem = disk
        .backing
        .first()
        .and_then(|b| b.rsplit_once('.').map(|(s, _)| s.to_string()))
        .unwrap_or_else(|| stem.to_string());
    format!("{stem}.{snap}.qcow2")
}

impl Node {
    fn snapshot_api_check(&self) -> Result<()> {
        if self.libvirt_version() < MIN_LIBVIRT_EXTERNAL {
            return Err(NodeError::Conflict(
                "reverting and deleting external snapshots needs libvirt 9.9 or newer".into(),
            ));
        }
        Ok(())
    }

    pub async fn snapshots(&self, id: Uuid) -> Result<Vec<SnapshotView>> {
        let c = self.client()?;
        let spec = self.local_spec(id)?;
        let dom = domain_ref(&spec);
        let current_xml = c.get_xml_desc(&dom, flags::DOMAIN_XML_INACTIVE).await?;
        let mut out = Vec::new();
        for snap in c.snapshots(&dom).await? {
            let xml = c.snapshot_xml(&snap, 0).await?;
            out.push(parse_snapshot(&xml));
        }
        // The current snapshot is the one whose overlays are active now.
        let active: Vec<String> = render::disk_sources(&current_xml)
            .into_iter()
            .filter_map(|(_, p)| p.map(|p| p.to_string_lossy().into_owned()))
            .collect();
        for s in &mut out {
            s.current = active
                .iter()
                .any(|p| p.contains(&format!(".{}.qcow2", s.name)));
        }
        out.sort_by_key(|s| s.created);
        Ok(out)
    }

    pub fn create_snapshot(
        self: &std::sync::Arc<Self>,
        id: Uuid,
        req: CreateSnapshot,
    ) -> Result<Task> {
        let spec = self.local_spec(id)?;
        let name = req.name.clone().unwrap_or_else(default_name);
        if !valid_name(&name) || name.len() > 64 {
            return Err(NodeError::Invalid(
                "snapshot names match [A-Za-z0-9._-]{1,64}".into(),
            ));
        }
        let resource = format!("/vms/{id}");
        if self.tasks.busy(&resource) {
            return Err(NodeError::Conflict(
                "another task is running on this VM".into(),
            ));
        }
        let node = std::sync::Arc::clone(self);
        Ok(self
            .tasks
            .spawn("snapshot.create", resource, move |ctx| async move {
                ctx.progress(Some(0.05), format!("preparing snapshot {name}"));
                node.do_create_snapshot(&spec, &name, &req, &ctx).await?;
                ctx.progress(Some(1.0), format!("snapshot {name} created"));
                Ok(serde_json::json!({ "name": name }))
            }))
    }

    async fn do_create_snapshot(
        &self,
        spec: &VmSpec,
        name: &str,
        req: &CreateSnapshot,
        ctx: &crate::tasks::TaskCtx,
    ) -> Result<()> {
        let _guard = self.ops.lock().await;
        let c = self.client()?;
        let dom = domain_ref(spec);
        let rt = self.runtime(spec.id);
        let mut xml = format!(
            "<domainsnapshot>\n  <name>{}</name>\n  <description>{}</description>\n",
            escape(name),
            escape(&req.description)
        );
        let memory_file = if req.memory && rt.state.is_active() {
            let disk = spec
                .disks
                .iter()
                .find(|d| d.device == DiskDevice::Disk)
                .ok_or_else(|| {
                    NodeError::Invalid("a memory snapshot needs a disk to store it beside".into())
                })?;
            let pool = self.storage.get(&disk.pool)?;
            let file = pool.path.join(format!("{}.{name}.mem", spec.name));
            let _ = writeln!(
                xml,
                "  <memory snapshot='external' file='{}'/>",
                escape(file.to_string_lossy().as_ref())
            );
            Some(file)
        } else {
            xml.push_str("  <memory snapshot='no'/>\n");
            None
        };
        xml.push_str("  <disks>\n");
        for disk in &spec.disks {
            match (disk.device, &disk.volume) {
                (DiskDevice::Disk, Some(_)) => {
                    let pool = self.storage.get(&disk.pool)?;
                    let overlay = overlay_name(disk, name);
                    if pool.volume(&overlay).is_ok() {
                        return Err(NodeError::Conflict(format!("{overlay} already exists")));
                    }
                    let _ = writeln!(
                        xml,
                        "    <disk name='{}' snapshot='external' type='file'>\n      <driver type='qcow2'/>\n      <source file='{}'/>\n    </disk>",
                        escape(&disk.target),
                        escape(pool.path.join(&overlay).to_string_lossy().as_ref())
                    );
                }
                _ => {
                    let _ = writeln!(
                        xml,
                        "    <disk name='{}' snapshot='no'/>",
                        escape(&disk.target)
                    );
                }
            }
        }
        xml.push_str("  </disks>\n</domainsnapshot>\n");

        ctx.progress(
            Some(0.2),
            if memory_file.is_some() {
                "saving memory and disks"
            } else {
                "freezing disks into overlays"
            },
        );
        let mut fl = op::SNAPSHOT_CREATE_ATOMIC;
        if memory_file.is_some() {
            fl |= op::SNAPSHOT_CREATE_LIVE;
        } else {
            fl |= op::SNAPSHOT_CREATE_DISK_ONLY;
            if rt.agent {
                fl |= op::SNAPSHOT_CREATE_QUIESCE;
            }
        }
        match c.snapshot_create(&dom, &xml, fl).await {
            Ok(_) => {}
            // Quiescing needs a cooperative agent; fall back to crash-consistent.
            Err(e) if fl & op::SNAPSHOT_CREATE_QUIESCE != 0 => {
                tracing::warn!(vm = %spec.name, "quiesced snapshot failed ({e}); retrying without");
                ctx.progress(Some(0.4), "guest agent could not quiesce; retrying");
                c.snapshot_create(&dom, &xml, fl & !op::SNAPSHOT_CREATE_QUIESCE)
                    .await?;
            }
            Err(e) => return Err(e.into()),
        }
        ctx.progress(Some(0.85), "updating disk chains");
        tracing::info!(vm = %spec.name, snapshot = name, "snapshot created");
        self.sync_disk_chains(spec.id).await
    }

    pub fn revert_snapshot(self: &std::sync::Arc<Self>, id: Uuid, name: String) -> Result<Task> {
        self.snapshot_api_check()?;
        let spec = self.local_spec(id)?;
        let node = std::sync::Arc::clone(self);
        let resource = format!("/vms/{id}");
        if self.tasks.busy(&resource) {
            return Err(NodeError::Conflict(
                "another task is running on this VM".into(),
            ));
        }
        Ok(self
            .tasks
            .spawn("snapshot.revert", resource, move |ctx| async move {
                ctx.progress(None, format!("reverting to {name}"));
                let _guard = node.ops.lock().await;
                let c = node.client()?;
                let snap = c
                    .snapshot_lookup(&domain_ref(&spec), &name)
                    .await
                    .map_err(NodeError::from)?;
                c.snapshot_revert(&snap, 0).await.map_err(NodeError::from)?;
                node.sync_disk_chains(spec.id).await?;
                node.refresh(&spec).await;
                Ok(serde_json::json!({ "name": name }))
            }))
    }

    pub fn delete_snapshot(self: &std::sync::Arc<Self>, id: Uuid, name: String) -> Result<Task> {
        self.snapshot_api_check()?;
        let spec = self.local_spec(id)?;
        let node = std::sync::Arc::clone(self);
        let resource = format!("/vms/{id}");
        if self.tasks.busy(&resource) {
            return Err(NodeError::Conflict(
                "another task is running on this VM".into(),
            ));
        }
        Ok(self
            .tasks
            .spawn("snapshot.delete", resource, move |ctx| async move {
                ctx.progress(None, format!("merging and deleting {name}"));
                let _guard = node.ops.lock().await;
                let c = node.client()?;
                let dom = domain_ref(&spec);
                let snap: DomainChild = c
                    .snapshot_lookup(&dom, &name)
                    .await
                    .map_err(NodeError::from)?;
                let xml = c.snapshot_xml(&snap, 0).await.map_err(NodeError::from)?;
                let before = chain_files(&spec);
                c.snapshot_delete(&snap, 0).await.map_err(NodeError::from)?;
                node.sync_disk_chains(spec.id).await?;
                // libvirt removes merged overlays; clean up the memory image.
                let after = chain_files(&node.local_spec(spec.id)?);
                for (pool, file) in before.iter().filter(|f| !after.contains(f)) {
                    if let Ok(p) = node.storage.get(pool) {
                        let _ = p.delete_volume(file);
                    }
                }
                if let Some(mem) = memory_file(&xml) {
                    for pool in node.storage.list() {
                        if let Ok(rest) = mem.strip_prefix(&pool.path)
                            && let Some(f) = rest.to_str()
                        {
                            let _ = pool.delete_volume(f);
                        }
                    }
                }
                Ok(serde_json::json!({ "name": name }))
            }))
    }

    /// All files in every disk chain plus snapshot memory images, as
    /// `(pool, volume)`; used to clean up when the VM is deleted.
    pub(crate) async fn snapshot_files(&self, spec: &VmSpec) -> Result<Vec<(String, String)>> {
        let mut files = chain_files(spec);
        let c = self.client()?;
        for snap in c.snapshots(&domain_ref(spec)).await.unwrap_or_default() {
            if let Ok(xml) = c.snapshot_xml(&snap, 0).await
                && let Some(mem) = memory_file(&xml)
            {
                for pool in self.storage.list() {
                    if let Ok(rest) = mem.strip_prefix(&pool.path)
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

fn chain_files(spec: &VmSpec) -> Vec<(String, String)> {
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

fn memory_file(snapshot_xml: &str) -> Option<std::path::PathBuf> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(snapshot_xml);
    let mut depth = 0usize;
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) => {
                depth += 1;
                if depth == 2 && e.name().as_ref() == "memory" {
                    return file_attr(&e);
                }
            }
            Event::Empty(e) if depth == 1 && e.name().as_ref() == "memory" => return file_attr(&e),
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Eof => return None,
            _ => {}
        }
    }
}

fn file_attr(e: &quick_xml::events::BytesStart<'_>) -> Option<std::path::PathBuf> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == "file")
        .map(|a| std::path::PathBuf::from(a.value.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = "<domainsnapshot>\
        <name>before-upgrade</name><description>pre &amp; post</description>\
        <state>running</state><parent><name>first</name></parent>\
        <creationTime>1791936067</creationTime>\
        <memory snapshot='external' file='/p/web.before-upgrade.mem'/>\
        <disks><disk name='vda' snapshot='external'/></disks>\
        <domain><name>web</name><memory unit='KiB'>1</memory></domain>\
        </domainsnapshot>";

    #[test]
    fn parses_snapshot() {
        let s = parse_snapshot(XML);
        assert_eq!(s.name, "before-upgrade");
        assert_eq!(s.description, "pre & post");
        assert_eq!(s.state, "running");
        assert_eq!(s.parent.as_deref(), Some("first"));
        assert_eq!(s.created, Some(1_791_936_067));
        assert!(s.memory);
        assert_eq!(
            memory_file(XML).unwrap(),
            Path::new("/p/web.before-upgrade.mem")
        );
    }

    #[test]
    fn overlay_names() {
        let mut d = DiskSpec {
            id: Uuid::nil(),
            pool: "p".into(),
            volume: Some("web-disk0.qcow2".into()),
            device: DiskDevice::Disk,
            format: "qcow2".into(),
            boot_order: None,
            target: "vda".into(),
            backing: vec![],
        };
        assert_eq!(overlay_name(&d, "s1"), "web-disk0.s1.qcow2");
        d.backing = vec!["web-disk0.qcow2".into()];
        d.volume = Some("web-disk0.s1.qcow2".into());
        assert_eq!(overlay_name(&d, "s2"), "web-disk0.s2.qcow2");
    }

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
