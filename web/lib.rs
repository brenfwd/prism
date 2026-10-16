//! Prism's web UI, embedded in the binary.
//!
//! `build.rs` compiles the console decoder to WebAssembly, then builds the
//! React UI with Bun into `$OUT_DIR/dist`. [`get`] serves files from that
//! directory: `index.html` at the root and the hashed bundles under
//! `assets/`.
//!
//! With `PRISM_SKIP_WEB=1` the Bun build is skipped and `dist` holds only a
//! stub `index.html` saying the UI is not included. Every other path then
//! returns `None`.

#![warn(missing_docs)]

use rust_embed::RustEmbed;

pub use rust_embed::EmbeddedFile;

/// The contents of `$OUT_DIR/dist`. Debug builds embed the files too
/// (`debug-embed`), so the binary never reads them from disk at runtime.
#[derive(RustEmbed)]
#[folder = "$OUT_DIR/dist"]
struct Assets;

/// Returns the built asset at `path`, relative with no leading `/` (for
/// example `index.html` or `assets/index-1a2b3c.js`). Returns `None` when
/// no such file was embedded.
pub fn get(path: &str) -> Option<EmbeddedFile> {
    Assets::get(path)
}
