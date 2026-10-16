//! What one node's data plane should look like, computed from replicated
//! state. Pure, so it is tested without netlink.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use ipnet::{Ipv4Net, Ipv6Net};
use prism_net::MacAddr;
use prism_net::dhcp::Lease;
use prism_net::link::VxlanFdb;
use prism_net::ra::RaConfig;
use uuid::Uuid;

use super::firewall::{GuestRules, Routed};
use super::{NetworkDoc, NetworkKind, VLAN_OVERHEAD, ipam, switch_mtu};
use crate::cluster::model::MemberDoc;
use crate::spec::{NicNetwork, Proto, VmSpec};
use prism_net::link::PortVlans;
use prism_net::nft::Ipv6Source;

const LEASE_TIME: Duration = Duration::from_secs(12 * 3600);
/// How long guests keep a node as their default IPv6 router.
const ROUTER_LIFETIME: Duration = Duration::from_secs(1800);
/// The Tailscale ULA range, which guests never reach through the host.
const TAILNET6: Ipv6Net =
    Ipv6Net::new_assert(Ipv6Addr::new(0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 0), 48);

/// Everything the plan depends on.
#[derive(Debug, Clone)]
pub struct PlanInput<'a> {
    pub node: Uuid,
    pub networks: &'a [NetworkDoc],
    pub vms: &'a [VmSpec],
    pub members: &'a [MemberDoc],
    /// The cluster's `wg-prism` MTU; `None` while standalone.
    pub wg_mtu: Option<u16>,
    /// This node's uplink (default-route interface), for masquerade.
    pub uplink: Option<String>,
    /// This node's IPv6 default-route interface, for NAT66.
    pub uplink6: Option<String>,
}

/// One network on this node.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchPlan {
    pub network: Uuid,
    pub bridge: String,
    pub mtu: u32,
    /// This node's address on the bridge, with the subnet's prefix.
    pub address: Option<Ipv4Net>,
    /// The VXLAN device and its peers; `None` while standalone.
    pub vxlan: Option<VxlanPlan>,
    /// DHCP leases for NICs of VMs on this node (empty when DHCP is off).
    pub leases: HashMap<MacAddr, Lease>,
    pub dhcp: bool,
    /// Every VLAN any NIC on the network uses, cluster-wide: the VXLAN
    /// port carries these tagged. Empty means the network has no VLANs.
    pub vlans: BTreeSet<u16>,
    /// Local taps on a VLAN-aware network and the VLAN membership of each
    /// (access on VLAN 1 when the NIC has none).
    pub ports: Vec<(String, PortVlans)>,
    /// IPv6 guests without a network prefix that forwards target: the host
    /// needs a route to each.
    pub routes6: Vec<Ipv6Addr>,
    /// This node's link-local address on the bridge, the source of router
    /// advertisements; set when the network has an IPv6 prefix or `routes6`
    /// is not empty.
    pub gateway6: Option<Ipv6Addr>,
    /// This node's ULA address on the bridge, with the network's `/64`.
    pub address6: Option<Ipv6Net>,
    /// The router advertisements to send; `None` while IPv6 is off.
    pub ra: Option<RaConfig>,
    /// The zone origin, `<network>.prism`.
    pub domain: String,
    /// Every VM on the network, by name.
    pub hosts: HashMap<String, Vec<IpAddr>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VxlanPlan {
    pub name: String,
    pub vni: u32,
    pub local: Ipv6Addr,
    pub fdb: VxlanFdb,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodePlan {
    pub switches: Vec<SwitchPlan>,
    pub firewall: GuestRules,
    /// Some network masquerades here, so IPv4 forwarding must be on.
    pub forwarding: bool,
    /// A forward targets an IPv6 guest, so IPv6 forwarding must be on.
    pub forwarding6: bool,
    /// Bridges that need `route_localnet`, for forwards from loopback.
    pub local_bridges: Vec<String>,
}

/// A link-local address unique to `node`, for its bridges.
fn link_local_of(node: Uuid) -> Ipv6Addr {
    let b = node.as_bytes();
    let mut o = [0u8; 16];
    o[0] = 0xfe;
    o[1] = 0x80;
    o[8..].copy_from_slice(&b[8..]);
    Ipv6Addr::from(o)
}

/// Whether `vm` is (or is about to be) running on `node`.
fn runs_on(vm: &VmSpec, node: Uuid) -> bool {
    vm.node == node || vm.migrating.is_some_and(|m| m.dst == node)
}

/// The tap libvirt creates for a NIC (see `render`).
pub fn tap_name(nic: Uuid) -> String {
    format!("pn{}", &nic.simple().to_string()[..8])
}

pub fn plan(input: &PlanInput<'_>) -> NodePlan {
    let mesh: BTreeMap<Uuid, Ipv6Addr> = input
        .members
        .iter()
        .map(|m| (m.node_id, m.mesh_addr))
        .collect();
    let local_mesh = mesh.get(&input.node).copied();
    let base_mtu = switch_mtu(input.wg_mtu);
    let mut out = NodePlan::default();
    for net in input.networks {
        let vlans = net_vlans(net.id, input.vms);
        let mtu = if vlans.is_empty() {
            base_mtu
        } else {
            base_mtu.saturating_sub(VLAN_OVERHEAD)
        };
        let gateway = net.gateways.get(&input.node).copied();
        let address = gateway.map(|g| Ipv4Net::new_assert(g, net.subnet.prefix_len()));
        let vxlan = local_mesh.map(|local| VxlanPlan {
            name: net.vxlan(),
            vni: net.vni,
            local,
            fdb: VxlanFdb {
                flood: mesh
                    .iter()
                    .filter(|(n, _)| **n != input.node)
                    .map(|(_, a)| IpAddr::V6(*a))
                    .collect(),
                macs: BTreeMap::new(),
            },
        });
        let address6 = net
            .subnet6
            .map(|p| Ipv6Net::new_assert(ipam::node_addr6(p, input.node), 64));
        let router6 = net.routes_v6(input.node) && input.uplink6.is_some();
        let ra = net.subnet6.map(|prefix| RaConfig {
            interface: net.bridge(),
            source: link_local_of(input.node),
            prefix,
            router_lifetime: if router6 {
                ROUTER_LIFETIME
            } else {
                Duration::ZERO
            },
            mtu: Some(mtu),
            dns: address6.iter().map(Ipv6Net::addr).collect(),
            domain: Some(net.domain()),
        });
        let mut sw = SwitchPlan {
            network: net.id,
            bridge: net.bridge(),
            mtu,
            address,
            vxlan,
            leases: HashMap::new(),
            dhcp: net.dhcp,
            vlans,
            ports: Vec::new(),
            routes6: Vec::new(),
            gateway6: None,
            address6,
            ra,
            domain: net.domain(),
            hosts: HashMap::new(),
        };
        let gateway6 = link_local_of(input.node);
        for vm in input.vms {
            for nic in &vm.nics {
                let NicNetwork::Network {
                    network,
                    ipv4,
                    ipv6,
                    egress,
                    forwards,
                    ..
                } = &nic.network
                else {
                    continue;
                };
                if *network != net.id {
                    continue;
                }
                let Ok(mac) = nic.mac.parse::<MacAddr>() else {
                    continue;
                };
                let v6 = net.guest_v6(*ipv6, &nic.mac);
                if let Some(ip) = ipv4 {
                    sw.hosts
                        .entry(vm.name.clone())
                        .or_default()
                        .push(IpAddr::V4(*ip));
                }
                if let Some(ip) = v6 {
                    sw.hosts
                        .entry(vm.name.clone())
                        .or_default()
                        .push(IpAddr::V6(ip));
                }
                if runs_on(vm, input.node) {
                    if !sw.vlans.is_empty()
                        && let Some(pv) = nic.network.port_vlans()
                    {
                        sw.ports.push((tap_name(nic.id), pv));
                    }
                    out.firewall.ports.push(prism_net::nft::GuestPort {
                        tap: tap_name(nic.id),
                        mac,
                        ipv4: *ipv4,
                        ipv6: match (net.subnet6, v6) {
                            (Some(prefix), Some(addr)) => Ipv6Source::Only { addr, prefix },
                            _ => Ipv6Source::Any,
                        },
                        trunk: matches!(nic.network.port_vlans(), Some(PortVlans::Trunk { .. })),
                    });
                    // Forwards need this node to route into the network.
                    if gateway.is_some() {
                        for f in forwards {
                            let guests = [
                                ipv4.map(IpAddr::V4)
                                    .filter(|_| !matches!(f.host_addr, Some(IpAddr::V6(_)))),
                                v6.map(IpAddr::V6)
                                    .filter(|_| !matches!(f.host_addr, Some(IpAddr::V4(_)))),
                            ];
                            for guest in guests.into_iter().flatten() {
                                out.forwarding |= guest.is_ipv4();
                                out.forwarding6 |= guest.is_ipv6();
                                if let IpAddr::V6(g) = guest
                                    && net.subnet6.is_none()
                                    && !sw.routes6.contains(&g)
                                {
                                    sw.routes6.push(g);
                                }
                                // Loopback destinations are only routable to the
                                // bridge with route_localnet.
                                if f.local && f.host_addr.is_none() && guest.is_ipv4() {
                                    out.local_bridges.push(sw.bridge.clone());
                                }
                                out.firewall.dnat.push(prism_net::nft::DnatRule {
                                    udp: f.proto == Proto::Udp,
                                    host_addr: f.host_addr,
                                    host_port: f.host_port,
                                    guest,
                                    guest_port: f.guest_port,
                                    bridge: sw.bridge.clone(),
                                    local: f.local,
                                    hairpin: f.hairpin,
                                    snat6: Some(address6.map_or(gateway6, |a| a.addr())),
                                });
                            }
                        }
                    }
                    if let (Some(ip), true) = (ipv4, net.dhcp) {
                        sw.leases
                            .insert(mac, lease(net, vm, *ip, *egress, gateway, mtu));
                    }
                } else if let (Some(v), Some(addr)) = (&mut sw.vxlan, mesh.get(&vm.node)) {
                    v.fdb.macs.insert(mac, IpAddr::V6(*addr));
                }
            }
        }
        if !sw.routes6.is_empty() || net.subnet6.is_some() {
            sw.gateway6 = Some(gateway6);
        }
        if let Some(g) = gateway {
            out.firewall.switches.push(prism_net::nft::SwitchServices {
                bridge: sw.bridge.clone(),
                vip: g,
                vip6: address6.map(|a| a.addr()),
            });
        }
        let nat = net.kind == NetworkKind::Nat;
        let mut out_ifaces: Vec<String> = Vec::new();
        if nat {
            out_ifaces.extend(input.uplink.clone());
            if let Some(u6) = &input.uplink6
                && !out_ifaces.contains(u6)
            {
                out_ifaces.push(u6.clone());
            }
        }
        if let (true, Some(uplink)) = (nat, &input.uplink) {
            out.forwarding = true;
            out.firewall.nat.push(prism_net::nft::NatRule {
                source: net.subnet,
                egress: uplink.clone(),
            });
        }
        if let (true, Some(prefix), Some(uplink6)) = (router6, net.subnet6, &input.uplink6) {
            out.forwarding6 = true;
            out.firewall.nat6.push(prism_net::nft::Nat6Rule {
                source: prefix,
                egress: uplink6.clone(),
            });
        }
        out.firewall.routed.push(Routed {
            bridge: sw.bridge.clone(),
            out: out_ifaces,
            allow: if nat && input.uplink.is_some() {
                net.allow_prefixes.clone()
            } else {
                Vec::new()
            },
        });
        out.switches.push(sw);
    }
    out.firewall.blocked = blocked_destinations(input.members);
    out.firewall.blocked6 = blocked_destinations6(input.members);
    out
}

/// The VLANs NICs on `network` use.
fn net_vlans(network: Uuid, vms: &[VmSpec]) -> BTreeSet<u16> {
    vms.iter()
        .flat_map(|v| &v.nics)
        .filter_map(|n| match &n.network {
            NicNetwork::Network {
                network: id,
                vlan,
                trunk,
                ..
            } if *id == network => Some(vlan.iter().chain(trunk).copied().collect::<Vec<_>>()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn lease(
    net: &NetworkDoc,
    vm: &VmSpec,
    ip: Ipv4Addr,
    egress: Option<Uuid>,
    gateway: Option<Ipv4Addr>,
    mtu: u32,
) -> Lease {
    Lease {
        ip,
        prefix_len: net.subnet.prefix_len(),
        router: net.router_for(vm.node, egress),
        dns: gateway.into_iter().collect(),
        domain: Some(net.domain()),
        hostname: Some(vm.name.clone()),
        mtu: u16::try_from(mtu).ok(),
        routes: Vec::new(),
        lease_time: LEASE_TIME,
    }
}

/// Guests never reach the tailnet or another member's underlay addresses
/// through masquerade.
fn blocked_destinations(members: &[MemberDoc]) -> Vec<Ipv4Net> {
    let mut out = vec![super::ipam::CGNAT];
    for m in members {
        for ep in &m.endpoints {
            let host = ep.rsplit_once(':').map_or(ep.as_str(), |(h, _)| h);
            if let Ok(ip) = host.parse::<Ipv4Addr>()
                && !out.iter().any(|n| n.contains(&ip))
            {
                out.push(Ipv4Net::from(ip));
            }
        }
    }
    out
}

/// The IPv6 counterpart of [`blocked_destinations`]: the tailnet's ULA range and the members'
/// IPv6 underlay endpoints.
fn blocked_destinations6(members: &[MemberDoc]) -> Vec<Ipv6Net> {
    let mut out = vec![TAILNET6];
    for m in members {
        for ep in &m.endpoints {
            if let Ok(std::net::SocketAddr::V6(a)) = ep.parse()
                && !out.iter().any(|n| n.contains(a.ip()))
            {
                out.push(Ipv6Net::from(*a.ip()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::model::MemberState;
    use crate::spec::NicSpec;
    use crate::spec::tests::sample_spec;

    const A: Uuid = Uuid::from_u128(1);
    const B: Uuid = Uuid::from_u128(2);

    fn net() -> NetworkDoc {
        NetworkDoc {
            id: Uuid::from_u128(10),
            name: "lab".into(),
            kind: NetworkKind::Nat,
            vni: 100_000,
            subnet: "10.88.0.0/24".parse().unwrap(),
            gateways: BTreeMap::from([
                (A, Ipv4Addr::new(10, 88, 0, 1)),
                (B, Ipv4Addr::new(10, 88, 0, 2)),
            ]),
            egress: None,
            dhcp: true,
            allow_prefixes: Vec::new(),
            subnet6: None,
            nat66: false,
            created: 0,
        }
    }

    fn member(id: Uuid, raft: u64) -> MemberDoc {
        MemberDoc {
            node_id: id,
            raft_id: raft,
            name: format!("n{raft}"),
            wg_pubkey: String::new(),
            mesh_addr: format!("fd00::{raft}").parse().unwrap(),
            endpoints: vec![format!("192.168.1.{raft}:51820")],
            state: MemberState::Active,
            voter: raft == 1,
            version: String::new(),
            feature_level: 1,
            joined: 0,
        }
    }

    fn vm(name: &str, node: Uuid, mac: &str, ip: u8, egress: Option<Uuid>) -> VmSpec {
        let mut v = sample_spec(name);
        v.node = node;
        v.nics = vec![NicSpec {
            id: Uuid::new_v4(),
            mac: mac.into(),
            network: NicNetwork::Network {
                network: Uuid::from_u128(10),
                ipv4: Some(Ipv4Addr::new(10, 88, 0, ip)),
                ipv6: None,
                egress,
                vlan: None,
                trunk: Vec::new(),
                forwards: Vec::new(),
            },
            boot_order: None,
        }];
        v
    }

    fn forward_plan(node: Uuid, net: &NetworkDoc, vm: &VmSpec) -> NodePlan {
        plan(&PlanInput {
            node,
            networks: std::slice::from_ref(net),
            vms: std::slice::from_ref(vm),
            members: &[],
            wg_mtu: None,
            uplink: Some("eth0".into()),
            uplink6: None,
        })
    }

    #[test]
    fn forwards_follow_the_running_node() {
        let (src, dst) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut guest = vm("a", src, "52:54:00:00:00:01", 5, None);
        if let NicNetwork::Network { forwards, .. } = &mut guest.nics[0].network {
            forwards.push(crate::spec::PortForward {
                proto: Proto::Tcp,
                host_addr: None,
                host_port: 2222,
                guest_port: 22,
                local: true,
                hairpin: true,
            });
        }
        let mut doc = net();
        doc.gateways.insert(src, Ipv4Addr::new(10, 88, 0, 1));
        doc.gateways.insert(dst, Ipv4Addr::new(10, 88, 0, 2));
        let p = forward_plan(src, &doc, &guest);
        assert_eq!(p.firewall.dnat.len(), 1);
        assert_eq!(
            p.firewall.dnat[0].guest,
            IpAddr::from(Ipv4Addr::new(10, 88, 0, 5))
        );
        assert_eq!(p.local_bridges.len(), 1);
        assert!(p.forwarding);
        assert_eq!(forward_plan(dst, &doc, &guest).firewall.dnat.len(), 0);
        guest.migrating = Some(crate::spec::Migration { src, dst });
        assert_eq!(forward_plan(dst, &doc, &guest).firewall.dnat.len(), 1);
    }

    #[test]
    fn standalone() {
        let nets = [net()];
        let vms = [vm("web", A, "52:54:00:00:00:01", 3, None)];
        let p = plan(&PlanInput {
            node: A,
            networks: &nets,
            vms: &vms,
            members: &[],
            wg_mtu: None,
            uplink: Some("eth0".into()),
            uplink6: None,
        });
        assert!(p.forwarding);
        let sw = &p.switches[0];
        assert_eq!(sw.bridge, "ps100000");
        assert_eq!(sw.mtu, 1500);
        assert_eq!(sw.address, Some("10.88.0.1/24".parse().unwrap()));
        assert!(sw.vxlan.is_none());
        let lease = &sw.leases[&"52:54:00:00:00:01".parse().unwrap()];
        assert_eq!(lease.ip, Ipv4Addr::new(10, 88, 0, 3));
        assert_eq!(lease.router, Some(Ipv4Addr::new(10, 88, 0, 1)));
        assert_eq!(lease.dns, vec![Ipv4Addr::new(10, 88, 0, 1)]);
        assert_eq!(lease.domain.as_deref(), Some("lab.prism"));
        assert_eq!(lease.mtu, Some(1500));
        assert_eq!(
            sw.hosts["web"],
            vec![IpAddr::V4(Ipv4Addr::new(10, 88, 0, 3))]
        );
        assert_eq!(p.firewall.nat[0].egress, "eth0");
        assert_eq!(p.firewall.ports[0].tap, tap_name(vms[0].nics[0].id));
        assert_eq!(p.firewall.switches[0].vip, Ipv4Addr::new(10, 88, 0, 1));
    }

    #[test]
    fn clustered_fdb_and_egress() {
        let nets = [net()];
        let members = [member(A, 1), member(B, 2)];
        let vms = [
            vm("here", A, "52:54:00:00:00:01", 3, Some(B)),
            vm("there", B, "52:54:00:00:00:02", 4, None),
        ];
        let p = plan(&PlanInput {
            node: A,
            networks: &nets,
            vms: &vms,
            members: &members,
            wg_mtu: Some(1420),
            uplink: None,
            uplink6: None,
        });
        let sw = &p.switches[0];
        assert_eq!(sw.mtu, 1350);
        let vx = sw.vxlan.as_ref().unwrap();
        assert_eq!(vx.local, "fd00::1".parse::<Ipv6Addr>().unwrap());
        assert_eq!(
            vx.fdb.flood.iter().copied().collect::<Vec<_>>(),
            vec!["fd00::2".parse::<IpAddr>().unwrap()]
        );
        assert_eq!(
            vx.fdb.macs[&"52:54:00:00:00:02".parse().unwrap()],
            "fd00::2".parse::<IpAddr>().unwrap()
        );
        // Only local VMs get leases; the override routes through B.
        assert_eq!(sw.leases.len(), 1);
        let lease = sw.leases.values().next().unwrap();
        assert_eq!(lease.router, Some(Ipv4Addr::new(10, 88, 0, 2)));
        assert_eq!(sw.hosts.len(), 2);
        assert!(!p.forwarding, "no uplink, no masquerade");
        assert!(
            p.firewall
                .blocked
                .contains(&"192.168.1.2/32".parse().unwrap())
        );
    }

    #[test]
    fn isolated_has_no_router() {
        let mut n = net();
        n.kind = NetworkKind::Isolated;
        let nets = [n];
        let vms = [vm("a", A, "52:54:00:00:00:01", 3, None)];
        let p = plan(&PlanInput {
            node: A,
            networks: &nets,
            vms: &vms,
            members: &[],
            wg_mtu: None,
            uplink: Some("eth0".into()),
            uplink6: None,
        });
        assert_eq!(p.firewall.nat, vec![]);
        assert!(!p.forwarding);
        assert_eq!(p.switches[0].leases.values().next().unwrap().router, None);
    }

    fn v6_net() -> NetworkDoc {
        let mut n = net();
        n.subnet6 = Some("fd01:203:405:7::/64".parse().unwrap());
        n.nat66 = true;
        n
    }

    fn plan6(node: Uuid, net: &NetworkDoc, vms: &[VmSpec], uplink6: Option<&str>) -> NodePlan {
        plan(&PlanInput {
            node,
            networks: std::slice::from_ref(net),
            vms,
            members: &[],
            wg_mtu: None,
            uplink: Some("eth0".into()),
            uplink6: uplink6.map(Into::into),
        })
    }

    #[test]
    fn ipv6_networks_advertise_and_nat() {
        let vms = [vm("web", A, "52:54:00:00:00:01", 3, None)];
        let p = plan6(A, &v6_net(), &vms, Some("eth1"));
        let sw = &p.switches[0];
        let addr: Ipv6Addr = "fd01:203:405:7:5054:ff:fe00:1".parse().unwrap();
        assert_eq!(sw.hosts["web"].len(), 2);
        assert!(sw.hosts["web"].contains(&IpAddr::V6(addr)));
        assert!(sw.routes6.is_empty(), "the /64 route covers the guest");
        assert!(sw.gateway6.is_some());
        let own = sw.address6.unwrap();
        assert_eq!(own.prefix_len(), 64);
        let ra = sw.ra.as_ref().unwrap();
        assert_eq!(ra.router_lifetime, ROUTER_LIFETIME);
        assert_eq!(ra.dns, vec![own.addr()]);
        assert_eq!(ra.source, sw.gateway6.unwrap());
        assert_eq!(p.firewall.nat6[0].egress, "eth1");
        assert!(p.forwarding6);
        assert_eq!(p.firewall.switches[0].vip6, Some(own.addr()));
        assert_eq!(
            p.firewall.ports[0].ipv6,
            Ipv6Source::Only {
                addr,
                prefix: "fd01:203:405:7::/64".parse().unwrap()
            }
        );
        assert_eq!(p.firewall.routed[0].out, vec!["eth0", "eth1"]);
        assert!(p.firewall.blocked6.contains(&TAILNET6));
    }

    #[test]
    fn ipv6_without_an_uplink_is_not_a_router() {
        let vms = [vm("web", A, "52:54:00:00:00:01", 3, None)];
        let p = plan6(A, &v6_net(), &vms, None);
        assert_eq!(
            p.switches[0].ra.as_ref().unwrap().router_lifetime,
            Duration::ZERO
        );
        assert_eq!(p.firewall.nat6, vec![]);
        assert!(!p.forwarding6);
        // The egress node alone routes.
        let mut n = v6_net();
        n.egress = Some(B);
        let p = plan6(A, &n, &vms, Some("eth1"));
        assert_eq!(
            p.switches[0].ra.as_ref().unwrap().router_lifetime,
            Duration::ZERO
        );
        assert_eq!(p.firewall.nat6, vec![]);
    }

    #[test]
    fn ipv6_forwards_use_the_slaac_address() {
        let mut guest = vm("web", A, "52:54:00:00:00:01", 3, None);
        if let NicNetwork::Network { forwards, .. } = &mut guest.nics[0].network {
            forwards.push(crate::spec::PortForward {
                proto: Proto::Tcp,
                host_addr: Some("2001:db8::1".parse().unwrap()),
                host_port: 8080,
                guest_port: 80,
                local: false,
                hairpin: false,
            });
        }
        let p = plan6(A, &v6_net(), &[guest], Some("eth1"));
        let rule = &p.firewall.dnat[0];
        assert_eq!(
            rule.guest,
            "fd01:203:405:7:5054:ff:fe00:1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(rule.snat6, Some(p.switches[0].address6.unwrap().addr()));
        assert_eq!(p.switches[0].routes6, Vec::<Ipv6Addr>::new());
    }

    #[test]
    fn networks_without_ipv6_stay_as_they_were() {
        let vms = [vm("web", A, "52:54:00:00:00:01", 3, None)];
        let p = plan6(A, &net(), &vms, Some("eth1"));
        let sw = &p.switches[0];
        assert!(sw.ra.is_none() && sw.address6.is_none() && sw.gateway6.is_none());
        assert_eq!(p.firewall.ports[0].ipv6, Ipv6Source::Any);
        assert_eq!(p.firewall.nat6, vec![]);
        assert_eq!(sw.hosts["web"].len(), 1);
    }
}
