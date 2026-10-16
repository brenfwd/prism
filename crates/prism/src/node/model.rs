//! The node's API-facing types: VM runtime state, host and node views.

use std::collections::BTreeMap;
use std::net::Ipv6Addr;

use prism_libvirt::DomainState;
use serde::Serialize;
use uuid::Uuid;

use super::{BalloonStatus, NodeError, Result, balloon};
use crate::spec::VmSpec;

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
    /// The IPv6 address each NIC on a network with IPv6 forms by SLAAC, by NIC ID.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub nic_ipv6: BTreeMap<Uuid, Ipv6Addr>,
    /// How the device change that produced this view reached the running
    /// guest; absent for views not returned by a device change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotplug: Option<HotplugResult>,
}

/// Whether one device change was applied live or waits for a restart.
#[derive(Debug, Clone, Serialize)]
pub struct HotplugResult {
    /// Applied to the running guest (or the VM is not running).
    pub applied: bool,
    /// The guest still needs a restart to see the change.
    pub restart_required: bool,
    /// Why the live change did not apply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    /// This node's archive member directories and their free space.
    #[serde(default)]
    pub archive: Vec<crate::archive::MemberCapacity>,
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
