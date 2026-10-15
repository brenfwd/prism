//! `/api/v1/networks`.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};

use super::{ApiError, AppState, Json};
use crate::network::{CheckNetwork, CreateNetwork, UpdateNetwork};
use crate::node::{NetworkCheck, NetworkView};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/networks", get(list).post(create))
        .route("/networks/check", post(check))
        .route("/networks/{id}", get(show).patch(update).delete(remove))
}

async fn list(State(node): State<AppState>) -> Result<axum::Json<Vec<NetworkView>>, ApiError> {
    Ok(axum::Json(node.networks()?))
}

async fn create(
    State(node): State<AppState>,
    Json(req): Json<CreateNetwork>,
) -> Result<impl IntoResponse, ApiError> {
    let net = node.create_network(req).await?;
    let location = format!("/api/v1/networks/{}", net.network.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        axum::Json(net),
    ))
}

async fn check(
    State(node): State<AppState>,
    Json(req): Json<CheckNetwork>,
) -> Result<axum::Json<NetworkCheck>, ApiError> {
    Ok(axum::Json(node.check_network(&req)?))
}

async fn show(
    State(node): State<AppState>,
    Path(id): Path<String>,
) -> Result<axum::Json<NetworkView>, ApiError> {
    Ok(axum::Json(node.network(&id)?))
}

async fn update(
    State(node): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateNetwork>,
) -> Result<axum::Json<NetworkView>, ApiError> {
    Ok(axum::Json(node.update_network(&id, req).await?))
}

async fn remove(
    State(node): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    node.delete_network(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}
