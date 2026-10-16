//! WebSocket consoles: raw RFB to QEMU's VNC socket, and the serial console.
//! Consoles of VMs on other nodes are tunnelled to their owner.

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use uuid::Uuid;

use super::{ApiError, AppState};
use crate::cluster::peers::open_remote_console;
use crate::node::Node;

/// The routes of this module, mounted under `/api/v1`.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/vms/{id}/console", get(vnc))
        .route("/vms/{id}/serial", get(serial))
}

fn parse_id(id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id).map_err(|_| ApiError::not_found(format!("vm {id}")))
}

/// The owner's member document when the VM runs on another node.
fn remote_owner(
    node: &Node,
    id: Uuid,
) -> Result<Option<crate::cluster::model::MemberDoc>, ApiError> {
    let spec = node.spec(id)?;
    if spec.node == node.id {
        return Ok(None);
    }
    let member = node
        .state
        .members()?
        .into_iter()
        .find(|m| m.node_id == spec.node)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "owner_unknown",
                "the VM's node is not a member",
            )
        })?;
    Ok(Some(member))
}

async fn tunnel(
    node: AppState,
    member: crate::cluster::model::MemberDoc,
    id: Uuid,
    kind: &'static str,
    ws: WebSocket,
) {
    let Some(cluster) = node.cluster.get() else {
        return;
    };
    let (tx, mut inbound) = match open_remote_console(cluster, &member, id, kind).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(vm = %id, "console tunnel to {}: {e:#}", member.name);
            return;
        }
    };
    let (mut ws_tx, mut ws_rx) = ws.split();
    let down = async {
        while let Ok(Some(f)) = inbound.message().await {
            if ws_tx.send(Message::Binary(f.data.into())).await.is_err() {
                break;
            }
        }
        // Teardown: the browser may already be gone.
        let _ = ws_tx.close().await;
    };
    let up = async {
        while let Some(Ok(msg)) = ws_rx.next().await {
            let data = match msg {
                Message::Binary(b) => b.to_vec(),
                Message::Text(t) => t.as_bytes().to_vec(),
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => continue,
            };
            if tx.send(data).await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        () = down => {}
        () = up => {}
    }
}

async fn vnc(
    State(node): State<AppState>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let id = parse_id(&id)?;
    if let Some(member) = remote_owner(&node, id)? {
        return Ok(ws
            .protocols(["binary"])
            .on_upgrade(move |ws| tunnel(node, member, id, "vnc", ws)));
    }
    let path = node.vnc_socket(id).await?;
    let sock = UnixStream::connect(&path).await.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "console_unavailable",
            e.to_string(),
        )
    })?;
    Ok(ws
        .protocols(["binary"])
        .on_upgrade(move |ws| pump_unix(ws, sock)))
}

async fn pump_unix(ws: WebSocket, sock: UnixStream) {
    let (mut ws_tx, mut ws_rx) = ws.split();
    let (mut rd, mut wr) = sock.into_split();
    let down = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match rd.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if ws_tx
                        .send(Message::Binary(buf[..n].to_vec().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
        // Teardown: the browser may already be gone.
        let _ = ws_tx.close().await;
    };
    let up = async {
        while let Some(Ok(msg)) = ws_rx.next().await {
            let data = match msg {
                Message::Binary(b) => b,
                Message::Text(t) => t.as_bytes().to_vec().into(),
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => continue,
            };
            if wr.write_all(&data).await.is_err() {
                break;
            }
        }
        // Teardown: the guest side may already be gone.
        let _ = wr.shutdown().await;
    };
    tokio::select! {
        () = down => {}
        () = up => {}
    }
}

async fn serial(
    State(node): State<AppState>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let id = parse_id(&id)?;
    if let Some(member) = remote_owner(&node, id)? {
        return Ok(ws.on_upgrade(move |ws| tunnel(node, member, id, "serial", ws)));
    }
    let stream = node.open_serial(id).await?;
    Ok(ws.on_upgrade(move |ws| async move {
        let (mut reader, writer) = stream.into_split();
        let (mut ws_tx, mut ws_rx) = ws.split();
        let down = async {
            while let Some(Ok(data)) = reader.recv().await {
                if ws_tx.send(Message::Binary(data.into())).await.is_err() {
                    break;
                }
            }
            // Teardown: the browser may already be gone.
            let _ = ws_tx.close().await;
        };
        let up = async {
            while let Some(Ok(msg)) = ws_rx.next().await {
                let res = match msg {
                    Message::Binary(b) => writer.send(&b),
                    Message::Text(t) => writer.send(t.as_bytes()),
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => Ok(()),
                };
                if res.is_err() {
                    break;
                }
            }
        };
        tokio::select! {
            () = down => {}
            () = up => {}
        }
    }))
}
