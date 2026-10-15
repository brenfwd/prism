//! Auto-ballooning: a per-host loop that moves memory between `Auto` VMs
//! based on host pressure (PSI, `MemAvailable`) and guest balloon stats.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use uuid::Uuid;

use super::{MemoryStats, Node, VmState, domain_ref, lock};
use crate::spec::{BalloonMode, MemorySpec, VmSpec};

const TICK: Duration = Duration::from_secs(10);
/// No direction reversal for a VM within this window.
const HYSTERESIS: Duration = Duration::from_secs(30);
const MAX_STEP_KIB: u64 = 256 * 1024;
/// Headroom above guest in-use memory.
const HEADROOM: f64 = 0.20;
/// Major faults per tick above which reclaiming stops for that VM.
const FAULT_SPIKE: u64 = 200;
/// Ticks a VM may miss its target before being flagged unresponsive.
const UNRESPONSIVE_TICKS: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PressureLevel {
    Green,
    Yellow,
    Red,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct HostPressure {
    pub level: PressureLevel,
    /// PSI `some avg10` and `full avg10`, percent; `None` without PSI.
    pub some_avg10: Option<f64>,
    pub full_avg10: Option<f64>,
    pub mem_available_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct BalloonStatus {
    pub target_kib: u64,
    pub reason: String,
    pub unresponsive: bool,
}

/// Classifies host memory pressure.
pub fn classify(some: Option<f64>, full: Option<f64>, avail_pct: f64) -> PressureLevel {
    let some = some.unwrap_or(0.0);
    let full = full.unwrap_or(0.0);
    if full > 2.0 || some > 10.0 || avail_pct < 5.0 {
        PressureLevel::Red
    } else if some >= 1.0 || avail_pct < 15.0 {
        PressureLevel::Yellow
    } else {
        PressureLevel::Green
    }
}

/// Parses `/proc/pressure/memory`.
fn parse_psi(text: &str) -> (Option<f64>, Option<f64>) {
    let field = |kind: &str| {
        text.lines()
            .find(|l| l.starts_with(kind))
            .and_then(|l| l.split_whitespace().find_map(|f| f.strip_prefix("avg10=")))
            .and_then(|v| v.parse().ok())
    };
    (field("some"), field("full"))
}

/// Parses `MemTotal` and `MemAvailable` (KiB) from `/proc/meminfo`.
fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let get = |key: &str| {
        text.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
    };
    Some((get("MemTotal:")?, get("MemAvailable:")?))
}

pub fn read_host_pressure() -> Option<HostPressure> {
    let (total, avail) = parse_meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?)?;
    let (some, full) =
        std::fs::read_to_string("/proc/pressure/memory").map_or((None, None), |t| parse_psi(&t));
    let pct = if total == 0 {
        100.0
    } else {
        avail as f64 * 100.0 / total as f64
    };
    Some(HostPressure {
        level: classify(some, full, pct),
        some_avg10: some,
        full_avg10: full,
        mem_available_pct: pct,
    })
}

/// Inputs to one VM's balloon decision.
#[derive(Debug, Clone)]
pub struct VmInputs {
    pub memory: MemorySpec,
    pub current_kib: u64,
    pub stats: MemoryStats,
    pub fault_delta: u64,
    /// The last change and its direction (+1 grow, -1 shrink).
    pub last_change: Option<(Instant, i8)>,
}

/// The new balloon size for one VM, or `None` to leave it.
pub fn decide(level: PressureLevel, v: &VmInputs, now: Instant) -> Option<(u64, &'static str)> {
    let min = v.memory.min_mib * 1024;
    let max = v.memory.max_mib * 1024;
    let (avail, usable) = (v.stats.available_kib?, v.stats.usable_kib?);
    let in_use = avail.saturating_sub(usable);
    let target = ((in_use as f64 * (1.0 + HEADROOM)) as u64).clamp(min, max);
    let step = MAX_STEP_KIB.min(max / 20).max(4096);
    let guest_tight = usable * 10 < avail;
    let blocked = |dir: i8| {
        v.last_change
            .is_some_and(|(t, d)| d != dir && now.duration_since(t) < HYSTERESIS)
    };

    let (want, reason) = match level {
        PressureLevel::Red => (min, "host memory critical"),
        PressureLevel::Yellow if v.current_kib > target => (target, "host memory pressure"),
        _ if guest_tight => (
            (v.current_kib + v.current_kib / 10).min(max),
            "guest under pressure",
        ),
        _ if v.current_kib < target => (target, "guest needs memory"),
        _ => return None,
    };
    let next = match want.cmp(&v.current_kib) {
        std::cmp::Ordering::Greater if !blocked(1) => (v.current_kib + step).min(want),
        std::cmp::Ordering::Less
            if !(blocked(-1) || v.fault_delta > FAULT_SPIKE || guest_tight) =>
        {
            v.current_kib.saturating_sub(step).max(want)
        }
        _ => return None,
    };
    (next != v.current_kib).then_some((next, reason))
}

#[derive(Debug, Default)]
struct VmTrack {
    last_faults: Option<u64>,
    last_change: Option<(Instant, i8)>,
    pending_target: Option<u64>,
    missed: u32,
}

impl Node {
    pub(crate) fn balloon_refuses_start(&self, _spec: &VmSpec) -> bool {
        lock(&self.pressure)
            .as_ref()
            .is_some_and(|p| p.level == PressureLevel::Red)
    }

    pub(super) async fn balloon_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut track: HashMap<Uuid, VmTrack> = HashMap::new();
        loop {
            tick.tick().await;
            let pressure = read_host_pressure();
            let changed = {
                let mut p = lock(&self.pressure);
                let changed = p.as_ref().map(|x| x.level) != pressure.as_ref().map(|x| x.level);
                p.clone_from(&pressure);
                changed
            };
            if changed {
                if let Some(p) = &pressure
                    && p.level == PressureLevel::Red
                {
                    tracing::warn!(?p, "host memory pressure is critical");
                }
                self.publish_node();
            }
            let Some(pressure) = pressure else { continue };
            let Ok(c) = self.client() else { continue };
            let Ok(specs) = self.state.vms() else {
                continue;
            };
            let now = Instant::now();
            for spec in specs
                .iter()
                .filter(|s| s.node == self.id && s.memory.balloon == BalloonMode::Auto)
            {
                let rt = self.runtime(spec.id);
                if rt.state != VmState::Running {
                    track.remove(&spec.id);
                    continue;
                }
                let t = track.entry(spec.id).or_default();
                let faults = rt.memory_stats.major_faults.unwrap_or(0);
                let fault_delta = t.last_faults.map_or(0, |f| faults.saturating_sub(f));
                t.last_faults = Some(faults);
                let current = rt.memory_stats.actual_kib.unwrap_or(rt.memory_kib);
                // Track whether the guest reached the last target.
                if let Some(target) = t.pending_target {
                    if current.abs_diff(target) <= 4096 {
                        t.pending_target = None;
                        t.missed = 0;
                    } else {
                        t.missed += 1;
                    }
                }
                let inputs = VmInputs {
                    memory: spec.memory,
                    current_kib: current,
                    stats: rt.memory_stats.clone(),
                    fault_delta,
                    last_change: t.last_change,
                };
                let unresponsive = t.missed >= UNRESPONSIVE_TICKS;
                let Some((target, reason)) = decide(pressure.level, &inputs, now) else {
                    if unresponsive != rt.balloon.as_ref().is_some_and(|b| b.unresponsive) {
                        self.update_runtime(spec.id, |r| {
                            r.balloon = Some(BalloonStatus {
                                target_kib: current,
                                reason: "steady".into(),
                                unresponsive,
                            });
                        });
                    }
                    continue;
                };
                let dir = if target > current { 1 } else { -1 };
                match c
                    .set_memory(
                        &domain_ref(spec),
                        target,
                        prism_libvirt::proto::flags::DOMAIN_AFFECT_LIVE,
                    )
                    .await
                {
                    Ok(()) => {
                        tracing::debug!(vm = %spec.name, current, target, reason, "balloon");
                        t.last_change = Some((now, dir));
                        t.pending_target = Some(target);
                        self.update_runtime(spec.id, |r| {
                            r.balloon = Some(BalloonStatus {
                                target_kib: target,
                                reason: reason.into(),
                                unresponsive,
                            });
                        });
                    }
                    Err(e) => tracing::warn!(vm = %spec.name, "setting balloon: {e}"),
                }
            }
            track.retain(|id, _| specs.iter().any(|s| s.id == *id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(current_mib: u64, avail_mib: u64, usable_mib: u64) -> VmInputs {
        VmInputs {
            memory: MemorySpec {
                max_mib: 4096,
                min_mib: 512,
                target_mib: 4096,
                balloon: BalloonMode::Auto,
            },
            current_kib: current_mib * 1024,
            stats: MemoryStats {
                available_kib: Some(avail_mib * 1024),
                usable_kib: Some(usable_mib * 1024),
                ..MemoryStats::default()
            },
            fault_delta: 0,
            last_change: None,
        }
    }

    #[test]
    fn classifies() {
        assert_eq!(classify(Some(0.1), Some(0.0), 50.0), PressureLevel::Green);
        assert_eq!(classify(Some(2.0), Some(0.0), 50.0), PressureLevel::Yellow);
        assert_eq!(classify(None, None, 10.0), PressureLevel::Yellow);
        assert_eq!(classify(Some(0.0), Some(3.0), 50.0), PressureLevel::Red);
        assert_eq!(classify(None, None, 3.0), PressureLevel::Red);
    }

    #[test]
    fn parses_proc_files() {
        let psi = "some avg10=1.50 avg60=0.10 avg300=0.00 total=1\nfull avg10=0.25 avg60=0.00 avg300=0.00 total=1\n";
        assert_eq!(parse_psi(psi), (Some(1.5), Some(0.25)));
        let mi = "MemTotal:       16000000 kB\nMemFree:  1 kB\nMemAvailable:    8000000 kB\n";
        assert_eq!(parse_meminfo(mi), Some((16_000_000, 8_000_000)));
    }

    #[test]
    fn yellow_reclaims_in_steps() {
        let now = Instant::now();
        // 4 GiB balloon, guest uses ~1 GiB: target ~1.2 GiB.
        let (next, _) = decide(PressureLevel::Yellow, &inputs(4096, 4000, 3000), now).unwrap();
        // Step is min(256 MiB, 5% of max) = 209_715 KiB.
        assert_eq!(next, 4096 * 1024 - 209_715, "one bounded step down");
        assert!(next < 4096 * 1024);
    }

    #[test]
    fn green_only_grows() {
        let now = Instant::now();
        assert!(decide(PressureLevel::Green, &inputs(4096, 4000, 3000), now).is_none());
        // Guest nearly out of memory: grow.
        let (next, reason) = decide(PressureLevel::Green, &inputs(1024, 1000, 50), now).unwrap();
        assert!(next > 1024 * 1024);
        assert_eq!(reason, "guest under pressure");
    }

    #[test]
    fn red_goes_to_floor_but_respects_faults_and_hysteresis() {
        let now = Instant::now();
        assert!(decide(PressureLevel::Red, &inputs(2048, 2000, 1500), now).is_some());
        let mut v = inputs(2048, 2000, 1500);
        v.fault_delta = 1000;
        assert!(
            decide(PressureLevel::Red, &v, now).is_none(),
            "fault spike stops reclaim"
        );
        let mut v = inputs(2048, 2000, 1500);
        v.last_change = Some((now, 1));
        assert!(
            decide(PressureLevel::Red, &v, now).is_none(),
            "no reversal within hysteresis"
        );
    }

    #[test]
    fn needs_stats() {
        let mut v = inputs(2048, 2000, 1500);
        v.stats.usable_kib = None;
        assert!(decide(PressureLevel::Red, &v, Instant::now()).is_none());
    }
}
