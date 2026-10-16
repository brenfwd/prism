//! Disk specs and guest device naming.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskSpec {
    pub id: Uuid,
    pub pool: String,
    /// The image file; `None` is an empty CD-ROM tray.
    pub volume: Option<String>,
    pub device: DiskDevice,
    pub format: String,
    #[serde(default)]
    pub boot_order: Option<u32>,
    /// Guest device name (`vda`, `sdb`); stable across hotplug.
    #[serde(default)]
    pub target: String,
    /// Read-only backing layers below `volume` (snapshot history), oldest
    /// first, as volume names in the same pool.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backing: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskDevice {
    Disk,
    Cdrom,
}

impl DiskDevice {
    pub fn target_prefix(self) -> &'static str {
        match self {
            Self::Disk => "vd",
            Self::Cdrom => "sd",
        }
    }
}

/// `vda`..`vdz`, then `vdaa`..: the Linux naming scheme.
pub fn disk_target(prefix: &str, index: usize) -> String {
    let letters = b"abcdefghijklmnopqrstuvwxyz";
    if index < 26 {
        format!("{prefix}{}", letters[index] as char)
    } else {
        format!(
            "{prefix}{}{}",
            letters[(index / 26 - 1) % 26] as char,
            letters[index % 26] as char
        )
    }
}
