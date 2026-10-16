//! `DELETE /archive/pools/{pool}/files/{path}`: removes a file everywhere.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{Method, StatusCode};

use super::{ApiError, AppState, LocalQuery, forward_to};
use crate::archive::LocalMember;
use crate::node::NodeError;
use crate::state::Command;

/// Deletes one replica file on this node without blocking the runtime.
/// A failure is logged by the member; the file record is already gone or
/// being removed, so an orphan is the worst outcome and a later scrub finds it.
async fn delete_local(member: Arc<LocalMember>, path: String) {
    if let Err(e) = tokio::task::spawn_blocking(move || member.delete_best_effort(&path)).await {
        tracing::warn!("archive delete task failed: {e}");
    }
}

/// Deletes the file record and then every replica. With `local` (set by a
/// peer) it removes only this node's copy.
pub(super) async fn delete_file(
    State(node): State<AppState>,
    Path((pool, path)): Path<(String, String)>,
    Query(q): Query<LocalQuery>,
) -> Result<StatusCode, ApiError> {
    if q.local.is_some()
        && let Some(name) = &q.member
    {
        if let Some(m) = node.archive.member(name).filter(|m| m.pool == pool) {
            delete_local(m, path.clone()).await;
        }
        return Ok(StatusCode::NO_CONTENT);
    }
    let file = node
        .state
        .archive_file(&pool, &path)?
        .ok_or_else(|| ApiError::not_found(format!("{pool}/{path}")))?;
    if q.local.is_some() {
        for r in file.replicas.iter().filter(|r| r.node == node.id) {
            if let Some(m) = node.archive.member(&r.member) {
                delete_local(m, path.clone()).await;
            }
        }
        return Ok(StatusCode::NO_CONTENT);
    }
    node.state
        .apply(Command::DeleteArchiveFile {
            pool: pool.clone(),
            path: path.clone(),
        })
        .await
        .map_err(NodeError::from)?;
    for r in &file.replicas {
        if r.node == node.id {
            if let Some(m) = node.archive.member(&r.member) {
                delete_local(m, path.clone()).await;
            }
        } else {
            let req = axum::http::Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/v1/archive/pools/{pool}/files/{path}"))
                .body(Body::empty())
                .map_err(anyhow::Error::from)?;
            if let Err(e) = forward_to(&node, r.node, req, None).await {
                tracing::warn!(
                    "removing replica of {pool}/{path} on {}: {}",
                    r.node,
                    e.detail()
                );
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}
