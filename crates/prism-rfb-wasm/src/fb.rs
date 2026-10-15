//! The client-side framebuffer: tightly packed RGBA, alpha always 255, so it
//! can be uploaded to a texture or `ImageData` without conversion.

/// Largest framebuffer side accepted from a server.
pub const MAX_DIM: u16 = 8192;

/// A rectangle in framebuffer coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    /// Number of pixels covered.
    pub fn area(self) -> usize {
        usize::from(self.w) * usize::from(self.h)
    }

    pub fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }
}

/// An opaque black pixel.
pub const BLACK: [u8; 4] = [0, 0, 0, 255];

/// RGBA framebuffer.
#[derive(Debug, Default)]
pub struct Framebuffer {
    width: u16,
    height: u16,
    data: Vec<u8>,
}

impl Framebuffer {
    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    /// The RGBA pixels, row-major, `width * 4` bytes per row.
    pub fn pixels(&self) -> &[u8] {
        &self.data
    }

    /// Reallocates to `width` x `height` and clears to black.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        self.data.clear();
        self.data
            .resize(usize::from(width) * usize::from(height) * 4, 0);
        for px in self.data.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
    }

    /// Whether `r` lies entirely inside the framebuffer.
    pub fn contains(&self, r: Rect) -> bool {
        u32::from(r.x) + u32::from(r.w) <= u32::from(self.width)
            && u32::from(r.y) + u32::from(r.h) <= u32::from(self.height)
    }

    fn offset(&self, x: u16, y: u16) -> usize {
        (usize::from(y) * usize::from(self.width) + usize::from(x)) * 4
    }

    fn row_span(&self, r: Rect, row: u16) -> std::ops::Range<usize> {
        let start = self.offset(r.x, r.y + row);
        start..start + usize::from(r.w) * 4
    }

    /// The RGBA pixel at (`x`, `y`). Panics when out of bounds.
    pub fn pixel(&self, x: u16, y: u16) -> [u8; 4] {
        let o = self.offset(x, y);
        [
            self.data[o],
            self.data[o + 1],
            self.data[o + 2],
            self.data[o + 3],
        ]
    }

    /// Fills `r` (which must be inside the framebuffer) with `px`.
    pub fn fill(&mut self, r: Rect, px: [u8; 4]) {
        debug_assert!(self.contains(r));
        for row in 0..r.h {
            let span = self.row_span(r, row);
            self.data[span].as_chunks_mut::<4>().0.fill(px);
        }
    }

    /// Writes `rows` rows starting at row `first` of `r` from 32-bit
    /// pixels in the negotiated wire format (R, G, B, padding).
    pub fn put_rgbx_rows(&mut self, r: Rect, first: u16, rows: u16, src: &[u8]) {
        let row_len = usize::from(r.w) * 4;
        for (i, line) in src
            .chunks_exact(row_len)
            .take(usize::from(rows))
            .enumerate()
        {
            let span = self.row_span(r, first + i as u16);
            let dst = &mut self.data[span];
            dst.copy_from_slice(line);
            for px in dst.as_chunks_mut::<4>().0 {
                px[3] = 255;
            }
        }
    }

    /// Writes `r` from 24-bit R, G, B pixels (Tight `TPIXEL` / ZRLE `CPIXEL`).
    pub fn put_rgb(&mut self, r: Rect, src: &[u8]) {
        let row_len = usize::from(r.w) * 3;
        for (row, line) in src.chunks_exact(row_len).take(usize::from(r.h)).enumerate() {
            let span = self.row_span(r, row as u16);
            let dst = self.data[span].as_chunks_mut::<4>().0;
            for (d, s) in dst.iter_mut().zip(line.as_chunks::<3>().0) {
                *d = [s[0], s[1], s[2], 255];
            }
        }
    }

    /// Writes `r` from RGBA pixels, forcing alpha to 255.
    pub fn put_rgba(&mut self, r: Rect, src: &[u8]) {
        self.put_rgbx_rows(r, 0, r.h, src);
    }

    /// Copies the `dst.w` x `dst.h` area at (`src_x`, `src_y`) to `dst`.
    /// Overlapping areas are handled like `memmove`.
    pub fn copy_rect(&mut self, src_x: u16, src_y: u16, dst: Rect) {
        let rows: Box<dyn Iterator<Item = u16>> = if src_y < dst.y {
            Box::new((0..dst.h).rev())
        } else {
            Box::new(0..dst.h)
        };
        let len = usize::from(dst.w) * 4;
        for row in rows {
            let s = self.offset(src_x, src_y + row);
            let d = self.offset(dst.x, dst.y + row);
            self.data.copy_within(s..s + len, d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_rect_overlapping_down() {
        let mut fb = Framebuffer::default();
        fb.resize(2, 3);
        fb.fill(Rect::new(0, 0, 2, 1), [1, 1, 1, 255]);
        fb.fill(Rect::new(0, 1, 2, 1), [2, 2, 2, 255]);
        fb.copy_rect(0, 0, Rect::new(0, 1, 2, 2));
        assert_eq!(fb.pixel(1, 0), [1, 1, 1, 255]);
        assert_eq!(fb.pixel(1, 1), [1, 1, 1, 255]);
        assert_eq!(fb.pixel(1, 2), [2, 2, 2, 255]);
    }

    #[test]
    fn put_rgbx_forces_alpha() {
        let mut fb = Framebuffer::default();
        fb.resize(1, 1);
        fb.put_rgbx_rows(Rect::new(0, 0, 1, 1), 0, 1, &[9, 8, 7, 0]);
        assert_eq!(fb.pixel(0, 0), [9, 8, 7, 255]);
    }
}
