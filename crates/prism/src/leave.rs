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
    remove_network_leftovers();
    Ok(())
}

/// Deletes the switches and firewall tables the stopped daemon left behind.
/// Best effort: all of it is rebuilt by the daemon after a rejoin.
fn remove_network_leftovers() {
    use std::process::{Command, Stdio};
    let quiet = |cmd: &str, args: &[&str]| {
        let _ = Command::new(cmd)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    };
    if let Ok(out) = Command::new("ip").args(["-br", "link"]).output() {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let name = line.split_whitespace().next().unwrap_or_default();
            let name = name.split('@').next().unwrap_or_default();
            let digits = name.get(2..).unwrap_or_default();
            let ours = (name.starts_with("ps") || name.starts_with("vx"))
                && !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit());
            if ours {
                quiet("ip", &["link", "del", name]);
            }
        }
    }
    for table in [["inet", "prism"], ["ip", "prism-nat"], ["bridge", "prism"]] {
        quiet("nft", &["delete", "table", table[0], table[1]]);
    }
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
