//! nftables tables built by this crate compared with what the `nft` CLI produces.

use crate::common::{Ns, boundary_cfg, require_root};
use ipnet::Ipv4Net;
use prism_net::nft::{
    Chain, ChainType, DnatRule, Family, GuestPort, Hook, NatRule, SwitchServices, Table,
    boundary_ruleset, bridge_guard_ruleset, guest_input_chain, nat_ruleset, priority,
};
use serde_json::Value;
use std::net::Ipv4Addr;

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
        ip daddr 192.0.2.7 tcp dport 18000 dnat to 10.20.0.5:8000
    }

    chain postrouting {
        type nat hook postrouting priority srcnat; policy accept;
        ip daddr 10.20.0.5 tcp dport 8000 masquerade
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
        vip6: None,
    }]));
    assert_same_as_cli(&mine, &Ns::new("pnt-nft-r1"), &inet, BOUNDARY_TEXT);

    let forwards = [DnatRule {
        udp: false,
        host_addr: Some(Ipv4Addr::new(192, 0, 2, 7).into()),
        host_port: 18000,
        guest: Ipv4Addr::new(10, 20, 0, 5).into(),
        guest_port: 8000,
        bridge: "ps1".into(),
        local: false,
        hairpin: false,
        snat6: None,
    }];
    let nat = nat_ruleset(&nat_rules(), &forwards);
    assert_same_as_cli(&mine, &Ns::new("pnt-nft-r2"), &nat, NAT_TEXT);

    let bridge = bridge_guard_ruleset(&[GuestPort {
        tap: "pn1".into(),
        mac: "52:54:00:00:00:01".parse().unwrap(),
        ipv4: Some(Ipv4Addr::new(10, 20, 0, 5)),
        ipv6: prism_net::nft::Ipv6Source::default(),
        trunk: false,
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
