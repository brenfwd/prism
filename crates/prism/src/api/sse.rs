//! `GET /api/v1/events`: one SSE stream with a snapshot, then live deltas.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;

use super::AppState;
use crate::events::{Resume, Sequenced};
use crate::node::{Node, NodeView, VmView};
use crate::storage::PoolInfo;

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    snapshot: Option<u8>,
}

#[derive(Debug, Serialize)]
struct Snapshot {
    cursor: String,
    nodes: Vec<NodeView>,
    vms: Vec<VmView>,
    pools: Vec<PoolInfo>,
}

fn snapshot(node: &Node) -> SseEvent {
    let cursor = node.bus.cursor();
    let snap = Snapshot {
        cursor: cursor.clone(),
        nodes: vec![node.view()],
        vms: node.vms().unwrap_or_default(),
        pools: node.storage.list().iter().map(|p| p.info()).collect(),
    };
    SseEvent::default()
        .id(cursor)
        .event("snapshot")
        .json_data(snap)
        .unwrap_or_default()
}

fn to_sse(e: &Sequenced) -> SseEvent {
    SseEvent::default()
        .id(e.id.clone())
        .event(e.event.ty)
        .json_data(&e.event)
        .unwrap_or_default()
}

pub async fn events(
    State(node): State<AppState>,
    Query(q): Query<EventsQuery>,
    headers: HeaderMap,
) -> impl axum::response::IntoResponse {
    let last_id = headers.get("last-event-id").and_then(|v| v.to_str().ok());
    let (resume, mut rx) = node.bus.subscribe(last_id);
    let (tx, out) = mpsc::channel::<Result<SseEvent, Infallible>>(256);
    let want_snapshot = q.snapshot == Some(1);

    tokio::spawn(async move {
        let send = |e: SseEvent| {
            let tx = tx.clone();
            async move { tx.send(Ok(e)).await.is_ok() }
        };
        if !send(SseEvent::default().retry(Duration::from_secs(3))).await {
            return;
        }
        match resume {
            Resume::Replay(items) => {
                for e in &items {
                    if !send(to_sse(e)).await {
                        return;
                    }
                }
            }
            Resume::Reset => {
                if !send(SseEvent::default().event("reset").data("{}")).await
                    || !send(snapshot(&node)).await
                {
                    return;
                }
            }
            Resume::Fresh if want_snapshot => {
                if !send(snapshot(&node)).await {
                    return;
                }
            }
            Resume::Fresh => {}
        }
        loop {
            let ev = tokio::select! {
                ev = rx.recv() => ev,
                () = tx.closed() => return,
            };
            let msg = match ev {
                Ok(e) => to_sse(&e),
                // Too slow: start over from a fresh snapshot.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if !send(SseEvent::default().event("reset").data("{}")).await {
                        return;
                    }
                    snapshot(&node)
                }
                Err(broadcast::error::RecvError::Closed) => return,
            };
            if !send(msg).await {
                return;
            }
        }
    });

    Sse::new(ReceiverStream::new(out)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}
