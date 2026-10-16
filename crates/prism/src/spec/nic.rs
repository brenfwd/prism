//! NIC specs, network attachment and port forwards.

use std::net::{Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicSpec {
    pub id: Uuid,
    pub mac: String,
    pub network: NicNetwork,
    #[serde(default)]
    pub boot_order: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NicNetwork {
    /// An existing host bridge (host bridging).
    Bridge { bridge: String },
    /// User-mode networking (passt); no host setup needed.
    User {
        /// Host ports forwarded into the guest.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        forwards: Vec<PortForward>,
    },
    /// A Prism network (switch).
    Network {
        network: Uuid,
        /// The IPAM lease; assigned when the VM is stored if absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ipv4: Option<Ipv4Addr>,
        /// The guest's global IPv6 address, which IPv6 port forwards target.
        /// Set by hand: the network itself is IPv4 only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ipv6: Option<Ipv6Addr>,
        /// Egress node override; `None` uses the network's egress.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        egress: Option<Uuid>,
        /// Access VLAN (2 to 4094) inside the network; the NIC then shares
        /// a segment only with NICs on the same VLAN, not the gateway.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vlan: Option<u16>,
        /// Extra VLANs (2 to 4094) delivered tagged, making the NIC a trunk;
        /// `vlan` (or the untagged default segment) stays its native VLAN.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        trunk: Vec<u16>,
        /// Host ports forwarded to this NIC's lease by destination NAT.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        forwards: Vec<PortForward>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Proto {
    Tcp,
    Udp,
}

impl Proto {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

/// One host port forwarded to a guest port. On a Prism network NIC it is
/// destination NAT on the owning node (any host address when `host_addr` is
/// absent); on a user-mode NIC it is passt (loopback when absent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortForward {
    pub proto: Proto,
    /// Host address to listen on; loopback when absent.
    #[serde(default)]
    pub host_addr: Option<std::net::IpAddr>,
    pub host_port: u16,
    pub guest_port: u16,
    /// Also reach the guest from the host itself (loopback, or `host_addr`).
    /// Prism networks only.
    #[serde(default = "yes")]
    pub local: bool,
    /// Also reach the guest through the host address from other guests on
    /// the same network. Prism networks only.
    #[serde(default = "yes")]
    pub hairpin: bool,
}

fn yes() -> bool {
    true
}

impl PortForward {
    /// The address actually bound on the host.
    pub fn listen_addr(&self) -> std::net::IpAddr {
        self.host_addr
            .unwrap_or(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST))
    }
}

/// Checks a NIC's forwards: ports in range, no duplicate listeners.
pub fn validate_forwards(forwards: &[PortForward]) -> Result<(), String> {
    for (i, f) in forwards.iter().enumerate() {
        if f.host_port == 0 || f.guest_port == 0 {
            return Err("ports must be 1..=65535".into());
        }
        if forwards[..i].iter().any(|g| {
            g.proto == f.proto && g.host_port == f.host_port && g.listen_addr() == f.listen_addr()
        }) {
            return Err(format!(
                "{}/{} is forwarded twice",
                f.proto.as_str(),
                f.host_port
            ));
        }
    }
    Ok(())
}

impl NicNetwork {
    /// A user-mode NIC without forwards.
    pub fn user() -> Self {
        Self::User {
            forwards: Vec::new(),
        }
    }

    /// The port forwards configured on this NIC.
    pub fn forwards(&self) -> &[PortForward] {
        match self {
            Self::User { forwards } | Self::Network { forwards, .. } => forwards,
            Self::Bridge { .. } => &[],
        }
    }

    /// Mutable access to the forwards, if this kind of NIC can have any.
    pub fn forwards_mut(&mut self) -> Option<&mut Vec<PortForward>> {
        match self {
            Self::User { forwards } | Self::Network { forwards, .. } => Some(forwards),
            Self::Bridge { .. } => None,
        }
    }

    /// The VLAN membership of a Prism network NIC: access on `vlan` (1 when
    /// unset) or, with a trunk list, `vlan` as native plus the tagged VIDs.
    pub fn port_vlans(&self) -> Option<prism_net::link::PortVlans> {
        let Self::Network { vlan, trunk, .. } = self else {
            return None;
        };
        let native = vlan.unwrap_or(1);
        Some(if trunk.is_empty() {
            prism_net::link::PortVlans::Access(native)
        } else {
            prism_net::link::PortVlans::Trunk {
                native: Some(native),
                tagged: trunk.iter().copied().collect(),
            }
        })
    }

    /// Checks the VLAN settings of a NIC.
    pub fn validate_vlans(&self) -> Result<(), String> {
        let Self::Network { vlan, trunk, .. } = self else {
            return Ok(());
        };
        let range = 2..=4094;
        if let Some(v) = vlan.filter(|v| !range.contains(v)) {
            return Err(format!("VLAN {v} is out of range 2 to 4094"));
        }
        if let Some(v) = trunk.iter().find(|v| !range.contains(v)) {
            return Err(format!("trunk VLAN {v} is out of range 2 to 4094"));
        }
        if let Some(v) = vlan.filter(|v| trunk.contains(v)) {
            return Err(format!("VLAN {v} is both native and tagged"));
        }
        if trunk
            .iter()
            .enumerate()
            .any(|(i, v)| trunk[..i].contains(v))
        {
            return Err("trunk VLANs must be unique".into());
        }
        Ok(())
    }

    /// The Prism network this NIC is on, if any.
    pub fn network_id(&self) -> Option<Uuid> {
        match self {
            Self::Network { network, .. } => Some(*network),
            _ => None,
        }
    }

    /// Forgets the IPAM lease, for copies of a NIC (restores, clones).
    pub fn clear_lease(&mut self) {
        if let Self::Network { ipv4, .. } = self {
            *ipv4 = None;
        }
    }
}

/// A locally administered unicast MAC in QEMU's `52:54:00` range.
pub fn random_mac() -> String {
    let b = Uuid::new_v4().into_bytes();
    format!("52:54:00:{:02x}:{:02x}:{:02x}", b[0], b[1], b[2])
}

/// Linux interface names: 1–15 bytes, no `/`, whitespace or `:`.
pub fn valid_ifname(name: &str) -> bool {
    (1..=15).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
