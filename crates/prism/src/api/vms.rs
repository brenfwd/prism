use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde::Deserialize;
use uuid::Uuid;

use super::{ApiError, AppState, Json};
use crate::node::{Action, VmView};
use crate::spec::{CreateVm, UpdateVm};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/vms", get(list).post(create))
        .route("/vms/{id}", get(show).patch(update).delete(remove))
        .route("/vms/{id}/actions/{action}", post(action))
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
