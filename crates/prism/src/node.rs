//! The local node: libvirt connection supervision, VM lifecycle and runtime state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prism_libvirt::proto::{error_codes, flags, memory_stat};
use prism_libvirt::{Client, Domain, DomainState, LifecycleEvent};
use serde::Serialize;
use tokio::sync::watch;
use uuid::Uuid;

use crate::config::Config;
use crate::db::Db;
use crate::events::{Bus, Op};
use crate::render::{self, RenderCtx};
use crate::spec::{
    BalloonMode, CpuSpec, CreateVm, DiskDevice, DiskSpec, MAX_VCPUS, MemorySpec, NicNetwork,
    NicSpec, UpdateVm, VmSpec, random_mac,
};
use crate::storage::{Storage, StorageError};

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
    #[error(transparent)]
    Libvirt(#[from] prism_libvirt::Error),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type Result<T, E = NodeError> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_field_names)] // units in names match the API
pub struct MemoryStats {
    /// Current balloon size.
    pub actual_kib: Option<u64>,
    pub unused_kib: Option<u64>,
    pub available_kib: Option<u64>,
    pub usable_kib: Option<u64>,
    pub disk_caches_kib: Option<u64>,
    pub rss_kib: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VmRuntime {
    pub state: VmState,
    pub vcpus: u16,
    pub memory_kib: u64,
    pub max_memory_kib: u64,
    pub cpu_time_ns: u64,
    pub memory_stats: MemoryStats,
    /// Spec changes that only apply after the next cold boot.
    pub pending_restart: bool,
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
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VmView {
    #[serde(flatten)]
    pub spec: VmSpec,
    pub node: Uuid,
    pub status: VmRuntime,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HostInfo {
    pub hostname: String,
    pub libvirt_version: String,
    pub cpu_model: String,
    pub cpus: i32,
    pub memory_kib: u64,
    pub free_memory_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeView {
    pub id: Uuid,
    pub name: String,
    pub version: &'static str,
    pub libvirt_connected: bool,
    pub domain_type: String,
    pub host: HostInfo,
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
    pub db: Db,
    pub storage: Storage,
    pub bus: Bus,
    libvirt: watch::Sender<Option<Client>>,
    runtime: Mutex<HashMap<Uuid, VmRuntime>>,
    host: Mutex<HostInfo>,
    /// Serialises spec mutations (create, update, delete).
    ops: tokio::sync::Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn domain_ref(spec: &VmSpec) -> Domain {
    Domain {
        name: spec.name.clone(),
        uuid: *spec.id.as_bytes(),
        id: -1,
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn format_version(v: u64) -> String {
    format!("{}.{}.{}", v / 1_000_000, v / 1000 % 1000, v % 1000)
}

impl Node {
    pub fn new(
        id: Uuid,
        name: String,
        cfg: Config,
        db: Db,
        storage: Storage,
        bus: Bus,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            name,
            cfg,
            db,
            storage,
            bus,
            libvirt: watch::channel(None).0,
            runtime: Mutex::default(),
            host: Mutex::default(),
            ops: tokio::sync::Mutex::new(()),
        })
    }

    /// Spawns the libvirt supervisor and the stats loop.
    pub fn start(self: &Arc<Self>) {
        tokio::spawn(Arc::clone(self).supervise());
        tokio::spawn(Arc::clone(self).stats_loop());
    }

    fn client(&self) -> Result<Client> {
        self.libvirt
            .borrow()
            .clone()
            .filter(|c| !c.is_closed())
            .ok_or(NodeError::Unavailable)
    }

    pub fn view(&self) -> NodeView {
        NodeView {
            id: self.id,
            name: self.name.clone(),
            version: env!("CARGO_PKG_VERSION"),
            libvirt_connected: self.client().is_ok(),
            domain_type: self.cfg.domain_type().into(),
            host: lock(&self.host).clone(),
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

    fn vm_view(&self, spec: VmSpec) -> VmView {
        let status = lock(&self.runtime)
            .get(&spec.id)
            .cloned()
            .unwrap_or_default();
        VmView {
            spec,
            node: self.id,
            status,
        }
    }

    pub fn vms(&self) -> Result<Vec<VmView>> {
        Ok(self
            .db
            .vms()?
            .into_iter()
            .map(|s| self.vm_view(s))
            .collect())
    }

    pub fn spec(&self, id: Uuid) -> Result<VmSpec> {
        self.db
            .vm(id)?
            .ok_or_else(|| NodeError::NotFound(format!("vm {id}")))
    }

    pub fn vm(&self, id: Uuid) -> Result<VmView> {
        Ok(self.vm_view(self.spec(id)?))
    }

    fn publish_config(&self, spec: &VmSpec) {
        let view = self.vm_view(spec.clone());
        self.bus.publish(
            "vm.config",
            format!("/vms/{}", spec.id),
            Op::Upsert,
            serde_json::to_value(view).unwrap_or_default(),
        );
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

    fn render(&self, spec: &VmSpec) -> Result<String> {
        let pool_path = |p: &str| self.storage.path_of(p);
        let ctx = RenderCtx {
            domain_type: self.cfg.domain_type(),
            emulator: self.cfg.libvirt.emulator.as_deref(),
            uefi_loader: self.cfg.libvirt.uefi_loader.as_deref(),
            pool_path: &pool_path,
        };
        render::render(spec, &ctx).map_err(NodeError::Invalid)
    }

    async fn define(&self, c: &Client, spec: &VmSpec) -> Result<Domain> {
        let xml = self.render(spec)?;
        Ok(c.define_xml(&xml, flags::DOMAIN_DEFINE_VALIDATE).await?)
    }

    /// Re-reads one VM's runtime state from libvirt.
    async fn refresh(&self, spec: &VmSpec) {
        let Ok(c) = self.client() else { return };
        let dom = domain_ref(spec);
        let pending = lock(&self.runtime)
            .get(&spec.id)
            .is_some_and(|r| r.pending_restart);
        let rt = match c.get_info(&dom).await {
            Ok(info) => {
                let state = VmState::from(info.state);
                let memory_stats = if state == VmState::Running {
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
                    pending_restart: pending && state != VmState::Stopped,
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

    async fn refresh_id(&self, id: Uuid) {
        if let Ok(Some(spec)) = self.db.vm(id) {
            self.refresh(&spec).await;
        }
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
        c.register_lifecycle_events().await?;
        let host = HostInfo {
            hostname: c.hostname().await?,
            libvirt_version: format_version(c.lib_version().await?),
            free_memory_bytes: c.free_memory().await?,
            ..{
                let n = c.node_info().await?;
                HostInfo {
                    cpu_model: n.model,
                    cpus: n.cpus,
                    memory_kib: n.memory_kib,
                    ..HostInfo::default()
                }
            }
        };
        *lock(&self.host) = host;
        self.libvirt.send_replace(Some(c.clone()));

        let node = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(ev) => {
                        let id = Uuid::from_bytes(ev.domain.uuid);
                        tracing::debug!(vm = %ev.domain.name, event = ?ev.event, "lifecycle");
                        if ev.event == LifecycleEvent::Stopped {
                            // A cold boot applies all pending changes.
                            if let Some(rt) = lock(&node.runtime).get_mut(&id) {
                                rt.pending_restart = false;
                            }
                        }
                        node.refresh_id(id).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        node.refresh_all().await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });

        self.reconcile(c).await;
        self.publish_node();
        Ok(())
    }

    /// Makes sure every VM in the database is defined in libvirt.
    async fn reconcile(&self, c: &Client) {
        let _guard = self.ops.lock().await;
        let specs = match self.db.vms() {
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
        for spec in &specs {
            if !defined.contains_key(spec.id.as_bytes())
                && let Err(e) = self.define(c, spec).await
            {
                tracing::error!(vm = %spec.name, "defining domain: {e}");
            }
            self.refresh(spec).await;
        }
    }

    async fn refresh_all(&self) {
        if let Ok(specs) = self.db.vms() {
            for spec in &specs {
                self.refresh(spec).await;
            }
        }
    }

    async fn stats_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(STATS_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
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
            let running: Vec<Uuid> = lock(&self.runtime)
                .iter()
                .filter(|(_, r)| {
                    matches!(
                        r.state,
                        VmState::Running | VmState::Paused | VmState::ShuttingDown
                    )
                })
                .map(|(id, _)| *id)
                .collect();
            for id in running {
                self.refresh_id(id).await;
            }
        }
    }

    pub async fn create_vm(&self, req: CreateVm) -> Result<VmView> {
        req.validate().map_err(NodeError::Invalid)?;
        let c = self.client()?;
        let _guard = self.ops.lock().await;
        if self.db.vm_by_name(&req.name)?.is_some() {
            return Err(NodeError::Conflict(format!(
                "a VM named {} already exists",
                req.name
            )));
        }
        let pool = self.storage.get(&req.pool)?;
        let memory = MemorySpec {
            max_mib: req.memory_mib,
            min_mib: req
                .min_memory_mib
                .unwrap_or_else(|| (req.memory_mib / 4).max(128).min(req.memory_mib)),
            target_mib: req.memory_mib,
            balloon: req.balloon,
        };
        memory.validate().map_err(NodeError::Invalid)?;

        let mut disks = Vec::new();
        let mut boot = 1;
        let mut created_volume = None;
        if let Some(gib) = req.disk_gib {
            let volume = format!("{}-disk0.{}", req.name, pool.format);
            pool.create_volume(&self.cfg.libvirt.qemu_img, &volume, gib << 30, &pool.format)
                .await?;
            created_volume = Some(volume.clone());
            disks.push(DiskSpec {
                id: Uuid::new_v4(),
                pool: pool.name.clone(),
                volume,
                device: DiskDevice::Disk,
                format: pool.format.clone(),
                boot_order: Some(boot),
            });
            boot += 1;
        }
        if let Some(iso) = &req.iso {
            pool.volume(iso)?;
            let cdrom = DiskSpec {
                id: Uuid::new_v4(),
                pool: pool.name.clone(),
                volume: iso.clone(),
                device: DiskDevice::Cdrom,
                format: "raw".into(),
                boot_order: Some(boot),
            };
            // Boot the installer first when the disk is blank.
            if let Some(d) = disks.first_mut() {
                d.boot_order = Some(2);
                disks.push(DiskSpec {
                    boot_order: Some(1),
                    ..cdrom
                });
            } else {
                disks.push(cdrom);
            }
        }
        let mut spec = VmSpec {
            id: Uuid::new_v4(),
            name: req.name,
            description: req.description,
            generation: 1,
            cpu: CpuSpec { vcpus: req.vcpus },
            memory,
            firmware: req.firmware,
            machine: "q35".into(),
            disks,
            nics: vec![NicSpec {
                id: Uuid::new_v4(),
                mac: random_mac(),
                network: req.network.unwrap_or(NicNetwork::User),
            }],
        };

        let res: Result<()> = async {
            let dom = self.define(&c, &spec).await?;
            // Pin the expanded machine type (never the bare alias).
            let xml = c.get_xml_desc(&dom, flags::DOMAIN_XML_INACTIVE).await?;
            if let Some(m) = render::machine_type(&xml) {
                spec.machine = m;
            }
            self.db.insert_vm(&spec, now_secs())?;
            Ok(())
        }
        .await;
        if let Err(e) = res {
            let _ = c.undefine(&domain_ref(&spec), undefine_flags()).await;
            if let Some(v) = created_volume {
                let _ = pool.delete_volume(&v);
            }
            return Err(e);
        }
        tracing::info!(vm = %spec.name, id = %spec.id, "created VM");
        self.refresh(&spec).await;
        self.publish_config(&spec);
        Ok(self.vm_view(spec))
    }

    pub async fn update_vm(&self, id: Uuid, req: UpdateVm) -> Result<VmView> {
        let c = self.client()?;
        let _guard = self.ops.lock().await;
        let old = self.spec(id)?;
        let mut spec = old.clone();
        if let Some(d) = req.description {
            spec.description = d;
        }
        if let Some(v) = req.vcpus {
            if v == 0 || v > MAX_VCPUS {
                return Err(NodeError::Invalid(format!("vcpus must be 1..={MAX_VCPUS}")));
            }
            spec.cpu.vcpus = v;
        }
        if let Some(m) = req.memory {
            spec.memory.max_mib = m.max_mib.unwrap_or(spec.memory.max_mib);
            spec.memory.min_mib = m.min_mib.unwrap_or(spec.memory.min_mib);
            spec.memory.target_mib = m.target_mib.unwrap_or(spec.memory.target_mib);
            if m.max_mib.is_some() && m.target_mib.is_none() {
                spec.memory.target_mib = spec.memory.target_mib.min(spec.memory.max_mib);
            }
            spec.memory.balloon = m.balloon.unwrap_or(spec.memory.balloon);
        }
        spec.memory.validate().map_err(NodeError::Invalid)?;
        if spec == old {
            return Ok(self.vm_view(spec));
        }
        spec.generation += 1;

        let dom = self.define(&c, &spec).await?;
        let running = c
            .get_info(&dom)
            .await
            .is_ok_and(|i| !matches!(VmState::from(i.state), VmState::Stopped));
        let mut needs_restart = spec.cpu != old.cpu
            || spec.memory.max_mib != old.memory.max_mib
            || (spec.memory.balloon == BalloonMode::Off)
                != (old.memory.balloon == BalloonMode::Off);
        if running && spec.memory.target_mib != old.memory.target_mib {
            if spec.memory.balloon == BalloonMode::Off {
                needs_restart = true;
            } else {
                c.set_memory(
                    &dom,
                    spec.memory.target_mib * 1024,
                    flags::DOMAIN_AFFECT_LIVE,
                )
                .await?;
            }
        }
        self.db.update_vm(&spec)?;
        if running
            && needs_restart
            && let Some(rt) = lock(&self.runtime).get_mut(&id)
        {
            rt.pending_restart = true;
        }
        self.refresh(&spec).await;
        self.publish_config(&spec);
        Ok(self.vm_view(spec))
    }

    pub async fn delete_vm(&self, id: Uuid, delete_disks: bool) -> Result<()> {
        let c = self.client()?;
        let _guard = self.ops.lock().await;
        let spec = self.spec(id)?;
        let dom = domain_ref(&spec);
        match c.destroy(&dom, 0).await {
            Ok(()) => {}
            Err(e)
                if matches!(
                    e.remote_code(),
                    Some(error_codes::OPERATION_INVALID | error_codes::NO_DOMAIN)
                ) => {}
            Err(e) => return Err(e.into()),
        }
        match c.undefine(&dom, undefine_flags()).await {
            Ok(()) => {}
            Err(e) if e.remote_code() == Some(error_codes::NO_DOMAIN) => {}
            Err(e) => return Err(e.into()),
        }
        self.db.delete_vm(id)?;
        lock(&self.runtime).remove(&id);
        if delete_disks {
            for d in spec.disks.iter().filter(|d| d.device == DiskDevice::Disk) {
                let res = self
                    .storage
                    .get(&d.pool)
                    .and_then(|p| p.delete_volume(&d.volume));
                if let Err(e) = res {
                    tracing::warn!(vm = %spec.name, "deleting {}/{}: {e}", d.pool, d.volume);
                }
            }
        }
        tracing::info!(vm = %spec.name, "deleted VM");
        self.bus.publish(
            "vm.config",
            format!("/vms/{id}"),
            Op::Delete,
            serde_json::Value::Null,
        );
        Ok(())
    }

    pub async fn action(&self, id: Uuid, action: Action) -> Result<VmView> {
        let c = self.client()?;
        let spec = self.spec(id)?;
        let dom = domain_ref(&spec);
        match action {
            Action::Start => {
                // Start from the current spec, so any pending change applies.
                self.define(&c, &spec).await?;
                c.create(&dom, 0).await?;
                if let Some(rt) = lock(&self.runtime).get_mut(&id) {
                    rt.pending_restart = false;
                }
            }
            Action::Shutdown => c.shutdown(&dom).await?,
            Action::Stop => c.destroy(&dom, 0).await?,
            Action::Reboot => c.reboot(&dom, 0).await?,
            Action::Reset => c.reset(&dom).await?,
            Action::Pause => c.suspend(&dom).await?,
            Action::Resume => c.resume(&dom).await?,
        }
        self.refresh(&spec).await;
        Ok(self.vm_view(spec))
    }

    /// The VNC UNIX socket of a running VM.
    pub async fn vnc_socket(&self, id: Uuid) -> Result<PathBuf> {
        let c = self.client()?;
        let spec = self.spec(id)?;
        let xml = c.get_xml_desc(&domain_ref(&spec), 0).await?;
        render::vnc_socket(&xml)
            .ok_or_else(|| NodeError::Conflict("VM has no VNC socket; is it running?".into()))
    }

    pub async fn open_serial(&self, id: Uuid) -> Result<prism_libvirt::Stream> {
        let c = self.client()?;
        let spec = self.spec(id)?;
        Ok(
            c.open_console(&domain_ref(&spec), None, flags::DOMAIN_CONSOLE_FORCE)
                .await?,
        )
    }

    /// Returns the names of VMs using `pool/volume`.
    pub fn volume_users(&self, pool: &str, volume: &str) -> Result<Vec<String>> {
        Ok(self
            .db
            .vms()?
            .into_iter()
            .filter(|s| s.disks.iter().any(|d| d.pool == pool && d.volume == volume))
            .map(|s| s.name)
            .collect())
    }

    pub fn pool_users(&self, pool: &str) -> Result<Vec<String>> {
        Ok(self
            .db
            .vms()?
            .into_iter()
            .filter(|s| s.disks.iter().any(|d| d.pool == pool))
            .map(|s| s.name)
            .collect())
    }
}

fn undefine_flags() -> u32 {
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
            (99, 1),
        ]);
        assert_eq!(m.actual_kib, Some(1024));
        assert_eq!(m.usable_kib, Some(512));
        assert_eq!(m.rss_kib, None);
    }
}
