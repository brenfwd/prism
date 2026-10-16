//! Request hygiene: stops websites and DNS rebinding from driving the API
//! through an operator's browser. This is not authentication.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::ApiError;
use crate::config::HttpConfig;

/// Request filter state: the host names and origins this node answers to.
///
/// A request passes when its `Host` is allowed (or `*` is configured) and,
/// for requests that change state, its `Origin` matches. Built once from
/// `http.allowed_hosts` and the node's names.
#[derive(Debug, Clone)]
pub(crate) struct Hygiene(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    any_host: bool,
    hosts: Vec<String>,
    origins: Vec<String>,
}

impl Hygiene {
    /// Collects the allowed hosts: the configured ones, `localhost`, the node
    /// name and the system host name (all lower-cased).
    pub(crate) fn new(cfg: &HttpConfig, node_name: &str) -> Self {
        let mut hosts: Vec<String> = cfg
            .allowed_hosts
            .iter()
            .map(|h| h.to_ascii_lowercase())
            .collect();
        hosts.push("localhost".into());
        hosts.push(node_name.to_ascii_lowercase());
        if let Ok(h) = rustix::system::uname().nodename().to_str() {
            hosts.push(h.to_ascii_lowercase());
        }
        Self(Arc::new(Inner {
            any_host: cfg.allowed_hosts.iter().any(|h| h == "*"),
            hosts,
            origins: cfg
                .allowed_origins
                .iter()
                .map(|o| o.trim_end_matches('/').to_ascii_lowercase())
                .collect(),
        }))
    }
}

/// Strips an optional port from a `Host` value.
fn host_name(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match host.rsplit_once(':') {
        Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    }
}

impl Inner {
    fn host_allowed(&self, host: &str) -> bool {
        if self.any_host {
            return true;
        }
        let name = host_name(host).to_ascii_lowercase();
        name.parse::<IpAddr>().is_ok() || self.hosts.contains(&name)
    }

    fn origin_allowed(&self, origin: &str, host: &str) -> bool {
        let origin = origin.trim_end_matches('/').to_ascii_lowercase();
        let host = host.to_ascii_lowercase();
        origin == format!("http://{host}")
            || origin == format!("https://{host}")
            || self.origins.contains(&origin)
    }
}

fn header_str<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

/// Middleware that answers disallowed requests with a problem response and
/// lets requests tunnelled from a peer through unchecked.
pub(crate) async fn check(State(h): State<Hygiene>, req: Request, next: Next) -> Response {
    // Requests tunnelled from a peer were checked by the node that took them.
    if req
        .extensions()
        .get::<crate::cluster::peers::Tunnelled>()
        .is_some()
    {
        return next.run(req).await;
    }
    match verdict(&h.0, req.method(), req.headers()) {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

fn verdict(h: &Inner, method: &Method, headers: &HeaderMap) -> Result<(), ApiError> {
    let host = header_str(headers, "x-forwarded-host")
        .or_else(|| header_str(headers, header::HOST.as_str()))
        .unwrap_or("");
    if !h.host_allowed(host) {
        return Err(ApiError::new(
            StatusCode::MISDIRECTED_REQUEST,
            "host_not_allowed",
            format!("Host {host:?} is not in http.allowed_hosts"),
        ));
    }
    let origin = header_str(headers, header::ORIGIN.as_str());
    let is_ws = headers
        .get(header::UPGRADE)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
    if is_ws {
        return match origin {
            Some(o) if !h.origin_allowed(o, host) => Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "cross_origin",
                "WebSocket Origin does not match Host",
            )),
            _ => Ok(()),
        };
    }
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(());
    }
    let same_site = match header_str(headers, "sec-fetch-site") {
        Some("same-origin" | "none") => true,
        Some(_) => false,
        None => origin.is_none(),
    };
    if !same_site && !origin.is_some_and(|o| h.origin_allowed(o, host)) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "cross_origin",
            "cross-site request refused",
        ));
    }
    // Writes must be JSON, except raw volume uploads.
    let ct = header_str(headers, header::CONTENT_TYPE.as_str()).unwrap_or("");
    let has_body = headers
        .get(header::CONTENT_LENGTH)
        .is_some_and(|v| v.as_bytes() != b"0")
        || headers.contains_key(header::TRANSFER_ENCODING);
    if has_body
        && !ct.starts_with("application/json")
        && !ct.starts_with("application/octet-stream")
    {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "writes require application/json or application/octet-stream",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn inner() -> Inner {
        Inner {
            any_host: false,
            hosts: vec!["localhost".into(), "kvm-a.example.ts.net".into()],
            origins: vec!["https://ops.example".into()],
        }
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn host_check() {
        let i = inner();
        assert!(verdict(&i, &Method::GET, &headers(&[("host", "127.0.0.1:8080")])).is_ok());
        assert!(verdict(&i, &Method::GET, &headers(&[("host", "[::1]:8080")])).is_ok());
        assert!(
            verdict(
                &i,
                &Method::GET,
                &headers(&[("host", "kvm-a.example.ts.net")])
            )
            .is_ok()
        );
        let e = verdict(&i, &Method::GET, &headers(&[("host", "evil.example:8080")])).unwrap_err();
        assert_eq!(e.status, StatusCode::MISDIRECTED_REQUEST);
    }

    #[test]
    fn cross_site_writes() {
        let i = inner();
        // curl: no Origin, no Sec-Fetch-Site.
        assert!(verdict(&i, &Method::POST, &headers(&[("host", "localhost:8080")])).is_ok());
        // Same-origin browser write.
        assert!(
            verdict(
                &i,
                &Method::POST,
                &headers(&[
                    ("host", "localhost:8080"),
                    ("origin", "http://localhost:8080"),
                    ("sec-fetch-site", "same-origin")
                ])
            )
            .is_ok()
        );
        // Cross-site form post.
        assert!(
            verdict(
                &i,
                &Method::POST,
                &headers(&[
                    ("host", "localhost:8080"),
                    ("origin", "https://evil.example"),
                    ("sec-fetch-site", "cross-site")
                ])
            )
            .is_err()
        );
        // Allowed origin.
        assert!(
            verdict(
                &i,
                &Method::POST,
                &headers(&[
                    ("host", "localhost:8080"),
                    ("origin", "https://ops.example"),
                    ("sec-fetch-site", "cross-site")
                ])
            )
            .is_ok()
        );
    }

    #[test]
    fn content_type() {
        let i = inner();
        let e = verdict(
            &i,
            &Method::POST,
            &headers(&[
                ("host", "localhost"),
                ("content-type", "text/plain"),
                ("content-length", "5"),
            ]),
        )
        .unwrap_err();
        assert_eq!(e.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[test]
    fn websocket_origin() {
        let i = inner();
        let ws = |origin| {
            headers(&[
                ("host", "localhost:8080"),
                ("upgrade", "websocket"),
                ("origin", origin),
            ])
        };
        assert!(verdict(&i, &Method::GET, &ws("http://localhost:8080")).is_ok());
        assert!(verdict(&i, &Method::GET, &ws("https://evil.example")).is_err());
    }
}
