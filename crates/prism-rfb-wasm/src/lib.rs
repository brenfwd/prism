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

mod client;
mod decode;
mod fb;
pub mod ffi;
mod inflate;

pub use client::{Action, Client, CursorImage, ImageFormat, MAX_CURSOR, Options, encoding};
pub use fb::{Framebuffer, MAX_DIM, Rect};

/// A protocol error. The connection cannot continue after one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}
