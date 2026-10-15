//! Forming and growing a cluster: init, invite, accept and join.

use std::net::Ipv6Addr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use prism_cluster::{PrismNode, RaftId};
use prism_proto::v1::cluster_client::ClusterClient;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::reconcile::{member_peer, sponsor_peer};
use super::{
    B64, Cluster, ClusterOpError, ClusterStatus, LocalMember, META_LOCAL, META_PENDING,
    PendingJoin, decode_key, grpc_addr, now,
};
use crate::cluster::blob::{self, Invite, Reply, Sponsor};
use crate::cluster::mesh::DEFAULT_MTU;
use crate::cluster::model::{
    ClusterDoc, ClusterMode, FEATURE_LEVEL, InviteDoc, MemberDoc, MemberState, levels_compatible,
};
use crate::spec::VmSpec;
use crate::state::{Command, PoolDoc, StateError};

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
    /// Turns this standalone node into the hub of a new cluster.
    pub async fn init(
        self: &Arc<Self>,
        cluster_name: &str,
        mode: ClusterMode,
    ) -> Result<ClusterStatus, ClusterOpError> {
        if self.raft().is_some() || self.state.db().meta(META_PENDING)?.is_some() {
            return Err(ClusterOpError::Conflict(
                "this node is already in a cluster".into(),
            ));
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
        let pubkey = self
            .mesh
            .ensure(mesh_addr, cluster.wg_mtu, self.cfg.wireguard_port)
            .await?;
        let local = LocalMember {
            cluster_id: cluster.cluster_id,
            raft_id,
            mesh_addr,
            wg_mtu: cluster.wg_mtu,
        };
        self.state
            .db()
            .set_meta(META_LOCAL, &serde_json::to_string(&local)?)?;
        *self
            .local
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(local);
        self.start_raft(raft_id).await?;
        let raft = self
            .raft()
            .ok_or_else(|| anyhow::anyhow!("raft did not start"))?;
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
            feature_level: crate::cluster::model::FEATURE_LEVEL,
            joined: now(),
        };
        // Wait for this single voter to become leader, then commit.
        for _ in 0..100 {
            if raft.is_leader() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.state
            .apply(Command::InitCluster { cluster, hub })
            .await?;
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
    pub async fn invite(
        &self,
        name_hint: Option<String>,
        ttl: Duration,
    ) -> Result<InviteView, ClusterOpError> {
        let cluster = self.state.cluster()?.ok_or_else(|| {
            ClusterOpError::Invalid("this node is not in a cluster; create one first".into())
        })?;
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
            match self
                .state
                .apply(Command::CreateInvite { invite: doc })
                .await
            {
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
                            endpoints: if me.endpoints.is_empty() {
                                self.endpoints()
                            } else {
                                me.endpoints.clone()
                            },
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
    pub async fn accept(
        &self,
        reply_blob: &str,
        confirm: bool,
    ) -> Result<AcceptPreview, ClusterOpError> {
        let reply =
            Reply::decode(reply_blob).map_err(|e| ClusterOpError::Invalid(e.to_string()))?;
        let cluster = self
            .state
            .cluster()?
            .ok_or_else(|| ClusterOpError::Invalid("this node is not in a cluster".into()))?;
        if reply.cluster_id != cluster.cluster_id {
            return Err(ClusterOpError::Invalid(
                "cluster_mismatch: this reply belongs to a different cluster".into(),
            ));
        }
        if !levels_compatible(reply.feature_level, FEATURE_LEVEL) {
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
            feature_level: crate::cluster::model::FEATURE_LEVEL,
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
        let invite =
            Invite::decode(invite_blob).map_err(|e| ClusterOpError::Invalid(e.to_string()))?;
        if self.raft().is_some() {
            return Err(ClusterOpError::Conflict(
                "this node is already in a cluster".into(),
            ));
        }
        if invite.exp < now() {
            tracing::warn!(
                skew_secs = now() - invite.exp,
                "the invite has expired by this node's clock; the cluster decides"
            );
        }
        if !levels_compatible(invite.feature_level, FEATURE_LEVEL) {
            return Err(ClusterOpError::Invalid(format!(
                "version_skew: the cluster runs prism {}; upgrade one side",
                invite.version
            )));
        }
        let pubkey = self
            .mesh
            .ensure(invite.assigned_addr, invite.wg_mtu, self.cfg.wireguard_port)
            .await?;
        self.mesh
            .set_peers(vec![sponsor_peer(&invite.sponsor)])
            .await?;
        let s = invite.assigned_addr.segments();
        let raft_id = (u64::from(s[5]) << 32) | (u64::from(s[6]) << 16) | u64::from(s[7]);
        let pending = PendingJoin {
            invite: invite.encode(),
            raft_id,
            sponsor_addr: invite.sponsor.mesh_addr,
        };
        self.state
            .db()
            .set_meta(META_PENDING, &serde_json::to_string(&pending)?)?;
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
    pub(super) async fn finish_join(self: Arc<Self>, pending: PendingJoin) {
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
                && self
                    .state
                    .members()
                    .is_ok_and(|m| m.iter().any(|m| m.node_id == self.me))
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
            self.state
                .db()
                .set_meta(META_LOCAL, &serde_json::to_string(&local)?)?;
            self.state.db().delete_meta(META_PENDING)
        })() {
            tracing::error!("recording membership: {e:#}");
        }
        *self
            .local
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(local);
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
        let cluster = self
            .state
            .cluster()?
            .ok_or_else(|| anyhow::anyhow!("not clustered"))?;
        Ok(serde_json::to_vec(&BootstrapMembers {
            cluster,
            members: self.state.members()?,
        })?)
    }
}
