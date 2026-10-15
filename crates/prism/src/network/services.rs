//! The DHCP and DNS servers of each network on this node.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use prism_net::dhcp::{DhcpConfig, DhcpServer, LeaseTable};
use prism_net::dns::{DnsConfig, DnsServer, Zone};
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
    dhcp: bool,
    leases: LeaseTable,
    dns: Arc<DnsServer>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        for t in &self.tasks {
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
                r.vip != vip || r.dhcp != sw.dhcp || r.tasks.iter().any(JoinHandle::is_finished)
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
            if let Some(r) = self.running.get(&sw.network) {
                r.leases
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone_from(&sw.leases);
                match zone(sw) {
                    Ok(z) => r.dns.set_zone(z),
                    Err(e) => tracing::warn!(bridge = %sw.bridge, "DNS zone: {e}"),
                }
            }
        }
    }
}

fn zone(sw: &SwitchPlan) -> prism_net::Result<Zone> {
    Zone::new(&sw.domain, &sw.hosts, DNS_TTL)?.with_blocked(BLOCKED)
}

fn start(sw: &SwitchPlan, vip: Ipv4Addr) -> anyhow::Result<Running> {
    let dns = DnsServer::bind(
        DnsConfig {
            listen: SocketAddr::from((vip, 53)),
            interface: Some(sw.bridge.clone()),
            upstreams: host::nameservers(&[vip]),
            upstream_timeout: UPSTREAM_TIMEOUT,
        },
        zone(sw)?,
    )?;
    let leases: LeaseTable = Arc::new(RwLock::new(HashMap::new()));
    let mut tasks = Vec::new();
    let d = Arc::clone(&dns);
    let bridge = sw.bridge.clone();
    tasks.push(tokio::spawn(async move {
        if let Err(e) = d.run().await {
            tracing::warn!(%bridge, "DNS stopped: {e}");
        }
    }));
    if sw.dhcp {
        let server = DhcpServer::bind(DhcpConfig::new(&sw.bridge, vip), Arc::clone(&leases))?;
        let bridge = sw.bridge.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = server.run().await {
                tracing::warn!(%bridge, "DHCP stopped: {e}");
            }
        }));
    }
    Ok(Running {
        vip,
        dhcp: sw.dhcp,
        leases,
        dns,
        tasks,
    })
}
