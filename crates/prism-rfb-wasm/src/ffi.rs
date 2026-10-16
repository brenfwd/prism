//! A small hand-written WebAssembly ABI over [`Client`], so the browser
//! needs no generated glue. One client per WebAssembly instance (one per
//! console worker).
//!
//! Calls that produce actions return the number of `u32` words written at
//! [`rfb_actions`], or -1 on a protocol error (message at [`rfb_error`]).
//! Byte payloads live in the buffer at [`rfb_data`], addressed by
//! (offset, length) pairs. Each action is a kind word followed by its
//! arguments:
//!
//! | kind | action      | words                                   |
//! |------|-------------|-----------------------------------------|
//! | 1    | send        | off, len                                |
//! | 2    | connected   | off, len (UTF-8 desktop name)           |
//! | 3    | resize      | width, height                           |
//! | 4    | dirty       | x, y, w, h                              |
//! | 5    | cursor      | hot x, hot y, w, h, off, len (RGBA)     |
//! | 6    | image       | x, y, w, h, format (1 JPEG, 2 PNG), off, len |
//! | 7    | bell        |                                         |
//! | 8    | clipboard   | off, len (UTF-8)                        |
//! | 9    | leds        | state                                   |
//!
//! Pointers and lengths are wasm32 addresses, so they fit in `u32`.

// Each export carries its own `#[allow(unsafe_code)]`: the `no_mangle`
// attribute is the only unsafe construct here, and a stray `unsafe` block elsewhere in
// the file still fails the crate's `deny(unsafe_code)`.

use std::cell::RefCell;

use crate::{Action, Client, Error, ErrorKind, Options};

#[derive(Default)]
struct Shim {
    client: Option<Client>,
    options: Option<Options>,
    input: Vec<u8>,
    words: Vec<u32>,
    data: Vec<u8>,
    error: String,
}

thread_local! {
    static SHIM: RefCell<Shim> = RefCell::new(Shim::default());
}

fn addr<T>(v: &[T]) -> u32 {
    v.as_ptr() as usize as u32
}

fn options(quality: i32, compression: i32) -> Options {
    Options::new(u8::try_from(quality).ok(), u8::try_from(compression).ok())
}

impl Shim {
    fn bytes(&mut self, b: &[u8]) -> [u32; 2] {
        let off = self.data.len() as u32;
        self.data.extend_from_slice(b);
        [off, b.len() as u32]
    }

    fn encode(&mut self, result: Result<Vec<Action>, Error>) -> i32 {
        self.words.clear();
        self.data.clear();
        let actions = match result {
            Ok(a) => a,
            Err(e) => {
                self.error = e.to_string();
                return -1;
            }
        };
        for a in actions {
            match a {
                Action::Send(b) => {
                    let [o, l] = self.bytes(&b);
                    self.words.extend([1, o, l]);
                }
                Action::Connected(name) => {
                    let [o, l] = self.bytes(name.as_bytes());
                    self.words.extend([2, o, l]);
                }
                Action::Resize { width, height } => {
                    self.words.extend([3, width.into(), height.into()]);
                }
                Action::Dirty(r) => {
                    self.words
                        .extend([4, r.x.into(), r.y.into(), r.w.into(), r.h.into()]);
                }
                Action::Cursor(c) => {
                    let [o, l] = self.bytes(&c.rgba);
                    self.words.extend([
                        5,
                        c.hotspot_x.into(),
                        c.hotspot_y.into(),
                        c.width.into(),
                        c.height.into(),
                        o,
                        l,
                    ]);
                }
                Action::DecodeImage { rect, format, data } => {
                    let [o, l] = self.bytes(&data);
                    self.words.extend([
                        6,
                        rect.x.into(),
                        rect.y.into(),
                        rect.w.into(),
                        rect.h.into(),
                        format as u32,
                        o,
                        l,
                    ]);
                }
                Action::Bell => self.words.push(7),
                Action::Clipboard(text) => {
                    let [o, l] = self.bytes(text.as_bytes());
                    self.words.extend([8, o, l]);
                }
                Action::Leds(s) => self.words.extend([9, s.into()]),
            }
        }
        self.words.len() as i32
    }

    fn client(&mut self) -> &mut Client {
        client_of(&mut self.client, self.options.as_ref())
    }
}

/// The first `len` bytes of `input`, or an error when the host claims more
/// bytes than it wrote.
fn prefix(input: &[u8], len: u32) -> Result<&[u8], Error> {
    input.get(..len as usize).ok_or_else(|| {
        Error::new(
            ErrorKind::OutOfRange,
            format!(
                "host passed length {len} but the input buffer holds {} bytes",
                input.len()
            ),
        )
    })
}

/// Returns the client, creating one from `options` on first use. A free
/// function so callers can borrow the input buffer at the same time.
fn client_of<'a>(client: &'a mut Option<Client>, options: Option<&Options>) -> &'a mut Client {
    client.get_or_insert_with(|| Client::new(options.cloned().unwrap_or_default()))
}

fn with<R>(f: impl FnOnce(&mut Shim) -> R) -> R {
    SHIM.with_borrow_mut(f)
}

/// Starts a fresh connection. `quality` 0..=9 enables Tight JPEG; -1
/// disables it. `compression` 0..=9, or -1 for the server default.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_create(quality: i32, compression: i32) {
    with(|s| {
        let opts = options(quality, compression);
        s.client = Some(Client::new(opts.clone()));
        s.options = Some(opts);
    });
}

/// Returns a buffer of `len` bytes for the next [`rfb_feed`],
/// [`rfb_image_done`] or [`rfb_cut_text`] call to read.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_input(len: u32) -> u32 {
    with(|s| {
        s.input.clear();
        s.input.resize(len as usize, 0);
        addr(&s.input)
    })
}

/// Feeds the first `len` bytes of the input buffer from the server. Returns
/// -1 if `len` exceeds the buffer from the last [`rfb_input`].
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_feed(len: u32) -> i32 {
    with(|s| {
        let r = prefix(&s.input, len)
            .and_then(|input| client_of(&mut s.client, s.options.as_ref()).feed(input));
        s.encode(r)
    })
}

/// Completes an image decode: the input buffer holds `len` RGBA bytes, or
/// `ok` is 0 if decoding failed. Returns -1 if `len` exceeds the input buffer.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_image_done(ok: u32, len: u32) -> i32 {
    with(|s| {
        let r = if ok == 0 {
            Ok(None)
        } else {
            prefix(&s.input, len).map(Some)
        }
        .and_then(|px| client_of(&mut s.client, s.options.as_ref()).image_decoded(px));
        s.encode(r)
    })
}

/// Sends a key event: `down` is nonzero for press, `keysym` is an X11 keysym
/// and `scancode` the XT scancode used when the server supports QEMU's
/// extended key event.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_key(down: u32, keysym: u32, scancode: u32) -> i32 {
    with(|s| {
        let a = s.client().key(down != 0, keysym, scancode);
        s.encode(Ok(a))
    })
}

/// Sends a pointer event at framebuffer position (`x`, `y`), clamped to
/// `u16::MAX`. `buttons` is the RFB button mask; only its low 8 bits are sent.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_pointer(x: u32, y: u32, buttons: u32) -> i32 {
    with(|s| {
        let clamp = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
        let a = s.client().pointer(clamp(x), clamp(y), buttons as u8);
        s.encode(Ok(a))
    })
}

/// Sends the first `len` bytes of the input buffer (UTF-8) as clipboard
/// text. Returns -1 if `len` exceeds the input buffer; invalid UTF-8 is
/// replaced with U+FFFD.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_cut_text(len: u32) -> i32 {
    with(|s| {
        let r = prefix(&s.input, len).map(|bytes| {
            let text = String::from_utf8_lossy(bytes);
            client_of(&mut s.client, s.options.as_ref()).cut_text(&text)
        });
        s.encode(r)
    })
}

/// Changes JPEG quality and compression level (as for [`rfb_create`]).
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_set_quality(quality: i32, compression: i32) -> i32 {
    with(|s| {
        let opts = options(quality, compression);
        s.options = Some(opts.clone());
        let a = s.client().set_options(opts);
        s.encode(Ok(a))
    })
}

/// Requests a full framebuffer refresh.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_refresh() -> i32 {
    with(|s| {
        let a = s.client().refresh();
        s.encode(Ok(a))
    })
}

/// Address of the action words written by the last call that returned a
/// count. Valid until the next call that returns actions.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_actions() -> u32 {
    with(|s| addr(&s.words))
}

/// Address of the byte payloads that the action words refer to by
/// (offset, length). Valid until the next call that returns actions.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_data() -> u32 {
    with(|s| addr(&s.data))
}

/// The RGBA framebuffer (moves when the framebuffer is resized).
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_fb() -> u32 {
    with(|s| addr(s.client().framebuffer().pixels()))
}

/// Framebuffer width in pixels.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_fb_width() -> u32 {
    with(|s| s.client().framebuffer().width().into())
}

/// Framebuffer height in pixels.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_fb_height() -> u32 {
    with(|s| s.client().framebuffer().height().into())
}

/// Address of the UTF-8 message for the last protocol error; its length is
/// [`rfb_error_len`].
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_error() -> u32 {
    with(|s| addr(s.error.as_bytes()))
}

/// Length in bytes of the message at [`rfb_error`]. Zero before any error.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn rfb_error_len() -> u32 {
    with(|s| s.error.len() as u32)
}
