//! Member-to-member services that make any node able to serve any request:
//! event fan-in, HTTP forwarding and console tunnelling.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::time::Duration;

use axum::body::Body;
use futures::{Stream, StreamExt};
use prism_proto::v1::console_client::ConsoleClient;
use prism_proto::v1::console_server::Console;
use prism_proto::v1::events_client::EventsClient;
use prism_proto::v1::events_server::Events;
use prism_proto::v1::http_tunnel_client::HttpTunnelClient;
use prism_proto::v1::http_tunnel_server::HttpTunnel;
use prism_proto::v1::{ConsoleFrame, EventMsg, HttpFrame, HttpHeader, SubscribeRequest};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use super::model::{MemberDoc, MemberState};
use super::runtime::Cluster;
use crate::events::Event;
use crate::node::Node;

/// Marks a request that arrived through the tunnel from a peer: it skips
/// request hygiene (already checked by the first node) and is never
/// forwarded again.
#[derive(Debug, Clone, Copy)]
pub struct Tunnelled;

pub const FORWARDED_BY: &str = "x-prism-forwarded-by";
pub const SERVED_BY: &str = "x-prism-served-by";
const FRAME_CHUNK: usize = 256 * 1024;
/// The first event on a subscription: the peer's runtime state.
const SNAPSHOT_ID: &str = "runtime-snapshot";

type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

pub fn grpc_addr(m: &MemberDoc) -> String {
    format!("[{}]:{}", m.mesh_addr, prism_proto::GRPC_PORT)
}

fn node_of(cluster: &Cluster) -> Result<Arc<Node>, Status> {
    cluster
        .node
        .get()
        .and_then(Weak::upgrade)
        .ok_or_else(|| Status::unavailable("node is starting"))
}

pub struct EventsService {
    pub cluster: Arc<Cluster>,
}

#[tonic::async_trait]
impl Events for EventsService {
    type SubscribeStream = BoxStream<EventMsg>;

    /// The runtime snapshot, then this node's own events as they happen.
    async fn subscribe(
        &self,
        req: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        super::grpc::check_active(&self.cluster, req.remote_addr())?;
        let node = node_of(&self.cluster)?;
        let me = node.bus.node().to_string();
        let (_, mut rx) = node.bus.subscribe(None);
        let snapshot = serde_json::to_vec(&node.runtime_snapshot())
            .map_err(|e| Status::internal(e.to_string()))?;
        let (tx, out) = mpsc::channel(512);
        tokio::spawn(async move {
            if tx
                .send(Ok(EventMsg {
                    id: SNAPSHOT_ID.into(),
                    event: snapshot,
                }))
                .await
                .is_err()
            {
                return;
            }
            loop {
                match rx.recv().await {
                    Ok(e) if e.event.origin_node == me => {
                        let Ok(event) = serde_json::to_vec(&e.event) else {
                            continue;
                        };
                        if tx.send(Ok(EventMsg { id: e.id, event })).await.is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    // Too slow: end the stream; the peer resubscribes and
                    // gets a fresh snapshot.
                    Err(_) => return,
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(out))))
    }
}

/// Keeps one subscription per active peer, folding their events into this
/// node's caches and stream.
pub async fn fan_in(cluster: Arc<Cluster>) {
    let mut tasks: HashMap<Uuid, tokio::task::JoinHandle<()>> = HashMap::new();
    let mut changes = cluster.state.subscribe();
    loop {
        let peers: Vec<MemberDoc> = cluster
            .state
            .members()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.node_id != cluster.me && m.state == MemberState::Active)
            .collect();
        tasks.retain(|id, t| {
            let keep = peers.iter().any(|p| p.node_id == *id) && !t.is_finished();
            if !keep {
                t.abort();
            }
            keep
        });
        for p in peers {
            tasks
                .entry(p.node_id)
                .or_insert_with(|| tokio::spawn(follow_peer(Arc::clone(&cluster), p)));
        }
        tokio::select! {
            r = changes.recv() => {
                if matches!(r, Err(broadcast::error::RecvError::Closed)) {
                    return;
                }
            }
            () = tokio::time::sleep(Duration::from_secs(10)) => {}
        }
    }
}

async fn follow_peer(cluster: Arc<Cluster>, peer: MemberDoc) {
    let mut delay = Duration::from_secs(1);
    loop {
        let res: anyhow::Result<()> = async {
            let ch = cluster.channels.get(&grpc_addr(&peer))?;
            let mut stream = EventsClient::new(ch)
                .subscribe(SubscribeRequest {
                    since: String::new(),
                })
                .await?
                .into_inner();
            while let Some(msg) = stream.message().await? {
                let Some(node) = cluster.node.get().and_then(Weak::upgrade) else {
                    return Ok(());
                };
                if msg.id == SNAPSHOT_ID {
                    node.apply_remote_snapshot(serde_json::from_slice(&msg.event)?);
                    node.bus.publish(
                        "node.status",
                        format!("/nodes/{}", peer.node_id),
                        crate::events::Op::Upsert,
                        serde_json::to_value(
                            node.nodes().into_iter().find(|n| n.id == peer.node_id),
                        )?,
                    );
                    continue;
                }
                let ev: Event = serde_json::from_slice(&msg.event)?;
                node.apply_remote_event(&ev);
                node.bus.republish(ev);
            }
            Ok(())
        }
        .await;
        if let Err(e) = res {
            tracing::debug!(peer = %peer.name, "event stream: {e:#}");
        }
        if let Some(node) = cluster.node.get().and_then(Weak::upgrade) {
            node.forget_remote(peer.node_id);
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(15));
    }
}

pub struct TunnelService {
    pub cluster: Arc<Cluster>,
}

#[tonic::async_trait]
impl HttpTunnel for TunnelService {
    type ForwardStream = BoxStream<HttpFrame>;

    /// Runs a forwarded request through this node's own HTTP API.
    async fn forward(
        &self,
        req: Request<Streaming<HttpFrame>>,
    ) -> Result<Response<Self::ForwardStream>, Status> {
        super::grpc::check_active(&self.cluster, req.remote_addr())?;
        let router = self
            .cluster
            .http
            .get()
            .cloned()
            .ok_or_else(|| Status::unavailable("node is starting"))?;
        let me = self.cluster.me;
        let mut frames = req.into_inner();
        let head = frames
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("missing request head"))?;
        // Body: the head's bytes, then frames until one says `end`.
        let first = bytes::Bytes::from(head.body.clone());
        let rest = if head.end {
            futures::stream::empty().boxed()
        } else {
            futures::stream::unfold(frames, |mut s| async move {
                match s.message().await {
                    Ok(Some(f)) if f.end && f.body.is_empty() => None,
                    Ok(Some(f)) => Some((Ok(bytes::Bytes::from(f.body)), s)),
                    Ok(None) => None,
                    Err(e) => Some((Err(std::io::Error::other(e.to_string())), s)),
                }
            })
            .boxed()
        };
        let body_stream = futures::stream::once(async move { Ok::<_, std::io::Error>(first) })
            .chain(rest)
            .filter(|b| futures::future::ready(b.as_ref().map_or(true, |b| !b.is_empty())));
        let mut builder = axum::http::Request::builder()
            .method(head.method.as_str())
            .uri(&head.path);
        for h in &head.headers {
            builder = builder.header(&h.name, h.value.as_slice());
        }
        let mut request = builder
            .body(Body::from_stream(body_stream))
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        request.extensions_mut().insert(Tunnelled);
        let response = tower::ServiceExt::oneshot(router, request)
            .await
            .unwrap_or_else(|e| match e {});
        let (parts, body) = response.into_parts();
        let mut headers: Vec<HttpHeader> = parts
            .headers
            .iter()
            .map(|(k, v)| HttpHeader {
                name: k.as_str().to_string(),
                value: v.as_bytes().to_vec(),
            })
            .collect();
        headers.push(HttpHeader {
            name: SERVED_BY.into(),
            value: me.to_string().into_bytes(),
        });
        let head = HttpFrame {
            status: u32::from(parts.status.as_u16()),
            headers,
            ..HttpFrame::default()
        };
        let body = body.into_data_stream().map(|chunk| match chunk {
            Ok(b) => Ok(HttpFrame {
                body: b.to_vec(),
                ..HttpFrame::default()
            }),
            Err(e) => Err(Status::internal(e.to_string())),
        });
        let out = futures::stream::once(async move { Ok(head) })
            .chain(body)
            .chain(futures::stream::once(async {
                Ok(HttpFrame {
                    end: true,
                    ..HttpFrame::default()
                })
            }));
        Ok(Response::new(Box::pin(out)))
    }
}

/// Forwards an HTTP request to `member` and streams the response back.
pub async fn forward_http(
    cluster: &Cluster,
    member: &MemberDoc,
    req: axum::http::Request<Body>,
) -> anyhow::Result<axum::response::Response> {
    let (parts, body) = req.into_parts();
    let mut headers: Vec<HttpHeader> = parts
        .headers
        .iter()
        .filter(|(k, _)| {
            !matches!(
                k.as_str(),
                "connection" | "transfer-encoding" | "keep-alive" | "upgrade"
            )
        })
        .map(|(k, v)| HttpHeader {
            name: k.as_str().to_string(),
            value: v.as_bytes().to_vec(),
        })
        .collect();
    headers.push(HttpHeader {
        name: FORWARDED_BY.into(),
        value: cluster.me.to_string().into_bytes(),
    });
    let head = HttpFrame {
        method: parts.method.to_string(),
        path: parts
            .uri
            .path_and_query()
            .map_or_else(|| "/".into(), ToString::to_string),
        headers,
        ..HttpFrame::default()
    };
    let body = body.into_data_stream().flat_map(|chunk| {
        let frames: Vec<HttpFrame> = match chunk {
            Ok(b) => b
                .chunks(FRAME_CHUNK)
                .map(|c| HttpFrame {
                    body: c.to_vec(),
                    ..HttpFrame::default()
                })
                .collect(),
            Err(_) => vec![],
        };
        futures::stream::iter(frames)
    });
    let outbound = futures::stream::once(async move { head })
        .chain(body)
        .chain(futures::stream::once(async {
            HttpFrame {
                end: true,
                ..HttpFrame::default()
            }
        }));
    let ch = cluster.channels.get(&grpc_addr(member))?;
    let mut inbound = HttpTunnelClient::new(ch)
        .forward(outbound)
        .await?
        .into_inner();
    let head = inbound
        .message()
        .await?
        .ok_or_else(|| anyhow::anyhow!("peer closed the tunnel"))?;
    let mut res = axum::http::Response::builder().status(u16::try_from(head.status)?);
    for h in &head.headers {
        res = res.header(&h.name, h.value.as_slice());
    }
    let body = futures::stream::unfold(inbound, |mut s| async move {
        match s.message().await {
            Ok(Some(f)) if f.end => None,
            Ok(Some(f)) => Some((Ok::<_, std::io::Error>(bytes::Bytes::from(f.body)), s)),
            Ok(None) => None,
            Err(e) => Some((Err(std::io::Error::other(e.to_string())), s)),
        }
    });
    Ok(res.body(Body::from_stream(body))?)
}

pub struct ConsoleService {
    pub cluster: Arc<Cluster>,
}

#[tonic::async_trait]
impl Console for ConsoleService {
    type OpenStream = BoxStream<ConsoleFrame>;

    /// Bridges a peer to a local VM's VNC or serial console.
    async fn open(
        &self,
        req: Request<Streaming<ConsoleFrame>>,
    ) -> Result<Response<Self::OpenStream>, Status> {
        super::grpc::check_active(&self.cluster, req.remote_addr())?;
        let node = node_of(&self.cluster)?;
        let mut inbound = req.into_inner();
        let first = inbound
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("missing console request"))?;
        let vm = Uuid::parse_str(&first.vm).map_err(|_| Status::invalid_argument("bad VM ID"))?;
        let (to_peer, out) = mpsc::channel::<Result<ConsoleFrame, Status>>(64);
        let (from_peer, mut from_peer_rx) = mpsc::channel::<Vec<u8>>(64);
        tokio::spawn(async move {
            while let Ok(Some(f)) = inbound.message().await {
                if from_peer.send(f.data).await.is_err() {
                    return;
                }
            }
        });
        match first.kind.as_str() {
            "vnc" => {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let path = node
                    .vnc_socket(vm)
                    .await
                    .map_err(|e| Status::failed_precondition(e.to_string()))?;
                let sock = tokio::net::UnixStream::connect(&path)
                    .await
                    .map_err(|e| Status::unavailable(e.to_string()))?;
                let (mut rd, mut wr) = sock.into_split();
                tokio::spawn(async move {
                    while let Some(d) = from_peer_rx.recv().await {
                        if wr.write_all(&d).await.is_err() {
                            return;
                        }
                    }
                });
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        match rd.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => {
                                let f = ConsoleFrame {
                                    data: buf[..n].to_vec(),
                                    ..ConsoleFrame::default()
                                };
                                if to_peer.send(Ok(f)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                });
            }
            "serial" => {
                let stream = node
                    .open_serial(vm)
                    .await
                    .map_err(|e| Status::failed_precondition(e.to_string()))?;
                let (mut reader, writer) = stream.into_split();
                tokio::spawn(async move {
                    while let Some(d) = from_peer_rx.recv().await {
                        if writer.send(&d).is_err() {
                            return;
                        }
                    }
                });
                tokio::spawn(async move {
                    while let Some(Ok(d)) = reader.recv().await {
                        let f = ConsoleFrame {
                            data: d,
                            ..ConsoleFrame::default()
                        };
                        if to_peer.send(Ok(f)).await.is_err() {
                            return;
                        }
                    }
                });
            }
            k => {
                return Err(Status::invalid_argument(format!(
                    "unknown console kind {k}"
                )));
            }
        }
        Ok(Response::new(Box::pin(ReceiverStream::new(out))))
    }
}

/// Opens a console on the VM's owner: send bytes on the returned sender,
/// read the VM's output from the stream.
pub async fn open_remote_console(
    cluster: &Cluster,
    member: &MemberDoc,
    vm: Uuid,
    kind: &str,
) -> anyhow::Result<(mpsc::Sender<Vec<u8>>, Streaming<ConsoleFrame>)> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
    let first = ConsoleFrame {
        vm: vm.to_string(),
        kind: kind.into(),
        data: Vec::new(),
    };
    let outbound =
        futures::stream::once(async move { first }).chain(ReceiverStream::new(rx).map(|data| {
            ConsoleFrame {
                data,
                ..ConsoleFrame::default()
            }
        }));
    let ch = cluster.channels.get(&grpc_addr(member))?;
    let inbound = ConsoleClient::new(ch).open(outbound).await?.into_inner();
    Ok((tx, inbound))
}
