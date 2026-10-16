//! The full backing chain in a migrated domain's XML.
//!
//! The migratable XML that `Begin3` returns names only the first backing
//! layer of a disk. The destination then knows two files of a longer chain,
//! and its `AppArmor` profile (built from the XML) grants QEMU only those, so
//! QEMU fails opening the deeper layers with "Permission denied". Listing
//! the whole chain gives libvirt every file to label and allow.

use std::fmt::Write as _;

/// One disk's chain: `target` is the guest device (`vda`), `layers` the
/// backing files nearest the top first, each with its format.
pub(super) struct Chain {
    pub target: String,
    pub layers: Vec<(String, String)>,
}

/// Rewrites the `<backingStore>` of each `chains` disk in `xml` to the full chain.
pub(super) fn expand_chains(xml: &str, chains: &[Chain]) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find("<disk ") {
        let Some(len) = rest[start..].find("</disk>") else {
            break;
        };
        let end = start + len + "</disk>".len();
        let block = &rest[start..end];
        out.push_str(&rest[..start]);
        match chains.iter().find(|c| {
            !c.layers.is_empty() && block.contains(&format!("<target dev='{}'", c.target))
        }) {
            Some(c) => out.push_str(&expand_block(block, &c.layers)),
            None => out.push_str(block),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn chain_xml(layers: &[(String, String)]) -> String {
    let mut open = String::new();
    for (path, format) in layers {
        let _ = write!(
            open,
            "<backingStore type='file'><format type='{format}'/><source file='{path}'/>"
        );
    }
    open.push_str("<backingStore/>");
    for _ in layers {
        open.push_str("</backingStore>");
    }
    open
}

fn expand_block(block: &str, layers: &[(String, String)]) -> String {
    let mut b = block.to_string();
    if let Some(s) = b.find("<backingStore")
        && let Some(e) = b.rfind("</backingStore>")
    {
        b.replace_range(s..e + "</backingStore>".len(), "");
    }
    // Right after the top source, which is self-closing when it has no children.
    if let Some(s) = b.find("<source ")
        && let Some(e) = b[s..].find("/>")
    {
        b.insert_str(s + e + 2, &chain_xml(layers));
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layers() -> Vec<(String, String)> {
        vec![
            ("/p/b2".into(), "qcow2".into()),
            ("/p/b1".into(), "qcow2".into()),
            ("/p/base".into(), "raw".into()),
        ]
    }

    #[test]
    fn replaces_a_one_level_chain_with_the_whole_chain() {
        let xml = "<disk type='file' device='disk'><source file='/p/top'/>\
<backingStore type='file'><format type='qcow2'/><source file='/p/b2'/></backingStore>\
<target dev='vda' bus='virtio'/></disk>";
        let out = expand_chains(
            xml,
            &[Chain {
                target: "vda".into(),
                layers: layers(),
            }],
        );
        let want = "<source file='/p/top'/>\
<backingStore type='file'><format type='qcow2'/><source file='/p/b2'/>\
<backingStore type='file'><format type='qcow2'/><source file='/p/b1'/>\
<backingStore type='file'><format type='raw'/><source file='/p/base'/>\
<backingStore/></backingStore></backingStore></backingStore><target dev='vda'";
        assert!(out.contains(want), "{out}");
    }

    #[test]
    fn adds_a_chain_where_there_was_none() {
        let xml =
            "<disk type='file' device='disk'><source file='/p/top'/><target dev='vda'/></disk>";
        let out = expand_chains(
            xml,
            &[Chain {
                target: "vda".into(),
                layers: layers(),
            }],
        );
        assert!(out.contains("<source file='/p/top'/><backingStore type='file'>"));
        assert_eq!(out.matches("<backingStore type='file'>").count(), 3);
    }

    #[test]
    fn leaves_other_disks_alone() {
        let xml = "<disk type='file' device='disk'><source file='/p/x'/><target dev='vdb'/></disk>";
        let out = expand_chains(
            xml,
            &[Chain {
                target: "vda".into(),
                layers: layers(),
            }],
        );
        assert_eq!(out, xml);
    }
}
