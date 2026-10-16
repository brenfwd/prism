//! The DHCP, DNS and router advertisement servers of each network on this node.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use ipnet::{Ipv4Net, Ipv6Net};
use prism_net::MacAddr;
use prism_net::dhcp::{DhcpConfig, DhcpServer, Lease, LeaseTable};
use prism_net::dns::{DnsConfig, DnsServer, Zone};
use prism_net::ra::{self, RaConfig, SharedRa};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::host;
use super::plan::SwitchPlan;

const DNS_TTL: u32 = 30;
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(2);
/// Names guests never resolve through the host.
const BLOCKED: &[&str] = &["ts.net"];

/// One layer 3 interface with DHCP and DNS: a switch bridge, or a VLAN interface on it. Only the
/// bridge has IPv6.
struct Segment<'a> {
    key: (Uuid, Option<u16>),
    interface: &'a str,
    address: Option<Ipv4Net>,
    address6: Option<Ipv6Net>,
    ra: Option<&'a RaConfig>,
    dhcp: bool,
    leases: &'a HashMap<MacAddr, Lease>,
    domain: &'a str,
    hosts: &'a HashMap<String, Vec<IpAddr>>,
}

/// The segments of `sw`: the bridge first, then each VLAN with a subnet.
fn segments(sw: &SwitchPlan) -> Vec<Segment<'_>> {
    let bridge = Segment {
        key: (sw.network, None),
        interface: &sw.bridge,
        address: sw.address,
        address6: sw.address6,
        ra: sw.ra.as_ref(),
        dhcp: sw.dhcp,
        leases: &sw.leases,
        domain: &sw.domain,
        hosts: &sw.hosts,
    };
    std::iter::once(bridge)
        .chain(sw.vlan_nets.iter().map(|v| Segment {
            key: (sw.network, Some(v.vlan)),
            interface: &v.interface,
            address: v.address,
            address6: None,
            ra: None,
            dhcp: sw.dhcp,
            leases: &v.leases,
            domain: &sw.domain,
            hosts: &v.hosts,
        }))
        .collect()
}

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

/// Running servers by network and VLAN (`None` is the bridge); follows the plan.
#[derive(Default)]
pub struct Services {
    running: HashMap<(Uuid, Option<u16>), Running>,
}

impl Services {
    /// Starts, updates and stops servers to match `switches`. Call after
    /// the bridges and VLAN interfaces carry their addresses.
    pub fn sync(&mut self, switches: &[SwitchPlan]) {
        let segments: Vec<Segment<'_>> = switches.iter().flat_map(segments).collect();
        self.running
            .retain(|key, _| segments.iter().any(|s| s.key == *key));
        for seg in &segments {
            let Some(vip) = seg.address.map(|a| a.addr()) else {
                self.running.remove(&seg.key);
                continue;
            };
            if self.running.get(&seg.key).is_some_and(|r| {
                r.vip != vip
                    || r.vip6 != seg.address6.map(|a| a.addr())
                    || r.dhcp != seg.dhcp
                    || r.ra.is_some() != seg.ra.is_some()
                    || r.tasks.iter().any(JoinHandle::is_finished)
            }) {
                self.running.remove(&seg.key);
            }
            if let std::collections::hash_map::Entry::Vacant(slot) = self.running.entry(seg.key) {
                match start(seg, vip) {
                    Ok(r) => {
                        slot.insert(r);
                    }
                    Err(e) => {
                        tracing::warn!(interface = %seg.interface, "starting DHCP/DNS: {e:#}");
                        continue;
                    }
                }
            }
            if let Some(r) = self.running.get_mut(&seg.key) {
                r.refresh(seg, vip);
            }
        }
    }
}

impl Running {
    /// Pushes the plan's leases, advertisements and zone into the running servers.
    fn refresh(&mut self, seg: &Segment<'_>, vip: Ipv4Addr) {
        self.leases
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone_from(seg.leases);
        if let (Some((shared, task)), Some(cfg)) = (&mut self.ra, seg.ra) {
            shared
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone_from(cfg);
            if task.is_finished() {
                *task = spawn_ra(shared, seg.interface);
            }
        }
        if self.dns6.is_none()
            && let Some(v6) = self.vip6
        {
            match dns_server(seg, SocketAddr::from((v6, 53)), host::nameservers(&[vip])) {
                Ok(d) => {
                    self.tasks.push(spawn_dns(&d, seg.interface));
                    self.dns6 = Some(d);
                }
                Err(e) => {
                    tracing::debug!(interface = %seg.interface, "IPv6 DNS not bound yet: {e:#}");
                }
            }
        }
        match zone(seg) {
            Ok(z) => {
                self.dns.set_zone(z.clone());
                if let Some(d) = &self.dns6 {
                    d.set_zone(z);
                }
            }
            Err(e) => tracing::warn!(interface = %seg.interface, "DNS zone: {e}"),
        }
    }
}

fn zone(seg: &Segment<'_>) -> prism_net::Result<Zone> {
    Zone::new(seg.domain, seg.hosts, DNS_TTL)?.with_blocked(BLOCKED)
}

fn dns_server(
    seg: &Segment<'_>,
    listen: SocketAddr,
    upstreams: Vec<SocketAddr>,
) -> anyhow::Result<Arc<DnsServer>> {
    Ok(DnsServer::bind(
        DnsConfig {
            listen,
            interface: Some(seg.interface.to_string()),
            upstreams,
            upstream_timeout: UPSTREAM_TIMEOUT,
        },
        zone(seg)?,
    )?)
}

fn spawn_dns(server: &Arc<DnsServer>, interface: &str) -> JoinHandle<()> {
    let (d, interface) = (Arc::clone(server), interface.to_string());
    tokio::spawn(async move {
        if let Err(e) = d.run().await {
            tracing::warn!(%interface, "DNS stopped: {e}");
        }
    })
}

fn spawn_ra(shared: &SharedRa, interface: &str) -> JoinHandle<()> {
    let (shared, interface) = (Arc::clone(shared), interface.to_string());
    tokio::spawn(async move {
        if let Err(e) = ra::run(shared).await {
            tracing::warn!(%interface, "router advertisements stopped: {e}");
        }
    })
}

fn start(seg: &Segment<'_>, vip: Ipv4Addr) -> anyhow::Result<Running> {
    let dns = dns_server(seg, SocketAddr::from((vip, 53)), host::nameservers(&[vip]))?;
    let leases: LeaseTable = Arc::new(RwLock::new(HashMap::new()));
    let mut tasks = vec![spawn_dns(&dns, seg.interface)];
    if seg.dhcp {
        let server = DhcpServer::bind(DhcpConfig::new(seg.interface, vip), Arc::clone(&leases))?;
        let interface = seg.interface.to_string();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = server.run().await {
                tracing::warn!(%interface, "DHCP stopped: {e}");
            }
        }));
    }
    let ra = seg.ra.cloned().map(|cfg| {
        let shared = Arc::new(RwLock::new(cfg));
        let task = spawn_ra(&shared, seg.interface);
        (shared, task)
    });
    Ok(Running {
        vip,
        vip6: seg.address6.map(|a| a.addr()),
        dhcp: seg.dhcp,
        leases,
        dns,
        dns6: None,
        ra,
        tasks,
    })
}
