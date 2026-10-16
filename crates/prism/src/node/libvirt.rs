//! The libvirt connection: supervision, domain events and reconciling
//! the domains defined here with committed state.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use prism_libvirt::proto::{event_ids, flags};
use prism_libvirt::{Client, Domain, DomainEvent, LifecycleEvent};
use tokio::sync::broadcast;
use uuid::Uuid;

use super::{Action, HostInfo, Node, NodeError, Result, VmState, apparmor, lock, not_running};

const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Formats libvirt's numeric version, `10000000` for example, as `10.0.0`.
pub(crate) fn format_version(v: u64) -> String {
    format!("{}.{}.{}", v / 1_000_000, v / 1000 % 1000, v % 1000)
}

impl Node {
    /// Waits for a domain event matching `f`, up to `timeout`.
    pub(crate) async fn wait_event<T>(
        &self,
        rx: &mut broadcast::Receiver<DomainEvent>,
        timeout: Duration,
        mut f: impl FnMut(&DomainEvent) -> Option<T>,
    ) -> Result<T> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Err(_) => {
                    return Err(NodeError::Internal(anyhow::anyhow!(
                        "timed out waiting for libvirt"
                    )));
                }
                Ok(Ok(ev)) => {
                    if let Some(v) = f(&ev) {
                        return Ok(v);
                    }
                }
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(broadcast::error::RecvError::Closed)) => return Err(NodeError::Unavailable),
            }
        }
    }

    pub(crate) fn subscribe_domain_events(&self) -> broadcast::Receiver<DomainEvent> {
        self.domain_events.subscribe()
    }

    pub(super) async fn supervise(self: Arc<Self>) {
        let socket = self.cfg.libvirt_socket();
        let mut backoff = Duration::from_secs(1);
        loop {
            match Client::connect(&socket, &self.cfg.libvirt.uri).await {
                Ok(c) => {
                    backoff = Duration::from_secs(1);
                    if let Err(e) = self.on_connect(&c).await {
                        tracing::error!("libvirt setup failed: {e}");
                    } else {
                        tracing::info!("connected to libvirt at {}", socket.display());
                        c.closed().await;
                        tracing::warn!("libvirt connection lost");
                    }
                    self.libvirt.send_replace(None);
                    self.publish_node();
                }
                Err(e) => {
                    tracing::warn!("cannot connect to libvirt at {}: {e}", socket.display());
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(RECONNECT_MAX);
        }
    }

    pub(super) async fn on_connect(self: &Arc<Self>, c: &Client) -> Result<()> {
        let mut events = c.events();
        for id in [
            event_ids::LIFECYCLE,
            event_ids::BALLOON_CHANGE,
            event_ids::BLOCK_JOB_2,
            event_ids::JOB_COMPLETED,
            event_ids::DEVICE_REMOVED,
            event_ids::AGENT_LIFECYCLE,
            event_ids::MIGRATION_ITERATION,
        ] {
            c.register_event(id).await?;
        }
        let n = c.node_info().await?;
        let version = c.lib_version().await?;
        *lock(&self.host) = HostInfo {
            hostname: c.hostname().await?,
            libvirt_version: format_version(version),
            libvirt_version_num: version,
            free_memory_bytes: c.free_memory().await?,
            cpu_model: n.model,
            cpus: n.cpus,
            memory_kib: n.memory_kib,
        };
        apparmor::allow_migration_sockets();
        self.sync_pool_apparmor();
        self.libvirt.send_replace(Some(c.clone()));

        let node = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(ev) => node.on_domain_event(ev).await,
                    Err(broadcast::error::RecvError::Lagged(_)) => node.refresh_all().await,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });

        self.reconcile(c).await;
        self.publish_node();
        if self
            .autostart_pending
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.autostart().await;
        }
        Ok(())
    }

    pub(super) async fn on_domain_event(&self, ev: DomainEvent) {
        let id = Uuid::from_bytes(ev.domain().uuid);
        // `send` fails only when nothing is subscribed, which is normal.
        self.domain_events.send(ev.clone()).ok();
        match ev {
            DomainEvent::Lifecycle { event, .. } => {
                tracing::debug!(vm = %ev.domain().name, ?event, "lifecycle");
                if event == LifecycleEvent::Stopped {
                    // A cold boot applies all pending changes.
                    if let Some(rt) = lock(&self.runtime).get_mut(&id) {
                        rt.pending_restart = false;
                    }
                }
                self.refresh_id(id).await;
            }
            DomainEvent::BalloonChange { actual_kib, .. } => {
                self.update_runtime(id, |rt| {
                    rt.memory_kib = actual_kib;
                    rt.memory_stats.actual_kib = Some(actual_kib);
                });
            }
            DomainEvent::AgentLifecycle { connected, .. } => {
                self.update_runtime(id, |rt| rt.agent = connected);
            }
            DomainEvent::BlockJob { .. }
            | DomainEvent::JobCompleted { .. }
            | DomainEvent::DeviceRemoved { .. }
            | DomainEvent::MigrationIteration { .. } => {}
        }
    }

    /// Makes sure every VM this node owns is defined in libvirt.
    pub(super) async fn reconcile(&self, c: &Client) {
        let _guard = self.ops.lock().await;
        let specs = match self.state.vms() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("reading VMs: {e}");
                return;
            }
        };
        let defined: HashMap<[u8; 16], Domain> = match c.list_all_domains(0).await {
            Ok(d) => d.into_iter().map(|d| (d.uuid, d)).collect(),
            Err(e) => {
                tracing::error!("listing domains: {e}");
                return;
            }
        };
        for spec in specs.iter().filter(|s| s.node == self.id) {
            let mut spec = spec.clone();
            spec.assign_targets();
            if !defined.contains_key(spec.id.as_bytes())
                && let Err(e) = self.define(c, &spec).await
            {
                tracing::error!(vm = %spec.name, "defining domain: {e}");
            }
            self.refresh(&spec).await;
        }
        // A VM never runs on two nodes: stop and undefine (keeping disks)
        // any Prism VM here that committed state gives to another node.
        // Ownership is frozen while the cluster is degraded.
        if self.cluster.get().is_some_and(|c| c.is_degraded()) {
            return;
        }
        for spec in specs
            .iter()
            .filter(|s| s.node != self.id && !s.node.is_nil())
        {
            let Some(dom) = defined.get(spec.id.as_bytes()) else {
                continue;
            };
            if self.migrating(spec.id) {
                continue;
            }
            tracing::warn!(vm = %spec.name, owner = %spec.node, "quarantining a VM owned by another node");
            if let Err(e) = c.destroy(dom, 0).await
                && !not_running(&e)
            {
                tracing::error!(vm = %spec.name, "stopping quarantined VM: {e}");
                continue;
            }
            if let Err(e) = c.undefine(dom, undefine_flags()).await {
                tracing::error!(vm = %spec.name, "undefining quarantined VM: {e}");
            }
            lock(&self.runtime).remove(&spec.id);
        }
    }

    /// Whether a live migration currently allows `id` on this node.
    pub(crate) fn migrating(&self, id: Uuid) -> bool {
        self.state
            .vm(id)
            .ok()
            .flatten()
            .and_then(|v| v.migrating)
            .is_some_and(|m| m.src == self.id || m.dst == self.id)
    }

    pub(super) async fn autostart(&self) {
        let Ok(specs) = self.state.vms() else { return };
        for spec in specs.iter().filter(|s| s.autostart && s.node == self.id) {
            if self.runtime(spec.id).state == VmState::Stopped {
                tracing::info!(vm = %spec.name, "autostart");
                if let Err(e) = self.action(spec.id, Action::Start).await {
                    tracing::error!(vm = %spec.name, "autostart failed: {e}");
                }
            }
        }
    }
}

pub(crate) fn undefine_flags() -> u32 {
    flags::DOMAIN_UNDEFINE_MANAGED_SAVE
        | flags::DOMAIN_UNDEFINE_SNAPSHOTS_METADATA
        | flags::DOMAIN_UNDEFINE_NVRAM
        | flags::DOMAIN_UNDEFINE_CHECKPOINTS_METADATA
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(format_version(12_000_000), "12.0.0");
        assert_eq!(format_version(10_002_001), "10.2.1");
    }
}
