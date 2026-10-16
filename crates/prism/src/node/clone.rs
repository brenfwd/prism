//! Clone a stopped VM: copy its disks (snapshot history flattened) into a
//! new VM with fresh identity, MACs and addresses.

use std::sync::Arc;

use serde::Deserialize;
use uuid::Uuid;

use super::{Node, NodeError, Result};
use crate::spec::{DiskDevice, NicNetwork, random_mac};
use crate::storage::valid_name;
use crate::tasks::Task;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneVm {
    /// Name of the copy.
    pub name: String,
}

impl Node {
    pub fn clone_vm(self: &Arc<Self>, id: Uuid, req: CloneVm) -> Result<Task> {
        let src = self.local_spec(id)?;
        if !valid_name(&req.name) || req.name.len() > 64 {
            return Err(NodeError::Invalid(
                "VM names match [A-Za-z0-9._-]{1,64}".into(),
            ));
        }
        if self.state.vm_by_name(&req.name)?.is_some() {
            return Err(NodeError::Conflict(format!(
                "a VM named {} already exists",
                req.name
            )));
        }
        if self.runtime(id).state.is_active() {
            return Err(NodeError::Conflict(format!(
                "{} is running; shut it down to clone it",
                src.name
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
            .spawn("vm.clone", resource, move |ctx| async move {
                let mut spec = src.clone();
                spec.id = Uuid::new_v4();
                spec.name.clone_from(&req.name);
                spec.node = node.id;
                spec.owner_epoch = crate::spec::FIRST_OWNER_EPOCH;
                spec.generation = 1;
                spec.migrating = None;
                spec.snapshot_xml.clear();
                for nic in &mut spec.nics {
                    nic.id = Uuid::new_v4();
                    nic.mac = random_mac();
                    nic.network.clear_lease();
                    if let NicNetwork::User { forwards } = &mut nic.network {
                        // Host ports belong to the original.
                        forwards.clear();
                    }
                }
                let todo: Vec<usize> = (0..spec.disks.len())
                    .filter(|&i| spec.disks[i].device == DiskDevice::Disk)
                    .filter(|&i| spec.disks[i].volume.is_some())
                    .collect();
                let mut made: Vec<(String, String)> = Vec::new();
                let res: anyhow::Result<()> = async {
                    for (n, &i) in todo.iter().enumerate() {
                        let d = &mut spec.disks[i];
                        d.id = Uuid::new_v4();
                        d.backing.clear();
                        ctx.progress(
                            Some(n as f64 / todo.len() as f64),
                            format!("copying {}", d.target),
                        );
                        let pool = node.storage.get(&d.pool)?;
                        let from = d.volume.clone().unwrap_or_default();
                        let to = format!("{}-{}.qcow2", req.name, d.target);
                        pool.copy_volume_flatten(&node.cfg.libvirt.qemu_img, &from, &to)
                            .await?;
                        made.push((d.pool.clone(), to.clone()));
                        d.volume = Some(to);
                        d.format = "qcow2".into();
                    }
                    ctx.progress(Some(0.95), "defining the copy");
                    let _guard = node.ops.lock().await;
                    let c = node.client()?;
                    node.define(&c, &spec).await?;
                    node.put_spec(&spec, None).await?;
                    Ok(())
                }
                .await;
                if let Err(e) = res {
                    for (pool, volume) in made {
                        if let Ok(p) = node.storage.get(&pool) {
                            p.delete_volume_best_effort(&volume);
                        }
                    }
                    return Err(e);
                }
                node.refresh(&spec).await;
                Ok(serde_json::json!({ "vm": spec.id }))
            }))
    }
}
