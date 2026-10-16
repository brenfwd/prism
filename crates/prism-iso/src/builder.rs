//! The [`IsoBuilder`] type: file validation, sector layout and image assembly.

use std::collections::BTreeSet;
use std::io::Write;
use std::time::SystemTime;

use crate::encode::{Descriptor, FIRST_DIR_LBA, PATH_TABLES_LBA, PVD_LBA, directory, path_table};
use crate::names::{primary_name, ucs2, validate_file_name, validate_volume_id};
use crate::timestamp::Timestamp;
use crate::{Error, SECTOR};

/// Builds an ISO image with files in the root directory.
///
/// Files are kept in the order added. Names must be unique, at most 64
/// UTF-16 units, and free of control characters and `* / : ; ? \`.
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
    /// Joliet (UCS-2, 16 characters) descriptors. [`IsoBuilder::build`] and
    /// [`IsoBuilder::write`] return [`Error::VolumeId`] for any other label.
    pub fn new(volume_id: impl Into<String>) -> Self {
        Self {
            volume_id: volume_id.into(),
            files: Vec::new(),
            timestamp: SystemTime::now(),
        }
    }

    /// Adds a file to the root directory.
    ///
    /// The name and contents are copied into the builder when they are
    /// borrowed, and moved when they are owned. A bad or repeated name is
    /// reported when the image is built.
    #[must_use]
    pub fn file(mut self, name: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        self.files.push((name.into(), data.into()));
        self
    }

    /// Sets the timestamp recorded for the volume and every file.
    /// Defaults to the time [`IsoBuilder::new`] was called. Times before
    /// 1970 are recorded as 1970, and years are clamped to 1900–2155 in
    /// directory records and 0–9999 in descriptors.
    #[must_use]
    pub fn timestamp(mut self, t: SystemTime) -> Self {
        self.timestamp = t;
        self
    }

    /// Builds the image and writes it to `w`, then flushes `w`.
    ///
    /// Fails with the errors of [`IsoBuilder::build`], or [`Error::Io`] when
    /// writing fails.
    pub fn write<W: Write>(&self, mut w: W) -> Result<(), Error> {
        w.write_all(&self.build()?)?;
        w.flush()?;
        Ok(())
    }

    /// Builds the whole image in memory.
    ///
    /// The result is a whole number of [`SECTOR`]-sized sectors. Fails with
    /// [`Error::VolumeId`], [`Error::FileName`] or [`Error::Duplicate`] for
    /// bad input, and [`Error::TooLarge`] when a file or the image exceeds
    /// 4 GiB.
    pub fn build(&self) -> Result<Vec<u8>, Error> {
        validate_volume_id(&self.volume_id)?;
        let time = Timestamp::from_system_time(self.timestamp);
        let mut entries = self.entries()?;
        let layout = lay_out(&mut entries, &time)?;
        Ok(self.write_sectors(&entries, &layout, &time))
    }

    /// Validates the files and derives their primary and Joliet names.
    fn entries(&self) -> Result<Vec<Entry<'_>>, Error> {
        let mut seen = BTreeSet::new();
        let mut primary_seen = BTreeSet::new();
        let mut out = Vec::with_capacity(self.files.len());
        for (name, data) in &self.files {
            validate_file_name(name)?;
            if !seen.insert(name.as_str()) {
                return Err(Error::Duplicate { name: name.clone() });
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

    /// Encodes every sector of the image from a finished layout.
    fn write_sectors(&self, entries: &[Entry<'_>], layout: &Layout, time: &Timestamp) -> Vec<u8> {
        let mut img = vec![0u8; layout.total as usize * SECTOR];

        let pvd = Descriptor {
            joliet: false,
            volume_id: &self.volume_id,
            total_sectors: layout.total,
            l_path_table: PATH_TABLES_LBA,
            m_path_table: PATH_TABLES_LBA + 1,
            root_lba: layout.primary_dir_lba,
            root_len: layout.primary_len,
            time,
        };
        let svd = Descriptor {
            joliet: true,
            l_path_table: PATH_TABLES_LBA + 2,
            m_path_table: PATH_TABLES_LBA + 3,
            root_lba: layout.joliet_dir_lba,
            root_len: layout.joliet_len,
            ..pvd
        };
        sector_mut(&mut img, PVD_LBA).copy_from_slice(&pvd.encode());
        sector_mut(&mut img, PVD_LBA + 1).copy_from_slice(&svd.encode());
        sector_mut(&mut img, PVD_LBA + 2)[..7].copy_from_slice(b"\xffCD001\x01");

        for (i, (dir_lba, big_endian)) in [
            (layout.primary_dir_lba, false),
            (layout.primary_dir_lba, true),
            (layout.joliet_dir_lba, false),
            (layout.joliet_dir_lba, true),
        ]
        .into_iter()
        .enumerate()
        {
            let table = path_table(dir_lba, big_endian);
            sector_mut(&mut img, PATH_TABLES_LBA + i as u32)[..table.len()].copy_from_slice(&table);
        }

        for (lba, len, order, joliet) in [
            (
                layout.primary_dir_lba,
                layout.primary_len,
                &layout.primary_order,
                false,
            ),
            (
                layout.joliet_dir_lba,
                layout.joliet_len,
                &layout.joliet_order,
                true,
            ),
        ] {
            let dir = directory(lba, len, entries, order, joliet, time);
            let start = lba as usize * SECTOR;
            img[start..start + dir.len()].copy_from_slice(&dir);
        }

        for e in entries {
            let start = e.lba as usize * SECTOR;
            img[start..start + e.data.len()].copy_from_slice(e.data);
        }
        img
    }
}

/// One file with both of its identifiers and its extent location.
pub(crate) struct Entry<'a> {
    /// ISO 9660 identifier including `;1`.
    pub(crate) primary: Vec<u8>,
    /// UCS-2BE Joliet identifier.
    pub(crate) joliet: Vec<u8>,
    pub(crate) data: &'a [u8],
    /// First sector of the file data, set by [`lay_out`].
    pub(crate) lba: u32,
}

/// Sector positions and directory sizes for one image.
struct Layout {
    primary_dir_lba: u32,
    joliet_dir_lba: u32,
    primary_len: u32,
    joliet_len: u32,
    /// Entry indexes in primary directory order.
    primary_order: Vec<usize>,
    /// Entry indexes in Joliet directory order.
    joliet_order: Vec<usize>,
    /// Total image size in sectors.
    total: u32,
}

/// Assigns each entry its first sector and sizes both directories.
fn lay_out(entries: &mut [Entry<'_>], time: &Timestamp) -> Result<Layout, Error> {
    // Directory sizes do not depend on extent locations, so size them
    // first with placeholder locations.
    let primary_order = sorted_by(entries, |e| e.primary.clone());
    let joliet_order = sorted_by(entries, |e| e.joliet.clone());
    let primary_dir_len = directory(0, 0, entries, &primary_order, false, time).len();
    let joliet_dir_len = directory(0, 0, entries, &joliet_order, true, time).len();

    let primary_dir_lba = FIRST_DIR_LBA;
    let joliet_dir_lba = primary_dir_lba + sectors(primary_dir_len)?;
    let mut next = joliet_dir_lba + sectors(joliet_dir_len)?;
    for e in entries.iter_mut() {
        e.lba = next;
        next = next
            .checked_add(sectors(e.data.len())?)
            .ok_or(Error::TooLarge)?;
    }
    Ok(Layout {
        primary_dir_lba,
        joliet_dir_lba,
        primary_len: u32::try_from(primary_dir_len).map_err(|_| Error::TooLarge)?,
        joliet_len: u32::try_from(joliet_dir_len).map_err(|_| Error::TooLarge)?,
        primary_order,
        joliet_order,
        total: next,
    })
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
