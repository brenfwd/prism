//! Developer tasks. Run with `cargo xtask <task>`.

mod cloudinit;
mod testbed;
mod virsh;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "cargo xtask", about = "Prism developer tasks")]
struct Cli {
    #[command(subcommand)]
    task: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    /// Nested-VM testbed on the system libvirt (`qemu:///system`).
    Testbed {
        #[command(subcommand)]
        cmd: TestbedCmd,
    },
}

#[derive(Debug, Subcommand)]
enum TestbedCmd {
    /// Create (or resume) the testbed nodes and wait for cloud-init.
    Up {
        /// Number of nodes, named prism-tb-1..N.
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=testbed::MAX_NODES))]
        nodes: u8,
    },
    /// Show node state, addresses and cloud-init progress.
    Status,
    /// SSH into node N, optionally running a command.
    Ssh {
        node: u8,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
    /// Install the prism binary and service on every running node.
    Deploy {
        /// Static binary to copy (build it with cargo zigbuild, see README).
        #[arg(long, default_value = "target/x86_64-unknown-linux-musl/release/prism")]
        binary: PathBuf,
    },
    /// Remove the nodes, their disks and seeds.
    Down {
        /// Also remove the base image, the cached download, the storage
        /// pool and the isolated network.
        #[arg(long)]
        all: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let Task::Testbed { cmd } = cli.task;
    let tb = testbed::Testbed::new()?;
    match cmd {
        TestbedCmd::Up { nodes } => tb.up(nodes),
        TestbedCmd::Status => tb.status(),
        TestbedCmd::Ssh { node, cmd } => tb.ssh(node, &cmd),
        TestbedCmd::Deploy { binary } => tb.deploy(&binary),
        TestbedCmd::Down { all } => tb.down(all),
    }
}
