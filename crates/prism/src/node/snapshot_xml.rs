//! The `<domainsnapshot>` document sent to libvirt when creating an
//! external snapshot.

use std::fmt::Write;
use std::path::Path;

use quick_xml::escape::escape;

/// How one disk takes part in a snapshot.
pub(super) enum SnapshotDisk<'a> {
    /// The disk is switched to a new qcow2 overlay at this path.
    Overlay { target: &'a str, overlay: &'a Path },
    /// The disk (a CD-ROM, or a disk without a volume) is left out.
    Skip { target: &'a str },
}

/// Builds the snapshot XML. With `memory_file` set, RAM is saved there as an
/// external memory snapshot; otherwise the snapshot is disk-only.
pub(super) fn snapshot_xml(
    name: &str,
    description: &str,
    memory_file: Option<&Path>,
    disks: &[SnapshotDisk<'_>],
) -> String {
    let mut xml = format!(
        "<domainsnapshot>\n  <name>{}</name>\n  <description>{}</description>\n",
        escape(name),
        escape(description)
    );
    match memory_file {
        Some(file) => writeln!(
            xml,
            "  <memory snapshot='external' file='{}'/>",
            escape(file.to_string_lossy().as_ref())
        ),
        None => writeln!(xml, "  <memory snapshot='no'/>"),
    }
    .expect("writing to a String cannot fail");
    xml.push_str("  <disks>\n");
    for disk in disks {
        match disk {
            SnapshotDisk::Overlay { target, overlay } => writeln!(
                xml,
                "    <disk name='{}' snapshot='external' type='file'>\n      <driver type='qcow2'/>\n      <source file='{}'/>\n    </disk>",
                escape(*target),
                escape(overlay.to_string_lossy().as_ref())
            ),
            SnapshotDisk::Skip { target } => {
                writeln!(xml, "    <disk name='{}' snapshot='no'/>", escape(*target))
            }
        }
        .expect("writing to a String cannot fail");
    }
    xml.push_str("  </disks>\n</domainsnapshot>\n");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_only_snapshot_skips_cdroms() {
        let xml = snapshot_xml(
            "before",
            "a < b",
            None,
            &[
                SnapshotDisk::Overlay {
                    target: "vda",
                    overlay: Path::new("/pool/vm.vda.before.qcow2"),
                },
                SnapshotDisk::Skip { target: "sda" },
            ],
        );
        assert!(xml.contains("<name>before</name>"));
        assert!(xml.contains("<description>a &lt; b</description>"));
        assert!(xml.contains("<memory snapshot='no'/>"));
        assert!(xml.contains("<source file='/pool/vm.vda.before.qcow2'/>"));
        assert!(xml.contains("<disk name='sda' snapshot='no'/>"));
    }

    #[test]
    fn memory_snapshot_names_its_file_escaped() {
        let xml = snapshot_xml("s", "", Some(Path::new("/pool/a&b.mem")), &[]);
        assert!(xml.contains("<memory snapshot='external' file='/pool/a&amp;b.mem'/>"));
    }
}
