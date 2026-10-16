//! Rectangle decoders for Hextile, ZRLE and Tight. Each works on a complete
//! unit of input (one Hextile tile, one ZRLE or Tight rectangle) so the
//! streaming state machine only has to find unit boundaries.

mod hextile;
mod tight;
mod zrle;

use crate::fb::Rect;
use crate::{Error, ErrorKind};

pub use hextile::{HextileColours, hextile_decode, hextile_tile, hextile_tile_len};
pub use tight::{TightBody, tight_basic_decode, tight_parse};
pub use zrle::{zrle_decode, zrle_limit};

/// Bounds-checked reader over a byte slice.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len());
        let end =
            end.ok_or_else(|| Error::new(ErrorKind::Truncated, "truncated rectangle data"))?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    /// A 24-bit R, G, B pixel (ZRLE `CPIXEL`, Tight `TPIXEL`).
    pub fn rgb(&mut self) -> Result<[u8; 4], Error> {
        let b = self.take(3)?;
        Ok([b[0], b[1], b[2], 255])
    }

    /// A full 32-bit pixel in the negotiated format.
    pub fn rgbx(&mut self) -> Result<[u8; 4], Error> {
        let b = self.take(4)?;
        Ok([b[0], b[1], b[2], 255])
    }
}

/// Expands `bits`-per-pixel palette indices (rows padded to bytes, most
/// significant bits first) into RGBA in `out`.
fn unpack_indexed(
    packed: &[u8],
    row_bytes: usize,
    t: Rect,
    bits: usize,
    palette: &[[u8; 4]],
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    out.clear();
    let mask = u8::MAX >> (8 - bits);
    for row in packed.chunks_exact(row_bytes).take(usize::from(t.h)) {
        for x in 0..usize::from(t.w) {
            let bit = x * bits;
            let idx = (row[bit / 8] >> (8 - bits - bit % 8)) & mask;
            let px = palette
                .get(usize::from(idx))
                .ok_or_else(|| Error::new(ErrorKind::OutOfRange, "palette index out of range"))?;
            out.extend_from_slice(px);
        }
    }
    Ok(())
}

#[cfg(test)]
fn test_fb(w: u16, h: u16) -> crate::fb::Framebuffer {
    let mut fb = crate::fb::Framebuffer::default();
    fb.resize(w, h);
    fb
}
