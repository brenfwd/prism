//! Prism's web UI. `build.rs` builds it with Bun into `OUT_DIR`, and the
//! output is embedded into the binary.

use rust_embed::RustEmbed;

pub use rust_embed::EmbeddedFile;

#[derive(RustEmbed)]
#[folder = "$OUT_DIR/dist"]
struct Assets;

/// Returns the built asset at `path` (relative, no leading `/`).
pub fn get(path: &str) -> Option<EmbeddedFile> {
    Assets::get(path)
}
