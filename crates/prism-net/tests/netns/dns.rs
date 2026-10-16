//! The DNS server: local zone answers and forwarding to a fake upstream.

use crate::common::{Ns, require_root, sh};
use prism_net::dns::{DnsConfig, DnsServer, Zone};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

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
