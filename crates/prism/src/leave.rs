//! `prism leave`: take this node out of its cluster and wipe its state.
//!
//! Security does not depend on this running: removal by another member cuts
//! the node's `WireGuard` key off everywhere. `--force` wipes offline.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::bail;

use crate::config::Config;

/// Removes the persistent cluster identity: replicated state, node ID and
/// `WireGuard` key. The config file stays.
///
/// Fails if a directory cannot be removed. Leftover bridges and firewall
/// tables only produce warnings on stderr, naming what remains.
fn wipe(dir: &Path) -> anyhow::Result<()> {
    for sub in ["data", "wg"] {
        let p = dir.join(sub);
        match std::fs::remove_dir_all(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => bail!("removing {}: {e}", p.display()),
        }
    }
    // The interface is gone with its key; a node that never joined has none,
    // so a failure here is expected and not reported.
    let _ = Command::new("ip")
        .args(["link", "del", "wg-prism"])
        .stderr(Stdio::null())
        .status();
    remove_network_leftovers();
    Ok(())
}

/// Whether `name` is a switch or tunnel interface Prism created: `ps` or
/// `vx` followed by digits (for example `ps3` or `vx100`).
fn is_prism_link(name: &str) -> bool {
    let digits = name
        .strip_prefix("ps")
        .or_else(|| name.strip_prefix("vx"))
        .unwrap_or_default();
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Interface names from `ip -br link` output, without the `@parent` suffix.
fn link_names(output: &str) -> impl Iterator<Item = &str> {
    output
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter_map(|n| n.split('@').next())
}

/// Runs a cleanup command and warns on stderr if it fails.
fn run_cleanup(what: &str, cmd: &str, args: &[&str]) {
    match Command::new(cmd).args(args).output() {
        Ok(out) if out.status.success() => {}
        Ok(out) => eprintln!(
            "warning: {what} was not removed ({cmd} {}): {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => eprintln!("warning: {what} was not removed ({cmd} failed to run: {e})"),
    }
}

/// Deletes the switches and firewall tables the stopped daemon left behind.
/// The daemon rebuilds all of it after a rejoin, so failures are warnings.
fn remove_network_leftovers() {
    match Command::new("ip").args(["-br", "link"]).output() {
        Ok(out) => {
            for name in
                link_names(&String::from_utf8_lossy(&out.stdout)).filter(|n| is_prism_link(n))
            {
                run_cleanup(&format!("interface {name}"), "ip", &["link", "del", name]);
            }
        }
        Err(e) => {
            eprintln!("warning: could not list interfaces, so Prism bridges may remain ({e})");
        }
    }
    for [family, table] in [["inet", "prism"], ["ip", "prism-nat"], ["bridge", "prism"]] {
        // A table that does not exist makes nft fail; that is the normal case
        // for tables of features the node never used.
        let exists = Command::new("nft")
            .args(["list", "table", family, table])
            .output()
            .is_ok_and(|o| o.status.success());
        if exists {
            run_cleanup(
                &format!("nft table {family} {table}"),
                "nft",
                &["delete", "table", family, table],
            );
        }
    }
}

/// Runs `prism leave`.
///
/// Without `force` it asks the running daemon to leave the cluster and waits
/// up to 30 seconds for it to release `run/prism.lock`. With `force` the
/// daemon must already be stopped. It then deletes `data/` and `wg/` under
/// `dir`. Fails if the daemon is still serving or the deletion fails.
pub async fn leave(dir: &Path, force: bool) -> anyhow::Result<()> {
    if !force {
        let cfg = Config::load(dir)?;
        crate::cli::call(&cfg, axum::http::Method::POST, "/cluster/leave", None).await?;
        println!("Removed this node from the cluster; waiting for prism to stop...");
    }
    // The daemon exits by itself after a graceful leave; with --force it
    // must already be stopped.
    let mut lock = None;
    for _ in 0..if force { 1 } else { 60 } {
        match crate::serve::lock_dir(dir) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_prism_links() {
        for ours in ["ps0", "ps12", "vx100"] {
            assert!(is_prism_link(ours), "{ours}");
        }
        for other in ["ps", "vx", "psx1", "eth0", "wg-prism", "br-ps1", ""] {
            assert!(!is_prism_link(other), "{other}");
        }
    }

    #[test]
    fn parses_link_listing() {
        let out = "lo UNKNOWN 127.0.0.1/8\nps1@NONE UP\nvx5 UP\n\n";
        assert_eq!(link_names(out).collect::<Vec<_>>(), ["lo", "ps1", "vx5"]);
    }
}
