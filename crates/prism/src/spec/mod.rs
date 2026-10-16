//! `VmSpec`: the desired state of a VM, as stored in the cluster database
//! and rendered to libvirt XML.
//!
//! The submodules hold the pieces: `disk` (disks and guest device names),
//! `nic` (NICs, networks and port forwards) and `request` (the API bodies
//! for creating and updating a VM). Everything public is re-exported here.

mod disk;
mod nic;
mod request;

pub use disk::{DiskDevice, DiskSpec, disk_target};
pub use nic::{
    NicNetwork, NicSpec, PortForward, Proto, random_mac, valid_ifname, validate_forwards,
};
pub use request::{CreateVm, UpdateVm};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::storage::valid_name;

/// The `owner_epoch` of a VM that has never moved between nodes.
pub const FIRST_OWNER_EPOCH: u64 = 1;

fn first_owner_epoch() -> u64 {
    FIRST_OWNER_EPOCH
}

/// Documents written before clustering carry epoch 0 or none at all. Both
/// load as [`FIRST_OWNER_EPOCH`], and the next write of the spec stores it.
/// Older nodes accept the stored 1 because they only compare epochs for
/// equality with their own.
fn owner_epoch_from_doc<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Ok(u64::deserialize(d)?.max(FIRST_OWNER_EPOCH))
}

/// The desired state of one VM. Stored in the cluster database; the domain XML is rendered from it.
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
    /// Owning node; nil in documents written before clustering existed.
    #[serde(default)]
    pub node: Uuid,
    /// Starts at [`FIRST_OWNER_EPOCH`] and grows by one whenever ownership
    /// moves; a node only runs VMs it owns.
    #[serde(
        default = "first_owner_epoch",
        deserialize_with = "owner_epoch_from_doc"
    )]
    pub owner_epoch: u64,
    /// Start the VM when its node starts (managed by Prism, not libvirt).
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub cloud_init: Option<CloudInit>,
    /// A live migration in progress: the VM may run on either node until
    /// it completes, and neither node quarantines it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrating: Option<Migration>,
    /// Snapshot metadata in transit during a move; redefined (and cleared)
    /// by the destination node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshot_xml: Vec<String>,
}

/// The two nodes of a live migration in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Migration {
    pub src: Uuid,
    pub dst: Uuid,
}

/// CPU count and model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuSpec {
    pub vcpus: u32,
    #[serde(default)]
    pub model: CpuModel,
}

/// How the guest CPU is derived from the host's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CpuModel {
    /// The host CPU model with its features; migratable between similar hosts.
    #[default]
    HostModel,
    /// Exactly the host CPU; fastest, but only migratable to identical CPUs.
    HostPassthrough,
}

/// cloud-init `NoCloud` data, delivered on a generated seed ISO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudInit {
    pub user_data: String,
    #[serde(default)]
    pub network_config: Option<String>,
}

/// Memory size and balloon settings, in MiB.
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

/// How a VM's memory balloon is driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalloonMode {
    Off,
    Manual,
    Auto,
}

/// Boot firmware: legacy BIOS or UEFI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Firmware {
    Bios,
    Uefi,
}

/// Most virtual CPUs a VM may have.
pub const MAX_VCPUS: u32 = 512;
/// Least memory a VM may have, in MiB.
pub const MIN_MEMORY_MIB: u64 = 64;

/// Whether `name` is a valid VM name: a valid volume name of at most 64 bytes.
pub fn valid_vm_name(name: &str) -> bool {
    valid_name(name) && name.len() <= 64
}

impl MemorySpec {
    /// Checks that max is at least `MIN_MEMORY_MIB`, min does not exceed max, and the
    /// target lies between them.
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

impl VmSpec {
    /// Assigns guest device names to disks that lack one: the lowest free
    /// name per bus prefix, so existing names never change.
    pub fn assign_targets(&mut self) {
        let mut used: Vec<String> = self
            .disks
            .iter()
            .filter(|d| !d.target.is_empty())
            .map(|d| d.target.clone())
            .collect();
        for disk in self.disks.iter_mut().filter(|d| d.target.is_empty()) {
            let prefix = disk.device.target_prefix();
            let name = (0..26 * 27)
                .map(|i| disk_target(prefix, i))
                .find(|n| !used.contains(n))
                .unwrap_or_default();
            used.push(name.clone());
            disk.target = name;
        }
    }

    /// Sets boot order from a list of disk and NIC IDs.
    pub fn set_boot_order(&mut self, order: &[Uuid]) -> Result<(), String> {
        for id in order {
            if !self.disks.iter().any(|d| d.id == *id) && !self.nics.iter().any(|n| n.id == *id) {
                return Err(format!("no disk or NIC {id}"));
            }
        }
        let pos = |id: Uuid| order.iter().position(|o| *o == id).map(|p| p as u32 + 1);
        for d in &mut self.disks {
            d.boot_order = pos(d.id);
        }
        for n in &mut self.nics {
            n.boot_order = pos(n.id);
        }
        Ok(())
    }

    /// The disk with this ID, if the VM has it.
    pub fn disk(&self, id: Uuid) -> Option<&DiskSpec> {
        self.disks.iter().find(|d| d.id == id)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn sample_spec(name: &str) -> VmSpec {
        VmSpec {
            id: Uuid::new_v4(),
            name: name.into(),
            description: String::new(),
            generation: 1,
            cpu: CpuSpec {
                vcpus: 1,
                model: CpuModel::HostModel,
            },
            memory: MemorySpec {
                max_mib: 512,
                min_mib: 256,
                target_mib: 512,
                balloon: BalloonMode::Manual,
            },
            firmware: Firmware::Bios,
            machine: "q35".into(),
            disks: vec![],
            nics: vec![],
            node: Uuid::nil(),
            owner_epoch: FIRST_OWNER_EPOCH,
            autostart: false,
            cloud_init: None,
            migrating: None,
            snapshot_xml: Vec::new(),
        }
    }

    fn disk(device: DiskDevice, target: &str) -> DiskSpec {
        DiskSpec {
            id: Uuid::new_v4(),
            pool: "p".into(),
            volume: Some("v".into()),
            device,
            format: "qcow2".into(),
            boot_order: None,
            target: target.into(),
            backing: vec![],
        }
    }

    #[test]
    fn targets_are_stable() {
        let mut s = sample_spec("a");
        s.disks = vec![
            disk(DiskDevice::Disk, "vdb"),
            disk(DiskDevice::Disk, ""),
            disk(DiskDevice::Cdrom, ""),
        ];
        s.assign_targets();
        assert_eq!(s.disks[0].target, "vdb");
        assert_eq!(s.disks[1].target, "vda");
        assert_eq!(s.disks[2].target, "sda");
        assert_eq!(disk_target("vd", 26), "vdaa");
    }

    #[test]
    fn boot_order() {
        let mut s = sample_spec("a");
        s.disks = vec![
            disk(DiskDevice::Disk, "vda"),
            disk(DiskDevice::Cdrom, "sda"),
        ];
        let (d, c) = (s.disks[0].id, s.disks[1].id);
        s.set_boot_order(&[c, d]).unwrap();
        assert_eq!(s.disks[1].boot_order, Some(1));
        assert_eq!(s.disks[0].boot_order, Some(2));
        s.set_boot_order(&[d]).unwrap();
        assert_eq!(s.disks[1].boot_order, None);
        assert!(s.set_boot_order(&[Uuid::new_v4()]).is_err());
    }

    #[test]
    fn update_distinguishes_null() {
        let u: UpdateVm = serde_json::from_str(r#"{"cloud_init": null}"#).unwrap();
        assert_eq!(u.cloud_init, Some(None));
        let u: UpdateVm = serde_json::from_str("{}").unwrap();
        assert_eq!(u.cloud_init, None);
    }

    #[test]
    fn old_documents_still_parse() {
        let old = r#"{"id":"8f0c2a51-4b1e-4c3e-9a0d-2f5b6c7d8e9f","name":"x","generation":1,
            "cpu":{"vcpus":1},"memory":{"max_mib":512,"min_mib":128,"target_mib":512,"balloon":"manual"},
            "firmware":"bios","machine":"pc-q35-10.2",
            "disks":[{"id":"11111111-2222-3333-4444-555555555555","pool":"default","volume":"x.qcow2",
                      "device":"disk","format":"qcow2","boot_order":1}],
            "nics":[{"id":"abcdef01-2222-3333-4444-555555555555","mac":"52:54:00:aa:bb:cc","network":{"type":"user"}}]}"#;
        let mut v: VmSpec = serde_json::from_str(old).unwrap();
        v.assign_targets();
        assert_eq!(v.disks[0].volume.as_deref(), Some("x.qcow2"));
        assert_eq!(v.disks[0].target, "vda");
        assert!(v.node.is_nil());
        assert_eq!(v.owner_epoch, FIRST_OWNER_EPOCH);
    }

    #[test]
    fn epoch_zero_loads_as_first_epoch() {
        let mut v = serde_json::to_value(sample_spec("e")).unwrap();
        v["owner_epoch"] = 0.into();
        let back: VmSpec = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(back.owner_epoch, FIRST_OWNER_EPOCH);
        v["owner_epoch"] = 7.into();
        let back: VmSpec = serde_json::from_value(v).unwrap();
        assert_eq!(back.owner_epoch, 7);
    }

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
