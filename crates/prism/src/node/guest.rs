//! What the guest agent reports from inside a running VM.

use std::net::Ipv6Addr;
use std::sync::Arc;
use std::time::Duration;

use prism_libvirt::AddressSource;
use prism_net::MacAddr;
use serde::Serialize;
use uuid::Uuid;

use super::{Node, Result, domain_ref, lock};

/// How often the guest agents of running VMs are asked for their addresses.
const ADDR_INTERVAL: Duration = Duration::from_secs(30);

/// One address on a guest interface.
#[derive(Debug, Clone, Serialize)]
pub struct GuestAddr {
    pub addr: String,
    pub prefix: u32,
    pub ipv6: bool,
}

/// One network interface as the guest reports it.
#[derive(Debug, Clone, Serialize)]
pub struct GuestNic {
    pub name: String,
    pub mac: Option<String>,
    pub addrs: Vec<GuestAddr>,
}

/// The guest's network interfaces, as read through the guest agent.
#[derive(Debug, Clone, Serialize)]
pub struct GuestNetwork {
    /// The agent answered; otherwise `interfaces` is empty.
    pub agent: bool,
    pub interfaces: Vec<GuestNic>,
}

impl Node {
    /// Interfaces and IPs as the guest sees them. A VM that is stopped or
    /// has no agent yields an empty list, not an error.
    pub async fn guest_network(&self, id: Uuid) -> Result<GuestNetwork> {
        let spec = self.local_spec(id)?;
        let none = GuestNetwork {
            agent: false,
            interfaces: Vec::new(),
        };
        if !self.runtime(id).state.is_active() {
            return Ok(none);
        }
        let c = self.client()?;
        let Ok(ifaces) = c
            .interface_addresses(&domain_ref(&spec), AddressSource::Agent)
            .await
        else {
            return Ok(none);
        };
        let interfaces = ifaces
            .into_iter()
            .filter(|i| i.name != "lo")
            .map(|i| GuestNic {
                name: i.name,
                mac: i.hwaddr,
                addrs: i
                    .addrs
                    .into_iter()
                    .map(|a| GuestAddr {
                        addr: a.addr,
                        prefix: a.prefix,
                        ipv6: a.ipv6,
                    })
                    .collect(),
            })
            .collect();
        Ok(GuestNetwork {
            agent: true,
            interfaces,
        })
    }
}

/// The global IPv6 addresses each interface reports, by its MAC. Link-local addresses are
/// left out: the bridge guard allows those anyway.
fn global_v6_by_mac(ifaces: &[GuestNic]) -> Vec<(MacAddr, Vec<Ipv6Addr>)> {
    ifaces
        .iter()
        .filter_map(|i| {
            let mac = i.mac.as_deref()?.parse::<MacAddr>().ok()?;
            let addrs = i
                .addrs
                .iter()
                .filter(|a| a.ipv6)
                .filter_map(|a| a.addr.parse::<Ipv6Addr>().ok())
                .filter(|a| (a.segments()[0] & 0xffc0) != 0xfe80 && !a.is_loopback())
                .collect();
            Some((mac, addrs))
        })
        .collect()
}

impl Node {
    /// Asks the guest agent of every running local VM for its addresses, so AAAA records and
    /// IPv6 forwards follow the address the guest really uses. A VM that stopped or has no
    /// agent loses what it reported before.
    pub(super) async fn guest_addr_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(ADDR_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let ids: Vec<Uuid> = lock(&self.runtime).keys().copied().collect();
            for id in ids {
                let Ok(spec) = self.local_spec(id) else {
                    continue;
                };
                let reported = match self.guest_network(id).await {
                    Ok(n) if n.agent => global_v6_by_mac(&n.interfaces),
                    _ => Vec::new(),
                };
                for nic in &spec.nics {
                    if let Ok(mac) = nic.mac.parse::<MacAddr>() {
                        let addrs = reported
                            .iter()
                            .find(|(m, _)| *m == mac)
                            .map(|(_, a)| a.clone())
                            .unwrap_or_default();
                        self.guest_addrs.set_agent(mac, addrs);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nic(mac: &str, addrs: &[(&str, bool)]) -> GuestNic {
        GuestNic {
            name: "eth0".into(),
            mac: Some(mac.into()),
            addrs: addrs
                .iter()
                .map(|(a, v6)| GuestAddr {
                    addr: (*a).into(),
                    prefix: 64,
                    ipv6: *v6,
                })
                .collect(),
        }
    }

    #[test]
    fn only_global_ipv6_addresses_are_kept() {
        let got = global_v6_by_mac(&[nic(
            "52:54:00:00:00:01",
            &[
                ("10.0.0.5", false),
                ("fe80::1", true),
                ("::1", true),
                ("fd01:2:3:4:aaaa:bbbb:cccc:dddd", true),
            ],
        )]);
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].1,
            vec![
                "fd01:2:3:4:aaaa:bbbb:cccc:dddd"
                    .parse::<Ipv6Addr>()
                    .unwrap()
            ]
        );
    }

    #[test]
    fn interfaces_without_a_mac_are_skipped() {
        let mut n = nic("52:54:00:00:00:01", &[]);
        n.mac = None;
        assert_eq!(global_v6_by_mac(&[n]).len(), 0);
    }
}
