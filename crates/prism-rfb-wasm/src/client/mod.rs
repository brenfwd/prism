//! The sans-IO RFB client state machine.

mod handshake;
mod update;

use crate::Error;
use crate::decode::HextileColours;
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
}

#[cfg(test)]
mod tests;
