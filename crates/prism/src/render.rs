//! `VmSpec` → libvirt domain XML. Pure functions, tested against golden output.

use std::fmt::Write;
use std::path::{Path, PathBuf};

use quick_xml::escape::escape;

use crate::spec::{
    BalloonMode, CpuModel, DiskDevice, DiskSpec, Firmware, NicNetwork, NicSpec, VmSpec,
};

/// Where a pool keeps its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolLoc {
    pub path: PathBuf,
    /// The filesystem supports `O_DIRECT`, so disks use `cache='none'`.
    pub direct_io: bool,
}

/// Host-specific inputs to rendering.
#[derive(Clone)]
pub struct RenderCtx<'a> {
    pub domain_type: &'a str,
    pub emulator: Option<&'a Path>,
    pub uefi_loader: Option<&'a Path>,
    pub pool: &'a dyn Fn(&str) -> Option<PoolLoc>,
    /// A cloud-init seed ISO to attach as an extra read-only CD-ROM.
    pub seed_iso: Option<&'a Path>,
}

pub const METADATA_NS: &str = "urn:prism:1";
/// Empty PCI Express root ports so disks and NICs can be hot-plugged on `q35`.
const SPARE_ROOT_PORTS: usize = 6;

fn esc(s: &str) -> std::borrow::Cow<'_, str> {
    escape(s)
}

/// The `<disk>` element for one disk, as used both in the domain and for hotplug.
pub fn disk_xml(disk: &DiskSpec, ctx: &RenderCtx<'_>, indent: &str) -> Result<String, String> {
    let pool = (ctx.pool)(&disk.pool).ok_or_else(|| format!("unknown pool {}", disk.pool))?;
    let mut x = String::new();
    let w = &mut x;
    let (device, bus) = match disk.device {
        DiskDevice::Disk => ("disk", "virtio"),
        DiskDevice::Cdrom => ("cdrom", "sata"),
    };
    let _ = writeln!(w, "{indent}<disk type='file' device='{device}'>");
    match disk.device {
        DiskDevice::Disk if pool.direct_io => {
            let _ = writeln!(
                w,
                "{indent}  <driver name='qemu' type='{}' cache='none' io='native' discard='unmap'/>",
                esc(&disk.format)
            );
        }
        DiskDevice::Disk => {
            let _ = writeln!(
                w,
                "{indent}  <driver name='qemu' type='{}' cache='writeback' discard='unmap'/>",
                esc(&disk.format)
            );
        }
        DiskDevice::Cdrom => {
            let _ = writeln!(w, "{indent}  <driver name='qemu' type='raw'/>");
        }
    }
    if let Some(volume) = &disk.volume {
        let path = pool.path.join(volume);
        let _ = writeln!(
            w,
            "{indent}  <source file='{}'/>",
            esc(&path.to_string_lossy())
        );
    }
    let _ = writeln!(
        w,
        "{indent}  <target dev='{}' bus='{bus}'/>",
        esc(&disk.target)
    );
    if disk.device == DiskDevice::Cdrom {
        let _ = writeln!(w, "{indent}  <readonly/>");
    }
    if let Some(order) = disk.boot_order {
        let _ = writeln!(w, "{indent}  <boot order='{order}'/>");
    }
    let _ = writeln!(
        w,
        "{indent}  <serial>{}</serial>",
        &disk.id.simple().to_string()[..20]
    );
    let _ = writeln!(w, "{indent}  <alias name='ua-{}'/>", disk.id);
    let _ = writeln!(w, "{indent}</disk>");
    Ok(x)
}

/// The `<interface>` element for one NIC.
pub fn nic_xml(nic: &NicSpec, indent: &str) -> String {
    let mut x = String::new();
    let w = &mut x;
    match &nic.network {
        NicNetwork::Bridge { bridge } => {
            let _ = writeln!(w, "{indent}<interface type='bridge'>");
            let _ = writeln!(w, "{indent}  <source bridge='{}'/>", esc(bridge));
            let _ = writeln!(
                w,
                "{indent}  <target dev='pn{}'/>",
                &nic.id.simple().to_string()[..8]
            );
        }
        NicNetwork::User => {
            let _ = writeln!(w, "{indent}<interface type='user'>");
        }
    }
    let _ = writeln!(w, "{indent}  <mac address='{}'/>", esc(&nic.mac));
    let _ = writeln!(w, "{indent}  <model type='virtio'/>");
    if let Some(order) = nic.boot_order {
        let _ = writeln!(w, "{indent}  <boot order='{order}'/>");
    }
    let _ = writeln!(w, "{indent}  <alias name='ua-{}'/>", nic.id);
    let _ = writeln!(w, "{indent}</interface>");
    x
}

/// The seed CD-ROM, as a `<disk>` element.
///
/// It takes the lowest SATA name not used by the VM's CD-ROMs: libvirt
/// derives the controller index from the name, so a high name like `sdzz`
/// would make it create a controller for every slot below it.
pub fn seed_xml(path: &Path, target: &str, indent: &str) -> String {
    format!(
        "{indent}<disk type='file' device='cdrom'>\n{indent}  <driver name='qemu' type='raw'/>\n\
         {indent}  <source file='{}'/>\n{indent}  <target dev='{target}' bus='sata'/>\n\
         {indent}  <readonly/>\n{indent}  <alias name='ua-cloud-init'/>\n{indent}</disk>\n",
        esc(&path.to_string_lossy())
    )
}

/// Renders the persistent domain definition for `spec`.
pub fn render(spec: &VmSpec, ctx: &RenderCtx<'_>) -> Result<String, String> {
    let mut x = String::with_capacity(4096);
    let w = &mut x;
    let _ = writeln!(w, "<domain type='{}'>", esc(ctx.domain_type));
    let _ = writeln!(w, "  <name>{}</name>", esc(&spec.name));
    let _ = writeln!(w, "  <uuid>{}</uuid>", spec.id);
    if !spec.description.is_empty() {
        let _ = writeln!(w, "  <description>{}</description>", esc(&spec.description));
    }
    let _ = writeln!(
        w,
        "  <metadata>\n    <prism:vm xmlns:prism='{METADATA_NS}' generation='{}' epoch='{}'/>\n  </metadata>",
        spec.generation, spec.owner_epoch
    );
    let _ = writeln!(
        w,
        "  <memory unit='KiB'>{}</memory>",
        spec.memory.max_mib * 1024
    );
    let current = match spec.memory.balloon {
        BalloonMode::Off => spec.memory.max_mib,
        BalloonMode::Manual | BalloonMode::Auto => spec.memory.target_mib,
    };
    let _ = writeln!(
        w,
        "  <currentMemory unit='KiB'>{}</currentMemory>",
        current * 1024
    );
    let _ = writeln!(w, "  <vcpu placement='static'>{}</vcpu>", spec.cpu.vcpus);

    match (spec.firmware, ctx.uefi_loader) {
        (Firmware::Uefi, None) => {
            let _ = writeln!(w, "  <os firmware='efi'>");
        }
        _ => {
            let _ = writeln!(w, "  <os>");
        }
    }
    let _ = writeln!(
        w,
        "    <type arch='x86_64' machine='{}'>hvm</type>",
        esc(&spec.machine)
    );
    if let (Firmware::Uefi, Some(loader)) = (spec.firmware, ctx.uefi_loader) {
        let _ = writeln!(
            w,
            "    <loader readonly='yes' type='rom'>{}</loader>",
            esc(&loader.to_string_lossy())
        );
    }
    let _ = writeln!(w, "  </os>");
    let _ = writeln!(w, "  <features>\n    <acpi/>\n    <apic/>\n  </features>");
    match (ctx.domain_type, spec.cpu.model) {
        ("kvm", CpuModel::HostModel) => {
            let _ = writeln!(w, "  <cpu mode='host-model'/>");
        }
        ("kvm", CpuModel::HostPassthrough) => {
            let _ = writeln!(w, "  <cpu mode='host-passthrough' migratable='on'/>");
        }
        _ => {
            let _ = writeln!(
                w,
                "  <cpu mode='custom' match='exact'>\n    <model fallback='allow'>qemu64</model>\n  </cpu>"
            );
        }
    }
    let _ = writeln!(w, "  <clock offset='utc'/>");
    let _ = writeln!(
        w,
        "  <on_poweroff>destroy</on_poweroff>\n  <on_reboot>restart</on_reboot>\n  <on_crash>destroy</on_crash>"
    );

    let _ = writeln!(w, "  <devices>");
    if let Some(emu) = ctx.emulator {
        let _ = writeln!(
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
        w.push_str(&nic_xml(nic, "    "));
    }
    // Declared first so libvirt numbers the spare ports from 1.
    let _ = writeln!(w, "    <controller type='pci' index='0' model='pcie-root'/>");
    for _ in 0..SPARE_ROOT_PORTS {
        let _ = writeln!(w, "    <controller type='pci' model='pcie-root-port'/>");
    }
    let _ = writeln!(
        w,
        "    <serial type='pty'>\n      <target port='0'/>\n    </serial>\n    <console type='pty'>\n      <target type='serial' port='0'/>\n    </console>"
    );
    let _ = writeln!(
        w,
        "    <channel type='unix'>\n      <target type='virtio' name='org.qemu.guest_agent.0'/>\n    </channel>"
    );
    let _ = writeln!(w, "    <input type='tablet' bus='usb'/>");
    let _ = writeln!(
        w,
        "    <graphics type='vnc'>\n      <listen type='socket'/>\n    </graphics>"
    );
    let _ = writeln!(
        w,
        "    <video>\n      <model type='virtio' heads='1'/>\n    </video>"
    );
    match spec.memory.balloon {
        BalloonMode::Off => {
            let _ = writeln!(w, "    <memballoon model='none'/>");
        }
        BalloonMode::Manual | BalloonMode::Auto => {
            let _ = writeln!(
                w,
                "    <memballoon model='virtio' autodeflate='on' freePageReporting='on'>\n      <stats period='5'/>\n    </memballoon>"
            );
        }
    }
    let _ = writeln!(
        w,
        "    <rng model='virtio'>\n      <backend model='random'>/dev/urandom</backend>\n    </rng>"
    );
    let _ = writeln!(w, "  </devices>");
    let _ = writeln!(w, "</domain>");
    Ok(x)
}

/// The SATA name for the cloud-init seed CD-ROM.
pub fn seed_target(spec: &VmSpec) -> String {
    (0..26 * 27)
        .map(|i| crate::spec::disk_target("sd", i))
        .find(|t| !spec.disks.iter().any(|d| d.target == *t))
        .unwrap_or_default()
}

/// `(target, source file)` for each `<disk>` in domain XML, in document order.
pub fn disk_sources(xml: &str) -> Vec<(String, Option<PathBuf>)> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut disk_depth = None;
    let (mut target, mut source) = (None, None);
    loop {
        let Ok(ev) = reader.read_event() else {
            return out;
        };
        match ev {
            Event::Start(e) => {
                depth += 1;
                if e.name().as_ref() == "disk" && disk_depth.is_none() {
                    disk_depth = Some(depth);
                    (target, source) = (None, None);
                } else if disk_depth.is_some() && depth == disk_depth.unwrap_or(0) + 1 {
                    take_disk_child(&e, &mut target, &mut source);
                }
            }
            Event::Empty(e) => {
                if disk_depth.is_some_and(|d| depth == d) {
                    take_disk_child(&e, &mut target, &mut source);
                }
            }
            Event::End(e) => {
                if e.name().as_ref() == "disk" && disk_depth == Some(depth) {
                    disk_depth = None;
                    if let Some(t) = target.take() {
                        out.push((t, source.take().map(PathBuf::from)));
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => return out,
            _ => {}
        }
    }
}

fn take_disk_child(
    e: &quick_xml::events::BytesStart<'_>,
    target: &mut Option<String>,
    source: &mut Option<String>,
) {
    match e.name().as_ref() {
        "target" => *target = get_attr(e, "dev"),
        "source" => *source = get_attr(e, "file"),
        _ => {}
    }
}

/// Extracts the VNC UNIX socket path from live domain XML.
pub fn vnc_socket(live_xml: &str) -> Option<PathBuf> {
    attr_in_element(live_xml, "graphics", "type='vnc'", "socket")
        .or_else(|| attr_in_element(live_xml, "graphics", "type=\"vnc\"", "socket"))
        .map(PathBuf::from)
}

/// Finds the first `<tag …>` containing `marker` and returns attribute `attr`
/// from it or from its first child element that has it.
fn attr_in_element(xml: &str, tag: &str, marker: &str, attr: &str) -> Option<String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    let mut depth_in = 0usize;
    let marker_key = marker.split('=').next()?;
    let marker_val = marker.split('=').nth(1)?.trim_matches(['\'', '"']);
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) | Event::Empty(e) if depth_in == 0 => {
                if e.name().as_ref() == tag
                    && e.attributes()
                        .flatten()
                        .any(|a| a.key.as_ref() == marker_key && a.value.as_ref() == marker_val)
                {
                    if let Some(v) = get_attr(&e, attr) {
                        return Some(v);
                    }
                    depth_in = 1;
                }
            }
            Event::Start(e) if depth_in > 0 => {
                if let Some(v) = get_attr(&e, attr) {
                    return Some(v);
                }
                depth_in += 1;
            }
            Event::Empty(e) if depth_in > 0 => {
                if let Some(v) = get_attr(&e, attr) {
                    return Some(v);
                }
            }
            Event::End(_) if depth_in > 0 => {
                depth_in -= 1;
                if depth_in == 0 {
                    return None;
                }
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

fn get_attr(e: &quick_xml::events::BytesStart<'_>, attr: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == attr)
        .and_then(|a| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(std::borrow::Cow::into_owned)
        })
}

/// Extracts the expanded machine type (for example `pc-q35-10.2`) from
/// domain XML.
pub fn machine_type(xml: &str) -> Option<String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) | Event::Empty(e) if e.name().as_ref() == "type" => {
                return get_attr(&e, "machine");
            }
            Event::Eof => return None,
            _ => {}
        }
    }
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
        }
    }

    #[test]
    fn golden() {
        let seed = PathBuf::from("/var/lib/prism/pools/default/.prism-seed-x.iso");
        let xml = render(&spec(), &ctx(&pools, Some(&seed))).unwrap();
        let golden = include_str!("../tests/golden/basic.xml");
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
        assert!(render(&spec(), &ctx(&none, None)).is_err());
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
