//! gRPC services for members, served as plaintext h2c on the mesh address.
//!
//! `WireGuard` authenticates every packet, so the source address identifies
//! the calling member. Joining members may call only `Cluster.Status` and
//! `Cluster.Bootstrap`.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use prism_cluster::{ClusterError, RaftService};
use prism_proto::v1::cluster_server::{Cluster as ClusterRpc, ClusterServer};
use prism_proto::v1::console_server::ConsoleServer;
use prism_proto::v1::events_server::EventsServer;
use prism_proto::v1::http_tunnel_server::HttpTunnelServer;
use prism_proto::v1::raft_server::RaftServer;
use prism_proto::v1::{BootstrapReply, Empty, StatusReply, WriteReply, WriteRequest};
use tonic::{Request, Response, Status};

use super::model::MemberState;
use super::runtime::Cluster;

/// Who is calling, by mesh source address.
enum Caller {
    Active,
    Joining,
    /// Not (yet) known locally: allowed only while this node is joining and
    /// has no membership list of its own.
    Unknown,
}

fn caller(cluster: &Cluster, remote: Option<SocketAddr>) -> Result<Caller, Status> {
    let Some(IpAddr::V6(addr)) = remote.map(|r| r.ip()) else {
        return Err(Status::permission_denied("calls must come over the mesh"));
    };
    match cluster.member_at(addr) {
        Some(m) if m.state == MemberState::Active => Ok(Caller::Active),
        Some(_) => Ok(Caller::Joining),
        None if cluster.pending_join() => Ok(Caller::Unknown),
        None => Err(Status::permission_denied(format!("{addr} is not a member"))),
    }
}

/// Allows only active members (or anyone while this node is still joining).
pub fn check_active(cluster: &Cluster, remote: Option<SocketAddr>) -> Result<(), Status> {
    require_active(cluster, remote)
}

fn require_active(cluster: &Cluster, remote: Option<SocketAddr>) -> Result<(), Status> {
    match caller(cluster, remote)? {
        Caller::Active | Caller::Unknown => Ok(()),
        Caller::Joining => Err(Status::permission_denied(
            "joining members may only bootstrap",
        )),
    }
}

pub struct ClusterService {
    pub cluster: Arc<Cluster>,
}

#[tonic::async_trait]
impl ClusterRpc for ClusterService {
    async fn status(&self, req: Request<Empty>) -> Result<Response<StatusReply>, Status> {
        caller(&self.cluster, req.remote_addr())?;
        let s = self
            .cluster
            .status()
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(StatusReply {
            status: serde_json::to_vec(&s).map_err(|e| Status::internal(e.to_string()))?,
        }))
    }

    async fn bootstrap(&self, req: Request<Empty>) -> Result<Response<BootstrapReply>, Status> {
        caller(&self.cluster, req.remote_addr())?;
        Ok(Response::new(BootstrapReply {
            members: self
                .cluster
                .bootstrap_members()
                .map_err(|e| Status::unavailable(e.to_string()))?,
            leader_raft_id: self.cluster.leader_raft_id(),
        }))
    }

    async fn forward_write(
        &self,
        req: Request<WriteRequest>,
    ) -> Result<Response<WriteReply>, Status> {
        require_active(&self.cluster, req.remote_addr())?;
        let command = String::from_utf8(req.into_inner().command)
            .map_err(|_| Status::invalid_argument("command is not UTF-8"))?;
        match self.cluster.write_local(command).await {
            Ok(outcome) => Ok(Response::new(WriteReply {
                result: outcome.result.into_bytes(),
                log_index: outcome.index,
            })),
            Err(ClusterError::ForwardToLeader(_)) => {
                Err(Status::failed_precondition("not the leader"))
            }
            Err(ClusterError::Degraded(r)) => Err(Status::unavailable(r.to_string())),
            Err(ClusterError::Raft(e)) => Err(Status::internal(e.to_string())),
        }
    }
}

/// Wraps the raft service with the caller check.
#[derive(Clone)]
struct GuardedRaft {
    inner: RaftService,
    cluster: Arc<Cluster>,
}

#[tonic::async_trait]
impl prism_proto::v1::raft_server::Raft for GuardedRaft {
    async fn append_entries(
        &self,
        req: Request<prism_proto::v1::Payload>,
    ) -> Result<Response<prism_proto::v1::Payload>, Status> {
        require_active(&self.cluster, req.remote_addr())?;
        self.inner.append_entries(req).await
    }

    async fn vote(
        &self,
        req: Request<prism_proto::v1::Payload>,
    ) -> Result<Response<prism_proto::v1::Payload>, Status> {
        require_active(&self.cluster, req.remote_addr())?;
        self.inner.vote(req).await
    }

    async fn install_snapshot(
        &self,
        req: Request<prism_proto::v1::Payload>,
    ) -> Result<Response<prism_proto::v1::Payload>, Status> {
        require_active(&self.cluster, req.remote_addr())?;
        self.inner.install_snapshot(req).await
    }
}

/// Serves gRPC on the mesh address, retrying the bind until `wg-prism`
/// carries it.
pub async fn serve(cluster: Arc<Cluster>, raft: prism_cluster::Raft) {
    loop {
        let Some(addr) = cluster.grpc_listen() else {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        };
        let router = tonic::transport::Server::builder()
            .tcp_nodelay(true)
            .http2_keepalive_interval(Some(Duration::from_secs(10)))
            .add_service(ClusterServer::new(ClusterService {
                cluster: Arc::clone(&cluster),
            }))
            .add_service(RaftServer::new(GuardedRaft {
                inner: RaftService::new(raft.clone()),
                cluster: Arc::clone(&cluster),
            }))
            .add_service(EventsServer::new(super::peers::EventsService {
                cluster: Arc::clone(&cluster),
            }))
            .add_service(HttpTunnelServer::new(super::peers::TunnelService {
                cluster: Arc::clone(&cluster),
            }))
            .add_service(ConsoleServer::new(super::peers::ConsoleService {
                cluster: Arc::clone(&cluster),
            }))
            .add_service(prism_proto::v1::data_plane_server::DataPlaneServer::new(
                super::dataplane::DataPlaneService {
                    cluster: Arc::clone(&cluster),
                    plane: Arc::clone(&cluster.dataplane),
                },
            ));
        tracing::info!("gRPC listening on {addr}");
        if let Err(e) = router.serve(addr).await {
            tracing::warn!("gRPC server on {addr}: {e}; retrying");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
