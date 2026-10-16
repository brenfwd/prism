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
use prism_libvirt::migrate::{flags as mf, params as mp};
use prism_libvirt::{Params, TypedParam};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::migrate_splice::remove_migration_dir;
use super::{Node, NodeError, Result, VmState, domain_ref};
use crate::cluster::model::{MemberDoc, MemberState};
use crate::spec::{DiskDevice, Migration, VmSpec};
use crate::tasks::Task;

/// Maximum pause at switch-over.
const MAX_DOWNTIME_MS: u64 = 300;

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
fn on_destination(spec: &VmSpec, dst: Uuid, pool: &str) -> VmSpec {
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
                let detached = std::sync::Mutex::new(None);
                let res = node
                    .run_migration(&cluster, &spec, &dst, &req, &ctx, &detached)
                    .await;
                // The source domain lost its snapshot metadata for the
                // migration; a failed attempt gives it back.
                let stash = super::lock(&detached).take();
                if let (Err(_), Some(xmls)) = (&res, stash) {
                    node.restore_snapshots(&spec, &xmls).await;
                }
                // Whatever happened, drop the intent and the socket directory.
                remove_migration_dir(migration);
                if let Err(e) = &res {
                    tracing::error!(vm = %spec.name, "migration failed: {e:#}");
                    if let Ok(mut cur) = node.spec(spec.id)
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
        detached: &std::sync::Mutex<Option<Vec<String>>>,
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
            self.detach_snapshots(spec).await?;
            *super::lock(detached) = Some(snapshot_xml.clone());
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
            .await?;
        let fin: FinishResp = self
            .call_peer(
                cluster,
                dst,
                &format!("/api/v1/internal/migrations/{migration}/finish"),
                &FinishReq {
                    params: prep.params.clone(),
                    cookie: B64.encode(&perform.finish_cookie),
                    flags,
                    cancelled: perform.error.is_some(),
                    pool: req.pool.clone(),
                    images: image_names,
                    snapshot_xml,
                    vm: Some(Box::new(on_destination(spec, dst.node_id, &req.pool))),
                },
            )
            .await?;
        for s in splicers {
            s.abort();
        }
        let succeeded = perform.error.is_none() && fin.ok;
        let confirm_cookie = B64.decode(&fin.cookie).unwrap_or_default();
        if let Err(e) = c
            .migrate_confirm3(&dom, &params, &confirm_cookie, flags, !succeeded)
            .await
        {
            // The destination may already run the guest: never resume the
            // source here; the intent stays until resolved.
            if succeeded {
                tracing::warn!(vm = %spec.name, "confirm on the source failed: {e}");
            } else {
                anyhow::bail!("migration failed and the source could not resume: {e}");
            }
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
        self.hand_over(spec, dst, &req.pool, &memory).await?;
        tracing::info!(vm = %spec.name, to = %dst.name, "live migration complete");
        Ok(serde_json::json!({ "node": dst.node_id }))
    }

    /// Sends the read-only backing layers and snapshot memory images to the
    /// destination pool. Returns the image requests for each disk's top layer
    /// and the names already copied (for cleanup on failure).
    async fn copy_static_files(
        &self,
        cluster: &crate::cluster::Cluster,
        spec: &VmSpec,
        dst: &MemberDoc,
        req: &MigrateVm,
        memory: &[(String, String)],
        ctx: &crate::tasks::TaskCtx,
    ) -> anyhow::Result<(Vec<ImageReq>, Vec<String>)> {
        let mut images = Vec::new();
        let mut copied = Vec::new();
        for disk in spec.disks.iter().filter(|d| d.device == DiskDevice::Disk) {
            let pool = self.storage.get(&disk.pool)?;
            for b in &disk.backing {
                ctx.progress(Some(0.0), format!("copying {b} to {}", dst.name));
                crate::cluster::dataplane::send_volume(
                    cluster,
                    dst,
                    pool.open_read(b)?,
                    &req.pool,
                    b,
                    Arc::new(|_, _| {}),
                )
                .await?;
                copied.push(b.clone());
            }
            let top = disk.volume.clone().unwrap_or_default();
            images.push(ImageReq {
                format: pool
                    .image_format(&top)
                    .unwrap_or_else(|| disk.format.clone()),
                size: pool.volume(&top)?.size_bytes,
                backing: disk.backing.last().cloned(),
                name: top,
            });
        }
        // Saved-memory images of snapshots travel with the chain.
        for (mpool, mem) in memory {
            ctx.progress(Some(0.0), format!("copying {mem} to {}", dst.name));
            crate::cluster::dataplane::send_volume(
                cluster,
                dst,
                self.storage.get(mpool)?.open_read(mem)?,
                &req.pool,
                mem,
                Arc::new(|_, _| {}),
            )
            .await?;
            copied.push(mem.clone());
        }
        Ok((images, copied))
    }

    /// Runs `Perform3Params` with progress from job stats. A failed or
    /// cancelled transfer is reported in the result rather than as an
    /// error, because Finish and Confirm must still run to clean up.
    async fn perform_transfer(
        &self,
        c: &prism_libvirt::Client,
        dom: &prism_libvirt::Domain,
        params: &[TypedParam],
        prep: &PrepareResp,
        flags: u32,
        ctx: &crate::tasks::TaskCtx,
    ) -> anyhow::Result<Transfer> {
        if let Err(e) = c.migrate_set_max_downtime(dom, MAX_DOWNTIME_MS).await {
            tracing::warn!("could not set the maximum migration downtime: {e}");
        }
        let perform_cookie = B64.decode(&prep.cookie)?;
        let progress = {
            let (c, dom, ctx) = (c.clone(), dom.clone(), ctx.clone());
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let Ok((_, stats)) = c.job_stats(&dom, 0).await else {
                        continue;
                    };
                    let total = stats.u64("data_total").unwrap_or(0);
                    let done = stats.u64("data_processed").unwrap_or(0);
                    let iter = stats.u64("memory_iteration").unwrap_or(0);
                    if total > 0 {
                        ctx.progress(
                            Some(done as f64 / total as f64),
                            format!("migrating (pass {iter})"),
                        );
                    }
                }
            })
        };
        let perform = tokio::select! {
            r = c.migrate_perform3(dom, params, &perform_cookie, flags) => r,
            () = ctx.cancel.cancelled() => {
                if let Err(e) = c.abort_job(dom).await {
                    tracing::warn!("could not abort the migration job: {e}");
                }
                Err(prism_libvirt::Error::Decode("cancelled".into()))
            }
        };
        progress.abort();
        Ok(match perform {
            Ok(finish_cookie) => Transfer {
                finish_cookie,
                error: None,
            },
            Err(e) => {
                tracing::warn!(vm = %dom.name, "perform failed: {e}");
                Transfer {
                    finish_cookie: Vec::new(),
                    error: Some(e.to_string()),
                }
            }
        })
    }

    /// Records the destination as owner (epoch + 1) and removes the source's
    /// disks, saved-memory images and runtime state.
    async fn hand_over(
        &self,
        spec: &VmSpec,
        dst: &MemberDoc,
        pool: &str,
        memory: &[(String, String)],
    ) -> anyhow::Result<()> {
        let cur = self.spec(spec.id)?;
        let mut moved = on_destination(&cur, dst.node_id, pool);
        moved.owner_epoch += 1;
        moved.generation += 1;
        moved.migrating = None;
        self.put_spec(&moved, Some(cur.generation)).await?;
        for disk in spec.disks.iter().filter(|d| d.device == DiskDevice::Disk) {
            if let Ok(pool) = self.storage.get(&disk.pool) {
                for f in disk.backing.iter().chain(disk.volume.iter()) {
                    pool.delete_volume_best_effort(f);
                }
            }
        }
        for (mpool, mem) in memory {
            if let Ok(pool) = self.storage.get(mpool) {
                pool.delete_volume_best_effort(mem);
            }
        }
        self.remove_seed_iso(spec);
        super::lock(&self.runtime).remove(&spec.id);
        Ok(())
    }

    pub(super) async fn call_peer<T: Serialize, R: serde::de::DeserializeOwned>(
        &self,
        cluster: &crate::cluster::Cluster,
        member: &MemberDoc,
        path: &str,
        body: &T,
    ) -> anyhow::Result<R> {
        let req = axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(serde_json::to_vec(body)?))?;
        let res = crate::cluster::peers::forward_http(cluster, member, req).await?;
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 16 << 20).await?;
        if !status.is_success() {
            let detail = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|v| v["detail"].as_str().map(str::to_string))
                .unwrap_or_else(|| status.to_string());
            anyhow::bail!("{}: {detail}", member.name);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
}

/// The outcome of the RAM and disk transfer.
struct Transfer {
    /// Cookie from `Perform3Params`, passed to Finish; empty on failure.
    finish_cookie: Vec<u8>,
    /// Why the transfer failed or was cancelled, if it did.
    error: Option<String>,
}
