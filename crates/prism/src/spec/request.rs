//! Request bodies for creating and updating VMs.

use serde::Deserialize;
use uuid::Uuid;

use super::nic::valid_ifname;
use super::{BalloonMode, CloudInit, CpuModel, Firmware, MAX_VCPUS, NicNetwork, valid_vm_name};

/// `POST /api/v1/vms` body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateVm {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub vcpus: u32,
    pub memory_mib: u64,
    #[serde(default)]
    pub min_memory_mib: Option<u64>,
    #[serde(default = "default_balloon")]
    pub balloon: BalloonMode,
    #[serde(default = "default_firmware")]
    pub firmware: Firmware,
    pub pool: String,
    /// Size of a new boot disk; omit for no disk.
    #[serde(default)]
    pub disk_gib: Option<u64>,
    /// Existing volume in `pool` attached as a CD-ROM.
    #[serde(default)]
    pub iso: Option<String>,
    /// Omit to join the default network (or user networking without one).
    #[serde(default)]
    pub network: Option<NicNetwork>,
    /// Copy this volume (in `pool`) as the boot disk, for example a cloud
    /// image; grown to `disk_gib` when given.
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub cloud_init: Option<CloudInit>,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub cpu_model: CpuModel,
}

fn default_balloon() -> BalloonMode {
    BalloonMode::Manual
}

fn default_firmware() -> Firmware {
    Firmware::Bios
}

/// `PATCH /api/v1/vms/{id}` body.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateVm {
    pub description: Option<String>,
    pub vcpus: Option<u32>,
    pub cpu_model: Option<CpuModel>,
    pub memory: Option<UpdateMemory>,
    pub autostart: Option<bool>,
    /// `Some(None)` removes cloud-init.
    #[allow(clippy::option_option)] // absent vs. null
    #[serde(default, deserialize_with = "double_option")]
    pub cloud_init: Option<Option<CloudInit>>,
    /// Disk and NIC IDs in boot order; devices not listed don't boot.
    pub boot_order: Option<Vec<Uuid>>,
    /// Optimistic concurrency: fail unless the VM is at this generation.
    pub generation: Option<u64>,
}

/// Distinguishes an absent field (`None`) from `null` (`Some(None)`).
#[allow(clippy::option_option)]
fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// Memory fields of an update request, in MiB. Absent fields keep their value.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateMemory {
    pub max_mib: Option<u64>,
    pub min_mib: Option<u64>,
    pub target_mib: Option<u64>,
    pub balloon: Option<BalloonMode>,
}

impl CreateVm {
    /// Checks the name, vCPU count, bridge name, disk size, and that `image` and `iso` are not both
    /// set.
    pub fn validate(&self) -> Result<(), String> {
        if !valid_vm_name(&self.name) {
            return Err("name must match [A-Za-z0-9._-]{1,64} without a leading '.' or '-'".into());
        }
        if self.vcpus == 0 || self.vcpus > MAX_VCPUS {
            return Err(format!("vcpus must be 1..={MAX_VCPUS}"));
        }
        if let Some(NicNetwork::Bridge { bridge }) = &self.network
            && !valid_ifname(bridge)
        {
            return Err("invalid bridge name".into());
        }
        if self.disk_gib == Some(0) {
            return Err("disk_gib must be positive".into());
        }
        if self.image.is_some() && self.iso.is_some() {
            return Err("give either image or iso, not both".into());
        }
        Ok(())
    }
}
