//! Disks, CD-ROMs and NICs: add, remove, change media, resize. Changes are
//! written to the persistent definition and hot-applied when the VM runs.

use std::time::Duration;

use prism_libvirt::DomainEvent;
use prism_libvirt::ops::flags as op;
use serde::Deserialize;
use uuid::Uuid;

use super::{Node, NodeError, Result, VmView, domain_ref};
use crate::render;
use crate::spec::{DiskDevice, DiskSpec, NicNetwork, NicSpec, VmSpec, random_mac, valid_ifname};

/// How long to wait for the guest to release a hot-unplugged device.
const UNPLUG_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddDisk {
    pub pool: String,
    /// Attach an existing volume…
    #[serde(default)]
    pub volume: Option<String>,
    /// …or create a new one of this size.
    #[serde(default)]
    pub size_gib: Option<u64>,
    #[serde(default = "default_device")]
    pub device: DiskDevice,
}

fn default_device() -> DiskDevice {
    DiskDevice::Disk
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateDisk {
    /// CD-ROM media: a volume in the disk's pool, or `null` to eject.
    #[allow(clippy::option_option)] // absent vs. null
    #[serde(default, deserialize_with = "double_option")]
    pub media: Option<Option<String>>,
    /// Grow the disk to this size.
    pub size_gib: Option<u64>,
}

#[allow(clippy::option_option)]
fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddNic {
    pub network: NicNetwork,
    #[serde(default)]
    pub mac: Option<String>,
}

fn valid_mac(mac: &str) -> bool {
    let parts: Vec<&str> = mac.split(':').collect();
    parts.len() == 6
        && parts.iter().all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
        // Unicast only.
        && u8::from_str_radix(parts[0], 16).is_ok_and(|b| b & 1 == 0)
}

impl Node {
    fn nic_xml(&self, nic: &crate::spec::NicSpec) -> Result<String> {
        render::nic_xml(nic, &self.network_bridges(), "").map_err(NodeError::Invalid)
    }

    fn device_xml(&self, disk: &DiskSpec) -> Result<String> {
        let pool = |p: &str| self.pool_loc(p);
        render::disk_xml(
            disk,
            &self.render_ctx(&pool, None, &self.network_bridges()),
            "",
        )
        .map_err(NodeError::Invalid)
    }

    /// Persists `spec` and, when the VM runs, applies `live` to it.
    async fn change_devices<F, Fut>(
        &self,
        old: &VmSpec,
        mut spec: VmSpec,
        live: F,
    ) -> Result<VmView>
    where
        F: FnOnce(prism_libvirt::Client, prism_libvirt::Domain) -> Fut,
        Fut: Future<Output = Result<bool>>,
    {
        let c = self.client()?;
        spec.generation += 1;
        spec.assign_targets();
        self.define(&c, &spec).await?;
        self.put_spec(&spec, Some(old.generation)).await?;
        if self.runtime(spec.id).state.is_active() {
            let applied = match live(c, domain_ref(&spec)).await {
                Ok(done) => done,
                Err(e) => {
                    tracing::warn!(vm = %spec.name, "live device change failed: {e}");
                    false
                }
            };
            if !applied {
                self.update_runtime(spec.id, |rt| rt.pending_restart = true);
            }
        }
        self.refresh(&spec).await;
        Ok(self.vm_view(spec))
    }

    pub async fn add_disk(&self, id: Uuid, req: AddDisk) -> Result<VmView> {
        let _guard = self.ops.lock().await;
        let old = self.local_spec(id)?;
        let pool = self.storage.get(&req.pool)?;
        let (volume, created) = match (req.volume, req.size_gib, req.device) {
            (Some(v), None, _) => {
                pool.volume(&v)?;
                (Some(v), false)
            }
            (None, Some(gib), DiskDevice::Disk) if gib > 0 => {
                let n = old.disks.len();
                let name = (n..n + 1000)
                    .map(|i| format!("{}-disk{i}.{}", old.name, pool.format))
                    .find(|v| pool.volume(v).is_err())
                    .unwrap_or_default();
                pool.create_volume(&self.cfg.libvirt.qemu_img, &name, gib << 30, &pool.format)
                    .await?;
                (Some(name), true)
            }
            (None, None, DiskDevice::Cdrom) => (None, false),
            _ => {
                return Err(NodeError::Invalid(
                    "give either volume or a positive size_gib (CD-ROMs may be empty)".into(),
                ));
            }
        };
        let format = match req.device {
            DiskDevice::Cdrom => "raw".to_string(),
            DiskDevice::Disk => volume
                .as_deref()
                .and_then(|v| pool.image_format(v))
                .unwrap_or_else(|| pool.format.clone()),
        };
        let disk = DiskSpec {
            id: Uuid::new_v4(),
            pool: pool.name.clone(),
            volume: volume.clone(),
            device: req.device,
            format,
            boot_order: None,
            target: String::new(),
            backing: Vec::new(),
        };
        let mut spec = old.clone();
        spec.disks.push(disk.clone());
        spec.assign_targets();
        let disk = spec.disks.last().cloned().unwrap_or(disk);
        let xml = self.device_xml(&disk)?;
        let res = self
            .change_devices(&old, spec, |c, dom| async move {
                // SATA CD-ROMs cannot be hot-plugged.
                if disk.device == DiskDevice::Cdrom {
                    return Ok(false);
                }
                c.attach_device(&dom, &xml, op::AFFECT_LIVE).await?;
                Ok(true)
            })
            .await;
        if res.is_err()
            && created
            && let Some(v) = volume
        {
            let _ = pool.delete_volume(&v);
        }
        res
    }

    pub async fn update_disk(&self, id: Uuid, disk_id: Uuid, req: UpdateDisk) -> Result<VmView> {
        let _guard = self.ops.lock().await;
        let old = self.local_spec(id)?;
        let disk = old
            .disk(disk_id)
            .cloned()
            .ok_or_else(|| NodeError::NotFound(format!("disk {disk_id}")))?;
        let mut spec = old.clone();
        if let Some(size) = req.size_gib {
            if disk.device != DiskDevice::Disk {
                return Err(NodeError::Invalid("only disks can be resized".into()));
            }
            let volume = disk
                .volume
                .as_deref()
                .ok_or_else(|| NodeError::Invalid("disk has no volume".into()))?;
            let bytes = size << 30;
            let c = self.client()?;
            if self.runtime(id).state.is_active() {
                c.block_resize(
                    &domain_ref(&spec),
                    &disk.target,
                    bytes,
                    op::BLOCK_RESIZE_BYTES,
                )
                .await?;
            } else {
                // The active layer may be a snapshot overlay.
                let active = self.active_volume(&spec, &disk).await?;
                let pool = self.storage.get(&disk.pool)?;
                pool.resize_volume(
                    &self.cfg.libvirt.qemu_img,
                    active.as_deref().unwrap_or(volume),
                    &disk.format,
                    bytes,
                )
                .await?;
            }
            if req.media.is_none() {
                self.refresh(&spec).await;
                return Ok(self.vm_view(spec));
            }
        }
        if let Some(media) = req.media {
            if disk.device != DiskDevice::Cdrom {
                return Err(NodeError::Invalid(
                    "only CD-ROMs have removable media".into(),
                ));
            }
            if let Some(v) = &media {
                self.storage.get(&disk.pool)?.volume(v)?;
            }
            let entry = spec
                .disks
                .iter_mut()
                .find(|d| d.id == disk_id)
                .ok_or_else(|| NodeError::NotFound(format!("disk {disk_id}")))?;
            entry.volume = media;
            let xml = self.device_xml(entry)?;
            return self
                .change_devices(&old, spec, |c, dom| async move {
                    c.update_device(&dom, &xml, op::AFFECT_LIVE).await?;
                    Ok(true)
                })
                .await;
        }
        Ok(self.vm_view(spec))
    }

    pub async fn remove_disk(
        &self,
        id: Uuid,
        disk_id: Uuid,
        delete_volume: bool,
    ) -> Result<VmView> {
        let _guard = self.ops.lock().await;
        let old = self.local_spec(id)?;
        let disk = old
            .disk(disk_id)
            .cloned()
            .ok_or_else(|| NodeError::NotFound(format!("disk {disk_id}")))?;
        let mut spec = old.clone();
        spec.disks.retain(|d| d.id != disk_id);
        let xml = self.device_xml(&disk)?;
        let mut events = self.subscribe_domain_events();
        let alias = format!("ua-{disk_id}");
        let view = self
            .change_devices(&old, spec, |c, dom| async move {
                if disk.device == DiskDevice::Cdrom {
                    return Ok(false);
                }
                c.detach_device(&dom, &xml, op::AFFECT_LIVE).await?;
                let uuid = dom.uuid;
                Ok(self
                    .wait_event(&mut events, UNPLUG_TIMEOUT, |ev| match ev {
                        DomainEvent::DeviceRemoved { domain, alias: a }
                            if domain.uuid == uuid && *a == alias =>
                        {
                            Some(())
                        }
                        _ => None,
                    })
                    .await
                    .is_ok())
            })
            .await?;
        if delete_volume
            && disk.device == DiskDevice::Disk
            && let Some(v) = &disk.volume
        {
            if view.status.pending_restart {
                tracing::warn!("not deleting {v}: still attached until the VM powers off");
            } else if self.volume_users(&disk.pool, v)?.is_empty() {
                self.storage.get(&disk.pool)?.delete_volume(v)?;
            }
        }
        Ok(view)
    }

    pub async fn add_nic(&self, id: Uuid, req: AddNic) -> Result<VmView> {
        let _guard = self.ops.lock().await;
        let old = self.local_spec(id)?;
        if let NicNetwork::Bridge { bridge } = &req.network
            && !valid_ifname(bridge)
        {
            return Err(NodeError::Invalid("invalid bridge name".into()));
        }
        let mac = match req.mac {
            Some(m) if valid_mac(&m) => m.to_ascii_lowercase(),
            Some(_) => return Err(NodeError::Invalid("invalid unicast MAC address".into())),
            None => random_mac(),
        };
        if self
            .state
            .vms()?
            .iter()
            .any(|v| v.nics.iter().any(|n| n.mac == mac))
        {
            return Err(NodeError::Conflict(format!("MAC {mac} is already in use")));
        }
        let nic = NicSpec {
            id: Uuid::new_v4(),
            mac,
            network: req.network,
            boot_order: None,
        };
        let mut spec = old.clone();
        spec.nics.push(nic.clone());
        let xml = self.nic_xml(&nic)?;
        self.change_devices(&old, spec, |c, dom| async move {
            c.attach_device(&dom, &xml, op::AFFECT_LIVE).await?;
            Ok(true)
        })
        .await
    }

    pub async fn remove_nic(&self, id: Uuid, nic_id: Uuid) -> Result<VmView> {
        let _guard = self.ops.lock().await;
        let old = self.local_spec(id)?;
        let nic = old
            .nics
            .iter()
            .find(|n| n.id == nic_id)
            .cloned()
            .ok_or_else(|| NodeError::NotFound(format!("nic {nic_id}")))?;
        let mut spec = old.clone();
        spec.nics.retain(|n| n.id != nic_id);
        let xml = self.nic_xml(&nic)?;
        let mut events = self.subscribe_domain_events();
        let alias = format!("ua-{nic_id}");
        self.change_devices(&old, spec, |c, dom| async move {
            c.detach_device(&dom, &xml, op::AFFECT_LIVE).await?;
            let uuid = dom.uuid;
            Ok(self
                .wait_event(&mut events, UNPLUG_TIMEOUT, |ev| match ev {
                    DomainEvent::DeviceRemoved { domain, alias: a }
                        if domain.uuid == uuid && *a == alias =>
                    {
                        Some(())
                    }
                    _ => None,
                })
                .await
                .is_ok())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macs() {
        assert!(valid_mac("52:54:00:aa:bb:cc"));
        assert!(!valid_mac("53:54:00:aa:bb:cc"), "multicast bit set");
        assert!(!valid_mac("52:54:00:aa:bb"));
        assert!(!valid_mac("52:54:00:aa:bb:zz"));
    }

    #[test]
    fn update_disk_parses_eject() {
        let u: UpdateDisk = serde_json::from_str(r#"{"media": null}"#).unwrap();
        assert_eq!(u.media, Some(None));
    }
}
