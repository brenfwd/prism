//! A small HTTP/1.1 client for the local daemon's JSON API.
//!
//! Every call opens one connection, sends one request with `Connection:
//! close` and reads the response to the end. Connecting and reading each have
//! a time limit so a stuck daemon cannot hang the command.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, bail};
use axum::http::Method;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::config::Config;

/// Longest wait to establish the TCP connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest wait for the whole request/response exchange. Joins and removals
/// talk to other nodes, so this is generous.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(120);

/// The daemon's API address: a loopback listener if there is one.
///
/// Fails when `http.listen` is empty.
pub(super) fn api_addr(cfg: &Config) -> anyhow::Result<SocketAddr> {
    cfg.http
        .listen
        .iter()
        .find(|a| a.ip().is_loopback())
        .or_else(|| cfg.http.listen.first())
        .copied()
        .context("http.listen is empty")
}

/// Sends one JSON request to `/api/v1{path}` on the local daemon and returns
/// the parsed response body (`Null` when it is empty).
///
/// Fails when the daemon is unreachable, the exchange times out, the response
/// is malformed, or the status is 400 or above; in that case the error text is
/// the response's `detail` field when present.
pub(crate) async fn call(
    cfg: &Config,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> anyhow::Result<Value> {
    let addr = api_addr(cfg)?;
    let mut sock = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow::anyhow!("timed out connecting to prism at {addr}"))?
        .with_context(|| format!("connecting to prism at {addr}; is `prism serve` running?"))?;
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let req = format!(
        "{method} /api/v1{path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let raw = tokio::time::timeout(EXCHANGE_TIMEOUT, async {
        sock.write_all(req.as_bytes()).await?;
        let mut raw = Vec::new();
        sock.read_to_end(&mut raw).await?;
        anyhow::Ok(raw)
    })
    .await
    .map_err(|_| anyhow::anyhow!("prism at {addr} did not answer in time"))??;
    let (status, content) = parse_response(&raw)?;
    let value: Value = if content.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        serde_json::from_slice(&content).context("parsing the response body")?
    };
    if status >= 400 {
        bail!(
            "{}",
            value["detail"]
                .as_str()
                .map_or_else(|| format!("HTTP {status}"), str::to_string)
        );
    }
    Ok(value)
}

/// Splits a raw response into its status code and decoded body.
fn parse_response(raw: &[u8]) -> anyhow::Result<(u16, Vec<u8>)> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut resp = httparse::Response::new(&mut headers);
    let httparse::Status::Complete(head_len) =
        resp.parse(raw).context("malformed HTTP response")?
    else {
        bail!("truncated HTTP response");
    };
    let status = resp.code.context("malformed HTTP status")?;
    let chunked = resp.headers.iter().any(|h| {
        h.name.eq_ignore_ascii_case("transfer-encoding")
            && String::from_utf8_lossy(h.value)
                .to_ascii_lowercase()
                .contains("chunked")
    });
    let content = &raw[head_len..];
    let content = if chunked {
        dechunk(content)?
    } else {
        content.to_vec()
    };
    Ok((status, content))
}

/// Decodes a chunked transfer-encoded body.
///
/// Fails on a bad chunk size or a body that ends before its last chunk.
fn dechunk(mut rest: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .context("truncated chunked body")?;
        let size_line = std::str::from_utf8(&rest[..eol]).context("bad chunk size")?;
        // Chunk extensions follow a semicolon and carry nothing we use.
        let size = size_line.split(';').next().unwrap_or_default().trim();
        let n = usize::from_str_radix(size, 16).context("bad chunk size")?;
        rest = &rest[eol + 2..];
        if n == 0 {
            return Ok(out);
        }
        if rest.len() < n {
            bail!("truncated chunked body");
        }
        out.extend_from_slice(&rest[..n]);
        rest = rest[n..].strip_prefix(b"\r\n").unwrap_or(&rest[n..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dechunks() {
        let body = dechunk(b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n").unwrap();
        assert_eq!(body, b"hello world");
    }

    #[test]
    fn dechunk_rejects_bad_input() {
        assert!(dechunk(b"zz\r\nhello\r\n").is_err());
        assert!(dechunk(b"9\r\nshort\r\n").is_err());
        assert!(dechunk(b"5\r\nhello").is_err());
    }

    #[test]
    fn parses_plain_and_chunked_responses() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(parse_response(plain).unwrap(), (200, b"{}".to_vec()));
        let chunked =
            b"HTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked).unwrap(), (404, b"{}".to_vec()));
        assert!(parse_response(b"garbage").is_err());
    }

    #[test]
    fn api_addr_prefers_loopback() {
        let mut cfg = Config::default();
        cfg.http.listen = vec![
            "10.0.0.5:8080".parse().unwrap(),
            "127.0.0.1:9000".parse().unwrap(),
        ];
        assert_eq!(api_addr(&cfg).unwrap().port(), 9000);
        cfg.http.listen = vec!["10.0.0.5:8080".parse().unwrap()];
        assert_eq!(api_addr(&cfg).unwrap().port(), 8080);
        cfg.http.listen.clear();
        assert!(api_addr(&cfg).is_err());
    }
}
