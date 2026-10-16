//! The Ubuntu cloud image: download, checksum and local cache.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

use super::{IMAGE_BASE_URL, IMAGE_FILE, Testbed, info, path_str, remove_file_best_effort, step};
use crate::virsh::{run, run_visible};

/// Seconds curl may take to connect before it gives up.
const CURL_CONNECT_SECS: &str = "30";
/// Seconds curl may spend on the checksum list.
const CURL_SUMS_SECS: &str = "60";
/// Seconds curl may spend on the image itself (about 600 MB).
const CURL_IMAGE_SECS: &str = "3600";

impl Testbed {
    /// Downloads the cloud image into `target/testbed` and verifies it.
    pub(super) fn ensure_image(&self) -> anyhow::Result<PathBuf> {
        let image = self.dir.join(IMAGE_FILE);
        step("Fetching SHA256SUMS");
        let sums = run(
            "curl",
            &[
                "-fsSL",
                "--connect-timeout",
                CURL_CONNECT_SECS,
                "--max-time",
                CURL_SUMS_SECS,
                &format!("{IMAGE_BASE_URL}/SHA256SUMS"),
            ],
        )?;
        let want = sums
            .lines()
            .find_map(|l| {
                let (hash, name) = l.split_once(' ')?;
                (name.trim_start_matches([' ', '*']) == IMAGE_FILE).then(|| hash.to_owned())
            })
            .with_context(|| format!("{IMAGE_FILE} not listed in SHA256SUMS"))?;

        if image.exists() {
            if sha256(&image)? == want {
                info(&format!("cached {} verified", image.display()));
                return Ok(image);
            }
            info("cached image is stale or corrupt; downloading again");
        }
        step(&format!("Downloading {IMAGE_FILE}"));
        let part = image.with_extension("img.part");
        run_visible(
            "curl",
            &[
                "-fL",
                "--connect-timeout",
                CURL_CONNECT_SECS,
                "--max-time",
                CURL_IMAGE_SECS,
                "--progress-bar",
                "-o",
                path_str(&part)?,
                &format!("{IMAGE_BASE_URL}/{IMAGE_FILE}"),
            ],
        )?;
        let got = sha256(&part)?;
        if got != want {
            remove_file_best_effort(&part);
            bail!("checksum mismatch for {IMAGE_FILE}: got {got}, want {want}");
        }
        std::fs::rename(&part, &image)?;
        info("checksum OK");
        Ok(image)
    }
}

/// SHA-256 of the file at `path` as lowercase hex, computed by `sha256sum`.
pub(super) fn sha256(path: &Path) -> anyhow::Result<String> {
    let out = run("sha256sum", &[path_str(path)?])?;
    out.split_whitespace()
        .next()
        .map(str::to_owned)
        .context("sha256sum printed nothing")
}
