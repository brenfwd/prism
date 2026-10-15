//! Archive health: filling replicas, draining a member, and scrubbing.
//!
//! The BLAKE3 in the replicated index is the trust anchor; every copy is
//! verified against it before it is recorded.

use std::collections::HashSet;

use axum::body::Body;
use axum::http::Request;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::Node;
use crate::archive::{
    ArchiveFileDoc, ArchiveMemberDoc, ArchivePoolDoc, MemberState, Replica, place,
};
use crate::state::Command;

type ByteStream = BoxStream<'static, Result<bytes::Bytes, std::io::Error>>;

/// What a scrub of this node's replicas found.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ScrubReport {
    pub checked: usize,
    pub corrupt: usize,
    pub missing: usize,
    /// Replicas dropped and re-copied from a good one.
    pub repaired: usize,
    /// Bad replicas with no good copy to repair from.
    pub unrecoverable: usize,
    /// Nodes that could not be scrubbed because they were unreachable.
    #[serde(default)]
    pub unreachable: usize,
}

/// What a repair or drain pass did.
#[derive(Debug, Default, Clone, Serialize)]
pub struct MoveReport {
    pub copied: usize,
    pub failed: usize,
}

impl Node {
    /// Sends a request to another member's node.
    pub(super) async fn archive_peer(
        &self,
        target: Uuid,
        req: Request<Body>,
    ) -> anyhow::Result<axum::response::Response> {
        let cluster = self
            .cluster
            .get()
            .ok_or_else(|| anyhow::anyhow!("not clustered"))?;
        let m = self
            .state
            .members()?
            .into_iter()
            .find(|m| m.node_id == target)
            .ok_or_else(|| anyhow::anyhow!("node {target} is not in the cluster"))?;
        let res = crate::cluster::peers::forward_http(cluster, &m, req).await?;
        anyhow::ensure!(
            res.status().is_success(),
            "{} returned {}",
            m.name,
            res.status()
        );
        Ok(res)
    }

    /// A byte stream of the file from one of its replicas other than
    /// `skip`, preferring one on this node.
    async fn archive_replica_stream(
        &self,
        file: &ArchiveFileDoc,
        skip: Option<&Replica>,
    ) -> anyhow::Result<ByteStream> {
        let mut candidates: Vec<&Replica> =
            file.replicas.iter().filter(|r| Some(*r) != skip).collect();
        candidates.sort_by_key(|r| r.node != self.id);
        let mut last = anyhow::anyhow!("no replicas");
        for r in candidates {
            if r.node == self.id {
                let Some(m) = self.archive.member(&r.member) else {
                    continue;
                };
                match m.open_read(&file.path) {
                    Ok(f) => {
                        return Ok(
                            tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(f)).boxed(),
                        );
                    }
                    Err(e) => last = e.into(),
                }
                continue;
            }
            let req = Request::builder()
                .uri(format!(
                    "/api/v1/archive/pools/{}/files/{}?local=1",
                    file.pool, file.path
                ))
                .body(Body::empty())?;
            match self.archive_peer(r.node, req).await {
                Ok(res) => {
                    return Ok(res
                        .into_body()
                        .into_data_stream()
                        .map(|c| c.map_err(std::io::Error::other))
                        .boxed());
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Free bytes per member, from every node's reported capacity.
    pub(super) fn archive_free(&self) -> Vec<(Uuid, String, u64)> {
        self.nodes()
            .into_iter()
            .flat_map(|n| {
                let id = n.id;
                n.archive.into_iter().map(move |c| (id, c.name, c.free))
            })
            .collect()
    }

    /// Picks a member of `pool` for a new copy of `file`: a node that holds
    /// no copy, or with `same_node_ok` any other member.
    pub(super) fn archive_pick_target(
        &self,
        file: &ArchiveFileDoc,
        same_node_ok: bool,
    ) -> anyhow::Result<Option<ArchiveMemberDoc>> {
        let members = self.state.archive_members()?;
        let free = self.archive_free();
        let cands: Vec<(&ArchiveMemberDoc, u64)> = members
            .iter()
            .filter(|m| m.pool == file.pool)
            .filter(|m| {
                !file
                    .replicas
                    .iter()
                    .any(|r| r.node == m.node && (!same_node_ok || r.member == m.name))
            })
            .filter_map(|m| {
                free.iter()
                    .find(|(n, name, _)| *n == m.node && *name == m.name)
                    .map(|(_, _, f)| (m, *f))
            })
            .collect();
        Ok(place(&cands, file.size, Uuid::new_v4().as_u128()).cloned())
    }

    /// Copies `file` to `target`, verifying BLAKE3, and records the replica.
    pub(crate) async fn archive_copy_replica(
        &self,
        file: &ArchiveFileDoc,
        target: &ArchiveMemberDoc,
        skip: Option<&Replica>,
    ) -> anyhow::Result<Replica> {
        let stream = self.archive_replica_stream(file, skip).await?;
        let replica = Replica {
            node: target.node,
            member: target.name.clone(),
        };
        if target.node == self.id {
            let member = self
                .archive
                .member(&target.name)
                .ok_or_else(|| anyhow::anyhow!("member {} is not open here", target.name))?;
            let (_, hash) = member.write(&file.path, stream).await?;
            if hash != file.blake3 {
                let _ = member.delete(&file.path);
                anyhow::bail!("copy of {} failed its BLAKE3 check", file.path);
            }
            self.state
                .apply(Command::AddArchiveReplica {
                    pool: file.pool.clone(),
                    path: file.path.clone(),
                    replica: replica.clone(),
                })
                .await?;
        } else {
            let req = Request::builder()
                .method("PUT")
                .uri(format!(
                    "/api/v1/archive/pools/{}/files/{}?member={}&replica=1",
                    file.pool, file.path, target.name
                ))
                .header("content-type", "application/octet-stream")
                .header("content-length", file.size)
                .body(Body::from_stream(stream))?;
            self.archive_peer(target.node, req).await?;
        }
        Ok(replica)
    }

    /// Receives a replica from a peer: writes it, verifies it against the
    /// index, and records it.
    pub(crate) async fn archive_receive_replica(
        &self,
        pool: &str,
        path: &str,
        member_name: &str,
        body: Body,
    ) -> anyhow::Result<()> {
        let file = self
            .state
            .archive_file(pool, path)?
            .ok_or_else(|| anyhow::anyhow!("{path} is not in {pool}"))?;
        let member = self
            .archive
            .member(member_name)
            .filter(|m| m.pool == pool)
            .ok_or_else(|| anyhow::anyhow!("no member {member_name} of {pool} on this node"))?;
        let (_, hash) = member.write(path, body.into_data_stream()).await?;
        if hash != file.blake3 {
            let _ = member.delete(path);
            anyhow::bail!("received copy of {path} failed its BLAKE3 check");
        }
        self.state
            .apply(Command::AddArchiveReplica {
                pool: pool.into(),
                path: path.into(),
                replica: Replica {
                    node: self.id,
                    member: member_name.into(),
                },
            })
            .await?;
        Ok(())
    }

    /// Deletes one replica's bytes, here or on its node.
    pub(super) async fn archive_delete_bytes(&self, file: &ArchiveFileDoc, r: &Replica) {
        if r.node == self.id {
            if let Some(m) = self.archive.member(&r.member) {
                let _ = m.delete(&file.path);
            }
            return;
        }
        let req = Request::builder()
            .method("DELETE")
            .uri(format!(
                "/api/v1/archive/pools/{}/files/{}?local=1&member={}",
                file.pool, file.path, r.member
            ))
            .body(Body::empty());
        if let Ok(req) = req
            && let Err(e) = self.archive_peer(r.node, req).await
        {
            tracing::warn!("removing {} from {}: {e:#}", file.path, r.node);
        }
    }

    /// Replicas of `file` that sit on members still accepting data.
    /// Replicas on nodes in `expired` (offline past the grace period) do
    /// not count.
    pub(super) fn archive_live_replicas(
        file: &ArchiveFileDoc,
        members: &[ArchiveMemberDoc],
        expired: &HashSet<Uuid>,
    ) -> usize {
        file.replicas
            .iter()
            .filter(|r| !expired.contains(&r.node))
            .filter(|r| {
                members.iter().any(|m| {
                    m.node == r.node && m.name == r.member && m.state == MemberState::Active
                })
            })
            .count()
    }

    /// Brings every file of `pool` up to the pool's replica count.
    pub async fn archive_repair_pool(&self, pool: &str) -> anyhow::Result<MoveReport> {
        let doc: ArchivePoolDoc = self
            .state
            .archive_pools()?
            .into_iter()
            .find(|p| p.name == pool)
            .ok_or_else(|| anyhow::anyhow!("no archive pool {pool}"))?;
        let mut report = MoveReport::default();
        let expired = self.archive_expired_nodes();
        for file in self.state.archive_files(pool, "")? {
            let mut file = file;
            loop {
                let members = self.state.archive_members()?;
                if Self::archive_live_replicas(&file, &members, &expired)
                    >= usize::from(doc.replicas)
                {
                    break;
                }
                let Some(target) = self.archive_pick_target(&file, false)? else {
                    report.failed += 1;
                    break;
                };
                match self.archive_copy_replica(&file, &target, None).await {
                    Ok(r) => {
                        file.replicas.push(r);
                        report.copied += 1;
                    }
                    Err(e) => {
                        tracing::warn!("replicating {}/{}: {e:#}", file.pool, file.path);
                        report.failed += 1;
                        break;
                    }
                }
            }
        }
        Ok(report)
    }

    /// Sets a member's state, replicated to the cluster.
    pub async fn archive_set_member_state(
        &self,
        node: Uuid,
        name: &str,
        state: MemberState,
    ) -> anyhow::Result<()> {
        let mut m = self
            .state
            .archive_members()?
            .into_iter()
            .find(|m| m.node == node && m.name == name)
            .ok_or_else(|| anyhow::anyhow!("no archive member {name} on {node}"))?;
        m.state = state;
        self.state
            .apply(Command::PutArchiveMember { member: m })
            .await?;
        Ok(())
    }

    /// Moves every file off a member (copy, swap in the index, delete the
    /// source), then disables it.
    pub async fn archive_drain(
        &self,
        node: Uuid,
        name: &str,
        ctx: &crate::tasks::TaskCtx,
    ) -> anyhow::Result<MoveReport> {
        self.archive_set_member_state(node, name, MemberState::Draining)
            .await?;
        let pool = self
            .state
            .archive_members()?
            .into_iter()
            .find(|m| m.node == node && m.name == name)
            .map(|m| m.pool)
            .ok_or_else(|| anyhow::anyhow!("no archive member {name}"))?;
        let here = Replica {
            node,
            member: name.into(),
        };
        let files: Vec<ArchiveFileDoc> = self
            .state
            .archive_files(&pool, "")?
            .into_iter()
            .filter(|f| f.replicas.contains(&here))
            .collect();
        let doc = self
            .state
            .archive_pools()?
            .into_iter()
            .find(|p| p.name == pool);
        let need = doc.map_or(1, |p| usize::from(p.replicas));
        let mut report = MoveReport::default();
        for (i, file) in files.iter().enumerate() {
            if ctx.cancel.is_cancelled() {
                anyhow::bail!("drain cancelled");
            }
            ctx.progress(
                Some(i as f64 / files.len() as f64),
                format!("moving {}", file.path),
            );
            let members = self.state.archive_members()?;
            let others = Self::archive_live_replicas(file, &members, &HashSet::new());
            if others < need {
                let target = match self.archive_pick_target(file, false)? {
                    Some(t) => Some(t),
                    None => self.archive_pick_target(file, true)?,
                };
                let Some(target) = target else {
                    report.failed += 1;
                    continue;
                };
                if let Err(e) = self.archive_copy_replica(file, &target, Some(&here)).await {
                    tracing::warn!("draining {}: {e:#}", file.path);
                    report.failed += 1;
                    continue;
                }
                report.copied += 1;
            }
            match self
                .state
                .apply(Command::RemoveArchiveReplica {
                    pool: file.pool.clone(),
                    path: file.path.clone(),
                    replica: here.clone(),
                })
                .await
            {
                Ok(()) => self.archive_delete_bytes(file, &here).await,
                Err(e) => {
                    tracing::warn!("draining {}: {e}", file.path);
                    report.failed += 1;
                }
            }
        }
        if report.failed > 0 {
            // The member stays draining; running the drain again retries.
            anyhow::bail!(
                "drain incomplete: {} file(s) could not be moved off {name} ({} moved); \
                 the member stays draining, fix the cause and drain again",
                report.failed,
                report.copied
            );
        }
        let left = self
            .state
            .archive_files(&pool, "")?
            .iter()
            .filter(|f| f.replicas.contains(&here))
            .count();
        if left > 0 {
            anyhow::bail!("drain incomplete: {left} file(s) still have a replica on {name}");
        }
        self.archive_set_member_state(node, name, MemberState::Disabled)
            .await?;
        Ok(report)
    }

    /// Re-hashes this node's replicas of `pool`; bad ones are dropped from
    /// the index and re-copied from a good replica.
    pub async fn archive_scrub_local(&self, pool: &str) -> anyhow::Result<ScrubReport> {
        let mut report = ScrubReport::default();
        let mut damaged = Vec::new();
        for file in self.state.archive_files(pool, "")? {
            for r in file.replicas.iter().filter(|r| r.node == self.id) {
                let Some(m) = self.archive.member(&r.member) else {
                    continue;
                };
                report.checked += 1;
                match m.hash(&file.path).await {
                    Ok(h) if h == file.blake3 => {}
                    Ok(_) => {
                        report.corrupt += 1;
                        damaged.push((file.clone(), r.clone()));
                    }
                    Err(_) => {
                        report.missing += 1;
                        damaged.push((file.clone(), r.clone()));
                    }
                }
            }
        }
        for (file, r) in damaged {
            let dropped = self
                .state
                .apply(Command::RemoveArchiveReplica {
                    pool: file.pool.clone(),
                    path: file.path.clone(),
                    replica: r.clone(),
                })
                .await;
            if dropped.is_err() {
                report.unrecoverable += 1;
                tracing::error!("{}/{} has no good replica", file.pool, file.path);
                continue;
            }
            self.archive_delete_bytes(&file, &r).await;
            report.repaired += 1;
        }
        if report.repaired > 0 {
            let _ = self.archive_repair_pool(pool).await;
        }
        Ok(report)
    }
}
