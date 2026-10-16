//! Live migration with local disks.
//!
//! Prism reimplements libvirt's managed V3 migration across the two nodes'
//! local daemons: Begin and Perform/Confirm on the source, Prepare and
//! Finish on the destination (through internal endpoints reached over the
//! mesh tunnel). QEMU migrates RAM and disks (NBD) over UNIX sockets at the
//! same path on both hosts; the source node splices each connection over
//! the data plane. Ownership: a `Migrating` intent is committed first, and
//! `owner_epoch + 1` for the destination after `Confirm3`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use prism_libvirt::TypedParam;
use prism_libvirt::migrate::{flags as mf, params as mp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::migrate_splice::remove_migration_dir;
use super::{Node, NodeError, Result, VmState, domain_ref};
use crate::cluster::model::{MemberDoc, MemberState};
use crate::spec::{DiskDevice, Migration, VmSpec};
use crate::tasks::Task;

/// What a failed migration needs to put back.
#[derive(Default)]
struct Rollback {
    /// Snapshot XML taken off the source domain for the migration.
    detached: std::sync::Mutex<Option<Vec<String>>>,
    /// Set while the destination may already run the guest, in which case
    /// the source must stay as it is.
    dest_may_run: std::sync::atomic::AtomicBool,
}

/// Tries at the Finish call to the destination.
const FINISH_TRIES: u32 = 3;

/// Request body for live-migrating a running VM to another node.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrateVm {
    pub node: Uuid,
    pub pool: String,
    /// Switch to post-copy if pre-copy does not converge (the destination
    /// then always wins).
    #[serde(default)]
    pub postcopy: bool,
}

/// An image the destination must create before Prepare.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageReq {
    pub name: String,
    pub format: String,
    pub size: u64,
    pub backing: Option<String>,
}

/// Source to destination: everything the destination needs to create images and call
/// `Prepare3Params`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareReq {
    pub vm: Box<VmSpec>,
    pub pool: String,
    pub images: Vec<ImageReq>,
    /// Source pool name → directory, to rewrite paths in the domain XML.
    pub src_pools: HashMap<String, String>,
    pub begin_xml: String,
    pub cookie: String,
    pub flags: u32,
}

/// Destination to source: the Prepare cookie and the migration parameters to use for Perform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareResp {
    pub cookie: String,
    pub params: Vec<(String, String)>,
}

/// Source to destination: finish the migration, or cancel it and delete the images.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinishReq {
    pub params: Vec<(String, String)>,
    pub cookie: String,
    pub flags: u32,
    pub cancelled: bool,
    /// Images to delete when cancelled.
    pub pool: String,
    pub images: Vec<String>,
    /// Snapshot metadata to redefine on the destination.
    #[serde(default)]
    pub snapshot_xml: Vec<String>,
    /// The VM as the destination will own it.
    #[serde(default)]
    pub vm: Option<Box<VmSpec>>,
}

/// Destination to source: the Finish cookie, and whether the guest now runs on the destination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinishResp {
    pub cookie: String,
    pub ok: bool,
    pub error: Option<String>,
}

pub(super) fn to_params(pairs: &[(String, String)]) -> Vec<TypedParam> {
    pairs
        .iter()
        .map(|(k, v)| TypedParam::string(k.clone(), v.clone()))
        .collect()
}

/// The destination's view of the VM: disks in `pool`, CD-ROMs empty.
pub(super) fn on_destination(spec: &VmSpec, dst: Uuid, pool: &str) -> VmSpec {
    let mut s = spec.clone();
    s.node = dst;
    for d in &mut s.disks {
        d.pool = pool.to_string();
    }
    s
}

impl Node {
    /// Starts a `vm.migrate` task. Fails with `Invalid` outside a cluster or when the target is
    /// this node, `NotFound` for an unknown or inactive node, and `Conflict` if the VM is not
    /// running, has a CD-ROM inserted or has another task running. A failed migration restores the
    /// source's snapshot metadata and clears the migration intent.
    pub fn migrate_vm(self: &Arc<Self>, id: Uuid, req: MigrateVm) -> Result<Task> {
        let spec = self.local_spec(id)?;
        let cluster = self
            .cluster
            .get()
            .cloned()
            .ok_or_else(|| NodeError::Invalid("live migration needs a cluster".into()))?;
        if req.node == self.id {
            return Err(NodeError::Invalid("the VM is already on that node".into()));
        }
        let dst = self
            .state
            .members()?
            .into_iter()
            .find(|m| m.node_id == req.node && m.state == MemberState::Active)
            .ok_or_else(|| NodeError::NotFound(format!("active member {}", req.node)))?;
        if self.runtime(id).state != VmState::Running {
            return Err(NodeError::Conflict(format!(
                "{} is not running; use move for stopped VMs",
                spec.name
            )));
        }
        if spec
            .disks
            .iter()
            .any(|d| d.device == DiskDevice::Cdrom && d.volume.is_some())
        {
            return Err(NodeError::Conflict("eject CD-ROMs before migrating".into()));
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
            .spawn("vm.migrate", resource, move |ctx| async move {
                let migration = ctx.task_id();
                let rollback = Rollback::default();
                let was_running = node.runtime(spec.id).state == VmState::Running;
                let res = node
                    .run_migration(&cluster, &spec, &dst, &req, &ctx, &rollback)
                    .await;
                // Every failure must leave the source running, unless the
                // destination may already have taken the guest over.
                if res.is_err()
                    && was_running
                    && !rollback.dest_may_run.load(std::sync::atomic::Ordering::SeqCst)
                {
                    node.resume_source(&spec).await;
                }
                // The source domain lost its snapshot metadata for the
                // migration; a failed attempt gives it back.
                let dest_may_run = rollback
                    .dest_may_run
                    .load(std::sync::atomic::Ordering::SeqCst);
                let stash = super::lock(&rollback.detached).take();
                if let (Err(_), Some(xmls), false) = (&res, stash, dest_may_run) {
                    node.restore_snapshots(&spec, &xmls).await;
                }
                // Whatever happened, drop the intent and the socket directory.
                remove_migration_dir(migration);
                if let Err(e) = &res {
                    tracing::error!(vm = %spec.name, "migration failed: {e:#}");
                    // While the destination may run the guest the intent
                    // stays: it exempts the VM from quarantine there until an
                    // operator resolves it.
                    if !dest_may_run
                        && let Ok(mut cur) = node.spec(spec.id)
                        && cur.migrating.is_some()
                        && cur.node == node.id
                    {
                        let base = cur.generation;
                        cur.migrating = None;
                        cur.generation += 1;
                        if let Err(e) = node.put_spec(&cur, Some(base)).await {
                            tracing::warn!(vm = %spec.name, "could not clear the migration intent: {e:#}");
                        }
                    }
                }
                res
            }))
    }

    /// Runs the migration phases in order; any phase may fail, and
    /// `migrate_vm` then rolls back the intent and the snapshot metadata.
    async fn run_migration(
        &self,
        cluster: &crate::cluster::Cluster,
        spec: &VmSpec,
        dst: &MemberDoc,
        req: &MigrateVm,
        ctx: &crate::tasks::TaskCtx,
        rollback: &Rollback,
    ) -> anyhow::Result<serde_json::Value> {
        let migration = ctx.task_id();
        let c = self.client()?;
        let dom = domain_ref(spec);
        let (snapshot_xml, memory) = self.export_snapshots(spec).await?;

        // 1. Intent: the VM may run on either node until this completes.
        let mut intent = spec.clone();
        intent.migrating = Some(Migration {
            src: self.id,
            dst: dst.node_id,
        });
        intent.generation += 1;
        self.put_spec(&intent, Some(spec.generation)).await?;

        // 2. Read-only layers and saved-memory images go over the data plane.
        let (images, copied) = self
            .copy_static_files(cluster, spec, dst, req, &memory, ctx)
            .await?;

        // 3. Begin on the source.
        let flags = mf::LIVE
            | mf::PERSIST_DEST
            | mf::UNDEFINE_SOURCE
            | mf::NON_SHARED_INC
            | mf::AUTO_CONVERGE
            | mf::ABORT_ON_ERROR
            | mf::CHANGE_PROTECTION
            | if req.postcopy { mf::POSTCOPY } else { 0 };
        let disks: Vec<String> = spec
            .disks
            .iter()
            .filter(|d| d.device == DiskDevice::Disk)
            .map(|d| d.target.clone())
            .collect();
        let begin_params: Vec<TypedParam> = disks
            .iter()
            .map(|t| TypedParam::string(mp::MIGRATE_DISKS, t.clone()))
            .collect();
        // libvirt refuses to migrate a domain that has snapshots. Their XML
        // travels in the request and is redefined on the destination.
        if !snapshot_xml.is_empty() {
            // Stash first: a failure part-way leaves some snapshots detached.
            *super::lock(&rollback.detached) = Some(snapshot_xml.clone());
            self.detach_snapshots(spec).await?;
        }
        let (cookie, xml) = match c.migrate_begin3(&dom, &begin_params, flags).await {
            Ok(r) => r,
            Err(e) => {
                super::moves::cleanup_remote(self, cluster, dst, &req.pool, &copied).await;
                return Err(e.into());
            }
        };

        // 4. Prepare the destination. Begin opened a migration job on the
        // source; if setup fails before Perform, Confirm with `cancelled`
        // closes it, or every later attempt fails with "another migration
        // job is already running".
        let setup = self
            .prepare_migration(
                cluster,
                dst,
                migration,
                &disks,
                PrepareReq {
                    vm: Box::new(on_destination(spec, dst.node_id, &req.pool)),
                    pool: req.pool.clone(),
                    images: images.clone(),
                    src_pools: HashMap::new(),
                    begin_xml: xml,
                    cookie: B64.encode(&cookie),
                    flags,
                },
            )
            .await;
        let (prep, params, splicers) = match setup {
            Ok(s) => s,
            Err(e) => {
                if let Err(ce) = c
                    .migrate_confirm3(&dom, &begin_params, &[], flags, true)
                    .await
                {
                    tracing::warn!(vm = %spec.name, "cancelling the source job failed: {ce}");
                }
                super::moves::cleanup_remote(self, cluster, dst, &req.pool, &copied).await;
                return Err(e);
            }
        };
        let image_names: Vec<String> = images
            .iter()
            .map(|i| i.name.clone())
            .chain(copied)
            .collect();

        // 5. Perform, then 6. Finish on the destination and Confirm on the
        // source.
        let perform = self
            .perform_transfer(&c, &dom, &params, &prep, flags, ctx)
            .await;
        // Until Finish and Confirm settle it, a transfer that completed may
        // have left the guest running on the destination.
        rollback
            .dest_may_run
            .store(perform.error.is_none(), std::sync::atomic::Ordering::SeqCst);
        let finish_req = FinishReq {
            params: prep.params.clone(),
            cookie: B64.encode(&perform.finish_cookie),
            flags,
            cancelled: perform.error.is_some(),
            pool: req.pool.clone(),
            images: image_names,
            snapshot_xml,
            vm: Some(Box::new(on_destination(spec, dst.node_id, &req.pool))),
        };
        let finish_path = format!("/api/v1/internal/migrations/{migration}/finish");
        let mut fin: Result<FinishResp, _> = Err(anyhow::anyhow!("not attempted"));
        // A failed transfer is cleaned up with a few tries, because the
        // destination keeps its half-built domain and images until Finish
        // runs. A finished transfer is retried too: only the destination can
        // say whether it took the guest over.
        for attempt in 0..FINISH_TRIES {
            fin = self
                .call_peer(cluster, dst, &finish_path, &finish_req)
                .await;
            if fin.is_ok() {
                break;
            }
            if attempt + 1 < FINISH_TRIES {
                tokio::time::sleep(Duration::from_secs(2 << attempt)).await;
            }
        }
        let fin = match fin {
            Ok(f) => f,
            Err(e) if perform.error.is_some() => {
                // The transfer failed, so the destination never owned the
                // guest. Close the source job and resume it below anyway.
                tracing::warn!(vm = %spec.name, "destination cleanup failed: {e:#}");
                FinishResp {
                    cookie: String::new(),
                    ok: false,
                    error: Some(format!("{e:#}")),
                }
            }
            Err(e) => {
                // The transfer finished but the destination cannot be
                // reached: the guest may run there. The source stays as it
                // is and the intent stays until an operator resolves it.
                for s in splicers {
                    s.abort();
                }
                return Err(e.context("could not confirm the migration on the destination"));
            }
        };
        for s in splicers {
            s.abort();
        }
        let succeeded = perform.error.is_none() && fin.ok;
        let confirm_cookie = B64.decode(&fin.cookie).unwrap_or_default();
        let confirmed = c
            .migrate_confirm3(&dom, &params, &confirm_cookie, flags, !succeeded)
            .await;
        // Aborting the job already ended the migration, so cancelling it again is not an error.
        let confirmed = match confirmed {
            Err(e)
                if !succeeded
                    && e.remote_code()
                        == Some(prism_libvirt::proto::error_codes::OPERATION_INVALID) =>
            {
                Ok(())
            }
            other => other,
        };
        if let Err(e) = confirmed {
            // The destination may already run the guest: never resume the
            // source here; the intent stays until resolved.
            if succeeded {
                tracing::warn!(vm = %spec.name, "confirm on the source failed: {e}");
            } else {
                // The destination never took the guest over (Finish said so
                // or the transfer failed), so resuming the source is safe.
                rollback
                    .dest_may_run
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                anyhow::bail!("migration failed and the source job could not be closed: {e}");
            }
        } else if !succeeded {
            rollback
                .dest_may_run
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        if !succeeded {
            anyhow::bail!(
                "migration failed: {}",
                perform
                    .error
                    .or(fin.error)
                    .unwrap_or_else(|| "unknown error".into())
            );
        }

        // 7. Ownership moves to the destination.
        // The guest already runs there, so a failed hand-over is retried.
        let mut handed = self.hand_over(spec, dst, &req.pool, &memory).await;
        for attempt in 0..FINISH_TRIES {
            if handed.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2 << attempt)).await;
            handed = self.hand_over(spec, dst, &req.pool, &memory).await;
        }
        handed?;
        tracing::info!(vm = %spec.name, to = %dst.name, "live migration complete");
        Ok(serde_json::json!({ "node": dst.node_id }))
    }
}
