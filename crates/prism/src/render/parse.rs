//! Read-only queries on libvirt domain XML: disk sources, the VNC socket
//! and the expanded machine type.

use std::path::PathBuf;

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
