use super::*;

fn header() -> Header {
    Header {
        prog: REMOTE_PROGRAM,
        vers: REMOTE_PROTOCOL_VERSION,
        proc: 7,
        ty: MessageType::Reply,
        serial: 9,
        status: Status::Error,
    }
}

#[test]
fn header_round_trips_in_24_bytes() {
    let mut e = Encoder::new();
    header().encode(&mut e);
    let buf = e.into_inner();
    assert_eq!(buf.len(), HEADER_LEN);
    assert_eq!(Header::decode(&mut Decoder::new(&buf)).unwrap(), header());
}

#[test]
fn header_rejects_unknown_type_and_status() {
    for (ty, status) in [(9, 0), (1, 9)] {
        let mut e = Encoder::new();
        e.u32(REMOTE_PROGRAM)
            .u32(1)
            .u32(1)
            .u32(ty)
            .u32(1)
            .u32(status);
        let buf = e.into_inner();
        assert!(Header::decode(&mut Decoder::new(&buf)).is_err());
    }
}

#[test]
fn domain_round_trips() {
    let dom = Domain {
        name: "vm".into(),
        uuid: [3; UUID_LEN],
        id: -1,
    };
    let mut e = Encoder::new();
    dom.encode(&mut e);
    let buf = e.into_inner();
    assert_eq!(Domain::decode(&mut Decoder::new(&buf)).unwrap(), dom);
}

#[test]
fn node_info_reads_char_vector_model() {
    let mut e = Encoder::new();
    for i in 0..32u32 {
        e.u32(if i < 3 {
            u32::from(b"abc"[i as usize])
        } else {
            0
        });
    }
    e.u64(16_777_216)
        .i32(8)
        .i32(3000)
        .i32(1)
        .i32(1)
        .i32(4)
        .i32(2);
    let buf = e.into_inner();
    let info = NodeInfo::decode(&mut Decoder::new(&buf)).unwrap();
    assert_eq!(info.model, "abc");
    assert_eq!(info.memory_kib, 16_777_216);
    assert_eq!((info.cpus, info.cores, info.threads), (8, 4, 2));
}

#[test]
fn remote_error_display_falls_back_without_message() {
    let e = RemoteError {
        code: 1,
        domain: 2,
        message: None,
        level: 0,
    };
    assert_eq!(e.to_string(), "unknown libvirt error (code 1, domain 2)");
}

#[test]
fn unknown_state_codes_are_kept() {
    assert_eq!(DomainState::from(5), DomainState::Shutoff);
    assert_eq!(DomainState::from(99), DomainState::Unknown(99));
    assert_eq!(LifecycleEvent::from(2), LifecycleEvent::Started);
}

#[test]
fn generated_procedure_numbers_match_the_protocol() {
    assert_eq!(procs::CONNECT_OPEN, 1);
    assert_eq!(procs::CONNECT_CLOSE, 2);
}
