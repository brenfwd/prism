//! `VmSpec`: the desired state of a VM.

use std::net::Ipv4Addr;

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
    /// Owning node; nil in documents written before clustering existed.
    #[serde(default)]
    pub node: Uuid,
    /// Bumped whenever ownership moves; a node only runs VMs it owns.
    #[serde(default)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Migration {
    pub src: Uuid,
    pub dst: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuSpec {
    pub vcpus: u32,
    #[serde(default)]
    pub model: CpuModel,
}

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
    /// The image file; `None` is an empty CD-ROM tray.
    pub volume: Option<String>,
    pub device: DiskDevice,
    pub format: String,
    #[serde(default)]
    pub boot_order: Option<u32>,
    /// Guest device name (`vda`, `sdb`); stable across hotplug.
    #[serde(default)]
    pub target: String,
    /// Read-only backing layers below `volume` (snapshot history), oldest
    /// first, as volume names in the same pool.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backing: Vec<String>,
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
    #[serde(default)]
    pub boot_order: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NicNetwork {
    /// An existing host bridge (host bridging).
    Bridge { bridge: String },
    /// User-mode networking (passt); no host setup needed.
    User {
        /// Host ports forwarded into the guest.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        forwards: Vec<PortForward>,
    },
    /// A Prism network (switch).
    Network {
        network: Uuid,
        /// The IPAM lease; assigned when the VM is stored if absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ipv4: Option<Ipv4Addr>,
        /// Egress node override; `None` uses the network's egress.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        egress: Option<Uuid>,
        /// Access VLAN (2 to 4094) inside the network; the NIC then shares
        /// a segment only with NICs on the same VLAN, not the gateway.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vlan: Option<u16>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Proto {
    Tcp,
    Udp,
}

impl Proto {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

/// One host port forwarded to a guest port on a user-mode NIC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortForward {
    pub proto: Proto,
    /// Host address to listen on; loopback when absent.
    #[serde(default)]
    pub host_addr: Option<std::net::IpAddr>,
    pub host_port: u16,
    pub guest_port: u16,
}

impl PortForward {
    /// The address actually bound on the host.
    pub fn listen_addr(&self) -> std::net::IpAddr {
        self.host_addr
            .unwrap_or(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST))
    }
}

/// Checks a NIC's forwards: ports in range, no duplicate listeners.
pub fn validate_forwards(forwards: &[PortForward]) -> Result<(), String> {
    for (i, f) in forwards.iter().enumerate() {
        if f.host_port == 0 || f.guest_port == 0 {
            return Err("ports must be 1..=65535".into());
        }
        if forwards[..i].iter().any(|g| {
            g.proto == f.proto && g.host_port == f.host_port && g.listen_addr() == f.listen_addr()
        }) {
            return Err(format!(
                "{}/{} is forwarded twice",
                f.proto.as_str(),
                f.host_port
            ));
        }
    }
    Ok(())
}

impl NicNetwork {
    /// A user-mode NIC without forwards.
    pub fn user() -> Self {
        Self::User {
            forwards: Vec::new(),
        }
    }

    /// The Prism network this NIC is on, if any.
    pub fn network_id(&self) -> Option<Uuid> {
        match self {
            Self::Network { network, .. } => Some(*network),
            _ => None,
        }
    }

    /// Forgets the IPAM lease, for copies of a NIC (restores, clones).
    pub fn clear_lease(&mut self) {
        if let Self::Network { ipv4, .. } = self {
            *ipv4 = None;
        }
    }
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

    pub fn disk(&self, id: Uuid) -> Option<&DiskSpec> {
        self.disks.iter().find(|d| d.id == id)
    }
}

impl DiskDevice {
    pub fn target_prefix(self) -> &'static str {
        match self {
            Self::Disk => "vd",
            Self::Cdrom => "sd",
        }
    }
}

/// `vda`..`vdz`, then `vdaa`..: the Linux naming scheme.
pub fn disk_target(prefix: &str, index: usize) -> String {
    let letters = b"abcdefghijklmnopqrstuvwxyz";
    if index < 26 {
        format!("{prefix}{}", letters[index] as char)
    } else {
        format!(
            "{prefix}{}{}",
            letters[(index / 26 - 1) % 26] as char,
            letters[index % 26] as char
        )
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
        if self.image.is_some() && self.iso.is_some() {
            return Err("give either image or iso, not both".into());
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
            owner_epoch: 0,
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
