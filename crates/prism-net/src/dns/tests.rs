use super::*;
use hickory_proto::op::Query;

fn zone() -> Zone {
    let mut hosts = HashMap::new();
    hosts.insert(
        "vm1".to_owned(),
        vec!["10.20.0.5".parse().unwrap(), "fd00::5".parse().unwrap()],
    );
    Zone::new("sw1.prism", &hosts, 30)
        .unwrap()
        .with_blocked(&["ts.net"])
        .unwrap()
}

fn query(name: &str, ty: RecordType) -> Message {
    let mut m = Message::query();
    m.add_query(Query::query(Name::from_ascii(name).unwrap(), ty));
    m
}

#[test]
fn answers_zone_records() {
    let z = zone();
    let r = z.answer(&query("VM1.sw1.prism.", RecordType::A)).unwrap();
    assert_eq!(r.metadata.response_code, ResponseCode::NoError);
    assert!(r.metadata.authoritative);
    assert_eq!(r.answers.len(), 1);
    assert_eq!(r.answers[0].data, RData::A(A("10.20.0.5".parse().unwrap())));
    let r = z
        .answer(&query("vm1.sw1.prism.", RecordType::AAAA))
        .unwrap();
    assert_eq!(r.answers.len(), 1);
    let r = z.answer(&query("nope.sw1.prism.", RecordType::A)).unwrap();
    assert_eq!(r.metadata.response_code, ResponseCode::NXDomain);
    let r = z
        .answer(&query("5.0.20.10.in-addr.arpa.", RecordType::PTR))
        .unwrap();
    assert_eq!(
        r.answers[0].data,
        RData::PTR(PTR(Name::from_ascii("vm1.sw1.prism.").unwrap()))
    );
    let r = z.answer(&query("host.foo.ts.net.", RecordType::A)).unwrap();
    assert_eq!(r.metadata.response_code, ResponseCode::NXDomain);
    assert!(z.answer(&query("example.com.", RecordType::A)).is_none());
}

#[test]
fn rand_id_succeeds() {
    assert!(rand_id().is_ok());
}

#[tokio::test]
async fn forward_rejects_short_query() {
    let cfg = DnsConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        interface: None,
        upstreams: vec!["127.0.0.1:9".parse().unwrap()],
        upstream_timeout: Duration::from_millis(50),
    };
    let server = DnsServer::bind(cfg, zone()).unwrap();
    assert!(server.forward(&[1]).await.is_none());
}
