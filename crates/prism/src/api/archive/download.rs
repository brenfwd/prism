//! `GET /archive/pools/{pool}/files/{path}`: ranged download from a replica.

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::{ApiError, AppState, LocalQuery, forward_to};

/// Parses `bytes=a-b`, `bytes=a-` or `bytes=-n` against `size`.
fn parse_range(v: &str, size: u64) -> Option<(u64, u64)> {
    let spec = v.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (size.saturating_sub(n), size.checked_sub(1)?)
        }
        (a, "") => (a.parse().ok()?, size.checked_sub(1)?),
        (a, b) => (
            a.parse().ok()?,
            b.parse::<u64>().ok()?.min(size.checked_sub(1)?),
        ),
    };
    (start <= end && end < size).then_some((start, end))
}

/// Serves a file (with `Range`) from a replica on this node, or proxies it
/// from one elsewhere.
pub(super) async fn download(
    State(node): State<AppState>,
    Path((pool, path)): Path<(String, String)>,
    Query(q): Query<LocalQuery>,
    headers: HeaderMap,
    req: Request,
) -> Result<Response, ApiError> {
    let file = node
        .state
        .archive_file(&pool, &path)?
        .ok_or_else(|| ApiError::not_found(format!("{pool}/{path}")))?;
    let local = file
        .replicas
        .iter()
        .find(|r| r.node == node.id)
        .and_then(|r| node.archive.member(&r.member));
    let Some(member) = local else {
        if q.local.is_some() {
            return Err(ApiError::not_found("no replica on this node"));
        }
        let holder = file.replicas.first().ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "replica_unavailable",
                "no replicas",
            )
        })?;
        return forward_to(&node, holder.node, req, None).await;
    };
    let mut f = tokio::fs::File::from_std(member.open_read(&path)?);
    let size = file.size;
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|v| parse_range(v, size));
    let name = path.rsplit('/').next().unwrap_or("file").to_string();
    let mut res = match range {
        Some(None) => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{size}"))],
            )
                .into_response());
        }
        Some(Some((start, end))) => {
            f.seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(anyhow::Error::from)?;
            let len = end - start + 1;
            let body = Body::from_stream(tokio_util::io::ReaderStream::new(f.take(len)));
            let mut r = (StatusCode::PARTIAL_CONTENT, body).into_response();
            r.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {start}-{end}/{size}"))
                    .map_err(anyhow::Error::from)?,
            );
            r.headers_mut()
                .insert(header::CONTENT_LENGTH, HeaderValue::from(len));
            r
        }
        None => {
            let mut r = Body::from_stream(tokio_util::io::ReaderStream::new(f)).into_response();
            r.headers_mut()
                .insert(header::CONTENT_LENGTH, HeaderValue::from(size));
            r
        }
    };
    let h = res.headers_mut();
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Ok(v) = HeaderValue::from_str(&format!("\"{}\"", file.blake3)) {
        h.insert(header::ETAG, v);
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=900-", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=990-2000", 1000), Some((990, 999)));
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("bytes=5-1", 1000), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 1000), None);
    }
}
