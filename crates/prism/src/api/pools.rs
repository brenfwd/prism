use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, put};
use serde::Deserialize;

use super::{ApiError, AppState, Json, check_node};
use crate::db::PoolRow;
use crate::events::Op;
use crate::node::Node;
use crate::storage::{Pool, PoolInfo, Volume};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/nodes/{node}/pools", get(list).post(create))
        .route("/nodes/{node}/pools/{pool}", get(show).delete(remove))
        .route(
            "/nodes/{node}/pools/{pool}/volumes",
            get(volumes).post(create_volume),
        )
        .route(
            "/nodes/{node}/pools/{pool}/volumes/{volume}",
            put(upload)
                .layer(DefaultBodyLimit::disable())
                .get(volume)
                .delete(delete_volume),
        )
}

fn publish_pool(node: &Node, pool: &str, op: Op) {
    let info = node.storage.get(pool).ok().map(|p| p.info());
    node.bus.publish(
        "pool",
        format!("/nodes/{}/pools/{pool}", node.id),
        op,
        serde_json::to_value(info).unwrap_or_default(),
    );
}

async fn list(
    State(node): State<AppState>,
    Path(n): Path<String>,
) -> Result<axum::Json<Vec<PoolInfo>>, ApiError> {
    check_node(&node, &n)?;
    Ok(axum::Json(
        node.storage.list().iter().map(|p| p.info()).collect(),
    ))
}

async fn show(
    State(node): State<AppState>,
    Path((n, pool)): Path<(String, String)>,
) -> Result<axum::Json<PoolInfo>, ApiError> {
    check_node(&node, &n)?;
    Ok(axum::Json(node.storage.get(&pool)?.info()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreatePool {
    name: String,
    /// One of `storage.pool_roots`; defaults to the first.
    #[serde(default)]
    root: Option<PathBuf>,
    #[serde(default = "default_format")]
    format: String,
}

fn default_format() -> String {
    "qcow2".into()
}

async fn create(
    State(node): State<AppState>,
    Path(n): Path<String>,
    Json(req): Json<CreatePool>,
) -> Result<impl IntoResponse, ApiError> {
    check_node(&node, &n)?;
    if node.storage.get(&req.name).is_ok() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "exists",
            format!("pool {} exists", req.name),
        ));
    }
    if !matches!(req.format.as_str(), "qcow2" | "raw") {
        return Err(ApiError::bad_request("format must be qcow2 or raw"));
    }
    let path = node.storage.new_pool_path(&req.name, req.root.as_deref())?;
    let pool = Pool::open(&req.name, &path, &req.format, false)?;
    node.db.insert_pool(&PoolRow {
        name: req.name.clone(),
        path: path.to_string_lossy().into_owned(),
        format: req.format,
    })?;
    let info = pool.info();
    node.storage.insert(pool);
    publish_pool(&node, &req.name, Op::Upsert);
    Ok((StatusCode::CREATED, axum::Json(info)))
}

/// Removes an API-created pool from Prism. Files are left in place.
async fn remove(
    State(node): State<AppState>,
    Path((n, pool)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    check_node(&node, &n)?;
    let p = node.storage.get(&pool)?;
    if p.static_config {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "static_pool",
            "pools from prism.toml are removed by editing prism.toml",
        ));
    }
    let users = node.pool_users(&pool)?;
    if !users.is_empty() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "in_use",
            format!("used by {}", users.join(", ")),
        ));
    }
    node.db.delete_pool(&pool)?;
    node.storage.remove(&pool);
    node.bus.publish(
        "pool",
        format!("/nodes/{}/pools/{pool}", node.id),
        Op::Delete,
        serde_json::Value::Null,
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn volumes(
    State(node): State<AppState>,
    Path((n, pool)): Path<(String, String)>,
) -> Result<axum::Json<Vec<Volume>>, ApiError> {
    check_node(&node, &n)?;
    let p = node.storage.get(&pool)?;
    Ok(axum::Json(
        tokio::task::spawn_blocking(move || p.volumes())
            .await
            .map_err(anyhow::Error::from)??,
    ))
}

async fn volume(
    State(node): State<AppState>,
    Path((n, pool, volume)): Path<(String, String, String)>,
) -> Result<axum::Json<Volume>, ApiError> {
    check_node(&node, &n)?;
    Ok(axum::Json(node.storage.get(&pool)?.volume(&volume)?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateVolume {
    name: String,
    size_gib: u64,
    #[serde(default)]
    format: Option<String>,
}

async fn create_volume(
    State(node): State<AppState>,
    Path((n, pool)): Path<(String, String)>,
    Json(req): Json<CreateVolume>,
) -> Result<impl IntoResponse, ApiError> {
    check_node(&node, &n)?;
    if req.size_gib == 0 {
        return Err(ApiError::bad_request("size_gib must be positive"));
    }
    let p = node.storage.get(&pool)?;
    let format = req.format.unwrap_or_else(|| p.format.clone());
    let v = p
        .create_volume(
            &node.cfg.libvirt.qemu_img,
            &req.name,
            req.size_gib << 30,
            &format,
        )
        .await?;
    publish_pool(&node, &pool, Op::Upsert);
    Ok((StatusCode::CREATED, axum::Json(v)))
}

/// Raw upload: `PUT` the file body as `application/octet-stream`.
async fn upload(
    State(node): State<AppState>,
    Path((n, pool, volume)): Path<(String, String, String)>,
    body: Body,
) -> Result<impl IntoResponse, ApiError> {
    check_node(&node, &n)?;
    let p = node.storage.get(&pool)?;
    let v = p.upload(&volume, body.into_data_stream()).await?;
    tracing::info!(pool = %pool, volume = %volume, bytes = v.size_bytes, "upload complete");
    publish_pool(&node, &pool, Op::Upsert);
    Ok((StatusCode::CREATED, axum::Json(v)))
}

async fn delete_volume(
    State(node): State<AppState>,
    Path((n, pool, volume)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    check_node(&node, &n)?;
    let users = node.volume_users(&pool, &volume)?;
    if !users.is_empty() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "in_use",
            format!("used by {}", users.join(", ")),
        ));
    }
    node.storage.get(&pool)?.delete_volume(&volume)?;
    publish_pool(&node, &pool, Op::Upsert);
    Ok(StatusCode::NO_CONTENT)
}
