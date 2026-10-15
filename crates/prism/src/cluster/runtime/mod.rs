//! The running cluster member: raft, the proposer, reconcilers and the
//! init/invite/accept/join flows.

mod join;
mod proposer;
mod reconcile;

use std::net::{Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use prism_cluster::{Channels, ClusterError, ClusterNode, GrpcNetwork, LogStore, RaftId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use proposer::Proposer;
use reconcile::sponsor_peer;

use super::blob::Invite;
use super::mesh::Mesh;
use super::model::{ClusterDoc, MemberDoc};
use crate::state::{State, StateError};

const META_LOCAL: &str = "cluster.local";
const META_PENDING: &str = "cluster.pending";
/// How long a follower waits to see its own forwarded write.
const READ_YOUR_WRITES: Duration = Duration::from_secs(10);
const RECONCILE_EVERY: Duration = Duration::from_secs(2);
pub const DEFAULT_INVITE_TTL: Duration = Duration::from_secs(3600);

/// This node's place in the cluster, persisted in `state.db` meta (never
/// replicated).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalMember {
    pub cluster_id: Uuid,
    pub raft_id: RaftId,
    pub mesh_addr: Ipv6Addr,
    pub wg_mtu: u16,
}

/// A join in progress on a new node.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingJoin {
    invite: String,
    raft_id: RaftId,
    sponsor_addr: Ipv6Addr,
}

#[derive(Debug, thiserror::Error)]
pub enum ClusterOpError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<serde_json::Error> for ClusterOpError {
    fn from(e: serde_json::Error) -> Self {
        Self::Internal(e.into())
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn grpc_addr(addr: Ipv6Addr) -> String {
    format!("[{addr}]:{}", prism_proto::GRPC_PORT)
}

fn decode_key(s: &str) -> anyhow::Result<[u8; 32]> {
    B64.decode(s)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("bad WireGuard key length"))
}

/// Settings from `prism.toml [cluster]`.
#[derive(Debug, Clone)]
pub struct ClusterConfig {
    /// Underlay hosts peers can reach this node at, preference-ordered.
    pub advertise: Vec<String>,
    pub wireguard_port: u16,
    pub data_dir: PathBuf,
}

pub struct Cluster {
    pub me: Uuid,
    pub name: String,
    pub state: State,
    pub mesh: Arc<dyn Mesh>,
    pub cfg: ClusterConfig,
    pub channels: Channels,
    raft: Mutex<Option<ClusterNode>>,
    local: Mutex<Option<LocalMember>>,
    /// The node this cluster runtime serves (set once at startup).
    pub node: std::sync::OnceLock<std::sync::Weak<crate::node::Node>>,
    /// The HTTP API, for requests tunnelled from peers.
    pub http: std::sync::OnceLock<axum::Router>,
    pub dataplane: Arc<super::dataplane::DataPlane>,
}

impl std::fmt::Debug for Cluster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cluster")
            .field("me", &self.me)
            .finish_non_exhaustive()
    }
}

/// Status for the API and `Cluster.Status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterStatus {
    pub clustered: bool,
    pub cluster: Option<ClusterDoc>,
    pub me: Uuid,
    pub raft_id: Option<RaftId>,
    pub role: String,
    pub leader: Option<Uuid>,
    pub applied_index: u64,
    pub members: Vec<MemberDoc>,
    pub degraded: bool,
    pub pending_join: bool,
}
impl Cluster {
    pub fn new(
        me: Uuid,
        name: String,
        state: State,
        mesh: Arc<dyn Mesh>,
        cfg: ClusterConfig,
    ) -> anyhow::Result<Arc<Self>> {
        let local: Option<LocalMember> = state
            .db()
            .meta(META_LOCAL)?
            .map(|s| serde_json::from_str(&s))
            .transpose()?;
        Ok(Arc::new(Self {
            me,
            name,
            state,
            mesh,
            cfg,
            channels: Channels::default(),
            raft: Mutex::default(),
            local: Mutex::new(local),
            node: std::sync::OnceLock::new(),
            http: std::sync::OnceLock::new(),
            dataplane: Arc::default(),
        }))
    }

    fn raft(&self) -> Option<ClusterNode> {
        self.raft
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn local(&self) -> Option<LocalMember> {
        self.local
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn endpoints(&self) -> Vec<String> {
        self.cfg
            .advertise
            .iter()
            .map(|h| {
                if h.parse::<Ipv6Addr>().is_ok() {
                    format!("[{h}]:{}", self.cfg.wireguard_port)
                } else {
                    format!("{h}:{}", self.cfg.wireguard_port)
                }
            })
            .collect()
    }

    /// Resumes cluster membership after a restart. Call once at startup.
    pub async fn resume(self: &Arc<Self>) -> anyhow::Result<()> {
        if let Some(pending) = self.state.db().meta(META_PENDING)? {
            let pending: PendingJoin = serde_json::from_str(&pending)?;
            let invite = Invite::decode(&pending.invite)?;
            self.mesh
                .ensure(invite.assigned_addr, invite.wg_mtu, self.cfg.wireguard_port)
                .await?;
            self.mesh
                .set_peers(vec![sponsor_peer(&invite.sponsor)])
                .await?;
            self.start_raft(pending.raft_id).await?;
            tokio::spawn(Arc::clone(self).finish_join(pending));
            return Ok(());
        }
        let Some(local) = self.local() else {
            return Ok(());
        };
        self.mesh
            .ensure(local.mesh_addr, local.wg_mtu, self.cfg.wireguard_port)
            .await?;
        self.reconcile_mesh().await;
        self.start_raft(local.raft_id).await?;
        tracing::info!(raft_id = local.raft_id, mesh = %local.mesh_addr, "cluster member resumed");
        Ok(())
    }

    async fn start_raft(self: &Arc<Self>, raft_id: RaftId) -> anyhow::Result<()> {
        if self.raft().is_some() {
            return Ok(());
        }
        let log = LogStore::open(&self.cfg.data_dir.join("raft.db"))?;
        let node = ClusterNode::start(
            raft_id,
            prism_cluster::raft_config("prism")?,
            GrpcNetwork {
                channels: self.channels.clone(),
            },
            log,
            Arc::new(self.state.backend()),
        )
        .await?;
        tokio::spawn(super::grpc::serve(Arc::clone(self), node.raft.clone()));
        tokio::spawn(Arc::clone(self).serve_dataplane());
        tokio::spawn(super::peers::fan_in(Arc::clone(self)));
        *self
            .raft
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(node);
        self.state.set_proposer(Arc::new(Proposer {
            cluster: Arc::clone(self),
        }));
        tokio::spawn(Arc::clone(self).reconcile_loop());
        Ok(())
    }

    async fn serve_dataplane(self: Arc<Self>) {
        loop {
            if let Some(addr) = self.grpc_listen() {
                let addr = SocketAddr::new(addr.ip(), prism_proto::DATA_PORT);
                Arc::clone(&self.dataplane).serve(addr).await;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// The gRPC listen address once this node has a mesh address.
    pub fn grpc_listen(&self) -> Option<SocketAddr> {
        self.local()
            .map(|l| l.mesh_addr)
            .or_else(|| {
                self.state
                    .db()
                    .meta(META_PENDING)
                    .ok()
                    .flatten()
                    .and_then(|p| serde_json::from_str::<PendingJoin>(&p).ok())
                    .and_then(|p| Invite::decode(&p.invite).ok())
                    .map(|i| i.assigned_addr)
            })
            .map(|a| SocketAddr::new(a.into(), prism_proto::GRPC_PORT))
    }

    pub fn status(&self) -> anyhow::Result<ClusterStatus> {
        let members = self.state.members()?;
        let raft = self.raft();
        let (role, leader, applied, degraded) = match &raft {
            None => ("standalone".to_string(), None, 0, false),
            Some(r) => {
                let m = r.metrics();
                let leader = m
                    .current_leader
                    .and_then(|id| members.iter().find(|x| x.raft_id == id).map(|x| x.node_id));
                (
                    format!("{:?}", m.state).to_lowercase(),
                    leader,
                    r.applied_index(),
                    m.current_leader.is_none(),
                )
            }
        };
        Ok(ClusterStatus {
            clustered: raft.is_some(),
            cluster: self.state.cluster()?,
            me: self.me,
            raft_id: self.local().map(|l| l.raft_id),
            role,
            leader,
            applied_index: applied,
            members,
            degraded,
            pending_join: self.state.db().meta(META_PENDING)?.is_some(),
        })
    }

    /// No hub/leader is known: configuration and ownership are frozen.
    pub fn is_degraded(&self) -> bool {
        self.raft()
            .is_some_and(|r| r.metrics().current_leader.is_none())
    }

    pub(crate) fn leader_raft_id(&self) -> u64 {
        self.raft()
            .and_then(|r| r.metrics().current_leader)
            .unwrap_or(0)
    }

    /// Proposes on this node if it leads. Used by `Cluster.ForwardWrite`.
    pub(crate) async fn write_local(&self, command: String) -> Result<(String, u64), ClusterError> {
        let raft = self
            .raft()
            .ok_or_else(|| ClusterError::Other("not clustered".into()))?;
        raft.write(command).await
    }

    /// The member at mesh address `addr`, for caller checks.
    pub(crate) fn member_at(&self, addr: Ipv6Addr) -> Option<MemberDoc> {
        self.state
            .members()
            .ok()?
            .into_iter()
            .find(|m| m.mesh_addr == addr)
    }

    pub(crate) fn pending_join(&self) -> bool {
        self.state.db().meta(META_PENDING).ok().flatten().is_some()
    }
}
