//! The ISO cache inside a pool: archive ISOs fetched for CD-ROM use are kept
//! as `archive-<hash>.iso` volumes, evicted least-recently-used once nothing
//! references them.

use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::{AtFlags, Timespec, Timestamps};

use super::{Pool, StorageError};

/// Name prefix of cached archive ISOs.
const PREFIX: &str = "archive-";
/// A volume used more recently than this is never evicted, so one that was
/// just fetched survives until its VM references it.
const MIN_AGE_SECS: i64 = 600;

pub fn is_cached_iso(name: &str) -> bool {
    name.starts_with(PREFIX)
        && std::path::Path::new(name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("iso"))
}

/// One cached ISO, as the eviction planner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    pub name: String,
    pub size: u64,
    /// Seconds since the epoch.
    pub last_used: i64,
    /// VMs whose disks use it.
    pub refs: usize,
}

/// Names to evict so that the cache stays within `cap` after adding
/// `incoming` bytes and the pool has `free` room for them. Oldest
/// unreferenced entries go first; referenced and fresh ones never do.
pub fn plan_eviction(
    entries: &[CacheEntry],
    incoming: u64,
    cap: u64,
    free: u64,
    now: i64,
) -> Vec<String> {
    let mut total: u64 = entries.iter().map(|e| e.size).sum();
    let mut room = free;
    let mut order: Vec<&CacheEntry> = entries
        .iter()
        .filter(|e| e.refs == 0 && now - e.last_used >= MIN_AGE_SECS)
        .collect();
    order.sort_by_key(|e| e.last_used);
    let mut out = Vec::new();
    for e in order {
        if total.saturating_add(incoming) <= cap && room >= incoming {
            break;
        }
        total = total.saturating_sub(e.size);
        room = room.saturating_add(e.size);
        out.push(e.name.clone());
    }
    out
}

impl Pool {
    /// Marks a volume as just used (its mtime is the LRU clock).
    pub fn touch_volume(&self, name: &str) -> Result<(), StorageError> {
        Self::check(name)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let t = Timespec {
            tv_sec: now,
            tv_nsec: 0,
        };
        rustix::fs::utimensat(
            &self.root,
            name,
            &Timestamps {
                last_access: t,
                last_modification: t,
            },
            AtFlags::SYMLINK_NOFOLLOW,
        )?;
        Ok(())
    }

    /// When a volume was last used, in seconds since the epoch.
    pub fn volume_last_used(&self, name: &str) -> Result<i64, StorageError> {
        let fd = self.open_existing(name)?;
        Ok(rustix::fs::fstat(&fd)?.st_mtime as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, size: u64, last_used: i64, refs: usize) -> CacheEntry {
        CacheEntry {
            name: name.into(),
            size,
            last_used,
            refs,
        }
    }

    #[test]
    fn evicts_oldest_unreferenced_first() {
        let entries = [e("a", 10, 100, 0), e("b", 10, 200, 0), e("c", 10, 50, 1)];
        // 30 cached, cap 30, 10 incoming: one eviction, the oldest free one.
        assert_eq!(plan_eviction(&entries, 10, 30, u64::MAX, 10_000), ["a"]);
    }

    #[test]
    fn referenced_and_fresh_entries_are_kept() {
        let entries = [e("a", 10, 9_900, 0), e("b", 10, 1, 2)];
        let none: [String; 0] = [];
        assert_eq!(plan_eviction(&entries, 10, 5, u64::MAX, 10_000), none);
    }

    #[test]
    fn frees_disk_space_even_under_the_cap() {
        let entries = [e("a", 10, 1, 0), e("b", 10, 2, 0)];
        assert_eq!(plan_eviction(&entries, 15, 1000, 6, 10_000), ["a"]);
    }

    #[test]
    fn nothing_to_do_within_limits() {
        let entries = [e("a", 10, 1, 0)];
        let none: [String; 0] = [];
        assert_eq!(plan_eviction(&entries, 0, 100, 0, 10_000), none);
    }

    #[test]
    fn recognises_cached_names() {
        assert!(is_cached_iso("archive-0123abcd.iso"));
        assert!(!is_cached_iso("ubuntu.iso"));
    }
}
