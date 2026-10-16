//! HTTP API under `/api/v1`, plus the embedded web UI at every other path.
//!
//! Each submodule owns the routes of one resource (VMs, pools, networks,
//! the archive, the cluster and so on). Errors leave as RFC 9457
//! `application/problem+json` bodies built from [`ApiError`]. Request
//! middleware, outermost first: host and origin checks ([`hygiene`]), then
//! for `/api/v1` the degraded-cluster header and forwarding of requests to the
//! node that owns the resource ([`forward`]). Request bodies over 1 MiB are
//! rejected, except where a route raises the limit (uploads).

mod archive;
mod archive_ops;
mod cluster;
mod console;
mod forward;
mod host;
mod hygiene;
mod internal;
mod networks;
mod openapi;
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

/// The shared node handle every handler receives.
pub type AppState = Arc<Node>;

/// Builds the full application: the API under `/api/v1`, the UI fallback and
/// the security headers and request checks.
pub(crate) fn router(node: AppState) -> Router {
    let api = Router::new()
        .route("/nodes", get(list_nodes))
        .route("/nodes/{node}", get(get_node))
        .merge(vms::routes())
        .merge(vms::backup_routes())
        .merge(pools::routes())
        .merge(host::routes())
        .merge(networks::routes())
        .merge(console::routes())
        .merge(tasks::routes())
        .merge(cluster::routes())
        .merge(internal::routes())
        .merge(archive::routes())
        .merge(archive_ops::routes())
        .route("/events", get(sse::events))
        .route("/openapi.json", get(openapi::serve))
        .fallback(|| async { ApiError::not_found("no such endpoint") })
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&node),
            forward::route,
        ))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&node),
            forward::degraded,
        ));

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
    axum::Json(node.nodes())
}

async fn get_node(
    State(node): State<AppState>,
    Path(id): Path<String>,
) -> Result<axum::Json<NodeView>, ApiError> {
    node.nodes()
        .into_iter()
        .find(|n| id == "local" && n.id == node.id || n.id.to_string() == id)
        .map(axum::Json)
        .ok_or_else(|| ApiError::not_found(format!("node {id}")))
}

/// Accepts this node's ID or the alias `local`; any other ID is a 404.
pub(crate) fn check_node(node: &Node, id: &str) -> Result<(), ApiError> {
    if id == "local" || id == node.id.to_string() {
        Ok(())
    } else {
        Err(ApiError::not_found(format!("node {id}")))
    }
}

/// Wire format of every error body (RFC 9457 problem details, plus `code`,
/// `retryable` and `degraded`).
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

/// An error response: an HTTP status, a stable machine-readable `code` and
/// a human-readable `detail`.
///
/// Statuses 503 and 502 are marked `retryable` in the body, and the code
/// `cluster_degraded` sets `degraded`. Server errors are logged when the
/// response is built.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    detail: String,
}

impl ApiError {
    /// An error with an explicit status and code.
    pub fn new(status: StatusCode, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code,
            detail: detail.into(),
        }
    }

    /// A 404 with code `not_found`.
    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", detail)
    }

    /// The human-readable message.
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// A 400 with code `invalid`.
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
            degraded: self.code == "cluster_degraded",
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
            NodeError::Degraded(_) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "cluster_degraded",
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

/// JSON request-body extractor. It replaces `axum::Json` so that a missing
/// content type or a malformed body is answered as problem+json with code
/// `invalid_body`, not as axum's plain-text rejection.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(e: impl Into<ApiError>) -> (StatusCode, &'static str, String) {
        let e = e.into();
        (e.status, e.code, e.detail)
    }

    #[test]
    fn node_errors_map_to_status_and_code() {
        let cases: Vec<(NodeError, StatusCode, &str)> = vec![
            (
                NodeError::NotFound("vm".into()),
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                NodeError::Conflict("busy".into()),
                StatusCode::CONFLICT,
                "conflict",
            ),
            (
                NodeError::Invalid("bad".into()),
                StatusCode::BAD_REQUEST,
                "invalid",
            ),
            (
                NodeError::Unavailable,
                StatusCode::SERVICE_UNAVAILABLE,
                "libvirt_unavailable",
            ),
            (
                NodeError::Degraded("read-only".into()),
                StatusCode::SERVICE_UNAVAILABLE,
                "cluster_degraded",
            ),
            (
                NodeError::Internal(anyhow::anyhow!("boom")),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ),
        ];
        for (err, status, code) in cases {
            let (s, c, _) = parts(err);
            assert_eq!((s, c), (status, code));
        }
    }

    #[test]
    fn storage_errors_map_to_status_and_code() {
        let (s, c, _) = parts(StorageError::NoPool("p".into()));
        assert_eq!((s, c), (StatusCode::NOT_FOUND, "not_found"));
        let (s, c, _) = parts(StorageError::Exists("v".into()));
        assert_eq!((s, c), (StatusCode::CONFLICT, "exists"));
        let (s, c, _) = parts(StorageError::InvalidName("..".into()));
        assert_eq!((s, c), (StatusCode::BAD_REQUEST, "invalid"));
    }

    #[tokio::test]
    async fn problem_body_flags_retryable_and_degraded() {
        let res =
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "cluster_degraded", "x").into_response();
        assert_eq!(
            res.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["retryable"], true);
        assert_eq!(v["degraded"], true);
        assert_eq!(v["status"], 503);

        let res = ApiError::bad_request("no").into_response();
        let body = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            (v["retryable"].clone(), v["degraded"].clone()),
            (false.into(), false.into())
        );
    }
}
