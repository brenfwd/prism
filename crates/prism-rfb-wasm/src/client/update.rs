//! Server messages after the handshake, chiefly framebuffer updates and
//! their rectangle encodings (RFC 6143 sections 7.6 and 7.7).

use super::{
    Action, Client, CursorImage, ImageFormat, MAX_CURSOR, MAX_CUT_TEXT, RectState, State, be16,
    be32, enc, latin1,
};
use crate::decode::{self, HextileColours, TightBody};
use crate::fb::Rect;
use crate::{Error, ErrorKind};

impl Client {
    pub(super) fn message(&mut self, input: &[u8]) -> Result<Option<usize>, Error> {
        let Some(&kind) = input.first() else {
            return Ok(None);
        };
        match kind {
            0 => {
                let Some(h) = input.get(..4) else {
                    return Ok(None);
                };
                self.state = State::RectHeader(be16(&h[2..4]));
                Ok(Some(4))
            }
            1 => {
                let Some(h) = input.get(..6) else {
                    return Ok(None);
                };
                let len = 6 + usize::from(be16(&h[4..6])) * 6;
                Ok((input.len() >= len).then_some(len))
            }
            2 => {
                self.actions.push(Action::Bell);
                Ok(Some(1))
            }
            3 => {
                let Some(h) = input.get(..8) else {
                    return Ok(None);
                };
                let len = be32(&h[4..8]) as usize;
                if len > MAX_CUT_TEXT {
                    return Err(Error::new(
                        ErrorKind::OutOfRange,
                        "server clipboard text too large",
                    ));
                }
                let Some(text) = input.get(8..8 + len) else {
                    return Ok(None);
                };
                self.actions.push(Action::Clipboard(latin1(text)));
                Ok(Some(8 + len))
            }
            150 => Ok(Some(1)), // EndOfContinuousUpdates
            k => Err(Error::new(
                ErrorKind::Unsupported,
                format!("unsupported server message type {k}"),
            )),
        }
    }

    pub(super) fn end_update(&mut self) {
        let full = std::mem::take(&mut self.need_full);
        self.request(!full);
        self.state = State::Message;
    }

    pub(super) fn rect_header(&mut self, input: &[u8], left: u16) -> Result<Option<usize>, Error> {
        if left == 0 {
            self.end_update();
            return Ok(Some(0));
        }
        let Some(h) = input.get(..12) else {
            return Ok(None);
        };
        let rect = Rect::new(
            be16(&h[0..2]),
            be16(&h[2..4]),
            be16(&h[4..6]),
            be16(&h[6..8]),
        );
        let encoding = be32(&h[8..12]) as i32;
        let left = left - 1;
        match encoding {
            enc::LAST_RECT => {
                self.end_update();
                return Ok(Some(12));
            }
            enc::DESKTOP_SIZE => {
                self.resize(rect.w, rect.h)?;
                self.state = State::RectHeader(left);
                return Ok(Some(12));
            }
            enc::QEMU_EXTENDED_KEY_EVENT => {
                self.ext_key = true;
                self.state = State::RectHeader(left);
                return Ok(Some(12));
            }
            enc::RAW | enc::COPY_RECT | enc::HEXTILE | enc::ZRLE | enc::TIGHT | enc::TIGHT_PNG
                if !self.fb.contains(rect) =>
            {
                return Err(Error::new(
                    ErrorKind::OutOfRange,
                    format!("rectangle {rect:?} outside the framebuffer"),
                ));
            }
            enc::RAW
            | enc::COPY_RECT
            | enc::HEXTILE
            | enc::ZRLE
            | enc::TIGHT
            | enc::TIGHT_PNG
            | enc::CURSOR
            | enc::EXTENDED_DESKTOP_SIZE
            | enc::QEMU_LED_STATE => {}
            e => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!("server used unrequested encoding {e}"),
                ));
            }
        }
        let sub = match encoding {
            enc::RAW => RectState::Raw(0),
            enc::HEXTILE => RectState::Hextile(0, HextileColours::default()),
            _ => RectState::Whole,
        };
        self.state = State::Rect {
            left,
            rect,
            encoding,
            sub,
        };
        Ok(Some(12))
    }

    pub(super) fn rect_done(&mut self, left: u16, dirty: Option<Rect>) {
        if let Some(r) = dirty.filter(|r| !r.is_empty()) {
            self.actions.push(Action::Dirty(r));
        }
        self.state = State::RectHeader(left);
    }

    pub(super) fn rect_body(
        &mut self,
        input: &[u8],
        left: u16,
        rect: Rect,
        encoding: i32,
        sub: RectState,
    ) -> Result<Option<usize>, Error> {
        match (encoding, sub) {
            (enc::RAW, RectState::Raw(done)) => {
                let row_len = usize::from(rect.w) * 4;
                if rect.is_empty() {
                    self.rect_done(left, None);
                    return Ok(Some(0));
                }
                let rows = (input.len() / row_len).min(usize::from(rect.h - done)) as u16;
                if rows == 0 {
                    return Ok(None);
                }
                self.fb.put_rgbx_rows(rect, done, rows, input);
                if done + rows == rect.h {
                    self.rect_done(left, Some(rect));
                } else {
                    self.state = State::Rect {
                        left,
                        rect,
                        encoding,
                        sub: RectState::Raw(done + rows),
                    };
                }
                Ok(Some(usize::from(rows) * row_len))
            }
            (enc::HEXTILE, RectState::Hextile(index, mut colours)) => {
                let Some(tile) = decode::hextile_tile(rect, index) else {
                    self.rect_done(left, Some(rect));
                    return Ok(Some(0));
                };
                let Some(len) = decode::hextile_tile_len(input, tile) else {
                    return Ok(None);
                };
                let Some(data) = input.get(..len) else {
                    return Ok(None);
                };
                decode::hextile_decode(&mut self.fb, tile, data, &mut colours)?;
                self.state = State::Rect {
                    left,
                    rect,
                    encoding,
                    sub: RectState::Hextile(index + 1, colours),
                };
                Ok(Some(len))
            }
            (enc::COPY_RECT, _) => {
                let Some(s) = input.get(..4) else {
                    return Ok(None);
                };
                let (sx, sy) = (be16(&s[0..2]), be16(&s[2..4]));
                if !self.fb.contains(Rect::new(sx, sy, rect.w, rect.h)) {
                    return Err(Error::new(
                        ErrorKind::OutOfRange,
                        "CopyRect source outside the framebuffer",
                    ));
                }
                self.fb.copy_rect(sx, sy, rect);
                self.rect_done(left, Some(rect));
                Ok(Some(4))
            }
            (enc::ZRLE, _) => {
                let Some(h) = input.get(..4) else {
                    return Ok(None);
                };
                let len = be32(h) as usize;
                let Some(data) = input.get(4..4 + len) else {
                    return Ok(None);
                };
                let mut out = std::mem::take(&mut self.scratch);
                let res = self
                    .zrle
                    .inflate(data, &mut out, decode::zrle_limit(rect))
                    .and_then(|()| decode::zrle_decode(&mut self.fb, rect, &out, &mut self.tile));
                self.scratch = out;
                res?;
                self.rect_done(left, Some(rect));
                Ok(Some(4 + len))
            }
            (enc::TIGHT | enc::TIGHT_PNG, _) => self.tight(input, left, rect),
            (enc::CURSOR, _) => {
                let pixels = rect.area() * 4;
                let mask_row = usize::from(rect.w).div_ceil(8);
                let len = pixels + mask_row * usize::from(rect.h);
                let Some(data) = input.get(..len) else {
                    return Ok(None);
                };
                if rect.w <= MAX_CURSOR && rect.h <= MAX_CURSOR {
                    let mut rgba = data[..pixels].to_vec();
                    let mask = &data[pixels..];
                    for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                        let (x, y) = (i % usize::from(rect.w), i / usize::from(rect.w));
                        let bit = mask[y * mask_row + x / 8] >> (7 - x % 8) & 1;
                        px[3] = if bit == 1 { 255 } else { 0 };
                    }
                    self.actions.push(Action::Cursor(CursorImage {
                        hotspot_x: rect.x,
                        hotspot_y: rect.y,
                        width: rect.w,
                        height: rect.h,
                        rgba,
                    }));
                }
                self.rect_done(left, None);
                Ok(Some(len))
            }
            (enc::EXTENDED_DESKTOP_SIZE, _) => {
                let Some(&screens) = input.first() else {
                    return Ok(None);
                };
                let len = 4 + usize::from(screens) * 16;
                if input.len() < len {
                    return Ok(None);
                }
                // x is the reason, y the status; non-zero status rejects a
                // client-requested resize and leaves the size unchanged.
                if rect.y == 0 {
                    self.resize(rect.w, rect.h)?;
                }
                self.rect_done(left, None);
                Ok(Some(len))
            }
            (enc::QEMU_LED_STATE, _) => {
                let Some(&leds) = input.first() else {
                    return Ok(None);
                };
                self.actions.push(Action::Leds(leds));
                self.rect_done(left, None);
                Ok(Some(1))
            }
            _ => unreachable!("rect_header admits only handled encodings"),
        }
    }

    pub(super) fn tight(
        &mut self,
        input: &[u8],
        left: u16,
        rect: Rect,
    ) -> Result<Option<usize>, Error> {
        let Some((t, len)) = decode::tight_parse(input, rect)? else {
            return Ok(None);
        };
        for (i, z) in self.tight.iter_mut().enumerate() {
            if t.resets & (1 << i) != 0 {
                z.reset();
            }
        }
        match t.body {
            TightBody::Fill(px) => self.fb.fill(rect, px),
            TightBody::Jpeg(d) | TightBody::Png(d) => {
                let format = if matches!(t.body, TightBody::Jpeg(_)) {
                    ImageFormat::Jpeg
                } else {
                    ImageFormat::Png
                };
                self.actions.push(Action::DecodeImage {
                    rect,
                    format,
                    data: d.to_vec(),
                });
                self.state = State::AwaitImage { left, rect };
                return Ok(Some(len));
            }
            TightBody::Basic {
                stream,
                filter,
                raw_len,
                data,
                compressed,
            } => {
                let mut out = std::mem::take(&mut self.scratch);
                let res = if compressed {
                    self.tight[stream]
                        .inflate(data, &mut out, raw_len)
                        .and_then(|()| {
                            if out.len() == raw_len {
                                Ok(&out[..])
                            } else {
                                Err(Error::new(
                                    ErrorKind::Truncated,
                                    "Tight data shorter than its rectangle",
                                ))
                            }
                        })
                } else {
                    Ok(data)
                }
                .and_then(|d| {
                    decode::tight_basic_decode(&mut self.fb, rect, &filter, d, &mut self.tile)
                });
                self.scratch = out;
                res?;
            }
        }
        self.rect_done(left, Some(rect));
        Ok(Some(len))
    }
}
