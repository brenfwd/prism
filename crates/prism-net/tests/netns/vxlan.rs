//! A VXLAN switch spanning two namespaces over the mesh.

use crate::common::{Mesh, Ns, boundary_cfg, mesh_addr, require_root, sh};
use prism_net::Netlink;
use prism_net::link::{
    BridgeConfig, FdbChanges, PortVlan, PortVlans, VXLAN_PORT, VxlanConfig, VxlanFdb,
};
use prism_net::nft::boundary_ruleset;
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv6Addr};

async fn switch_side(nl: &Netlink, local: Ipv6Addr) {
    nl.ensure_bridge("ps1", &BridgeConfig { mtu: 1346 })
        .await
        .unwrap();
    nl.ensure_vxlan(
        "vx1",
        &VxlanConfig {
            vni: 100_001,
            local,
            port: VXLAN_PORT,
            mtu: 1350,
            bridge: Some("ps1".into()),
        },
    )
    .await
    .unwrap();
    nl.set_port_vlans(
        "vx1",
        &PortVlans::Trunk {
            native: None,
            tagged: [10].into_iter().collect(),
        },
    )
    .await
    .unwrap();
    nl.set_controller("pn1", Some("ps1")).await.unwrap();
    nl.set_mtu("pn1", 1346).await.unwrap();
    nl.set_link_up("pn1").await.unwrap();
    nl.set_port_vlans("pn1", &PortVlans::Access(10))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs root and network namespaces"]
async fn vxlan_switch_over_mesh() {
    require_root();
    let m = Mesh::new("pnt-vx").await;
    for ns in [&m.a, &m.b] {
        ns.nft()
            .replace_table(&boundary_ruleset(&boundary_cfg()))
            .unwrap();
    }
    let guests = [Ns::new("pnt-vx-ga"), Ns::new("pnt-vx-gb")];
    for (i, (host, guest)) in [(&m.a, &guests[0]), (&m.b, &guests[1])]
        .into_iter()
        .enumerate()
    {
        sh(&format!(
            "ip link add pn1 netns {} type veth peer name eth0 netns {}",
            host.name(),
            guest.name()
        ));
        guest.exec(&format!("ip addr add 10.20.0.{}/24 dev eth0", 11 + i));
        guest.exec("ip link set eth0 mtu 1346 up");
        switch_side(m.nl(i), mesh_addr(i as u16 + 1)).await;
        // Idempotent.
        let idx = m.nl(i).link_index("vx1").await.unwrap();
        switch_side(m.nl(i), mesh_addr(i as u16 + 1)).await;
        assert_eq!(m.nl(i).link_index("vx1").await.unwrap(), idx);
    }
    assert_eq!(
        m.nla.port_vlans("pn1").await.unwrap(),
        vec![PortVlan {
            vid: 10,
            pvid: true,
            untagged: true
        }]
    );
    assert_eq!(
        m.nla.port_vlans("vx1").await.unwrap(),
        vec![PortVlan {
            vid: 10,
            pvid: false,
            untagged: false
        }]
    );

    // Head-end replication only: ARP floods to the other VTEP.
    for i in 0..2 {
        let fdb = VxlanFdb {
            flood: [IpAddr::V6(mesh_addr(2 - i as u16))].into_iter().collect(),
            macs: BTreeMap::new(),
        };
        let ch = m.nl(i).sync_vxlan_fdb("vx1", &fdb).await.unwrap();
        assert_eq!(ch.added, 1);
        assert_eq!(m.nl(i).vxlan_fdb("vx1").await.unwrap(), fdb);
    }
    guests[0].exec("ping -c 3 -i 0.2 -W 2 10.20.0.12");
    // Full switch MTU with a VLAN: 1346 - 20 (IPv4) - 8 (ICMP).
    guests[0].exec("ping -c 1 -W 2 -M do -s 1318 10.20.0.12");

    // Unicast entries for the remote guest MAC, diffed: second sync is a no-op.
    let gb = guests[1].netlink();
    let gb_mac = gb.link("eth0").await.unwrap().unwrap().mac.unwrap();
    let mut fdb = VxlanFdb {
        flood: [IpAddr::V6(mesh_addr(2))].into_iter().collect(),
        macs: [(gb_mac, IpAddr::V6(mesh_addr(2)))].into_iter().collect(),
    };
    let ch = m.nla.sync_vxlan_fdb("vx1", &fdb).await.unwrap();
    assert_eq!((ch.added, ch.removed), (1, 0));
    assert_eq!(m.nla.vxlan_fdb("vx1").await.unwrap(), fdb);
    assert_eq!(
        m.nla.sync_vxlan_fdb("vx1", &fdb).await.unwrap(),
        FdbChanges::default()
    );
    println!("{}", m.a.exec("bridge fdb show dev vx1"));
    guests[0].exec("ping -c 2 -i 0.2 -W 2 10.20.0.12");

    // Pointing the MAC at a wrong VTEP breaks unicast; fixing it restores it.
    fdb.macs.insert(gb_mac, IpAddr::V6(mesh_addr(9)));
    assert_eq!(m.nla.sync_vxlan_fdb("vx1", &fdb).await.unwrap().replaced, 1);
    assert!(!guests[0].exec_ok("ping -c 1 -W 1 10.20.0.12"));
    fdb.macs.insert(gb_mac, IpAddr::V6(mesh_addr(2)));
    m.nla.sync_vxlan_fdb("vx1", &fdb).await.unwrap();
    guests[0].exec("ping -c 1 -W 2 10.20.0.12");

    // Removing everything empties the FDB.
    let ch = m
        .nla
        .sync_vxlan_fdb("vx1", &VxlanFdb::default())
        .await
        .unwrap();
    assert_eq!(ch.removed, 2);
    assert_eq!(m.nla.vxlan_fdb("vx1").await.unwrap(), VxlanFdb::default());

    // Changing the VNI recreates the device.
    let cfg = VxlanConfig {
        vni: 100_002,
        local: mesh_addr(1),
        port: VXLAN_PORT,
        mtu: 1350,
        bridge: Some("ps1".into()),
    };
    let before = m.nla.link_index("vx1").await.unwrap();
    let after = m.nla.ensure_vxlan("vx1", &cfg).await.unwrap();
    assert_ne!(before, after);
    assert!(m.a.exec("ip -d link show vx1").contains("id 100002"));
    assert!(m.nla.delete_link("vx1").await.unwrap());
    assert!(!m.nla.delete_link("vx1").await.unwrap());
}
