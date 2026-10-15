//! Backups into archive pools, and restores from them.
//!
//! Running VMs use libvirt push-mode backups with checkpoints, so after the
//! first full backup each one copies only changed blocks; the guest never
//! pauses. Stopped VMs are copied by flattening their disk chains. Each
//! disk image is staged in the VM's pool, streamed into an archive pool on
//! any node, and recorded with the VM's spec for restore.

use std::fmt::Write;
use std::sync::Arc;
use std::time::Duration;

use prism_libvirt::DomainEvent;
use quick_xml::escape::escape;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Node, NodeError, Result, domain_ref};
use crate::archive::{ArchiveFileDoc, ArchiveMemberDoc, FileKind, Replica, place};
use crate::spec::{DiskDevice, VmSpec, random_mac};
use crate::state::Command;
use crate::tasks::Task;

pub const BACKUPS: &str = "backup";
const JOB_TIMEOUT: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupKind {
    Full,
    Incremental,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupDisk {
    pub target: String,
    pub path: String,
    pub size: u64,
    pub blake3: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupDoc {
    pub id: Uuid,
    pub vm: Uuid,
    pub vm_name: String,
    pub created: i64,
    pub kind: BackupKind,
    /// The backup this one is incremental to.
    pub parent: Option<Uuid>,
    /// libvirt checkpoint marking this backup's point in time.
    pub checkpoint: Option<String>,
    pub pool: String,
    pub disks: Vec<BackupDisk>,
    /// The VM as it was, for restore.
    pub spec: Box<VmSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBackup {
    /// Archive pool to store it in.
    pub pool: String,
    /// Force a full backup even when an incremental is possible.
    #[serde(default)]
    pub full: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreBackup {
    /// Name of the new VM.
    pub name: String,
    /// Local pool for its disks.
    pub pool: String,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Node {
    pub fn backups(&self, vm: Option<Uuid>) -> Result<Vec<BackupDoc>> {
        let mut all: Vec<BackupDoc> = self.state.db().list(BACKUPS)?;
        all.retain(|b| vm.is_none_or(|v| b.vm == v));
        all.sort_by_key(|b| b.created);
        Ok(all)
    }

    pub fn backup(&self, id: Uuid) -> Result<BackupDoc> {
        self.state
            .db()
            .get(BACKUPS, &id.to_string())?
            .ok_or_else(|| NodeError::NotFound(format!("backup {id}")))
    }

    pub fn create_backup(self: &Arc<Self>, id: Uuid, req: CreateBackup) -> Result<Task> {
        let spec = self.local_spec(id)?;
        if !self
            .state
            .archive_pools()?
            .iter()
            .any(|p| p.name == req.pool)
        {
            return Err(NodeError::NotFound(format!("archive pool {}", req.pool)));
        }
        let resource = format!("/vms/{id}");
        if self.tasks.busy(&resource) {
            return Err(NodeError::Conflict(
                "another task is running on this VM".into(),
            ));
        }
        let node = Arc::clone(self);
        Ok(self
            .tasks
            .spawn("vm.backup", resource, move |ctx| async move {
                let backup = node.run_backup(&spec, &req, &ctx).await?;
                Ok(serde_json::json!({ "backup": backup.id, "kind": backup.kind }))
            }))
    }

    #[allow(clippy::too_many_lines)]
    async fn run_backup(
        &self,
        spec: &VmSpec,
        req: &CreateBackup,
        ctx: &crate::tasks::TaskCtx,
    ) -> anyhow::Result<BackupDoc> {
        let c = self.client()?;
        let dom = domain_ref(spec);
        let backup_id = Uuid::now_v7();
        let running = self.runtime(spec.id).state.is_active();
        let disks: Vec<_> = spec
            .disks
            .iter()
            .filter(|d| d.device == DiskDevice::Disk && d.volume.is_some())
            .collect();
        let staging_pool = self.storage.get(
            &disks
                .first()
                .ok_or_else(|| anyhow::anyhow!("the VM has no disks"))?
                .pool,
        )?;
        let staged = |target: &str| format!(".prism-backup-{}-{target}.qcow2", backup_id.simple());

        // The newest earlier backup whose checkpoint libvirt still has.
        let parent = if req.full || !running {
            None
        } else {
            let existing = c.checkpoints(&dom).await.unwrap_or_default();
            self.backups(Some(spec.id))?.into_iter().rev().find(|b| {
                b.checkpoint
                    .as_ref()
                    .is_some_and(|cp| existing.iter().any(|e| &e.name == cp))
            })
        };
        let checkpoint = running.then(|| format!("prism-{}", backup_id.simple()));

        if running {
            ctx.progress(
                None,
                if parent.is_some() {
                    "incremental backup"
                } else {
                    "full backup"
                },
            );
            let mut bxml = String::from("<domainbackup mode='push'>\n");
            if let Some(p) = &parent {
                let _ = writeln!(
                    bxml,
                    "  <incremental>{}</incremental>",
                    escape(p.checkpoint.as_deref().unwrap_or_default())
                );
            }
            bxml.push_str("  <disks>\n");
            let mut cxml = format!(
                "<domaincheckpoint>\n  <name>{}</name>\n  <disks>\n",
                escape(checkpoint.as_deref().unwrap_or_default())
            );
            for d in &spec.disks {
                if disks.iter().any(|x| x.id == d.id) {
                    let file = staging_pool.path.join(staged(&d.target));
                    let _ = write!(
                        bxml,
                        "    <disk name='{}' backup='yes' type='file'>\n      <driver type='qcow2'/>\n      <target file='{}'/>\n    </disk>\n",
                        escape(&d.target),
                        escape(file.to_string_lossy().as_ref())
                    );
                    let _ = writeln!(
                        cxml,
                        "    <disk name='{}' checkpoint='bitmap'/>",
                        escape(&d.target)
                    );
                } else {
                    let _ = writeln!(bxml, "    <disk name='{}' backup='no'/>", escape(&d.target));
                    let _ = writeln!(
                        cxml,
                        "    <disk name='{}' checkpoint='no'/>",
                        escape(&d.target)
                    );
                }
            }
            bxml.push_str("  </disks>\n</domainbackup>\n");
            cxml.push_str("  </disks>\n</domaincheckpoint>\n");

            let mut events = self.subscribe_domain_events();
            c.backup_begin(&dom, &bxml, Some(&cxml), 0).await?;
            let uuid = dom.uuid;
            let poll = {
                let (c, dom, ctx) = (c.clone(), dom.clone(), ctx.clone());
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        if let Ok((_, stats)) = c.job_stats(&dom, 0).await {
                            use prism_libvirt::Params;
                            let total = stats.u64("data_total").unwrap_or(0);
                            let done = stats.u64("data_processed").unwrap_or(0);
                            if total > 0 {
                                ctx.progress(
                                    Some(0.8 * done as f64 / total as f64),
                                    "copying changed blocks",
                                );
                            }
                        }
                    }
                })
            };
            let done = tokio::select! {
                r = self.wait_event(&mut events, JOB_TIMEOUT, |ev| match ev {
                    DomainEvent::JobCompleted { domain, .. } if domain.uuid == uuid => Some(()),
                    _ => None,
                }) => r,
                () = ctx.cancel.cancelled() => {
                    let _ = c.abort_job(&dom).await;
                    Err(NodeError::Invalid("cancelled".into()))
                }
            };
            poll.abort();
            if let Err(e) = done {
                for d in &disks {
                    let _ = staging_pool.delete_volume(&staged(&d.target));
                }
                if let Some(cp) = &checkpoint
                    && let Ok(cp) = c.checkpoint_lookup(&dom, cp).await
                {
                    let _ = c.checkpoint_delete(&cp, 0).await;
                }
                return Err(e.into());
            }
        } else {
            // Stopped: flatten each disk chain into the staging file.
            for d in &disks {
                ctx.progress(None, format!("copying {}", d.target));
                let active = d.volume.clone().unwrap_or_default();
                staging_pool
                    .copy_volume_flatten(&self.cfg.libvirt.qemu_img, &active, &staged(&d.target))
                    .await?;
            }
        }

        // Stream the staged images into the archive pool.
        let mut out = Vec::new();
        for (i, d) in disks.iter().enumerate() {
            ctx.progress(
                Some(0.8 + 0.2 * i as f64 / disks.len() as f64),
                format!("storing {} in {}", d.target, req.pool),
            );
            let name = staged(&d.target);
            let path = format!(
                "backups/{}/{}/{}.qcow2",
                spec.name,
                backup_id.simple(),
                d.target
            );
            let file = staging_pool.open_read(&name)?;
            let stored = self.archive_store_file(&req.pool, &path, file).await;
            let _ = staging_pool.delete_volume(&name);
            let stored = stored?;
            out.push(BackupDisk {
                target: d.target.clone(),
                path,
                size: stored.size,
                blake3: stored.blake3,
            });
        }

        // The parent's checkpoint is no longer needed: libvirt merges its
        // bitmap into the new one.
        if let Some(p) = &parent
            && let Some(cp) = &p.checkpoint
            && let Ok(cp) = c.checkpoint_lookup(&dom, cp).await
        {
            let _ = c.checkpoint_delete(&cp, 0).await;
        }
        // The spec (including cloud-init user-data) is kept for restores.
        let saved = spec.clone();
        let doc = BackupDoc {
            id: backup_id,
            vm: spec.id,
            vm_name: spec.name.clone(),
            created: unix_now(),
            kind: if parent.is_some() {
                BackupKind::Incremental
            } else {
                BackupKind::Full
            },
            parent: parent.map(|p| p.id),
            checkpoint,
            pool: req.pool.clone(),
            disks: out,
            spec: Box::new(saved),
        };
        self.state
            .apply(Command::PutBackup {
                backup: Box::new(doc.clone()),
            })
            .await?;
        tracing::info!(vm = %spec.name, backup = %doc.id, kind = ?doc.kind, "backup complete");
        Ok(doc)
    }

    /// Stores a local file in an archive pool: on a member here, or
    /// streamed to the chosen member's node.
    pub(crate) async fn archive_store_file(
        &self,
        pool: &str,
        path: &str,
        file: std::fs::File,
    ) -> anyhow::Result<ArchiveFileDoc> {
        let size = file.metadata()?.len();
        let members = self.state.archive_members()?;
        let caps: Vec<(Uuid, crate::archive::MemberCapacity)> = self
            .nodes()
            .into_iter()
            .flat_map(|n| {
                let id = n.id;
                n.archive.into_iter().map(move |c| (id, c))
            })
            .collect();
        let cands: Vec<(&ArchiveMemberDoc, u64)> = members
            .iter()
            .filter(|m| m.pool == pool)
            .filter_map(|m| {
                caps.iter()
                    .find(|(n, c)| *n == m.node && c.name == m.name)
                    .map(|(_, c)| (m, c.free))
            })
            .collect();
        let chosen = place(&cands, size, Uuid::new_v4().as_u128()).ok_or_else(|| {
            anyhow::anyhow!("no member of archive pool {pool} has room for {size} bytes")
        })?;
        let body = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file));
        if chosen.node == self.id {
            let member = self
                .archive
                .member(&chosen.name)
                .ok_or_else(|| anyhow::anyhow!("member {} is not open here", chosen.name))?;
            let (size, blake3) = member.write(path, body).await?;
            let doc = ArchiveFileDoc {
                pool: pool.into(),
                path: path.into(),
                size,
                blake3,
                kind: FileKind::Backup,
                created: unix_now(),
                replicas: vec![Replica {
                    node: self.id,
                    member: chosen.name.clone(),
                }],
            };
            if let Err(e) = self
                .state
                .apply(Command::PutArchiveFile {
                    file: doc.clone(),
                    create: true,
                })
                .await
            {
                let _ = member.delete(path);
                anyhow::bail!("{e}");
            }
            return Ok(doc);
        }
        let cluster = self
            .cluster
            .get()
            .ok_or_else(|| anyhow::anyhow!("not clustered"))?;
        let m = self
            .state
            .members()?
            .into_iter()
            .find(|m| m.node_id == chosen.node)
            .ok_or_else(|| anyhow::anyhow!("member node left the cluster"))?;
        let req = axum::http::Request::builder()
            .method("PUT")
            .uri(format!(
                "/api/v1/archive/pools/{pool}/files/{path}?member={}",
                chosen.name
            ))
            .header("content-type", "application/octet-stream")
            .header("content-length", size)
            .body(axum::body::Body::from_stream(body))?;
        let res = crate::cluster::peers::forward_http(cluster, &m, req).await?;
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await?;
        anyhow::ensure!(
            status.is_success(),
            "{}: {}",
            m.name,
            String::from_utf8_lossy(&bytes)
        );
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Fetches an archive file into a hidden partial file in `pool`,
    /// verifying its BLAKE3. Returns the partial name.
    async fn archive_fetch(
        &self,
        file: &ArchiveFileDoc,
        pool: &crate::storage::Pool,
    ) -> anyhow::Result<String> {
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;
        let (partial, out) = pool.begin_partial(&format!("restore-{}", Uuid::new_v4().simple()))?;
        let mut out = tokio::fs::File::from_std(out);
        let mut hasher = blake3::Hasher::new();
        let local = file
            .replicas
            .iter()
            .find(|r| r.node == self.id)
            .and_then(|r| self.archive.member(&r.member));
        let res: anyhow::Result<()> = async {
            if let Some(m) = local {
                let mut s = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(
                    m.open_read(&file.path)?,
                ));
                while let Some(chunk) = s.next().await {
                    let chunk = chunk?;
                    hasher.update(&chunk);
                    out.write_all(&chunk).await?;
                }
            } else {
                let cluster = self
                    .cluster
                    .get()
                    .ok_or_else(|| anyhow::anyhow!("not clustered"))?;
                let holder = file
                    .replicas
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("no replicas"))?;
                let m = self
                    .state
                    .members()?
                    .into_iter()
                    .find(|m| m.node_id == holder.node)
                    .ok_or_else(|| anyhow::anyhow!("replica holder left the cluster"))?;
                let req = axum::http::Request::builder()
                    .uri(format!(
                        "/api/v1/archive/pools/{}/files/{}?local=1",
                        file.pool, file.path
                    ))
                    .body(axum::body::Body::empty())?;
                let res = crate::cluster::peers::forward_http(cluster, &m, req).await?;
                anyhow::ensure!(
                    res.status().is_success(),
                    "{} returned {}",
                    m.name,
                    res.status()
                );
                let mut s = res.into_body().into_data_stream();
                while let Some(chunk) = s.next().await {
                    let chunk = chunk?;
                    hasher.update(&chunk);
                    out.write_all(&chunk).await?;
                }
            }
            out.sync_all().await?;
            anyhow::ensure!(
                hasher.finalize().to_hex().as_str() == file.blake3,
                "{} failed its BLAKE3 check",
                file.path
            );
            Ok(())
        }
        .await;
        if let Err(e) = res {
            pool.abort_partial(&partial);
            return Err(e);
        }
        Ok(partial)
    }

    /// Restores a backup (and the chain it depends on) as a new VM.
    pub fn restore_backup(self: &Arc<Self>, id: Uuid, req: RestoreBackup) -> Result<Task> {
        let backup = self.backup(id)?;
        if self.state.vm_by_name(&req.name)?.is_some() {
            return Err(NodeError::Conflict(format!(
                "a VM named {} already exists",
                req.name
            )));
        }
        let pool = self.storage.get(&req.pool)?;
        let node = Arc::clone(self);
        Ok(self.tasks.spawn(
            "backup.restore",
            format!("/backups/{id}"),
            move |ctx| async move {
                // Full backup first, then each incremental up to this one.
                let mut chain = vec![backup.clone()];
                while let Some(p) = chain.last().and_then(|b| b.parent) {
                    chain.push(node.backup(p)?);
                }
                chain.reverse();
                let mut spec = (*backup.spec).clone();
                spec.id = Uuid::new_v4();
                spec.name.clone_from(&req.name);
                spec.node = node.id;
                spec.owner_epoch = 1;
                spec.generation = 1;
                spec.migrating = None;
                for nic in &mut spec.nics {
                    nic.mac = random_mac();
                }
                for d in &mut spec.disks {
                    d.pool.clone_from(&req.pool);
                    d.backing.clear();
                    if d.device == DiskDevice::Cdrom {
                        d.volume = None;
                    }
                }
                let targets: Vec<String> = backup.disks.iter().map(|d| d.target.clone()).collect();
                for (n, target) in targets.iter().enumerate() {
                    ctx.progress(
                        Some(n as f64 / targets.len() as f64),
                        format!("restoring {target}"),
                    );
                    // Fetch every layer, link each incremental onto the previous,
                    // then flatten into the new disk.
                    let mut layers = Vec::new();
                    for b in &chain {
                        let Some(bd) = b.disks.iter().find(|d| &d.target == target) else {
                            continue;
                        };
                        let file =
                            node.state.archive_file(&b.pool, &bd.path)?.ok_or_else(|| {
                                anyhow::anyhow!(
                                    "{} is missing from archive pool {}",
                                    bd.path,
                                    b.pool
                                )
                            })?;
                        layers.push(node.archive_fetch(&file, &pool).await?);
                    }
                    let volume = format!("{}-{target}.{}", req.name, pool.format);
                    let res = pool
                        .flatten_layers(&node.cfg.libvirt.qemu_img, &layers, &volume, &pool.format)
                        .await;
                    for l in &layers {
                        pool.abort_partial(l);
                    }
                    res?;
                    if let Some(d) = spec.disks.iter_mut().find(|d| &d.target == target) {
                        d.volume = Some(volume);
                        d.format.clone_from(&pool.format);
                    }
                }
                let c = node.client()?;
                node.define(&c, &spec).await?;
                node.put_spec(&spec, None).await?;
                node.refresh(&spec).await;
                Ok(serde_json::json!({ "vm": spec.id }))
            },
        ))
    }

    pub async fn delete_backup(&self, id: Uuid) -> Result<()> {
        let backup = self.backup(id)?;
        if self
            .backups(Some(backup.vm))?
            .iter()
            .any(|b| b.parent == Some(id))
        {
            return Err(NodeError::Conflict(
                "later incremental backups depend on this one; delete them first".into(),
            ));
        }
        for d in &backup.disks {
            let _ = self
                .state
                .apply(Command::DeleteArchiveFile {
                    pool: backup.pool.clone(),
                    path: d.path.clone(),
                })
                .await;
        }
        self.state.apply(Command::DeleteBackup { id }).await?;
        Ok(())
    }
}
