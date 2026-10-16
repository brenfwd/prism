//! The `<domainbackup>` and `<domaincheckpoint>` documents for a push-mode
//! backup of a running VM.

use std::fmt::Write;
use std::path::Path;

use quick_xml::escape::escape;

const INFALLIBLE: &str = "writing to a String cannot fail";

/// One disk of the VM. `staging` is the qcow2 file libvirt writes the
/// backup to, or `None` for a disk the backup leaves out.
pub(super) struct BackupDisk<'a> {
    pub target: &'a str,
    pub staging: Option<&'a Path>,
}

/// Builds `<domainbackup mode='push'>`. `parent_checkpoint` makes it
/// incremental: only blocks changed since that checkpoint are copied.
pub(super) fn backup_xml(parent_checkpoint: Option<&str>, disks: &[BackupDisk<'_>]) -> String {
    let mut xml = String::from("<domainbackup mode='push'>\n");
    if let Some(cp) = parent_checkpoint {
        writeln!(xml, "  <incremental>{}</incremental>", escape(cp)).expect(INFALLIBLE);
    }
    xml.push_str("  <disks>\n");
    for d in disks {
        match d.staging {
            Some(file) => writeln!(
                xml,
                "    <disk name='{}' backup='yes' type='file'>\n      <driver type='qcow2'/>\n      <target file='{}'/>\n    </disk>",
                escape(d.target),
                escape(file.to_string_lossy().as_ref())
            ),
            None => writeln!(xml, "    <disk name='{}' backup='no'/>", escape(d.target)),
        }
        .expect(INFALLIBLE);
    }
    xml.push_str("  </disks>\n</domainbackup>\n");
    xml
}

/// Builds the `<domaincheckpoint>` that records a bitmap for each backed-up
/// disk, so the next backup can be incremental.
pub(super) fn checkpoint_xml(name: &str, disks: &[BackupDisk<'_>]) -> String {
    let mut xml = format!(
        "<domaincheckpoint>\n  <name>{}</name>\n  <disks>\n",
        escape(name)
    );
    for d in disks {
        let mode = if d.staging.is_some() { "bitmap" } else { "no" };
        writeln!(
            xml,
            "    <disk name='{}' checkpoint='{mode}'/>",
            escape(d.target)
        )
        .expect(INFALLIBLE);
    }
    xml.push_str("  </disks>\n</domaincheckpoint>\n");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disks() -> [BackupDisk<'static>; 2] {
        [
            BackupDisk {
                target: "vda",
                staging: Some(Path::new("/pool/stage-vda.qcow2")),
            },
            BackupDisk {
                target: "sda",
                staging: None,
            },
        ]
    }

    #[test]
    fn full_backup_has_no_incremental_element() {
        let xml = backup_xml(None, &disks());
        assert!(xml.starts_with("<domainbackup mode='push'>"));
        assert!(!xml.contains("<incremental>"));
        assert!(xml.contains("<disk name='vda' backup='yes' type='file'>"));
        assert!(xml.contains("<target file='/pool/stage-vda.qcow2'/>"));
        assert!(xml.contains("<disk name='sda' backup='no'/>"));
    }

    #[test]
    fn incremental_backup_names_its_parent_checkpoint() {
        let xml = backup_xml(Some("prism-a&b"), &disks());
        assert!(xml.contains("<incremental>prism-a&amp;b</incremental>"));
    }

    #[test]
    fn checkpoint_tracks_only_backed_up_disks() {
        let xml = checkpoint_xml("prism-1", &disks());
        assert!(xml.contains("<name>prism-1</name>"));
        assert!(xml.contains("<disk name='vda' checkpoint='bitmap'/>"));
        assert!(xml.contains("<disk name='sda' checkpoint='no'/>"));
    }
}
