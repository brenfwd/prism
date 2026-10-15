//! `cargo xtask testbed`: Ubuntu VMs on the system libvirt that run Prism
//! as root, with nested KVM, for clustering and networking tests.

mod deploy;
mod node;
mod provision;
mod ssh;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use crate::virsh::{self, virsh, virsh_ok};
use node::{NodeSpec, existing_nodes, node_ip};
use provision::{ensure_nat_network, ensure_pool, net_active, pool_exists};

pub const MAX_NODES: i64 = 50;

const POOL: &str = "prism-testbed";
const POOL_PATH: &str = "/var/lib/libvirt/images/prism-testbed";
const NET_NAT: &str = "default";
const NET_LAN: &str = "prism-tb";
const DOMAIN_PREFIX: &str = "prism-tb-";
const IMAGE_BASE_URL: &str = "https://cloud-images.ubuntu.com/resolute/current";
const IMAGE_FILE: &str = "resolute-server-cloudimg-amd64.img";
const BASE_VOL: &str = "ubuntu-26.04-base.qcow2";
const DISK_SIZE: &str = "20G";
const MEMORY_MIB: u32 = 4096;
const VCPUS: u32 = 4;
const SSH_USER: &str = "ubuntu";
const PRISM_PORT: u16 = 8080;

const IP_TIMEOUT: Duration = Duration::from_secs(180);
const SSH_TIMEOUT: Duration = Duration::from_secs(300);

pub struct Testbed {
    /// `target/testbed` in the workspace.
    dir: PathBuf,
}

fn step(msg: &str) {
    println!("==> {msg}");
}

fn info(msg: &str) {
    println!("    {msg}");
}

impl Testbed {
    pub fn new() -> anyhow::Result<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .context("locating the workspace root")?;
        let dir = root.join("target/testbed");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir })
    }

    fn key_path(&self) -> PathBuf {
        self.dir.join("id_ed25519")
    }

    // ---- up ----------------------------------------------------------

    pub fn up(&self, nodes: u8) -> anyhow::Result<()> {
        if !Path::new("/dev/kvm").exists() {
            bail!("/dev/kvm is missing on this host");
        }
        let pubkey = self.ensure_ssh_key()?;
        self.ensure_lan_network()?;
        ensure_nat_network()?;
        ensure_pool()?;
        self.ensure_base_volume()?;

        let specs: Vec<_> = (1..=nodes).map(|index| NodeSpec { index }).collect();
        for spec in &specs {
            self.ensure_node(spec, &pubkey)?;
        }

        step("Waiting for nodes");
        let mut failed = Vec::new();
        for spec in &specs {
            match self.wait_ready(spec) {
                Ok(ip) => info(&format!("{}: ready at {ip}", spec.domain())),
                Err(e) => {
                    info(&format!("{}: {e:#}", spec.domain()));
                    failed.push(spec.domain());
                }
            }
        }
        if !failed.is_empty() {
            bail!(
                "nodes not ready: {} (see `cargo xtask testbed status`)",
                failed.join(", ")
            );
        }
        println!();
        self.status()
    }

    // ---- status ------------------------------------------------------

    pub fn status(&self) -> anyhow::Result<()> {
        let specs = existing_nodes()?;
        if specs.is_empty() {
            println!("No testbed nodes. Run `cargo xtask testbed up`.");
            return Ok(());
        }
        println!(
            "{:<12} {:<9} {:<16} {:<14} {:<10} cloud-init",
            "NODE", "STATE", "MGMT IP", "LAN IP", "PRISM"
        );
        for spec in specs {
            let name = spec.domain();
            let state = domstate(&name).unwrap_or_else(|_| "?".into());
            let ip = node_ip(&spec);
            let (ci, prism) = match &ip {
                Some(ip) if state == "running" => (
                    self.remote_line(
                        ip,
                        "cloud-init status 2>/dev/null | sed -n 's/^status: //p'",
                    ),
                    self.remote_line(ip, "systemctl is-active prism 2>/dev/null || true"),
                ),
                _ => (String::from("-"), String::from("-")),
            };
            let lan = spec.lan_addr();
            println!(
                "{name:<12} {state:<9} {:<16} {:<14} {prism:<10} {ci}",
                ip.as_deref().unwrap_or("-"),
                lan.split('/').next().unwrap_or(&lan),
            );
        }
        println!(
            "\nSSH: cargo xtask testbed ssh N [cmd…]   (key {})",
            self.key_path().display()
        );
        Ok(())
    }

    // ---- down --------------------------------------------------------

    pub fn down(&self, all: bool) -> anyhow::Result<()> {
        for spec in existing_nodes()? {
            let name = spec.domain();
            step(&format!("Removing {name}"));
            if domstate(&name)? != "shut off" {
                virsh(&["destroy", &name])?;
            }
            virsh(&["undefine", &name])?;
            let _ = std::fs::remove_file(self.dir.join(format!("{name}.xml")));
            let _ = std::fs::remove_file(self.dir.join(spec.seed_vol()));
        }
        if pool_exists() {
            virsh(&["pool-refresh", POOL])?;
            let vols = virsh::volume_names(&virsh(&["vol-list", POOL])?);
            for vol in vols {
                if vol.starts_with(DOMAIN_PREFIX) || (all && vol == BASE_VOL) {
                    info(&format!("deleting volume {vol}"));
                    virsh(&["vol-delete", "--pool", POOL, &vol])?;
                }
            }
        }
        if all {
            if pool_exists() {
                step(&format!("Removing pool {POOL}"));
                let _ = virsh(&["pool-destroy", POOL]);
                // Removes the (now empty) directory.
                virsh(&["pool-delete", POOL])?;
                virsh(&["pool-undefine", POOL])?;
            }
            if virsh_ok(&["net-info", NET_LAN]) {
                step(&format!("Removing network {NET_LAN}"));
                if net_active(NET_LAN)? {
                    virsh(&["net-destroy", NET_LAN])?;
                }
                virsh(&["net-undefine", NET_LAN])?;
            }
            let image = self.dir.join(IMAGE_FILE);
            if image.exists() {
                info(&format!("deleting {}", image.display()));
                std::fs::remove_file(image)?;
            }
        }
        println!("Done.");
        Ok(())
    }
}

fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> anyhow::Result<T> {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return Ok(v);
        }
        if start.elapsed() > timeout {
            bail!("timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn path_str(p: &Path) -> anyhow::Result<&str> {
    p.to_str()
        .with_context(|| format!("non-UTF-8 path {}", p.display()))
}

fn domstate(name: &str) -> anyhow::Result<String> {
    Ok(virsh(&["domstate", name])?.trim().to_owned())
}
