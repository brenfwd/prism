//! `/api/v1/archive`: pools, members and files across the cluster.
//!
//! This file has the route table, pool listing and creation, file listing and
//! the helper that forwards a request to another node. Uploads, downloads and
//! deletes are in the submodules.

mod delete;
mod download;
mod upload;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{ApiError, AppState, Json};
use crate::archive::{ArchiveMemberDoc, ArchivePoolDoc, MemberCapacity};
use crate::node::{Node, NodeError};
use crate::state::Command;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/archive/pools", get(list_pools).post(create_pool))
        .route("/archive/pools/{pool}/files", get(list_files))
        .route(
            "/archive/pools/{pool}/files/{*path}",
            get(download::download)
                .put(upload::upload)
                .layer(DefaultBodyLimit::disable())
                .delete(delete::delete_file),
        )
}

#[derive(Debug, Serialize)]
struct MemberView {
    #[serde(flatten)]
    doc: ArchiveMemberDoc,
    node_name: String,
    capacity: Option<MemberCapacity>,
}

#[derive(Debug, Serialize)]
struct PoolView {
    #[serde(flatten)]
    doc: ArchivePoolDoc,
    members: Vec<MemberView>,
    files: usize,
    used_bytes: u64,
}

/// Capacity of every member, from the node views this node knows.
pub(super) fn capacities(node: &Node) -> Vec<(Uuid, String, MemberCapacity)> {
    node.nodes()
        .into_iter()
        .flat_map(|n| {
            let (id, name) = (n.id, n.name.clone());
            n.archive.into_iter().map(move |c| (id, name.clone(), c))
        })
        .collect()
}

async fn list_pools(State(node): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let caps = capacities(&node);
    let members = node.state.archive_members()?;
    let mut out = Vec::new();
    for pool in node.state.archive_pools()? {
        let files = node.state.archive_files(&pool.name, "")?;
        out.push(PoolView {
            members: members
                .iter()
                .filter(|m| m.pool == pool.name)
                .map(|m| {
                    let c = caps
                        .iter()
                        .find(|(n, _, c)| *n == m.node && c.name == m.name);
                    MemberView {
                        doc: m.clone(),
                        node_name: c.map(|(_, n, _)| n.clone()).unwrap_or_default(),
                        capacity: c.map(|(_, _, c)| c.clone()),
                    }
                })
                .collect(),
            files: files.len(),
            used_bytes: files.iter().map(|f| f.size).sum(),
            doc: pool,
        });
    }
    Ok(axum::Json(out))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreatePool {
    name: String,
    #[serde(default = "one")]
    replicas: u8,
}

fn one() -> u8 {
    1
}

async fn create_pool(
    State(node): State<AppState>,
    Json(req): Json<CreatePool>,
) -> Result<impl IntoResponse, ApiError> {
    if !crate::storage::valid_name(&req.name) {
        return Err(ApiError::bad_request("invalid pool name"));
    }
    if !(1..=3).contains(&req.replicas) {
        return Err(ApiError::bad_request("replicas must be 1-3"));
    }
    if node
        .state
        .archive_pools()?
        .iter()
        .any(|p| p.name == req.name)
    {
        return Err(
            NodeError::Conflict(format!("archive pool {} already exists", req.name)).into(),
        );
    }
    let doc = ArchivePoolDoc {
        name: req.name,
        replicas: req.replicas,
    };
    node.state
        .apply(Command::PutArchivePool { pool: doc.clone() })
        .await
        .map_err(NodeError::from)?;
    Ok((StatusCode::CREATED, axum::Json(doc)))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default)]
    prefix: String,
}

async fn list_files(
    State(node): State<AppState>,
    Path(pool): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let mut files = node.state.archive_files(&pool, &q.prefix)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(axum::Json(files))
}

/// Query of requests that act on one node's replica only.
#[derive(Debug, Deserialize)]
struct LocalQuery {
    /// Set on requests forwarded by a peer: do not look further.
    local: Option<u8>,
    /// With `local`: delete only this member's copy.
    member: Option<String>,
}

/// Sends `req` to the node `target` over the mesh. With `member` the peer
/// stores the body on that member directory; without it the peer is asked to
/// act on its own replica only (`local=1`). Peer failures become 502.
async fn forward_to(
    node: &Node,
    target: Uuid,
    req: Request,
    member: Option<&str>,
) -> Result<Response, ApiError> {
    let cluster = node.cluster.get().ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "not_clustered",
            "that member is on another node",
        )
    })?;
    let m = node
        .state
        .members()?
        .into_iter()
        .find(|m| m.node_id == target)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "owner_unknown",
                "the member's node is not in the cluster",
            )
        })?;
    let (mut parts, body) = req.into_parts();
    let path = parts
        .extensions
        .get::<axum::extract::OriginalUri>()
        .map_or_else(|| parts.uri.path().to_string(), |o| o.0.path().to_string());
    let uri = match member {
        Some(mem) => format!("{path}?member={mem}"),
        None => format!("{path}?local=1"),
    };
    parts.uri = uri.parse().map_err(|_| ApiError::bad_request("bad path"))?;
    crate::cluster::peers::forward_http(cluster, &m, Request::from_parts(parts, body))
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "replica_unavailable",
                format!("{}: {e:#}", m.name),
            )
        })
}
