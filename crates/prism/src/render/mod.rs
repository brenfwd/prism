//! `VmSpec` to libvirt domain XML. Rendering is pure: the same spec and
//! context always give the same text, which the golden test in this module
//! compares byte for byte against `tests/golden/basic.xml`.
//!
//! [`render`] builds the persistent domain definition. [`disk_xml`],
//! [`nic_xml`] and [`seed_xml`] produce single device elements for hot-plug.
//! The `parse` submodule reads values back out of libvirt's live XML.

mod devices;
mod parse;

use std::path::{Path, PathBuf};

use quick_xml::escape::escape;

use crate::spec::{BalloonMode, CpuModel, Firmware, VmSpec};

use devices::root_ports;
pub use devices::{disk_xml, nic_xml, seed_target, seed_xml};
pub use parse::{disk_sources, machine_type, vnc_socket};

/// Appends one line of XML to a `String`. Writing to a `String` cannot fail.
macro_rules! line {
    ($w:expr, $($arg:tt)*) => {{
        use std::fmt::Write as _;
        writeln!($w, $($arg)*).expect("writing to a String cannot fail");
    }};
}
use line;

/// Why a spec could not be rendered.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// A disk names a storage pool this node does not have.
    #[error("unknown pool {0}")]
    UnknownPool(String),
    /// A NIC is attached to a Prism network with no bridge on this node.
    #[error("unknown network {0}")]
    UnknownNetwork(uuid::Uuid),
}

/// Where a pool keeps its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolLoc {
    /// Absolute directory of the pool.
    pub path: PathBuf,
    /// The filesystem supports `O_DIRECT`, so disks use `cache='none'`.
    pub direct_io: bool,
}

/// Host-specific inputs to rendering.
#[derive(Clone)]
pub struct RenderCtx<'a> {
    /// libvirt domain type: `kvm`, or `qemu` for software emulation.
    pub domain_type: &'a str,
    /// Path of the QEMU binary; libvirt picks one when `None`.
    pub emulator: Option<&'a Path>,
    /// UEFI firmware image; with `None`, UEFI VMs use libvirt's firmware
    /// autoselection.
    pub uefi_loader: Option<&'a Path>,
    /// Resolves a pool name to its location.
    pub pool: &'a dyn Fn(&str) -> Option<PoolLoc>,
    /// A cloud-init seed ISO to attach as an extra read-only CD-ROM.
    pub seed_iso: Option<&'a Path>,
    /// The bridge of each Prism network, for NICs on one.
    pub bridges: &'a std::collections::HashMap<uuid::Uuid, String>,
}

/// XML namespace of the `<prism:vm>` metadata element.
pub const METADATA_NS: &str = "urn:prism:1";

fn esc(s: &str) -> std::borrow::Cow<'_, str> {
    escape(s)
}

/// Renders the persistent domain definition for `spec`.
pub fn render(spec: &VmSpec, ctx: &RenderCtx<'_>) -> Result<String, RenderError> {
    let mut x = String::with_capacity(4096);
    let w = &mut x;
    write_identity(w, spec, ctx);
    write_boot_and_cpu(w, spec, ctx);
    line!(w, "  <devices>");
    write_devices(w, spec, ctx)?;
    line!(w, "  </devices>");
    line!(w, "</domain>");
    Ok(x)
}

/// Name, UUID, Prism metadata, memory and vCPU count.
fn write_identity(w: &mut String, spec: &VmSpec, ctx: &RenderCtx<'_>) {
    line!(w, "<domain type='{}'>", esc(ctx.domain_type));
    line!(w, "  <name>{}</name>", esc(&spec.name));
    line!(w, "  <uuid>{}</uuid>", spec.id);
    if !spec.description.is_empty() {
        line!(w, "  <description>{}</description>", esc(&spec.description));
    }
    line!(
        w,
        "  <metadata>\n    <prism:vm xmlns:prism='{METADATA_NS}' generation='{}' epoch='{}'/>\n  </metadata>",
        spec.generation,
        spec.owner_epoch
    );
    line!(
        w,
        "  <memory unit='KiB'>{}</memory>",
        spec.memory.max_mib * 1024
    );
    let current = match spec.memory.balloon {
        BalloonMode::Off => spec.memory.max_mib,
        BalloonMode::Manual | BalloonMode::Auto => spec.memory.target_mib,
    };
    line!(
        w,
        "  <currentMemory unit='KiB'>{}</currentMemory>",
        current * 1024
    );
    line!(w, "  <vcpu placement='static'>{}</vcpu>", spec.cpu.vcpus);
}

/// Firmware, machine type, features, CPU model, clock and power events.
fn write_boot_and_cpu(w: &mut String, spec: &VmSpec, ctx: &RenderCtx<'_>) {
    match (spec.firmware, ctx.uefi_loader) {
        (Firmware::Uefi, None) => {
            line!(w, "  <os firmware='efi'>");
        }
        _ => {
            line!(w, "  <os>");
        }
    }
    line!(
        w,
        "    <type arch='x86_64' machine='{}'>hvm</type>",
        esc(&spec.machine)
    );
    if let (Firmware::Uefi, Some(loader)) = (spec.firmware, ctx.uefi_loader) {
        line!(
            w,
            "    <loader readonly='yes' type='rom'>{}</loader>",
            esc(&loader.to_string_lossy())
        );
    }
    line!(w, "  </os>");
    line!(w, "  <features>\n    <acpi/>\n    <apic/>\n  </features>");
    match (ctx.domain_type, spec.cpu.model) {
        ("kvm", CpuModel::HostModel) => {
            line!(w, "  <cpu mode='host-model'/>");
        }
        ("kvm", CpuModel::HostPassthrough) => {
            line!(w, "  <cpu mode='host-passthrough' migratable='on'/>");
        }
        _ => {
            line!(
                w,
                "  <cpu mode='custom' match='exact'>\n    <model fallback='allow'>qemu64</model>\n  </cpu>"
            );
        }
    }
    line!(w, "  <clock offset='utc'/>");
    line!(
        w,
        "  <on_poweroff>destroy</on_poweroff>\n  <on_reboot>restart</on_reboot>\n  <on_crash>destroy</on_crash>"
    );
}

/// Everything inside `<devices>`: disks, NICs, PCI root ports, console,
/// display, balloon and RNG.
fn write_devices(w: &mut String, spec: &VmSpec, ctx: &RenderCtx<'_>) -> Result<(), RenderError> {
    if let Some(emu) = ctx.emulator {
        line!(
            w,
            "    <emulator>{}</emulator>",
            esc(&emu.to_string_lossy())
        );
    }
    for disk in &spec.disks {
        w.push_str(&disk_xml(disk, ctx, "    ")?);
    }
    if let Some(seed) = ctx.seed_iso {
        w.push_str(&seed_xml(seed, &seed_target(spec), "    "));
    }
    for nic in &spec.nics {
        w.push_str(&nic_xml(nic, ctx.bridges, "    ")?);
    }
    // Declared first so libvirt numbers the spare ports from 1.
    line!(
        w,
        "    <controller type='pci' index='0' model='pcie-root'/>"
    );
    for _ in 0..root_ports(spec) {
        line!(w, "    <controller type='pci' model='pcie-root-port'/>");
    }
    line!(
        w,
        "    <serial type='pty'>\n      <target port='0'/>\n    </serial>\n    <console type='pty'>\n      <target type='serial' port='0'/>\n    </console>"
    );
    line!(
        w,
        "    <channel type='unix'>\n      <target type='virtio' name='org.qemu.guest_agent.0'/>\n    </channel>"
    );
    line!(w, "    <input type='tablet' bus='usb'/>");
    line!(
        w,
        "    <graphics type='vnc'>\n      <listen type='socket'/>\n    </graphics>"
    );
    line!(
        w,
        "    <video>\n      <model type='virtio' heads='1'/>\n    </video>"
    );
    match spec.memory.balloon {
        BalloonMode::Off => {
            line!(w, "    <memballoon model='none'/>");
        }
        BalloonMode::Manual | BalloonMode::Auto => {
            line!(
                w,
                "    <memballoon model='virtio' autodeflate='on' freePageReporting='on'>\n      <stats period='5'/>\n    </memballoon>"
            );
        }
    }
    line!(
        w,
        "    <rng model='virtio'>\n      <backend model='random'>/dev/urandom</backend>\n    </rng>"
    );
    line!(w, "  </devices>");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::*;
    use uuid::Uuid;

    fn spec() -> VmSpec {
        VmSpec {
            id: Uuid::parse_str("8f0c2a51-4b1e-4c3e-9a0d-2f5b6c7d8e9f").unwrap(),
            name: "web<1>".into(),
            description: "a & b".into(),
            generation: 3,
            cpu: CpuSpec {
                vcpus: 2,
                model: CpuModel::HostModel,
            },
            memory: MemorySpec {
                max_mib: 2048,
                min_mib: 512,
                target_mib: 1024,
                balloon: BalloonMode::Manual,
            },
            firmware: Firmware::Bios,
            machine: "q35".into(),
            disks: vec![
                DiskSpec {
                    id: Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
                    pool: "default".into(),
                    volume: Some("web-root.qcow2".into()),
                    device: DiskDevice::Disk,
                    format: "qcow2".into(),
                    boot_order: Some(1),
                    target: "vda".into(),
                    backing: vec![],
                },
                DiskSpec {
                    id: Uuid::parse_str("66666666-7777-8888-9999-000000000000").unwrap(),
                    pool: "default".into(),
                    volume: Some("install.iso".into()),
                    device: DiskDevice::Cdrom,
                    format: "raw".into(),
                    boot_order: Some(2),
                    target: "sda".into(),
                    backing: vec![],
                },
                DiskSpec {
                    id: Uuid::parse_str("77777777-7777-8888-9999-000000000000").unwrap(),
                    pool: "slow".into(),
                    volume: None,
                    device: DiskDevice::Cdrom,
                    format: "raw".into(),
                    boot_order: None,
                    target: "sdb".into(),
                    backing: vec![],
                },
            ],
            nics: vec![NicSpec {
                id: Uuid::parse_str("abcdef01-2222-3333-4444-555555555555").unwrap(),
                mac: "52:54:00:aa:bb:cc".into(),
                network: NicNetwork::Bridge {
                    bridge: "br0".into(),
                },
                boot_order: None,
            }],
            node: Uuid::nil(),
            owner_epoch: 4,
            autostart: false,
            cloud_init: None,
            migrating: None,
            snapshot_xml: Vec::new(),
        }
    }

    fn pools(p: &str) -> Option<PoolLoc> {
        match p {
            "default" => Some(PoolLoc {
                path: "/var/lib/prism/pools/default".into(),
                direct_io: true,
            }),
            "slow" => Some(PoolLoc {
                path: "/srv/slow".into(),
                direct_io: false,
            }),
            _ => None,
        }
    }

    fn ctx<'a>(f: &'a dyn Fn(&str) -> Option<PoolLoc>, seed: Option<&'a Path>) -> RenderCtx<'a> {
        RenderCtx {
            domain_type: "kvm",
            emulator: None,
            uefi_loader: None,
            pool: f,
            seed_iso: seed,
            bridges: &NO_BRIDGES,
        }
    }

    static NO_BRIDGES: std::sync::LazyLock<std::collections::HashMap<uuid::Uuid, String>> =
        std::sync::LazyLock::new(Default::default);

    #[test]
    fn golden() {
        let seed = PathBuf::from("/var/lib/prism/pools/default/.prism-seed-x.iso");
        let xml = render(&spec(), &ctx(&pools, Some(&seed))).unwrap();
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/basic.xml"),
                &xml,
            )
            .unwrap();
        }
        let golden = include_str!("../../tests/golden/basic.xml");
        assert_eq!(xml, golden, "rendered:\n{xml}");
    }

    #[test]
    fn writeback_without_direct_io() {
        let mut s = spec();
        s.disks[0].pool = "slow".into();
        let xml = disk_xml(&s.disks[0], &ctx(&pools, None), "").unwrap();
        assert!(xml.contains("cache='writeback'"));
        assert!(!xml.contains("io='native'"));
    }

    #[test]
    fn unknown_pool() {
        let none = |_: &str| None;
        assert!(matches!(
            render(&spec(), &ctx(&none, None)),
            Err(RenderError::UnknownPool(_))
        ));
    }

    #[test]
    fn finds_vnc_socket() {
        let live = "<domain><devices><graphics type='vnc' port='-1'>\
            <listen type='socket' socket='/var/lib/libvirt/qemu/domain-1-x/vnc.sock'/>\
            </graphics></devices></domain>";
        assert_eq!(
            vnc_socket(live).unwrap(),
            PathBuf::from("/var/lib/libvirt/qemu/domain-1-x/vnc.sock")
        );
        assert!(vnc_socket("<domain/>").is_none());
    }

    #[test]
    fn finds_machine() {
        let xml = "<domain><os><type arch='x86_64' machine='pc-q35-10.2'>hvm</type></os></domain>";
        assert_eq!(machine_type(xml).as_deref(), Some("pc-q35-10.2"));
    }

    #[test]
    fn reads_disk_sources() {
        let xml = "<domain><devices>\
            <disk type='file' device='disk'><driver name='qemu' type='qcow2'/>\
              <source file='/p/a.snap1' index='2'><foo/></source>\
              <backingStore type='file' index='1'><source file='/p/a.qcow2'/></backingStore>\
              <target dev='vda' bus='virtio'/></disk>\
            <disk type='file' device='cdrom'><target dev='sda' bus='sata'/></disk>\
            </devices></domain>";
        assert_eq!(
            disk_sources(xml),
            vec![
                ("vda".to_string(), Some(PathBuf::from("/p/a.snap1"))),
                ("sda".to_string(), None)
            ]
        );
    }
}
