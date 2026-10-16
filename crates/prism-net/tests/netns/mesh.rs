//! The `WireGuard` mesh and the boundary rules that close it to guests and QEMU.

use crate::common::{Mesh, Ns, QEMU_UID, boundary_cfg, require_root};
use prism_net::link::VXLAN_PORT;
use prism_net::nft::boundary_ruleset;
use prism_net::wireguard::PeerChanges;
use std::net::{Ipv6Addr, SocketAddr, TcpListener, UdpSocket};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs root and network namespaces"]
async fn wireguard_mesh() {
    require_root();
    let m = Mesh::new("pnt-wg").await;

    // Idempotent: a second pass changes nothing.
    for side in 0..2 {
        m.nl(side)
            .ensure_wireguard(&m.wg_config(side))
            .await
            .unwrap();
        let again = m
            .nl(side)
            .set_wireguard_peers("wg-prism", &m.peers_of(side))
            .await
            .unwrap();
        assert_eq!(again, PeerChanges::default());
    }

    // The kernel derives the same public key we did.
    let pk = m.a.exec("wg show wg-prism public-key");
    assert_eq!(pk.trim(), m.keys[0].public.to_string());

    m.a.exec("ping -6 -c 3 -i 0.2 -W 2 fd12:3456:789a:1::2");
    // A full-size, unfragmentable inner packet: W - 40 (IPv6) - 8 (ICMPv6).
    m.a.exec("ping -6 -c 1 -W 2 -M do -s 1372 fd12:3456:789a:1::2");

    let st = m.nla.wireguard_status("wg-prism").await.unwrap();
    assert_eq!(st.public_key, Some(m.keys[0].public));
    assert_eq!(st.listen_port, 51820);
    assert_eq!(st.peers.len(), 1);
    let p = &st.peers[0];
    assert_eq!(p.public_key, m.keys[1].public);
    assert!(p.last_handshake.is_some(), "no handshake: {p:?}");
    assert!(p.rx_bytes > 0 && p.tx_bytes > 0, "{p:?}");
    assert_eq!(p.endpoint, Some("10.99.0.2:51820".parse().unwrap()));
    assert_eq!(
        p.allowed_ips,
        vec!["fd12:3456:789a:1::2/128".parse().unwrap()]
    );
    assert_eq!(p.keepalive, 25);
    println!("peer status: {p:?}");

    // Replacing the peer set with nothing removes the peer.
    let ch = m.nla.set_wireguard_peers("wg-prism", &[]).await.unwrap();
    assert_eq!(ch.removed, 1);
    let left = m.nla.wireguard_status("wg-prism").await.unwrap().peers;
    assert_eq!(left, Vec::new());
    assert!(!m.a.exec_ok("ping -6 -c 1 -W 1 fd12:3456:789a:1::2"));
}

/// Sends one UDP datagram from `src` (inside `from`) to `dst` and reports whether a socket
/// bound to `listen` inside `to` received it.
fn udp_reaches(from: &Ns, src: SocketAddr, to: &Ns, listen: SocketAddr, dst: SocketAddr) -> bool {
    let rx = to.enter(move || {
        let s = UdpSocket::bind(listen).expect("bind listener");
        s.set_read_timeout(Some(Duration::from_millis(1500)))
            .unwrap();
        s
    });
    from.enter(move || {
        let s = UdpSocket::bind(src).expect("bind sender");
        // A few datagrams, so a slow neighbour resolution cannot lose the probe.
        for _ in 0..3 {
            s.send_to(b"probe", dst).ok();
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let mut buf = [0u8; 16];
    rx.recv_from(&mut buf).is_ok()
}

/// A TCP listener inside `ns` that accepts forever on a background thread.
fn tcp_listen(ns: &Ns, addr: SocketAddr) {
    let l = ns.enter(move || TcpListener::bind(addr).expect("bind tcp"));
    std::thread::spawn(move || for _ in l.incoming() {});
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs root and network namespaces"]
async fn boundary_rules_close_the_mesh() {
    require_root();
    let m = Mesh::new("pnt-bd").await;
    m.a.exec("ping -6 -c 1 -W 2 fd12:3456:789a:1::2");
    let (a, b) = (&m.a, &m.b);
    let any = |port| SocketAddr::from((Ipv6Addr::UNSPECIFIED, port));
    let addr = |ip: &str, port| SocketAddr::new(ip.parse().unwrap(), port);

    // Spoofed destination: a routes b's mesh address over the LAN instead of WireGuard.
    a.exec("ip -6 route add fd12:3456:789a:1::2/128 via fd99::2 dev va");
    let spoofed_dst = || {
        udp_reaches(
            a,
            addr("fd99::1", 0),
            b,
            any(9000),
            addr("fd12:3456:789a:1::2", 9000),
        )
    };
    // Spoofed source: a mesh address arriving on the LAN interface.
    let spoofed_src = || {
        udp_reaches(
            a,
            addr("fd12:3456:789a:1::1", 0),
            b,
            any(9001),
            addr("fd99::2", 9001),
        )
    };
    // VXLAN from the LAN.
    let lan_vxlan = || {
        udp_reaches(
            a,
            addr("fd99::1", 0),
            b,
            any(VXLAN_PORT),
            addr("fd99::2", VXLAN_PORT),
        )
    };
    assert!(spoofed_dst(), "without rules the weak-host hole is open");
    assert!(
        spoofed_src(),
        "without rules a spoofed mesh source is accepted"
    );
    assert!(lan_vxlan(), "without rules VXLAN is accepted from the LAN");

    let mut nft = b.nft();
    nft.replace_table(&boundary_ruleset(&boundary_cfg()))
        .unwrap();
    assert!(!spoofed_dst(), "spoofed mesh destination must be dropped");
    assert!(!spoofed_src(), "spoofed mesh source must be dropped");
    assert!(!lan_vxlan(), "VXLAN from the LAN must be dropped");
    a.exec("ip -6 route del fd12:3456:789a:1::2/128");

    // Real mesh traffic through wg-prism still works, including VXLAN's port.
    a.exec("ping -6 -c 1 -W 2 fd12:3456:789a:1::2");
    assert!(udp_reaches(
        a,
        addr("fd12:3456:789a:1::1", 0),
        b,
        any(VXLAN_PORT),
        addr("fd12:3456:789a:1::2", VXLAN_PORT),
    ));

    // Process rule: only root may send to the mesh; the qemu uid opens no connections.
    tcp_listen(a, addr("fd12:3456:789a:1::1", 7443));
    tcp_listen(a, addr("fd99::1", 7000));
    let nc = |uid: Option<u32>, target: &str| {
        let pre = uid.map_or(String::new(), |u| {
            format!("setpriv --reuid={u} --regid={u} --clear-groups ")
        });
        b.exec_ok(&format!("{pre}nc -6 -z -w 2 {target}"))
    };
    assert!(
        nc(None, "fd12:3456:789a:1::1 7443"),
        "root reaches the mesh"
    );
    assert!(
        !nc(Some(65534), "fd12:3456:789a:1::1 7443"),
        "non-root must not"
    );
    assert!(nc(Some(65534), "fd99::1 7000"), "non-root reaches the LAN");
    assert!(
        !nc(Some(QEMU_UID), "fd99::1 7000"),
        "qemu uid opens no connections"
    );

    // Re-applying is idempotent and leaves exactly one table.
    nft.replace_table(&boundary_ruleset(&boundary_cfg()))
        .unwrap();
    let tables = b.exec("nft list tables");
    assert_eq!(tables.trim(), "table inet prism");
}
