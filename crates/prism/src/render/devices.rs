//! Per-device XML: disks, NICs and the cloud-init seed CD-ROM. Each function
//! returns one element, indented by the caller-supplied prefix, so the same
//! output serves the full domain definition and live hot-plug.

use std::path::Path;

use super::{RenderCtx, RenderError, esc, line};
use crate::spec::{DiskDevice, DiskSpec, NicNetwork, NicSpec, VmSpec};

/// Empty PCI Express root ports so disks and NICs can be hot-plugged on `q35`.
/// libvirt puts the VM's own devices on the declared ports first, so these
/// come on top of the ports those devices take (see `root_ports`).
const SPARE_ROOT_PORTS: usize = 6;
/// Devices libvirt adds on its own that sit behind a root port: balloon,
/// RNG, the guest-agent serial controller and the USB controller.
const BUILTIN_PORT_DEVICES: usize = 5;

/// Root ports to declare: one per disk and NIC, the built-in devices and the
/// hot-plug spares.
pub(super) fn root_ports(spec: &VmSpec) -> usize {
    let disks = spec
        .disks
        .iter()
        .filter(|d| d.device == DiskDevice::Disk)
        .count();
    disks + spec.nics.len() + BUILTIN_PORT_DEVICES + SPARE_ROOT_PORTS
}

/// The `<disk>` element for one disk, as used both in the domain and for hotplug.
pub fn disk_xml(disk: &DiskSpec, ctx: &RenderCtx<'_>, indent: &str) -> Result<String, RenderError> {
    let pool = (ctx.pool)(&disk.pool).ok_or_else(|| RenderError::UnknownPool(disk.pool.clone()))?;
    let mut x = String::new();
    let w = &mut x;
    let (device, bus) = match disk.device {
        DiskDevice::Disk => ("disk", "virtio"),
        DiskDevice::Cdrom => ("cdrom", "sata"),
    };
    line!(w, "{indent}<disk type='file' device='{device}'>");
    match disk.device {
        DiskDevice::Disk if pool.direct_io => {
            line!(
                w,
                "{indent}  <driver name='qemu' type='{}' cache='none' io='native' discard='unmap'/>",
                esc(&disk.format)
            );
        }
        DiskDevice::Disk => {
            line!(
                w,
                "{indent}  <driver name='qemu' type='{}' cache='writeback' discard='unmap'/>",
                esc(&disk.format)
            );
        }
        DiskDevice::Cdrom => {
            line!(w, "{indent}  <driver name='qemu' type='raw'/>");
        }
    }
    if let Some(volume) = &disk.volume {
        let path = pool.path.join(volume);
        line!(
            w,
            "{indent}  <source file='{}'/>",
            esc(&path.to_string_lossy())
        );
    }
    line!(
        w,
        "{indent}  <target dev='{}' bus='{bus}'/>",
        esc(&disk.target)
    );
    if disk.device == DiskDevice::Cdrom {
        line!(w, "{indent}  <readonly/>");
    }
    if let Some(order) = disk.boot_order {
        line!(w, "{indent}  <boot order='{order}'/>");
    }
    line!(
        w,
        "{indent}  <serial>{}</serial>",
        &disk.id.simple().to_string()[..20]
    );
    line!(w, "{indent}  <alias name='ua-{}'/>", disk.id);
    line!(w, "{indent}</disk>");
    Ok(x)
}

/// The `<interface>` element for one NIC.
pub fn nic_xml(
    nic: &NicSpec,
    bridges: &std::collections::HashMap<uuid::Uuid, String>,
    indent: &str,
) -> Result<String, RenderError> {
    let mut x = String::new();
    let w = &mut x;
    let tap = |w: &mut String, bridge: &str| {
        line!(w, "{indent}<interface type='bridge'>");
        line!(w, "{indent}  <source bridge='{}'/>", esc(bridge));
        line!(
            w,
            "{indent}  <target dev='pn{}'/>",
            &nic.id.simple().to_string()[..8]
        );
    };
    match &nic.network {
        NicNetwork::Bridge { bridge } => tap(w, bridge),
        NicNetwork::Network { network, .. } => {
            let bridge = bridges
                .get(network)
                .ok_or(RenderError::UnknownNetwork(*network))?;
            tap(w, bridge);
        }
        NicNetwork::User { forwards } => {
            line!(w, "{indent}<interface type='user'>");
            if !forwards.is_empty() {
                // Only the passt backend can forward ports.
                line!(w, "{indent}  <backend type='passt'/>");
                for f in forwards {
                    line!(
                        w,
                        "{indent}  <portForward proto='{}' address='{}'><range start='{}' to='{}'/></portForward>",
                        f.proto.as_str(),
                        f.listen_addr(),
                        f.host_port,
                        f.guest_port
                    );
                }
            }
        }
    }
    line!(w, "{indent}  <mac address='{}'/>", esc(&nic.mac));
    line!(w, "{indent}  <model type='virtio'/>");
    if let Some(order) = nic.boot_order {
        line!(w, "{indent}  <boot order='{order}'/>");
    }
    line!(w, "{indent}  <alias name='ua-{}'/>", nic.id);
    line!(w, "{indent}</interface>");
    Ok(x)
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

/// The SATA name for the cloud-init seed CD-ROM.
pub fn seed_target(spec: &VmSpec) -> String {
    (0..26 * 27)
        .map(|i| crate::spec::disk_target("sd", i))
        .find(|t| !spec.disks.iter().any(|d| d.target == *t))
        .unwrap_or_default()
}
