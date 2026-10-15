//! Serves any request on any node: requests for another node's VM, pool or
//! task are tunnelled to that node over the mesh (one hop at most).

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use super::{ApiError, AppState};
use crate::cluster::peers::{Tunnelled, forward_http};
use crate::node::Node;

/// The node that owns the resource a request path names, if not this one.
fn owner(node: &Node, path: &str, query: Option<&str>) -> Option<Uuid> {
    let rest = path.strip_prefix("/api/v1/")?;
    let mut parts = rest.split('/');
    let owner = match (parts.next(), parts.next()) {
        (Some("vms"), Some(id)) if !id.is_empty() => {
            let id = Uuid::parse_str(id).ok()?;
            node.state.vm(id).ok()??.node
        }
        // Creating a VM on another node: POST /vms?node=<id>.
        (Some("vms"), None | Some("")) => query?
            .split('&')
            .find_map(|kv| kv.strip_prefix("node="))
            .and_then(|v| Uuid::parse_str(v).ok())?,
        (Some("nodes"), Some(id)) if id != "local" && parts.next().is_some() => {
            Uuid::parse_str(id).ok()?
        }
        _ => return None,
    };
    (owner != node.id).then_some(owner)
}

/// Marks every response while the cluster is degraded (config read-only).
pub async fn degraded(State(node): State<AppState>, req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    if node.cluster.get().is_some_and(|c| c.is_degraded()) {
        res.headers_mut().insert(
            "x-prism-degraded",
            axum::http::HeaderValue::from_static("true"),
        );
    }
    res
}

pub async fn route(State(node): State<AppState>, req: Request, next: Next) -> Response {
    // Already forwarded once, standalone, or a WebSocket (consoles tunnel
    // themselves): handle here.
    let is_ws = req.headers().contains_key(axum::http::header::UPGRADE);
    if req.extensions().get::<Tunnelled>().is_some() || !node.state.clustered() || is_ws {
        return next.run(req).await;
    }
    let Some(owner) = owner(&node, req.uri().path(), req.uri().query()) else {
        return next.run(req).await;
    };
    let Some(cluster) = node.cluster.get() else {
        return next.run(req).await;
    };
    let member = match node.state.members() {
        Ok(m) => m.into_iter().find(|m| m.node_id == owner),
        Err(e) => return ApiError::from(e).into_response(),
    };
    let Some(member) = member else {
        return ApiError::new(
            StatusCode::NOT_FOUND,
            "no_such_node",
            format!("node {owner} is not a member"),
        )
        .into_response();
    };
    match forward_http(cluster, &member, req).await {
        Ok(r) => r,
        Err(e) => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "owner_unreachable",
            format!(
                "{} owns this resource and is unreachable: {e:#}",
                member.name
            ),
        )
        .into_response(),
    }
}
