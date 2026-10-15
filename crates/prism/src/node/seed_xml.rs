//! Domain XML repair for the cloud-init seed CD-ROM.
//!
//! libvirt's native snapshot revert gives the read-only seed CD-ROM
//! qcow2 overlays (`.prism-seed-<id>.<epoch>`) like any other disk. Those
//! overlays are never shipped to a migration destination, which regenerates
//! the plain ISO instead, so the XML must point at the ISO again.

/// Replaces the source chain of every seed CD-ROM in `xml` with the single
/// raw ISO `iso` (a path on the destination).
pub(super) fn flatten_seed_cdroms(xml: &str, iso: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find("<disk ") {
        let Some(len) = rest[start..].find("</disk>") else {
            break;
        };
        let end = start + len + "</disk>".len();
        let block = &rest[start..end];
        out.push_str(&rest[..start]);
        if block.contains("device='cdrom'") && block.contains(".prism-seed-") {
            out.push_str(&flatten_block(block, iso));
        } else {
            out.push_str(block);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn flatten_block(block: &str, iso: &str) -> String {
    let mut b = block.to_string();
    // Drop the backing chain.
    if let Some(s) = b.find("<backingStore")
        && let Some(e) = b.rfind("</backingStore>")
    {
        b.replace_range(s..e + "</backingStore>".len(), "");
    }
    // The top source, keeping its attributes other than the path.
    if let Some(s) = b.find("<source file='")
        && let Some(e) = b[s..].find("/>")
    {
        b.replace_range(s..s + e + 2, &format!("<source file='{iso}'/>"));
    }
    b.replace(
        "<driver name='qemu' type='qcow2'/>",
        "<driver name='qemu' type='raw'/>",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_overlay_chain() {
        let xml = "<devices><disk type='file' device='disk'><source file='/p/a'/></disk>\
<disk type='file' device='cdrom'><driver name='qemu' type='qcow2'/>\
<source file='/p/.prism-seed-1.22' index='12'/>\
<backingStore type='file' index='1'><format type='qcow2'/><source file='/p/.prism-seed-1.11'/>\
<backingStore type='file'><format type='raw'/><source file='/p/.prism-seed-1.iso'/><backingStore/></backingStore>\
</backingStore><target dev='sda'/></disk></devices>";
        let out = flatten_seed_cdroms(xml, "/q/.prism-seed-1.iso");
        assert!(out.contains("<source file='/p/a'/>"));
        assert!(out.contains("<driver name='qemu' type='raw'/><source file='/q/.prism-seed-1.iso'/><target dev='sda'/>"));
        assert!(!out.contains("backingStore"));
    }
}
