//! A small ISO 9660 image writer with Joliet extensions.
//!
//! It writes a single root directory of small files, which is all a
//! cloud-init `NoCloud` seed needs:
//!
//! ```
//! # fn main() -> Result<(), prism_iso::Error> {
//! let iso = prism_iso::IsoBuilder::new("CIDATA")
//!     .file("user-data", "#cloud-config\n")
//!     .file("meta-data", "instance-id: tb-1\n")
//!     .build()?;
//! assert_eq!(iso.len() % prism_iso::SECTOR, 0);
//! # Ok(())
//! # }
//! ```
//!
//! The image holds a primary volume descriptor with uppercase ISO 9660
//! names (`USER_DATA.;1`), and a Joliet supplementary volume descriptor
//! whose directory carries the original names (`user-data`) in UCS-2.
//! Linux, Windows and libarchive read the Joliet tree when present.
//!
//! Layout, in 2048-byte sectors:
//!
//! | sector | contents |
//! |---|---|
//! | 0–15 | system area (zeros) |
//! | 16 | primary volume descriptor |
//! | 17 | Joliet supplementary volume descriptor |
//! | 18 | volume descriptor set terminator |
//! | 19–22 | path tables: primary L, primary M, Joliet L, Joliet M |
//! | 23… | primary root directory, Joliet root directory, file data |

#![warn(missing_docs)]

mod builder;
mod encode;
mod names;
mod timestamp;

pub use builder::IsoBuilder;

/// Logical sector and block size in bytes.
pub const SECTOR: usize = 2048;

/// Why an image could not be built or written.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The volume label is empty, longer than 16 bytes, or has a character
    /// outside `A-Z`, `0-9` and `_`.
    #[error(
        "invalid volume identifier {id:?}: use 1-16 characters from A-Z, 0-9 and _ (for example CIDATA)"
    )]
    VolumeId {
        /// The rejected label.
        id: String,
    },
    /// A file name breaks the Joliet limits on length or characters.
    #[error("invalid file name {name:?}: {reason}")]
    FileName {
        /// The rejected name.
        name: String,
        /// Which rule the name breaks.
        reason: &'static str,
    },
    /// Two files share a name.
    #[error("duplicate file name {name:?}")]
    Duplicate {
        /// The repeated name.
        name: String,
    },
    /// A file is 4 GiB or larger, or the image would need more than
    /// `u32::MAX` sectors.
    #[error("image too large")]
    TooLarge,
    /// Writing the image to the destination failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests;
