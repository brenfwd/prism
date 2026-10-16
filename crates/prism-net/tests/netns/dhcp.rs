//! The embedded DHCP server answering `udhcpc` clients.

use crate::common::{Ns, require_root, sh};
use prism_net::MacAddr;
use prism_net::dhcp::{DhcpConfig, DhcpServer, Lease};
use prism_net::link::BridgeConfig;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

const UDHCPC_SCRIPT: &str = "#!/bin/sh\nenv > /tmp/prism-net-udhcpc-$interface-$1.env\n";

fn udhcpc(ns: &Ns) -> bool {
    ns.exec_ok(
        "busybox udhcpc -f -q -n -R -t 3 -T 1 -i eth0 -s /tmp/prism-net-udhcpc.sh -O mtu -O domain",
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs root and network namespaces"]
async fn dhcp_static_leases() {
    require_root();
    let host = Ns::new("pnt-dh");
    let g1 = Ns::new("pnt-dh-g1");
    let g2 = Ns::new("pnt-dh-g2");
    let nl = host.netlink();
    nl.ensure_bridge("ps1", &BridgeConfig { mtu: 1350 })
        .await
        .unwrap();
    nl.ensure_address("ps1", "10.30.0.1/24".parse().unwrap())
        .await
        .unwrap();
    let macs = ["52:54:00:00:00:51", "52:54:00:00:00:52"];
    for (i, (g, mac)) in [(&g1, macs[0]), (&g2, macs[1])].into_iter().enumerate() {
        let port = format!("pn{}", i + 1);
        sh(&format!(
            "ip link add {port} netns {} type veth peer name eth0 netns {}",
            host.name(),
            g.name()
        ));
        g.exec(&format!("ip link set eth0 address {mac} up"));
        nl.set_controller(&port, Some("ps1")).await.unwrap();
        nl.set_link_up(&port).await.unwrap();
    }

    let lease = |ip: u8| Lease {
        ip: Ipv4Addr::new(10, 30, 0, ip),
        prefix_len: 24,
        router: Some(Ipv4Addr::new(10, 30, 0, 1)),
        dns: vec![Ipv4Addr::new(10, 30, 0, 1)],
        domain: Some("sw1.prism".into()),
        hostname: Some(format!("vm{ip}")),
        mtu: Some(1350),
        routes: Vec::new(),
        lease_time: Duration::from_secs(3600),
    };
    let leases = Arc::new(RwLock::new(HashMap::from([(
        macs[0].parse::<MacAddr>().unwrap(),
        lease(50),
    )])));
    let server = host.enter({
        let leases = Arc::clone(&leases);
        move || DhcpServer::bind(DhcpConfig::new("ps1", Ipv4Addr::new(10, 30, 0, 1)), leases)
    });
    let server = server.unwrap();
    let task = tokio::spawn(async move { server.run().await });

    std::fs::write("/tmp/prism-net-udhcpc.sh", UDHCPC_SCRIPT).unwrap();
    sh("chmod +x /tmp/prism-net-udhcpc.sh; rm -f /tmp/prism-net-udhcpc-*.env");

    assert!(udhcpc(&g1), "known MAC gets a lease");
    let env = std::fs::read_to_string("/tmp/prism-net-udhcpc-eth0-bound.env").unwrap();
    for want in [
        "ip=10.30.0.50",
        "mask=24",
        "subnet=255.255.255.0",
        "router=10.30.0.1",
        "dns=10.30.0.1",
        "domain=sw1.prism",
        "mtu=1350",
        "lease=3600",
        "serverid=10.30.0.1",
        "hostname=vm50",
    ] {
        assert!(
            env.lines().any(|l| l.trim() == want),
            "missing {want} in:\n{env}"
        );
    }
    std::fs::remove_file("/tmp/prism-net-udhcpc-eth0-bound.env").ok();

    assert!(!udhcpc(&g2), "unknown MAC must get nothing");
    assert!(!std::path::Path::new("/tmp/prism-net-udhcpc-eth0-bound.env").exists());

    // Lease table updates apply to the next request.
    leases
        .write()
        .unwrap()
        .insert(macs[1].parse().unwrap(), lease(51));
    assert!(udhcpc(&g2), "MAC added at runtime gets a lease");
    let env = std::fs::read_to_string("/tmp/prism-net-udhcpc-eth0-bound.env").unwrap();
    assert!(env.lines().any(|l| l == "ip=10.30.0.51"), "{env}");

    task.abort();
    sh("rm -f /tmp/prism-net-udhcpc.sh /tmp/prism-net-udhcpc-*.env");
}
