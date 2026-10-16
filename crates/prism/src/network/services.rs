//! The DHCP, DNS and router advertisement servers of each network on this node.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use prism_net::dhcp::{DhcpConfig, DhcpServer, LeaseTable};
use prism_net::dns::{DnsConfig, DnsServer, Zone};
use prism_net::ra::{self, SharedRa};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::host;
use super::plan::SwitchPlan;

const DNS_TTL: u32 = 30;
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(2);
/// Names guests never resolve through the host.
const BLOCKED: &[&str] = &["ts.net"];

struct Running {
    vip: Ipv4Addr,
    vip6: Option<Ipv6Addr>,
    dhcp: bool,
    leases: LeaseTable,
    dns: Arc<DnsServer>,
    /// The server on the node's IPv6 address. It binds once duplicate address
    /// detection has finished with that address, which can be after `start`.
    dns6: Option<Arc<DnsServer>>,
    /// The advertisement daemon, restarted on its own when it stops so a
    /// slow link-local address does not restart DHCP and DNS.
    ra: Option<(SharedRa, JoinHandle<()>)>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        for t in self.tasks.iter().chain(self.ra.iter().map(|(_, t)| t)) {
            t.abort();
        }
    }
}

/// Running servers by network; follows the plan.
#[derive(Default)]
pub struct Services {
    running: HashMap<Uuid, Running>,
}

impl Services {
    /// Starts, updates and stops servers to match `switches`. Call after
    /// the bridges carry their addresses.
    pub fn sync(&mut self, switches: &[SwitchPlan]) {
        self.running
            .retain(|id, _| switches.iter().any(|s| s.network == *id));
        for sw in switches {
            let Some(vip) = sw.address.map(|a| a.addr()) else {
                self.running.remove(&sw.network);
                continue;
            };
            if self.running.get(&sw.network).is_some_and(|r| {
                r.vip != vip
                    || r.vip6 != sw.address6.map(|a| a.addr())
                    || r.dhcp != sw.dhcp
                    || r.ra.is_some() != sw.ra.is_some()
                    || r.tasks.iter().any(JoinHandle::is_finished)
            }) {
                self.running.remove(&sw.network);
            }
            if let std::collections::hash_map::Entry::Vacant(slot) = self.running.entry(sw.network)
            {
                match start(sw, vip) {
                    Ok(r) => {
                        slot.insert(r);
                    }
                    Err(e) => {
                        tracing::warn!(bridge = %sw.bridge, "starting DHCP/DNS: {e:#}");
                        continue;
                    }
                }
            }
            if let Some(r) = self.running.get_mut(&sw.network) {
                r.leases
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone_from(&sw.leases);
                if let (Some((shared, task)), Some(cfg)) = (&mut r.ra, &sw.ra) {
                    shared
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone_from(cfg);
                    if task.is_finished() {
                        *task = spawn_ra(shared, &sw.bridge);
                    }
                }
                if r.dns6.is_none()
                    && let Some(v6) = r.vip6
                {
                    match dns_server(sw, SocketAddr::from((v6, 53)), host::nameservers(&[vip])) {
                        Ok(d) => {
                            r.tasks.push(spawn_dns(&d, &sw.bridge));
                            r.dns6 = Some(d);
                        }
                        Err(e) => {
                            tracing::debug!(bridge = %sw.bridge, "IPv6 DNS not bound yet: {e:#}");
                        }
                    }
                }
                match zone(sw) {
                    Ok(z) => {
                        r.dns.set_zone(z.clone());
                        if let Some(d) = &r.dns6 {
                            d.set_zone(z);
                        }
                    }
                    Err(e) => tracing::warn!(bridge = %sw.bridge, "DNS zone: {e}"),
                }
            }
        }
    }
}

fn zone(sw: &SwitchPlan) -> prism_net::Result<Zone> {
    Zone::new(&sw.domain, &sw.hosts, DNS_TTL)?.with_blocked(BLOCKED)
}

fn dns_server(
    sw: &SwitchPlan,
    listen: SocketAddr,
    upstreams: Vec<SocketAddr>,
) -> anyhow::Result<Arc<DnsServer>> {
    Ok(DnsServer::bind(
        DnsConfig {
            listen,
            interface: Some(sw.bridge.clone()),
            upstreams,
            upstream_timeout: UPSTREAM_TIMEOUT,
        },
        zone(sw)?,
    )?)
}

fn spawn_dns(server: &Arc<DnsServer>, bridge: &str) -> JoinHandle<()> {
    let (d, bridge) = (Arc::clone(server), bridge.to_string());
    tokio::spawn(async move {
        if let Err(e) = d.run().await {
            tracing::warn!(%bridge, "DNS stopped: {e}");
        }
    })
}

fn spawn_ra(shared: &SharedRa, bridge: &str) -> JoinHandle<()> {
    let (shared, bridge) = (Arc::clone(shared), bridge.to_string());
    tokio::spawn(async move {
        if let Err(e) = ra::run(shared).await {
            tracing::warn!(%bridge, "router advertisements stopped: {e}");
        }
    })
}

fn start(sw: &SwitchPlan, vip: Ipv4Addr) -> anyhow::Result<Running> {
    let dns = dns_server(sw, SocketAddr::from((vip, 53)), host::nameservers(&[vip]))?;
    let leases: LeaseTable = Arc::new(RwLock::new(HashMap::new()));
    let mut tasks = vec![spawn_dns(&dns, &sw.bridge)];
    if sw.dhcp {
        let server = DhcpServer::bind(DhcpConfig::new(&sw.bridge, vip), Arc::clone(&leases))?;
        let bridge = sw.bridge.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = server.run().await {
                tracing::warn!(%bridge, "DHCP stopped: {e}");
            }
        }));
    }
    let ra = sw.ra.clone().map(|cfg| {
        let shared = Arc::new(RwLock::new(cfg));
        let task = spawn_ra(&shared, &sw.bridge);
        (shared, task)
    });
    Ok(Running {
        vip,
        vip6: sw.address6.map(|a| a.addr()),
        dhcp: sw.dhcp,
        leases,
        dns,
        dns6: None,
        ra,
        tasks,
    })
}
