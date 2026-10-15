//! Root-only integration tests. Each test builds its own network namespaces and removes them.
//!
//! They are `#[ignore]`d so `cargo test` stays unprivileged. On a testbed node:
//!
//! ```sh
//! cargo zigbuild --tests --target x86_64-unknown-linux-musl -p prism-net
//! sudo ./netns-<hash> --ignored --test-threads=1
//! ```
//!
//! Needs `ip`, `nft`, `wg`, `ping`, `nc`, `setpriv`, `busybox udhcpc` and `dig`.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, UdpSocket};
use std::os::fd::AsFd;
use std::process::Command;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use ipnet::{Ipv4Net, Ipv6Net};
use prism_net::dhcp::{DhcpConfig, DhcpServer, Lease};
use prism_net::dns::{DnsConfig, DnsServer, Zone};
use prism_net::link::{
    BridgeConfig, FdbChanges, PortVlan, PortVlans, VXLAN_PORT, VxlanConfig, VxlanFdb,
};
use prism_net::nft::{
    BoundaryConfig, Chain, ChainType, Family, GuestPort, Hook, NatRule, Nft, SwitchServices, Table,
    boundary_ruleset, bridge_guard_ruleset, guest_input_chain, nat_ruleset, priority,
};
use prism_net::wireguard::{InterfaceConfig, KeyPair, PeerChanges, PeerConfig};
use prism_net::{MacAddr, Netlink};
use serde_json::Value;

const QEMU_UID: u32 = 64055;

fn require_root() {
    assert!(
        rustix::process::geteuid().is_root(),
        "these tests need root (run with sudo)"
    );
}

fn run(cmd: &str) -> std::process::Output {
    let go = || {
        Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .expect("spawn sh")
    };
    match tokio::runtime::Handle::try_current() {
        Ok(_) => tokio::task::block_in_place(go),
        Err(_) => go(),
    }
}

/// Runs a shell command and returns stdout; panics on failure.
fn sh(cmd: &str) -> String {
    let out = run(cmd);
    assert!(
        out.status.success(),
        "`{cmd}` failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn sh_ok(cmd: &str) -> bool {
    run(cmd).status.success()
}

/// A network namespace that is deleted on drop.
struct Ns(String);

impl Ns {
    fn new(name: &str) -> Self {
        let _ = sh_ok(&format!("ip netns del {name} 2>/dev/null"));
        sh(&format!("ip netns add {name}"));
        sh(&format!("ip -n {name} link set lo up"));
        // No duplicate address detection: addresses are usable as soon as links are up.
        sh(&format!(
            "ip netns exec {name} sysctl -qw net.ipv6.conf.all.accept_dad=0 net.ipv6.conf.default.accept_dad=0"
        ));
        Self(name.to_owned())
    }

    fn name(&self) -> &str {
        &self.0
    }

    fn exec(&self, cmd: &str) -> String {
        sh(&format!("ip netns exec {} {cmd}", self.0))
    }

    fn exec_ok(&self, cmd: &str) -> bool {
        sh_ok(&format!("ip netns exec {} {cmd}", self.0))
    }

    /// Runs `f` on a thread inside the namespace (with the current tokio runtime entered), so
    /// sockets it creates belong to the namespace.
    fn enter<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> T {
        let path = format!("/run/netns/{}", self.0);
        let rt = tokio::runtime::Handle::try_current().ok();
        let go = move || {
            std::thread::spawn(move || {
                let fd = File::open(&path).expect("open netns");
                rustix::thread::move_into_link_name_space(
                    fd.as_fd(),
                    Some(rustix::thread::LinkNameSpaceType::Network),
                )
                .expect("setns");
                let _guard = rt.as_ref().map(tokio::runtime::Handle::enter);
                f()
            })
            .join()
            .expect("netns thread panicked")
        };
        match tokio::runtime::Handle::try_current() {
            Ok(_) => tokio::task::block_in_place(go),
            Err(_) => go(),
        }
    }

    fn netlink(&self) -> Netlink {
        self.enter(|| Netlink::connect().expect("netlink"))
    }

    fn nft(&self) -> Nft {
        self.enter(|| Nft::new().expect("nft socket"))
    }
}

impl Drop for Ns {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["netns", "del", &self.0]).status();
    }
}

fn mesh_prefix() -> Ipv6Net {
    "fd12:3456:789a::/48".parse().unwrap()
}

fn mesh_addr(n: u16) -> Ipv6Addr {
    Ipv6Addr::new(0xfd12, 0x3456, 0x789a, 1, 0, 0, 0, n)
}

/// Two namespaces joined by a veth "underlay" (`10.99.0.0/24`, `fd99::/64`) with `wg-prism`
/// configured on both through this crate.
struct Mesh {
    a: Ns,
    b: Ns,
    nla: Netlink,
    nlb: Netlink,
    keys: [KeyPair; 2],
}

impl Mesh {
    fn peers_of(&self, side: usize) -> Vec<PeerConfig> {
        let other = 1 - side;
        vec![PeerConfig {
            public_key: self.keys[other].public,
            endpoint: Some(SocketAddr::from((
                Ipv4Addr::new(10, 99, 0, other as u8 + 1),
                51820,
            ))),
            mesh_address: mesh_addr(other as u16 + 1),
            keepalive: Some(25),
        }]
    }

    fn wg_config(&self, side: usize) -> InterfaceConfig {
        InterfaceConfig {
            name: "wg-prism".into(),
            private_key: self.keys[side].private.clone(),
            listen_port: 51820,
            fwmark: None,
            mtu: 1420,
            address: Ipv6Net::new(mesh_addr(side as u16 + 1), 64).unwrap(),
        }
    }

    async fn new(prefix: &str) -> Self {
        let a = Ns::new(&format!("{prefix}-a"));
        let b = Ns::new(&format!("{prefix}-b"));
        sh(&format!(
            "ip link add va netns {} type veth peer name vb netns {}",
            a.name(),
            b.name()
        ));
        for (ns, dev, n) in [(&a, "va", 1), (&b, "vb", 2)] {
            ns.exec(&format!("ip addr add 10.99.0.{n}/24 dev {dev}"));
            ns.exec(&format!("ip -6 addr add fd99::{n}/64 dev {dev} nodad"));
            ns.exec(&format!("ip link set {dev} up"));
        }
        // The first neighbour resolution on a fresh veth can take over a second.
        for target in ["10.99.0.2", "fd99::2"] {
            assert!(
                (0..10).any(|_| a.exec_ok(&format!("ping -c 1 -W 1 {target}"))),
                "underlay {target} unreachable"
            );
        }
        let mesh = Self {
            nla: a.netlink(),
            nlb: b.netlink(),
            a,
            b,
            keys: [KeyPair::generate(), KeyPair::generate()],
        };
        for side in 0..2 {
            let nl = mesh.nl(side);
            nl.ensure_wireguard(&mesh.wg_config(side)).await.unwrap();
            let changes = nl
                .set_wireguard_peers("wg-prism", &mesh.peers_of(side))
                .await
                .unwrap();
            assert_eq!(changes.added, 1);
        }
        mesh
    }

    fn nl(&self, side: usize) -> &Netlink {
        if side == 0 { &self.nla } else { &self.nlb }
    }
}

fn boundary_cfg() -> BoundaryConfig {
    BoundaryConfig {
        mesh_prefix: mesh_prefix(),
        wg_interface: "wg-prism".into(),
        qemu_uid: Some(QEMU_UID),
    }
}

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

/// `nft -j list table` with handles and metainfo removed.
fn listing(ns: &Ns, family: &str, name: &str) -> Value {
    fn strip(v: &mut Value) {
        match v {
            Value::Object(m) => {
                m.remove("handle");
                m.values_mut().for_each(strip);
            }
            Value::Array(a) => a.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut v: Value =
        serde_json::from_str(&ns.exec(&format!("nft -j list table {family} {name}")))
            .expect("nft json");
    let items = v["nftables"].as_array_mut().expect("nftables array");
    items.retain(|i| i.get("metainfo").is_none());
    strip(&mut v);
    v
}

fn assert_same_as_cli(mine: &Ns, reference: &Ns, table: &Table, text: &str) {
    let path = format!("/tmp/prism-net-ref-{}.nft", reference.name());
    std::fs::write(&path, text).unwrap();
    reference.exec(&format!("nft -f {path}"));
    std::fs::remove_file(&path).ok();
    mine.nft().replace_table(table).unwrap();
    let fam = table.family.as_str();
    let got = listing(mine, fam, &table.name);
    let want = listing(reference, fam, &table.name);
    assert_eq!(
        got,
        want,
        "\nours:\n{}\nnft -f:\n{}",
        mine.exec(&format!("nft list table {fam} {}", table.name)),
        reference.exec(&format!("nft list table {fam} {}", table.name))
    );
}

const BOUNDARY_TEXT: &str = r#"
table inet prism {
    chain prerouting {
        type filter hook prerouting priority raw; policy accept;
        iifname != "wg-prism" iifname != "lo" ip6 saddr fd12:3456:789a::/48 drop
        iifname != "wg-prism" iifname != "lo" ip6 daddr fd12:3456:789a::/48 drop
        iifname != "wg-prism" udp dport 4789 drop
    }
    chain forward {
        type filter hook forward priority filter; policy accept;
        iifname "wg-prism" drop
        oifname "wg-prism" drop
    }
    chain output {
        type filter hook output priority filter; policy accept;
        meta skuid 64055 ct state new drop
        oifname "wg-prism" ip6 daddr fd12:3456:789a::/48 icmpv6 type echo-request accept
        ip6 daddr fd12:3456:789a::/48 meta skuid != 0 drop
    }
    chain input {
        type filter hook input priority filter; policy accept;
        iifname "ps1" ct state established,related accept
        iifname "ps1" udp dport 67 accept
        iifname "ps1" ip daddr 10.20.0.1 udp dport 53 accept
        iifname "ps1" ip daddr 10.20.0.1 tcp dport 53 accept
        iifname "ps1" icmp type echo-request accept
        iifname "ps1" icmpv6 type nd-router-solicit accept
        iifname "ps1" icmpv6 type nd-router-advert accept
        iifname "ps1" icmpv6 type nd-neighbor-solicit accept
        iifname "ps1" icmpv6 type nd-neighbor-advert accept
        iifname "ps1" drop
    }
}
"#;

const NAT_TEXT: &str = r#"
table ip prism-nat {
    chain prerouting {
        type nat hook prerouting priority dstnat; policy accept;
    }

    chain postrouting {
        type nat hook postrouting priority srcnat; policy accept;
        oifname "eth0" ip saddr 10.20.0.0/24 masquerade
        oifname "eth1" ip saddr 10.21.0.0/23 masquerade
    }
}
"#;

const BRIDGE_TEXT: &str = r#"
table bridge prism {
    chain prerouting {
        type filter hook prerouting priority filter; policy accept;
        iifname "pn1" ether saddr != 52:54:00:00:00:01 drop
        iifname "pn1" ip saddr != 10.20.0.5 ip saddr != 0.0.0.0 drop
        iifname "pn1" arp saddr ip != 10.20.0.5 drop
        iifname "pn1" udp sport 67 drop
        iifname "pn1" icmpv6 type nd-router-advert drop
        iifname "pn1" ether type 8021q drop
        iifname "pn1" ether type 8021ad drop
    }
}
"#;

fn nat_rules() -> Vec<NatRule> {
    vec![
        NatRule {
            source: "10.20.0.0/24".parse().unwrap(),
            egress: "eth0".into(),
        },
        NatRule {
            source: "10.21.0.0/23".parse().unwrap(),
            egress: "eth1".into(),
        },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs root and network namespaces"]
async fn nft_tables_match_cli() {
    require_root();
    let mine = Ns::new("pnt-nft-mine");

    let mut inet = boundary_ruleset(&boundary_cfg());
    inet.chains.push(guest_input_chain(&[SwitchServices {
        bridge: "ps1".into(),
        vip: Ipv4Addr::new(10, 20, 0, 1),
    }]));
    assert_same_as_cli(&mine, &Ns::new("pnt-nft-r1"), &inet, BOUNDARY_TEXT);

    let nat = nat_ruleset(&nat_rules(), &[]);
    assert_same_as_cli(&mine, &Ns::new("pnt-nft-r2"), &nat, NAT_TEXT);

    let bridge = bridge_guard_ruleset(&[GuestPort {
        tap: "pn1".into(),
        mac: "52:54:00:00:00:01".parse().unwrap(),
        ipv4: Some(Ipv4Addr::new(10, 20, 0, 5)),
    }]);
    assert_same_as_cli(&mine, &Ns::new("pnt-nft-r3"), &bridge, BRIDGE_TEXT);

    // A transaction the kernel rejects (masquerade outside a nat chain) changes nothing.
    let before = listing(&mine, "ip", "prism-nat");
    let mut bad = Table::new(Family::Ip, "prism-nat");
    bad.chain(Chain::base(
        "postrouting",
        ChainType::Filter,
        Hook::Postrouting,
        priority::SRCNAT,
    ))
    .push(
        prism_net::nft::Rule::new(Family::Ip)
            .ip_saddr(Ipv4Net::new(Ipv4Addr::new(10, 9, 0, 0), 16).unwrap())
            .masquerade(),
    );
    let err = mine.nft().replace_table(&bad).unwrap_err();
    println!("rejected as expected: {err}");
    assert!(err.errno().is_some());
    assert_eq!(listing(&mine, "ip", "prism-nat"), before);

    // Several tables in one transaction, then deletion (idempotent).
    let mut nft = mine.nft();
    nft.replace_tables(&[inet, nat, bridge]).unwrap();
    assert_eq!(mine.exec("nft list tables").lines().count(), 3);
    nft.delete_table(Family::Ip, "prism-nat").unwrap();
    nft.delete_table(Family::Ip, "prism-nat").unwrap();
    assert_eq!(mine.exec("nft list tables").lines().count(), 2);
}

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
    nl.set_link_up("pn1", true).await.unwrap();
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
        nl.set_link_up(&port, true).await.unwrap();
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

/// The DNS test re-executes itself inside the server namespace so that upstream sockets the
/// forwarder opens live there too.
#[test]
#[ignore = "needs root and network namespaces"]
fn dns_zone_and_forwarding() {
    if std::env::var_os("PRISM_NET_DNS_CHILD").is_some() {
        dns_child();
        return;
    }
    require_root();
    let srv = Ns::new("pnt-dns");
    let cl = Ns::new("pnt-dns-cl");
    sh(&format!(
        "ip link add ps1 netns {} type veth peer name eth0 netns {}",
        srv.name(),
        cl.name()
    ));
    srv.exec("ip addr add 10.40.0.1/24 dev ps1");
    srv.exec("ip link set ps1 up");
    cl.exec("ip addr add 10.40.0.2/24 dev eth0");
    cl.exec("ip link set eth0 up");
    let exe = std::env::current_exe().unwrap();
    let status = Command::new("ip")
        .args(["netns", "exec", srv.name()])
        .arg(exe)
        .args([
            "--ignored",
            "--exact",
            "dns_zone_and_forwarding",
            "--nocapture",
        ])
        .env("PRISM_NET_DNS_CHILD", "1")
        .status()
        .unwrap();
    assert!(status.success());
}

/// A fake upstream on 127.0.0.2:5300: `example.com` over UDP; `big.example.com` is truncated
/// over UDP and answered only over TCP.
async fn fake_upstream() {
    use hickory_proto::op::{Message, ResponseCode};
    use hickory_proto::rr::rdata::A;
    use hickory_proto::rr::{RData, Record};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn answer(q: &[u8], tcp: bool) -> Vec<u8> {
        let req = Message::from_vec(q).unwrap();
        let mut m = Message::response(req.metadata.id, req.metadata.op_code);
        m.metadata.recursion_available = true;
        m.add_queries(req.queries.iter().cloned());
        let name = req.queries[0].name().clone();
        match name.to_ascii().as_str() {
            "example.com." => {
                m.add_answer(Record::from_rdata(
                    name,
                    60,
                    RData::A(A::new(93, 184, 216, 34)),
                ));
            }
            "big.example.com." if !tcp => m.metadata.truncation = true,
            "big.example.com." => {
                m.add_answer(Record::from_rdata(
                    name,
                    60,
                    RData::A(A::new(198, 51, 100, 7)),
                ));
            }
            _ => m.metadata.response_code = ResponseCode::NXDomain,
        }
        m.to_vec().unwrap()
    }

    let udp = tokio::net::UdpSocket::bind("127.0.0.2:5300").await.unwrap();
    let tcp = tokio::net::TcpListener::bind("127.0.0.2:5300")
        .await
        .unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = tcp.accept().await.unwrap();
            let len = s.read_u16().await.unwrap();
            let mut q = vec![0; usize::from(len)];
            s.read_exact(&mut q).await.unwrap();
            let r = answer(&q, true);
            s.write_u16(r.len() as u16).await.unwrap();
            s.write_all(&r).await.unwrap();
        }
    });
    tokio::spawn(async move {
        let mut buf = [0u8; 1500];
        loop {
            let (n, from) = udp.recv_from(&mut buf).await.unwrap();
            udp.send_to(&answer(&buf[..n], false), from).await.unwrap();
        }
    });
}

fn dns_child() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        fake_upstream().await;
        let hosts = HashMap::from([(
            "vm1".to_owned(),
            vec![
                IpAddr::V4(Ipv4Addr::new(10, 40, 0, 50)),
                "fd40::50".parse().unwrap(),
            ],
        )]);
        let zone = Zone::new("sw1.prism", &hosts, 30)
            .unwrap()
            .with_blocked(&["ts.net"])
            .unwrap();
        let server = DnsServer::bind(
            DnsConfig {
                listen: "10.40.0.1:53".parse().unwrap(),
                interface: Some("ps1".into()),
                upstreams: vec![
                    "127.0.0.9:53".parse().unwrap(), // nothing there: falls through
                    "127.0.0.2:5300".parse().unwrap(),
                ],
                upstream_timeout: Duration::from_millis(500),
            },
            zone,
        )
        .unwrap();
        tokio::spawn(Arc::clone(&server).run());

        let dig = |args: &str| {
            sh(&format!(
                "ip netns exec pnt-dns-cl dig +time=3 +tries=1 @10.40.0.1 {args}"
            ))
        };
        assert_eq!(dig("+short vm1.sw1.prism A").trim(), "10.40.0.50");
        assert_eq!(dig("+short VM1.SW1.prism AAAA").trim(), "fd40::50");
        assert_eq!(dig("+short -x 10.40.0.50").trim(), "vm1.sw1.prism.");
        let full = dig("vm1.sw1.prism");
        assert!(full.contains("flags: qr aa rd ra"), "{full}");
        assert!(dig("nope.sw1.prism").contains("status: NXDOMAIN"));
        assert!(dig("+tcp +short vm1.sw1.prism").contains("10.40.0.50"));
        // Forwarded, past a dead first upstream.
        assert_eq!(dig("+short example.com").trim(), "93.184.216.34");
        // Upstream truncates over UDP; the forwarder retries over TCP.
        assert_eq!(dig("+short big.example.com").trim(), "198.51.100.7");
        assert!(dig("missing.example.com").contains("status: NXDOMAIN"));
        // Never forwarded.
        assert!(dig("host.tail1234.ts.net").contains("status: NXDOMAIN"));

        // Zone updates take effect immediately.
        let hosts = HashMap::from([(
            "vm2".to_owned(),
            vec![IpAddr::V4(Ipv4Addr::new(10, 40, 0, 51))],
        )]);
        server.set_zone(Zone::new("sw1.prism", &hosts, 30).unwrap());
        assert_eq!(dig("+short vm2.sw1.prism").trim(), "10.40.0.51");
        assert!(dig("vm1.sw1.prism").contains("status: NXDOMAIN"));
    });
}
