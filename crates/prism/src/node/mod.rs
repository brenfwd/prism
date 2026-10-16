//! The local node: libvirt connection supervision, VM runtime state and the
//! operations that act on this node's VMs.

mod apparmor;
mod archive_health;
mod archive_io;
mod archive_repair;
mod backups;
mod balloon;
mod clone;
mod devices;
mod download;
mod forwards;
mod guest;
mod iso_cache;
mod libvirt;
mod migrate;
mod migrate_setup;
mod model;
mod moves;
mod networks;
mod remote;
mod runtime;
mod seed_xml;
mod snapshot_move;
mod snapshot_xml;
mod snapshots;
mod state_events;
mod vlan_subnets;
mod vlans;
mod vm_delete;
mod vms;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use prism_libvirt::proto::{error_codes, flags};
use prism_libvirt::{Client, Domain, DomainEvent};
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

use crate::config::Config;
use crate::events::{Bus, Op};
use crate::render::{self, PoolLoc, RenderCtx};
use crate::spec::VmSpec;
use crate::state::{Command, State, StateError};
use crate::storage::{Storage, StorageError};
use crate::tasks::Tasks;

pub use backups::{BACKUPS, BackupDoc, CreateBackup, RestoreBackup};
pub use balloon::BalloonStatus;
pub use clone::CloneVm;
pub use devices::{AddDisk, AddNic, UpdateDisk};
pub use download::DownloadReq;
pub use guest::GuestNetwork;
pub use migrate::{FinishReq, MigrateVm, PrepareReq};
pub use model::{
    Action, HostInfo, HotplugResult, MemoryStats, NodeView, VmRuntime, VmState, VmView,
};
pub use moves::MoveVm;
pub use networks::{NetworkCheck, NetworkView};
pub use vlans::SetVlans;
pub use vm_delete::DeletePlan;

pub(crate) use libvirt::undefine_flags;
pub use snapshots::{CreateSnapshot, SnapshotView};

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Invalid(String),
    #[error("libvirt is not connected")]
    Unavailable,
    /// The cluster cannot commit changes (no hub/leader or no quorum).
    #[error("cluster is degraded: {0}")]
    Degraded(String),
    #[error(transparent)]
    Libvirt(#[from] prism_libvirt::Error),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<StateError> for NodeError {
    fn from(e: StateError) -> Self {
        match e {
            StateError::Conflict(m) => Self::Conflict(m),
            StateError::NotFound(m) => Self::NotFound(m),
            StateError::Degraded(m) => Self::Degraded(m),
            StateError::Internal(e) => Self::Internal(e),
        }
    }
}

pub type Result<T, E = NodeError> = std::result::Result<T, E>;

#[derive(Debug)]
pub struct Node {
    pub id: Uuid,
    pub name: String,
    pub cfg: Config,
    pub state: State,
    pub storage: Storage,
    pub archive: crate::archive::Archive,
    pub bus: Arc<Bus>,
    pub tasks: Tasks,
    libvirt: watch::Sender<Option<Client>>,
    runtime: Mutex<HashMap<Uuid, VmRuntime>>,
    host: Mutex<HostInfo>,
    pressure: Mutex<Option<balloon::HostPressure>>,
    /// Domain events from the current libvirt connection, for waiters.
    domain_events: broadcast::Sender<DomainEvent>,
    /// Serialises spec mutations per process.
    ops: tokio::sync::Mutex<()>,
    /// Starts autostart VMs on the first libvirt connection after host boot.
    autostart_pending: std::sync::atomic::AtomicBool,
    /// Cluster membership, once the mesh layer is available.
    pub cluster: std::sync::OnceLock<Arc<crate::cluster::Cluster>>,
    /// Runtime state of VMs on other nodes, from their event streams.
    remote_runtime: Mutex<HashMap<Uuid, VmRuntime>>,
    /// Other nodes' status, from their event streams.
    remote_nodes: Mutex<HashMap<Uuid, NodeView>>,
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn domain_ref(spec: &VmSpec) -> Domain {
    Domain {
        name: spec.name.clone(),
        uuid: *spec.id.as_bytes(),
        id: -1,
    }
}

/// Whether a libvirt error means "the domain is not running".
pub(crate) fn not_running(e: &prism_libvirt::Error) -> bool {
    matches!(
        e.remote_code(),
        Some(error_codes::OPERATION_INVALID | error_codes::NO_DOMAIN)
    )
}

impl Node {
    #[allow(clippy::too_many_arguments)] // assembled once, in main
    pub fn new(
        id: Uuid,
        name: String,
        cfg: Config,
        state: State,
        storage: Storage,
        archive: crate::archive::Archive,
        bus: Arc<Bus>,
        tasks: Tasks,
        host_booted: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            name,
            cfg,
            state,
            storage,
            archive,
            bus,
            tasks,
            libvirt: watch::channel(None).0,
            runtime: Mutex::default(),
            host: Mutex::default(),
            pressure: Mutex::default(),
            domain_events: broadcast::channel(1024).0,
            ops: tokio::sync::Mutex::new(()),
            autostart_pending: host_booted.into(),
            cluster: std::sync::OnceLock::new(),
            remote_runtime: Mutex::default(),
            remote_nodes: Mutex::default(),
        })
    }

    /// Registers this node's archive member directories in the shared
    /// index (idempotent).
    pub async fn register_archive_members(&self) {
        let existing = self.state.archive_members().unwrap_or_default();
        for (name, m) in &self.cfg.archive_members {
            let doc = crate::archive::ArchiveMemberDoc {
                node: self.id,
                name: name.clone(),
                pool: m.pool.clone(),
                weight: m.weight,
                reserve: m.reserve,
                state: existing
                    .iter()
                    .find(|e| e.node == self.id && e.name == *name)
                    .map_or(crate::archive::MemberState::Active, |e| e.state),
            };
            if existing.contains(&doc) {
                continue;
            }
            if let Err(e) = self
                .state
                .apply(Command::PutArchiveMember { member: doc })
                .await
            {
                tracing::warn!(member = %name, "registering archive member: {e}");
            }
        }
    }

    /// Spawns the libvirt supervisor, the state watcher, the stats loop and
    /// the balloon policy loop.
    pub fn start(self: &Arc<Self>) {
        tokio::spawn(Arc::clone(self).supervise());
        tokio::spawn(Arc::clone(self).watch_state());
        tokio::spawn(Arc::clone(self).stats_loop());
        tokio::spawn(Arc::clone(self).balloon_loop());
        tokio::spawn(Arc::clone(self).archive_health_loop());
        tokio::spawn(Arc::clone(self).iso_cache_loop());
        tokio::spawn(
            crate::network::Agent::new(self.id, self.state.clone(), self.domain_events.clone())
                .run(),
        );
        tokio::spawn(Arc::clone(self).seed_default_network());
    }

    pub(crate) fn client(&self) -> Result<Client> {
        self.libvirt
            .borrow()
            .clone()
            .filter(|c| !c.is_closed())
            .ok_or(NodeError::Unavailable)
    }

    /// libvirt's version as `major * 1_000_000 + minor * 1000 + micro`.
    pub(crate) fn libvirt_version(&self) -> u64 {
        lock(&self.host).libvirt_version_num
    }

    pub fn view(&self) -> NodeView {
        NodeView {
            id: self.id,
            name: self.name.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
            libvirt_connected: self.client().is_ok(),
            domain_type: self.cfg.domain_type().into(),
            host: lock(&self.host).clone(),
            memory_pressure: lock(&self.pressure).clone(),
            archive: self.archive.capacity(),
            reachable: true,
        }
    }

    fn publish_node(&self) {
        self.bus.publish(
            "node.status",
            format!("/nodes/{}", self.id),
            Op::Upsert,
            serde_json::to_value(self.view()).unwrap_or_default(),
        );
    }

    pub(crate) fn vm_view(&self, mut spec: VmSpec) -> VmView {
        let status = if spec.node == self.id {
            lock(&self.runtime)
                .get(&spec.id)
                .cloned()
                .unwrap_or_default()
        } else {
            lock(&self.remote_runtime)
                .get(&spec.id)
                .cloned()
                .unwrap_or_default()
        };
        // User-data may hold secrets; the API only shows a fingerprint.
        if let Some(ci) = &mut spec.cloud_init {
            ci.user_data = format!("blake3:{}", blake3::hash(ci.user_data.as_bytes()).to_hex());
        }
        let nic_ipv6 = self.nic_ipv6(&spec);
        VmView {
            spec,
            status,
            nic_ipv6,
            hotplug: None,
        }
    }

    pub fn vms(&self) -> Result<Vec<VmView>> {
        Ok(self
            .state
            .vms()?
            .into_iter()
            .map(|s| self.vm_view(s))
            .collect())
    }

    pub fn spec(&self, id: Uuid) -> Result<VmSpec> {
        let mut spec = self
            .state
            .vm(id)?
            .ok_or_else(|| NodeError::NotFound(format!("vm {id}")))?;
        spec.assign_targets();
        Ok(spec)
    }

    pub fn vm(&self, id: Uuid) -> Result<VmView> {
        Ok(self.vm_view(self.spec(id)?))
    }

    /// The spec of a VM this node owns, for operations that act on it.
    pub(crate) fn local_spec(&self, id: Uuid) -> Result<VmSpec> {
        let spec = self.spec(id)?;
        if spec.node != self.id {
            return Err(NodeError::Conflict(format!(
                "vm {} is owned by node {}",
                spec.name, spec.node
            )));
        }
        Ok(spec)
    }

    /// Writes a VM spec through the state layer, enforcing that nobody
    /// changed it since `base` was read.
    pub(crate) async fn put_spec(&self, spec: &VmSpec, base: Option<u64>) -> Result<()> {
        self.state
            .apply(Command::PutVm {
                vm: Box::new(spec.clone()),
                expect_generation: Some(base),
            })
            .await?;
        Ok(())
    }

    pub(crate) fn pool_loc(&self, name: &str) -> Option<PoolLoc> {
        self.storage.get(name).ok().map(|p| PoolLoc {
            path: p.path.clone(),
            direct_io: p.direct_io,
        })
    }

    pub(crate) fn render_ctx<'a>(
        &'a self,
        pool: &'a dyn Fn(&str) -> Option<PoolLoc>,
        seed: Option<&'a std::path::Path>,
        bridges: &'a HashMap<Uuid, String>,
    ) -> RenderCtx<'a> {
        RenderCtx {
            domain_type: self.cfg.domain_type(),
            emulator: self.cfg.libvirt.emulator.as_deref(),
            uefi_loader: self.cfg.libvirt.uefi_loader.as_deref(),
            pool,
            seed_iso: seed,
            bridges,
        }
    }

    /// Re-grants `virt-aa-helper` read access to every pool directory.
    pub(crate) fn sync_pool_apparmor(&self) {
        let dirs: Vec<_> = self.storage.list().iter().map(|p| p.path.clone()).collect();
        apparmor::allow_pool_dirs(&dirs);
    }

    pub(crate) async fn define(&self, c: &Client, spec: &VmSpec) -> Result<Domain> {
        let seed = self.seed_iso(spec)?;
        let pool = |p: &str| self.pool_loc(p);
        let bridges = self.network_bridges();
        let xml = render::render(spec, &self.render_ctx(&pool, seed.as_deref(), &bridges))
            .map_err(NodeError::Invalid)?;
        Ok(c.define_xml(&xml, flags::DOMAIN_DEFINE_VALIDATE).await?)
    }

    /// Names of VMs whose disks use `pool/volume`.
    pub fn volume_users(&self, pool: &str, volume: &str) -> Result<Vec<String>> {
        Ok(self
            .state
            .vms()?
            .into_iter()
            .filter(|s| {
                s.node == self.id
                    && s.disks.iter().any(|d| {
                        d.pool == pool
                            && (d.volume.as_deref() == Some(volume)
                                || d.backing.iter().any(|b| b == volume))
                    })
            })
            .map(|s| s.name)
            .collect())
    }

    /// Names of VMs with disks in `pool`.
    pub fn pool_users(&self, pool: &str) -> Result<Vec<String>> {
        Ok(self
            .state
            .vms()?
            .into_iter()
            .filter(|s| s.node == self.id && s.disks.iter().any(|d| d.pool == pool))
            .map(|s| s.name)
            .collect())
    }
}
