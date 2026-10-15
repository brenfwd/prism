use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{delete, get, patch, post};
use serde::Deserialize;
use uuid::Uuid;

use super::{ApiError, AppState, Json};
use crate::node::{Action, AddDisk, AddNic, CreateSnapshot, SnapshotView, UpdateDisk, VmView};
use crate::spec::{CreateVm, UpdateVm};
use crate::tasks::Task;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/vms", get(list).post(create))
        .route("/vms/{id}", get(show).patch(update).delete(remove))
        .route("/vms/{id}/actions/{action}", post(action))
        .route("/vms/{id}/disks", post(add_disk))
        .route(
            "/vms/{id}/disks/{disk}",
            patch(update_disk).delete(remove_disk),
        )
        .route("/vms/{id}/nics", post(add_nic))
        .route("/vms/{id}/nics/{nic}", delete(remove_nic))
        .route("/vms/{id}/snapshots", get(snapshots).post(create_snapshot))
        .route("/vms/{id}/snapshots/{name}", delete(delete_snapshot))
        .route("/vms/{id}/snapshots/{name}/revert", post(revert_snapshot))
}

fn parse_sub(id: &str, what: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id).map_err(|_| ApiError::not_found(format!("{what} {id}")))
}

/// `202 Accepted` with the task and its `Location`.
fn accepted(task: Task) -> impl IntoResponse {
    let location = format!("/api/v1/tasks/{}", task.id);
    (
        StatusCode::ACCEPTED,
        [(header::LOCATION, location)],
        axum::Json(task),
    )
}

async fn add_disk(
    State(node): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AddDisk>,
) -> Result<axum::Json<VmView>, ApiError> {
    Ok(axum::Json(node.add_disk(parse_id(&id)?, req).await?))
}

async fn update_disk(
    State(node): State<AppState>,
    Path((id, disk)): Path<(String, String)>,
    Json(req): Json<UpdateDisk>,
) -> Result<axum::Json<VmView>, ApiError> {
    Ok(axum::Json(
        node.update_disk(parse_id(&id)?, parse_sub(&disk, "disk")?, req)
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
struct RemoveDiskQuery {
    #[serde(default)]
    delete_volume: bool,
}

async fn remove_disk(
    State(node): State<AppState>,
    Path((id, disk)): Path<(String, String)>,
    Query(q): Query<RemoveDiskQuery>,
) -> Result<axum::Json<VmView>, ApiError> {
    Ok(axum::Json(
        node.remove_disk(parse_id(&id)?, parse_sub(&disk, "disk")?, q.delete_volume)
            .await?,
    ))
}

async fn add_nic(
    State(node): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AddNic>,
) -> Result<axum::Json<VmView>, ApiError> {
    Ok(axum::Json(node.add_nic(parse_id(&id)?, req).await?))
}

async fn remove_nic(
    State(node): State<AppState>,
    Path((id, nic)): Path<(String, String)>,
) -> Result<axum::Json<VmView>, ApiError> {
    Ok(axum::Json(
        node.remove_nic(parse_id(&id)?, parse_sub(&nic, "nic")?)
            .await?,
    ))
}

async fn snapshots(
    State(node): State<AppState>,
    Path(id): Path<String>,
) -> Result<axum::Json<Vec<SnapshotView>>, ApiError> {
    Ok(axum::Json(node.snapshots(parse_id(&id)?).await?))
}

async fn create_snapshot(
    State(node): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CreateSnapshot>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(accepted(node.create_snapshot(parse_id(&id)?, req)?))
}

async fn delete_snapshot(
    State(node): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(accepted(node.delete_snapshot(parse_id(&id)?, name)?))
}

async fn revert_snapshot(
    State(node): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(accepted(node.revert_snapshot(parse_id(&id)?, name)?))
}

fn parse_id(id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id).map_err(|_| ApiError::not_found(format!("vm {id}")))
}

async fn list(State(node): State<AppState>) -> Result<axum::Json<Vec<VmView>>, ApiError> {
    Ok(axum::Json(node.vms()?))
}

async fn create(
    State(node): State<AppState>,
    Json(req): Json<CreateVm>,
) -> Result<impl IntoResponse, ApiError> {
    let vm = node.create_vm(req).await?;
    let location = format!("/api/v1/vms/{}", vm.spec.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        axum::Json(vm),
    ))
}

async fn show(
    State(node): State<AppState>,
    Path(id): Path<String>,
) -> Result<axum::Json<VmView>, ApiError> {
    Ok(axum::Json(node.vm(parse_id(&id)?)?))
}

async fn update(
    State(node): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateVm>,
) -> Result<axum::Json<VmView>, ApiError> {
    Ok(axum::Json(node.update_vm(parse_id(&id)?, req).await?))
}

#[derive(Debug, Deserialize)]
struct DeleteQuery {
    #[serde(default)]
    delete_disks: bool,
}

async fn remove(
    State(node): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> Result<StatusCode, ApiError> {
    node.delete_vm(parse_id(&id)?, q.delete_disks).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn action(
    State(node): State<AppState>,
    Path((id, action)): Path<(String, String)>,
) -> Result<axum::Json<VmView>, ApiError> {
    let action: Action = action.parse()?;
    Ok(axum::Json(node.action(parse_id(&id)?, action).await?))
}
