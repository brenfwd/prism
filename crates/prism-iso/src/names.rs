//! File name validation and the mapping to ISO 9660 and Joliet identifiers.

use std::collections::BTreeSet;

use crate::Error;

/// Joliet allows 64 UCS-2 characters per name.
const JOLIET_NAME_MAX: usize = 64;
/// ISO 9660 level 2 allows 31 bytes; keep room for the `;1` version.
const PRIMARY_NAME_MAX: usize = 30;

/// Checks a volume label: 1 to 16 bytes from `A-Z`, `0-9` and `_`.
pub(crate) fn validate_volume_id(id: &str) -> Result<(), Error> {
    let ok = (1..=16).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(Error::VolumeId { id: id.to_owned() })
    }
}

/// Checks a file name against the Joliet limits: 1 to 64 UTF-16 units from the
/// Basic Multilingual Plane, no control characters and none of `* / : ; ? \`.
pub(crate) fn validate_file_name(name: &str) -> Result<(), Error> {
    let err = |reason| {
        Err(Error::FileName {
            name: name.to_owned(),
            reason,
        })
    };
    if name.is_empty() || name == "." || name == ".." {
        return err("empty or a dot entry");
    }
    if name.encode_utf16().count() > JOLIET_NAME_MAX {
        return err("longer than 64 characters");
    }
    if name.chars().any(|c| u32::from(c) > 0xffff) {
        return err("characters outside the Basic Multilingual Plane are not supported");
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '*' | '/' | ':' | ';' | '?' | '\\'))
    {
        return err("contains a control character or one of * / : ; ? \\");
    }
    Ok(())
}

/// Maps a name to an ISO 9660 level 2 file identifier: d-characters, one
/// dot, and `;1`. A name that collides with one in `taken` gets a numeric
/// suffix (`A_B_1.;1`); the extension keeps at most 15 characters.
pub(crate) fn primary_name(name: &str, taken: &BTreeSet<Vec<u8>>) -> Vec<u8> {
    let map = |s: &str| -> String {
        s.chars()
            .map(|c| {
                let c = c.to_ascii_uppercase();
                if c.is_ascii_uppercase() || c.is_ascii_digit() {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) if !b.is_empty() => (map(b), map(e)),
        _ => (map(name), String::new()),
    };
    let ext: String = ext.chars().take(PRIMARY_NAME_MAX / 2).collect();
    let room = PRIMARY_NAME_MAX - 1 - ext.len();
    for n in 0u32.. {
        let suffix = if n == 0 {
            String::new()
        } else {
            format!("_{n}")
        };
        let keep = room.saturating_sub(suffix.len());
        let base: String = base.chars().take(keep).collect();
        let id = format!("{base}{suffix}.{ext};1").into_bytes();
        if !taken.contains(&id) {
            return id;
        }
    }
    unreachable!("ran out of suffixes")
}

/// Encodes `s` as big-endian UTF-16, the Joliet name encoding.
pub(crate) fn ucs2(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_be_bytes).collect()
}
