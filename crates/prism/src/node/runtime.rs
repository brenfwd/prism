//! This node's VM runtime state: reading it from libvirt, caching it and
//! publishing changes.

use std::sync::Arc;
use std::time::Duration;

use prism_libvirt::proto::{error_codes, memory_stat};
use uuid::Uuid;

use super::{MemoryStats, Node, VmRuntime, VmState, domain_ref, lock};
use crate::events::Op;
use crate::spec::VmSpec;

const STATS_INTERVAL: Duration = Duration::from_secs(5);

impl Node {
    pub(crate) fn runtime(&self, id: Uuid) -> VmRuntime {
        lock(&self.runtime).get(&id).cloned().unwrap_or_default()
    }

    pub(crate) fn update_runtime(&self, id: Uuid, f: impl FnOnce(&mut VmRuntime)) {
        let mut rt = self.runtime(id);
        f(&mut rt);
        self.set_runtime(id, rt);
    }

    pub(super) fn set_runtime(&self, id: Uuid, rt: VmRuntime) {
        let payload = serde_json::to_value(&rt).unwrap_or_default();
        let ty = {
            let mut map = lock(&self.runtime);
            let ty = match map.get(&id) {
                Some(p) if *p == rt => return,
                Some(p) if p.state == rt.state && p.pending_restart == rt.pending_restart => {
                    "vm.stats"
                }
                _ => "vm.state",
            };
            map.insert(id, rt);
            ty
        };
        self.bus
            .publish(ty, format!("/vms/{id}"), Op::Upsert, payload);
    }

    /// Re-reads one VM's runtime state from libvirt.
    ///
    /// The libvirt calls take a while, so the result is applied to the state as it is when
    /// they return: other tasks (the guest address loop, the balloon, the agent events) change
    /// their own fields meanwhile, and a copy taken before the calls would undo those.
    pub(crate) async fn refresh(&self, spec: &VmSpec) {
        let Ok(c) = self.client() else { return };
        let dom = domain_ref(spec);
        match c.get_info(&dom).await {
            Ok(info) => {
                let state = VmState::from(info.state);
                let memory_stats = if state.is_active() {
                    c.memory_stats(&dom)
                        .await
                        .map(|s| parse_mem_stats(&s))
                        .unwrap_or_default()
                } else {
                    MemoryStats::default()
                };
                self.update_runtime(spec.id, |rt| {
                    rt.state = state;
                    rt.vcpus = info.nr_virt_cpu;
                    rt.memory_kib = info.memory_kib;
                    rt.max_memory_kib = info.max_mem_kib;
                    rt.cpu_time_ns = info.cpu_time_ns;
                    rt.memory_stats = memory_stats;
                    if !state.is_active() {
                        rt.pending_restart = false;
                        rt.agent = false;
                        rt.balloon = None;
                        rt.learned_v6.clear();
                    }
                });
            }
            Err(e) if e.remote_code() == Some(error_codes::NO_DOMAIN) => {
                self.set_runtime(
                    spec.id,
                    VmRuntime {
                        state: VmState::Missing,
                        ..VmRuntime::default()
                    },
                );
            }
            Err(e) => tracing::warn!(vm = %spec.name, "get_info failed: {e}"),
        }
    }

    pub(crate) async fn refresh_id(&self, id: Uuid) {
        if let Ok(Some(spec)) = self.state.vm(id) {
            self.refresh(&spec).await;
        }
    }

    pub(super) async fn refresh_all(&self) {
        if let Ok(specs) = self.state.vms() {
            for spec in specs.iter().filter(|s| s.node == self.id) {
                self.refresh(spec).await;
            }
        }
    }

    pub(super) async fn stats_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(STATS_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut ticks: u64 = 0;
        loop {
            tick.tick().await;
            let Ok(c) = self.client() else { continue };
            if let Ok(free) = c.free_memory().await {
                let changed = {
                    let mut h = lock(&self.host);
                    let changed = h.free_memory_bytes.abs_diff(free) > 16 << 20;
                    h.free_memory_bytes = free;
                    changed
                };
                if changed {
                    self.publish_node();
                }
            }
            let active: Vec<Uuid> = lock(&self.runtime)
                .iter()
                .filter(|(_, r)| r.state.is_active())
                .map(|(id, _)| *id)
                .collect();
            for id in active {
                self.refresh_id(id).await;
            }
            // Periodic full reconcile in a cluster (every 30 s).
            ticks += 1;
            if ticks.is_multiple_of(6) && self.state.clustered() {
                self.reconcile(&c).await;
            }
        }
    }
}

fn parse_mem_stats(stats: &[prism_libvirt::MemoryStat]) -> MemoryStats {
    let mut m = MemoryStats::default();
    for &prism_libvirt::MemoryStat { tag, value: val } in stats {
        match tag {
            memory_stat::ACTUAL_BALLOON => m.actual_kib = Some(val),
            memory_stat::UNUSED => m.unused_kib = Some(val),
            memory_stat::AVAILABLE => m.available_kib = Some(val),
            memory_stat::USABLE => m.usable_kib = Some(val),
            memory_stat::DISK_CACHES => m.disk_caches_kib = Some(val),
            memory_stat::RSS => m.rss_kib = Some(val),
            memory_stat::MAJOR_FAULT => m.major_faults = Some(val),
            memory_stat::SWAP_IN => m.swap_in_kib = Some(val),
            _ => {}
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_stats() {
        let stat = |tag, value| prism_libvirt::MemoryStat { tag, value };
        let m = parse_mem_stats(&[
            stat(memory_stat::ACTUAL_BALLOON, 1024),
            stat(memory_stat::USABLE, 512),
            stat(memory_stat::MAJOR_FAULT, 3),
            stat(99, 1),
        ]);
        assert_eq!(m.actual_kib, Some(1024));
        assert_eq!(m.usable_kib, Some(512));
        assert_eq!(m.major_faults, Some(3));
        assert_eq!(m.rss_kib, None);
    }
}
