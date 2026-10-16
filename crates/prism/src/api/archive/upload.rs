//! `PUT /archive/pools/{pool}/files/{path}`: streaming upload with placement.

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;

use super::{ApiError, AppState, capacities, forward_to};
use crate::archive::{ArchiveFileDoc, ArchiveMemberDoc, FileKind, Replica, place, valid_path};
use crate::node::NodeError;
use crate::state::Command;

#[derive(Debug, Deserialize)]
pub(super) struct UploadQuery {
    /// Set when a peer forwards an upload to the chosen member's node.
    member: Option<String>,
    /// With `member`: this is a replica copy of an existing file.
    replica: Option<u8>,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Streams a file into the pool on a member chosen by placement (on this
/// node or forwarded to the member's node).
pub(super) async fn upload(
    State(node): State<AppState>,
    Path((pool, path)): Path<(String, String)>,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    req: Request,
) -> Result<impl IntoResponse, ApiError> {
    if !valid_path(&path) {
        return Err(ApiError::bad_request(
            "paths are /-separated names of [A-Za-z0-9._-]",
        ));
    }
    if q.replica.is_some()
        && let Some(m) = &q.member
    {
        node.archive_receive_replica(&pool, &path, m, req.into_body())
            .await
            .map_err(NodeError::Internal)?;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    if node.state.archive_file(&pool, &path)?.is_some() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "exists",
            format!("{path} already exists"),
        ));
    }
    let size_hint = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let member_name = if let Some(m) = q.member {
        m
    } else {
        {
            let members = node.state.archive_members()?;
            let caps = capacities(&node);
            let cands: Vec<(&ArchiveMemberDoc, u64)> = members
                .iter()
                .filter(|m| m.pool == pool)
                .filter_map(|m| {
                    caps.iter()
                        .find(|(n, _, c)| *n == m.node && c.name == m.name)
                        .map(|(_, _, c)| (m, c.free))
                })
                .collect();
            if cands.is_empty() {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "no_members",
                    format!(
                        "archive pool {pool} has no reachable member directories; add [archive_members] to a node's prism.toml"
                    ),
                ));
            }
            let chosen = place(&cands, size_hint, Uuid::new_v4().as_u128()).ok_or_else(|| {
                ApiError::new(
                    StatusCode::INSUFFICIENT_STORAGE,
                    "pool_full",
                    format!("no member of {pool} has room"),
                )
            })?;
            if chosen.node != node.id {
                return forward_to(&node, chosen.node, req, Some(&chosen.name)).await;
            }
            chosen.name.clone()
        }
    };
    let member = node
        .archive
        .member(&member_name)
        .filter(|m| m.pool == pool)
        .ok_or_else(|| {
            ApiError::not_found(format!("member {member_name} of {pool} on this node"))
        })?;
    let (size, blake3) = member
        .write(&path, req.into_body().into_data_stream())
        .await?;
    let file = ArchiveFileDoc {
        pool: pool.clone(),
        path: path.clone(),
        size,
        blake3,
        kind: FileKind::guess(&path),
        created: unix_now(),
        replicas: vec![Replica {
            node: node.id,
            member: member.name.clone(),
        }],
    };
    if let Err(e) = node
        .state
        .apply(Command::PutArchiveFile {
            file: file.clone(),
            create: true,
        })
        .await
    {
        member.delete_best_effort(&path);
        return Err(NodeError::from(e).into());
    }
    if let Ok(Some(p)) = node
        .state
        .archive_pools()
        .map(|p| p.into_iter().find(|p| p.name == pool && p.replicas > 1))
    {
        let node = std::sync::Arc::clone(&node);
        tokio::spawn(async move {
            if let Err(e) = node.archive_repair_pool(&p.name).await {
                tracing::warn!("filling replicas of {}: {e:#}", p.name);
            }
        });
    }
    Ok((StatusCode::CREATED, axum::Json(file)).into_response())
}
