//! Reading file paths out of libvirt snapshot XML.

use std::path::PathBuf;
pub(super) fn memory_file(snapshot_xml: &str) -> Option<PathBuf> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(snapshot_xml);
    let mut depth = 0usize;
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) => {
                depth += 1;
                if depth == 2 && e.name().as_ref() == "memory" {
                    return file_attr(&e);
                }
            }
            Event::Empty(e) if depth == 1 && e.name().as_ref() == "memory" => return file_attr(&e),
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Eof => return None,
            _ => {}
        }
    }
}

/// The files in a snapshot's `<disks>`: the overlay each disk continued in when it was taken.
pub(super) fn snapshot_overlays(snapshot_xml: &str) -> Vec<PathBuf> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(snapshot_xml);
    let mut path: Vec<String> = Vec::new();
    let mut out = Vec::new();
    let in_disk = |path: &[String]| path.ends_with(&["disks".to_owned(), "disk".to_owned()]);
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => {
                if e.name().as_ref() == "source"
                    && path.len() == 3
                    && in_disk(&path)
                    && let Some(f) = file_attr(&e)
                {
                    out.push(f);
                }
                path.push(e.name().as_ref().to_owned());
            }
            Event::Empty(e) => {
                if e.name().as_ref() == "source"
                    && path.len() == 3
                    && in_disk(&path)
                    && let Some(f) = file_attr(&e)
                {
                    out.push(f);
                }
            }
            Event::End(_) => {
                path.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

fn file_attr(e: &quick_xml::events::BytesStart<'_>) -> Option<PathBuf> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == "file")
        .map(|a| PathBuf::from(a.value.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_overlays_a_snapshot_created() {
        let xml = "<domainsnapshot><name>s1</name>\
            <disks><disk name='vda' snapshot='external' type='file'>\
            <driver type='qcow2'/><source file='/p/web.s1.qcow2'><privateData/></source>\
            </disk><disk name='sda' snapshot='no'/></disks>\
            <domain><devices><disk type='file'><source file='/p/web.qcow2'/></disk></devices></domain>\
            </domainsnapshot>";
        assert_eq!(snapshot_overlays(xml), [PathBuf::from("/p/web.s1.qcow2")]);
        assert_eq!(snapshot_overlays("<domainsnapshot><disks><disk name='vda' snapshot='external'/></disks></domainsnapshot>").len(), 0);
    }
}
