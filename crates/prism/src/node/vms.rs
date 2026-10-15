//! VM lifecycle: create, update, delete, power actions and consoles.

use std::path::PathBuf;

use prism_libvirt::proto::flags;
use uuid::Uuid;

use super::{
    Action, Node, NodeError, Result, VmView, domain_ref, lock, not_running, undefine_flags,
};
use crate::render;
use crate::spec::{
    BalloonMode, CpuSpec, CreateVm, DiskDevice, DiskSpec, MAX_VCPUS, MemorySpec, NicSpec, UpdateVm,
    VmSpec, random_mac,
};

impl Node {
    pub async fn create_vm(&self, req: CreateVm) -> Result<VmView> {
        req.validate().map_err(NodeError::Invalid)?;
        let c = self.client()?;
        let _guard = self.ops.lock().await;
        if self.state.vm_by_name(&req.name)?.is_some() {
            return Err(NodeError::Conflict(format!(
                "a VM named {} already exists",
                req.name
            )));
        }
        let pool = self.storage.get(&req.pool)?;
        let memory = MemorySpec {
            max_mib: req.memory_mib,
            min_mib: req
                .min_memory_mib
                .unwrap_or_else(|| (req.memory_mib / 4).max(128).min(req.memory_mib)),
            target_mib: req.memory_mib,
            balloon: req.balloon,
        };
        memory.validate().map_err(NodeError::Invalid)?;

        let mut disks = Vec::new();
        let mut created_volume = None;
        let qemu_img = &self.cfg.libvirt.qemu_img;
        if let Some(image) = &req.image {
            let volume = format!("{}-disk0.{}", req.name, pool.format);
            let size = req.disk_gib.map(|g| g << 30);
            pool.copy_volume(qemu_img, image, &volume, &pool.format, size)
                .await?;
            created_volume = Some(volume.clone());
            disks.push(new_disk(
                &pool.name,
                Some(volume),
                DiskDevice::Disk,
                &pool.format,
                Some(1),
            ));
        } else if let Some(gib) = req.disk_gib {
            let volume = format!("{}-disk0.{}", req.name, pool.format);
            pool.create_volume(qemu_img, &volume, gib << 30, &pool.format)
                .await?;
            created_volume = Some(volume.clone());
            // Boot the installer first while the disk is blank.
            let order = if req.iso.is_some() { 2 } else { 1 };
            disks.push(new_disk(
                &pool.name,
                Some(volume),
                DiskDevice::Disk,
                &pool.format,
                Some(order),
            ));
        }
        if let Some(iso) = &req.iso {
            pool.volume(iso)?;
            disks.push(new_disk(
                &pool.name,
                Some(iso.clone()),
                DiskDevice::Cdrom,
                "raw",
                Some(1),
            ));
        }
        if let Some(n) = &req.network {
            super::forwards::check_nic_forwards(n)?;
        }
        let mut spec = VmSpec {
            id: Uuid::new_v4(),
            name: req.name,
            description: req.description,
            generation: 1,
            cpu: CpuSpec {
                vcpus: req.vcpus,
                model: req.cpu_model,
            },
            memory,
            firmware: req.firmware,
            machine: "q35".into(),
            disks,
            nics: vec![NicSpec {
                id: Uuid::new_v4(),
                mac: random_mac(),
                network: self.default_nic_network(req.network),
                boot_order: None,
            }],
            node: self.id,
            owner_epoch: 1,
            autostart: req.autostart,
            cloud_init: req.cloud_init,
            migrating: None,
            snapshot_xml: Vec::new(),
        };
        spec.assign_targets();

        let res: Result<()> = async {
            let dom = self.define(&c, &spec).await?;
            // Pin the expanded machine type (never the bare alias).
            let xml = c.get_xml_desc(&dom, flags::DOMAIN_XML_INACTIVE).await?;
            if let Some(m) = render::machine_type(&xml) {
                spec.machine = m;
            }
            self.put_spec(&spec, None).await?;
            // The state machine assigned the NIC leases.
            if let Some(stored) = self.state.vm(spec.id)? {
                spec = stored;
            }
            Ok(())
        }
        .await;
        if let Err(e) = res {
            let _ = c.undefine(&domain_ref(&spec), undefine_flags()).await;
            if let Some(v) = created_volume {
                let _ = pool.delete_volume(&v);
            }
            return Err(e);
        }
        tracing::info!(vm = %spec.name, id = %spec.id, "created VM");
        self.refresh(&spec).await;
        Ok(self.vm_view(spec))
    }

    pub async fn update_vm(&self, id: Uuid, req: UpdateVm) -> Result<VmView> {
        let c = self.client()?;
        let _guard = self.ops.lock().await;
        let old = self.local_spec(id)?;
        if req.generation.is_some_and(|g| g != old.generation) {
            return Err(NodeError::Conflict(format!(
                "vm {} is at generation {}, not {}",
                old.name,
                old.generation,
                req.generation.unwrap_or_default()
            )));
        }
        let mut spec = old.clone();
        if let Some(d) = req.description {
            spec.description = d;
        }
        if let Some(v) = req.vcpus {
            if v == 0 || v > MAX_VCPUS {
                return Err(NodeError::Invalid(format!("vcpus must be 1..={MAX_VCPUS}")));
            }
            spec.cpu.vcpus = v;
        }
        if let Some(m) = req.cpu_model {
            spec.cpu.model = m;
        }
        if let Some(m) = req.memory {
            spec.memory.max_mib = m.max_mib.unwrap_or(spec.memory.max_mib);
            spec.memory.min_mib = m.min_mib.unwrap_or(spec.memory.min_mib);
            spec.memory.target_mib = m.target_mib.unwrap_or(spec.memory.target_mib);
            if m.max_mib.is_some() && m.target_mib.is_none() {
                spec.memory.target_mib = spec.memory.target_mib.min(spec.memory.max_mib);
            }
            spec.memory.balloon = m.balloon.unwrap_or(spec.memory.balloon);
        }
        spec.memory.validate().map_err(NodeError::Invalid)?;
        if let Some(a) = req.autostart {
            spec.autostart = a;
        }
        if let Some(ci) = req.cloud_init {
            spec.cloud_init = ci;
        }
        if let Some(order) = &req.boot_order {
            spec.set_boot_order(order).map_err(NodeError::Invalid)?;
        }
        if spec == old {
            return Ok(self.vm_view(spec));
        }
        spec.generation += 1;

        let dom = self.define(&c, &spec).await?;
        let running = self.runtime(id).state.is_active();
        let mut needs_restart = spec.cpu != old.cpu
            || spec.memory.max_mib != old.memory.max_mib
            || (spec.memory.balloon == BalloonMode::Off)
                != (old.memory.balloon == BalloonMode::Off)
            || spec.cloud_init != old.cloud_init
            || boot_orders(&spec) != boot_orders(&old);
        if running && spec.memory.target_mib != old.memory.target_mib {
            if spec.memory.balloon == BalloonMode::Off {
                needs_restart = true;
            } else {
                c.set_memory(
                    &dom,
                    spec.memory.target_mib * 1024,
                    flags::DOMAIN_AFFECT_LIVE,
                )
                .await?;
            }
        }
        self.put_spec(&spec, Some(old.generation)).await?;
        if running && needs_restart {
            self.update_runtime(id, |rt| rt.pending_restart = true);
        }
        self.refresh(&spec).await;
        Ok(self.vm_view(spec))
    }

    pub async fn delete_vm(&self, id: Uuid, delete_disks: bool) -> Result<()> {
        let c = self.client()?;
        let _guard = self.ops.lock().await;
        let spec = self.local_spec(id)?;
        let dom = domain_ref(&spec);
        match c.destroy(&dom, 0).await {
            Ok(()) => {}
            Err(e) if not_running(&e) => {}
            Err(e) => return Err(e.into()),
        }
        // Snapshot overlays belong to the VM; collect them before libvirt
        // forgets the chain.
        let overlays = self.snapshot_files(&spec).await.unwrap_or_default();
        match c.undefine(&dom, undefine_flags()).await {
            Ok(()) => {}
            Err(e) if e.remote_code() == Some(prism_libvirt::proto::error_codes::NO_DOMAIN) => {}
            Err(e) => return Err(e.into()),
        }
        self.state
            .apply(crate::state::Command::DeleteVm { id })
            .await?;
        lock(&self.runtime).remove(&id);
        self.remove_seed_iso(&spec);
        if delete_disks {
            let mut files: Vec<(String, String)> = spec
                .disks
                .iter()
                .filter(|d| d.device == DiskDevice::Disk)
                .filter_map(|d| d.volume.clone().map(|v| (d.pool.clone(), v)))
                .collect();
            files.extend(overlays);
            files.sort();
            files.dedup();
            for (pool, volume) in files {
                let res = self
                    .storage
                    .get(&pool)
                    .and_then(|p| p.delete_volume(&volume));
                if let Err(e) = res {
                    tracing::warn!(vm = %spec.name, "deleting {pool}/{volume}: {e}");
                }
            }
        }
        tracing::info!(vm = %spec.name, "deleted VM");
        Ok(())
    }

    pub async fn action(&self, id: Uuid, action: Action) -> Result<VmView> {
        let c = self.client()?;
        let spec = self.local_spec(id)?;
        let dom = domain_ref(&spec);
        match action {
            Action::Start => {
                if self.balloon_refuses_start(&spec) {
                    return Err(NodeError::Conflict(
                        "host memory pressure is critical; refusing to start more VMs".into(),
                    ));
                }
                // Start from the current spec, so any pending change applies.
                self.define(&c, &spec).await?;
                c.create(&dom, 0).await?;
                self.update_runtime(id, |rt| rt.pending_restart = false);
            }
            Action::Shutdown => c.shutdown(&dom).await?,
            Action::Stop => c.destroy(&dom, 0).await?,
            Action::Reboot => c.reboot(&dom, 0).await?,
            Action::Reset => c.reset(&dom).await?,
            Action::Pause => c.suspend(&dom).await?,
            Action::Resume => c.resume(&dom).await?,
        }
        self.refresh(&spec).await;
        Ok(self.vm_view(spec))
    }

    /// The VNC UNIX socket of a running VM.
    pub async fn vnc_socket(&self, id: Uuid) -> Result<PathBuf> {
        let c = self.client()?;
        let spec = self.local_spec(id)?;
        let xml = c.get_xml_desc(&domain_ref(&spec), 0).await?;
        render::vnc_socket(&xml)
            .ok_or_else(|| NodeError::Conflict("VM has no VNC socket; is it running?".into()))
    }

    pub async fn open_serial(&self, id: Uuid) -> Result<prism_libvirt::Stream> {
        let c = self.client()?;
        let spec = self.local_spec(id)?;
        Ok(
            c.open_console(&domain_ref(&spec), None, flags::DOMAIN_CONSOLE_FORCE)
                .await?,
        )
    }

    /// Builds (or rebuilds) the cloud-init seed ISO next to the VM's first
    /// disk and returns its path, or `None` without cloud-init.
    pub(crate) fn seed_iso(&self, spec: &VmSpec) -> Result<Option<PathBuf>> {
        let Some(ci) = &spec.cloud_init else {
            return Ok(None);
        };
        let pool_name = spec
            .disks
            .iter()
            .find(|d| d.device == DiskDevice::Disk)
            .or_else(|| spec.disks.first())
            .map(|d| d.pool.clone())
            .or_else(|| self.storage.list().first().map(|p| p.name.clone()))
            .ok_or_else(|| NodeError::Invalid("cloud-init needs a storage pool".into()))?;
        let pool = self.storage.get(&pool_name)?;
        let meta = format!("instance-id: {}\nlocal-hostname: {}\n", spec.id, spec.name);
        let path =
            pool.write_seed_iso(&spec.id, &ci.user_data, &meta, ci.network_config.as_deref())?;
        Ok(Some(path))
    }

    pub(crate) fn remove_seed_iso(&self, spec: &VmSpec) {
        for pool in self.storage.list() {
            pool.remove_seed_iso(&spec.id);
        }
    }
}

fn new_disk(
    pool: &str,
    volume: Option<String>,
    device: DiskDevice,
    format: &str,
    boot: Option<u32>,
) -> DiskSpec {
    DiskSpec {
        id: Uuid::new_v4(),
        pool: pool.into(),
        volume,
        device,
        format: format.into(),
        boot_order: boot,
        target: String::new(),
        backing: Vec::new(),
    }
}

fn boot_orders(spec: &VmSpec) -> Vec<(Uuid, Option<u32>)> {
    spec.disks
        .iter()
        .map(|d| (d.id, d.boot_order))
        .chain(spec.nics.iter().map(|n| (n.id, n.boot_order)))
        .collect()
}
