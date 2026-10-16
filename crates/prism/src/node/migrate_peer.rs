//! Source-side steps of a live migration that talk to the destination:
//! copying static files, handing over ownership and the peer RPC helper.

use std::sync::Arc;

use serde::Serialize;

use super::Node;
use super::migrate::{ImageReq, MigrateVm, on_destination};
use crate::cluster::model::MemberDoc;
use crate::spec::{DiskDevice, VmSpec};

impl Node {
    /// Sends the read-only backing layers and snapshot memory images to the
    /// destination pool. Returns the image requests for each disk's top layer
    /// and the names already copied (for cleanup on failure).
    pub(super) async fn copy_static_files(
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

    /// Records the destination as owner (epoch + 1) and removes the source's
    /// disks, saved-memory images and runtime state.
    pub(super) async fn hand_over(
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
