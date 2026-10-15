//! The sans-IO RFB client state machine.

use crate::Error;
use crate::decode::{self, HextileColours, TightBody};
use crate::fb::{Framebuffer, MAX_DIM, Rect};
use crate::inflate::Inflater;

/// Encoding numbers (RFC 6143 section 7.7 plus registered extensions).
pub mod encoding {
    pub const RAW: i32 = 0;
    pub const COPY_RECT: i32 = 1;
    pub const HEXTILE: i32 = 5;
    pub const TIGHT: i32 = 7;
    pub const ZRLE: i32 = 16;
    pub const TIGHT_PNG: i32 = -260;
    /// JPEG quality 0..=9 is `JPEG_QUALITY_0 + q`.
    pub const JPEG_QUALITY_0: i32 = -32;
    /// Compression level 0..=9 is `COMPRESS_LEVEL_0 + l`.
    pub const COMPRESS_LEVEL_0: i32 = -256;
    pub const DESKTOP_SIZE: i32 = -223;
    pub const LAST_RECT: i32 = -224;
    pub const CURSOR: i32 = -239;
    pub const QEMU_EXTENDED_KEY_EVENT: i32 = -258;
    pub const QEMU_LED_STATE: i32 = -261;
    pub const EXTENDED_DESKTOP_SIZE: i32 = -308;
}

use encoding as enc;

/// Largest cursor image accepted (larger ones are ignored).
pub const MAX_CURSOR: u16 = 256;
/// Largest server clipboard text accepted.
const MAX_CUT_TEXT: usize = 1 << 20;

/// Connection options.
#[derive(Clone, Debug)]
pub struct Options {
    /// Encodings and pseudo-encodings, in preference order.
    pub encodings: Vec<i32>,
    /// Ask the server to keep other clients connected.
    pub shared: bool,
}

impl Options {
    /// The browser defaults: Tight (JPEG when `quality` is set), `TightPNG`,
    /// ZRLE, Hextile, Raw, plus every supported pseudo-encoding.
    pub fn new(quality: Option<u8>, compression: Option<u8>) -> Self {
        Self::with_encodings(
            &[
                enc::TIGHT,
                enc::TIGHT_PNG,
                enc::ZRLE,
                enc::HEXTILE,
                enc::COPY_RECT,
                enc::RAW,
            ],
            quality,
            compression,
        )
    }

    /// `primary` followed by the pseudo-encodings.
    pub fn with_encodings(primary: &[i32], quality: Option<u8>, compression: Option<u8>) -> Self {
        let mut encodings = primary.to_vec();
        encodings.extend([
            enc::EXTENDED_DESKTOP_SIZE,
            enc::DESKTOP_SIZE,
            enc::CURSOR,
            enc::LAST_RECT,
            enc::QEMU_EXTENDED_KEY_EVENT,
            enc::QEMU_LED_STATE,
        ]);
        if let Some(q) = quality {
            encodings.push(enc::JPEG_QUALITY_0 + i32::from(q.min(9)));
        }
        if let Some(c) = compression {
            encodings.push(enc::COMPRESS_LEVEL_0 + i32::from(c.min(9)));
        }
        Self {
            encodings,
            shared: true,
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self::new(Some(7), Some(2))
    }
}

/// Image formats the embedder decodes (the browser does it natively).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg = 1,
    Png = 2,
}

/// A cursor image, RGBA with alpha from the server's bitmask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorImage {
    pub hotspot_x: u16,
    pub hotspot_y: u16,
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
}

/// Something the embedder must do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Write these bytes to the server.
    Send(Vec<u8>),
    /// Handshake finished; the desktop's name.
    Connected(String),
    /// The framebuffer was reallocated at a new size (and cleared).
    Resize {
        width: u16,
        height: u16,
    },
    /// This area of the framebuffer changed.
    Dirty(Rect),
    /// New cursor shape. A zero-sized image hides the cursor.
    Cursor(CursorImage),
    /// Decode this image to `rect.w * rect.h` RGBA pixels and hand them to
    /// [`Client::image_decoded`]. Input processing pauses until then.
    DecodeImage {
        rect: Rect,
        format: ImageFormat,
        data: Vec<u8>,
    },
    Bell,
    /// Server clipboard text.
    Clipboard(String),
    /// Keyboard LEDs: bit 0 Scroll Lock, bit 1 Num Lock, bit 2 Caps Lock.
    Leds(u8),
}

#[derive(Clone, Copy, Debug)]
enum RectState {
    /// Raw rows already written.
    Raw(u16),
    /// Next tile index and carried colours.
    Hextile(u32, HextileColours),
    /// Decoded from one complete buffered unit.
    Whole,
}

#[derive(Clone, Copy, Debug)]
enum State {
    Version,
    SecurityTypes,
    SecurityResult,
    ServerInit,
    Message,
    /// Inside a `FramebufferUpdate` with this many rectangles left.
    RectHeader(u16),
    Rect {
        left: u16,
        rect: Rect,
        encoding: i32,
        sub: RectState,
    },
    /// Paused until the embedder decodes an image.
    AwaitImage {
        left: u16,
        rect: Rect,
    },
    Failed,
}

/// The RFB client. Feed it server bytes; it returns [`Action`]s.
#[derive(Debug)]
pub struct Client {
    opts: Options,
    state: State,
    minor: u8,
    buf: Vec<u8>,
    pos: usize,
    fb: Framebuffer,
    actions: Vec<Action>,
    tight: [Inflater; 4],
    zrle: Inflater,
    scratch: Vec<u8>,
    tile: Vec<u8>,
    ext_key: bool,
    need_full: bool,
    error: Option<String>,
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| char::from(c)).collect()
}

impl Client {
    pub fn new(opts: Options) -> Self {
        Self {
            opts,
            state: State::Version,
            minor: 8,
            buf: Vec::new(),
            pos: 0,
            fb: Framebuffer::default(),
            actions: Vec::new(),
            tight: Default::default(),
            zrle: Inflater::default(),
            scratch: Vec::new(),
            tile: Vec::new(),
            ext_key: false,
            need_full: false,
            error: None,
        }
    }

    pub fn framebuffer(&self) -> &Framebuffer {
        &self.fb
    }

    /// Whether the handshake has completed.
    pub fn is_connected(&self) -> bool {
        !matches!(
            self.state,
            State::Version
                | State::SecurityTypes
                | State::SecurityResult
                | State::ServerInit
                | State::Failed
        )
    }

    /// Whether the server accepted QEMU extended (scancode) key events.
    pub fn has_extended_keys(&self) -> bool {
        self.ext_key
    }

    /// Processes bytes received from the server.
    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<Action>, Error> {
        if let Some(e) = &self.error {
            return Err(Error::new(e.clone()));
        }
        self.buf.extend_from_slice(data);
        let buf = std::mem::take(&mut self.buf);
        let result = self.run(&buf);
        self.buf = buf;
        if self.pos > 0 && (self.pos == self.buf.len() || self.pos > 1 << 16) {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        match result {
            Ok(()) => Ok(std::mem::take(&mut self.actions)),
            Err(e) => {
                self.state = State::Failed;
                self.error = Some(e.to_string());
                Err(e)
            }
        }
    }

    fn run(&mut self, buf: &[u8]) -> Result<(), Error> {
        while let Some(n) = self.step(&buf[self.pos..])? {
            self.pos += n;
        }
        Ok(())
    }

    /// Resumes after [`Action::DecodeImage`]. `rgba` is `None` if decoding
    /// failed (the area is left as it was).
    pub fn image_decoded(&mut self, rgba: Option<&[u8]>) -> Result<Vec<Action>, Error> {
        let State::AwaitImage { left, rect } = self.state else {
            return Err(Error::new("no image decode pending"));
        };
        if let Some(px) = rgba.filter(|p| p.len() == rect.area() * 4) {
            self.fb.put_rgba(rect, px);
            self.actions.push(Action::Dirty(rect));
        }
        self.state = State::RectHeader(left);
        self.feed(&[])
    }

    /// A key press or release. With QEMU extended key events, `scancode`
    /// (XT set 1; `0xE0`-prefixed codes as `0xE0xx`) drives the guest and
    /// `keysym` is advisory; otherwise `keysym` is sent.
    pub fn key(&mut self, down: bool, keysym: u32, scancode: u32) -> Vec<Action> {
        if !self.is_connected() {
            return Vec::new();
        }
        let mut msg = Vec::with_capacity(12);
        if self.ext_key && scancode != 0 {
            // QEMU encodes 0xE0-prefixed scancodes with the high bit set.
            let code = if scancode >> 8 == 0xe0 {
                (scancode & 0x7f) | 0x80
            } else {
                scancode & 0xff
            };
            msg.extend([255, 0]);
            msg.extend(u16::from(down).to_be_bytes());
            msg.extend(keysym.to_be_bytes());
            msg.extend(code.to_be_bytes());
        } else if keysym != 0 {
            msg.extend([4, u8::from(down), 0, 0]);
            msg.extend(keysym.to_be_bytes());
        } else {
            return Vec::new();
        }
        vec![Action::Send(msg)]
    }

    /// Pointer position (clamped to the framebuffer) and button mask
    /// (bit 0 left, 1 middle, 2 right, 3/4 wheel up/down, 5/6 wheel left/right).
    pub fn pointer(&mut self, x: u16, y: u16, buttons: u8) -> Vec<Action> {
        if !self.is_connected() {
            return Vec::new();
        }
        let x = x.min(self.fb.width().saturating_sub(1));
        let y = y.min(self.fb.height().saturating_sub(1));
        let mut msg = vec![5, buttons];
        msg.extend(x.to_be_bytes());
        msg.extend(y.to_be_bytes());
        vec![Action::Send(msg)]
    }

    /// Sends clipboard text (Latin-1; other characters become `?`).
    pub fn cut_text(&mut self, text: &str) -> Vec<Action> {
        if !self.is_connected() {
            return Vec::new();
        }
        let bytes: Vec<u8> = text
            .chars()
            .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
            .collect();
        let mut msg = vec![6, 0, 0, 0];
        msg.extend((bytes.len() as u32).to_be_bytes());
        msg.extend(bytes);
        vec![Action::Send(msg)]
    }

    /// Changes the requested encodings (e.g. JPEG quality).
    pub fn set_options(&mut self, opts: Options) -> Vec<Action> {
        self.opts = opts;
        if !self.is_connected() {
            return Vec::new();
        }
        self.send(self.set_encodings_msg());
        self.request(false);
        std::mem::take(&mut self.actions)
    }

    /// Requests a full (non-incremental) refresh.
    pub fn refresh(&mut self) -> Vec<Action> {
        if self.is_connected() {
            self.request(false);
        }
        std::mem::take(&mut self.actions)
    }

    fn send(&mut self, bytes: Vec<u8>) {
        if let Some(Action::Send(prev)) = self.actions.last_mut() {
            prev.extend(bytes);
        } else {
            self.actions.push(Action::Send(bytes));
        }
    }

    fn request(&mut self, incremental: bool) {
        let mut msg = vec![3, u8::from(incremental), 0, 0, 0, 0];
        msg.extend(self.fb.width().to_be_bytes());
        msg.extend(self.fb.height().to_be_bytes());
        self.send(msg);
    }

    fn set_encodings_msg(&self) -> Vec<u8> {
        let mut msg = vec![2, 0];
        msg.extend((self.opts.encodings.len() as u16).to_be_bytes());
        for e in &self.opts.encodings {
            msg.extend(e.to_be_bytes());
        }
        msg
    }

    fn resize(&mut self, w: u16, h: u16) -> Result<(), Error> {
        if w > MAX_DIM || h > MAX_DIM {
            return Err(Error::new(format!(
                "framebuffer {w}x{h} exceeds {MAX_DIM}x{MAX_DIM}"
            )));
        }
        // QEMU repeats ExtendedDesktopSize on every full update request;
        // an unchanged size must not clear the screen.
        if (w, h) == (self.fb.width(), self.fb.height()) && !self.fb.pixels().is_empty() {
            return Ok(());
        }
        self.fb.resize(w, h);
        self.actions.push(Action::Resize {
            width: w,
            height: h,
        });
        self.need_full = true;
        Ok(())
    }

    /// One parsing step on the unconsumed input. `Ok(None)` means more
    /// input is needed; `Ok(Some(n))` consumed `n` bytes (possibly zero
    /// after a state change).
    fn step(&mut self, input: &[u8]) -> Result<Option<usize>, Error> {
        match self.state {
            State::Version => self.version(input),
            State::SecurityTypes => self.security_types(input),
            State::SecurityResult => self.security_result(input),
            State::ServerInit => self.server_init(input),
            State::Message => self.message(input),
            State::RectHeader(left) => self.rect_header(input, left),
            State::Rect {
                left,
                rect,
                encoding,
                sub,
            } => self.rect_body(input, left, rect, encoding, sub),
            State::AwaitImage { .. } | State::Failed => Ok(None),
        }
    }

    fn version(&mut self, input: &[u8]) -> Result<Option<usize>, Error> {
        let Some(v) = input.get(..12) else {
            return Ok(None);
        };
        let bad = || {
            Error::new(format!(
                "not an RFB server: {:?}",
                String::from_utf8_lossy(v)
            ))
        };
        if &v[..4] != b"RFB " || v[7] != b'.' || v[11] != b'\n' {
            return Err(bad());
        }
        let num = |s: &[u8]| {
            std::str::from_utf8(s)
                .ok()
                .and_then(|s| s.parse::<u16>().ok())
        };
        let (major, minor) = (
            num(&v[4..7]).ok_or_else(bad)?,
            num(&v[8..11]).ok_or_else(bad)?,
        );
        if major != 3 || minor < 3 {
            return Err(bad());
        }
        self.minor = match minor {
            3..=6 => 3,
            7 => 7,
            _ => 8,
        };
        self.send(format!("RFB 003.00{}\n", self.minor).into_bytes());
        self.state = State::SecurityTypes;
        Ok(Some(12))
    }

    /// Reads a u32-length-prefixed reason string at `input[at..]`.
    fn reason(input: &[u8], at: usize) -> Option<(String, usize)> {
        let len = be32(input.get(at..at + 4)?) as usize;
        let text = input.get(at + 4..at + 4 + len.min(4096))?;
        Some((latin1(text), at + 4 + len))
    }

    fn security_types(&mut self, input: &[u8]) -> Result<Option<usize>, Error> {
        if self.minor == 3 {
            let Some(t) = input.get(..4).map(be32) else {
                return Ok(None);
            };
            return match t {
                1 => {
                    self.client_init();
                    Ok(Some(4))
                }
                0 => match Self::reason(input, 4) {
                    Some((r, _)) => Err(Error::new(format!("server refused connection: {r}"))),
                    None => Ok(None),
                },
                t => Err(Error::new(format!("unsupported security type {t}"))),
            };
        }
        let Some(&n) = input.first() else {
            return Ok(None);
        };
        if n == 0 {
            return match Self::reason(input, 1) {
                Some((r, _)) => Err(Error::new(format!("server refused connection: {r}"))),
                None => Ok(None),
            };
        }
        let Some(types) = input.get(1..1 + usize::from(n)) else {
            return Ok(None);
        };
        if !types.contains(&1) {
            return Err(Error::new(format!(
                "server requires authentication (security types {types:?}); only None is supported"
            )));
        }
        self.send(vec![1]);
        if self.minor >= 8 {
            self.state = State::SecurityResult;
        } else {
            self.client_init();
        }
        Ok(Some(1 + usize::from(n)))
    }

    fn security_result(&mut self, input: &[u8]) -> Result<Option<usize>, Error> {
        let Some(r) = input.get(..4).map(be32) else {
            return Ok(None);
        };
        if r == 0 {
            self.client_init();
            return Ok(Some(4));
        }
        match Self::reason(input, 4) {
            Some((r, _)) => Err(Error::new(format!("security handshake failed: {r}"))),
            None => Ok(None),
        }
    }

    fn client_init(&mut self) {
        self.send(vec![u8::from(self.opts.shared)]);
        self.state = State::ServerInit;
    }

    fn server_init(&mut self, input: &[u8]) -> Result<Option<usize>, Error> {
        let Some(head) = input.get(..24) else {
            return Ok(None);
        };
        let name_len = be32(&head[20..24]) as usize;
        if name_len > 1 << 16 {
            return Err(Error::new("desktop name too long"));
        }
        let Some(name) = input.get(24..24 + name_len) else {
            return Ok(None);
        };
        let name = String::from_utf8_lossy(name).into_owned();
        self.resize(be16(&head[0..2]), be16(&head[2..4]))?;
        self.actions.push(Action::Connected(name));
        // SetPixelFormat: 32 bpp, depth 24, little-endian true colour with
        // red in the lowest byte, so wire pixels are R, G, B, X in memory.
        let mut msg = vec![0, 0, 0, 0, 32, 24, 0, 1];
        msg.extend(255u16.to_be_bytes());
        msg.extend(255u16.to_be_bytes());
        msg.extend(255u16.to_be_bytes());
        msg.extend([0, 8, 16, 0, 0, 0]);
        self.send(msg);
        self.send(self.set_encodings_msg());
        self.request(false);
        self.need_full = false;
        self.state = State::Message;
        Ok(Some(24 + name_len))
    }

    fn message(&mut self, input: &[u8]) -> Result<Option<usize>, Error> {
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
                    return Err(Error::new("server clipboard text too large"));
                }
                let Some(text) = input.get(8..8 + len) else {
                    return Ok(None);
                };
                self.actions.push(Action::Clipboard(latin1(text)));
                Ok(Some(8 + len))
            }
            150 => Ok(Some(1)), // EndOfContinuousUpdates
            k => Err(Error::new(format!("unsupported server message type {k}"))),
        }
    }

    fn end_update(&mut self) {
        let full = std::mem::take(&mut self.need_full);
        self.request(!full);
        self.state = State::Message;
    }

    fn rect_header(&mut self, input: &[u8], left: u16) -> Result<Option<usize>, Error> {
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
                return Err(Error::new(format!(
                    "rectangle {rect:?} outside the framebuffer"
                )));
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
            e => return Err(Error::new(format!("server used unrequested encoding {e}"))),
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

    fn rect_done(&mut self, left: u16, dirty: Option<Rect>) {
        if let Some(r) = dirty.filter(|r| !r.is_empty()) {
            self.actions.push(Action::Dirty(r));
        }
        self.state = State::RectHeader(left);
    }

    fn rect_body(
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
                    return Err(Error::new("CopyRect source outside the framebuffer"));
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

    fn tight(&mut self, input: &[u8], left: u16, rect: Rect) -> Result<Option<usize>, Error> {
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
                                Err(Error::new("Tight data shorter than its rectangle"))
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

#[cfg(test)]
mod tests;
