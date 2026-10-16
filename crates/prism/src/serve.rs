//! `prism serve`: opens the databases, pools and cluster state under a config
//! directory and runs the HTTP API until SIGINT or SIGTERM.
//!
//! The directory holds `prism.toml` (settings), `data/` (`identity`,
//! `state.db`, `node.db`), `wg/` (`WireGuard` keys) and `run/prism.lock`
//! (the single-instance lock).

use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::Config;
use crate::db::{NodeDb, StateDb};
use crate::events::Bus;
use crate::node::Node;
use crate::state::State;
use crate::storage::{Pool, Storage};
use crate::tasks::Tasks;
use crate::{api, archive, cluster};

/// Contents of `data/identity`: which node this directory belongs to and the
/// machine it was created on.
#[derive(Debug, Serialize, Deserialize)]
struct Identity {
    node_id: Uuid,
    machine_id: String,
}

/// The host's `/etc/machine-id`, or an empty string when it cannot be read.
fn machine_id() -> String {
    std::fs::read_to_string("/etc/machine-id")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Reads or creates `data/identity` under `dir` and returns the node ID.
///
/// A directory created on another machine (different machine-id) is refused
/// unless `adopt` is set, in which case the identity is rewritten for this
/// machine. Fails when the file cannot be read, parsed or written.
pub(crate) fn load_identity(dir: &Path, adopt: bool) -> anyhow::Result<Uuid> {
    let path = dir.join("data/identity");
    let mid = machine_id();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let mut id: Identity = serde_json::from_str(&text).context("parsing data/identity")?;
            if id.machine_id != mid {
                if !adopt {
                    anyhow::bail!(
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

/// Takes `run/prism.lock` under `dir` so a second instance refuses to start.
///
/// The lock lasts as long as the returned file is open. Fails when the
/// directory cannot be created or another process holds the lock.
pub(crate) fn lock_dir(dir: &Path) -> anyhow::Result<std::fs::File> {
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

/// Everything `serve` reads from the on-disk databases before building the
/// node.
struct Opened {
    node_id: Uuid,
    state: State,
    node_db: NodeDb,
    boot_epoch: u64,
    host_booted: bool,
}

/// The kernel's ID for the current host boot, if readable.
fn read_boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|s| s.trim().to_string())
        .map_err(|e| tracing::warn!("reading the boot ID: {e}; VMs will not autostart"))
        .ok()
}

/// The epoch for this start: the stored value plus one. An unparsable stored
/// value restarts the count at 1 and is logged.
fn next_boot_epoch(stored: Option<&str>) -> u64 {
    let previous = match stored {
        None => 0,
        Some(s) => s.parse().unwrap_or_else(|e| {
            tracing::warn!("boot_epoch {s:?} in state.db is not a number ({e}); restarting at 1");
            0
        }),
    };
    previous + 1
}

/// Opens `data/state.db` and `data/node.db`, loads the node identity and
/// records this start: increments `boot_epoch` and compares the kernel boot
/// ID with the stored one to decide whether the host itself just booted
/// (VMs autostart once per host boot, not on every Prism restart).
fn open_state(dir: &Path, adopt: bool) -> anyhow::Result<Opened> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir.join("data"))?;
    let node_id = load_identity(dir, adopt)?;
    let state_db = StateDb::open(&dir.join("data/state.db"))?;
    let node_db = NodeDb::open(&dir.join("data/node.db"))?;
    let boot_epoch = next_boot_epoch(state_db.meta("boot_epoch")?.as_deref());
    state_db.set_meta("boot_epoch", &boot_epoch.to_string())?;
    // With an unreadable boot ID, treat the start as a restart: autostarting
    // every VM on each restart is worse than skipping it after a reboot.
    let host_booted = match read_boot_id() {
        Some(id) => {
            let fresh = state_db.meta("host_boot_id")?.as_deref() != Some(id.as_str());
            state_db.set_meta("host_boot_id", &id)?;
            fresh
        }
        None => false,
    };
    Ok(Opened {
        node_id,
        state: State::new(state_db, node_id),
        node_db,
        boot_epoch,
        host_booted,
    })
}

/// Opens the pools from `prism.toml` (which must work) and then the ones
/// defined through the API (which are skipped with an error when broken). A
/// name in both places uses the `prism.toml` definition.
fn open_pools(cfg: &Config, state: &State, node_id: Uuid) -> anyhow::Result<Storage> {
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
    Ok(storage)
}

/// Opens the archive member directories listed in `prism.toml`.
fn open_archive(cfg: &Config) -> anyhow::Result<archive::Archive> {
    let mut archive = archive::Archive::default();
    for (name, m) in &cfg.archive_members {
        let member = archive::LocalMember::open(name, &m.pool, &m.path)
            .with_context(|| format!("opening archive member {name}"))?;
        archive.members.insert(name.clone(), Arc::new(member));
    }
    Ok(archive)
}

/// Creates the cluster runtime, links it with the node and the HTTP app, and
/// resumes membership from disk. A failed resume is logged, not fatal: the
/// node then serves as standalone until the operator intervenes.
async fn start_cluster(
    dir: &Path,
    cfg: &Config,
    node: &Arc<Node>,
    app: &axum::Router,
    name: String,
    state: State,
) -> anyhow::Result<()> {
    let c = cluster::Cluster::new(
        node.id,
        name,
        state,
        cluster::wg::mesh(dir),
        cluster::ClusterConfig {
            advertise: cfg.cluster.advertise.clone(),
            wireguard_port: cfg.cluster.wireguard_port,
            data_dir: dir.join("data"),
        },
    )?;
    // The cells are new, so these sets cannot fail.
    c.node
        .set(Arc::downgrade(node))
        .expect("cluster node link is set once at startup");
    c.http
        .set(app.clone())
        .expect("cluster http link is set once at startup");
    if let Err(e) = c.resume().await {
        tracing::error!("resuming cluster membership: {e:#}");
    }
    node.cluster
        .set(c)
        .unwrap_or_else(|_| unreachable!("node cluster is set once at startup"));
    Ok(())
}

/// Installs SIGINT and SIGTERM handlers and returns a token that is
/// cancelled when either arrives. Fails if a handler cannot be installed.
fn shutdown_token() -> anyhow::Result<CancellationToken> {
    let mut term = signal(SignalKind::terminate()).context("installing SIGTERM handler")?;
    let mut int = signal(SignalKind::interrupt()).context("installing SIGINT handler")?;
    let token = CancellationToken::new();
    let t = token.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = int.recv() => {}
            _ = term.recv() => {}
        }
        t.cancel();
    });
    Ok(token)
}

/// Runs the node described by the config in `dir` until it is told to stop.
///
/// Fails if the config is invalid, another instance holds the lock, a
/// database or `prism.toml` pool cannot be opened, or a listen address cannot
/// be bound.
pub async fn serve(dir: &Path, adopt: bool) -> anyhow::Result<()> {
    let cfg = Config::load(dir)?;
    let _lock = lock_dir(dir)?;
    let opened = open_state(dir, adopt)?;
    let Opened {
        node_id,
        state,
        node_db,
        boot_epoch,
        host_booted,
    } = opened;
    let storage = open_pools(&cfg, &state, node_id)?;
    let name = cfg.node.name.clone().unwrap_or_else(|| {
        rustix::system::uname()
            .nodename()
            .to_string_lossy()
            .into_owned()
    });
    let archive = open_archive(&cfg)?;
    let shutdown = shutdown_token()?;

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
    start_cluster(dir, &cfg, &node, &app, name, state).await?;
    node.start();
    node.register_archive_members().await;

    let mut servers = tokio::task::JoinSet::new();
    for addr in &cfg.http.listen {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding {addr}"))?;
        tracing::info!("listening on http://{addr}");
        let app = app.clone();
        let stop = shutdown.clone().cancelled_owned();
        servers.spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(stop)
                .await
        });
    }
    while let Some(res) = servers.join_next().await {
        res??;
    }
    tracing::info!("stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_epoch_counts_up() {
        assert_eq!(next_boot_epoch(None), 1);
        assert_eq!(next_boot_epoch(Some("41")), 42);
        assert_eq!(next_boot_epoch(Some("garbage")), 1);
    }

    #[test]
    fn identity_is_stable_and_machine_bound() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("data")).unwrap();
        let first = load_identity(dir.path(), false).unwrap();
        assert_eq!(load_identity(dir.path(), false).unwrap(), first);
        let path = dir.path().join("data/identity");
        let moved = Identity {
            node_id: first,
            machine_id: "another-machine".into(),
        };
        std::fs::write(&path, serde_json::to_string(&moved).unwrap()).unwrap();
        assert!(load_identity(dir.path(), false).is_err());
        assert_eq!(load_identity(dir.path(), true).unwrap(), first);
    }

    #[test]
    fn second_lock_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let held = lock_dir(dir.path()).unwrap();
        assert!(lock_dir(dir.path()).is_err());
        drop(held);
        assert!(lock_dir(dir.path()).is_ok());
    }
}
