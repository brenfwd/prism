//! VLANs with their own subnet: each gets an interface on the switch bridge, a gateway on every
//! node, a DHCP scope, NAT and firewall rules.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{NetworkDoc, ipam, vni_of};

/// The lowest and highest VLAN ID a network can give a subnet to.
pub const VLAN_RANGE: std::ops::RangeInclusive<u16> = 2..=4094;

/// Most VLANs with their own subnet on one network.
pub const MAX_VLAN_SUBNETS: usize = 64;

/// The IPv4 side of one VLAN on a network: a subnet, and each node's gateway in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VlanSubnet {
    pub subnet: Ipv4Net,
    /// Each node's address on the VLAN interface: its gateway, DHCP server and DNS.
    #[serde(default)]
    pub gateways: BTreeMap<Uuid, Ipv4Addr>,
}

/// Reads the `vlan_subnets` map of a [`NetworkDoc`]. JSON object keys are strings, and a command
/// that is internally tagged (as every replicated one is) hands them over as buffered strings,
/// which serde cannot turn into `u16` keys by itself.
pub(super) fn deserialize_by_vlan<'de, D>(de: D) -> Result<BTreeMap<u16, VlanSubnet>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    BTreeMap::<String, VlanSubnet>::deserialize(de)?
        .into_iter()
        .map(|(vid, subnet)| {
            vid.parse::<u16>()
                .map(|vid| (vid, subnet))
                .map_err(|_| serde::de::Error::custom(format!("invalid VLAN ID `{vid}`")))
        })
        .collect()
}

/// `POST /api/v1/networks/{id}/vlans` body: give a VLAN its own subnet.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddVlanSubnet {
    /// The VLAN ID, 2 to 4094.
    pub vlan: u16,
    /// A free subnet is picked when this is left out.
    pub subnet: Option<Ipv4Net>,
}

/// The VLAN interface for `vid` on `bridge`, `ps<vni>.<vid>`; at most 13 characters.
pub fn vlan_interface(bridge: &str, vid: u16) -> String {
    format!("{bridge}.{vid}")
}

/// The VNI of a VLAN interface name made by [`vlan_interface`].
pub fn vlan_interface_vni(ifname: &str) -> Option<u32> {
    let (bridge, vid) = ifname.split_once('.')?;
    vid.parse::<u16>().ok()?;
    bridge.starts_with("ps").then(|| vni_of(bridge)).flatten()
}

/// Checks the VLAN subnets of `net`: IDs in range, usable subnets, and no two of the network's
/// subnets (the main one included) overlapping.
pub fn valid_vlan_subnets(net: &NetworkDoc) -> Result<(), String> {
    if net.vlan_subnets.len() > MAX_VLAN_SUBNETS {
        return Err(format!(
            "at most {MAX_VLAN_SUBNETS} VLANs can have their own subnet"
        ));
    }
    for (vid, vs) in &net.vlan_subnets {
        if !VLAN_RANGE.contains(vid) {
            return Err(format!("VLAN {vid} is out of range 2 to 4094"));
        }
        super::valid_subnet(vs.subnet).map_err(|e| format!("VLAN {vid}: {e}"))?;
    }
    let subnets = net.subnets();
    for (i, s) in subnets.iter().enumerate() {
        if let Some(o) = ipam::conflicts(*s, &subnets[i + 1..]) {
            return Err(format!("{s} overlaps {o}, which this network also uses"));
        }
    }
    Ok(())
}

impl NetworkDoc {
    /// The VLAN whose own subnet a NIC on `vlan` uses: `vlan` itself when it has a subnet, and
    /// `None` (the network's subnet) otherwise.
    pub fn segment(&self, vlan: Option<u16>) -> Option<u16> {
        vlan.filter(|v| self.vlan_subnets.contains_key(v))
    }

    /// The subnet of `segment`: a VLAN's own, or the network's for `None`.
    pub fn subnet_of(&self, segment: Option<u16>) -> Ipv4Net {
        segment
            .and_then(|v| self.vlan_subnets.get(&v))
            .map_or(self.subnet, |s| s.subnet)
    }

    /// The gateways of `segment`, by node.
    pub fn gateways_of(&self, segment: Option<u16>) -> &BTreeMap<Uuid, Ipv4Addr> {
        segment
            .and_then(|v| self.vlan_subnets.get(&v))
            .map_or(&self.gateways, |s| &s.gateways)
    }

    /// Every IPv4 subnet the network uses: its own and each VLAN's.
    pub fn subnets(&self) -> Vec<Ipv4Net> {
        std::iter::once(self.subnet)
            .chain(self.vlan_subnets.values().map(|s| s.subnet))
            .collect()
    }

    /// The interface that carries `segment` on every node: the bridge, or a VLAN interface on it.
    pub fn interface_of(&self, segment: Option<u16>) -> String {
        segment.map_or_else(|| self.bridge(), |v| vlan_interface(&self.bridge(), v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::NetworkKind;

    const A: Uuid = Uuid::from_u128(1);

    fn net() -> NetworkDoc {
        NetworkDoc {
            id: Uuid::nil(),
            name: "lab".into(),
            kind: NetworkKind::Nat,
            vni: 100_001,
            subnet: "10.88.0.0/24".parse().unwrap(),
            gateways: BTreeMap::from([(A, Ipv4Addr::new(10, 88, 0, 1))]),
            egress: None,
            dhcp: true,
            allow_prefixes: Vec::new(),
            subnet6: None,
            nat66: false,
            vlan_subnets: BTreeMap::from([(
                20,
                VlanSubnet {
                    subnet: "10.88.1.0/24".parse().unwrap(),
                    gateways: BTreeMap::from([(A, Ipv4Addr::new(10, 88, 1, 1))]),
                },
            )]),
            created: 0,
        }
    }

    #[test]
    fn vlan_subnets_survive_an_internally_tagged_command() {
        let cmd = crate::state::Command::PutNetwork {
            network: Box::new(net()),
            now: 0,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: crate::state::Command = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn interface_names() {
        assert_eq!(vlan_interface("ps100001", 4094), "ps100001.4094");
        assert_eq!(vlan_interface_vni("ps100001.20"), Some(100_001));
        assert_eq!(vlan_interface_vni("vx100001.20"), None);
        assert_eq!(vlan_interface_vni("ps100001"), None);
        assert_eq!(vlan_interface_vni("ps100001.x"), None);
    }

    #[test]
    fn segments() {
        let d = net();
        assert_eq!(d.segment(Some(20)), Some(20));
        assert_eq!(d.segment(Some(30)), None);
        assert_eq!(d.segment(None), None);
        assert_eq!(d.subnet_of(Some(20)).to_string(), "10.88.1.0/24");
        assert_eq!(d.subnet_of(None), d.subnet);
        assert_eq!(d.subnets().len(), 2);
        assert_eq!(d.interface_of(Some(20)), "ps100001.20");
        assert_eq!(d.interface_of(None), "ps100001");
        assert_eq!(
            d.router_for(Some(20), A, None),
            Some(Ipv4Addr::new(10, 88, 1, 1))
        );
        // A VLAN without a subnet uses the network's gateway.
        assert_eq!(
            d.router_for(d.segment(Some(30)), A, None),
            Some(Ipv4Addr::new(10, 88, 0, 1))
        );
    }

    #[test]
    fn validation() {
        let mut d = net();
        assert!(valid_vlan_subnets(&d).is_ok());
        d.vlan_subnets.get_mut(&20).unwrap().subnet = "10.88.0.128/25".parse().unwrap();
        assert!(valid_vlan_subnets(&d).unwrap_err().contains("overlaps"));
        d = net();
        let vs = d.vlan_subnets[&20].clone();
        d.vlan_subnets.insert(1, vs.clone());
        assert!(valid_vlan_subnets(&d).is_err());
        d.vlan_subnets.remove(&1);
        d.vlan_subnets.insert(30, vs);
        assert!(valid_vlan_subnets(&d).unwrap_err().contains("overlaps"));
    }
}
