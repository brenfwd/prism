//! What the guest agent reports from inside a running VM.

use prism_libvirt::AddressSource;
use serde::Serialize;
use uuid::Uuid;

use super::{Node, Result, domain_ref};

#[derive(Debug, Clone, Serialize)]
pub struct GuestAddr {
    pub addr: String,
    pub prefix: u32,
    pub ipv6: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GuestNic {
    pub name: String,
    pub mac: Option<String>,
    pub addrs: Vec<GuestAddr>,
}

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
