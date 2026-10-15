//! Rectangle decoders for Hextile, ZRLE and Tight. Each works on a complete
//! unit of input (one Hextile tile, one ZRLE or Tight rectangle) so the
//! streaming state machine only has to find unit boundaries.

use crate::Error;
use crate::fb::{BLACK, Framebuffer, Rect};

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
        let end = end.ok_or_else(|| Error::new("truncated rectangle data"))?;
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

// ---------------------------------------------------------------- Hextile

const HEXTILE_RAW: u8 = 1;
const HEXTILE_BG: u8 = 2;
const HEXTILE_FG: u8 = 4;
const HEXTILE_ANY_SUBRECTS: u8 = 8;
const HEXTILE_COLOURED: u8 = 16;

/// Background and foreground carried from tile to tile within a rectangle.
#[derive(Clone, Copy, Debug)]
pub struct HextileColours {
    pub bg: [u8; 4],
    pub fg: [u8; 4],
}

impl Default for HextileColours {
    fn default() -> Self {
        Self {
            bg: BLACK,
            fg: BLACK,
        }
    }
}

/// The tile with index `index` of a Hextile rectangle (16x16, row-major).
pub fn hextile_tile(r: Rect, index: u32) -> Option<Rect> {
    let cols = u32::from(r.w).div_ceil(16);
    let rows = u32::from(r.h).div_ceil(16);
    if index >= cols * rows {
        return None;
    }
    let tx = (index % cols) * 16;
    let ty = (index / cols) * 16;
    Some(Rect::new(
        r.x + tx as u16,
        r.y + ty as u16,
        (u32::from(r.w) - tx).min(16) as u16,
        (u32::from(r.h) - ty).min(16) as u16,
    ))
}

/// Length of the encoded tile at the start of `input`, or `None` if more
/// bytes are needed to tell.
pub fn hextile_tile_len(input: &[u8], tile: Rect) -> Option<usize> {
    let mask = *input.first()?;
    if mask & HEXTILE_RAW != 0 {
        return Some(1 + tile.area() * 4);
    }
    let mut len = 1;
    if mask & HEXTILE_BG != 0 {
        len += 4;
    }
    if mask & HEXTILE_FG != 0 {
        len += 4;
    }
    if mask & HEXTILE_ANY_SUBRECTS != 0 {
        let n = usize::from(*input.get(len)?);
        len += 1 + n * if mask & HEXTILE_COLOURED != 0 { 6 } else { 2 };
    }
    Some(len)
}

/// Decodes one complete tile (as sized by [`hextile_tile_len`]).
pub fn hextile_decode(
    fb: &mut Framebuffer,
    tile: Rect,
    data: &[u8],
    c: &mut HextileColours,
) -> Result<(), Error> {
    let mut rd = Reader::new(data);
    let mask = rd.u8()?;
    if mask & HEXTILE_RAW != 0 {
        fb.put_rgbx_rows(tile, 0, tile.h, rd.take(tile.area() * 4)?);
        return Ok(());
    }
    if mask & HEXTILE_BG != 0 {
        c.bg = rd.rgbx()?;
    }
    if mask & HEXTILE_FG != 0 {
        c.fg = rd.rgbx()?;
    }
    fb.fill(tile, c.bg);
    if mask & HEXTILE_ANY_SUBRECTS != 0 {
        let n = rd.u8()?;
        for _ in 0..n {
            let px = if mask & HEXTILE_COLOURED != 0 {
                rd.rgbx()?
            } else {
                c.fg
            };
            let xy = rd.u8()?;
            let wh = rd.u8()?;
            let (sx, sy) = (u16::from(xy >> 4), u16::from(xy & 15));
            let (sw, sh) = (u16::from(wh >> 4) + 1, u16::from(wh & 15) + 1);
            if sx + sw > tile.w || sy + sh > tile.h {
                return Err(Error::new("hextile subrectangle outside its tile"));
            }
            fb.fill(Rect::new(tile.x + sx, tile.y + sy, sw, sh), px);
        }
    }
    Ok(())
}

// ------------------------------------------------------------------- ZRLE

/// Upper bound on the decompressed size of a ZRLE rectangle.
pub fn zrle_limit(r: Rect) -> usize {
    let tiles = usize::from(r.w).div_ceil(64) * usize::from(r.h).div_ceil(64);
    // Worst case is plain RLE with one-pixel runs: 4 bytes per pixel, plus
    // a subencoding byte and a 127-colour palette per tile.
    r.area() * 4 + tiles * (1 + 127 * 3) + 16
}

fn run_length(rd: &mut Reader<'_>) -> Result<usize, Error> {
    let mut len = 1usize;
    loop {
        let b = rd.u8()?;
        len += usize::from(b);
        if b != 255 {
            return Ok(len);
        }
    }
}

/// Decodes a whole (decompressed) ZRLE rectangle.
pub fn zrle_decode(
    fb: &mut Framebuffer,
    r: Rect,
    data: &[u8],
    tile_buf: &mut Vec<u8>,
) -> Result<(), Error> {
    let mut rd = Reader::new(data);
    for ty in (0..r.h).step_by(64) {
        for tx in (0..r.w).step_by(64) {
            let t = Rect::new(r.x + tx, r.y + ty, (r.w - tx).min(64), (r.h - ty).min(64));
            zrle_tile(fb, t, &mut rd, tile_buf)?;
        }
    }
    Ok(())
}

fn zrle_tile(
    fb: &mut Framebuffer,
    t: Rect,
    rd: &mut Reader<'_>,
    buf: &mut Vec<u8>,
) -> Result<(), Error> {
    let sub = rd.u8()?;
    let n = t.area();
    match sub {
        0 => fb.put_rgb(t, rd.take(n * 3)?),
        1 => fb.fill(t, rd.rgb()?),
        2..=16 => {
            let palette = read_palette(rd, usize::from(sub))?;
            let bits = match sub {
                2 => 1,
                3 | 4 => 2,
                _ => 4,
            };
            let row_bytes = (usize::from(t.w) * bits).div_ceil(8);
            let packed = rd.take(row_bytes * usize::from(t.h))?;
            unpack_indexed(packed, row_bytes, t, bits, &palette, buf)?;
            fb.put_rgba(t, buf);
        }
        128 => {
            buf.clear();
            while buf.len() < n * 4 {
                let px = rd.rgb()?;
                let run = run_length(rd)?;
                push_run(buf, px, run, n)?;
            }
            fb.put_rgba(t, buf);
        }
        130..=255 => {
            let palette = read_palette(rd, usize::from(sub - 128))?;
            buf.clear();
            while buf.len() < n * 4 {
                let b = rd.u8()?;
                let px = *palette
                    .get(usize::from(b & 127))
                    .ok_or_else(|| Error::new("ZRLE palette index out of range"))?;
                let run = if b & 128 != 0 { run_length(rd)? } else { 1 };
                push_run(buf, px, run, n)?;
            }
            fb.put_rgba(t, buf);
        }
        _ => return Err(Error::new(format!("invalid ZRLE subencoding {sub}"))),
    }
    Ok(())
}

fn push_run(buf: &mut Vec<u8>, px: [u8; 4], run: usize, total: usize) -> Result<(), Error> {
    if buf.len() / 4 + run > total {
        return Err(Error::new("ZRLE run overflows its tile"));
    }
    for _ in 0..run {
        buf.extend_from_slice(&px);
    }
    Ok(())
}

fn read_palette(rd: &mut Reader<'_>, n: usize) -> Result<Vec<[u8; 4]>, Error> {
    (0..n).map(|_| rd.rgb()).collect()
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
                .ok_or_else(|| Error::new("palette index out of range"))?;
            out.extend_from_slice(px);
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ Tight

/// Data smaller than this is sent uncompressed in Tight basic rectangles.
const TIGHT_MIN_TO_COMPRESS: usize = 12;

/// Tight's per-rectangle filter.
#[derive(Debug, PartialEq, Eq)]
pub enum TightFilter<'a> {
    Copy,
    /// Palette colours as packed R, G, B.
    Palette(&'a [u8]),
    Gradient,
}

/// A fully buffered Tight rectangle.
#[derive(Debug, PartialEq, Eq)]
pub enum TightBody<'a> {
    Fill([u8; 4]),
    Jpeg(&'a [u8]),
    Png(&'a [u8]),
    Basic {
        stream: usize,
        filter: TightFilter<'a>,
        /// Uncompressed size after the filter's input format.
        raw_len: usize,
        data: &'a [u8],
        compressed: bool,
    },
}

/// A parsed Tight rectangle: stream-reset bits plus body.
#[derive(Debug, PartialEq, Eq)]
pub struct TightRect<'a> {
    pub resets: u8,
    pub body: TightBody<'a>,
}

/// Reads Tight's 1-3 byte compact length at `input[at..]`, returning the
/// length and the bytes it occupied.
fn compact_len(input: &[u8], at: usize) -> Option<(usize, usize)> {
    let b0 = *input.get(at)?;
    let mut len = usize::from(b0 & 0x7f);
    if b0 & 0x80 == 0 {
        return Some((len, 1));
    }
    let b1 = *input.get(at + 1)?;
    len |= usize::from(b1 & 0x7f) << 7;
    if b1 & 0x80 == 0 {
        return Some((len, 2));
    }
    let b2 = *input.get(at + 2)?;
    Some((len | usize::from(b2) << 14, 3))
}

/// Parses a Tight rectangle from the start of `input`. Returns `Ok(None)`
/// when more input is needed, else the rectangle and the bytes it used.
pub fn tight_parse(input: &[u8], rect: Rect) -> Result<Option<(TightRect<'_>, usize)>, Error> {
    let Some(&ctl) = input.first() else {
        return Ok(None);
    };
    let resets = ctl & 0x0f;
    let kind = ctl >> 4;
    let sized = |at: usize| -> Option<(&[u8], usize)> {
        let (len, n) = compact_len(input, at)?;
        let start = at + n;
        let data = input.get(start..start + len)?;
        Some((data, start + len))
    };
    let parsed = match kind {
        8 => input
            .get(1..4)
            .map(|b| (TightBody::Fill([b[0], b[1], b[2], 255]), 4)),
        9 => sized(1).map(|(d, n)| (TightBody::Jpeg(d), n)),
        10 => sized(1).map(|(d, n)| (TightBody::Png(d), n)),
        flags if flags & 8 == 0 => {
            let stream = usize::from(flags & 3);
            let mut at = 1;
            let filter = if flags & 4 != 0 {
                let Some(&f) = input.get(1) else {
                    return Ok(None);
                };
                at = 2;
                match f {
                    0 => TightFilter::Copy,
                    1 => {
                        let Some(&n) = input.get(2) else {
                            return Ok(None);
                        };
                        let colours = (usize::from(n) + 1) * 3;
                        let Some(p) = input.get(3..3 + colours) else {
                            return Ok(None);
                        };
                        at = 3 + colours;
                        TightFilter::Palette(p)
                    }
                    2 => TightFilter::Gradient,
                    _ => return Err(Error::new(format!("unknown Tight filter {f}"))),
                }
            } else {
                TightFilter::Copy
            };
            let raw_len = match filter {
                TightFilter::Palette(p) if p.len() == 6 => {
                    usize::from(rect.w).div_ceil(8) * usize::from(rect.h)
                }
                TightFilter::Palette(_) => rect.area(),
                TightFilter::Copy | TightFilter::Gradient => rect.area() * 3,
            };
            if raw_len < TIGHT_MIN_TO_COMPRESS {
                input.get(at..at + raw_len).map(|d| {
                    let body = TightBody::Basic {
                        stream,
                        filter,
                        raw_len,
                        data: d,
                        compressed: false,
                    };
                    (body, at + raw_len)
                })
            } else {
                sized(at).map(|(d, n)| {
                    let body = TightBody::Basic {
                        stream,
                        filter,
                        raw_len,
                        data: d,
                        compressed: true,
                    };
                    (body, n)
                })
            }
        }
        _ => {
            return Err(Error::new(format!(
                "invalid Tight compression control {ctl:#04x}"
            )));
        }
    };
    Ok(parsed.map(|(body, n)| (TightRect { resets, body }, n)))
}

/// Applies a Tight basic rectangle's filter to its (decompressed) `data`.
pub fn tight_basic_decode(
    fb: &mut Framebuffer,
    r: Rect,
    filter: &TightFilter<'_>,
    data: &[u8],
    buf: &mut Vec<u8>,
) -> Result<(), Error> {
    match filter {
        TightFilter::Copy => fb.put_rgb(r, data),
        TightFilter::Palette(p) => {
            let palette: Vec<[u8; 4]> = p
                .as_chunks::<3>()
                .0
                .iter()
                .map(|c| [c[0], c[1], c[2], 255])
                .collect();
            if palette.len() == 2 {
                let row_bytes = usize::from(r.w).div_ceil(8);
                unpack_indexed(data, row_bytes, r, 1, &palette, buf)?;
            } else {
                unpack_indexed(data, usize::from(r.w), r, 8, &palette, buf)?;
            }
            fb.put_rgba(r, buf);
        }
        TightFilter::Gradient => {
            gradient(data, r, buf);
            fb.put_rgba(r, buf);
        }
    }
    Ok(())
}

/// Undoes Tight's gradient filter: each component is predicted as
/// `left + above - above_left` (clamped) and the data holds the difference.
fn gradient(data: &[u8], r: Rect, out: &mut Vec<u8>) {
    let w = usize::from(r.w);
    out.clear();
    out.resize(r.area() * 4, 255);
    let mut prev = vec![[0i16; 3]; w];
    let mut cur = vec![[0i16; 3]; w];
    for (y, row) in data.chunks_exact(w * 3).take(usize::from(r.h)).enumerate() {
        for x in 0..w {
            for c in 0..3 {
                let left = if x > 0 { cur[x - 1][c] } else { 0 };
                let up_left = if x > 0 { prev[x - 1][c] } else { 0 };
                let predicted = (left + prev[x][c] - up_left).clamp(0, 255);
                let v = (i16::from(row[x * 3 + c]) + predicted) & 0xff;
                cur[x][c] = v;
                out[(y * w + x) * 4 + c] = v as u8;
            }
        }
        std::mem::swap(&mut prev, &mut cur);
    }
}

#[cfg(test)]
#[allow(clippy::many_single_char_names)]
mod tests {
    use super::*;

    fn fb(w: u16, h: u16) -> Framebuffer {
        let mut fb = Framebuffer::default();
        fb.resize(w, h);
        fb
    }

    #[test]
    fn hextile_tiles_cover_rect() {
        let r = Rect::new(10, 20, 40, 17);
        let tiles: Vec<Rect> = (0..).map_while(|i| hextile_tile(r, i)).collect();
        assert_eq!(tiles.len(), 6);
        assert_eq!(tiles[2], Rect::new(42, 20, 8, 16));
        assert_eq!(tiles[5], Rect::new(42, 36, 8, 1));
    }

    #[test]
    fn hextile_bg_fg_subrects() {
        let mut f = fb(16, 16);
        let t = Rect::new(0, 0, 16, 16);
        // bg + fg + any-subrects: bg red, fg green, one 2x3 subrect at (1,2).
        let data = [
            HEXTILE_BG | HEXTILE_FG | HEXTILE_ANY_SUBRECTS,
            255,
            0,
            0,
            0,
            0,
            255,
            0,
            0,
            1,
            0x12,
            0x12,
        ];
        assert_eq!(hextile_tile_len(&data, t), Some(data.len()));
        assert_eq!(hextile_tile_len(&data[..9], t), None);
        let mut c = HextileColours::default();
        hextile_decode(&mut f, t, &data, &mut c).unwrap();
        assert_eq!(f.pixel(0, 0), [255, 0, 0, 255]);
        assert_eq!(f.pixel(1, 2), [0, 255, 0, 255]);
        assert_eq!(f.pixel(2, 4), [0, 255, 0, 255]);
        assert_eq!(f.pixel(3, 4), [255, 0, 0, 255]);
        // A following tile with no flags reuses the background.
        let mut f2 = fb(16, 16);
        hextile_decode(&mut f2, t, &[0], &mut c).unwrap();
        assert_eq!(f2.pixel(15, 15), [255, 0, 0, 255]);
    }

    #[test]
    fn hextile_coloured_subrect_and_raw() {
        let mut f = fb(4, 2);
        let t = Rect::new(0, 0, 4, 2);
        let data = [
            HEXTILE_ANY_SUBRECTS | HEXTILE_COLOURED,
            1,
            1,
            2,
            3,
            0,
            0x01,
            0x30,
        ];
        assert_eq!(hextile_tile_len(&data, t), Some(data.len()));
        hextile_decode(&mut f, t, &data, &mut HextileColours::default()).unwrap();
        assert_eq!(f.pixel(0, 1), [1, 2, 3, 255]);
        assert_eq!(f.pixel(3, 1), [1, 2, 3, 255]);
        assert_eq!(f.pixel(0, 0), BLACK);

        let mut raw = vec![HEXTILE_RAW];
        raw.extend((0..8u8).flat_map(|i| [i, i, i, 0]));
        assert_eq!(hextile_tile_len(&raw, t), Some(33));
        hextile_decode(&mut f, t, &raw, &mut HextileColours::default()).unwrap();
        assert_eq!(f.pixel(3, 1), [7, 7, 7, 255]);
    }

    #[test]
    fn hextile_rejects_out_of_tile_subrect() {
        let mut f = fb(4, 4);
        let data = [HEXTILE_ANY_SUBRECTS, 1, 0x33, 0x11];
        assert!(
            hextile_decode(
                &mut f,
                Rect::new(0, 0, 4, 4),
                &data,
                &mut HextileColours::default()
            )
            .is_err()
        );
    }

    #[test]
    fn zrle_subencodings() {
        // 66x2 rect: two tiles (64x2 and 2x2).
        let mut f = fb(66, 2);
        let r = Rect::new(0, 0, 66, 2);
        let mut d = Vec::new();
        // Tile 1: palette RLE with two colours.
        d.extend([130, 10, 20, 30, 40, 50, 60]);
        d.extend([0x80, 99]); // colour 0, run 100
        d.extend([0x81, 26]); // colour 1, run 27
        d.push(0x00); // colour 0, run 1
        // Tile 2: packed palette, 2 colours, 1 bit: rows 0b10, 0b01.
        d.extend([2, 1, 1, 1, 2, 2, 2, 0b1000_0000, 0b0100_0000]);
        zrle_decode(&mut f, r, &d, &mut Vec::new()).unwrap();
        assert_eq!(f.pixel(0, 0), [10, 20, 30, 255]);
        assert_eq!(f.pixel(35, 1), [10, 20, 30, 255]); // pixel 99
        assert_eq!(f.pixel(36, 1), [40, 50, 60, 255]); // pixel 100
        assert_eq!(f.pixel(62, 1), [40, 50, 60, 255]); // pixel 126
        assert_eq!(f.pixel(63, 1), [10, 20, 30, 255]); // pixel 127
        assert_eq!(f.pixel(64, 0), [2, 2, 2, 255]);
        assert_eq!(f.pixel(65, 0), [1, 1, 1, 255]);
        assert_eq!(f.pixel(65, 1), [2, 2, 2, 255]);
    }

    #[test]
    fn zrle_raw_solid_plain_rle() {
        let mut f = fb(3, 1);
        zrle_decode(
            &mut f,
            Rect::new(0, 0, 3, 1),
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(f.pixel(2, 0), [7, 8, 9, 255]);
        zrle_decode(
            &mut f,
            Rect::new(0, 0, 3, 1),
            &[1, 5, 5, 5],
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(f.pixel(1, 0), [5, 5, 5, 255]);
        zrle_decode(
            &mut f,
            Rect::new(0, 0, 3, 1),
            &[128, 1, 1, 1, 1, 2, 2, 2, 0],
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(f.pixel(1, 0), [1, 1, 1, 255]);
        assert_eq!(f.pixel(2, 0), [2, 2, 2, 255]);
        // Run longer than the tile.
        assert!(
            zrle_decode(
                &mut f,
                Rect::new(0, 0, 3, 1),
                &[128, 1, 1, 1, 3],
                &mut Vec::new()
            )
            .is_err()
        );
    }

    #[test]
    fn tight_compact_len() {
        assert_eq!(compact_len(&[0x05], 0), Some((5, 1)));
        assert_eq!(compact_len(&[0x90, 0x4e], 0), Some((10000, 2)));
        assert_eq!(compact_len(&[0xff, 0xff, 0x7f], 0), Some((2_097_151, 3)));
        assert_eq!(compact_len(&[0x80], 0), None);
    }

    #[test]
    fn tight_parse_fill_jpeg_incomplete() {
        let r = Rect::new(0, 0, 8, 8);
        let (t, n) = tight_parse(&[0x81, 1, 2, 3, 99], r).unwrap().unwrap();
        assert_eq!((t.resets, n), (1, 4));
        assert_eq!(t.body, TightBody::Fill([1, 2, 3, 255]));
        let (t, n) = tight_parse(&[0x90, 2, 0xff, 0xd8], r).unwrap().unwrap();
        assert_eq!((t.body, n), (TightBody::Jpeg(&[0xff, 0xd8]), 4));
        assert!(tight_parse(&[0x90, 3, 0xff], r).unwrap().is_none());
        assert!(tight_parse(&[0xf0], r).is_err());
    }

    #[test]
    fn tight_parse_small_palette_uncompressed() {
        // 8x1 two-colour palette rect: 1 byte of indices, so sent raw.
        let r = Rect::new(0, 0, 8, 1);
        let data = [0x40 | 0x20, 1, 1, 0, 0, 0, 255, 255, 255, 0b1010_0000];
        let (t, n) = tight_parse(&data, r).unwrap().unwrap();
        assert_eq!(n, data.len());
        let TightBody::Basic {
            stream,
            filter,
            compressed,
            data: d,
            ..
        } = t.body
        else {
            panic!()
        };
        assert_eq!((stream, compressed, d), (2, false, &[0b1010_0000][..]));
        let mut f = fb(8, 1);
        tight_basic_decode(&mut f, r, &filter, d, &mut Vec::new()).unwrap();
        assert_eq!(f.pixel(0, 0), [255, 255, 255, 255]);
        assert_eq!(f.pixel(1, 0), [0, 0, 0, 255]);
        assert_eq!(f.pixel(2, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn tight_palette_8bit_indices() {
        // 4x3 three-colour palette rect: 12 index bytes, so compressed.
        let r = Rect::new(0, 0, 4, 3);
        let palette = [1, 1, 1, 2, 2, 2, 3, 3, 3];
        let idx = [0, 1, 2, 0, 1, 2, 0, 1, 2, 2, 2, 2];
        let z = crate::inflate::tests::zlib_chunks(&[&idx]);
        let mut data = vec![0x40, 1, 2];
        data.extend(palette);
        data.push(z[0].len() as u8);
        data.extend(&z[0]);
        let (t, n) = tight_parse(&data, r).unwrap().unwrap();
        assert_eq!(n, data.len());
        let TightBody::Basic {
            filter,
            raw_len,
            compressed: true,
            data: d,
            ..
        } = t.body
        else {
            panic!()
        };
        assert_eq!(raw_len, 12);
        let mut out = Vec::new();
        crate::inflate::Inflater::default()
            .inflate(d, &mut out, raw_len)
            .unwrap();
        let mut f = fb(4, 3);
        tight_basic_decode(&mut f, r, &filter, &out, &mut Vec::new()).unwrap();
        assert_eq!(f.pixel(1, 0), [2, 2, 2, 255]);
        assert_eq!(f.pixel(2, 0), [3, 3, 3, 255]);
        assert_eq!(f.pixel(2, 1), [1, 1, 1, 255]);
        assert_eq!(f.pixel(3, 2), [3, 3, 3, 255]);
    }

    #[test]
    fn tight_gradient() {
        // 2x2, all pixels (10,20,30): residuals per the predictor.
        let r = Rect::new(0, 0, 2, 2);
        let data = [10, 20, 30, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut f = fb(2, 2);
        tight_basic_decode(&mut f, r, &TightFilter::Gradient, &data, &mut Vec::new()).unwrap();
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            assert_eq!(f.pixel(x, y), [10, 20, 30, 255]);
        }
    }
}
