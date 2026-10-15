//! Cold move: copy a stopped VM's disks to another node and hand it over.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;
use uuid::Uuid;

use super::{Node, NodeError, Result, VmState, domain_ref, not_running, undefine_flags};
use crate::cluster::dataplane::send_volume;
use crate::cluster::model::MemberState;
use crate::spec::DiskDevice;
use crate::tasks::Task;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveVm {
    /// Destination node ID.
    pub node: Uuid,
    /// Destination pool on that node.
    pub pool: String,
}

impl Node {
    pub fn move_vm(self: &Arc<Self>, id: Uuid, req: MoveVm) -> Result<Task> {
        let spec = self.local_spec(id)?;
        let cluster = self
            .cluster
            .get()
            .cloned()
            .ok_or_else(|| NodeError::Invalid("moving VMs needs a cluster".into()))?;
        if req.node == self.id {
            return Err(NodeError::Invalid("the VM is already on that node".into()));
        }
        let dst = self
            .state
            .members()?
            .into_iter()
            .find(|m| m.node_id == req.node && m.state == MemberState::Active)
            .ok_or_else(|| NodeError::NotFound(format!("active member {}", req.node)))?;
        if self.runtime(id).state.is_active() {
            return Err(NodeError::Conflict(format!(
                "{} is running; shut it down first (live migration moves running VMs)",
                spec.name
            )));
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
            .spawn("vm.move", resource, move |ctx| async move {
                let _guard = node.ops.lock().await;
                let c = node.client()?;
                let snapshot_xml = node.export_snapshots(&spec).await?;
                // Every file of every disk chain, base first.
                let files: Vec<(String, String)> = spec
                    .disks
                    .iter()
                    .filter(|d| d.device == DiskDevice::Disk)
                    .flat_map(|d| {
                        d.backing
                            .iter()
                            .chain(d.volume.iter())
                            .map(|v| (d.pool.clone(), v.clone()))
                            .collect::<Vec<_>>()
                    })
                    .collect();
                let total: u64 = files
                    .iter()
                    .filter_map(|(p, v)| node.storage.get(p).ok()?.volume(v).ok())
                    .map(|v| v.file_bytes)
                    .sum::<u64>()
                    .max(1);
                let done = Arc::new(AtomicU64::new(0));
                let mut sent: Vec<String> = Vec::new();
                for (pool, volume) in &files {
                    ctx.progress(
                        Some(done.load(Ordering::Relaxed) as f64 / total as f64),
                        format!("copying {volume} to {}", dst.name),
                    );
                    let file = node.storage.get(pool)?.open_read(volume)?;
                    let base = done.load(Ordering::Relaxed);
                    let (d2, ctx2) = (Arc::clone(&done), ctx.clone());
                    let progress: Arc<dyn Fn(u64, u64) + Send + Sync> = Arc::new(move |off, _| {
                        d2.store(base + off, Ordering::Relaxed);
                        ctx2.progress(Some((base + off) as f64 / total as f64), String::new());
                    });
                    let res = tokio::select! {
                        r = send_volume(&cluster, &dst, file, &req.pool, volume, progress) => r,
                        () = ctx.cancel.cancelled() => Err(anyhow::anyhow!("cancelled")),
                    };
                    if let Err(e) = res {
                        cleanup_remote(&node, &cluster, &dst, &req.pool, &sent).await;
                        return Err(e.context(format!("copying {volume}")));
                    }
                    sent.push(volume.clone());
                }

                // Hand over: the destination owns the VM from this commit on.
                let mut moved = spec.clone();
                moved.node = dst.node_id;
                moved.owner_epoch += 1;
                moved.generation += 1;
                moved.snapshot_xml = snapshot_xml;
                for d in &mut moved.disks {
                    d.pool.clone_from(&req.pool);
                    if d.device == DiskDevice::Cdrom {
                        // ISOs stay behind; the destination starts with an empty tray.
                        d.volume = None;
                    }
                }
                if let Err(e) = node.put_spec(&moved, Some(spec.generation)).await {
                    cleanup_remote(&node, &cluster, &dst, &req.pool, &sent).await;
                    return Err(anyhow::anyhow!("committing the move: {e}"));
                }
                ctx.progress(Some(1.0), "cleaning up the source");
                let dom = domain_ref(&spec);
                if let Err(e) = c.destroy(&dom, 0).await
                    && !not_running(&e)
                {
                    tracing::warn!(vm = %spec.name, "stopping moved VM: {e}");
                }
                let _ = c.undefine(&dom, undefine_flags()).await;
                node.remove_seed_iso(&spec);
                for (pool, volume) in &files {
                    if let Err(e) = node.storage.get(pool).and_then(|p| p.delete_volume(volume)) {
                        tracing::warn!(vm = %spec.name, "removing {pool}/{volume}: {e}");
                    }
                }
                super::lock(&node.runtime).remove(&spec.id);
                tracing::info!(vm = %spec.name, to = %dst.name, "moved VM");
                Ok(serde_json::json!({ "node": dst.node_id, "pool": req.pool }))
            }))
    }

    /// Defines a VM this node has just been given (after a move).
    pub(crate) async fn adopt(&self, id: Uuid) {
        let Ok(c) = self.client() else { return };
        let Ok(spec) = self.spec(id) else { return };
        if spec.node != self.id || self.runtime(id).state != VmState::Unknown {
            return;
        }
        let _guard = self.ops.lock().await;
        match self.define(&c, &spec).await {
            Ok(_) => {
                tracing::info!(vm = %spec.name, "took over VM");
                self.adopt_snapshots(&spec).await;
            }
            Err(e) => tracing::error!(vm = %spec.name, "defining VM moved here: {e}"),
        }
        self.refresh(&spec).await;
    }
}

/// Best-effort removal of volumes already copied to the destination.
async fn cleanup_remote(
    node: &Node,
    cluster: &crate::cluster::Cluster,
    dst: &crate::cluster::model::MemberDoc,
    pool: &str,
    volumes: &[String],
) {
    for v in volumes {
        let req = axum::http::Request::builder()
            .method("DELETE")
            .uri(format!(
                "/api/v1/nodes/{}/pools/{pool}/volumes/{v}",
                dst.node_id
            ))
            .body(axum::body::Body::empty());
        if let Ok(req) = req
            && let Err(e) = crate::cluster::peers::forward_http(cluster, dst, req).await
        {
            tracing::warn!(vm = ?node.id, "removing copied {v} on {}: {e:#}", dst.name);
        }
    }
}
