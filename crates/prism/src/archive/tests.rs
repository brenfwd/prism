use super::*;

fn member(node: u128, name: &str, weight: f64) -> ArchiveMemberDoc {
    ArchiveMemberDoc {
        node: Uuid::from_u128(node),
        name: name.into(),
        pool: "archive".into(),
        weight,
        reserve: 10,
        state: MemberState::Active,
    }
}

#[test]
fn paths() {
    assert!(valid_path("isos/alpine.iso"));
    assert!(valid_path("backups/web/2026-10-15.qcow2"));
    assert!(!valid_path("../etc/passwd"));
    assert!(!valid_path("a//b"));
    assert!(!valid_path("/abs"));
    assert!(!valid_path(".prism-partial/x"));
    assert!(!valid_path(""));
}

#[test]
fn placement_respects_reserve_and_prefers_free_space() {
    let (a, b, c) = (
        member(1, "a", 1.0),
        member(2, "b", 1.0),
        member(3, "c", 1.0),
    );
    let cands = [(&a, 15u64), (&b, 1_000_000), (&c, 5)];
    // `a` would drop below its reserve with a 10-byte file; `c` already is.
    for seed in 0..50u128 {
        assert_eq!(
            place(&cands, 10, seed * 7919 + (seed << 64)).unwrap().name,
            "b"
        );
    }
    assert!(place(&[(&c, 5)], 1, 0).is_none());
}

#[test]
fn placement_spreads_by_weight() {
    let (a, b) = (member(1, "a", 1.0), member(2, "b", 1.0));
    let cands = [(&a, 1000u64), (&b, 1000)];
    let mut hits = 0;
    for seed in 0..200u128 {
        let s = (seed * 0x9E37_79B9_7F4A_7C15) | ((seed * 0xD1B5_4A32_D192_ED03) << 64);
        if place(&cands, 1, s).unwrap().name == "a" {
            hits += 1;
        }
    }
    assert!((40..160).contains(&hits), "a got {hits}/200");
}

#[tokio::test]
async fn write_read_delete_nested() {
    let dir = tempfile::tempdir().unwrap();
    let m = LocalMember::open("m", "archive", dir.path()).unwrap();
    let body = futures::stream::iter(vec![Ok::<_, std::io::Error>(bytes::Bytes::from_static(
        b"hello",
    ))]);
    let (size, hash) = m.write("isos/a/b.iso", body).await.unwrap();
    assert_eq!(size, 5);
    assert_eq!(hash, blake3::hash(b"hello").to_hex().to_string());
    assert!(m.open_read("isos/a/b.iso").is_ok());
    let again = futures::stream::iter(vec![Ok::<_, std::io::Error>(bytes::Bytes::new())]);
    assert!(matches!(
        m.write("isos/a/b.iso", again).await,
        Err(StorageError::Exists(_))
    ));
    m.delete("isos/a/b.iso").unwrap();
    assert!(m.open_read("isos/a/b.iso").is_err());
    assert!(m.open_read("../x").is_err());
}
