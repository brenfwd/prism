//! A VLAN interface on a VLAN-filtering bridge.

use crate::common::{Ns, require_root};
use prism_net::link::BridgeConfig;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs root and network namespaces"]
async fn vlan_interface_on_a_bridge() {
    require_root();
    let ns = Ns::new("pnt-vi");
    let nl = ns.netlink();
    nl.ensure_bridge("ps1", &BridgeConfig { mtu: 1346 })
        .await
        .unwrap();
    nl.ensure_vlan_interface("ps1.20", "ps1", 20, 1346)
        .await
        .unwrap();
    // A second call corrects the MTU instead of failing.
    nl.ensure_vlan_interface("ps1.20", "ps1", 20, 1300)
        .await
        .unwrap();
    nl.ensure_address("ps1.20", "10.30.20.1/24".parse().unwrap())
        .await
        .unwrap();
    let link = nl.link("ps1.20").await.unwrap().unwrap();
    assert_eq!(link.kind.as_deref(), Some("vlan"));
    assert_eq!(link.mtu, Some(1300));
    assert!(link.up);
    // The VLAN is in the bridge's own table, so frames sent by the interface are not dropped.
    assert!(
        ns.exec("bridge vlan show dev ps1")
            .lines()
            .any(|l| l.trim() == "20")
    );
    assert!(
        nl.ensure_vlan_interface("ps1.1", "ps1", 4095, 1300)
            .await
            .is_err()
    );
    assert!(nl.delete_link("ps1.20").await.unwrap());
}
