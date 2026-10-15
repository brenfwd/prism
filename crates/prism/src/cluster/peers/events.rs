//! Event fan-in: each member streams its own events to every peer, which
//! folds them into its caches and its own SSE stream.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use prism_proto::v1::events_client::EventsClient;
use prism_proto::v1::events_server::Events;
use prism_proto::v1::{EventMsg, SubscribeRequest};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use super::{BoxStream, grpc_addr, node_of};
use crate::cluster::model::{MemberDoc, MemberState};
use crate::cluster::runtime::Cluster;
use crate::events::Event;

/// The first event on a subscription: the peer's runtime state.
const SNAPSHOT_ID: &str = "runtime-snapshot";

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
        crate::cluster::grpc::check_active(&self.cluster, req.remote_addr())?;
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
