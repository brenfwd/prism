//! Moving files into and out of archive pools, locally or through the
//! member node that holds them.

use uuid::Uuid;

use super::Node;
use super::backups::unix_now;
use crate::archive::{ArchiveFileDoc, ArchiveMemberDoc, FileKind, Replica, place};
use crate::state::Command;

impl Node {
    /// Stores a local file in an archive pool: on a member here, or
    /// streamed to the chosen member's node.
    pub(crate) async fn archive_store_file(
        &self,
        pool: &str,
        path: &str,
        file: std::fs::File,
    ) -> anyhow::Result<ArchiveFileDoc> {
        let size = file.metadata()?.len();
        let members = self.state.archive_members()?;
        let caps: Vec<(Uuid, crate::archive::MemberCapacity)> = self
            .nodes()
            .into_iter()
            .flat_map(|n| {
                let id = n.id;
                n.archive.into_iter().map(move |c| (id, c))
            })
            .collect();
        let cands: Vec<(&ArchiveMemberDoc, u64)> = members
            .iter()
            .filter(|m| m.pool == pool)
            .filter_map(|m| {
                caps.iter()
                    .find(|(n, c)| *n == m.node && c.name == m.name)
                    .map(|(_, c)| (m, c.free))
            })
            .collect();
        let chosen = place(&cands, size, Uuid::new_v4().as_u128()).ok_or_else(|| {
            anyhow::anyhow!("no member of archive pool {pool} has room for {size} bytes")
        })?;
        let body = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file));
        if chosen.node == self.id {
            let member = self
                .archive
                .member(&chosen.name)
                .ok_or_else(|| anyhow::anyhow!("member {} is not open here", chosen.name))?;
            let (size, blake3) = member.write(path, body).await?;
            let doc = ArchiveFileDoc {
                pool: pool.into(),
                path: path.into(),
                size,
                blake3,
                kind: FileKind::Backup,
                created: unix_now(),
                replicas: vec![Replica {
                    node: self.id,
                    member: chosen.name.clone(),
                }],
            };
            if let Err(e) = self
                .state
                .apply(Command::PutArchiveFile {
                    file: doc.clone(),
                    create: true,
                })
                .await
            {
                let _ = member.delete(path);
                anyhow::bail!("{e}");
            }
            return Ok(doc);
        }
        let cluster = self
            .cluster
            .get()
            .ok_or_else(|| anyhow::anyhow!("not clustered"))?;
        let m = self
            .state
            .members()?
            .into_iter()
            .find(|m| m.node_id == chosen.node)
            .ok_or_else(|| anyhow::anyhow!("member node left the cluster"))?;
        let req = axum::http::Request::builder()
            .method("PUT")
            .uri(format!(
                "/api/v1/archive/pools/{pool}/files/{path}?member={}",
                chosen.name
            ))
            .header("content-type", "application/octet-stream")
            .header("content-length", size)
            .body(axum::body::Body::from_stream(body))?;
        let res = crate::cluster::peers::forward_http(cluster, &m, req).await?;
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await?;
        anyhow::ensure!(
            status.is_success(),
            "{}: {}",
            m.name,
            String::from_utf8_lossy(&bytes)
        );
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Fetches an archive file into a hidden partial file in `pool`,
    /// verifying its BLAKE3. Returns the partial name.
    pub(super) async fn archive_fetch(
        &self,
        file: &ArchiveFileDoc,
        pool: &crate::storage::Pool,
    ) -> anyhow::Result<String> {
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;
        let (partial, out) = pool.begin_partial(&format!("restore-{}", Uuid::new_v4().simple()))?;
        let mut out = tokio::fs::File::from_std(out);
        let mut hasher = blake3::Hasher::new();
        let local = file
            .replicas
            .iter()
            .find(|r| r.node == self.id)
            .and_then(|r| self.archive.member(&r.member));
        let res: anyhow::Result<()> = async {
            if let Some(m) = local {
                let mut s = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(
                    m.open_read(&file.path)?,
                ));
                while let Some(chunk) = s.next().await {
                    let chunk = chunk?;
                    hasher.update(&chunk);
                    out.write_all(&chunk).await?;
                }
            } else {
                let cluster = self
                    .cluster
                    .get()
                    .ok_or_else(|| anyhow::anyhow!("not clustered"))?;
                let holder = file
                    .replicas
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("no replicas"))?;
                let m = self
                    .state
                    .members()?
                    .into_iter()
                    .find(|m| m.node_id == holder.node)
                    .ok_or_else(|| anyhow::anyhow!("replica holder left the cluster"))?;
                let req = axum::http::Request::builder()
                    .uri(format!(
                        "/api/v1/archive/pools/{}/files/{}?local=1",
                        file.pool, file.path
                    ))
                    .body(axum::body::Body::empty())?;
                let res = crate::cluster::peers::forward_http(cluster, &m, req).await?;
                anyhow::ensure!(
                    res.status().is_success(),
                    "{} returned {}",
                    m.name,
                    res.status()
                );
                let mut s = res.into_body().into_data_stream();
                while let Some(chunk) = s.next().await {
                    let chunk = chunk?;
                    hasher.update(&chunk);
                    out.write_all(&chunk).await?;
                }
            }
            out.sync_all().await?;
            anyhow::ensure!(
                hasher.finalize().to_hex().as_str() == file.blake3,
                "{} failed its BLAKE3 check",
                file.path
            );
            Ok(())
        }
        .await;
        if let Err(e) = res {
            pool.abort_partial(&partial);
            return Err(e);
        }
        Ok(partial)
    }
}
