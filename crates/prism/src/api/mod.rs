//! HTTP API under `/api/v1`, plus the embedded UI.

mod console;
mod hygiene;
mod pools;
mod sse;
mod tasks;
mod vms;

use std::sync::Arc;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Serialize;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::node::{Node, NodeError, NodeView};
use crate::storage::StorageError;

pub type AppState = Arc<Node>;

pub fn router(node: AppState) -> Router {
    let api = Router::new()
        .route("/nodes", get(list_nodes))
        .route("/nodes/{node}", get(get_node))
        .merge(vms::routes())
        .merge(pools::routes())
        .merge(console::routes())
        .merge(tasks::routes())
        .route("/events", get(sse::events))
        .fallback(|| async { ApiError::not_found("no such endpoint") });

    let hygiene = hygiene::Hygiene::new(&node.cfg.http, &node.name);
    Router::new()
        .nest("/api/v1", api)
        .fallback(crate::ui::serve)
        .layer(DefaultBodyLimit::max(1 << 20))
        .layer(axum::middleware::from_fn_with_state(
            hygiene,
            hygiene::check,
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("cross-origin-resource-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(node)
}

async fn list_nodes(State(node): State<AppState>) -> axum::Json<Vec<NodeView>> {
    axum::Json(vec![node.view()])
}

async fn get_node(
    State(node): State<AppState>,
    Path(id): Path<String>,
) -> Result<axum::Json<NodeView>, ApiError> {
    check_node(&node, &id)?;
    Ok(axum::Json(node.view()))
}

/// Accepts this node's ID or the alias `local`.
pub fn check_node(node: &Node, id: &str) -> Result<(), ApiError> {
    if id == "local" || id == node.id.to_string() {
        Ok(())
    } else {
        Err(ApiError::not_found(format!("node {id}")))
    }
}

/// RFC 9457 problem details.
#[derive(Debug, Serialize)]
pub struct Problem {
    #[serde(rename = "type")]
    ty: &'static str,
    title: String,
    status: u16,
    code: &'static str,
    detail: String,
    retryable: bool,
    degraded: bool,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    detail: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code,
            detail: detail.into(),
        }
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", detail)
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid", detail)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::error!(code = self.code, "{}", self.detail);
        }
        let body = Problem {
            ty: "about:blank",
            title: self.status.canonical_reason().unwrap_or("Error").into(),
            status: self.status.as_u16(),
            code: self.code,
            retryable: matches!(
                self.status,
                StatusCode::SERVICE_UNAVAILABLE | StatusCode::BAD_GATEWAY
            ),
            degraded: false,
            detail: self.detail,
        };
        let mut res = (self.status, axum::Json(body)).into_response();
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        res
    }
}

impl From<NodeError> for ApiError {
    fn from(e: NodeError) -> Self {
        match e {
            NodeError::NotFound(_) => Self::new(StatusCode::NOT_FOUND, "not_found", e.to_string()),
            NodeError::Conflict(_) => Self::new(StatusCode::CONFLICT, "conflict", e.to_string()),
            NodeError::Invalid(_) => Self::bad_request(e.to_string()),
            NodeError::Unavailable => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "libvirt_unavailable",
                e.to_string(),
            ),
            NodeError::Libvirt(prism_libvirt::Error::Remote(r)) => Self::new(
                StatusCode::CONFLICT,
                "libvirt_error",
                r.message
                    .unwrap_or_else(|| format!("libvirt error {}", r.code)),
            ),
            NodeError::Libvirt(e) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "libvirt_unavailable",
                e.to_string(),
            ),
            NodeError::Storage(e) => e.into(),
            NodeError::Internal(e) => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                format!("{e:#}"),
            ),
        }
    }
}

impl From<StorageError> for ApiError {
    fn from(e: StorageError) -> Self {
        let (status, code) = match &e {
            StorageError::NoPool(_) | StorageError::NoVolume(_) => {
                (StatusCode::NOT_FOUND, "not_found")
            }
            StorageError::Exists(_) => (StatusCode::CONFLICT, "exists"),
            StorageError::InvalidName(_) | StorageError::Invalid(_) => {
                (StatusCode::BAD_REQUEST, "invalid")
            }
            StorageError::Io(_) | StorageError::QemuImg(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "storage_error")
            }
        };
        Self::new(status, code, e.to_string())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        NodeError::Internal(e).into()
    }
}

/// JSON body extractor that reports errors as problem+json.
pub struct Json<T>(pub T);

impl<T, S> axum::extract::FromRequest<S> for Json<T>
where
    axum::Json<T>:
        axum::extract::FromRequest<S, Rejection = axum::extract::rejection::JsonRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        axum::Json::<T>::from_request(req, state)
            .await
            .map(|axum::Json(v)| Self(v))
            .map_err(|e| ApiError::new(e.status(), "invalid_body", e.body_text()))
    }
}
