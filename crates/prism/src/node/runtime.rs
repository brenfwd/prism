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
    pub(crate) async fn refresh(&self, spec: &VmSpec) {
        let Ok(c) = self.client() else { return };
        let dom = domain_ref(spec);
        let prev = self.runtime(spec.id);
        let rt = match c.get_info(&dom).await {
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
                VmRuntime {
                    state,
                    vcpus: info.nr_virt_cpu,
                    memory_kib: info.memory_kib,
                    max_memory_kib: info.max_mem_kib,
                    cpu_time_ns: info.cpu_time_ns,
                    memory_stats,
                    pending_restart: prev.pending_restart && state.is_active(),
                    agent: prev.agent && state.is_active(),
                    balloon: prev.balloon.filter(|_| state.is_active()),
                }
            }
            Err(e) if e.remote_code() == Some(error_codes::NO_DOMAIN) => VmRuntime {
                state: VmState::Missing,
                ..VmRuntime::default()
            },
            Err(e) => {
                tracing::warn!(vm = %spec.name, "get_info failed: {e}");
                return;
            }
        };
        self.set_runtime(spec.id, rt);
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
