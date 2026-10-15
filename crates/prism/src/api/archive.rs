//! `/api/v1/archive`: pools, members and files across the cluster.

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use uuid::Uuid;

use super::{ApiError, AppState, Json};
use crate::archive::{
    ArchiveFileDoc, ArchiveMemberDoc, ArchivePoolDoc, FileKind, MemberCapacity, Replica, place,
    valid_path,
};
use crate::node::{Node, NodeError};
use crate::state::Command;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/archive/pools", get(list_pools).post(create_pool))
        .route("/archive/pools/{pool}/files", get(list_files))
        .route(
            "/archive/pools/{pool}/files/{*path}",
            get(download)
                .put(upload)
                .layer(DefaultBodyLimit::disable())
                .delete(delete_file),
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
fn capacities(node: &Node) -> Vec<(Uuid, String, MemberCapacity)> {
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

#[derive(Debug, Deserialize)]
struct UploadQuery {
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
async fn upload(
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
        let _ = member.delete(&path);
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

/// Parses `bytes=a-b`, `bytes=a-` or `bytes=-n` against `size`.
fn parse_range(v: &str, size: u64) -> Option<(u64, u64)> {
    let spec = v.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (size.saturating_sub(n), size.checked_sub(1)?)
        }
        (a, "") => (a.parse().ok()?, size.checked_sub(1)?),
        (a, b) => (
            a.parse().ok()?,
            b.parse::<u64>().ok()?.min(size.checked_sub(1)?),
        ),
    };
    (start <= end && end < size).then_some((start, end))
}

#[derive(Debug, Deserialize)]
struct LocalQuery {
    local: Option<u8>,
    /// With `local`: delete only this member's copy.
    member: Option<String>,
}

/// Serves a file (with `Range`) from a replica on this node, or proxies it
/// from one elsewhere.
async fn download(
    State(node): State<AppState>,
    Path((pool, path)): Path<(String, String)>,
    Query(q): Query<LocalQuery>,
    headers: HeaderMap,
    req: Request,
) -> Result<Response, ApiError> {
    let file = node
        .state
        .archive_file(&pool, &path)?
        .ok_or_else(|| ApiError::not_found(format!("{pool}/{path}")))?;
    let local = file
        .replicas
        .iter()
        .find(|r| r.node == node.id)
        .and_then(|r| node.archive.member(&r.member));
    let Some(member) = local else {
        if q.local.is_some() {
            return Err(ApiError::not_found("no replica on this node"));
        }
        let holder = file.replicas.first().ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "replica_unavailable",
                "no replicas",
            )
        })?;
        return forward_to(&node, holder.node, req, None).await;
    };
    let mut f = tokio::fs::File::from_std(member.open_read(&path)?);
    let size = file.size;
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|v| parse_range(v, size));
    let name = path.rsplit('/').next().unwrap_or("file").to_string();
    let mut res = match range {
        Some(None) => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{size}"))],
            )
                .into_response());
        }
        Some(Some((start, end))) => {
            f.seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(anyhow::Error::from)?;
            let len = end - start + 1;
            let body = Body::from_stream(tokio_util::io::ReaderStream::new(f.take(len)));
            let mut r = (StatusCode::PARTIAL_CONTENT, body).into_response();
            r.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {start}-{end}/{size}"))
                    .map_err(anyhow::Error::from)?,
            );
            r.headers_mut()
                .insert(header::CONTENT_LENGTH, HeaderValue::from(len));
            r
        }
        None => {
            let mut r = Body::from_stream(tokio_util::io::ReaderStream::new(f)).into_response();
            r.headers_mut()
                .insert(header::CONTENT_LENGTH, HeaderValue::from(size));
            r
        }
    };
    let h = res.headers_mut();
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Ok(v) = HeaderValue::from_str(&format!("\"{}\"", file.blake3)) {
        h.insert(header::ETAG, v);
    }
    Ok(res)
}

async fn delete_file(
    State(node): State<AppState>,
    Path((pool, path)): Path<(String, String)>,
    Query(q): Query<LocalQuery>,
) -> Result<StatusCode, ApiError> {
    if q.local.is_some()
        && let Some(name) = &q.member
    {
        if let Some(m) = node.archive.member(name).filter(|m| m.pool == pool) {
            let _ = m.delete(&path);
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
                let _ = m.delete(&path);
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
                let _ = m.delete(&path);
            }
        } else {
            let req = axum::http::Request::builder()
                .method("DELETE")
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

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=900-", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=990-2000", 1000), Some((990, 999)));
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("bytes=5-1", 1000), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 1000), None);
    }
}
