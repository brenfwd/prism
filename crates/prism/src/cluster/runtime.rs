//! The running cluster member: raft, the proposer, reconcilers and the
//! init/invite/accept/join flows.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use futures::future::BoxFuture;
use prism_cluster::{Channels, ClusterError, ClusterNode, GrpcNetwork, LogStore, PrismNode, RaftId};
use prism_proto::v1::WriteRequest;
use prism_proto::v1::cluster_client::ClusterClient;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use super::blob::{self, Invite, Reply, Sponsor};
use super::mesh::{DEFAULT_MTU, Mesh, MeshPeer};
use super::model::{ClusterDoc, ClusterMode, FEATURE_LEVEL, InviteDoc, MemberDoc, MemberState};
use crate::spec::VmSpec;
use crate::state::{Change, Command, PoolDoc, Propose, State, StateError};

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
}

impl std::fmt::Debug for Cluster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cluster").field("me", &self.me).finish_non_exhaustive()
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

/// The preview shown before admitting a node.
#[derive(Debug, Clone, Serialize)]
pub struct AcceptPreview {
    pub node_id: Uuid,
    pub name: String,
    pub endpoints: Vec<String>,
    pub fingerprint: String,
    pub invite_id: String,
    pub admitted: bool,
}

/// What a joining node shows the operator.
#[derive(Debug, Clone, Serialize)]
pub struct JoinReply {
    pub reply: String,
    pub fingerprint: String,
    pub cluster_name: String,
    pub sponsor: String,
}

/// An invite as returned to the operator.
#[derive(Debug, Clone, Serialize)]
pub struct InviteView {
    pub invite_id: String,
    pub blob: String,
    pub expires: i64,
    pub assigned_addr: Ipv6Addr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BootstrapMembers {
    cluster: ClusterDoc,
    members: Vec<MemberDoc>,
}

impl Cluster {
    pub fn new(me: Uuid, name: String, state: State, mesh: Arc<dyn Mesh>, cfg: ClusterConfig) -> anyhow::Result<Arc<Self>> {
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
        }))
    }

    fn raft(&self) -> Option<ClusterNode> {
        self.raft.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn local(&self) -> Option<LocalMember> {
        self.local.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
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
            self.mesh.ensure(invite.assigned_addr, invite.wg_mtu, self.cfg.wireguard_port).await?;
            self.mesh.set_peers(vec![sponsor_peer(&invite.sponsor)]).await?;
            self.start_raft(pending.raft_id).await?;
            tokio::spawn(Arc::clone(self).finish_join(pending));
            return Ok(());
        }
        let Some(local) = self.local() else { return Ok(()) };
        self.mesh.ensure(local.mesh_addr, local.wg_mtu, self.cfg.wireguard_port).await?;
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
        *self.raft.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(node);
        self.state.set_proposer(Arc::new(Proposer {
            cluster: Arc::clone(self),
        }));
        tokio::spawn(Arc::clone(self).reconcile_loop());
        Ok(())
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

    /// Turns this standalone node into the hub of a new cluster.
    pub async fn init(self: &Arc<Self>, cluster_name: &str, mode: ClusterMode) -> Result<ClusterStatus, ClusterOpError> {
        if self.raft().is_some() || self.state.db().meta(META_PENDING)?.is_some() {
            return Err(ClusterOpError::Conflict("this node is already in a cluster".into()));
        }
        let id = Uuid::new_v4();
        let b = id.as_bytes();
        // fdXX:XXXX:XXXX::/48 with 40 random bits (RFC 4193).
        let prefix = Ipv6Addr::new(
            0xfd00 | u16::from(b[0]),
            u16::from_be_bytes([b[1], b[2]]),
            u16::from_be_bytes([b[3], b[4]]),
            0,
            0,
            0,
            0,
            0,
        );
        let cluster = ClusterDoc {
            cluster_id: Uuid::new_v4(),
            name: cluster_name.to_string(),
            mode,
            mesh_prefix: prefix,
            wg_mtu: DEFAULT_MTU,
            feature_level: FEATURE_LEVEL,
            created: now(),
        };
        let raft_id = 1;
        let mesh_addr = cluster.mesh_addr(raft_id);
        let pubkey = self.mesh.ensure(mesh_addr, cluster.wg_mtu, self.cfg.wireguard_port).await?;
        let local = LocalMember {
            cluster_id: cluster.cluster_id,
            raft_id,
            mesh_addr,
            wg_mtu: cluster.wg_mtu,
        };
        self.state.db().set_meta(META_LOCAL, &serde_json::to_string(&local)?)?;
        *self.local.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(local);
        self.start_raft(raft_id).await?;
        let raft = self.raft().ok_or_else(|| anyhow::anyhow!("raft did not start"))?;
        raft.initialize(PrismNode {
            node_id: self.me,
            name: self.name.clone(),
            addr: grpc_addr(mesh_addr),
        })
        .await?;
        let hub = MemberDoc {
            node_id: self.me,
            raft_id,
            name: self.name.clone(),
            wg_pubkey: B64.encode(pubkey),
            mesh_addr,
            endpoints: self.endpoints(),
            state: MemberState::Active,
            voter: true,
            version: env!("CARGO_PKG_VERSION").into(),
            joined: now(),
        };
        // Wait for this single voter to become leader, then commit.
        for _ in 0..100 {
            if raft.is_leader() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.state.apply(Command::InitCluster { cluster, hub }).await?;
        // Pre-cluster documents are not in the log: snapshot and purge so
        // every future member starts from a snapshot that includes them.
        let _ = raft.raft.trigger().snapshot().await;
        for _ in 0..100 {
            if raft.metrics().snapshot.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if let Some(s) = raft.metrics().snapshot {
            let _ = raft.raft.trigger().purge_log(s.index).await;
        }
        tracing::info!(mesh = %mesh_addr, "initialised a new cluster as its hub");
        Ok(self.status()?)
    }

    fn next_raft_id(&self) -> anyhow::Result<RaftId> {
        let members = self.state.members()?.into_iter().map(|m| m.raft_id);
        let invites = self.state.invites()?.into_iter().map(|i| i.raft_id);
        let tombs = self.state.tombstones()?.into_iter().map(|t| {
            let s = t.mesh_addr.segments();
            (u64::from(s[5]) << 32) | (u64::from(s[6]) << 16) | u64::from(s[7])
        });
        Ok(members.chain(invites).chain(tombs).max().unwrap_or(0) + 1)
    }

    /// Reserves a slot and returns the invite blob for a new node.
    pub async fn invite(&self, name_hint: Option<String>, ttl: Duration) -> Result<InviteView, ClusterOpError> {
        let cluster = self
            .state
            .cluster()?
            .ok_or_else(|| ClusterOpError::Invalid("this node is not in a cluster; create one first".into()))?;
        let me = self
            .state
            .members()?
            .into_iter()
            .find(|m| m.node_id == self.me)
            .ok_or_else(|| ClusterOpError::Invalid("this node is not an active member".into()))?;
        if me.endpoints.is_empty() && self.endpoints().is_empty() {
            return Err(ClusterOpError::Invalid(
                "this node has no reachable endpoint: set [cluster] advertise in prism.toml, or invite from another member".into(),
            ));
        }
        let mut last_err = None;
        for _ in 0..5 {
            let raft_id = self.next_raft_id()?;
            let invite_id: [u8; 16] = *Uuid::new_v4().as_bytes();
            let exp = now() + ttl.as_secs() as i64;
            let assigned_addr = cluster.mesh_addr(raft_id);
            let doc = InviteDoc {
                invite_id: blob::hex(&invite_id),
                raft_id,
                assigned_addr,
                exp,
                created_by: self.me,
                name_hint: name_hint.clone(),
                consumed_by: None,
            };
            match self.state.apply(Command::CreateInvite { invite: doc }).await {
                Ok(()) => {
                    let inv = Invite {
                        cluster_id: cluster.cluster_id,
                        cluster_name: cluster.name.clone(),
                        invite_id,
                        exp,
                        mesh_prefix: cluster.mesh_prefix,
                        assigned_addr,
                        wg_mtu: cluster.wg_mtu,
                        sponsor: Sponsor {
                            node_id: self.me,
                            name: me.name.clone(),
                            wg_pubkey: decode_key(&me.wg_pubkey)?,
                            mesh_addr: me.mesh_addr,
                            endpoints: if me.endpoints.is_empty() { self.endpoints() } else { me.endpoints.clone() },
                        },
                        feature_level: FEATURE_LEVEL,
                        version: env!("CARGO_PKG_VERSION").into(),
                        name_hint: name_hint.clone(),
                    };
                    return Ok(InviteView {
                        invite_id: inv.invite_id_hex(),
                        blob: inv.encode(),
                        expires: exp,
                        assigned_addr,
                    });
                }
                // Another invite took this address concurrently; retry.
                Err(StateError::Conflict(m)) => last_err = Some(m),
                Err(e) => return Err(e.into()),
            }
        }
        Err(ClusterOpError::Conflict(last_err.unwrap_or_default()))
    }

    /// Checks a reply; with `confirm`, admits the node.
    pub async fn accept(&self, reply_blob: &str, confirm: bool) -> Result<AcceptPreview, ClusterOpError> {
        let reply = Reply::decode(reply_blob).map_err(|e| ClusterOpError::Invalid(e.to_string()))?;
        let cluster = self
            .state
            .cluster()?
            .ok_or_else(|| ClusterOpError::Invalid("this node is not in a cluster".into()))?;
        if reply.cluster_id != cluster.cluster_id {
            return Err(ClusterOpError::Invalid(
                "cluster_mismatch: this reply belongs to a different cluster".into(),
            ));
        }
        if reply.feature_level.abs_diff(FEATURE_LEVEL) > 1 {
            return Err(ClusterOpError::Invalid(format!(
                "version_skew: the node runs prism {} (feature level {}); upgrade one side",
                reply.version, reply.feature_level
            )));
        }
        let invite_id = reply.invite_id_hex();
        let invite = self
            .state
            .invites()?
            .into_iter()
            .find(|i| i.invite_id == invite_id)
            .ok_or_else(|| ClusterOpError::Invalid("invite_not_found: no such invite".into()))?;
        let preview = AcceptPreview {
            node_id: reply.node_id,
            name: reply.name.clone(),
            endpoints: reply.endpoints.clone(),
            fingerprint: blob::fingerprint(&reply.wg_pubkey),
            invite_id: invite_id.clone(),
            admitted: false,
        };
        if !confirm {
            return Ok(preview);
        }
        let member = MemberDoc {
            node_id: reply.node_id,
            raft_id: invite.raft_id,
            name: reply.name,
            wg_pubkey: B64.encode(reply.wg_pubkey),
            mesh_addr: reply.assigned_addr,
            endpoints: reply.endpoints,
            state: MemberState::Joining,
            voter: false,
            version: reply.version,
            joined: now(),
        };
        self.state
            .apply(Command::AddMember {
                member,
                invite_id,
                now: now(),
            })
            .await?;
        Ok(AcceptPreview {
            admitted: true,
            ..preview
        })
    }

    /// Starts joining a cluster from an invite blob; returns the reply blob.
    pub async fn join(self: &Arc<Self>, invite_blob: &str) -> Result<JoinReply, ClusterOpError> {
        let invite = Invite::decode(invite_blob).map_err(|e| ClusterOpError::Invalid(e.to_string()))?;
        if self.raft().is_some() {
            return Err(ClusterOpError::Conflict("this node is already in a cluster".into()));
        }
        if invite.exp < now() {
            tracing::warn!(
                skew_secs = now() - invite.exp,
                "the invite has expired by this node's clock; the cluster decides"
            );
        }
        if invite.feature_level.abs_diff(FEATURE_LEVEL) > 1 {
            return Err(ClusterOpError::Invalid(format!(
                "version_skew: the cluster runs prism {}; upgrade one side",
                invite.version
            )));
        }
        let pubkey = self
            .mesh
            .ensure(invite.assigned_addr, invite.wg_mtu, self.cfg.wireguard_port)
            .await?;
        self.mesh.set_peers(vec![sponsor_peer(&invite.sponsor)]).await?;
        let s = invite.assigned_addr.segments();
        let raft_id = (u64::from(s[5]) << 32) | (u64::from(s[6]) << 16) | u64::from(s[7]);
        let pending = PendingJoin {
            invite: invite.encode(),
            raft_id,
            sponsor_addr: invite.sponsor.mesh_addr,
        };
        self.state.db().set_meta(META_PENDING, &serde_json::to_string(&pending)?)?;
        let reply = Reply {
            cluster_id: invite.cluster_id,
            invite_id: invite.invite_id,
            assigned_addr: invite.assigned_addr,
            node_id: self.me,
            name: self.name.clone(),
            wg_pubkey: pubkey,
            endpoints: self.endpoints(),
            feature_level: FEATURE_LEVEL,
            version: env!("CARGO_PKG_VERSION").into(),
            arch: std::env::consts::ARCH.into(),
        };
        self.start_raft(raft_id).await?;
        tokio::spawn(Arc::clone(self).finish_join(pending));
        Ok(JoinReply {
            reply: reply.encode(),
            fingerprint: blob::fingerprint(&pubkey),
            cluster_name: invite.cluster_name,
            sponsor: invite.sponsor.name,
        })
    }

    /// Waits for admission, installs all peers, then re-proposes this node's
    /// own VMs and pools into the cluster once the snapshot has arrived.
    async fn finish_join(self: Arc<Self>, pending: PendingJoin) {
        // Documents this node owned before joining; the snapshot replaces them.
        let own_vms: Vec<VmSpec> = self.state.vms().unwrap_or_default();
        let own_pools: Vec<PoolDoc> = self.state.pools(self.me).unwrap_or_default();
        let sponsor = grpc_addr(pending.sponsor_addr);
        let mut delay = Duration::from_secs(1);
        let boot = loop {
            match self.bootstrap(&sponsor).await {
                Ok(b) => break b,
                Err(e) => {
                    tracing::info!("waiting to be admitted by {sponsor}: {e}");
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(15));
                }
            }
        };
        let peers = boot
            .members
            .iter()
            .filter(|m| m.node_id != self.me)
            .filter_map(member_peer)
            .collect();
        if let Err(e) = self.mesh.set_peers(peers).await {
            tracing::error!("installing mesh peers: {e:#}");
        }
        // Wait for the snapshot from the leader.
        loop {
            if self.state.cluster().ok().flatten().is_some()
                && self.state.members().is_ok_and(|m| m.iter().any(|m| m.node_id == self.me))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let local = LocalMember {
            cluster_id: boot.cluster.cluster_id,
            raft_id: pending.raft_id,
            mesh_addr: boot.cluster.mesh_addr(pending.raft_id),
            wg_mtu: boot.cluster.wg_mtu,
        };
        if let Err(e) = (|| -> anyhow::Result<()> {
            self.state.db().set_meta(META_LOCAL, &serde_json::to_string(&local)?)?;
            self.state.db().delete_meta(META_PENDING)
        })() {
            tracing::error!("recording membership: {e:#}");
        }
        *self.local.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(local);
        for mut vm in own_vms {
            vm.node = self.me;
            if let Err(e) = self
                .state
                .apply(Command::PutVm {
                    vm: Box::new(vm.clone()),
                    expect_generation: Some(None),
                })
                .await
            {
                tracing::warn!(vm = %vm.name, "could not bring this VM into the cluster: {e}");
            }
        }
        for mut pool in own_pools {
            pool.node = self.me;
            let _ = self.state.apply(Command::PutPool { pool }).await;
        }
        tracing::info!("joined the cluster");
    }

    async fn bootstrap(&self, addr: &str) -> anyhow::Result<BootstrapMembers> {
        let ch = self.channels.get(addr)?;
        let reply = ClusterClient::new(ch)
            .bootstrap(prism_proto::v1::Empty {})
            .await?
            .into_inner();
        Ok(serde_json::from_slice(&reply.members)?)
    }

    pub(crate) fn bootstrap_members(&self) -> anyhow::Result<Vec<u8>> {
        let cluster = self.state.cluster()?.ok_or_else(|| anyhow::anyhow!("not clustered"))?;
        Ok(serde_json::to_vec(&BootstrapMembers {
            cluster,
            members: self.state.members()?,
        })?)
    }

    pub(crate) fn leader_raft_id(&self) -> u64 {
        self.raft().and_then(|r| r.metrics().current_leader).unwrap_or(0)
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
        self.state.members().ok()?.into_iter().find(|m| m.mesh_addr == addr)
    }

    pub(crate) fn pending_join(&self) -> bool {
        self.state.db().meta(META_PENDING).ok().flatten().is_some()
    }

    /// Mesh peers from committed membership.
    async fn reconcile_mesh(&self) {
        let Ok(members) = self.state.members() else { return };
        let peers: Vec<MeshPeer> = members
            .iter()
            .filter(|m| m.node_id != self.me)
            .filter_map(member_peer)
            .collect();
        if peers.is_empty() && self.pending_join() {
            return;
        }
        if let Err(e) = self.mesh.set_peers(peers).await {
            tracing::warn!("updating mesh peers: {e:#}");
        }
    }

    /// The leader drives raft membership from committed member documents.
    async fn reconcile_membership(&self) -> anyhow::Result<()> {
        let Some(raft) = self.raft() else { return Ok(()) };
        if !raft.is_leader() {
            return Ok(());
        }
        let metrics = raft.metrics();
        let membership = metrics.membership_config.membership().clone();
        let in_raft: BTreeSet<RaftId> = membership.nodes().map(|(id, _)| *id).collect();
        let members = self.state.members()?;
        let cluster = self.state.cluster()?;
        for m in &members {
            if !in_raft.contains(&m.raft_id) {
                tracing::info!(member = %m.name, "adding raft learner");
                raft.add_learner(
                    m.raft_id,
                    PrismNode {
                        node_id: m.node_id,
                        name: m.name.clone(),
                        addr: grpc_addr(m.mesh_addr),
                    },
                )
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
        }
        // Joining members become active once caught up with the leader.
        let last = metrics.last_log_index.unwrap_or(0);
        let replication: BTreeMap<RaftId, u64> = metrics
            .replication
            .as_ref()
            .map(|r| r.iter().map(|(id, l)| (*id, l.map_or(0, |l| l.index))).collect())
            .unwrap_or_default();
        for m in members.iter().filter(|m| m.state == MemberState::Joining) {
            if replication.get(&m.raft_id).is_some_and(|i| *i + 2 >= last) {
                self.state
                    .apply(Command::SetMemberState {
                        node_id: m.node_id,
                        state: MemberState::Active,
                    })
                    .await?;
                tracing::info!(member = %m.name, "member is active");
            }
        }
        // Removed members leave raft.
        let gone: BTreeSet<RaftId> = in_raft
            .iter()
            .copied()
            .filter(|id| !members.iter().any(|m| m.raft_id == *id))
            .collect();
        // Voters: the hub in hub-spoke mode; flagged voters in consensus mode.
        let want_voters: BTreeSet<RaftId> = members
            .iter()
            .filter(|m| m.voter && m.state == MemberState::Active)
            .map(|m| m.raft_id)
            .collect();
        let voters: BTreeSet<RaftId> = membership.voter_ids().collect();
        if !want_voters.is_empty()
            && want_voters != voters
            && cluster.is_some()
        {
            tracing::info!(?want_voters, "changing raft voters");
            raft.set_voters(want_voters).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        if !gone.is_empty() {
            raft.remove_nodes(gone).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Ok(())
    }

    async fn reconcile_loop(self: Arc<Self>) {
        let mut changes = self.state.subscribe();
        loop {
            tokio::select! {
                c = changes.recv() => {
                    if matches!(c, Err(broadcast::error::RecvError::Closed)) {
                        return;
                    }
                    if matches!(c, Ok(Change::Applied(_)) | Ok(Change::Reset) | Err(_)) {
                        self.reconcile_mesh().await;
                    }
                }
                () = tokio::time::sleep(RECONCILE_EVERY) => {}
            }
            if let Err(e) = self.reconcile_membership().await {
                tracing::debug!("membership reconcile: {e:#}");
            }
        }
    }
}

fn sponsor_peer(s: &Sponsor) -> MeshPeer {
    MeshPeer {
        pubkey: s.wg_pubkey,
        mesh_addr: s.mesh_addr,
        endpoint: s.endpoints.first().cloned(),
    }
}

fn member_peer(m: &MemberDoc) -> Option<MeshPeer> {
    Some(MeshPeer {
        pubkey: decode_key(&m.wg_pubkey).ok()?,
        mesh_addr: m.mesh_addr,
        endpoint: m.endpoints.first().cloned(),
    })
}

/// Routes commands to the leader and waits to see them applied locally.
struct Proposer {
    cluster: Arc<Cluster>,
}

impl Proposer {
    async fn propose_inner(&self, command: String) -> Result<String, StateError> {
        let raft = self
            .cluster
            .raft()
            .ok_or_else(|| StateError::Degraded("raft is not running".into()))?;
        for _ in 0..3 {
            match raft.write(command.clone()).await {
                Ok((result, _)) => return Ok(result),
                Err(ClusterError::ForwardToLeader(Some((_, leader)))) => {
                    let ch = self
                        .cluster
                        .channels
                        .get(&leader.addr)
                        .map_err(|e| StateError::Degraded(format!("cannot reach the leader: {e}")))?;
                    let mut req = tonic::Request::new(WriteRequest {
                        command: command.clone().into_bytes(),
                    });
                    req.set_timeout(prism_cluster::WRITE_TIMEOUT + Duration::from_secs(2));
                    match ClusterClient::new(ch).forward_write(req).await {
                        Ok(r) => {
                            let r = r.into_inner();
                            raft.wait_applied(r.log_index, READ_YOUR_WRITES)
                                .await
                                .map_err(|e| StateError::Degraded(e.to_string()))?;
                            return String::from_utf8(r.result).map_err(|e| StateError::Internal(e.into()));
                        }
                        // The leader moved; ask again.
                        Err(s) if s.code() == tonic::Code::FailedPrecondition => {}
                        Err(s) if matches!(s.code(), tonic::Code::Unavailable | tonic::Code::DeadlineExceeded) => {
                            return Err(StateError::Degraded(format!("the leader {} is unreachable", leader.name)));
                        }
                        Err(s) => return Err(StateError::Internal(anyhow::anyhow!("forwarding write: {s}"))),
                    }
                }
                Err(ClusterError::ForwardToLeader(None)) => {
                    return Err(StateError::Degraded("no hub or leader is reachable".into()));
                }
                Err(ClusterError::Degraded(m)) => return Err(StateError::Degraded(m)),
                Err(ClusterError::Other(m)) => return Err(StateError::Internal(anyhow::anyhow!(m))),
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Err(StateError::Degraded("leadership kept changing".into()))
    }
}

impl Propose for Proposer {
    fn propose(&self, command: String) -> BoxFuture<'_, Result<String, StateError>> {
        Box::pin(self.propose_inner(command))
    }
}
