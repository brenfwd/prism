//! `/api/v1/archive` maintenance: replica count, repair, drain and scrub.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{post, put};
use serde::Deserialize;
use uuid::Uuid;

use super::{ApiError, AppState, Json};
use crate::archive::{ArchivePoolDoc, MemberState};
use crate::node::NodeError;
use crate::state::Command;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/archive/pools/{pool}", put(set_replicas))
        .route("/archive/pools/{pool}/repair", post(repair))
        .route("/archive/pools/{pool}/scrub", post(scrub))
        .route("/archive/pools/{pool}/rebalance", post(rebalance))
        .route("/archive/members/{node}/{name}", put(set_state))
        .route("/archive/members/{node}/{name}/drain", post(drain))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Replicas {
    replicas: u8,
}

/// Changes a pool's copy count; extra copies fill in the background.
async fn set_replicas(
    State(node): State<AppState>,
    Path(pool): Path<String>,
    Json(req): Json<Replicas>,
) -> Result<impl IntoResponse, ApiError> {
    if !(1..=3).contains(&req.replicas) {
        return Err(ApiError::bad_request("replicas must be 1-3"));
    }
    if !node.state.archive_pools()?.iter().any(|p| p.name == pool) {
        return Err(ApiError::not_found(format!("archive pool {pool}")));
    }
    let doc = ArchivePoolDoc {
        name: pool.clone(),
        replicas: req.replicas,
    };
    node.state
        .apply(Command::PutArchivePool { pool: doc.clone() })
        .await
        .map_err(NodeError::from)?;
    let task = node
        .tasks
        .spawn("archive.repair", format!("/archive/pools/{pool}"), {
            let node = std::sync::Arc::clone(&node);
            move |_| async move {
                Ok(serde_json::to_value(
                    node.archive_repair_pool(&pool).await?,
                )?)
            }
        });
    Ok((StatusCode::ACCEPTED, axum::Json(task)))
}

async fn repair(
    State(node): State<AppState>,
    Path(pool): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let resource = format!("/archive/pools/{pool}");
    if node.tasks.busy(&resource) {
        return Err(
            NodeError::Conflict("another archive task is running on this pool".into()).into(),
        );
    }
    let n = std::sync::Arc::clone(&node);
    let task = node
        .tasks
        .spawn("archive.repair", resource, move |_| async move {
            Ok(serde_json::to_value(n.archive_repair_pool(&pool).await?)?)
        });
    Ok((StatusCode::ACCEPTED, axum::Json(task)))
}

#[derive(Debug, Deserialize)]
struct ScrubQuery {
    /// Scrub only this node's replicas and answer with the report (used by
    /// peers during a cluster-wide scrub).
    local: Option<u8>,
}

/// Re-hashes every node's replicas of the pool.
async fn scrub(
    State(node): State<AppState>,
    Path(pool): Path<String>,
    Query(q): Query<ScrubQuery>,
) -> Result<axum::response::Response, ApiError> {
    if q.local.is_some() {
        let report = node
            .archive_scrub_local(&pool)
            .await
            .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
        return Ok(axum::Json(report).into_response());
    }
    let resource = format!("/archive/pools/{pool}");
    if node.tasks.busy(&resource) {
        return Err(
            NodeError::Conflict("another archive task is running on this pool".into()).into(),
        );
    }
    let n = std::sync::Arc::clone(&node);
    let task = node
        .tasks
        .spawn("archive.scrub", resource, move |_| async move {
            Ok(serde_json::to_value(n.archive_scrub(&pool).await?)?)
        });
    Ok((StatusCode::ACCEPTED, axum::Json(task)).into_response())
}

/// Moves files from fuller members toward lighter ones.
async fn rebalance(
    State(node): State<AppState>,
    Path(pool): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let resource = format!("/archive/pools/{pool}");
    if node.tasks.busy(&resource) {
        return Err(
            NodeError::Conflict("another archive task is running on this pool".into()).into(),
        );
    }
    let n = std::sync::Arc::clone(&node);
    let task = node
        .tasks
        .spawn("archive.rebalance", resource, move |_| async move {
            Ok(serde_json::to_value(
                n.archive_rebalance_pool(&pool).await?,
            )?)
        });
    Ok((StatusCode::ACCEPTED, axum::Json(task)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetState {
    state: MemberState,
}

async fn set_state(
    State(node): State<AppState>,
    Path((member_node, name)): Path<(Uuid, String)>,
    Json(req): Json<SetState>,
) -> Result<StatusCode, ApiError> {
    node.archive_set_member_state(member_node, &name, req.state)
        .await
        .map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn drain(
    State(node): State<AppState>,
    Path((member_node, name)): Path<(Uuid, String)>,
) -> Result<impl IntoResponse, ApiError> {
    if !node
        .state
        .archive_members()?
        .iter()
        .any(|m| m.node == member_node && m.name == name)
    {
        return Err(ApiError::not_found(format!("archive member {name}")));
    }
    let resource = format!("/archive/members/{member_node}/{name}");
    if node.tasks.busy(&resource) {
        return Err(NodeError::Conflict("this member is already draining".into()).into());
    }
    let n = std::sync::Arc::clone(&node);
    let task = node
        .tasks
        .spawn("archive.drain", resource, move |ctx| async move {
            Ok(serde_json::to_value(
                n.archive_drain(member_node, &name, &ctx).await?,
            )?)
        });
    Ok((StatusCode::ACCEPTED, axum::Json(task)))
}
