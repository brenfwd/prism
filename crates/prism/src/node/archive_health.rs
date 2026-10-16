//! Background archive health: re-replicating after a node has been offline
//! past the grace period, rebalancing fill, and scrubbing.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::Request;
use uuid::Uuid;

use super::Node;
use super::archive_repair::{MoveReport, ScrubReport};
use crate::archive::{ArchiveFileDoc, ArchiveMemberDoc, MemberState, Replica};
use crate::cluster::model::{FEATURE_LEVEL, feature_enabled};
use crate::state::Command;

/// A node offline this long no longer counts toward a file's replicas.
pub const OFFLINE_GRACE: Duration = Duration::from_secs(24 * 3600);
const HEALTH_EVERY: Duration = Duration::from_secs(300);
const REBALANCE_EVERY: Duration = Duration::from_secs(24 * 3600);
const OFFLINE_META: &str = "archive.offline";
/// Feature level that replicates the archive timers.
const TIMERS_LEVEL: u32 = 3;
const SCRUB_META: &str = "archive.scrub_at";
const SCRUB_EVERY: Duration = Duration::from_secs(7 * 24 * 3600);
/// Fill may differ from the pool's mean by this much before files move.
const REBALANCE_MARGIN: f64 = 0.10;
/// Files moved per rebalance pass, to keep the churn gentle.
const REBALANCE_MAX_MOVES: usize = 20;

/// A member's usage, for rebalancing.
#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    pub node: Uuid,
    pub name: String,
    pub used: u64,
    pub total: u64,
}

impl Fill {
    fn ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.used as f64 / self.total as f64
        }
    }
}

/// The heaviest and lightest member (as indexes) when the heaviest sits more
/// than the margin above the pool's mean fill and moving files would help.
pub fn rebalance_pair(fills: &[Fill]) -> Option<(usize, usize)> {
    let total: u64 = fills.iter().map(|f| f.total).sum();
    if fills.len() < 2 || total == 0 {
        return None;
    }
    let mean = fills.iter().map(|f| f.used).sum::<u64>() as f64 / total as f64;
    let by = |a: &&(usize, &Fill), b: &&(usize, &Fill)| a.1.ratio().total_cmp(&b.1.ratio());
    let idx: Vec<(usize, &Fill)> = fills.iter().enumerate().collect();
    let heavy = idx.iter().max_by(by)?.0;
    let light = idx.iter().min_by(by)?.0;
    (fills[heavy].ratio() > mean + REBALANCE_MARGIN && fills[light].ratio() < mean)
        .then_some((heavy, light))
}

/// The largest candidate whose move leaves the target no fuller than the
/// source was.
pub fn pick_file<'a>(
    files: &'a [ArchiveFileDoc],
    src: &Fill,
    dst: &Fill,
) -> Option<&'a ArchiveFileDoc> {
    files
        .iter()
        .filter(|f| dst.total > 0 && (dst.used + f.size) as f64 / dst.total as f64 <= src.ratio())
        .max_by_key(|f| f.size)
}

impl Node {
    /// Nodes that have been unreachable for longer than the grace period.
    /// First-seen-offline times are replicated, so a new leader keeps the
    /// clock, and a daemon restart does not restart it.
    pub(super) async fn archive_expired_nodes(&self) -> HashSet<Uuid> {
        let now = unix_now();
        let before = self.stored_offline();
        let mut offline = before.clone();
        let mut expired = HashSet::new();
        let nodes = self.nodes();
        offline.retain(|id, _| nodes.iter().any(|n| n.id == *id && !n.reachable));
        for n in nodes.iter().filter(|n| !n.reachable) {
            let since = *offline.entry(n.id).or_insert(now);
            if now.saturating_sub(since) >= OFFLINE_GRACE.as_secs() {
                expired.insert(n.id);
            }
        }
        if offline != before {
            self.store_offline(&offline).await;
        }
        expired
    }

    /// Whether the replicated timers are in use: the cluster has reached
    /// the feature level that carries them.
    fn timers_replicated(&self) -> bool {
        let level = self
            .state
            .cluster()
            .ok()
            .flatten()
            .map_or(FEATURE_LEVEL, |c| c.feature_level);
        feature_enabled(level, TIMERS_LEVEL)
    }

    fn stored_offline(&self) -> BTreeMap<Uuid, u64> {
        if self.timers_replicated() {
            return self
                .state
                .archive_timers()
                .unwrap_or_default()
                .offline_since;
        }
        self.state
            .db()
            .meta(OFFLINE_META)
            .ok()
            .flatten()
            .and_then(|m| serde_json::from_str(&m).ok())
            .unwrap_or_default()
    }

    async fn store_offline(&self, offline: &BTreeMap<Uuid, u64>) {
        if self.timers_replicated() {
            let cmd = Command::SetArchiveOffline {
                since: offline.clone(),
            };
            if let Err(e) = self.state.apply(cmd).await {
                tracing::warn!("replicating archive offline clock: {e:#}");
            }
        } else if let Ok(json) = serde_json::to_string(offline)
            && let Err(e) = self.state.db().set_meta(OFFLINE_META, &json)
        {
            tracing::warn!("persisting archive offline clock: {e:#}");
        }
    }

    fn archive_fills(&self, pool: &str) -> anyhow::Result<Vec<Fill>> {
        let members = self.state.archive_members()?;
        let reachable: HashSet<Uuid> = self
            .nodes()
            .into_iter()
            .filter(|n| n.reachable)
            .map(|n| n.id)
            .collect();
        Ok(self
            .nodes()
            .into_iter()
            .filter(|n| reachable.contains(&n.id))
            .flat_map(|n| n.archive.into_iter().map(move |c| (n.id, c)))
            .filter(|(node, c)| {
                c.pool == pool
                    && members.iter().any(|m| {
                        m.node == *node && m.name == c.name && m.state == MemberState::Active
                    })
            })
            .map(|(node, c)| Fill {
                node,
                name: c.name,
                used: c.total.saturating_sub(c.free),
                total: c.total,
            })
            .collect())
    }

    /// Moves files from members above the pool's mean fill to lighter ones.
    pub async fn archive_rebalance_pool(&self, pool: &str) -> anyhow::Result<MoveReport> {
        let mut report = MoveReport::default();
        let mut fills = self.archive_fills(pool)?;
        for _ in 0..REBALANCE_MAX_MOVES {
            let Some((h, l)) = rebalance_pair(&fills) else {
                break;
            };
            let (src, dst) = (fills[h].clone(), fills[l].clone());
            let here = Replica {
                node: src.node,
                member: src.name.clone(),
            };
            let movable: Vec<ArchiveFileDoc> = self
                .state
                .archive_files(pool, "")?
                .into_iter()
                .filter(|f| f.replicas.contains(&here))
                .filter(|f| !f.replicas.iter().any(|r| r.node == dst.node))
                .collect();
            let Some(file) = pick_file(&movable, &src, &dst) else {
                break;
            };
            let Some(target) = self
                .state
                .archive_members()?
                .into_iter()
                .find(|m| m.pool == pool && m.node == dst.node && m.name == dst.name)
            else {
                break;
            };
            match self.archive_move_replica(file, &here, &target).await {
                Ok(()) => {
                    report.copied += 1;
                    fills[h].used = fills[h].used.saturating_sub(file.size);
                    fills[l].used += file.size;
                }
                Err(e) => {
                    tracing::warn!("rebalancing {}: {e:#}", file.path);
                    report.failed += 1;
                    break;
                }
            }
        }
        Ok(report)
    }

    /// Copies a replica to `target`, then swaps it in the index and deletes
    /// the source bytes.
    async fn archive_move_replica(
        &self,
        file: &ArchiveFileDoc,
        from: &Replica,
        target: &ArchiveMemberDoc,
    ) -> anyhow::Result<()> {
        self.archive_copy_replica(file, target, Some(from)).await?;
        self.state
            .apply(Command::RemoveArchiveReplica {
                pool: file.pool.clone(),
                path: file.path.clone(),
                replica: from.clone(),
            })
            .await?;
        self.archive_delete_bytes(file, from).await;
        Ok(())
    }

    /// Scrubs this node's replicas, then asks every other node holding a
    /// member of the pool to scrub its own over the tunnel.
    pub async fn archive_scrub(&self, pool: &str) -> anyhow::Result<ScrubReport> {
        let mut total = self.archive_scrub_local(pool).await?;
        let peers: HashSet<Uuid> = self
            .state
            .archive_members()?
            .into_iter()
            .filter(|m| m.pool == pool && m.node != self.id)
            .map(|m| m.node)
            .collect();
        for peer in peers {
            match self.archive_scrub_peer(peer, pool).await {
                Ok(r) => {
                    total.checked += r.checked;
                    total.corrupt += r.corrupt;
                    total.missing += r.missing;
                    total.repaired += r.repaired;
                    total.unrecoverable += r.unrecoverable;
                    total.unreachable += r.unreachable;
                    total.delete_failed += r.delete_failed;
                }
                Err(e) => {
                    tracing::warn!("scrubbing {pool} on {peer}: {e:#}");
                    total.unreachable += 1;
                }
            }
        }
        Ok(total)
    }

    async fn archive_scrub_peer(&self, peer: Uuid, pool: &str) -> anyhow::Result<ScrubReport> {
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/archive/pools/{pool}/scrub?local=1"))
            .body(Body::empty())?;
        let res = self.archive_peer(peer, req).await?;
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Whether a weekly scrub is due; stamps the time when it is, so the
    /// schedule survives restarts and leader changes.
    async fn archive_scrub_due(&self) -> bool {
        let now = unix_now();
        let replicated = self.timers_replicated();
        let last = if replicated {
            self.state.archive_timers().ok().and_then(|t| t.scrub_at)
        } else {
            self.state
                .db()
                .meta(SCRUB_META)
                .ok()
                .flatten()
                .and_then(|m| m.parse::<u64>().ok())
        };
        // First run: start the clock rather than scrubbing at once.
        let due = last.is_some_and(|l| now.saturating_sub(l) >= SCRUB_EVERY.as_secs());
        if last.is_none() || due {
            if replicated {
                if let Err(e) = self
                    .state
                    .apply(Command::StampArchiveScrub { at: now })
                    .await
                {
                    tracing::warn!("replicating archive scrub time: {e:#}");
                }
            } else if let Err(e) = self.state.db().set_meta(SCRUB_META, &now.to_string()) {
                tracing::warn!("recording archive scrub time: {e:#}");
            }
        }
        due
    }

    /// Every pass: re-replicate after the grace period (on the leader only,
    /// so nodes don't race), rebalance daily, and scrub the whole cluster weekly (leader only).
    pub(super) async fn archive_health_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(HEALTH_EVERY);
        let mut last_rebalance = Instant::now();
        tick.tick().await;
        loop {
            tick.tick().await;
            let leads = self.cluster.get().is_none_or(|c| c.is_leader());
            let pools = self.state.archive_pools().unwrap_or_default();
            if leads {
                let rebalance = last_rebalance.elapsed() >= REBALANCE_EVERY;
                for p in &pools {
                    if let Err(e) = self.archive_repair_pool(&p.name).await {
                        tracing::warn!(pool = %p.name, "archive repair: {e:#}");
                    }
                    if rebalance && let Err(e) = self.archive_rebalance_pool(&p.name).await {
                        tracing::warn!(pool = %p.name, "archive rebalance: {e:#}");
                    }
                }
                if rebalance {
                    last_rebalance = Instant::now();
                }
            }
            if leads && self.archive_scrub_due().await {
                for p in &pools {
                    match self.archive_scrub(&p.name).await {
                        Ok(r) if r.corrupt + r.missing > 0 => {
                            tracing::warn!(pool = %p.name, "scrub: {r:?}");
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!(pool = %p.name, "scrub failed: {e:#}"),
                    }
                }
            }
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::FileKind;

    fn fill(name: &str, used: u64, total: u64) -> Fill {
        Fill {
            node: Uuid::nil(),
            name: name.into(),
            used,
            total,
        }
    }

    fn file(size: u64) -> ArchiveFileDoc {
        ArchiveFileDoc {
            pool: "p".into(),
            path: format!("f{size}"),
            size,
            blake3: String::new(),
            kind: FileKind::Other,
            created: 0,
            replicas: vec![],
        }
    }

    #[test]
    fn balanced_pools_stay_put() {
        assert_eq!(
            rebalance_pair(&[fill("a", 50, 100), fill("b", 55, 100)]),
            None
        );
    }

    #[test]
    fn skewed_pools_move_heavy_to_light() {
        let f = [fill("a", 90, 100), fill("b", 10, 100), fill("c", 50, 100)];
        assert_eq!(rebalance_pair(&f), Some((0, 1)));
    }

    #[test]
    fn picks_the_largest_file_that_does_not_overshoot() {
        let files = [file(10), file(30), file(70)];
        let picked = pick_file(&files, &fill("a", 90, 100), &fill("b", 10, 100)).unwrap();
        assert_eq!(picked.size, 70);
        let picked = pick_file(&files, &fill("a", 90, 100), &fill("b", 40, 100)).unwrap();
        assert_eq!(picked.size, 30);
    }
}
