//! Fetch an ISO or disk image from a URL into a pool, as a task.

use std::sync::Arc;

use serde::Deserialize;

use super::{Node, NodeError, Result};
use crate::storage::valid_name;
use crate::tasks::Task;

/// Request body for downloading a file into a pool.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadReq {
    /// An `http://` or `https://` URL.
    pub url: String,
    /// Volume name; defaults to the last segment of the URL path.
    #[serde(default)]
    pub name: Option<String>,
    /// Expected SHA-256 (hex). The download fails, and nothing is kept, if
    /// the file does not match.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// The default volume name for `url`.
fn name_from_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let last = path.rsplit('/').next()?;
    (valid_name(last) && last.len() <= 128).then(|| last.to_string())
}

impl Node {
    /// Starts a `volume.download` task that fetches `req.url` into a partial file and renames it
    /// into the pool once the SHA-256 (if given) matches. Fails with `Invalid` for a non-HTTP URL,
    /// a bad checksum or a bad name, and `Conflict` if the volume exists.
    pub fn download_volume(self: &Arc<Self>, pool: &str, req: DownloadReq) -> Result<Task> {
        if !(req.url.starts_with("http://") || req.url.starts_with("https://")) {
            return Err(NodeError::Invalid("the URL must be http or https".into()));
        }
        let sha256 = match req
            .sha256
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(s) => Some(crate::fetch::parse_sha256(s).ok_or_else(|| {
                NodeError::Invalid("the checksum must be 64 hex digits (SHA-256)".into())
            })?),
            None => None,
        };
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
        let policy = crate::fetch::Policy::new(
            &self.cfg.download,
            self.state
                .cluster()
                .ok()
                .flatten()
                .and_then(|c| ipnet::Ipv6Net::new(c.mesh_prefix, 48).ok()),
            crate::fetch::own_addresses(),
        );
        let node = Arc::clone(self);
        let resource = format!("/nodes/{}/pools/{}/volumes/{name}", self.id, pool.name);
        Ok(self
            .tasks
            .spawn("volume.download", resource, move |ctx| async move {
                ctx.progress(None, format!("connecting to {}", req.url));
                let (partial, path) = pool.new_partial()?;
                let fetched = crate::fetch::download(
                    &req.url,
                    &path,
                    sha256.as_deref(),
                    &policy,
                    &ctx.cancel,
                    |done, total| {
                        let frac = total
                            .filter(|t| *t > 0)
                            .map(|t| (done as f64 / t as f64).min(0.99));
                        ctx.progress(frac, format!("downloaded {} MiB", done >> 20));
                    },
                )
                .await;
                let fetched = match fetched {
                    Ok(f) => f,
                    Err(e) => {
                        pool.abort_partial(&partial);
                        return Err(e.into());
                    }
                };
                pool.commit_partial(&partial, &name)?;
                let v = pool.volume(&name)?;
                node.bus.publish(
                    "pool",
                    format!("/nodes/{}/pools/{}", node.id, pool.name),
                    crate::events::Op::Upsert,
                    serde_json::to_value(pool.info()).unwrap_or_default(),
                );
                Ok(serde_json::json!({ "name": name, "size_bytes": v.file_bytes, "sha256": fetched.sha256, "downloaded_bytes": fetched.bytes }))
            }))
    }
}
