//! Sans-IO RFB (VNC) client core for Prism's browser console.
//!
//! [`Client`] consumes bytes from the server and returns [`Action`]s: bytes
//! to send, framebuffer areas that changed, resizes, cursor images, images
//! for the browser to decode (Tight JPEG and PNG) and bells. The RGBA
//! framebuffer lives in the client; in the browser it sits in WebAssembly
//! memory and dirty areas are uploaded straight to a texture.
//!
//! Written from RFC 6143 and the community RFB extensions, not derived
//! from any other implementation.
//!
//! The WebAssembly exports live in a private module; the crate is also
//! built as an `rlib` so the client can run in native tests.

#![warn(missing_docs)]

mod client;
mod decode;
mod fb;
mod ffi;
mod inflate;

pub use client::{Action, Client, CursorImage, ImageFormat, MAX_CURSOR, Options, encoding};
pub use fb::{Framebuffer, MAX_DIM, Rect};

/// What went wrong, for callers that react to the cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The server sent fewer bytes than a message or rectangle needs.
    Truncated,
    /// The server used a message type, encoding, filter or security type
    /// this client does not implement, or one it did not request.
    Unsupported,
    /// The server refused the connection or the security handshake.
    Refused,
    /// A size, index, clipboard length or rectangle is outside its limits,
    /// such as a framebuffer larger than [`MAX_DIM`].
    OutOfRange,
    /// The zlib stream is invalid, stalled or larger than its rectangle.
    Inflate,
    /// Any other violation of the protocol.
    Malformed,
}

/// A protocol error. The connection cannot continue after one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

impl Error {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The cause of the error.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
