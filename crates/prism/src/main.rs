//! The `prism` binary: a libvirt/KVM VM manager with a web UI and optional
//! multi-node clustering.
//!
//! Subcommands:
//!
//! - `init` creates a config directory with a default `prism.toml`.
//! - `serve` runs the node and its HTTP API (default listen address is in
//!   `prism.toml`).
//! - `doctor` checks libvirt, `/dev/kvm`, `qemu-img`, optional host programs
//!   and the configured pools.
//! - `cluster`, `join` and `leave` manage cluster membership through the
//!   running daemon's API.
//!
//! Every subcommand takes `--config <dir>`. The directory holds `prism.toml`
//! (settings), `data/` (`identity`, `state.db` for replicated state and
//! `node.db` for this node's task history), `wg/` (`WireGuard` keys) and
//! `run/prism.lock` (held while `serve` runs). Directories are created with
//! mode 0700 and files with 0600.

mod api;
mod archive;
mod cli;
mod cluster;
mod config;
mod db;
mod doctor;
mod events;
mod fetch;
mod leave;
mod network;
mod node;
mod prereqs;
mod render;
mod serve;
mod spec;
mod state;
mod storage;
mod tasks;
mod ui;

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::bail;
use clap::{Parser, Subcommand};

/// Command line of the `prism` binary.
#[derive(Debug, Parser)]
#[command(name = "prism", version, about = "libvirt/KVM manager")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The subcommands; each takes the config directory with `--config`.
#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new config directory.
    Init {
        #[arg(long)]
        config: PathBuf,
        /// Directory for the default VM disk pool.
        #[arg(long, default_value = "/var/lib/prism/pools/default")]
        pool_path: PathBuf,
    },
    /// Run the node.
    Serve {
        #[arg(long)]
        config: PathBuf,
        /// Accept a config directory copied from another machine.
        #[arg(long)]
        adopt: bool,
    },
    /// Check host prerequisites.
    Doctor {
        #[arg(long)]
        config: PathBuf,
    },
    /// Manage the cluster this node belongs to.
    Cluster {
        #[arg(long)]
        config: PathBuf,
        #[command(subcommand)]
        cmd: cli::ClusterCmd,
    },
    /// Leave the cluster and wipe this node's state.
    Leave {
        #[arg(long)]
        config: PathBuf,
        /// Wipe offline, without telling the cluster (stop `prism serve`
        /// first); remove the node from another member afterwards.
        #[arg(long)]
        force: bool,
    },
    /// Join a cluster with an invite from `prism cluster invite`.
    Join {
        #[arg(long)]
        config: PathBuf,
        invite: String,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .init();
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));

    match Cli::parse().command {
        Command::Init { config, pool_path } => init(&config, &pool_path),
        Command::Serve { config, adopt } => {
            tokio::runtime::Runtime::new()?.block_on(serve::serve(&config, adopt))
        }
        Command::Doctor { config } => {
            tokio::runtime::Runtime::new()?.block_on(doctor::doctor(&config))
        }
        Command::Cluster { config, cmd } => {
            tokio::runtime::Runtime::new()?.block_on(cli::cluster(&config, cmd))
        }
        Command::Leave { config, force } => {
            tokio::runtime::Runtime::new()?.block_on(leave::leave(&config, force))
        }
        Command::Join { config, invite } => {
            tokio::runtime::Runtime::new()?.block_on(cli::join(&config, &invite))
        }
    }
}

/// Creates `dir` (mode 0700) with a `data/` subdirectory and a default
/// `prism.toml` whose default pool is at `pool_path`.
///
/// Fails if `prism.toml` already exists or a path cannot be written.
fn init(dir: &Path, pool_path: &Path) -> anyhow::Result<()> {
    let toml = dir.join(config::CONFIG_FILE);
    if toml.exists() {
        bail!("{} already exists", toml.display());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir.join("data"))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(&toml, config::default_toml(pool_path))?;
    println!(
        "Initialised {}. Review prism.toml, then run:",
        dir.display()
    );
    println!("  prism serve --config {}", dir.display());
    Ok(())
}
