//! The bridge guard against crafted IPv6 frames: extension headers in front of a router
//! advertisement, a DHCPv6 reply or a neighbour advertisement must not get them past the
//! drops.

use crate::common::{Ns, require_root};
use prism_net::nft::{GuestPort, Ipv6Source, bridge_guard_ruleset};
use std::collections::BTreeSet;
use std::net::Ipv6Addr;

const MAC: [u8; 6] = [0x52, 0x54, 0, 0, 0, 1];
const MCAST_MAC: [u8; 6] = [0x33, 0x33, 0, 0, 0, 1];
const OWN: &str = "fd00:1::5";
const OTHER: &str = "fd00:1::99";
const LINK_LOCAL: &str = "fe80::5054:ff:fe00:1";
const ALL_NODES: &str = "ff02::1";

fn addr(s: &str) -> [u8; 16] {
    s.parse::<Ipv6Addr>().unwrap().octets()
}

/// An IPv6 packet: header, extension headers already folded into `payload`.
fn ipv6(src: &str, next: u8, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0x60, 0, 0, 0];
    p.extend((payload.len() as u16).to_be_bytes());
    p.push(next);
    p.push(255);
    p.extend(addr(src));
    p.extend(addr(ALL_NODES));
    p.extend(payload);
    p
}

fn icmp(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut m = vec![kind, 0, 0, 0];
    m.extend(body);
    m
}

fn udp(sport: u16, body: &[u8]) -> Vec<u8> {
    let mut m = sport.to_be_bytes().to_vec();
    m.extend(546u16.to_be_bytes());
    m.extend((8 + body.len() as u16).to_be_bytes());
    m.extend([0, 0]);
    m.extend(body);
    m
}

/// A neighbour advertisement for `target` with a target link-layer option naming `mac`.
fn na(target: &str, mac: [u8; 6]) -> Vec<u8> {
    let mut b = vec![0x20, 0, 0, 0];
    b.extend(addr(target));
    b.extend([2, 1]);
    b.extend(mac);
    icmp(136, &b)
}

fn ra() -> Vec<u8> {
    icmp(134, &[64, 0, 0x07, 0x08, 0, 0, 0, 0, 0, 0, 0, 0])
}

/// How the upper-layer message is reached.
#[derive(Clone, Copy)]
enum Ext {
    None,
    HopByHop,
    DestOpts,
    Routing,
    Fragment,
    HopThenDest,
}

/// Wraps `l4` (protocol `proto`) in the extension headers `ext`; returns the first next
/// header value and the bytes after the IPv6 header.
fn wrap(ext: Ext, proto: u8, l4: &[u8]) -> (u8, Vec<u8>) {
    let pad = [1, 4, 0, 0, 0, 0];
    let header = |next: u8| {
        let mut h = vec![next, 0];
        h.extend(pad);
        h
    };
    let mut out = Vec::new();
    let first = match ext {
        Ext::None => proto,
        Ext::HopByHop => {
            out.extend(header(proto));
            0
        }
        Ext::DestOpts => {
            out.extend(header(proto));
            60
        }
        Ext::Routing => {
            out.extend([proto, 0, 4, 0, 0, 0, 0, 0]);
            43
        }
        Ext::Fragment => {
            out.extend([proto, 0, 0, 0, 0, 0, 0, 1]);
            44
        }
        Ext::HopThenDest => {
            out.extend(header(60));
            out.extend(header(proto));
            0
        }
    };
    out.extend(l4);
    (first, out)
}

fn eth(src_mac: [u8; 6], vlan: Option<u16>, payload: &[u8]) -> Vec<u8> {
    let mut f = MCAST_MAC.to_vec();
    f.extend(src_mac);
    if let Some(vid) = vlan {
        f.extend([0x81, 0x00]);
        f.extend(vid.to_be_bytes());
    }
    f.extend([0x86, 0xdd]);
    f.extend(payload);
    f
}

/// The ICMPv6 checksum of `msg` sent from `src` to the all-nodes address.
fn icmp_checksum(src: &str, msg: &[u8]) -> u16 {
    let mut data = addr(src).to_vec();
    data.extend(addr(ALL_NODES));
    data.extend((msg.len() as u32).to_be_bytes());
    data.extend([0, 0, 0, 58]);
    data.extend(msg);
    if data.len() % 2 == 1 {
        data.push(0);
    }
    let mut sum: u32 = data
        .chunks(2)
        .map(|c| u32::from(u16::from_be_bytes([c[0], c[1]])))
        .sum();
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

struct Frame {
    name: String,
    iface: &'static str,
    bytes: Vec<u8>,
    /// Whether the guard should let the frame through.
    pass: bool,
}

fn frame(
    name: &str,
    iface: &'static str,
    vlan: Option<u16>,
    src: &str,
    ext: Ext,
    proto: u8,
    l4: &[u8],
    pass: bool,
) -> Frame {
    let mut l4 = l4.to_vec();
    if proto == 58 {
        // The bridge's multicast snooping drops ICMPv6 behind a hop-by-hop header whose
        // checksum is wrong, before the guard's verdict would matter.
        let sum = icmp_checksum(src, &l4);
        l4[2..4].copy_from_slice(&sum.to_be_bytes());
    }
    let (first, rest) = wrap(ext, proto, &l4);
    Frame {
        name: name.to_owned(),
        iface,
        bytes: eth(MAC, vlan, &ipv6(src, first, &rest)),
        pass,
    }
}

fn ext_name(e: Ext) -> &'static str {
    match e {
        Ext::None => "plain",
        Ext::HopByHop => "hbh",
        Ext::DestOpts => "dst",
        Ext::Routing => "rt",
        Ext::Fragment => "frag",
        Ext::HopThenDest => "hbh-dst",
    }
}

fn port(tap: &str, trunk: bool) -> GuestPort {
    GuestPort {
        tap: tap.into(),
        mac: "52:54:00:00:00:01".parse().unwrap(),
        ipv4: None,
        ipv6: Ipv6Source::Only {
            addrs: vec![OWN.parse().unwrap()],
            prefix: "fd00:1::/64".parse().unwrap(),
            gateways: vec!["fd00:1::ffff".parse().unwrap()],
        },
        trunk,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs root, network namespaces and python3"]
async fn extension_headers_do_not_bypass_the_ipv6_guard() {
    require_root();
    let ns = Ns::new("pnt-guard");
    ns.exec("ip link add br0 type bridge stp_state 0");
    for (tap, host) in [("pn1", "g1"), ("pn2", "r2"), ("pn3", "g3")] {
        ns.exec(&format!("ip link add {tap} type veth peer name {host}"));
        for dev in [tap, host] {
            ns.exec(&format!("sysctl -qw net.ipv6.conf.{dev}.disable_ipv6=1"));
            ns.exec(&format!("ip link set {dev} up"));
        }
    }
    for tap in ["pn1", "pn2", "pn3"] {
        ns.exec(&format!("ip link set {tap} master br0"));
    }
    ns.exec("ip link set br0 up");
    ns.nft()
        .replace_table(&bridge_guard_ruleset(&[
            port("pn1", false),
            port("pn3", true),
        ]))
        .unwrap();

    let all = [
        Ext::None,
        Ext::HopByHop,
        Ext::DestOpts,
        Ext::Routing,
        Ext::Fragment,
        Ext::HopThenDest,
    ];
    let mut frames = Vec::new();
    // (iface, vlan, label): the untagged port and the trunk's tagged frames
    for (iface, vlan) in [("g1", None), ("g3", Some(100))] {
        let tag = if vlan.is_some() { "t" } else { "u" };
        for ext in all {
            let e = ext_name(ext);
            // On the trunk a tagged frame that starts with an extension header is dropped
            // whatever it carries, except a lone hop-by-hop header in front of plain
            // ICMPv6, tested below.
            let ext_ok = vlan.is_none() || matches!(ext, Ext::None);
            frames.push(frame(
                &format!("{tag}-ra-{e}"),
                iface,
                vlan,
                LINK_LOCAL,
                ext,
                58,
                &ra(),
                false,
            ));
            frames.push(frame(
                &format!("{tag}-dhcp6-{e}"),
                iface,
                vlan,
                LINK_LOCAL,
                ext,
                17,
                &udp(547, b"reply"),
                false,
            ));
            frames.push(frame(
                &format!("{tag}-na-claim-{e}"),
                iface,
                vlan,
                LINK_LOCAL,
                ext,
                58,
                &na(OTHER, MAC),
                false,
            ));
            frames.push(frame(
                &format!("{tag}-na-lladdr-{e}"),
                iface,
                vlan,
                LINK_LOCAL,
                ext,
                58,
                &na(OWN, [2, 2, 2, 2, 2, 2]),
                false,
            ));
            frames.push(frame(
                &format!("{tag}-spoof-{e}"),
                iface,
                vlan,
                OTHER,
                ext,
                17,
                &udp(5000, b"data"),
                false,
            ));
            frames.push(frame(
                &format!("{tag}-udp-{e}"),
                iface,
                vlan,
                OWN,
                ext,
                17,
                &udp(5000, b"data"),
                ext_ok,
            ));
            frames.push(frame(
                &format!("{tag}-na-own-{e}"),
                iface,
                vlan,
                LINK_LOCAL,
                ext,
                58,
                &na(OWN, MAC),
                ext_ok,
            ));
        }
    }
    // MLD report behind a hop-by-hop header passes on the trunk.
    frames.push(frame(
        "t-mld-hbh",
        "g3",
        Some(100),
        LINK_LOCAL,
        Ext::HopByHop,
        58,
        &icmp(131, &[0, 0, 0, 0]),
        true,
    ));

    let script = "/tmp/prism-net-guard.py";
    std::fs::write(script, include_str!("guard.py")).unwrap();
    let mut cmd = format!("python3 {script} r2");
    for f in &frames {
        cmd.push_str(&format!(" {}={}:{}", f.name, f.iface, hex(&f.bytes)));
    }
    let seen: BTreeSet<String> = ns.exec(&cmd).lines().map(str::to_owned).collect();

    let mut wrong = Vec::new();
    for f in &frames {
        if seen.contains(&f.name) != f.pass {
            wrong.push(format!(
                "{}: expected {}",
                f.name,
                if f.pass { "delivered" } else { "dropped" }
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter().fold(String::new(), |mut s, x| {
        write!(s, "{x:02x}").unwrap();
        s
    })
}
