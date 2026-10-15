//! The per-node reconciler: makes bridges, VXLAN devices, FDB entries,
//! DHCP/DNS servers and firewall rules match the replicated networks.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use ipnet::{IpNet, Ipv6Net};
use prism_net::Netlink;
use prism_net::link::{BridgeConfig, VXLAN_PORT, VxlanConfig};
use tokio::sync::broadcast;
use uuid::Uuid;

use super::firewall::{Firewall, MeshBoundary};
use super::plan::{NodePlan, PlanInput, SwitchPlan, plan};
use super::services::Services;
use super::{host, vni_of};
use crate::state::State;

const RESYNC: Duration = Duration::from_secs(15);
/// Waits for a burst of changes to settle before reconciling.
const DEBOUNCE: Duration = Duration::from_millis(250);

pub struct Agent {
    node: Uuid,
    state: State,
    firewall: Firewall,
    services: tokio::sync::Mutex<Services>,
}

impl Agent {
    pub fn new(node: Uuid, state: State) -> Arc<Self> {
        Arc::new(Self {
            node,
            state,
            firewall: Firewall,
            services: tokio::sync::Mutex::default(),
        })
    }

    /// Reconciles on every state change and periodically, forever.
    pub async fn run(self: Arc<Self>) {
        let mut rx = self.state.subscribe();
        let mut nl: Option<Netlink> = None;
        loop {
            if nl.is_none() {
                nl = Netlink::connect()
                    .inspect_err(|e| tracing::warn!("network agent: netlink: {e}"))
                    .ok();
            }
            if let Some(nl) = &nl
                && let Err(e) = self.reconcile(nl).await
            {
                tracing::warn!("network reconcile: {e:#}");
            }
            tokio::select! {
                r = rx.recv() => {
                    if matches!(r, Err(broadcast::error::RecvError::Closed)) {
                        return;
                    }
                    tokio::time::sleep(DEBOUNCE).await;
                    while rx.try_recv().is_ok() {}
                }
                () = tokio::time::sleep(RESYNC) => {}
            }
        }
    }

    async fn reconcile(&self, nl: &Netlink) -> anyhow::Result<()> {
        let (state, node) = (self.state.clone(), self.node);
        let (networks, vms, members, cluster) = tokio::task::spawn_blocking(move || {
            anyhow::Ok((
                state.networks()?,
                state.vms()?,
                state.members()?,
                state.cluster()?,
            ))
        })
        .await??;
        let clustered = !members.is_empty() && cluster.is_some();
        let routes = host::routes();
        let p = plan(&PlanInput {
            node,
            networks: &networks,
            vms: &vms,
            members: if clustered { &members } else { &[] },
            wg_mtu: cluster.as_ref().filter(|_| clustered).map(|c| c.wg_mtu),
            uplink: host::uplink_of(&routes),
        });
        if p.forwarding {
            host::enable_forwarding();
        }
        let mut errors = Vec::new();
        for sw in &p.switches {
            if let Err(e) = apply_switch(nl, sw).await {
                errors.push(format!("{}: {e}", sw.bridge));
            }
        }
        remove_stale(nl, &p).await;
        self.services.lock().await.sync(&p.switches);
        let mesh = cluster.filter(|_| clustered).map(|c| MeshBoundary {
            prefix: Ipv6Net::new(c.mesh_prefix, 48).unwrap_or_default().trunc(),
            qemu_uid: crate::cluster::wg::qemu_uid(),
        });
        if let Err(e) = self.firewall.apply(&p.firewall, mesh.as_ref()).await {
            errors.push(format!("firewall: {e:#}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("{}", errors.join("; "))
        }
    }
}

async fn apply_switch(nl: &Netlink, sw: &SwitchPlan) -> anyhow::Result<()> {
    nl.ensure_bridge(&sw.bridge, &BridgeConfig { mtu: sw.mtu })
        .await?;
    if let Some(a) = sw.address {
        nl.ensure_address(&sw.bridge, IpNet::V4(a)).await?;
    }
    if let Some(v) = &sw.vxlan {
        nl.ensure_vxlan(
            &v.name,
            &VxlanConfig {
                vni: v.vni,
                local: v.local,
                port: VXLAN_PORT,
                mtu: sw.mtu,
                bridge: Some(sw.bridge.clone()),
            },
        )
        .await?;
        nl.sync_vxlan_fdb(&v.name, &v.fdb).await?;
    }
    Ok(())
}

/// Deletes bridges and VXLAN devices of networks that no longer exist, and
/// VXLAN devices left over from a cluster this node has left.
async fn remove_stale(nl: &Netlink, p: &NodePlan) {
    let want: BTreeSet<String> = p
        .switches
        .iter()
        .flat_map(|s| {
            std::iter::once(s.bridge.clone()).chain(s.vxlan.iter().map(|v| v.name.clone()))
        })
        .collect();
    let Ok(links) = nl.links().await else { return };
    for l in links {
        if vni_of(&l.name).is_some()
            && !want.contains(&l.name)
            && let Err(e) = nl.delete_link(&l.name).await
        {
            tracing::warn!(link = %l.name, "removing stale link: {e}");
        }
    }
}
