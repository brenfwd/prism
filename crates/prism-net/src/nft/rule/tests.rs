use super::*;

#[test]
fn prefixes_are_encoded_like_nft() {
    let r = Rule::new(Family::Ip).ip_saddr("10.21.0.0/23".parse().unwrap());
    assert_eq!(
        r.exprs(),
        &[
            Expr::Payload {
                base: PayloadBase::Network,
                offset: 12,
                len: 4
            },
            Expr::Bitwise {
                mask: vec![0xff, 0xff, 0xfe, 0],
                xor: vec![0; 4]
            },
            Expr::Cmp {
                op: CmpOp::Eq,
                data: vec![10, 21, 0, 0]
            },
        ]
    );
    let r = Rule::new(Family::Inet).ip6_saddr("fd12:3456:789a::/48".parse().unwrap());
    assert_eq!(r.exprs().len(), 4); // nfproto dependency + payload + cmp
    assert_eq!(
        r.exprs()[2],
        Expr::Payload {
            base: PayloadBase::Network,
            offset: 8,
            len: 6
        }
    );
    let r = Rule::new(Family::Ip).ip_saddr("0.0.0.0/0".parse().unwrap());
    assert_eq!(r.exprs(), &[]);
}

#[test]
fn dependencies_are_added_once() {
    let r = Rule::new(Family::Bridge)
        .ip_saddr_ne(Ipv4Addr::new(10, 0, 0, 1))
        .ip_saddr_ne(Ipv4Addr::UNSPECIFIED);
    // meta protocol + cmp, then two payload + cmp pairs.
    assert_eq!(r.exprs().len(), 6);
}
