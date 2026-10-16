//! Raft RPC transport: gRPC (`h2c` on the `WireGuard` mesh) between members.
//!
//! Requests and replies are JSON in a `Payload` message. The server replies
//! with a serialized `Result`, so a raft-level error (for example "not a
//! voter") reaches the caller as a [`RemoteError`] and a gRPC status is used
//! only for transport and decode failures.
//!
//! Timeouts: each RPC carries the deadline openraft gives it
//! (`RPCOption::hard_ttl`, derived from the heartbeat and election settings
//! in [`crate::raft_config`]). A TCP connect gives up after 3 seconds, and
//! HTTP/2 pings every 10 seconds drop a connection whose peer stops
//! answering for 10 seconds.
#![allow(clippy::result_large_err)]

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

use crate::sync::lock_unpoisoned;
use crate::types::{PrismNode, Raft, RaftId, TypeConfig};

/// How long a TCP connect may take before the peer counts as unreachable.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Interval between HTTP/2 keepalive pings on an idle connection.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
/// How long a keepalive ping may go unanswered before the connection drops.
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

type RpcResult<T, E = RaftError<RaftId>> = Result<T, RPCError<RaftId, PrismNode, E>>;

/// Lazily connected gRPC channels, keyed by address.
#[derive(Debug, Clone, Default)]
pub struct Channels {
    inner: Arc<Mutex<HashMap<String, Channel>>>,
}

impl Channels {
    /// A channel to `addr` (`[ipv6]:port`). Connection happens on first use.
    pub fn get(&self, addr: &str) -> Result<Channel, tonic::transport::Error> {
        let mut map = lock_unpoisoned(&self.inner);
        if let Some(c) = map.get(addr) {
            return Ok(c.clone());
        }
        let ch = Endpoint::from_shared(format!("http://{addr}"))?
            .connect_timeout(CONNECT_TIMEOUT)
            .tcp_nodelay(true)
            .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
            .keep_alive_timeout(KEEPALIVE_TIMEOUT)
            .connect_lazy();
        map.insert(addr.to_string(), ch.clone());
        Ok(ch)
    }

    /// Drops the cached channel for `addr`, so the next [`get`](Self::get)
    /// builds a fresh one. Called when a peer is unavailable, which bounds
    /// the cache for members that moved to a new address.
    pub fn forget(&self, addr: &str) {
        lock_unpoisoned(&self.inner).remove(addr);
    }
}

/// Creates raft clients over gRPC.
#[derive(Debug, Clone, Default)]
pub struct GrpcNetwork {
    channels: Channels,
}

impl GrpcNetwork {
    /// A network that shares `channels` with the rest of the process.
    pub fn new(channels: Channels) -> Self {
        Self { channels }
    }
}

/// A raft client to one member, created by [`GrpcNetwork`].
#[derive(Debug)]
pub struct GrpcPeer {
    target: RaftId,
    node: PrismNode,
    channels: Channels,
}

#[allow(clippy::unused_async_trait_impl)] // creating a client does no I/O
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
        let reply = f(RaftClient::new(ch), request).await.map_err(|s| {
            if s.code() == tonic::Code::Unavailable {
                self.channels.forget(&self.node.addr);
            }
            map_status(&s)
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

/// Maps a gRPC failure to the openraft error that drives its retry policy:
/// `Unavailable` and `DeadlineExceeded` mean the peer is unreachable, any
/// other code is a network error.
fn map_status<E: std::error::Error>(s: &tonic::Status) -> RPCError<RaftId, PrismNode, E> {
    match s.code() {
        tonic::Code::Unavailable | tonic::Code::DeadlineExceeded => {
            RPCError::Unreachable(Unreachable::new(s))
        }
        _ => RPCError::Network(NetworkError::new(s)),
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
    raft: Raft,
}

impl std::fmt::Debug for RaftService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RaftService").finish_non_exhaustive()
    }
}

impl RaftService {
    /// A service that dispatches to `raft`.
    pub fn new(raft: Raft) -> Self {
        Self { raft }
    }
}

/// Decodes the JSON request, runs `f` and encodes its result. Failures to
/// decode become `InvalidArgument` and are logged, since the caller sees only
/// the status.
async fn handle<Req, Resp>(
    rpc: &'static str,
    req: tonic::Request<Payload>,
    f: impl AsyncFnOnce(Req) -> Resp,
) -> Result<tonic::Response<Payload>, tonic::Status>
where
    Req: serde::de::DeserializeOwned,
    Resp: serde::Serialize,
{
    let rpc_req: Req = serde_json::from_slice(&req.into_inner().json).map_err(|e| {
        tracing::warn!(rpc, error = %e, "malformed raft request");
        tonic::Status::invalid_argument(format!("bad raft payload: {e}"))
    })?;
    let resp = f(rpc_req).await;
    serde_json::to_vec(&resp)
        .map(|json| tonic::Response::new(Payload { json }))
        .map_err(|e| {
            tracing::warn!(rpc, error = %e, "cannot encode raft reply");
            tonic::Status::internal(e.to_string())
        })
}

#[tonic::async_trait]
impl prism_proto::v1::raft_server::Raft for RaftService {
    async fn append_entries(
        &self,
        req: tonic::Request<Payload>,
    ) -> Result<tonic::Response<Payload>, tonic::Status> {
        handle(
            "append_entries",
            req,
            async |rpc: AppendEntriesRequest<TypeConfig>| self.raft.append_entries(rpc).await,
        )
        .await
    }

    async fn vote(
        &self,
        req: tonic::Request<Payload>,
    ) -> Result<tonic::Response<Payload>, tonic::Status> {
        handle("vote", req, async |rpc: VoteRequest<RaftId>| {
            self.raft.vote(rpc).await
        })
        .await
    }

    async fn install_snapshot(
        &self,
        req: tonic::Request<Payload>,
    ) -> Result<tonic::Response<Payload>, tonic::Status> {
        handle(
            "install_snapshot",
            req,
            async |rpc: InstallSnapshotRequest<TypeConfig>| self.raft.install_snapshot(rpc).await,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Err = RPCError<RaftId, PrismNode, RaftError<RaftId>>;

    #[test]
    fn unavailable_and_deadline_are_unreachable() {
        for code in [tonic::Code::Unavailable, tonic::Code::DeadlineExceeded] {
            let e: Err = map_status(&tonic::Status::new(code, "x"));
            assert!(matches!(e, RPCError::Unreachable(_)), "{code:?}");
        }
    }

    #[test]
    fn other_codes_are_network_errors() {
        for code in [tonic::Code::Internal, tonic::Code::InvalidArgument] {
            let e: Err = map_status(&tonic::Status::new(code, "x"));
            assert!(matches!(e, RPCError::Network(_)), "{code:?}");
        }
    }

    #[test]
    fn vote_request_round_trips_through_json() {
        let req = VoteRequest::<RaftId>::new(openraft::Vote::new(4, 2), None);
        let json = serde_json::to_vec(&req).unwrap();
        let back: VoteRequest<RaftId> = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.vote, req.vote);
    }

    #[tokio::test]
    async fn channels_forget_drops_the_cache_entry() {
        let c = Channels::default();
        let _ = c.get("[::1]:1").unwrap();
        assert_eq!(lock_unpoisoned(&c.inner).len(), 1);
        c.forget("[::1]:1");
        assert!(lock_unpoisoned(&c.inner).is_empty());
    }
}
