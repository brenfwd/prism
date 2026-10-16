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

mod learned;

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
    /// IPv6 addresses guests were seen using, by NIC MAC (see `GuestAddrs`).
    pub learned: &'a HashMap<MacAddr, Vec<Ipv6Addr>>,
}

/// One network on this node.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchPlan {
    pub network: Uuid,
    pub bridge: String,
    pub mtu: u32,
    /// This node's address on the bridge, with the subnet's prefix.
    pub address: Option<Ipv4Net>,
    /// The VLANs with a subnet of their own: an interface on the bridge each.
    pub vlan_nets: Vec<VlanPlan>,
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

/// One VLAN with its own subnet on a switch: the interface that carries it on this node, with
/// its gateway and DHCP scope. DHCP, DNS and the MTU follow the switch.
#[derive(Debug, Clone, PartialEq)]
pub struct VlanPlan {
    pub vlan: u16,
    /// The VLAN interface on the bridge, `ps<vni>.<vid>`.
    pub interface: String,
    /// This node's address on the interface, with the VLAN subnet's prefix.
    pub address: Option<Ipv4Net>,
    /// DHCP leases for NICs of VMs on this node that sit on the VLAN.
    pub leases: HashMap<MacAddr, Lease>,
    /// Every VM with a NIC on the VLAN, by name.
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
        let mut vlans = net_vlans(net.id, input.vms);
        vlans.extend(net.vlan_subnets.keys());
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
            vlan_nets: net
                .vlan_subnets
                .iter()
                .map(|(vid, vs)| VlanPlan {
                    vlan: *vid,
                    interface: net.interface_of(Some(*vid)),
                    address: vs
                        .gateways
                        .get(&input.node)
                        .map(|g| Ipv4Net::new_assert(*g, vs.subnet.prefix_len())),
                    leases: HashMap::new(),
                    hosts: HashMap::new(),
                })
                .collect(),
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
        let reserved6 = learned::reserved(net, input.node, input.vms, input.members);
        let node_addrs6 = learned::gateways(net, input.node, input.members, link_local_of);
        for vm in input.vms {
            for nic in &vm.nics {
                let NicNetwork::Network {
                    network,
                    ipv4,
                    ipv6,
                    egress,
                    forwards,
                    vlan,
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
                let eui = net.guest_v6(*ipv6, &nic.mac);
                let learned = learned::for_nic(net, eui, input.learned.get(&mac), &reserved6);
                let v6s: Vec<Ipv6Addr> = if learned.is_empty() {
                    eui.into_iter().collect()
                } else {
                    learned
                };
                let v6 = v6s.first().copied();
                let segment = net.segment(*vlan);
                let iface = net.interface_of(segment);
                let nic_gateway = net.gateways_of(segment).get(&input.node).copied();
                if let Some(ip) = ipv4 {
                    sw.hosts
                        .entry(vm.name.clone())
                        .or_default()
                        .push(IpAddr::V4(*ip));
                    if let Some(vp) = sw.vlan_nets.iter_mut().find(|v| Some(v.vlan) == segment) {
                        vp.hosts
                            .entry(vm.name.clone())
                            .or_default()
                            .push(IpAddr::V4(*ip));
                    }
                }
                for ip in &v6s {
                    sw.hosts
                        .entry(vm.name.clone())
                        .or_default()
                        .push(IpAddr::V6(*ip));
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
                        ipv6: match net.subnet6 {
                            Some(prefix) if !v6s.is_empty() => Ipv6Source::Only {
                                addrs: learned::allowed(&v6s, eui),
                                prefix,
                                gateways: node_addrs6.clone(),
                            },
                            _ => Ipv6Source::Any,
                        },
                        trunk: matches!(nic.network.port_vlans(), Some(PortVlans::Trunk { .. })),
                    });
                    // Forwards need this node to route into the network.
                    if nic_gateway.is_some() {
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
                                    out.local_bridges.push(iface.clone());
                                }
                                out.firewall.dnat.push(prism_net::nft::DnatRule {
                                    udp: f.proto == Proto::Udp,
                                    host_addr: f.host_addr,
                                    host_port: f.host_port,
                                    guest,
                                    guest_port: f.guest_port,
                                    bridge: iface.clone(),
                                    uplinks: if guest.is_ipv4() {
                                        input.uplink.clone()
                                    } else {
                                        input.uplink6.clone()
                                    }
                                    .into_iter()
                                    .collect(),
                                    local: f.local,
                                    hairpin: f.hairpin,
                                    snat6: Some(address6.map_or(gateway6, |a| a.addr())),
                                });
                            }
                        }
                    }
                    if let (Some(ip), true) = (ipv4, net.dhcp) {
                        let l = lease(net, vm, *ip, *egress, (segment, nic_gateway), mtu);
                        match sw.vlan_nets.iter_mut().find(|v| Some(v.vlan) == segment) {
                            Some(vp) => vp.leases.insert(mac, l),
                            None => sw.leases.insert(mac, l),
                        };
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
        for vp in &sw.vlan_nets {
            if let Some(a) = vp.address {
                out.firewall.switches.push(prism_net::nft::SwitchServices {
                    bridge: vp.interface.clone(),
                    vip: a.addr(),
                    vip6: None,
                });
            }
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
        if let (true, Some(uplink)) = (nat, &input.uplink) {
            for vs in net.vlan_subnets.values() {
                out.firewall.nat.push(prism_net::nft::NatRule {
                    source: vs.subnet,
                    egress: uplink.clone(),
                });
            }
        }
        if let (true, Some(prefix), Some(uplink6)) = (router6, net.subnet6, &input.uplink6) {
            out.forwarding6 = true;
            out.firewall.nat6.push(prism_net::nft::Nat6Rule {
                source: prefix,
                egress: uplink6.clone(),
            });
        }
        let allow = if nat && input.uplink.is_some() {
            net.allow_prefixes.clone()
        } else {
            Vec::new()
        };
        for vp in &sw.vlan_nets {
            out.firewall.routed.push(Routed {
                bridge: vp.interface.clone(),
                out: out_ifaces.clone(),
                allow: allow.clone(),
            });
        }
        out.firewall.routed.push(Routed {
            bridge: sw.bridge.clone(),
            out: out_ifaces,
            allow,
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
    (segment, gateway): (Option<u16>, Option<Ipv4Addr>),
    mtu: u32,
) -> Lease {
    Lease {
        ip,
        prefix_len: net.subnet_of(segment).prefix_len(),
        router: net.router_for(segment, vm.node, egress),
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
mod tests;
