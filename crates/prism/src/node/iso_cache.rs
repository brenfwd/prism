//! Keeps each pool's cache of archive ISOs within bounds.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::Node;
use crate::storage::Pool;
use crate::storage::cache::{CacheEntry, is_cached_iso, plan_eviction};

/// Most bytes of cached archive ISOs per pool.
const CACHE_CAP: u64 = 32 << 30;
const SWEEP_EVERY: Duration = Duration::from_secs(3600);

impl Node {
    fn iso_cache_entries(&self, pool: &Pool) -> Vec<CacheEntry> {
        let Ok(volumes) = pool.volumes() else {
            return Vec::new();
        };
        volumes
            .into_iter()
            .filter(|v| is_cached_iso(&v.name))
            .map(|v| CacheEntry {
                last_used: pool.volume_last_used(&v.name).unwrap_or(0),
                refs: self
                    .volume_users(&pool.name, &v.name)
                    .map_or(1, |u| u.len()),
                size: v.file_bytes,
                name: v.name,
            })
            .collect()
    }

    /// Evicts least-recently-used cached ISOs nothing references, so that
    /// `incoming` more bytes fit in the cap and on disk.
    pub(super) fn iso_cache_make_room(&self, pool: &Pool, incoming: u64) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let entries = self.iso_cache_entries(pool);
        let free = pool.info().available_bytes;
        for name in plan_eviction(&entries, incoming, CACHE_CAP, free, now) {
            match pool.delete_volume(&name) {
                Ok(()) => tracing::info!(pool = %pool.name, %name, "evicted cached ISO"),
                Err(e) => tracing::warn!(pool = %pool.name, %name, "evicting cached ISO: {e}"),
            }
        }
    }

    pub(super) async fn iso_cache_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        loop {
            tick.tick().await;
            for pool in self.storage.list() {
                self.iso_cache_make_room(&pool, 0);
            }
        }
    }
}
