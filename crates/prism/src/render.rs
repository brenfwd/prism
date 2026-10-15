//! `VmSpec` → libvirt domain XML. A pure function, tested against golden output.

use std::fmt::Write;
use std::path::{Path, PathBuf};

use quick_xml::escape::escape;

use crate::spec::{BalloonMode, DiskDevice, Firmware, NicNetwork, VmSpec};

/// Host-specific inputs to rendering.
#[derive(Clone)]
pub struct RenderCtx<'a> {
    pub domain_type: &'a str,
    pub emulator: Option<&'a Path>,
    pub uefi_loader: Option<&'a Path>,
    /// Resolves a pool name to its directory.
    pub pool_path: &'a dyn Fn(&str) -> Option<PathBuf>,
}

pub const METADATA_NS: &str = "urn:prism:1";

fn esc(s: &str) -> std::borrow::Cow<'_, str> {
    escape(s)
}

fn disk_target(device: DiskDevice, index: usize) -> String {
    let prefix = match device {
        DiskDevice::Disk => "vd",
        DiskDevice::Cdrom => "sd",
    };
    // vda..vdz, then vdaa..: enough for any VM Prism creates.
    let letters = b"abcdefghijklmnopqrstuvwxyz";
    if index < 26 {
        format!("{prefix}{}", letters[index] as char)
    } else {
        format!(
            "{prefix}{}{}",
            letters[index / 26 - 1] as char,
            letters[index % 26] as char
        )
    }
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
        "  <metadata>\n    <prism:vm xmlns:prism='{METADATA_NS}' generation='{}'/>\n  </metadata>",
        spec.generation
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
    if ctx.domain_type == "kvm" {
        let _ = writeln!(w, "  <cpu mode='host-model'/>");
    } else {
        let _ = writeln!(
            w,
            "  <cpu mode='custom' match='exact'>\n    <model fallback='allow'>qemu64</model>\n  </cpu>"
        );
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
    let (mut n_disk, mut n_cd) = (0, 0);
    for disk in &spec.disks {
        let dir =
            (ctx.pool_path)(&disk.pool).ok_or_else(|| format!("unknown pool {}", disk.pool))?;
        let path = dir.join(&disk.volume);
        let (target, bus) = match disk.device {
            DiskDevice::Disk => {
                n_disk += 1;
                (disk_target(DiskDevice::Disk, n_disk - 1), "virtio")
            }
            DiskDevice::Cdrom => {
                n_cd += 1;
                (disk_target(DiskDevice::Cdrom, n_cd - 1), "sata")
            }
        };
        let device = match disk.device {
            DiskDevice::Disk => "disk",
            DiskDevice::Cdrom => "cdrom",
        };
        let _ = writeln!(w, "    <disk type='file' device='{device}'>");
        match disk.device {
            DiskDevice::Disk => {
                let _ = writeln!(
                    w,
                    "      <driver name='qemu' type='{}' cache='none' io='native' discard='unmap'/>",
                    esc(&disk.format)
                );
            }
            DiskDevice::Cdrom => {
                let _ = writeln!(w, "      <driver name='qemu' type='raw'/>");
            }
        }
        let _ = writeln!(w, "      <source file='{}'/>", esc(&path.to_string_lossy()));
        let _ = writeln!(w, "      <target dev='{target}' bus='{bus}'/>");
        if disk.device == DiskDevice::Cdrom {
            let _ = writeln!(w, "      <readonly/>");
        }
        if let Some(order) = disk.boot_order {
            let _ = writeln!(w, "      <boot order='{order}'/>");
        }
        let _ = writeln!(
            w,
            "      <serial>{}</serial>",
            &disk.id.simple().to_string()[..20]
        );
        let _ = writeln!(w, "    </disk>");
    }
    for nic in &spec.nics {
        match &nic.network {
            NicNetwork::Bridge { bridge } => {
                let _ = writeln!(w, "    <interface type='bridge'>");
                let _ = writeln!(w, "      <source bridge='{}'/>", esc(bridge));
                let _ = writeln!(
                    w,
                    "      <target dev='pn{}'/>",
                    &nic.id.simple().to_string()[..8]
                );
            }
            NicNetwork::User => {
                let _ = writeln!(w, "    <interface type='user'>");
            }
        }
        let _ = writeln!(w, "      <mac address='{}'/>", esc(&nic.mac));
        let _ = writeln!(w, "      <model type='virtio'/>");
        let _ = writeln!(w, "    </interface>");
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
            cpu: CpuSpec { vcpus: 2 },
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
                    volume: "web-root.qcow2".into(),
                    device: DiskDevice::Disk,
                    format: "qcow2".into(),
                    boot_order: Some(1),
                },
                DiskSpec {
                    id: Uuid::parse_str("66666666-7777-8888-9999-000000000000").unwrap(),
                    pool: "default".into(),
                    volume: "install.iso".into(),
                    device: DiskDevice::Cdrom,
                    format: "raw".into(),
                    boot_order: Some(2),
                },
            ],
            nics: vec![NicSpec {
                id: Uuid::parse_str("abcdef01-2222-3333-4444-555555555555").unwrap(),
                mac: "52:54:00:aa:bb:cc".into(),
                network: NicNetwork::Bridge {
                    bridge: "br0".into(),
                },
            }],
        }
    }

    fn ctx(f: &dyn Fn(&str) -> Option<PathBuf>) -> RenderCtx<'_> {
        RenderCtx {
            domain_type: "kvm",
            emulator: None,
            uefi_loader: None,
            pool_path: f,
        }
    }

    #[test]
    fn golden() {
        let pools =
            |p: &str| (p == "default").then(|| PathBuf::from("/var/lib/prism/pools/default"));
        let xml = render(&spec(), &ctx(&pools)).unwrap();
        let golden = include_str!("../tests/golden/basic.xml");
        assert_eq!(xml, golden, "rendered:\n{xml}");
    }

    #[test]
    fn unknown_pool() {
        let pools = |_: &str| None;
        assert!(render(&spec(), &ctx(&pools)).is_err());
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
    fn targets() {
        assert_eq!(disk_target(DiskDevice::Disk, 0), "vda");
        assert_eq!(disk_target(DiskDevice::Disk, 26), "vdaa");
        assert_eq!(disk_target(DiskDevice::Cdrom, 1), "sdb");
    }
}
