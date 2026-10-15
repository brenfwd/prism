//! Tight: fill, JPEG, or zlib-compressed basic rectangles with copy,
//! palette or gradient filters.

use super::unpack_indexed;
use crate::Error;
use crate::fb::{Framebuffer, Rect};

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
    use crate::decode::test_fb as fb;

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
