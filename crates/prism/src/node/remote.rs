//! Peer caches: other nodes' status, VM runtime and tasks, kept current
//! from their event streams so any node can answer for the whole cluster.

use std::collections::HashMap;

use serde::Serialize;
use uuid::Uuid;

use super::{Node, NodeView, VmRuntime, lock};

/// What a peer sends first on `Events.Subscribe`.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct RuntimeSnapshot {
    pub node: NodeView,
    pub vms: HashMap<Uuid, VmRuntime>,
    /// Recent tasks; absent from peers that predate cluster-wide tasks.
    #[serde(default)]
    pub tasks: Vec<crate::tasks::Task>,
}

/// How many recent tasks a runtime snapshot carries.
const SNAPSHOT_TASKS: usize = 500;

impl Node {
    /// This node and every peer heard from.
    pub fn nodes(&self) -> Vec<NodeView> {
        let mut out = vec![self.view()];
        out.extend(lock(&self.remote_nodes).values().cloned());
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Runtime state of this node, for a peer's first event.
    pub fn runtime_snapshot(&self) -> RuntimeSnapshot {
        RuntimeSnapshot {
            node: self.view(),
            vms: lock(&self.runtime).clone(),
            tasks: self.tasks.list(SNAPSHOT_TASKS).unwrap_or_default(),
        }
    }

    /// Replaces what this node knows about a peer: its VMs' runtime state, its tasks and its
    /// status. Called when a peer's event stream sends its snapshot message.
    pub fn apply_remote_snapshot(&self, snap: RuntimeSnapshot) {
        let node = snap.node.id;
        let owned: Vec<Uuid> = self
            .state
            .vms()
            .unwrap_or_default()
            .into_iter()
            .filter(|v| v.node == node)
            .map(|v| v.id)
            .collect();
        let mut rt = lock(&self.remote_runtime);
        for id in &owned {
            rt.remove(id);
            self.guest_addrs.set_peer(*id, HashMap::new());
        }
        for (id, vm) in &snap.vms {
            self.learn_from_peer(*id, vm);
        }
        rt.extend(snap.vms);
        drop(rt);
        self.tasks.apply_remote_snapshot(node, snap.tasks);
        lock(&self.remote_nodes).insert(node, snap.node);
    }

    /// Updates the peer caches from one of a peer's own events.
    pub fn apply_remote_event(&self, ev: &crate::events::Event) {
        let id = ev
            .resource
            .rsplit('/')
            .next()
            .and_then(|s| Uuid::parse_str(s).ok());
        match (ev.ty.as_str(), id) {
            ("vm.state" | "vm.stats", Some(id)) => {
                if let Ok(rt) = serde_json::from_value::<VmRuntime>(ev.payload.clone()) {
                    self.learn_from_peer(id, &rt);
                    lock(&self.remote_runtime).insert(id, rt);
                }
            }
            // Peers announce what they learned about other nodes, us
            // included; this node's own view is never cached as remote.
            ("node.status", Some(id)) if id != self.id => {
                if let Ok(v) = serde_json::from_value::<NodeView>(ev.payload.clone()) {
                    lock(&self.remote_nodes).insert(id, v);
                }
            }
            ("task.progress", _) => {
                if let Ok(t) = serde_json::from_value(ev.payload.clone()) {
                    self.tasks.apply_remote_event(t);
                }
            }
            _ => {}
        }
    }

    /// Hands the IPv6 addresses a peer learned for its VM to the network agent.
    fn learn_from_peer(&self, id: Uuid, rt: &VmRuntime) {
        let addrs = rt
            .learned_v6
            .iter()
            .filter_map(|(mac, a)| Some((mac.parse::<prism_net::MacAddr>().ok()?, a.clone())))
            .collect();
        self.guest_addrs.set_peer(id, addrs);
    }

    /// Marks a peer unreachable in the cache and forgets the addresses it learned.
    pub fn forget_remote(&self, node: Uuid) {
        for vm in self.state.vms().unwrap_or_default() {
            if vm.node == node {
                self.guest_addrs.set_peer(vm.id, HashMap::new());
            }
        }
        if let Some(v) = lock(&self.remote_nodes).get_mut(&node) {
            v.libvirt_connected = false;
            v.reachable = false;
        }
    }
}
