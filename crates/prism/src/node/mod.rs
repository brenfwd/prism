//! The local node: libvirt connection supervision, VM runtime state and the
//! operations that act on this node's VMs.

mod balloon;
mod devices;
mod snapshots;
mod vms;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prism_libvirt::proto::{error_codes, event_ids, flags, memory_stat};
use prism_libvirt::{Client, Domain, DomainEvent, DomainState, LifecycleEvent};
use serde::Serialize;
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

use crate::config::Config;
use crate::events::{Bus, Op};
use crate::render::{self, PoolLoc, RenderCtx};
use crate::spec::VmSpec;
use crate::state::{Change, Command, State, StateError};
use crate::storage::{Storage, StorageError};
use crate::tasks::Tasks;

pub use balloon::BalloonStatus;
pub use devices::{AddDisk, AddNic, UpdateDisk};
pub use snapshots::{CreateSnapshot, SnapshotView};

const STATS_INTERVAL: Duration = Duration::from_secs(5);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmState {
    Stopped,
    Running,
    Paused,
    ShuttingDown,
    Crashed,
    Suspended,
    /// Not defined in libvirt (yet).
    Missing,
    Unknown,
}

impl From<DomainState> for VmState {
    fn from(s: DomainState) -> Self {
        match s {
            DomainState::Running | DomainState::Blocked => Self::Running,
            DomainState::Paused => Self::Paused,
            DomainState::Shutdown => Self::ShuttingDown,
            DomainState::Shutoff => Self::Stopped,
            DomainState::Crashed => Self::Crashed,
            DomainState::PmSuspended => Self::Suspended,
            DomainState::NoState | DomainState::Unknown(_) => Self::Unknown,
        }
    }
}

impl VmState {
    /// QEMU is running (possibly paused).
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Running | Self::Paused | Self::ShuttingDown | Self::Suspended
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[allow(clippy::struct_field_names)] // units in names match the API
pub struct MemoryStats {
    /// Current balloon size.
    pub actual_kib: Option<u64>,
    pub unused_kib: Option<u64>,
    pub available_kib: Option<u64>,
    pub usable_kib: Option<u64>,
    pub disk_caches_kib: Option<u64>,
    pub rss_kib: Option<u64>,
    pub major_faults: Option<u64>,
    pub swap_in_kib: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct VmRuntime {
    pub state: VmState,
    pub vcpus: u16,
    pub memory_kib: u64,
    pub max_memory_kib: u64,
    pub cpu_time_ns: u64,
    pub memory_stats: MemoryStats,
    /// Spec changes that only apply after the next cold boot.
    pub pending_restart: bool,
    /// The QEMU guest agent is connected.
    pub agent: bool,
    pub balloon: Option<BalloonStatus>,
}

impl Default for VmRuntime {
    fn default() -> Self {
        Self {
            state: VmState::Unknown,
            vcpus: 0,
            memory_kib: 0,
            max_memory_kib: 0,
            cpu_time_ns: 0,
            memory_stats: MemoryStats::default(),
            pending_restart: false,
            agent: false,
            balloon: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VmView {
    #[serde(flatten)]
    pub spec: VmSpec,
    pub status: VmRuntime,
}

#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct HostInfo {
    pub hostname: String,
    pub libvirt_version: String,
    pub libvirt_version_num: u64,
    pub cpu_model: String,
    pub cpus: i32,
    pub memory_kib: u64,
    pub free_memory_bytes: u64,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct NodeView {
    pub id: Uuid,
    pub name: String,
    pub version: String,
    pub libvirt_connected: bool,
    pub domain_type: String,
    pub host: HostInfo,
    pub memory_pressure: Option<balloon::HostPressure>,
    /// False when this view is a cached copy of an unreachable peer.
    #[serde(default = "yes")]
    pub reachable: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Shutdown,
    Stop,
    Reboot,
    Reset,
    Pause,
    Resume,
}

impl std::str::FromStr for Action {
    type Err = NodeError;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            "start" => Self::Start,
            "shutdown" => Self::Shutdown,
            "stop" => Self::Stop,
            "reboot" => Self::Reboot,
            "reset" => Self::Reset,
            "pause" => Self::Pause,
            "resume" => Self::Resume,
            _ => return Err(NodeError::NotFound(format!("action {s}"))),
        })
    }
}

#[derive(Debug)]
pub struct Node {
    pub id: Uuid,
    pub name: String,
    pub cfg: Config,
    pub state: State,
    pub storage: Storage,
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

/// What a peer sends first on `Events.Subscribe`.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct RuntimeSnapshot {
    pub node: NodeView,
    pub vms: HashMap<Uuid, VmRuntime>,
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

fn format_version(v: u64) -> String {
    format!("{}.{}.{}", v / 1_000_000, v / 1000 % 1000, v % 1000)
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

    /// Spawns the libvirt supervisor, the state watcher, the stats loop and
    /// the balloon policy loop.
    pub fn start(self: &Arc<Self>) {
        tokio::spawn(Arc::clone(self).supervise());
        tokio::spawn(Arc::clone(self).watch_state());
        tokio::spawn(Arc::clone(self).stats_loop());
        tokio::spawn(Arc::clone(self).balloon_loop());
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

    /// This node and every peer heard from.
    pub fn nodes(&self) -> Vec<NodeView> {
        let mut out = vec![self.view()];
        out.extend(lock(&self.remote_nodes).values().cloned());
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Runtime state of this node, for a peer's first event.
    pub fn runtime_snapshot(&self) -> RuntimeSnapshot {
        RuntimeSnapshot {
            node: self.view(),
            vms: lock(&self.runtime).clone(),
        }
    }

    pub fn apply_remote_snapshot(&self, snap: RuntimeSnapshot) {
        let node = snap.node.id;
        let owned: Vec<Uuid> = self
            .state
            .vms()
            .unwrap_or_default()
            .into_iter()
            .filter(|v| v.node == node)
            .map(|v| v.id)
            .collect();
        let mut rt = lock(&self.remote_runtime);
        for id in owned {
            rt.remove(&id);
        }
        rt.extend(snap.vms);
        drop(rt);
        lock(&self.remote_nodes).insert(node, snap.node);
    }

    /// Updates the peer caches from one of a peer's own events.
    pub fn apply_remote_event(&self, ev: &crate::events::Event) {
        let id = ev.resource.rsplit('/').next().and_then(|s| Uuid::parse_str(s).ok());
        match (ev.ty.as_str(), id) {
            ("vm.state" | "vm.stats", Some(id)) => {
                if let Ok(rt) = serde_json::from_value::<VmRuntime>(ev.payload.clone()) {
                    lock(&self.remote_runtime).insert(id, rt);
                }
            }
            ("node.status", Some(id)) => {
                if let Ok(v) = serde_json::from_value::<NodeView>(ev.payload.clone()) {
                    lock(&self.remote_nodes).insert(id, v);
                }
            }
            _ => {}
        }
    }

    /// Marks a peer unreachable in the cache.
    pub fn forget_remote(&self, node: Uuid) {
        if let Some(v) = lock(&self.remote_nodes).get_mut(&node) {
            v.libvirt_connected = false;
            v.reachable = false;
        }
    }

    pub(crate) fn vm_view(&self, mut spec: VmSpec) -> VmView {
        let status = if spec.node == self.id {
            lock(&self.runtime).get(&spec.id).cloned().unwrap_or_default()
        } else {
            lock(&self.remote_runtime).get(&spec.id).cloned().unwrap_or_default()
        };
        // User-data may hold secrets; the API only shows a fingerprint.
        if let Some(ci) = &mut spec.cloud_init {
            ci.user_data = format!("blake3:{}", blake3::hash(ci.user_data.as_bytes()).to_hex());
        }
        VmView { spec, status }
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

    pub(crate) fn runtime(&self, id: Uuid) -> VmRuntime {
        lock(&self.runtime).get(&id).cloned().unwrap_or_default()
    }

    pub(crate) fn update_runtime(&self, id: Uuid, f: impl FnOnce(&mut VmRuntime)) {
        let mut rt = self.runtime(id);
        f(&mut rt);
        self.set_runtime(id, rt);
    }

    fn set_runtime(&self, id: Uuid, rt: VmRuntime) {
        let payload = serde_json::to_value(&rt).unwrap_or_default();
        let ty = {
            let mut map = lock(&self.runtime);
            let ty = match map.get(&id) {
                Some(p) if *p == rt => return,
                Some(p) if p.state == rt.state && p.pending_restart == rt.pending_restart => {
                    "vm.stats"
                }
                _ => "vm.state",
            };
            map.insert(id, rt);
            ty
        };
        self.bus
            .publish(ty, format!("/vms/{id}"), Op::Upsert, payload);
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
    ) -> RenderCtx<'a> {
        RenderCtx {
            domain_type: self.cfg.domain_type(),
            emulator: self.cfg.libvirt.emulator.as_deref(),
            uefi_loader: self.cfg.libvirt.uefi_loader.as_deref(),
            pool,
            seed_iso: seed,
        }
    }

    pub(crate) async fn define(&self, c: &Client, spec: &VmSpec) -> Result<Domain> {
        let seed = self.seed_iso(spec)?;
        let pool = |p: &str| self.pool_loc(p);
        let xml = render::render(spec, &self.render_ctx(&pool, seed.as_deref()))
            .map_err(NodeError::Invalid)?;
        Ok(c.define_xml(&xml, flags::DOMAIN_DEFINE_VALIDATE).await?)
    }

    /// Re-reads one VM's runtime state from libvirt.
    pub(crate) async fn refresh(&self, spec: &VmSpec) {
        let Ok(c) = self.client() else { return };
        let dom = domain_ref(spec);
        let prev = self.runtime(spec.id);
        let rt = match c.get_info(&dom).await {
            Ok(info) => {
                let state = VmState::from(info.state);
                let memory_stats = if state.is_active() {
                    c.memory_stats(&dom)
                        .await
                        .map(|s| parse_mem_stats(&s))
                        .unwrap_or_default()
                } else {
                    MemoryStats::default()
                };
                VmRuntime {
                    state,
                    vcpus: info.nr_virt_cpu,
                    memory_kib: info.memory_kib,
                    max_memory_kib: info.max_mem_kib,
                    cpu_time_ns: info.cpu_time_ns,
                    memory_stats,
                    pending_restart: prev.pending_restart && state.is_active(),
                    agent: prev.agent && state.is_active(),
                    balloon: prev.balloon.filter(|_| state.is_active()),
                }
            }
            Err(e) if e.remote_code() == Some(error_codes::NO_DOMAIN) => VmRuntime {
                state: VmState::Missing,
                ..VmRuntime::default()
            },
            Err(e) => {
                tracing::warn!(vm = %spec.name, "get_info failed: {e}");
                return;
            }
        };
        self.set_runtime(spec.id, rt);
    }

    pub(crate) async fn refresh_id(&self, id: Uuid) {
        if let Ok(Some(spec)) = self.state.vm(id) {
            self.refresh(&spec).await;
        }
    }

    /// Waits for a domain event matching `f`, up to `timeout`.
    pub(crate) async fn wait_event<T>(
        &self,
        rx: &mut broadcast::Receiver<DomainEvent>,
        timeout: Duration,
        mut f: impl FnMut(&DomainEvent) -> Option<T>,
    ) -> Result<T> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Err(_) => {
                    return Err(NodeError::Internal(anyhow::anyhow!(
                        "timed out waiting for libvirt"
                    )));
                }
                Ok(Ok(ev)) => {
                    if let Some(v) = f(&ev) {
                        return Ok(v);
                    }
                }
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(broadcast::error::RecvError::Closed)) => return Err(NodeError::Unavailable),
            }
        }
    }

    pub(crate) fn subscribe_domain_events(&self) -> broadcast::Receiver<DomainEvent> {
        self.domain_events.subscribe()
    }

    async fn supervise(self: Arc<Self>) {
        let socket = self.cfg.libvirt_socket();
        let mut backoff = Duration::from_secs(1);
        loop {
            match Client::connect(&socket, &self.cfg.libvirt.uri).await {
                Ok(c) => {
                    backoff = Duration::from_secs(1);
                    if let Err(e) = self.on_connect(&c).await {
                        tracing::error!("libvirt setup failed: {e}");
                    } else {
                        tracing::info!("connected to libvirt at {}", socket.display());
                        c.closed().await;
                        tracing::warn!("libvirt connection lost");
                    }
                    self.libvirt.send_replace(None);
                    self.publish_node();
                }
                Err(e) => {
                    tracing::warn!("cannot connect to libvirt at {}: {e}", socket.display());
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(RECONNECT_MAX);
        }
    }

    async fn on_connect(self: &Arc<Self>, c: &Client) -> Result<()> {
        let mut events = c.events();
        for id in [
            event_ids::LIFECYCLE,
            event_ids::BALLOON_CHANGE,
            event_ids::BLOCK_JOB_2,
            event_ids::JOB_COMPLETED,
            event_ids::DEVICE_REMOVED,
            event_ids::AGENT_LIFECYCLE,
            event_ids::MIGRATION_ITERATION,
        ] {
            c.register_event(id).await?;
        }
        let n = c.node_info().await?;
        let version = c.lib_version().await?;
        *lock(&self.host) = HostInfo {
            hostname: c.hostname().await?,
            libvirt_version: format_version(version),
            libvirt_version_num: version,
            free_memory_bytes: c.free_memory().await?,
            cpu_model: n.model,
            cpus: n.cpus,
            memory_kib: n.memory_kib,
        };
        self.libvirt.send_replace(Some(c.clone()));

        let node = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(ev) => node.on_domain_event(ev).await,
                    Err(broadcast::error::RecvError::Lagged(_)) => node.refresh_all().await,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });

        self.reconcile(c).await;
        self.publish_node();
        if self
            .autostart_pending
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.autostart().await;
        }
        Ok(())
    }

    async fn on_domain_event(&self, ev: DomainEvent) {
        let id = Uuid::from_bytes(ev.domain().uuid);
        let _ = self.domain_events.send(ev.clone());
        match ev {
            DomainEvent::Lifecycle { event, .. } => {
                tracing::debug!(vm = %ev.domain().name, ?event, "lifecycle");
                if event == LifecycleEvent::Stopped {
                    // A cold boot applies all pending changes.
                    if let Some(rt) = lock(&self.runtime).get_mut(&id) {
                        rt.pending_restart = false;
                    }
                }
                self.refresh_id(id).await;
            }
            DomainEvent::BalloonChange { actual_kib, .. } => {
                self.update_runtime(id, |rt| {
                    rt.memory_kib = actual_kib;
                    rt.memory_stats.actual_kib = Some(actual_kib);
                });
            }
            DomainEvent::AgentLifecycle { connected, .. } => {
                self.update_runtime(id, |rt| rt.agent = connected);
            }
            DomainEvent::BlockJob { .. }
            | DomainEvent::JobCompleted { .. }
            | DomainEvent::DeviceRemoved { .. }
            | DomainEvent::MigrationIteration { .. } => {}
        }
    }

    /// Makes sure every VM this node owns is defined in libvirt.
    async fn reconcile(&self, c: &Client) {
        let _guard = self.ops.lock().await;
        let specs = match self.state.vms() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("reading VMs: {e}");
                return;
            }
        };
        let defined: HashMap<[u8; 16], Domain> = match c.list_all_domains(0).await {
            Ok(d) => d.into_iter().map(|d| (d.uuid, d)).collect(),
            Err(e) => {
                tracing::error!("listing domains: {e}");
                return;
            }
        };
        for spec in specs.iter().filter(|s| s.node == self.id) {
            let mut spec = spec.clone();
            spec.assign_targets();
            if !defined.contains_key(spec.id.as_bytes())
                && let Err(e) = self.define(c, &spec).await
            {
                tracing::error!(vm = %spec.name, "defining domain: {e}");
            }
            self.refresh(&spec).await;
        }
        // A VM never runs on two nodes: stop and undefine (keeping disks)
        // any Prism VM here that committed state gives to another node.
        // Ownership is frozen while the cluster is degraded.
        if self.cluster.get().is_some_and(|c| c.is_degraded()) {
            return;
        }
        for spec in specs.iter().filter(|s| s.node != self.id && !s.node.is_nil()) {
            let Some(dom) = defined.get(spec.id.as_bytes()) else { continue };
            if self.migrating(spec.id) {
                continue;
            }
            tracing::warn!(vm = %spec.name, owner = %spec.node, "quarantining a VM owned by another node");
            if let Err(e) = c.destroy(dom, 0).await
                && !not_running(&e)
            {
                tracing::error!(vm = %spec.name, "stopping quarantined VM: {e}");
                continue;
            }
            if let Err(e) = c.undefine(dom, undefine_flags()).await {
                tracing::error!(vm = %spec.name, "undefining quarantined VM: {e}");
            }
            lock(&self.runtime).remove(&spec.id);
        }
    }

    /// Whether a live migration currently allows `id` on this node.
    pub(crate) fn migrating(&self, _id: Uuid) -> bool {
        false
    }

    async fn autostart(&self) {
        let Ok(specs) = self.state.vms() else { return };
        for spec in specs.iter().filter(|s| s.autostart && s.node == self.id) {
            if self.runtime(spec.id).state == VmState::Stopped {
                tracing::info!(vm = %spec.name, "autostart");
                if let Err(e) = self.action(spec.id, Action::Start).await {
                    tracing::error!(vm = %spec.name, "autostart failed: {e}");
                }
            }
        }
    }

    async fn refresh_all(&self) {
        if let Ok(specs) = self.state.vms() {
            for spec in specs.iter().filter(|s| s.node == self.id) {
                self.refresh(spec).await;
            }
        }
    }

    /// Publishes SSE events for every applied state change.
    async fn watch_state(self: Arc<Self>) {
        let mut rx = self.state.subscribe();
        loop {
            let cmd = match rx.recv().await {
                Ok(Change::Applied(c)) => c,
                // Everything may have changed: re-snapshot clients and
                // reconcile this node's VMs.
                Ok(Change::Reset) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    self.bus.publish("reset", "/".into(), Op::Upsert, serde_json::Value::Null);
                    if let Ok(c) = self.client() {
                        self.reconcile(&c).await;
                    }
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            };
            match &*cmd {
                Command::PutVm { vm, .. } => {
                    let view = self.vm_view((**vm).clone());
                    self.bus.publish(
                        "vm.config",
                        format!("/vms/{}", vm.id),
                        Op::Upsert,
                        serde_json::to_value(view).unwrap_or_default(),
                    );
                }
                Command::DeleteVm { id } => {
                    lock(&self.runtime).remove(id);
                    self.bus.publish(
                        "vm.config",
                        format!("/vms/{id}"),
                        Op::Delete,
                        serde_json::Value::Null,
                    );
                }
                Command::PutPool { pool } => {
                    self.bus.publish(
                        "pool",
                        format!("/nodes/{}/pools/{}", pool.node, pool.name),
                        Op::Upsert,
                        serde_json::Value::Null,
                    );
                }
                Command::DeletePool { node, name } => {
                    self.bus.publish(
                        "pool",
                        format!("/nodes/{node}/pools/{name}"),
                        Op::Delete,
                        serde_json::Value::Null,
                    );
                }
                Command::InitCluster { .. }
                | Command::CreateInvite { .. }
                | Command::RevokeInvite { .. }
                | Command::AddMember { .. }
                | Command::SetMemberState { .. }
                | Command::SetVoter { .. }
                | Command::RemoveMember { .. } => {
                    self.bus.publish("cluster", "/cluster".into(), Op::Upsert, serde_json::Value::Null);
                }
            }
        }
    }

    async fn stats_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(STATS_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut ticks: u64 = 0;
        loop {
            tick.tick().await;
            let Ok(c) = self.client() else { continue };
            if let Ok(free) = c.free_memory().await {
                let changed = {
                    let mut h = lock(&self.host);
                    let changed = h.free_memory_bytes.abs_diff(free) > 16 << 20;
                    h.free_memory_bytes = free;
                    changed
                };
                if changed {
                    self.publish_node();
                }
            }
            let active: Vec<Uuid> = lock(&self.runtime)
                .iter()
                .filter(|(_, r)| r.state.is_active())
                .map(|(id, _)| *id)
                .collect();
            for id in active {
                self.refresh_id(id).await;
            }
            // Periodic full reconcile in a cluster (every 30 s).
            ticks += 1;
            if ticks % 6 == 0 && self.state.clustered() {
                self.reconcile(&c).await;
            }
        }
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

pub(crate) fn undefine_flags() -> u32 {
    flags::DOMAIN_UNDEFINE_MANAGED_SAVE
        | flags::DOMAIN_UNDEFINE_SNAPSHOTS_METADATA
        | flags::DOMAIN_UNDEFINE_NVRAM
        | flags::DOMAIN_UNDEFINE_CHECKPOINTS_METADATA
}

fn parse_mem_stats(stats: &[(i32, u64)]) -> MemoryStats {
    let mut m = MemoryStats::default();
    for &(tag, val) in stats {
        match tag {
            memory_stat::ACTUAL_BALLOON => m.actual_kib = Some(val),
            memory_stat::UNUSED => m.unused_kib = Some(val),
            memory_stat::AVAILABLE => m.available_kib = Some(val),
            memory_stat::USABLE => m.usable_kib = Some(val),
            memory_stat::DISK_CACHES => m.disk_caches_kib = Some(val),
            memory_stat::RSS => m.rss_kib = Some(val),
            memory_stat::MAJOR_FAULT => m.major_faults = Some(val),
            memory_stat::SWAP_IN => m.swap_in_kib = Some(val),
            _ => {}
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(format_version(12_000_000), "12.0.0");
        assert_eq!(format_version(10_002_001), "10.2.1");
    }

    #[test]
    fn mem_stats() {
        let m = parse_mem_stats(&[
            (memory_stat::ACTUAL_BALLOON, 1024),
            (memory_stat::USABLE, 512),
            (memory_stat::MAJOR_FAULT, 3),
            (99, 1),
        ]);
        assert_eq!(m.actual_kib, Some(1024));
        assert_eq!(m.usable_kib, Some(512));
        assert_eq!(m.major_faults, Some(3));
        assert_eq!(m.rss_kib, None);
    }
}
