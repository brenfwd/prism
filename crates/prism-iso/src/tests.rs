use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::*;

/// A minimal, strict ISO 9660 reader used to check the writer.
struct Parsed {
    volume_id: String,
    total_sectors: u32,
    files: Vec<(String, Vec<u8>)>,
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}

fn both32_read(b: &[u8]) -> u32 {
    let (l, m) = (le32(b), be32(&b[4..]));
    assert_eq!(l, m, "both-endian mismatch");
    l
}

fn both16_read(b: &[u8]) -> u16 {
    let l = u16::from_le_bytes([b[0], b[1]]);
    let m = u16::from_be_bytes([b[2], b[3]]);
    assert_eq!(l, m, "both-endian mismatch");
    l
}

fn sector(img: &[u8], lba: u32) -> &[u8] {
    &img[lba as usize * SECTOR..(lba as usize + 1) * SECTOR]
}

fn decode(name: &[u8], joliet: bool) -> String {
    if joliet {
        let units: Vec<u16> = name
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&units).unwrap()
    } else {
        String::from_utf8(name.to_vec()).unwrap()
    }
}

fn parse(img: &[u8], descriptor_type: u8) -> Parsed {
    assert_eq!(img.len() % SECTOR, 0);
    assert!(img[..16 * SECTOR].iter().all(|&b| b == 0));
    // Walk the descriptor set.
    let mut lba = 16;
    let mut found = None;
    loop {
        let d = sector(img, lba);
        assert_eq!(&d[1..6], b"CD001");
        assert_eq!(d[6], 1);
        if d[0] == 255 {
            break;
        }
        if d[0] == descriptor_type {
            found = Some(lba);
        }
        lba += 1;
    }
    let d = sector(img, found.expect("descriptor present"));
    let joliet = descriptor_type == 2;
    if joliet {
        assert_eq!(&d[88..91], b"%/E");
    }
    let volume_id = decode(&d[40..72], joliet).trim_end().to_owned();
    let total_sectors = both32_read(&d[80..88]);
    assert_eq!(total_sectors as usize * SECTOR, img.len());
    assert_eq!(both16_read(&d[128..132]), 2048);
    assert_eq!(&d[813..817], b"2024"); // creation year (fixed in tests)

    // Path tables: L and M must agree with the root record.
    let pt_len = both32_read(&d[132..140]) as usize;
    let l = &sector(img, le32(&d[140..144]))[..pt_len];
    let m = &sector(img, be32(&d[148..152]))[..pt_len];
    let root = &d[156..190];
    assert_eq!(root[0], 34);
    assert_eq!(root[25], 2);
    let root_lba = both32_read(&root[2..10]);
    let root_len = both32_read(&root[10..18]);
    assert_eq!(l[0], 1);
    assert_eq!(le32(&l[2..6]), root_lba);
    assert_eq!(u16::from_le_bytes([l[6], l[7]]), 1);
    assert_eq!(m[0], 1);
    assert_eq!(be32(&m[2..6]), root_lba);
    assert_eq!(u16::from_be_bytes([m[6], m[7]]), 1);

    // Root directory records.
    let dir = &img[root_lba as usize * SECTOR..][..root_len as usize];
    let mut files = Vec::new();
    let mut off = 0;
    let mut idx = 0;
    while off < dir.len() {
        let len = dir[off] as usize;
        if len == 0 {
            off = (off / SECTOR + 1) * SECTOR;
            continue;
        }
        assert_eq!(
            off / SECTOR,
            (off + len - 1) / SECTOR,
            "record crosses sector"
        );
        let r = &dir[off..off + len];
        let name_len = r[32] as usize;
        assert_eq!(len, 33 + name_len + usize::from(name_len.is_multiple_of(2)));
        let name = &r[33..33 + name_len];
        let extent = both32_read(&r[2..10]);
        let size = both32_read(&r[10..18]);
        assert_eq!(both16_read(&r[28..32]), 1);
        assert_eq!(r[18], 124); // 2024
        match idx {
            0 => assert_eq!((name, extent, size), (&[0u8][..], root_lba, root_len)),
            1 => assert_eq!((name, extent, size), (&[1u8][..], root_lba, root_len)),
            _ => {
                assert_eq!(r[25], 0);
                let data = img[extent as usize * SECTOR..][..size as usize].to_vec();
                files.push((decode(name, joliet), data));
            }
        }
        idx += 1;
        off += len;
    }
    Parsed {
        volume_id,
        total_sectors,
        files,
    }
}

fn fixed_time() -> SystemTime {
    // 2024-02-29T12:34:56Z
    UNIX_EPOCH + Duration::from_secs(1_709_210_096)
}

fn seed() -> IsoBuilder {
    IsoBuilder::new("CIDATA")
        .timestamp(fixed_time())
        .file("user-data", "#cloud-config\nhostname: tb-1\n")
        .file("meta-data", "instance-id: tb-1\nlocal-hostname: tb-1\n")
        .file("network-config", "version: 2\n")
}

#[test]
fn round_trip_joliet() {
    let img = seed().build().unwrap();
    let p = parse(&img, 2);
    assert_eq!(p.volume_id, "CIDATA");
    let names: Vec<_> = p.files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["meta-data", "network-config", "user-data"]);
    assert_eq!(p.files[2].1, b"#cloud-config\nhostname: tb-1\n");
    assert_eq!(p.files[1].1, b"version: 2\n");
}

#[test]
fn round_trip_primary() {
    let img = seed().build().unwrap();
    let p = parse(&img, 1);
    assert_eq!(p.volume_id, "CIDATA");
    let names: Vec<_> = p.files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["META_DATA.;1", "NETWORK_CONFIG.;1", "USER_DATA.;1"]);
    assert_eq!(p.files[0].1, b"instance-id: tb-1\nlocal-hostname: tb-1\n");
}

#[test]
fn layout_is_compact_and_deterministic() {
    let a = seed().build().unwrap();
    let b = seed().build().unwrap();
    assert_eq!(a, b);
    // 23 fixed sectors, two one-sector directories, three one-sector files.
    assert_eq!(parse(&a, 1).total_sectors, 28);
    let mut buf = Vec::new();
    seed().write(&mut buf).unwrap();
    assert_eq!(buf, a);
}

#[test]
fn large_and_empty_files_and_multi_sector_directory() {
    let big: Vec<u8> = (0..10_000u32).map(|i| (i * 7) as u8).collect();
    let mut b = IsoBuilder::new("BIG")
        .timestamp(fixed_time())
        .file("big.bin", big.clone())
        .file("empty", Vec::new());
    // Enough long names to spill the directory past one sector.
    for i in 0..40 {
        b = b.file(format!("file-with-a-long-name-{i:02}.txt"), format!("{i}"));
    }
    let img = b.build().unwrap();
    for t in [1, 2] {
        let p = parse(&img, t);
        assert_eq!(p.files.len(), 42);
        let get = |n: &str| p.files.iter().find(|(k, _)| k == n).map(|(_, d)| d);
        if t == 2 {
            assert_eq!(get("big.bin").unwrap(), &big);
            assert_eq!(get("empty").unwrap().len(), 0);
            assert_eq!(get("file-with-a-long-name-39.txt").unwrap(), b"39");
        } else {
            assert_eq!(get("BIG.BIN;1").unwrap(), &big);
        }
    }
}

#[test]
fn primary_names_are_mapped_and_deduplicated() {
    let taken = BTreeSet::new();
    assert_eq!(primary_name("user-data", &taken), b"USER_DATA.;1");
    assert_eq!(primary_name("Foo.tar.gz", &taken), b"FOO_TAR.GZ;1");
    assert_eq!(primary_name(".hidden", &taken), b"_HIDDEN.;1");
    let taken: BTreeSet<_> = [b"A_B.;1".to_vec()].into();
    assert_eq!(primary_name("a-b", &taken), b"A_B_1.;1");
    let long = "x".repeat(60);
    assert!(primary_name(&long, &BTreeSet::new()).len() <= 32);

    let img = IsoBuilder::new("X")
        .timestamp(fixed_time())
        .file("a-b", "1")
        .file("a_b", "2")
        .build()
        .unwrap();
    let p = parse(&img, 1);
    let names: Vec<_> = p.files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["A_B.;1", "A_B_1.;1"]);
}

#[test]
fn rejects_bad_input() {
    let t = |b: IsoBuilder| b.build().unwrap_err().to_string();
    assert!(t(IsoBuilder::new("cidata")).contains("volume identifier"));
    assert!(t(IsoBuilder::new("")).contains("volume identifier"));
    assert!(t(IsoBuilder::new("A".repeat(17))).contains("volume identifier"));
    assert!(t(IsoBuilder::new("X").file("a/b", "")).contains("invalid file name"));
    assert!(t(IsoBuilder::new("X").file("..", "")).contains("invalid file name"));
    assert!(t(IsoBuilder::new("X").file("x".repeat(65), "")).contains("64"));
    assert!(t(IsoBuilder::new("X").file("a", "").file("a", "")).contains("duplicate"));
}

#[test]
fn timestamps() {
    let t = Timestamp::from_system_time(fixed_time());
    assert_eq!(
        t,
        Timestamp {
            year: 2024,
            month: 2,
            day: 29,
            hour: 12,
            minute: 34,
            second: 56
        }
    );
    assert_eq!(&t.descriptor()[..16], b"2024022912345600");
    assert_eq!(t.record(), [124, 2, 29, 12, 34, 56, 0]);
    let epoch = Timestamp::from_system_time(UNIX_EPOCH);
    assert_eq!((epoch.year, epoch.month, epoch.day), (1970, 1, 1));
    let y2k = Timestamp::from_system_time(UNIX_EPOCH + Duration::from_secs(946_684_799));
    assert_eq!(
        (
            y2k.year, y2k.month, y2k.day, y2k.hour, y2k.minute, y2k.second
        ),
        (1999, 12, 31, 23, 59, 59)
    );
}

/// Cross-checks against whichever external ISO readers are installed.
/// Each one is skipped when absent.
#[test]
fn external_tools() {
    let dir = std::env::temp_dir().join(format!("prism-iso-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("seed.iso");
    seed().write(std::fs::File::create(&path).unwrap()).unwrap();

    let run = |prog: &str, args: &[&str]| -> Option<String> {
        let out = Command::new(prog).args(args).output().ok()?;
        assert!(
            out.status.success(),
            "{prog} {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let iso = path.to_str().unwrap();
    let mut checked = Vec::new();

    if let Some(out) = run("blkid", &["-p", "-o", "export", iso]) {
        assert!(out.contains("LABEL=CIDATA"), "{out}");
        assert!(out.contains("TYPE=iso9660"), "{out}");
        checked.push("blkid");
    }
    if let Some(out) = run("isoinfo", &["-J", "-l", "-i", iso]) {
        assert!(out.contains("user-data"), "{out}");
        checked.push("isoinfo");
    }
    if let Some(out) = run("isoinfo", &["-J", "-x", "/user-data", "-i", iso]) {
        assert!(out.starts_with("#cloud-config"), "{out}");
    }
    if let Some(out) = run(
        "xorriso",
        &["-indev", iso, "-find", "/", "-exec", "lsdl", "--"],
    ) {
        assert!(out.contains("user-data"), "{out}");
        checked.push("xorriso");
    }
    if let Some(out) = run("7z", &["l", iso]) {
        assert!(out.contains("user-data"), "{out}");
        checked.push("7z");
    }
    if let Some(out) = run("bsdtar", &["-tf", iso]) {
        assert!(out.contains("user-data"), "{out}");
        checked.push("bsdtar");
    }
    if let Some(out) = run("bsdtar", &["-xOf", iso, "network-config"]) {
        assert_eq!(out, "version: 2\n");
    }
    if Path::new(iso).exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    eprintln!("external ISO checks run with: {checked:?}");
}
