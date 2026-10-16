//! Archive timers kept in the replicated state, so a new leader continues
//! the schedule of the old one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Collection holding the timer document.
pub const TIMERS: &str = "timer";
/// Key of the archive timer document.
pub const ARCHIVE_TIMERS_ID: &str = "archive";

/// When the last weekly scrub started and when each node was first seen
/// offline, both in Unix seconds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveTimers {
    #[serde(default)]
    pub scrub_at: Option<u64>,
    #[serde(default)]
    pub offline_since: BTreeMap<Uuid, u64>,
}
