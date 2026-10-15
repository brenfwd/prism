//! Hextile (RFC 6143 section 7.7.4): 16x16 tiles, each raw or a
//! background plus subrectangles.

use super::Reader;
use crate::Error;
use crate::fb::{BLACK, Framebuffer, Rect};

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

#[cfg(test)]
#[allow(clippy::many_single_char_names)]
mod tests {
    use super::*;
    use crate::decode::test_fb as fb;

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
}
