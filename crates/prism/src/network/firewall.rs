//! The nftables tables that guest networks need, around the mesh boundary
//! rules `wg.rs` installs.

use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use prism_net::nft::{
    BoundaryConfig, Chain, ChainType, CtState, DnatRule, Family, GuestPort, Hook, Nat6Rule,
    NatRule, Nft, Rule, SwitchServices, Table, boundary_ruleset, bridge_guard_ruleset,
    guest_input_chain, nat_ruleset, nat6_ruleset, priority,
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
    /// NAT66, one masquerade per network that routes IPv6 out of this node.
    pub nat6: Vec<Nat6Rule>,
    /// Port forwards to local guests, as destination NAT.
    pub dnat: Vec<DnatRule>,
    /// Destinations guests never reach through the host (the tailnet and
    /// other members' underlay addresses).
    pub blocked: Vec<Ipv4Net>,
    /// The IPv6 counterpart of `blocked`.
    pub blocked6: Vec<Ipv6Net>,
    /// Each bridge and the only interface it may forward out of.
    pub routed: Vec<Routed>,
}

/// A bridge's forwarding policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routed {
    pub bridge: String,
    /// The only interfaces it may forward out of: the uplinks of a NAT network, none for an
    /// isolated one.
    pub out: Vec<String>,
    /// Blocked destinations this bridge may still reach.
    pub allow: Vec<IpNet>,
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
    /// `ip prism-nat`, `ip6 prism-nat` (NAT66 and IPv6 forwards) and `bridge prism`.
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
            // Hairpinned forwards enter and leave the same bridge; they must
            // pass the per-bridge drop below.
            for f in &rules.dnat {
                let family = Family::Inet;
                if let Some(rule) = f.hairpin_rule(family) {
                    fwd.push(rule);
                }
            }
            // Replies to forwarded connections (hairpinned, or to clients
            // off the uplink) must pass the per-bridge drop below. New
            // connections from guests are still filtered by it.
            let mut forward_bridges: Vec<&str> =
                rules.dnat.iter().map(|f| f.bridge.as_str()).collect();
            forward_bridges.sort_unstable();
            forward_bridges.dedup();
            for bridge in forward_bridges {
                fwd.push(
                    Rule::new(Family::Inet)
                        .iifname(bridge)
                        .ct_state(CtState::ESTABLISHED | CtState::RELATED)
                        .accept(),
                );
            }
            for Routed { bridge, out, allow } in &rules.routed {
                for net in allow {
                    let rule = Rule::new(Family::Inet).iifname(bridge);
                    fwd.push(match net {
                        IpNet::V4(n) => rule.ip_daddr(*n).accept(),
                        IpNet::V6(n) => rule.ip6_daddr(*n).accept(),
                    });
                }
                for net in &rules.blocked {
                    fwd.push(
                        Rule::new(Family::Inet)
                            .iifname(bridge)
                            .ip_daddr(*net)
                            .drop(),
                    );
                }
                for net in &rules.blocked6 {
                    fwd.push(
                        Rule::new(Family::Inet)
                            .iifname(bridge)
                            .ip6_daddr(*net)
                            .drop(),
                    );
                }
                let rule = out
                    .iter()
                    .fold(Rule::new(Family::Inet).iifname(bridge), |r, uplink| {
                        r.oifname_ne(uplink)
                    });
                fwd.push(rule.drop());
            }
        }
        // Always replaced, so turning NAT66 and IPv6 forwards off removes their rules.
        vec![
            inet,
            nat_ruleset(&rules.nat, &rules.dnat),
            nat6_ruleset(&rules.nat6, &rules.dnat),
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

    fn hairpin_rules() -> GuestRules {
        GuestRules {
            dnat: vec![DnatRule {
                udp: false,
                host_addr: None,
                host_port: 80,
                guest: "fd00::5".parse().unwrap(),
                guest_port: 80,
                bridge: "ps100000".into(),
                uplinks: vec!["eth0".into()],
                local: true,
                hairpin: true,
                snat6: None,
            }],
            ..GuestRules::default()
        }
    }

    #[test]
    fn standalone_tables() {
        let rules = GuestRules {
            switches: vec![SwitchServices {
                bridge: "ps100000".into(),
                vip: "10.88.0.1".parse().unwrap(),
                vip6: None,
            }],
            blocked: vec!["100.64.0.0/10".parse().unwrap()],
            routed: vec![Routed {
                bridge: "ps100000".into(),
                out: vec!["eth0".into()],
                allow: vec!["100.64.9.0/24".parse().unwrap()],
            }],
            ..GuestRules::default()
        };
        let t = Firewall::tables(&rules, None);
        assert_eq!(t.len(), 4);
        assert!(Firewall::tables(&hairpin_rules(), None).len() >= 4);
        let fwd = t[0].chains.iter().find(|c| c.name == "forward").unwrap();
        assert_eq!(fwd.rules.len(), 3);
        assert!(t[0].chains.iter().any(|c| c.name == "input"));
    }
}
