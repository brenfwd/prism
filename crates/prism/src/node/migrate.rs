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
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use prism_libvirt::migrate::{flags as mf, params as mp};
use prism_libvirt::{Params, TypedParam};
use serde::{Deserialize, Serialize};
use tokio::net::UnixListener;
use uuid::Uuid;

use super::{Node, NodeError, Result, VmState, domain_ref};
use crate::cluster::dataplane::{Purpose, migration_dir};
use crate::cluster::model::{MemberDoc, MemberState};
use crate::spec::{DiskDevice, Migration, VmSpec};
use crate::tasks::Task;

/// Maximum pause at switch-over.
const MAX_DOWNTIME_MS: u64 = 300;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrepareResp {
    pub cookie: String,
    pub params: Vec<(String, String)>,
}

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
                let migration = ctx_id(&ctx);
                let res = node
                    .run_migration(&cluster, &spec, &dst, &req, migration, &ctx)
                    .await;
                // Whatever happened, drop the intent and the socket directory.
                let _ = std::fs::remove_dir_all(migration_dir(migration));
                if let Err(e) = &res {
                    tracing::error!(vm = %spec.name, "migration failed: {e:#}");
                    if let Ok(mut cur) = node.spec(spec.id)
                        && cur.migrating.is_some()
                        && cur.node == node.id
                    {
                        let base = cur.generation;
                        cur.migrating = None;
                        cur.generation += 1;
                        let _ = node.put_spec(&cur, Some(base)).await;
                    }
                }
                res
            }))
    }

    #[allow(clippy::too_many_lines)]
    async fn run_migration(
        &self,
        cluster: &crate::cluster::Cluster,
        spec: &VmSpec,
        dst: &MemberDoc,
        req: &MigrateVm,
        migration: Uuid,
        ctx: &crate::tasks::TaskCtx,
    ) -> anyhow::Result<serde_json::Value> {
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

        // 2. Read-only backing layers go over the data plane first.
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
        for (mpool, mem) in &memory {
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

        // 3. Begin on the source.
        // libvirt refuses to migrate with UNDEFINE_SOURCE while the domain
        // has snapshots; the source is undefined here after the hand-over.
        let undefine_here = !snapshot_xml.is_empty();
        let flags = mf::LIVE
            | mf::PERSIST_DEST
            | if undefine_here {
                0
            } else {
                mf::UNDEFINE_SOURCE
            }
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
        let (cookie, xml) = match c.migrate_begin3(&dom, &begin_params, flags).await {
            Ok(r) => r,
            Err(e) => {
                super::moves::cleanup_remote(self, cluster, dst, &req.pool, &copied).await;
                return Err(e.into());
            }
        };

        // 4–5. Begin opened a migration job on the source; if setup fails
        // before Perform, Confirm with `cancelled` closes it, or every later
        // attempt fails with "another migration job is already running".
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

        // 6. Perform, with progress from job stats.
        let _ = c.migrate_set_max_downtime(&dom, MAX_DOWNTIME_MS).await;
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
            r = c.migrate_perform3(&dom, &params, &perform_cookie, flags) => r,
            () = ctx.cancel.cancelled() => {
                let _ = c.abort_job(&dom).await;
                Err(prism_libvirt::Error::Decode("cancelled".into()))
            }
        };
        progress.abort();
        let (perform_ok, finish_cookie) = match &perform {
            Ok(cookie) => (true, cookie.clone()),
            Err(e) => {
                tracing::warn!(vm = %spec.name, "perform failed: {e}");
                (false, Vec::new())
            }
        };

        // 7. Finish on the destination; 8. Confirm on the source.
        let fin: FinishResp = self
            .call_peer(
                cluster,
                dst,
                &format!("/api/v1/internal/migrations/{migration}/finish"),
                &FinishReq {
                    params: prep.params.clone(),
                    cookie: B64.encode(&finish_cookie),
                    flags,
                    cancelled: !perform_ok,
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
        let succeeded = perform_ok && fin.ok;
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
                    .err()
                    .map(|e| e.to_string())
                    .or(fin.error)
                    .unwrap_or_else(|| "unknown error".into())
            );
        }

        if undefine_here && let Err(e) = c.undefine(&dom, super::libvirt::undefine_flags()).await {
            tracing::warn!(vm = %spec.name, "undefining the migrated domain: {e}");
        }

        // 9. Ownership moves to the destination.
        let cur = self.spec(spec.id)?;
        let mut moved = on_destination(&cur, dst.node_id, &req.pool);
        moved.owner_epoch += 1;
        moved.generation += 1;
        moved.migrating = None;
        self.put_spec(&moved, Some(cur.generation)).await?;
        for disk in spec.disks.iter().filter(|d| d.device == DiskDevice::Disk) {
            if let Ok(pool) = self.storage.get(&disk.pool) {
                for f in disk.backing.iter().chain(disk.volume.iter()) {
                    let _ = pool.delete_volume(f);
                }
            }
        }
        for (mpool, mem) in &memory {
            if let Ok(pool) = self.storage.get(mpool) {
                let _ = pool.delete_volume(mem);
            }
        }
        self.remove_seed_iso(spec);
        super::lock(&self.runtime).remove(&spec.id);
        tracing::info!(vm = %spec.name, to = %dst.name, "live migration complete");
        Ok(serde_json::json!({ "node": dst.node_id }))
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

    /// Destination: create images, the seed ISO and the socket directory,
    /// then `Prepare3Params`.
    pub async fn migration_prepare(&self, migration: Uuid, req: PrepareReq) -> Result<PrepareResp> {
        let c = self.client()?;
        let pool = self.storage.get(&req.pool)?;
        let mut created = Vec::new();
        for img in &req.images {
            match pool
                .create_image(
                    &self.cfg.libvirt.qemu_img,
                    &img.name,
                    &img.format,
                    img.size,
                    img.backing.as_deref(),
                )
                .await
            {
                Ok(_) => created.push(img.name.clone()),
                Err(e) => {
                    for n in &created {
                        let _ = pool.delete_volume(n);
                    }
                    return Err(e.into());
                }
            }
        }
        let seed = self.seed_iso(&req.vm)?;
        let dir = migration_dir(migration);
        // Parents must be traversable by QEMU's user.
        crate::storage::create_traversable(&dir).map_err(anyhow::Error::from)?;
        // QEMU (its own user) creates the listening sockets here.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))
            .map_err(anyhow::Error::from)?;

        // Point every source pool path at the destination pool, and the
        // seed ISO at the one just written.
        let mut xml = req.begin_xml.clone();
        let dst_path = pool.path.to_string_lossy().into_owned();
        for src in req.src_pools.values() {
            xml = xml.replace(&format!("'{src}/"), &format!("'{dst_path}/"));
            xml = xml.replace(&format!("\"{src}/"), &format!("\"{dst_path}/"));
        }
        // The seed ISO keeps its name, so the rewrite above already points
        // at the copy written here.
        drop(seed);
        let mem = format!("unix://{}", dir.join("mem.sock").display());
        let nbd = format!("unix://{}", dir.join("nbd.sock").display());
        // Finish looks the incoming domain up by DEST_NAME; libvirt's own
        // client always sets it, so Prism must too.
        let pairs = vec![
            (mp::DEST_NAME.to_string(), req.vm.name.clone()),
            (mp::DEST_XML.to_string(), xml.clone()),
            (mp::PERSIST_XML.to_string(), xml),
            (mp::URI.to_string(), mem),
            (mp::DISKS_URI.to_string(), nbd),
        ];
        let cookie = B64.decode(&req.cookie).map_err(anyhow::Error::from)?;
        let mut params = to_params(&pairs);
        params.extend(
            req.vm
                .disks
                .iter()
                .filter(|d| d.device == DiskDevice::Disk)
                .map(|d| TypedParam::string(mp::MIGRATE_DISKS, d.target.clone())),
        );
        let (cookie_out, _uri) = match c.migrate_prepare3(&params, &cookie, req.flags).await {
            Ok(r) => r,
            Err(e) => {
                for n in &created {
                    let _ = pool.delete_volume(n);
                }
                return Err(e.into());
            }
        };
        Ok(PrepareResp {
            cookie: B64.encode(cookie_out),
            params: pairs,
        })
    }

    /// Destination: `Finish3Params`; cleans up on cancel.
    pub async fn migration_finish(&self, migration: Uuid, req: FinishReq) -> Result<FinishResp> {
        let c = self.client()?;
        let cookie = B64.decode(&req.cookie).map_err(anyhow::Error::from)?;
        let res = c
            .migrate_finish3(&to_params(&req.params), &cookie, req.flags, req.cancelled)
            .await;
        let _ = std::fs::remove_dir_all(migration_dir(migration));
        match res {
            Ok((_, cookie_out)) if !req.cancelled => {
                if let Some(vm) = &req.vm {
                    self.import_migrated_snapshots(vm.id, &req.pool, vm, &req.snapshot_xml)
                        .await;
                }
                Ok(FinishResp {
                    cookie: B64.encode(cookie_out),
                    ok: true,
                    error: None,
                })
            }
            other => {
                if let Ok(pool) = self.storage.get(&req.pool) {
                    for n in &req.images {
                        let _ = pool.delete_volume(n);
                    }
                }
                Ok(FinishResp {
                    cookie: String::new(),
                    ok: false,
                    error: other.err().map(|e| e.to_string()),
                })
            }
        }
    }
}

/// A per-task migration ID (the task's own ID).
fn ctx_id(ctx: &crate::tasks::TaskCtx) -> Uuid {
    ctx.task_id()
}

/// Accepts QEMU's connections on a source socket and splices each one to
/// the destination over a data-plane session.
pub(super) async fn splice_loop(
    listener: UnixListener,
    channels: prism_cluster::Channels,
    dst: MemberDoc,
    migration: Uuid,
    which: &'static str,
) {
    while let Ok((mut unix, _)) = listener.accept().await {
        let (channels, dst) = (channels.clone(), dst.clone());
        tokio::spawn(async move {
            let res: anyhow::Result<()> = async {
                let ch = channels.get(&crate::cluster::peers::grpc_addr(&dst))?;
                let purpose = serde_json::to_vec(&Purpose::MigrationSocket {
                    migration,
                    which: which.into(),
                })?;
                let reply = prism_proto::v1::data_plane_client::DataPlaneClient::new(ch)
                    .open(prism_proto::v1::OpenSession { purpose })
                    .await?
                    .into_inner();
                let mut tcp =
                    crate::cluster::dataplane::connect(dst.mesh_addr, &reply.session_id).await?;
                tokio::io::copy_bidirectional(&mut unix, &mut tcp).await?;
                Ok(())
            }
            .await;
            if let Err(e) = res {
                tracing::warn!("migration {which} splice: {e:#}");
            }
        });
    }
}
