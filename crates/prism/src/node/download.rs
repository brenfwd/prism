//! Fetch an ISO or disk image from a URL into a pool, as a task.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use super::{Node, NodeError, Result};
use crate::storage::valid_name;
use crate::tasks::{Task, TaskCtx};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadReq {
    /// An `http://` or `https://` URL.
    pub url: String,
    /// Volume name; defaults to the last segment of the URL path.
    #[serde(default)]
    pub name: Option<String>,
}

/// The default volume name for `url`.
fn name_from_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let last = path.rsplit('/').next()?;
    (valid_name(last) && last.len() <= 128).then(|| last.to_string())
}

fn curl_base(cmd: &mut tokio::process::Command) {
    cmd.args([
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--proto",
        "=http,https",
        "--proto-redir",
        "=http,https",
        "--max-redirs",
        "5",
        "--connect-timeout",
        "15",
    ]);
}

/// The size announced by the server, when it says.
async fn remote_size(url: &str) -> Option<u64> {
    let mut cmd = tokio::process::Command::new("curl");
    curl_base(&mut cmd);
    let out = cmd
        .args(["--head", "--max-time", "20", "--", url])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .rev()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())
                .flatten()
        })
}

impl Node {
    pub fn download_volume(self: &Arc<Self>, pool: &str, req: DownloadReq) -> Result<Task> {
        if !(req.url.starts_with("http://") || req.url.starts_with("https://")) {
            return Err(NodeError::Invalid("the URL must be http or https".into()));
        }
        let name = req
            .name
            .clone()
            .or_else(|| name_from_url(&req.url))
            .ok_or_else(|| {
                NodeError::Invalid("name the volume; the URL has no file name".into())
            })?;
        if !valid_name(&name) {
            return Err(NodeError::Invalid("invalid volume name".into()));
        }
        let pool = self.storage.get(pool)?;
        if pool.volume(&name).is_ok() {
            return Err(NodeError::Conflict(format!("{name} already exists")));
        }
        let node = Arc::clone(self);
        let resource = format!("/nodes/{}/pools/{}/volumes/{name}", self.id, pool.name);
        Ok(self
            .tasks
            .spawn("volume.download", resource, move |ctx| async move {
                ctx.progress(None, format!("connecting to {}", req.url));
                let total = remote_size(&req.url).await;
                let (partial, path) = pool.new_partial()?;
                let res = fetch(&ctx, &req.url, &path, total).await;
                if let Err(e) = res {
                    pool.abort_partial(&partial);
                    return Err(e);
                }
                pool.commit_partial(&partial, &name)?;
                let v = pool.volume(&name)?;
                node.bus.publish(
                    "pool",
                    format!("/nodes/{}/pools/{}", node.id, pool.name),
                    crate::events::Op::Upsert,
                    serde_json::to_value(pool.info()).unwrap_or_default(),
                );
                Ok(serde_json::json!({ "name": name, "size_bytes": v.file_bytes }))
            }))
    }
}

async fn fetch(
    ctx: &TaskCtx,
    url: &str,
    path: &std::path::Path,
    total: Option<u64>,
) -> anyhow::Result<()> {
    let mut cmd = tokio::process::Command::new("curl");
    curl_base(&mut cmd);
    let mut child = cmd
        .arg("--output")
        .arg(path)
        .args(["--", url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("starting curl: {e}"))?;
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            () = ctx.cancel.cancelled() => {
                let _ = child.kill().await;
                anyhow::bail!("cancelled");
            }
            _ = tick.tick() => {
                let done = tokio::fs::metadata(path).await.map_or(0, |m| m.len());
                let frac = total.filter(|t| *t > 0).map(|t| (done as f64 / t as f64).min(0.99));
                ctx.progress(frac, format!("downloaded {} MiB", done >> 20));
            }
            status = child.wait() => {
                let status = status?;
                if status.success() {
                    return Ok(());
                }
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = s.read_to_string(&mut err).await;
                }
                anyhow::bail!("download failed: {}", err.trim());
            }
        }
    }
}
