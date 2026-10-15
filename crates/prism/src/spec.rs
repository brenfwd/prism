//! `VmSpec`: the desired state of a VM.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::storage::valid_name;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmSpec {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub generation: u64,
    pub cpu: CpuSpec,
    pub memory: MemorySpec,
    pub firmware: Firmware,
    /// Machine type, pinned at creation when libvirt expands the alias.
    pub machine: String,
    pub disks: Vec<DiskSpec>,
    pub nics: Vec<NicSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuSpec {
    pub vcpus: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemorySpec {
    /// Boot (maximum) memory.
    pub max_mib: u64,
    /// Balloon floor.
    pub min_mib: u64,
    /// Balloon target applied at start.
    pub target_mib: u64,
    pub balloon: BalloonMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalloonMode {
    Off,
    Manual,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Firmware {
    Bios,
    Uefi,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskSpec {
    pub id: Uuid,
    pub pool: String,
    pub volume: String,
    pub device: DiskDevice,
    pub format: String,
    #[serde(default)]
    pub boot_order: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskDevice {
    Disk,
    Cdrom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicSpec {
    pub id: Uuid,
    pub mac: String,
    pub network: NicNetwork,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NicNetwork {
    /// An existing host bridge (host bridging).
    Bridge { bridge: String },
    /// QEMU user-mode networking; no host setup needed.
    User,
}

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
    #[serde(default)]
    pub network: Option<NicNetwork>,
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
    pub memory: Option<UpdateMemory>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateMemory {
    pub max_mib: Option<u64>,
    pub min_mib: Option<u64>,
    pub target_mib: Option<u64>,
    pub balloon: Option<BalloonMode>,
}

pub const MAX_VCPUS: u32 = 512;
pub const MIN_MEMORY_MIB: u64 = 64;

pub fn valid_vm_name(name: &str) -> bool {
    valid_name(name) && name.len() <= 64
}

impl MemorySpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_mib < MIN_MEMORY_MIB {
            return Err(format!("memory must be at least {MIN_MEMORY_MIB} MiB"));
        }
        if self.min_mib > self.max_mib {
            return Err("min_mib exceeds max_mib".into());
        }
        if self.target_mib < self.min_mib || self.target_mib > self.max_mib {
            return Err("target_mib must be between min_mib and max_mib".into());
        }
        Ok(())
    }
}

/// A locally administered unicast MAC in QEMU's `52:54:00` range.
pub fn random_mac() -> String {
    let b = Uuid::new_v4().into_bytes();
    format!("52:54:00:{:02x}:{:02x}:{:02x}", b[0], b[1], b[2])
}

impl CreateVm {
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
        Ok(())
    }
}

/// Linux interface names: 1–15 bytes, no `/`, whitespace or `:`.
pub fn valid_ifname(name: &str) -> bool {
    (1..=15).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_format() {
        let m = random_mac();
        assert_eq!(m.len(), 17);
        assert!(m.starts_with("52:54:00:"));
    }

    #[test]
    fn memory_bounds() {
        let mut m = MemorySpec {
            max_mib: 1024,
            min_mib: 256,
            target_mib: 512,
            balloon: BalloonMode::Manual,
        };
        assert!(m.validate().is_ok());
        m.target_mib = 2048;
        assert!(m.validate().is_err());
    }

    #[test]
    fn ifnames() {
        assert!(valid_ifname("br0"));
        assert!(!valid_ifname("this-name-is-too-long"));
        assert!(!valid_ifname("a/b"));
    }
}
