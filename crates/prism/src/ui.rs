//! Serves the embedded web UI (`prism-web`), with SPA fallback.

use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
    style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; \
    worker-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'; object-src 'none'";

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let (file, name) = match prism_web::get(path) {
        Some(f) if !path.is_empty() => (f, path),
        _ if path.starts_with("api/") || path.starts_with("assets/") => {
            return StatusCode::NOT_FOUND.into_response();
        }
        _ => match prism_web::get("index.html") {
            Some(f) => (f, "index.html"),
            None => {
                return (StatusCode::NOT_FOUND, "UI assets missing from this build")
                    .into_response();
            }
        },
    };
    let mime = mime_guess::from_path(name).first_or_octet_stream();
    let cache = if name.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut res = (
        [
            (header::CONTENT_TYPE, mime.as_ref().to_string()),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        file.data.into_owned(),
    )
        .into_response();
    if name == "index.html" {
        res.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        );
        res.headers_mut()
            .insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    }
    res
}
