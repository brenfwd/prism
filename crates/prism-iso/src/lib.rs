//! A small ISO 9660 image writer with Joliet extensions.
//!
//! It writes a single root directory of small files, which is all a
//! cloud-init `NoCloud` seed needs:
//!
//! ```
//! let iso = prism_iso::IsoBuilder::new("CIDATA")
//!     .file("user-data", "#cloud-config\n")
//!     .file("meta-data", "instance-id: tb-1\n")
//!     .build()
//!     .unwrap();
//! assert_eq!(iso.len() % prism_iso::SECTOR, 0);
//! ```
//!
//! The image holds a primary volume descriptor with uppercase ISO 9660
//! names (`USER_DATA.;1`), and a Joliet supplementary volume descriptor
//! whose directory carries the original names (`user-data`) in UCS-2.
//! Linux, Windows and libarchive read the Joliet tree when present.
//!
//! Layout, in 2048-byte sectors:
//!
//! | sector | contents |
//! |---|---|
//! | 0–15 | system area (zeros) |
//! | 16 | primary volume descriptor |
//! | 17 | Joliet supplementary volume descriptor |
//! | 18 | volume descriptor set terminator |
//! | 19–22 | path tables: primary L, primary M, Joliet L, Joliet M |
//! | 23… | primary root directory, Joliet root directory, file data |

use std::collections::BTreeSet;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

/// Logical sector and block size.
pub const SECTOR: usize = 2048;

const PVD_LBA: u32 = 16;
const PATH_TABLES_LBA: u32 = 19;
const FIRST_DIR_LBA: u32 = 23;
/// Path table holding only the root: 8-byte header, 1-byte name, 1 pad.
const PATH_TABLE_LEN: u32 = 10;
/// Joliet UCS-2 level 3.
const JOLIET_ESCAPE: &[u8; 3] = b"%/E";
const APPLICATION_ID: &str = "PRISM-ISO";
/// Joliet allows 64 UCS-2 characters per name.
const JOLIET_NAME_MAX: usize = 64;
/// ISO 9660 level 2 allows 31 bytes; keep room for the `;1` version.
const PRIMARY_NAME_MAX: usize = 30;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "invalid volume identifier {0:?}: use 1-16 characters from A-Z, 0-9 and _ (for example CIDATA)"
    )]
    VolumeId(String),
    #[error("invalid file name {0:?}: {1}")]
    FileName(String, &'static str),
    #[error("duplicate file name {0:?}")]
    Duplicate(String),
    #[error("image too large")]
    TooLarge,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Builds an ISO image with files in the root directory.
#[derive(Debug, Clone)]
pub struct IsoBuilder {
    volume_id: String,
    files: Vec<(String, Vec<u8>)>,
    timestamp: SystemTime,
}

impl IsoBuilder {
    /// Starts an image with the given volume identifier (label).
    ///
    /// The label must be 1–16 characters from `A-Z`, `0-9` and `_`, so the
    /// same string fits both the primary (d-characters, 32 bytes) and the
    /// Joliet (UCS-2, 16 characters) descriptors.
    pub fn new(volume_id: impl Into<String>) -> Self {
        Self {
            volume_id: volume_id.into(),
            files: Vec::new(),
            timestamp: SystemTime::now(),
        }
    }

    /// Adds a file to the root directory.
    #[must_use]
    pub fn file(mut self, name: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        self.files.push((name.into(), data.into()));
        self
    }

    /// Sets the timestamp recorded for the volume and every file.
    /// Defaults to the time [`IsoBuilder::new`] was called.
    #[must_use]
    pub fn timestamp(mut self, t: SystemTime) -> Self {
        self.timestamp = t;
        self
    }

    /// Writes the image to `w`.
    pub fn write<W: Write>(&self, mut w: W) -> Result<(), Error> {
        w.write_all(&self.build()?)?;
        w.flush()?;
        Ok(())
    }

    /// Builds the whole image in memory.
    pub fn build(&self) -> Result<Vec<u8>, Error> {
        validate_volume_id(&self.volume_id)?;
        let time = Timestamp::from_system_time(self.timestamp);
        let entries = self.entries()?;

        // Directory sizes do not depend on extent locations, so size them
        // first with placeholder locations.
        let primary_order = sorted_by(&entries, |e| e.primary.clone());
        let joliet_order = sorted_by(&entries, |e| e.joliet.clone());
        let dir_len =
            |order: &[usize], joliet: bool| directory(0, 0, &entries, order, joliet, &time).len();
        let primary_dir_len = dir_len(&primary_order, false);
        let joliet_dir_len = dir_len(&joliet_order, true);

        let primary_dir_lba = FIRST_DIR_LBA;
        let joliet_dir_lba = primary_dir_lba + sectors(primary_dir_len)?;
        let mut next = joliet_dir_lba + sectors(joliet_dir_len)?;
        let mut entries = entries;
        for e in &mut entries {
            e.lba = next;
            next = next
                .checked_add(sectors(e.data.len())?)
                .ok_or(Error::TooLarge)?;
        }
        let total = next;

        let primary_len = u32::try_from(primary_dir_len).map_err(|_| Error::TooLarge)?;
        let joliet_len = u32::try_from(joliet_dir_len).map_err(|_| Error::TooLarge)?;
        let image_len = total as usize * SECTOR;
        let mut img = vec![0u8; image_len];

        let pvd = Descriptor {
            joliet: false,
            volume_id: &self.volume_id,
            total_sectors: total,
            l_path_table: PATH_TABLES_LBA,
            m_path_table: PATH_TABLES_LBA + 1,
            root_lba: primary_dir_lba,
            root_len: primary_len,
            time: &time,
        };
        let svd = Descriptor {
            joliet: true,
            l_path_table: PATH_TABLES_LBA + 2,
            m_path_table: PATH_TABLES_LBA + 3,
            root_lba: joliet_dir_lba,
            root_len: joliet_len,
            ..pvd
        };
        sector_mut(&mut img, PVD_LBA).copy_from_slice(&pvd.encode());
        sector_mut(&mut img, PVD_LBA + 1).copy_from_slice(&svd.encode());
        sector_mut(&mut img, PVD_LBA + 2)[..7].copy_from_slice(b"\xffCD001\x01");

        for (i, (dir_lba, big_endian)) in [
            (primary_dir_lba, false),
            (primary_dir_lba, true),
            (joliet_dir_lba, false),
            (joliet_dir_lba, true),
        ]
        .into_iter()
        .enumerate()
        {
            let table = path_table(dir_lba, big_endian);
            sector_mut(&mut img, PATH_TABLES_LBA + i as u32)[..table.len()].copy_from_slice(&table);
        }

        for (lba, len, order, joliet) in [
            (primary_dir_lba, primary_len, &primary_order, false),
            (joliet_dir_lba, joliet_len, &joliet_order, true),
        ] {
            let dir = directory(lba, len, &entries, order, joliet, &time);
            let start = lba as usize * SECTOR;
            img[start..start + dir.len()].copy_from_slice(&dir);
        }

        for e in &entries {
            let start = e.lba as usize * SECTOR;
            img[start..start + e.data.len()].copy_from_slice(e.data);
        }
        Ok(img)
    }

    fn entries(&self) -> Result<Vec<Entry<'_>>, Error> {
        let mut seen = BTreeSet::new();
        let mut primary_seen = BTreeSet::new();
        let mut out = Vec::with_capacity(self.files.len());
        for (name, data) in &self.files {
            validate_file_name(name)?;
            if !seen.insert(name.as_str()) {
                return Err(Error::Duplicate(name.clone()));
            }
            u32::try_from(data.len()).map_err(|_| Error::TooLarge)?;
            let primary = primary_name(name, &primary_seen);
            primary_seen.insert(primary.clone());
            out.push(Entry {
                primary,
                joliet: ucs2(name),
                data,
                lba: 0,
            });
        }
        Ok(out)
    }
}

struct Entry<'a> {
    /// ISO 9660 identifier including `;1`.
    primary: Vec<u8>,
    /// UCS-2BE Joliet identifier.
    joliet: Vec<u8>,
    data: &'a [u8],
    lba: u32,
}

fn sorted_by(entries: &[Entry<'_>], key: impl Fn(&Entry<'_>) -> Vec<u8>) -> Vec<usize> {
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| key(&entries[i]));
    order
}

fn sectors(len: usize) -> Result<u32, Error> {
    u32::try_from(len.div_ceil(SECTOR)).map_err(|_| Error::TooLarge)
}

fn sector_mut(img: &mut [u8], lba: u32) -> &mut [u8] {
    let start = lba as usize * SECTOR;
    &mut img[start..start + SECTOR]
}

fn validate_volume_id(id: &str) -> Result<(), Error> {
    let ok = (1..=16).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(Error::VolumeId(id.to_owned()))
    }
}

fn validate_file_name(name: &str) -> Result<(), Error> {
    let err = |why| Err(Error::FileName(name.to_owned(), why));
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
/// dot, and `;1`. Collisions get a numeric suffix.
fn primary_name(name: &str, taken: &BTreeSet<Vec<u8>>) -> Vec<u8> {
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

fn ucs2(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

/// A date and time in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Timestamp {
    year: i64,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

impl Timestamp {
    fn from_system_time(t: SystemTime) -> Self {
        let secs = t
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        // Howard Hinnant's civil_from_days.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Self {
            year,
            month: month as u8,
            day: day as u8,
            hour: (rem / 3600) as u8,
            minute: (rem / 60 % 60) as u8,
            second: (rem % 60) as u8,
        }
    }

    /// The 7-byte directory record form (years since 1900, UTC).
    fn record(&self) -> [u8; 7] {
        let year = (self.year - 1900).clamp(0, 255) as u8;
        [
            year,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second,
            0,
        ]
    }

    /// The 17-byte volume descriptor form (`YYYYMMDDHHMMSScc` + offset).
    fn descriptor(&self) -> [u8; 17] {
        let s = format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}00",
            self.year.clamp(0, 9999),
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second
        );
        let mut out = [0u8; 17];
        out[..16].copy_from_slice(s.as_bytes());
        out
    }
}

/// An unset descriptor date: sixteen ASCII zeros and a zero offset.
const NO_DATE: [u8; 17] = *b"0000000000000000\0";

fn both16(buf: &mut [u8], v: u16) {
    buf[..2].copy_from_slice(&v.to_le_bytes());
    buf[2..4].copy_from_slice(&v.to_be_bytes());
}

fn both32(buf: &mut [u8], v: u32) {
    buf[..4].copy_from_slice(&v.to_le_bytes());
    buf[4..8].copy_from_slice(&v.to_be_bytes());
}

/// Fills a text field, padding with spaces (UCS-2 spaces for Joliet).
fn text(buf: &mut [u8], s: &str, joliet: bool) {
    if joliet {
        for pair in buf.chunks_mut(2) {
            if pair.len() == 2 {
                pair.copy_from_slice(&[0, b' ']);
            } else {
                pair[0] = 0;
            }
        }
        let enc = ucs2(s);
        buf[..enc.len()].copy_from_slice(&enc);
    } else {
        buf.fill(b' ');
        buf[..s.len()].copy_from_slice(s.as_bytes());
    }
}

fn dir_record(name: &[u8], lba: u32, len: u32, flags: u8, time: &Timestamp) -> Vec<u8> {
    let pad = usize::from(name.len().is_multiple_of(2));
    let rec_len = 33 + name.len() + pad;
    let mut r = vec![0u8; rec_len];
    r[0] = rec_len as u8;
    both32(&mut r[2..10], lba);
    both32(&mut r[10..18], len);
    r[18..25].copy_from_slice(&time.record());
    r[25] = flags;
    both16(&mut r[28..32], 1);
    r[32] = name.len() as u8;
    r[33..33 + name.len()].copy_from_slice(name);
    r
}

/// Encodes the root directory. Records never cross a sector boundary, and
/// the result is padded to whole sectors.
fn directory(
    lba: u32,
    len: u32,
    entries: &[Entry<'_>],
    order: &[usize],
    joliet: bool,
    time: &Timestamp,
) -> Vec<u8> {
    const DIRECTORY: u8 = 0x02;
    let mut records = vec![
        dir_record(&[0], lba, len, DIRECTORY, time),
        dir_record(&[1], lba, len, DIRECTORY, time),
    ];
    for &i in order {
        let e = &entries[i];
        let name = if joliet { &e.joliet } else { &e.primary };
        records.push(dir_record(name, e.lba, e.data.len() as u32, 0, time));
    }
    let mut out = Vec::new();
    for r in records {
        let used = out.len() % SECTOR;
        if used + r.len() > SECTOR {
            out.resize(out.len() + SECTOR - used, 0);
        }
        out.extend_from_slice(&r);
    }
    out.resize(out.len().div_ceil(SECTOR) * SECTOR, 0);
    out
}

fn path_table(root_lba: u32, big_endian: bool) -> [u8; PATH_TABLE_LEN as usize] {
    let mut t = [0u8; PATH_TABLE_LEN as usize];
    t[0] = 1; // identifier length
    if big_endian {
        t[2..6].copy_from_slice(&root_lba.to_be_bytes());
        t[6..8].copy_from_slice(&1u16.to_be_bytes());
    } else {
        t[2..6].copy_from_slice(&root_lba.to_le_bytes());
        t[6..8].copy_from_slice(&1u16.to_le_bytes());
    }
    // t[8] = 0: the root's identifier; t[9]: padding.
    t
}

#[derive(Clone, Copy)]
struct Descriptor<'a> {
    joliet: bool,
    volume_id: &'a str,
    total_sectors: u32,
    l_path_table: u32,
    m_path_table: u32,
    root_lba: u32,
    root_len: u32,
    time: &'a Timestamp,
}

impl Descriptor<'_> {
    fn encode(&self) -> [u8; SECTOR] {
        let j = self.joliet;
        let mut d = [0u8; SECTOR];
        d[0] = if j { 2 } else { 1 };
        d[1..6].copy_from_slice(b"CD001");
        d[6] = 1;
        text(&mut d[8..40], "", j); // system identifier
        text(&mut d[40..72], self.volume_id, j);
        both32(&mut d[80..88], self.total_sectors);
        if j {
            d[88..91].copy_from_slice(JOLIET_ESCAPE);
        }
        both16(&mut d[120..124], 1); // volume set size
        both16(&mut d[124..128], 1); // volume sequence number
        both16(&mut d[128..132], SECTOR as u16);
        both32(&mut d[132..140], PATH_TABLE_LEN);
        d[140..144].copy_from_slice(&self.l_path_table.to_le_bytes());
        d[148..152].copy_from_slice(&self.m_path_table.to_be_bytes());
        let root = dir_record(&[0], self.root_lba, self.root_len, 0x02, self.time);
        d[156..190].copy_from_slice(&root);
        text(&mut d[190..318], "", j); // volume set
        text(&mut d[318..446], "", j); // publisher
        text(&mut d[446..574], "", j); // data preparer
        text(&mut d[574..702], APPLICATION_ID, j);
        text(&mut d[702..739], "", j); // copyright file
        text(&mut d[739..776], "", j); // abstract file
        text(&mut d[776..813], "", j); // bibliographic file
        let now = self.time.descriptor();
        d[813..830].copy_from_slice(&now); // creation
        d[830..847].copy_from_slice(&now); // modification
        d[847..864].copy_from_slice(&NO_DATE); // expiration
        d[864..881].copy_from_slice(&NO_DATE); // effective
        d[881] = 1; // file structure version
        d
    }
}

#[cfg(test)]
mod tests;
