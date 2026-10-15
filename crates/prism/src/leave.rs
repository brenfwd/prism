//! `prism leave`: take this node out of its cluster and wipe its state.
//!
//! Security does not depend on this running: removal by another member cuts
//! the node's `WireGuard` key off everywhere. `--force` wipes offline.

use std::path::Path;
use std::time::Duration;

use anyhow::bail;

use crate::config::Config;

/// Removes the persistent cluster identity: replicated state, node ID and
/// `WireGuard` key. The config file stays.
fn wipe(dir: &Path) -> anyhow::Result<()> {
    for sub in ["data", "wg"] {
        let p = dir.join(sub);
        match std::fs::remove_dir_all(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => bail!("removing {}: {e}", p.display()),
        }
    }
    // The interface is gone with its key; failure just means it never existed.
    let _ = std::process::Command::new("ip")
        .args(["link", "del", "wg-prism"])
        .stderr(std::process::Stdio::null())
        .status();
    Ok(())
}

pub async fn leave(dir: &Path, force: bool) -> anyhow::Result<()> {
    if !force {
        let cfg = Config::load(dir)?;
        crate::cli::call(&cfg, "POST", "/cluster/leave", None).await?;
        println!("Removed this node from the cluster; waiting for prism to stop...");
    }
    // The daemon exits by itself after a graceful leave; with --force it
    // must already be stopped.
    let mut lock = None;
    for _ in 0..if force { 1 } else { 60 } {
        match crate::lock_dir(dir) {
            Ok(l) => {
                lock = Some(l);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    let Some(_lock) = lock else {
        bail!("prism is still serving {}; stop it first", dir.display());
    };
    wipe(dir)?;
    println!(
        "Wiped {}/data and {}/wg. To rejoin, get a new invite and run `prism join`.",
        dir.display(),
        dir.display()
    );
    Ok(())
}
