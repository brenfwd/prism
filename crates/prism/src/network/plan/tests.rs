//! Tests for building the network plan from VM specs and cluster state.

use super::*;
use crate::cluster::model::MemberState;
use crate::network::VlanSubnet;
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
        vlan_subnets: BTreeMap::new(),
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
        learned: &HashMap::new(),
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
    assert_eq!(p.firewall.dnat[0].uplinks, vec!["eth0".to_string()]);
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
        learned: &HashMap::new(),
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
        learned: &HashMap::new(),
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
        learned: &HashMap::new(),
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
        learned: &HashMap::new(),
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
    let Ipv6Source::Only { gateways, .. } = &p.firewall.ports[0].ipv6 else {
        panic!("expected a source check");
    };
    // One bridge address and one link-local address for the node.
    assert_eq!(gateways.len(), 2);
    let gateways = gateways.clone();
    assert_eq!(
        p.firewall.ports[0].ipv6,
        Ipv6Source::Only {
            addrs: vec![addr],
            prefix: "fd01:203:405:7::/64".parse().unwrap(),
            gateways: gateways.clone(),
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

#[test]
fn vlan_subnets_get_their_own_gateway_scope_and_rules() {
    let mut doc = net();
    doc.vlan_subnets.insert(
        20,
        VlanSubnet {
            subnet: "10.88.1.0/24".parse().unwrap(),
            gateways: BTreeMap::from([(A, Ipv4Addr::new(10, 88, 1, 1))]),
        },
    );
    let mut guest = vm("db", A, "52:54:00:00:00:02", 3, None);
    if let NicNetwork::Network { vlan, ipv4, .. } = &mut guest.nics[0].network {
        *vlan = Some(20);
        *ipv4 = Some(Ipv4Addr::new(10, 88, 1, 7));
    }
    let plain = vm("web", A, "52:54:00:00:00:01", 3, None);
    let vms = [guest, plain];
    let p = plan(&PlanInput {
        node: A,
        networks: std::slice::from_ref(&doc),
        vms: &vms,
        members: &[],
        wg_mtu: None,
        uplink: Some("eth0".into()),
        uplink6: None,
        learned: &HashMap::new(),
    });
    let sw = &p.switches[0];
    assert_eq!(sw.vlans.iter().copied().collect::<Vec<_>>(), vec![20]);
    assert_eq!(sw.mtu, 1500 - VLAN_OVERHEAD);
    let v = &sw.vlan_nets[0];
    assert_eq!(v.interface, "ps100000.20");
    assert_eq!(v.address, Some("10.88.1.1/24".parse().unwrap()));
    // The VLAN guest leases from its own scope, the other from the bridge's.
    let lease = &v.leases[&"52:54:00:00:00:02".parse().unwrap()];
    assert_eq!(lease.ip, Ipv4Addr::new(10, 88, 1, 7));
    assert_eq!(lease.router, Some(Ipv4Addr::new(10, 88, 1, 1)));
    assert_eq!(lease.dns, vec![Ipv4Addr::new(10, 88, 1, 1)]);
    assert_eq!(lease.prefix_len, 24);
    assert_eq!(v.leases.len(), 1);
    assert_eq!(sw.leases.len(), 1);
    assert!(
        sw.leases
            .contains_key(&"52:54:00:00:00:01".parse().unwrap())
    );
    assert_eq!(v.hosts.len(), 1);
    // NAT, input and forward rules cover the VLAN interface.
    assert!(
        p.firewall
            .nat
            .iter()
            .any(|n| n.source == doc.vlan_subnets[&20].subnet)
    );
    assert!(
        p.firewall
            .switches
            .iter()
            .any(|s| s.bridge == "ps100000.20" && s.vip == Ipv4Addr::new(10, 88, 1, 1))
    );
    assert!(p.firewall.routed.iter().any(|r| r.bridge == "ps100000.20"));
    // Another node has no address on the VLAN until it has a gateway there.
    let other = plan(&PlanInput {
        node: B,
        networks: std::slice::from_ref(&doc),
        vms: &vms,
        members: &[],
        wg_mtu: None,
        uplink: Some("eth0".into()),
        uplink6: None,
        learned: &HashMap::new(),
    });
    assert_eq!(other.switches[0].vlan_nets[0].address, None);
}

fn plan_learned(vm: &VmSpec, learned: &[(&str, &[&str])], members: &[MemberDoc]) -> NodePlan {
    let learned: HashMap<MacAddr, Vec<Ipv6Addr>> = learned
        .iter()
        .map(|(m, a)| {
            (
                m.parse().unwrap(),
                a.iter().map(|x| x.parse().unwrap()).collect(),
            )
        })
        .collect();
    let mut guest = vm.clone();
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
    plan(&PlanInput {
        node: A,
        networks: &[v6_net()],
        vms: &[guest],
        members,
        wg_mtu: None,
        uplink: Some("eth0".into()),
        uplink6: Some("eth1".into()),
        learned: &learned,
    })
}

#[test]
fn learned_addresses_replace_the_slaac_one_in_dns_and_forwards() {
    let vm = vm("web", A, "52:54:00:00:00:01", 3, None);
    let stable: Ipv6Addr = "fd01:203:405:7:1111:2222:3333:4444".parse().unwrap();
    let slaac: Ipv6Addr = "fd01:203:405:7:5054:ff:fe00:1".parse().unwrap();
    let p = plan_learned(
        &vm,
        &[("52:54:00:00:00:01", &["fd01:203:405:7:1111:2222:3333:4444"])],
        &[],
    );
    let hosts = &p.switches[0].hosts["web"];
    assert!(hosts.contains(&IpAddr::V6(stable)));
    assert!(!hosts.contains(&IpAddr::V6(slaac)));
    assert_eq!(p.firewall.dnat[0].guest, IpAddr::V6(stable));
    // The firewall still lets the guest fall back to its SLAAC address.
    let Ipv6Source::Only { addrs, .. } = &p.firewall.ports[0].ipv6 else {
        panic!("expected a source check");
    };
    assert_eq!(addrs, &vec![stable, slaac]);
}

#[test]
fn learned_addresses_outside_the_prefix_or_reserved_are_ignored() {
    let vm = vm("web", A, "52:54:00:00:00:01", 3, None);
    let node_addr = ipam::node_addr6("fd01:203:405:7::/64".parse().unwrap(), A);
    let node_addr = node_addr.to_string();
    let p = plan_learned(
        &vm,
        &[("52:54:00:00:00:01", &["2001:db8::5", node_addr.as_str()])],
        &[],
    );
    // Nothing usable: the SLAAC address stays.
    let slaac: Ipv6Addr = "fd01:203:405:7:5054:ff:fe00:1".parse().unwrap();
    let Ipv6Source::Only { gateways, .. } = &p.firewall.ports[0].ipv6 else {
        panic!("expected a source check");
    };
    // One bridge address and one link-local address for the node.
    assert_eq!(gateways.len(), 2);
    let gateways = gateways.clone();
    assert_eq!(
        p.firewall.ports[0].ipv6,
        Ipv6Source::Only {
            addrs: vec![slaac],
            prefix: "fd01:203:405:7::/64".parse().unwrap(),
            gateways: gateways.clone(),
        }
    );
    assert_eq!(p.firewall.dnat[0].guest, IpAddr::V6(slaac));
}

#[test]
fn a_guest_using_its_slaac_address_keeps_one_entry() {
    let vm = vm("web", A, "52:54:00:00:00:01", 3, None);
    let p = plan_learned(
        &vm,
        &[("52:54:00:00:00:01", &["fd01:203:405:7:5054:ff:fe00:1"])],
        &[],
    );
    let Ipv6Source::Only { addrs, .. } = &p.firewall.ports[0].ipv6 else {
        panic!("expected a source check");
    };
    assert_eq!(addrs.len(), 1);
}
