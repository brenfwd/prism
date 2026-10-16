//! ZRLE (RFC 6143 section 7.7.6): zlib-compressed 64x64 tiles, raw,
//! solid, palette or run-length encoded.

use super::{Reader, unpack_indexed};
use crate::fb::{Framebuffer, Rect};
use crate::{Error, ErrorKind};

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
                let px = *palette.get(usize::from(b & 127)).ok_or_else(|| {
                    Error::new(ErrorKind::OutOfRange, "ZRLE palette index out of range")
                })?;
                let run = if b & 128 != 0 { run_length(rd)? } else { 1 };
                push_run(buf, px, run, n)?;
            }
            fb.put_rgba(t, buf);
        }
        _ => {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("invalid ZRLE subencoding {sub}"),
            ));
        }
    }
    Ok(())
}

fn push_run(buf: &mut Vec<u8>, px: [u8; 4], run: usize, total: usize) -> Result<(), Error> {
    if buf.len() / 4 + run > total {
        return Err(Error::new(
            ErrorKind::OutOfRange,
            "ZRLE run overflows its tile",
        ));
    }
    for _ in 0..run {
        buf.extend_from_slice(&px);
    }
    Ok(())
}

fn read_palette(rd: &mut Reader<'_>, n: usize) -> Result<Vec<[u8; 4]>, Error> {
    (0..n).map(|_| rd.rgb()).collect()
}

#[cfg(test)]
#[allow(clippy::many_single_char_names)]
mod tests {
    use super::*;
    use crate::decode::test_fb as fb;

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
}
