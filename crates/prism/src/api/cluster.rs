//! `/api/v1/cluster`: status, create, invite, accept, join, remove.

use std::time::Duration;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use serde::Deserialize;
use uuid::Uuid;

use super::{ApiError, AppState, Json};
use crate::cluster::model::ClusterMode;
use crate::cluster::runtime::{ClusterOpError, DEFAULT_INVITE_TTL};
use crate::node::NodeError;
use crate::state::Command;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/cluster", get(status).post(create))
        .route("/cluster/invites", get(invites).post(invite))
        .route("/cluster/invites/{id}", delete(revoke))
        .route("/cluster/accept", post(accept))
        .route("/cluster/join", post(join))
        .route("/cluster/leave", post(leave))
        .route("/cluster/removed", get(removed))
        .route("/cluster/members/{node}", delete(remove))
        .route("/cluster/members/{node}/voter", post(set_voter))
        .route("/cluster/hub", post(transfer_hub))
        .route("/cluster/mesh", get(mesh))
}

impl From<ClusterOpError> for ApiError {
    fn from(e: ClusterOpError) -> Self {
        match e {
            ClusterOpError::Invalid(m) => Self::bad_request(m),
            ClusterOpError::Conflict(m) => Self::new(StatusCode::CONFLICT, "conflict", m),
            ClusterOpError::State(s) => NodeError::from(s).into(),
            ClusterOpError::Internal(e) => NodeError::Internal(e).into(),
        }
    }
}

fn cluster(node: &crate::node::Node) -> Result<&std::sync::Arc<crate::cluster::Cluster>, ApiError> {
    node.cluster.get().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "cluster_unavailable",
            "clustering is not available on this node",
        )
    })
}

async fn status(State(node): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    Ok(axum::Json(cluster(&node)?.status()?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateCluster {
    name: String,
    #[serde(default = "default_mode")]
    mode: ClusterMode,
}

fn default_mode() -> ClusterMode {
    ClusterMode::HubSpoke
}

async fn create(
    State(node): State<AppState>,
    Json(req): Json<CreateCluster>,
) -> Result<impl IntoResponse, ApiError> {
    if req.name.trim().is_empty() || req.name.len() > 64 {
        return Err(ApiError::bad_request(
            "cluster name must be 1-64 characters",
        ));
    }
    let s = cluster(&node)?.init(req.name.trim(), req.mode).await?;
    Ok((StatusCode::CREATED, axum::Json(s)))
}

async fn invites(State(node): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    Ok(axum::Json(node.state.invites()?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInvite {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    ttl_secs: Option<u64>,
}

async fn invite(
    State(node): State<AppState>,
    Json(req): Json<CreateInvite>,
) -> Result<impl IntoResponse, ApiError> {
    let ttl = req.ttl_secs.map_or(DEFAULT_INVITE_TTL, Duration::from_secs);
    if ttl > Duration::from_secs(7 * 86_400) {
        return Err(ApiError::bad_request("invites last at most 7 days"));
    }
    let v = cluster(&node)?.invite(req.name, ttl).await?;
    Ok((StatusCode::CREATED, axum::Json(v)))
}

async fn revoke(
    State(node): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    node.state
        .apply(Command::RevokeInvite { invite_id: id })
        .await
        .map_err(NodeError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Accept {
    reply: String,
    /// Without `confirm`, returns the preview (name, endpoints, key
    /// fingerprint) to show the operator first.
    #[serde(default)]
    confirm: bool,
}

async fn accept(
    State(node): State<AppState>,
    Json(req): Json<Accept>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(axum::Json(
        cluster(&node)?.accept(&req.reply, req.confirm).await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Join {
    invite: String,
}

async fn join(
    State(node): State<AppState>,
    Json(req): Json<Join>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(axum::Json(cluster(&node)?.join(&req.invite).await?))
}

async fn remove(
    State(node): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let node_id = Uuid::parse_str(&id).map_err(|_| ApiError::not_found(format!("member {id}")))?;
    if node_id == node.id {
        return Err(ApiError::bad_request(
            "remove this node from another member",
        ));
    }
    cluster(&node)?
        .check_removal(node_id)
        .map_err(ApiError::bad_request)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    node.state
        .apply(Command::RemoveMember { node_id, now })
        .await
        .map_err(NodeError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Removes this node from the cluster, then stops the daemon so the CLI can
/// wipe its state.
async fn leave(State(node): State<AppState>) -> Result<StatusCode, ApiError> {
    let c = cluster(&node)?;
    let Some(doc) = c.state.cluster()? else {
        return Err(ApiError::bad_request("this node is not in a cluster"));
    };
    let others = c
        .state
        .members()?
        .iter()
        .filter(|m| m.node_id != node.id)
        .count();
    let is_hub = doc.mode == ClusterMode::HubSpoke
        && c.state
            .members()?
            .iter()
            .any(|m| m.node_id == node.id && m.voter);
    if others > 0 && is_hub {
        return Err(ApiError::bad_request(
            "this node is the hub; hand the hub role to another member first (`prism cluster promote <node>`), or run `prism leave --force`",
        ));
    }
    if others > 0 {
        c.check_removal(node.id).map_err(ApiError::bad_request)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        node.state
            .apply(Command::RemoveMember {
                node_id: node.id,
                now,
            })
            .await
            .map_err(NodeError::from)?;
    }
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        std::process::exit(0);
    });
    Ok(StatusCode::NO_CONTENT)
}

/// Tombstones: removed members whose identity is blocked for good.
async fn removed(State(node): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    Ok(axum::Json(
        cluster(&node)?
            .removal_status()
            .await
            .map_err(|e| ApiError::bad_request(format!("{e:#}")))?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Voter {
    voter: bool,
}

async fn set_voter(
    State(node): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<Voter>,
) -> Result<StatusCode, ApiError> {
    let node_id = Uuid::parse_str(&id).map_err(|_| ApiError::not_found(format!("member {id}")))?;
    let c = cluster(&node)?;
    let Some(doc) = c.state.cluster()? else {
        return Err(ApiError::bad_request("not in a cluster"));
    };
    if doc.mode != ClusterMode::Consensus {
        return Err(ApiError::bad_request(
            "voters are chosen only in consensus mode; hub-spoke has one voter, the hub",
        ));
    }
    let voters = c
        .state
        .members()?
        .iter()
        .filter(|m| m.voter && m.node_id != node_id)
        .count()
        + usize::from(req.voter);
    if !matches!(voters, 1 | 3 | 5) {
        return Err(ApiError::bad_request(format!(
            "that would leave {voters} voters; use 3 or 5 (or 1 while setting up)"
        )));
    }
    node.state
        .apply(Command::SetVoter {
            node_id,
            voter: req.voter,
        })
        .await
        .map_err(NodeError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferHub {
    /// The member that becomes the hub.
    node: Uuid,
}

/// Hands the hub role (the single raft voter) from this node to another.
async fn transfer_hub(
    State(node): State<AppState>,
    Json(req): Json<TransferHub>,
) -> Result<StatusCode, ApiError> {
    cluster(&node)?.transfer_hub(req.node).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn mesh(State(node): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    Ok(axum::Json(cluster(&node)?.mesh.status().await?))
}
