//! Endpoints only peers may call, through the mesh tunnel.

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use uuid::Uuid;

use super::{ApiError, AppState, Json};
use crate::cluster::peers::Tunnelled;
use crate::node::{FinishReq, PrepareReq};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/internal/migrations/{id}/prepare", post(prepare))
        .route("/internal/migrations/{id}/finish", post(finish))
        .layer(axum::middleware::from_fn(only_tunnelled))
}

async fn only_tunnelled(req: Request, next: Next) -> Response {
    if req.extensions().get::<Tunnelled>().is_none() {
        return ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such endpoint")
            .into_response();
    }
    next.run(req).await
}

fn parse(id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id).map_err(|_| ApiError::not_found("migration"))
}

async fn prepare(
    State(node): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PrepareReq>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(axum::Json(node.migration_prepare(parse(&id)?, req).await?))
}

async fn finish(
    State(node): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<FinishReq>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(axum::Json(node.migration_finish(parse(&id)?, req).await?))
}
