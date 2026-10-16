//! Per-VLAN subnets and leases through the state machine.

use std::collections::BTreeMap;

use super::*;
use crate::network::{NetworkKind, VlanSubnet};
use crate::spec::tests::sample_spec;
use crate::spec::{NicNetwork, NicSpec};

const NODE: Uuid = Uuid::from_u128(1);

fn state() -> State {
    State::new(StateDb::memory().unwrap(), NODE)
}

fn network(vlans: &[(u16, &str)]) -> NetworkDoc {
    NetworkDoc {
        id: Uuid::from_u128(10),
        name: "lab".into(),
        kind: NetworkKind::Nat,
        vni: 0,
        subnet: "10.88.0.0/24".parse().unwrap(),
        gateways: BTreeMap::default(),
        egress: None,
        dhcp: true,
        allow_prefixes: Vec::new(),
        subnet6: None,
        nat66: false,
        vlan_subnets: vlans
            .iter()
            .map(|(v, s)| {
                (
                    *v,
                    VlanSubnet {
                        subnet: s.parse().unwrap(),
                        gateways: BTreeMap::default(),
                    },
                )
            })
            .collect(),
        created: 0,
    }
}

async fn put_network(s: &State, doc: NetworkDoc) -> Result<(), StateError> {
    s.apply(Command::PutNetwork {
        network: Box::new(doc),
        now: 0,
    })
    .await
}

fn vm(name: &str, vlan: Option<u16>) -> VmSpec {
    let mut v = sample_spec(name);
    v.nics = vec![NicSpec {
        id: Uuid::new_v4(),
        mac: "52:54:00:00:00:01".into(),
        network: NicNetwork::Network {
            network: Uuid::from_u128(10),
            ipv4: None,
            ipv6: None,
            egress: None,
            vlan,
            trunk: Vec::new(),
            forwards: Vec::new(),
        },
        boot_order: None,
    }];
    v
}

async fn put_vm(s: &State, vm: VmSpec) -> Result<(), StateError> {
    s.apply(Command::PutVm {
        vm: Box::new(vm),
        expect_generation: None,
    })
    .await
}

fn ip_of(s: &State, name: &str) -> std::net::Ipv4Addr {
    let vm = s
        .vms()
        .unwrap()
        .into_iter()
        .find(|v| v.name == name)
        .unwrap();
    match &vm.nics[0].network {
        NicNetwork::Network { ipv4, .. } => ipv4.unwrap(),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn leases_come_from_the_subnet_of_the_nic_vlan() {
    let s = state();
    put_network(&s, network(&[(20, "10.88.1.0/24")]))
        .await
        .unwrap();
    let net = &s.networks().unwrap()[0];
    assert_eq!(
        net.vlan_subnets[&20].gateways[&NODE].to_string(),
        "10.88.1.1"
    );
    put_vm(&s, vm("plain", None)).await.unwrap();
    put_vm(&s, vm("tagged", Some(20))).await.unwrap();
    // A VLAN without a subnet shares the network's.
    put_vm(&s, vm("other", Some(30))).await.unwrap();
    assert_eq!(ip_of(&s, "plain").to_string(), "10.88.0.2");
    assert_eq!(ip_of(&s, "tagged").to_string(), "10.88.1.2");
    assert_eq!(ip_of(&s, "other").to_string(), "10.88.0.3");
}

#[tokio::test]
async fn a_new_vlan_subnet_moves_the_nics_on_it() {
    let s = state();
    put_network(&s, network(&[])).await.unwrap();
    put_vm(&s, vm("tagged", Some(20))).await.unwrap();
    assert_eq!(ip_of(&s, "tagged").to_string(), "10.88.0.2");
    let mut doc = s.networks().unwrap().remove(0);
    doc.vlan_subnets.insert(
        20,
        VlanSubnet {
            subnet: "10.88.1.0/24".parse().unwrap(),
            gateways: BTreeMap::default(),
        },
    );
    put_network(&s, doc).await.unwrap();
    assert_eq!(ip_of(&s, "tagged").to_string(), "10.88.1.2");
}

#[tokio::test]
async fn vlan_subnets_cannot_overlap_or_vanish_under_a_nic() {
    let s = state();
    let clash = put_network(&s, network(&[(20, "10.88.0.128/25")])).await;
    assert!(matches!(clash, Err(StateError::Conflict(_))));
    put_network(&s, network(&[(20, "10.88.1.0/24")]))
        .await
        .unwrap();
    put_vm(&s, vm("tagged", Some(20))).await.unwrap();
    let mut doc = s.networks().unwrap().remove(0);
    doc.vlan_subnets.get_mut(&20).unwrap().subnet = "10.88.2.0/24".parse().unwrap();
    assert!(matches!(
        put_network(&s, doc.clone()).await,
        Err(StateError::Conflict(_))
    ));
    doc.vlan_subnets.clear();
    assert!(matches!(
        put_network(&s, doc.clone()).await,
        Err(StateError::Conflict(_))
    ));
    let id = s.vms().unwrap()[0].id;
    s.apply(Command::DeleteVm { id }).await.unwrap();
    put_network(&s, doc).await.unwrap();
    assert!(s.networks().unwrap()[0].vlan_subnets.is_empty());
}

#[tokio::test]
async fn a_second_network_cannot_take_a_vlan_subnet() {
    let s = state();
    put_network(&s, network(&[(20, "10.88.1.0/24")]))
        .await
        .unwrap();
    let mut other = network(&[]);
    other.id = Uuid::from_u128(11);
    other.name = "other".into();
    other.subnet = "10.88.1.0/25".parse().unwrap();
    assert!(matches!(
        put_network(&s, other).await,
        Err(StateError::Conflict(_))
    ));
}
