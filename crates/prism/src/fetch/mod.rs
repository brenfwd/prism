//! A small HTTP/1.1 downloader: GET with redirects, streamed to a file, with
//! progress, cancellation and optional SHA-256 verification.
//!
//! TLS is `rustls` with the `ring` provider and the bundled Mozilla roots
//! (`webpki-roots`). That keeps the build free of OpenSSL and of any system
//! certificate store, so a fresh host behaves the same as the dev machine.
//! A full HTTP stack (hyper, reqwest) would be far more than a single
//! streaming GET needs.
//!
//! Connecting and each read have their own timeouts. A download has no total
//! deadline: a server that keeps sending slowly runs until the task's
//! cancellation token fires.

mod body;
mod url;

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use body::BodyReader;
use url::Url;

const MAX_REDIRECTS: usize = 5;
const MAX_HEADERS: usize = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

type Reader = BufReader<Box<dyn AsyncRead + Unpin + Send>>;

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Failed(String),
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    Checksum { expected: String, actual: String },
}

impl From<std::io::Error> for FetchError {
    fn from(e: std::io::Error) -> Self {
        FetchError::Failed(e.to_string())
    }
}

fn fail<T>(msg: impl Into<String>) -> Result<T, FetchError> {
    Err(FetchError::Failed(msg.into()))
}

/// What a finished download produced.
#[derive(Debug)]
pub struct Fetched {
    pub bytes: u64,
    pub sha256: String,
}

/// Checks `s` is a 64-digit hex SHA-256 and returns it lowercase.
pub fn parse_sha256(s: &str) -> Option<String> {
    let s = s.trim().to_ascii_lowercase();
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then_some(s)
}

/// The shared client TLS configuration, built on first use. The `expect`
/// cannot fail: `ring` supports the default protocol versions.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring supports the default protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(cfg)
    })
    .clone()
}

struct Head {
    status: u16,
    location: Option<String>,
    content_length: Option<u64>,
    chunked: bool,
}

async fn connect(
    u: &Url,
) -> Result<(Box<dyn AsyncRead + Unpin + Send>, Box<dyn Writer>), FetchError> {
    let tcp = tokio::time::timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((u.host.as_str(), u.port)),
    )
    .await
    .map_err(|_| FetchError::Failed(format!("connecting to {}: timed out", u.host)))?
    .map_err(|e| FetchError::Failed(format!("connecting to {}: {e}", u.host)))?;
    if let Err(e) = tcp.set_nodelay(true) {
        tracing::debug!(host = %u.host, "could not set TCP_NODELAY: {e}");
    }
    if !u.tls {
        let (r, w) = tcp.into_split();
        return Ok((Box::new(r), Box::new(w)));
    }
    let name = rustls::pki_types::ServerName::try_from(u.host.clone())
        .map_err(|_| FetchError::Failed(format!("invalid host name {}", u.host)))?;
    let tls = tokio_rustls::TlsConnector::from(tls_config())
        .connect(name, tcp)
        .await
        .map_err(|e| FetchError::Failed(format!("TLS to {}: {e}", u.host)))?;
    let (r, w) = tokio::io::split(tls);
    Ok((Box::new(r), Box::new(w)))
}

trait Writer: tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncWrite + Unpin + Send> Writer for T {}

async fn read_head(r: &mut Reader) -> Result<Head, FetchError> {
    let mut buf = Vec::new();
    loop {
        let mut hdrs = [httparse::EMPTY_HEADER; 64];
        let mut resp = httparse::Response::new(&mut hdrs);
        match resp.parse(&buf) {
            Ok(httparse::Status::Complete(_)) => {
                let find = |n: &str| {
                    resp.headers
                        .iter()
                        .find(|h| h.name.eq_ignore_ascii_case(n))
                        .map(|h| String::from_utf8_lossy(h.value).trim().to_string())
                };
                return Ok(Head {
                    status: resp.code.unwrap_or(0),
                    location: find("location"),
                    content_length: find("content-length").and_then(|v| v.parse().ok()),
                    chunked: find("transfer-encoding")
                        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked")),
                });
            }
            Ok(httparse::Status::Partial) => {}
            Err(e) => return fail(format!("bad response from server: {e}")),
        }
        if buf.len() > MAX_HEADERS {
            return fail("response headers too large");
        }
        // Read one byte at a time so no body bytes are consumed past the
        // blank line; the BufReader makes this cheap.
        let b = tokio::time::timeout(IDLE_TIMEOUT, r.read_u8())
            .await
            .map_err(|_| FetchError::Failed("server timed out".into()))??;
        buf.push(b);
    }
}

/// Downloads `url` to `dest`. `progress(done, total)` is called as data
/// arrives; `cancel` aborts promptly. When `want_sha256` is given the file
/// must match it or the call fails (the caller discards the partial file).
pub async fn download(
    url: &str,
    dest: &Path,
    want_sha256: Option<&str>,
    cancel: &CancellationToken,
    mut progress: impl FnMut(u64, Option<u64>) + Send,
) -> Result<Fetched, FetchError> {
    let mut u = Url::parse(url).map_err(FetchError::Failed)?;
    for _ in 0..=MAX_REDIRECTS {
        let (reader, mut writer) = tokio::select! {
            () = cancel.cancelled() => return Err(FetchError::Cancelled),
            r = connect(&u) => r?,
        };
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: prism\r\nAccept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
            u.target,
            u.authority()
        );
        writer.write_all(req.as_bytes()).await?;
        writer.flush().await?;
        let mut reader: Reader = BufReader::new(reader);
        let head = tokio::select! {
            () = cancel.cancelled() => return Err(FetchError::Cancelled),
            h = read_head(&mut reader) => h?,
        };
        match head.status {
            301 | 302 | 303 | 307 | 308 => {
                let loc = head
                    .location
                    .ok_or_else(|| FetchError::Failed("redirect without a location".into()))?;
                u = u.join(&loc).map_err(FetchError::Failed)?;
                continue;
            }
            200 => {}
            s => return fail(format!("server answered HTTP {s}")),
        }
        let mut body = BodyReader::new(reader, head.content_length, head.chunked);
        return stream_to_file(
            &mut body,
            dest,
            head.content_length,
            want_sha256,
            cancel,
            &mut progress,
        )
        .await;
    }
    fail("too many redirects")
}

async fn stream_to_file(
    body: &mut BodyReader,
    dest: &Path,
    total: Option<u64>,
    want: Option<&str>,
    cancel: &CancellationToken,
    progress: &mut (impl FnMut(u64, Option<u64>) + Send),
) -> Result<Fetched, FetchError> {
    let mut file = tokio::fs::File::create(dest).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    let mut last = std::time::Instant::now();
    loop {
        let n = tokio::select! {
            () = cancel.cancelled() => return Err(FetchError::Cancelled),
            n = tokio::time::timeout(IDLE_TIMEOUT, body.read(&mut buf)) => {
                n.map_err(|_| FetchError::Failed("download stalled".into()))??
            }
        };
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).await?;
        done += n as u64;
        if last.elapsed() >= Duration::from_millis(500) {
            last = std::time::Instant::now();
            progress(done, total);
        }
    }
    file.flush().await?;
    file.sync_all().await?;
    if let Some(t) = total
        && t != done
    {
        return fail(format!("connection closed after {done} of {t} bytes"));
    }
    progress(done, total);
    let actual = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if let Some(expected) = want
        && expected != actual
    {
        return Err(FetchError::Checksum {
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(Fetched {
        bytes: done,
        sha256: actual,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_parse() {
        assert!(parse_sha256(&"A".repeat(64)).is_some());
        assert!(parse_sha256("abc").is_none());
    }
}
