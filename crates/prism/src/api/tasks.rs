use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use serde::Deserialize;
use uuid::Uuid;

use super::{ApiError, AppState};
use crate::tasks::Task;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/tasks", get(list))
        .route("/tasks/{id}", get(show))
        .route("/tasks/{id}/cancel", post(cancel))
}

fn parse_id(id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id).map_err(|_| ApiError::not_found(format!("task {id}")))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    100
}

async fn list(
    State(node): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<axum::Json<Vec<Task>>, ApiError> {
    // Every node's tasks: peers' come from their event streams.
    Ok(axum::Json(node.tasks.list_cluster(q.limit.min(1000))?))
}

async fn show(
    State(node): State<AppState>,
    Path(id): Path<String>,
) -> Result<axum::Json<Task>, ApiError> {
    node.tasks
        .get(parse_id(&id)?)?
        .map(axum::Json)
        .ok_or_else(|| ApiError::not_found(format!("task {id}")))
}

async fn cancel(
    State(node): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if node.tasks.cancel(parse_id(&id)?) {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_running",
            "task is not running",
        ))
    }
}
