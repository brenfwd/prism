//! Prism: a libvirt/KVM manager in one static binary.

mod api;
mod archive;
mod cli;
mod cluster;
mod config;
mod db;
mod events;
mod network;
mod node;
mod render;
mod spec;
mod state;
mod storage;
mod tasks;
mod ui;

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Config;
use crate::db::{NodeDb, StateDb};
use crate::events::Bus;
use crate::node::Node;
use crate::state::State;
use crate::storage::{Pool, Storage};
use crate::tasks::Tasks;

#[derive(Debug, Parser)]
#[command(name = "prism", version, about = "libvirt/KVM manager")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

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
    /// Join a cluster with an invite from `prism cluster invite`.
    Join {
        #[arg(long)]
        config: PathBuf,
        invite: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct Identity {
    node_id: Uuid,
    machine_id: String,
}

fn machine_id() -> String {
    std::fs::read_to_string("/etc/machine-id")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
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
            tokio::runtime::Runtime::new()?.block_on(serve(&config, adopt))
        }
        Command::Doctor { config } => tokio::runtime::Runtime::new()?.block_on(doctor(&config)),
        Command::Cluster { config, cmd } => {
            tokio::runtime::Runtime::new()?.block_on(cli::cluster(&config, cmd))
        }
        Command::Join { config, invite } => {
            tokio::runtime::Runtime::new()?.block_on(cli::join(&config, &invite))
        }
    }
}

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

fn load_identity(dir: &Path, adopt: bool) -> anyhow::Result<Uuid> {
    let path = dir.join("data/identity");
    let mid = machine_id();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let mut id: Identity = serde_json::from_str(&text).context("parsing data/identity")?;
            if id.machine_id != mid {
                if !adopt {
                    bail!(
                        "this config directory belongs to another machine (machine-id {}); \
                         pass --adopt if it was moved here deliberately",
                        id.machine_id
                    );
                }
                id.machine_id = mid;
                std::fs::write(&path, serde_json::to_string_pretty(&id)?)?;
            }
            Ok(id.node_id)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let id = Identity {
                node_id: Uuid::new_v4(),
                machine_id: mid,
            };
            std::fs::write(&path, serde_json::to_string_pretty(&id)?)?;
            Ok(id.node_id)
        }
        Err(e) => Err(e).context("reading data/identity"),
    }
}

/// Takes `run/prism.lock` so a second instance refuses to start.
fn lock_dir(dir: &Path) -> anyhow::Result<std::fs::File> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir.join("run"))?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("run/prism.lock"))?;
    rustix::fs::flock(&f, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| anyhow::anyhow!("another prism is already serving {}", dir.display()))?;
    Ok(f)
}

async fn serve(dir: &Path, adopt: bool) -> anyhow::Result<()> {
    let cfg = Config::load(dir)?;
    let _lock = lock_dir(dir)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir.join("data"))?;
    let node_id = load_identity(dir, adopt)?;
    let state_db = StateDb::open(&dir.join("data/state.db"))?;
    let node_db = NodeDb::open(&dir.join("data/node.db"))?;
    let boot_epoch: u64 = state_db
        .meta("boot_epoch")?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
        + 1;
    state_db.set_meta("boot_epoch", &boot_epoch.to_string())?;
    // Autostart VMs only once per host boot, not on every Prism restart.
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let host_booted = state_db.meta("host_boot_id")?.as_deref() != Some(boot_id.as_str());
    state_db.set_meta("host_boot_id", &boot_id)?;
    let state = State::new(state_db, node_id);

    let storage = Storage::new(cfg.storage.pool_roots.clone());
    for (name, p) in &cfg.pools {
        storage.insert(
            Pool::open(name, &p.path, &p.format, true)
                .with_context(|| format!("opening pool {name}"))?,
        );
    }
    for p in state.pools(node_id)? {
        if storage.get(&p.name).is_ok() {
            tracing::warn!(
                "pool {} is defined in prism.toml and the API; using prism.toml",
                p.name
            );
            continue;
        }
        match Pool::open(&p.name, &p.path, &p.format, false) {
            Ok(pool) => storage.insert(pool),
            Err(e) => tracing::error!("pool {}: {e}", p.name),
        }
    }

    let name = cfg.node.name.clone().unwrap_or_else(|| {
        rustix::system::uname()
            .nodename()
            .to_string_lossy()
            .into_owned()
    });
    let mut archive = archive::Archive::default();
    for (mname, m) in &cfg.archive_members {
        let member = archive::LocalMember::open(mname, &m.pool, &m.path)
            .with_context(|| format!("opening archive member {mname}"))?;
        archive.members.insert(mname.clone(), Arc::new(member));
    }

    let bus = Arc::new(Bus::new(node_id.to_string(), boot_epoch));
    let tasks = Tasks::new(node_db, Arc::clone(&bus), node_id);
    let node = Node::new(
        node_id,
        name.clone(),
        cfg.clone(),
        state.clone(),
        storage,
        archive,
        bus,
        tasks,
        host_booted,
    );
    let app = api::router(Arc::clone(&node));

    {
        let mesh = cluster::wg::mesh(dir);
        let c = cluster::Cluster::new(
            node_id,
            name,
            state,
            mesh,
            cluster::ClusterConfig {
                advertise: cfg.cluster.advertise.clone(),
                wireguard_port: cfg.cluster.wireguard_port,
                data_dir: dir.join("data"),
            },
        )?;
        let _ = c.node.set(Arc::downgrade(&node));
        let _ = c.http.set(app.clone());
        if let Err(e) = c.resume().await {
            tracing::error!("resuming cluster membership: {e:#}");
        }
        let _ = node.cluster.set(c);
    }
    node.start();
    node.register_archive_members().await;

    let mut servers = tokio::task::JoinSet::new();
    for addr in &cfg.http.listen {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding {addr}"))?;
        tracing::info!("listening on http://{addr}");
        let app = app.clone();
        servers.spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown())
                .await
        });
    }
    while let Some(res) = servers.join_next().await {
        res??;
    }
    tracing::info!("stopped");
    Ok(())
}

async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("installing SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

async fn doctor(dir: &Path) -> anyhow::Result<()> {
    let cfg = Config::load(dir)?;
    let mut ok = true;
    let mut check = |name: &str, res: Result<String, String>| match res {
        Ok(detail) => println!("  ok    {name}: {detail}"),
        Err(detail) => {
            ok = false;
            println!("  FAIL  {name}: {detail}");
        }
    };
    let sock = cfg.libvirt_socket();
    let lv = match prism_libvirt::Client::connect(&sock, &cfg.libvirt.uri).await {
        Ok(c) => c
            .lib_version()
            .await
            .map(|v| format!("{} via {}", v, sock.display()))
            .map_err(|e| e.to_string()),
        Err(e) => Err(format!("{}: {e}", sock.display())),
    };
    check("libvirt", lv);
    let kvm = Path::new("/dev/kvm").exists();
    check(
        "kvm",
        match (kvm, cfg.domain_type()) {
            (true, t) => Ok(format!("/dev/kvm present; domain type {t}")),
            (false, "qemu") => Ok("/dev/kvm missing; using TCG emulation as configured".into()),
            (false, _) => {
                Err("/dev/kvm missing; set libvirt.domain_type = \"qemu\" for TCG".into())
            }
        },
    );
    let qi = tokio::process::Command::new(&cfg.libvirt.qemu_img)
        .arg("--version")
        .output()
        .await;
    check(
        "qemu-img",
        match qi {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .into()),
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).into()),
            Err(e) => Err(format!("{}: {e}", cfg.libvirt.qemu_img.display())),
        },
    );
    for (name, p) in &cfg.pools {
        check(
            &format!("pool {name}"),
            Pool::open(name, &p.path, &p.format, true)
                .map(|p| p.path.display().to_string())
                .map_err(|e| e.to_string()),
        );
    }
    if ok {
        Ok(())
    } else {
        bail!("some checks failed")
    }
}
