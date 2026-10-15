//! Prism networks ("switches"): cluster-wide L2 segments built from a bridge
//! on every node, joined by VXLAN inside the `WireGuard` mesh, with embedded
//! DHCP, DNS and NAT.
//!
//! The replicated [`NetworkDoc`] is the desired state; [`agent`] turns it into
//! bridges, VXLAN devices, FDB entries, DHCP/DNS servers and nftables rules on
//! each node.

pub mod agent;
pub mod firewall;
pub mod host;
pub mod ipam;
pub mod plan;
mod services;

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use agent::Agent;

/// Collection of [`NetworkDoc`]s.
pub const NETWORKS: &str = "network";
/// Collection of released VNIs ([`ReleasedVni`]), kept for the cooldown.
pub const RELEASED_VNIS: &str = "network.vni";
/// The network new VMs join when the request names none.
pub const DEFAULT_NETWORK: &str = "default";

/// VNIs Prism allocates from.
pub const VNI_MIN: u32 = 100_000;
pub const VNI_MAX: u32 = 999_999;
/// A deleted network's VNI is not reused for this long, so stale FDB entries
/// on a lagging node never join two networks.
pub const VNI_COOLDOWN_SECS: i64 = 24 * 3600;

/// Switch MTU on a standalone node, and the most a clustered one gets.
pub const MAX_MTU: u32 = 1500;
/// VXLAN over IPv6 costs 70 bytes inside `wg-prism`.
pub const VXLAN_OVERHEAD: u32 = 70;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkKind {
    /// Guests reach the outside through masquerade on the egress node.
    #[default]
    Nat,
    /// VM-to-VM traffic only.
    Isolated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkDoc {
    pub id: Uuid,
    /// A DNS label: guests resolve `<vm>.<name>.prism`.
    pub name: String,
    pub kind: NetworkKind,
    /// The VXLAN network identifier, allocated when the network is created.
    pub vni: u32,
    pub subnet: Ipv4Net,
    /// Each node's address on the bridge: its gateway, DHCP server and DNS.
    #[serde(default)]
    pub gateways: BTreeMap<Uuid, Ipv4Addr>,
    /// The node that NATs this network's traffic; `None` means every node
    /// egresses for its own VMs.
    #[serde(default)]
    pub egress: Option<Uuid>,
    /// Serve DHCP with IPAM leases.
    pub dhcp: bool,
    pub created: i64,
}

/// A VNI released by a deleted network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleasedVni {
    pub vni: u32,
    pub released: i64,
}

impl NetworkDoc {
    /// The bridge on every node, `ps<vni>`.
    pub fn bridge(&self) -> String {
        bridge_name(self.vni)
    }

    pub fn vxlan(&self) -> String {
        format!("vx{}", self.vni)
    }

    /// The DNS zone guests on this network live in.
    pub fn domain(&self) -> String {
        format!("{}.prism", self.name)
    }

    /// The gateway a NIC uses: its override, else the network's egress node,
    /// else the node the VM runs on; `None` on isolated networks.
    pub fn router_for(&self, vm_node: Uuid, nic_egress: Option<Uuid>) -> Option<Ipv4Addr> {
        if self.kind == NetworkKind::Isolated {
            return None;
        }
        [nic_egress, self.egress, Some(vm_node)]
            .into_iter()
            .flatten()
            .find_map(|n| self.gateways.get(&n).copied())
    }
}

pub fn bridge_name(vni: u32) -> String {
    format!("ps{vni}")
}

/// The VNI of a Prism bridge or VXLAN device name.
pub fn vni_of(ifname: &str) -> Option<u32> {
    let digits = ifname
        .strip_prefix("ps")
        .or_else(|| ifname.strip_prefix("vx"))?;
    digits
        .parse()
        .ok()
        .filter(|v| (VNI_MIN..=VNI_MAX).contains(v))
}

/// The switch MTU for a cluster whose `wg-prism` MTU is `wg_mtu`.
pub fn switch_mtu(wg_mtu: Option<u16>) -> u32 {
    wg_mtu.map_or(MAX_MTU, |w| {
        MAX_MTU.min(u32::from(w).saturating_sub(VXLAN_OVERHEAD))
    })
}

/// Network names are DNS labels: 1–32 of `[a-z0-9-]`, no leading or
/// trailing hyphen.
pub fn valid_network_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// Subnet sizes Prism accepts: room for gateways and guests, and not so big
/// that IPAM scans get slow.
pub fn valid_subnet(net: Ipv4Net) -> Result<(), String> {
    if !(16..=29).contains(&net.prefix_len()) {
        return Err("subnet prefix must be /16 to /29".into());
    }
    if net.network() != net.addr() {
        return Err(format!("{net} has host bits set; use {}", net.trunc()));
    }
    let a = net.network();
    if a.is_loopback() || a.is_multicast() || a.is_link_local() || a.is_unspecified() {
        return Err(format!("{net} is not usable for guests"));
    }
    if ipam::CGNAT.contains(&a) {
        return Err(format!(
            "{net} overlaps 100.64.0.0/10 (carrier-grade NAT, Tailscale)"
        ));
    }
    Ok(())
}

/// `POST /api/v1/networks` body; every field has a smart default.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateNetwork {
    pub name: Option<String>,
    #[serde(default)]
    pub kind: NetworkKind,
    pub subnet: Option<Ipv4Net>,
    pub egress: Option<Uuid>,
    pub dhcp: Option<bool>,
}

/// `POST /api/v1/networks/check` body: what the form has so far.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckNetwork {
    /// The network being edited, if any.
    pub id: Option<Uuid>,
    pub name: Option<String>,
    pub subnet: Option<Ipv4Net>,
}

/// `PATCH /api/v1/networks/{id}` body.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateNetwork {
    pub name: Option<String>,
    /// `null` returns egress to each VM's own node.
    #[allow(clippy::option_option)]
    #[serde(default, deserialize_with = "double_option")]
    pub egress: Option<Option<Uuid>>,
    pub dhcp: Option<bool>,
}

#[allow(clippy::option_option)]
fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> NetworkDoc {
        NetworkDoc {
            id: Uuid::nil(),
            name: "lab".into(),
            kind: NetworkKind::Nat,
            vni: 100_001,
            subnet: "10.88.0.0/24".parse().unwrap(),
            gateways: BTreeMap::from([
                (Uuid::from_u128(1), Ipv4Addr::new(10, 88, 0, 1)),
                (Uuid::from_u128(2), Ipv4Addr::new(10, 88, 0, 2)),
            ]),
            egress: None,
            dhcp: true,
            created: 0,
        }
    }

    #[test]
    fn names() {
        let d = doc();
        assert_eq!(d.bridge(), "ps100001");
        assert_eq!(d.vxlan(), "vx100001");
        assert_eq!(d.domain(), "lab.prism");
        assert_eq!(vni_of("ps100001"), Some(100_001));
        assert_eq!(vni_of("vx999999"), Some(999_999));
        assert_eq!(vni_of("ps12"), None);
        assert_eq!(vni_of("eth0"), None);
        assert!(valid_network_name("default"));
        assert!(valid_network_name("lab-2"));
        assert!(!valid_network_name("Lab"));
        assert!(!valid_network_name("-x"));
        assert!(!valid_network_name("a.b"));
    }

    #[test]
    fn routers() {
        let mut d = doc();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        assert_eq!(d.router_for(a, None), Some(Ipv4Addr::new(10, 88, 0, 1)));
        assert_eq!(d.router_for(a, Some(b)), Some(Ipv4Addr::new(10, 88, 0, 2)));
        d.egress = Some(b);
        assert_eq!(d.router_for(a, None), Some(Ipv4Addr::new(10, 88, 0, 2)));
        assert_eq!(d.router_for(b, Some(a)), Some(Ipv4Addr::new(10, 88, 0, 1)));
        // An unknown override falls back.
        d.egress = None;
        assert_eq!(
            d.router_for(a, Some(Uuid::from_u128(9))),
            Some(Ipv4Addr::new(10, 88, 0, 1))
        );
        d.kind = NetworkKind::Isolated;
        assert_eq!(d.router_for(a, None), None);
    }

    #[test]
    fn mtus_and_subnets() {
        assert_eq!(switch_mtu(None), 1500);
        assert_eq!(switch_mtu(Some(1420)), 1350);
        assert_eq!(switch_mtu(Some(9000)), 1500);
        assert!(valid_subnet("10.88.0.0/24".parse().unwrap()).is_ok());
        assert!(valid_subnet("10.88.0.1/24".parse().unwrap()).is_err());
        assert!(valid_subnet("10.0.0.0/8".parse().unwrap()).is_err());
        assert!(valid_subnet("100.64.1.0/24".parse().unwrap()).is_err());
        assert!(valid_subnet("127.0.0.0/24".parse().unwrap()).is_err());
    }

    #[test]
    fn update_distinguishes_null() {
        let u: UpdateNetwork = serde_json::from_str(r#"{"egress": null}"#).unwrap();
        assert_eq!(u.egress, Some(None));
        let u: UpdateNetwork = serde_json::from_str("{}").unwrap();
        assert_eq!(u.egress, None);
    }
}
