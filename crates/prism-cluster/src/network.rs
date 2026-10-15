//! Raft RPC transport: gRPC (`h2c` on the `WireGuard` mesh) between members.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openraft::error::{
    InstallSnapshotError, NetworkError, RPCError, RaftError, RemoteError, Unreachable,
};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use prism_proto::v1::Payload;
use prism_proto::v1::raft_client::RaftClient;
use tonic::transport::{Channel, Endpoint};

use crate::types::{PrismNode, Raft, RaftId, TypeConfig};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

type RpcResult<T, E = RaftError<RaftId>> = Result<T, RPCError<RaftId, PrismNode, E>>;

/// Lazily connected gRPC channels, keyed by address.
#[derive(Debug, Clone, Default)]
pub struct Channels {
    inner: Arc<Mutex<HashMap<String, Channel>>>,
}

impl Channels {
    /// A channel to `addr` (`[ipv6]:port`). Connection happens on first use.
    pub fn get(&self, addr: &str) -> Result<Channel, tonic::transport::Error> {
        let mut map = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(c) = map.get(addr) {
            return Ok(c.clone());
        }
        let ch = Endpoint::from_shared(format!("http://{addr}"))?
            .connect_timeout(CONNECT_TIMEOUT)
            .tcp_nodelay(true)
            .http2_keep_alive_interval(Duration::from_secs(10))
            .keep_alive_timeout(Duration::from_secs(10))
            .connect_lazy();
        map.insert(addr.to_string(), ch.clone());
        Ok(ch)
    }
}

/// Creates raft clients over gRPC.
#[derive(Debug, Clone, Default)]
pub struct GrpcNetwork {
    pub channels: Channels,
}

pub struct GrpcPeer {
    target: RaftId,
    node: PrismNode,
    channels: Channels,
}

impl RaftNetworkFactory<TypeConfig> for GrpcNetwork {
    type Network = GrpcPeer;

    async fn new_client(&mut self, target: RaftId, node: &PrismNode) -> Self::Network {
        GrpcPeer {
            target,
            node: node.clone(),
            channels: self.channels.clone(),
        }
    }
}

impl GrpcPeer {
    async fn call<Req, Resp, E>(
        &self,
        req: &Req,
        timeout: Duration,
        f: impl AsyncFnOnce(
            RaftClient<Channel>,
            tonic::Request<Payload>,
        ) -> Result<tonic::Response<Payload>, tonic::Status>,
    ) -> RpcResult<Resp, E>
    where
        Req: serde::Serialize,
        Resp: serde::de::DeserializeOwned,
        E: std::error::Error + serde::de::DeserializeOwned,
    {
        let json = serde_json::to_vec(req).map_err(|e| RPCError::Network(NetworkError::new(&e)))?;
        let ch = self
            .channels
            .get(&self.node.addr)
            .map_err(|e| RPCError::Unreachable(Unreachable::new(&e)))?;
        let mut request = tonic::Request::new(Payload { json });
        request.set_timeout(timeout);
        let reply = f(RaftClient::new(ch), request)
            .await
            .map_err(|s| match s.code() {
                tonic::Code::Unavailable | tonic::Code::DeadlineExceeded => {
                    RPCError::Unreachable(Unreachable::new(&s))
                }
                _ => RPCError::Network(NetworkError::new(&s)),
            })?;
        let res: Result<Resp, E> = serde_json::from_slice(&reply.into_inner().json)
            .map_err(|e| RPCError::Network(NetworkError::new(&e)))?;
        res.map_err(|e| {
            RPCError::RemoteError(RemoteError::new_with_node(
                self.target,
                self.node.clone(),
                e,
            ))
        })
    }
}

impl RaftNetwork<TypeConfig> for GrpcPeer {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<RaftId>> {
        self.call(&rpc, option.hard_ttl(), async |mut c, r| {
            c.append_entries(r).await
        })
        .await
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<InstallSnapshotResponse<RaftId>, RaftError<RaftId, InstallSnapshotError>> {
        self.call(&rpc, option.hard_ttl(), async |mut c, r| {
            c.install_snapshot(r).await
        })
        .await
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<RaftId>,
        option: RPCOption,
    ) -> RpcResult<VoteResponse<RaftId>> {
        self.call(&rpc, option.hard_ttl(), async |mut c, r| c.vote(r).await)
            .await
    }
}

/// The gRPC `Raft` service: hands requests to the local raft instance.
#[derive(Clone)]
pub struct RaftService {
    pub raft: Raft,
}

fn invalid(e: &serde_json::Error) -> tonic::Status {
    tonic::Status::invalid_argument(format!("bad raft payload: {e}"))
}

fn reply<T: serde::Serialize>(v: &T) -> Result<tonic::Response<Payload>, tonic::Status> {
    serde_json::to_vec(v)
        .map(|json| tonic::Response::new(Payload { json }))
        .map_err(|e| tonic::Status::internal(e.to_string()))
}

#[tonic::async_trait]
impl prism_proto::v1::raft_server::Raft for RaftService {
    async fn append_entries(
        &self,
        req: tonic::Request<Payload>,
    ) -> Result<tonic::Response<Payload>, tonic::Status> {
        let rpc: AppendEntriesRequest<TypeConfig> =
            serde_json::from_slice(&req.into_inner().json).map_err(|e| invalid(&e))?;
        reply(&self.raft.append_entries(rpc).await)
    }

    async fn vote(
        &self,
        req: tonic::Request<Payload>,
    ) -> Result<tonic::Response<Payload>, tonic::Status> {
        let rpc: VoteRequest<RaftId> =
            serde_json::from_slice(&req.into_inner().json).map_err(|e| invalid(&e))?;
        reply(&self.raft.vote(rpc).await)
    }

    async fn install_snapshot(
        &self,
        req: tonic::Request<Payload>,
    ) -> Result<tonic::Response<Payload>, tonic::Status> {
        let rpc: InstallSnapshotRequest<TypeConfig> =
            serde_json::from_slice(&req.into_inner().json).map_err(|e| invalid(&e))?;
        reply(&self.raft.install_snapshot(rpc).await)
    }
}
