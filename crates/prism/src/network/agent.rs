//! The per-node reconciler: makes bridges, VXLAN devices, FDB entries,
//! DHCP/DNS servers and firewall rules match the replicated networks.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use ipnet::{IpNet, Ipv6Net};
use prism_libvirt::DomainEvent;
use prism_net::Netlink;
use prism_net::link::{BridgeConfig, PortVlans, VXLAN_PORT, VxlanConfig};
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
/// Delay between the follow-up passes after a VM lifecycle event.
const FOLLOW_UP: Duration = Duration::from_secs(1);
/// Follow-up passes after a lifecycle event, for a tap that appears late.
const FOLLOW_UPS: u8 = 5;

pub struct Agent {
    node: Uuid,
    state: State,
    firewall: Firewall,
    services: tokio::sync::Mutex<Services>,
    /// Libvirt domain events: a VM starting creates its tap, which needs
    /// its VLANs applied right away.
    domains: broadcast::Sender<DomainEvent>,
}

impl Agent {
    pub fn new(node: Uuid, state: State, domains: broadcast::Sender<DomainEvent>) -> Arc<Self> {
        Arc::new(Self {
            node,
            state,
            firewall: Firewall,
            services: tokio::sync::Mutex::default(),
            domains,
        })
    }

    /// Reconciles on every state change and periodically, forever.
    pub async fn run(self: Arc<Self>) {
        let mut rx = self.state.subscribe();
        let mut dom = self.domains.subscribe();
        let mut nl: Option<Netlink> = None;
        let mut follow_ups = 0u8;
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
                r = dom.recv() => {
                    match r {
                        Err(broadcast::error::RecvError::Closed) => return,
                        Ok(DomainEvent::Lifecycle { .. }) | Err(_) => {}
                        Ok(_) => continue,
                    }
                    // The tap appears a moment after the lifecycle event,
                    // and after a quick stop and start the first pass can
                    // run before the new tap exists. Keep passing at short
                    // intervals instead of waiting for the next resync.
                    tokio::time::sleep(DEBOUNCE).await;
                    while dom.try_recv().is_ok() {}
                    follow_ups = FOLLOW_UPS;
                }
                () = tokio::time::sleep(if follow_ups > 0 { FOLLOW_UP } else { RESYNC }) => {
                    follow_ups = follow_ups.saturating_sub(1);
                }
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
        let uplink6 = host::uplink6_of(&host::routes6());
        let p = plan(&PlanInput {
            node,
            networks: &networks,
            vms: &vms,
            members: if clustered { &members } else { &[] },
            wg_mtu: cluster.as_ref().filter(|_| clustered).map(|c| c.wg_mtu),
            uplink: host::uplink_of(&routes),
            uplink6: uplink6.clone(),
        });
        if p.forwarding {
            host::enable_forwarding();
        }
        if p.forwarding6 {
            host::enable_forwarding6(uplink6.as_deref());
        }
        let mut errors = Vec::new();
        for sw in &p.switches {
            if let Err(e) = apply_switch(nl, sw).await {
                errors.push(format!("{}: {e}", sw.bridge));
            }
            // A failure above must not leave a freshly started VM's tap
            // without its VLANs.
            apply_ports(nl, sw).await;
        }
        for bridge in &p.local_bridges {
            host::enable_route_localnet(bridge);
        }
        remove_stale(nl, &p).await;
        self.services.lock().await.sync(&p.switches);
        // Keyed on the cluster record alone: an empty member list while
        // state loads must not replace the boundary table with an open one.
        let mesh = cluster.map(|c| MeshBoundary {
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
    nl.prune_v4_addresses(&sw.bridge, sw.address.map(IpNet::V4))
        .await?;
    if sw.ra.is_some() {
        host::lock_bridge6(&sw.bridge);
    }
    if let Some(g) = sw.gateway6 {
        nl.ensure_address(&sw.bridge, IpNet::V6(Ipv6Net::new(g, 64)?))
            .await?;
    }
    if let Some(a) = sw.address6 {
        nl.ensure_address(&sw.bridge, IpNet::V6(a)).await?;
    }
    nl.prune_ula_addresses(&sw.bridge, sw.address6.map(|a| a.addr()))
        .await?;
    for g in &sw.routes6 {
        nl.ensure_host_route6(&sw.bridge, *g).await?;
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
        nl.set_port_vlans(
            &v.name,
            &PortVlans::Trunk {
                native: Some(1),
                tagged: sw.vlans.iter().copied().collect(),
            },
        )
        .await?;
    }
    Ok(())
}

/// Applies the VLANs of every VM tap on the switch. The tap only exists
/// while its VM runs; the follow-up passes and the resync catch the rest.
async fn apply_ports(nl: &Netlink, sw: &SwitchPlan) {
    for (tap, vlans) in &sw.ports {
        if let Err(e) = nl.set_port_vlans(tap, vlans).await {
            tracing::debug!(%tap, "port vlan: {e}");
        }
    }
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
