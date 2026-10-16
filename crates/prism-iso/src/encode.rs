//! Byte-level encoders for volume descriptors, path tables and directories.

use crate::SECTOR;
use crate::builder::Entry;
use crate::names::ucs2;
use crate::timestamp::Timestamp;

pub(crate) const PVD_LBA: u32 = 16;
pub(crate) const PATH_TABLES_LBA: u32 = 19;
pub(crate) const FIRST_DIR_LBA: u32 = 23;
/// Path table holding only the root: 8-byte header, 1-byte name, 1 pad.
const PATH_TABLE_LEN: u32 = 10;
/// Joliet UCS-2 level 3.
const JOLIET_ESCAPE: &[u8; 3] = b"%/E";
const APPLICATION_ID: &str = "PRISM-ISO";

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

pub(crate) fn dir_record(name: &[u8], lba: u32, len: u32, flags: u8, time: &Timestamp) -> Vec<u8> {
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
pub(crate) fn directory(
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

pub(crate) fn path_table(root_lba: u32, big_endian: bool) -> [u8; PATH_TABLE_LEN as usize] {
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
pub(crate) struct Descriptor<'a> {
    pub(crate) joliet: bool,
    pub(crate) volume_id: &'a str,
    pub(crate) total_sectors: u32,
    pub(crate) l_path_table: u32,
    pub(crate) m_path_table: u32,
    pub(crate) root_lba: u32,
    pub(crate) root_len: u32,
    pub(crate) time: &'a Timestamp,
}

impl Descriptor<'_> {
    pub(crate) fn encode(&self) -> [u8; SECTOR] {
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
