//! The nftables tables that guest networks need, around the mesh boundary
//! rules `wg.rs` installs.

use ipnet::{Ipv4Net, Ipv6Net};
use prism_net::nft::{
    BoundaryConfig, Chain, ChainType, Family, GuestPort, Hook, NatRule, Nft, Rule, SwitchServices,
    Table, boundary_ruleset, bridge_guard_ruleset, guest_input_chain, nat_ruleset, priority,
};

/// What the firewall should enforce for guests on this node.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GuestRules {
    /// Taps of local VMs on Prism networks, for anti-spoofing.
    pub ports: Vec<GuestPort>,
    /// Bridges with a gateway here: only DHCP, DNS and ping reach the host.
    pub switches: Vec<SwitchServices>,
    /// Masquerade, one rule per NAT network.
    pub nat: Vec<NatRule>,
    /// Destinations guests never reach through the host (the tailnet and
    /// other members' underlay addresses).
    pub blocked: Vec<Ipv4Net>,
    /// Each bridge and the only interface it may forward out of.
    pub routed: Vec<(String, Option<String>)>,
}

/// The mesh, when this node is in a cluster.
#[derive(Debug, Clone)]
pub struct MeshBoundary {
    pub prefix: Ipv6Net,
    pub qemu_uid: Option<u32>,
}

/// Installs [`GuestRules`] atomically.
#[derive(Debug, Default)]
pub struct Firewall;

impl Firewall {
    /// The tables for `rules`: `inet prism` (boundary plus guest chains),
    /// `ip prism-nat` and `bridge prism`.
    pub fn tables(rules: &GuestRules, mesh: Option<&MeshBoundary>) -> Vec<Table> {
        let mut inet = if let Some(m) = mesh {
            boundary_ruleset(&BoundaryConfig {
                mesh_prefix: m.prefix,
                wg_interface: crate::cluster::mesh::INTERFACE.into(),
                qemu_uid: m.qemu_uid,
            })
        } else {
            let mut t = Table::new(Family::Inet, "prism");
            t.chain(Chain::base(
                "forward",
                ChainType::Filter,
                Hook::Forward,
                priority::FILTER,
            ));
            t
        };
        inet.chains.push(guest_input_chain(&rules.switches));
        if let Some(fwd) = inet.chain_mut("forward") {
            for (bridge, out) in &rules.routed {
                for net in &rules.blocked {
                    fwd.push(
                        Rule::new(Family::Inet)
                            .iifname(bridge)
                            .ip_daddr(*net)
                            .drop(),
                    );
                }
                let rule = Rule::new(Family::Inet).iifname(bridge);
                fwd.push(match out {
                    Some(uplink) => rule.oifname_ne(uplink).drop(),
                    None => rule.drop(),
                });
            }
        }
        vec![
            inet,
            nat_ruleset(&rules.nat),
            bridge_guard_ruleset(&rules.ports),
        ]
    }

    pub async fn apply(
        &self,
        rules: &GuestRules,
        mesh: Option<&MeshBoundary>,
    ) -> anyhow::Result<()> {
        let nft = Nft::new()?;
        let (_, res) = nft.replace_tables_async(Self::tables(rules, mesh)).await;
        Ok(res?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_tables() {
        let rules = GuestRules {
            switches: vec![SwitchServices {
                bridge: "ps100000".into(),
                vip: "10.88.0.1".parse().unwrap(),
            }],
            blocked: vec!["100.64.0.0/10".parse().unwrap()],
            routed: vec![("ps100000".into(), Some("eth0".into()))],
            ..GuestRules::default()
        };
        let t = Firewall::tables(&rules, None);
        assert_eq!(t.len(), 3);
        let fwd = t[0].chains.iter().find(|c| c.name == "forward").unwrap();
        assert_eq!(fwd.rules.len(), 2);
        assert!(t[0].chains.iter().any(|c| c.name == "input"));
    }
}
