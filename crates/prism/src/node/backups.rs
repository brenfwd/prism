//! Backups into archive pools, and restores from them.
//!
//! Running VMs use libvirt push-mode backups with checkpoints, so after the
//! first full backup each one copies only changed blocks; the guest never
//! pauses. Stopped VMs are copied by flattening their disk chains. Each
//! disk image is staged in the VM's pool, streamed into an archive pool on
//! any node, and recorded with the VM's spec for restore.

use std::sync::Arc;
use std::time::Duration;

use prism_libvirt::DomainEvent;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::backup_xml::{BackupDisk as XmlDisk, backup_xml, checkpoint_xml};
use super::{Node, NodeError, Result, domain_ref};
use crate::spec::{DiskDevice, VmSpec, random_mac};
use crate::state::Command;
use crate::tasks::Task;

/// Database collection holding one [`BackupDoc`] per backup, keyed by backup ID.
pub const BACKUPS: &str = "backup";
const JOB_TIMEOUT: Duration = Duration::from_secs(24 * 3600);

/// Whether a backup holds whole disks or only blocks changed since its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupKind {
    Full,
    Incremental,
}

/// One disk image of a backup, stored as a file in the backup's archive pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupDisk {
    pub target: String,
    pub path: String,
    pub size: u64,
    pub blake3: String,
}

/// A stored backup: which VM, when, where the disk images are and the VM spec for a restore.
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

/// Request body for starting a backup.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBackup {
    /// Archive pool to store it in.
    pub pool: String,
    /// Force a full backup even when an incremental is possible.
    #[serde(default)]
    pub full: bool,
}

/// Request body for restoring a backup as a new VM.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreBackup {
    /// Name of the new VM.
    pub name: String,
    /// Local pool for its disks.
    pub pool: String,
}

pub(super) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Node {
    /// Lists backups, oldest first, for one VM or (with `None`) for every VM.
    pub fn backups(&self, vm: Option<Uuid>) -> Result<Vec<BackupDoc>> {
        let mut all: Vec<BackupDoc> = self.state.db().list(BACKUPS)?;
        all.retain(|b| vm.is_none_or(|v| b.vm == v));
        all.sort_by_key(|b| b.created);
        Ok(all)
    }

    /// Returns one backup. Fails with `NotFound` if `id` is unknown.
    pub fn backup(&self, id: Uuid) -> Result<BackupDoc> {
        self.state
            .db()
            .get(BACKUPS, &id.to_string())?
            .ok_or_else(|| NodeError::NotFound(format!("backup {id}")))
    }

    /// Starts a `vm.backup` task for a VM this node owns. A running VM gets a push-mode backup
    /// (incremental when an earlier checkpoint still exists); a stopped VM is flattened and copied.
    /// Fails with `NotFound` for an unknown archive pool and `Conflict` while another task runs on
    /// the VM.
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
        let staged = |target: &str| format!("prism-backup-{}-{target}.qcow2", backup_id.simple());

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

        if let Some(cp) = &checkpoint {
            self.push_backup(
                &c,
                spec,
                &disks,
                &staging_pool,
                &staged,
                parent.as_ref(),
                cp,
                ctx,
            )
            .await?;
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

        let out = self
            .store_staged_disks(
                spec,
                &disks,
                &staging_pool,
                &staged,
                backup_id,
                &req.pool,
                ctx,
            )
            .await?;

        // The parent's checkpoint is no longer needed: libvirt merges its
        // bitmap into the new one.
        if let Some(p) = &parent
            && let Some(cp) = &p.checkpoint
        {
            delete_checkpoint(&c, &dom, cp).await;
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

    /// Runs a libvirt push-mode backup of a running VM into staging files,
    /// waiting for the job to finish. On failure or cancellation the staged
    /// files and the new checkpoint are removed.
    #[allow(clippy::too_many_arguments)]
    async fn push_backup(
        &self,
        c: &prism_libvirt::Client,
        spec: &VmSpec,
        disks: &[&crate::spec::DiskSpec],
        staging_pool: &crate::storage::Pool,
        staged: &impl Fn(&str) -> String,
        parent: Option<&BackupDoc>,
        checkpoint: &str,
        ctx: &crate::tasks::TaskCtx,
    ) -> anyhow::Result<()> {
        let dom = domain_ref(spec);
        ctx.progress(
            None,
            if parent.is_some() {
                "incremental backup"
            } else {
                "full backup"
            },
        );
        let files: Vec<_> = spec
            .disks
            .iter()
            .map(|d| {
                disks
                    .iter()
                    .any(|x| x.id == d.id)
                    .then(|| staging_pool.path.join(staged(&d.target)))
            })
            .collect();
        let xml_disks: Vec<_> = spec
            .disks
            .iter()
            .zip(&files)
            .map(|(d, f)| XmlDisk {
                target: &d.target,
                staging: f.as_deref(),
            })
            .collect();
        let bxml = backup_xml(parent.and_then(|p| p.checkpoint.as_deref()), &xml_disks);
        let cxml = checkpoint_xml(checkpoint, &xml_disks);

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
                if let Err(e) = c.abort_job(&dom).await {
                    tracing::warn!(vm = %spec.name, "could not abort backup job: {e}");
                }
                Err(NodeError::Invalid("cancelled".into()))
            }
        };
        poll.abort();
        if let Err(e) = done {
            for d in disks {
                staging_pool.delete_volume_best_effort(&staged(&d.target));
            }
            delete_checkpoint(c, &dom, checkpoint).await;
            return Err(e.into());
        }
        Ok(())
    }

    /// Streams each staged image into the archive pool `pool` and deletes
    /// the staging file, whether or not the store succeeded.
    #[allow(clippy::too_many_arguments)]
    async fn store_staged_disks(
        &self,
        spec: &VmSpec,
        disks: &[&crate::spec::DiskSpec],
        staging_pool: &crate::storage::Pool,
        staged: &impl Fn(&str) -> String,
        backup_id: Uuid,
        pool: &str,
        ctx: &crate::tasks::TaskCtx,
    ) -> anyhow::Result<Vec<BackupDisk>> {
        let mut out = Vec::new();
        for (i, d) in disks.iter().enumerate() {
            ctx.progress(
                Some(0.8 + 0.2 * i as f64 / disks.len() as f64),
                format!("storing {} in {pool}", d.target),
            );
            let name = staged(&d.target);
            let path = format!(
                "backups/{}/{}/{}.qcow2",
                spec.name,
                backup_id.simple(),
                d.target
            );
            let file = staging_pool.open_read(&name)?;
            let stored = self.archive_store_file(pool, &path, file).await;
            staging_pool.delete_volume_best_effort(&name);
            let stored = stored?;
            out.push(BackupDisk {
                target: d.target.clone(),
                path,
                size: stored.size,
                blake3: stored.blake3,
            });
        }
        Ok(out)
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
                spec.owner_epoch = crate::spec::FIRST_OWNER_EPOCH;
                spec.generation = 1;
                spec.migrating = None;
                for nic in &mut spec.nics {
                    nic.mac = random_mac();
                    nic.network.clear_lease();
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

    /// Deletes a backup and its archive files. Fails with `Conflict` while a later incremental
    /// backup depends on it. A file that cannot be removed is logged and the record is still
    /// deleted.
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
            if let Err(e) = self
                .state
                .apply(Command::DeleteArchiveFile {
                    pool: backup.pool.clone(),
                    path: d.path.clone(),
                })
                .await
            {
                tracing::warn!(backup = %id, path = %d.path, "could not delete backup file: {e:#}");
            }
        }
        self.state.apply(Command::DeleteBackup { id }).await?;
        Ok(())
    }
}

/// Deletes a libvirt checkpoint, logging a warning if libvirt refuses.
async fn delete_checkpoint(c: &prism_libvirt::Client, dom: &prism_libvirt::Domain, name: &str) {
    let Ok(cp) = c.checkpoint_lookup(dom, name).await else {
        return;
    };
    if let Err(e) = c.checkpoint_delete(&cp, 0).await {
        tracing::warn!(checkpoint = name, "could not delete checkpoint: {e}");
    }
}
