use super::*;
use crate::inflate::tests::zlib_chunks;

fn sent(actions: &[Action]) -> Vec<u8> {
    actions
        .iter()
        .filter_map(|a| {
            if let Action::Send(b) = a {
                Some(b.clone())
            } else {
                None
            }
        })
        .flatten()
        .collect()
}

fn server_init(w: u16, h: u16) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(w.to_be_bytes());
    b.extend(h.to_be_bytes());
    b.extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
    b.extend(4u32.to_be_bytes());
    b.extend(b"test");
    b
}

/// A client past the handshake with a `w` x `h` framebuffer.
fn connected(w: u16, h: u16) -> Client {
    let mut c = Client::new(Options::default());
    let a = c.feed(b"RFB 003.008\n").unwrap();
    assert_eq!(sent(&a), b"RFB 003.008\n");
    let a = c.feed(&[2, 2, 1]).unwrap();
    assert_eq!(sent(&a), [1]);
    let a = c.feed(&[0, 0, 0, 0]).unwrap();
    assert_eq!(sent(&a), [1], "ClientInit, shared");
    // ServerInit split across two reads.
    let init = server_init(w, h);
    assert_eq!(c.feed(&init[..10]).unwrap(), []);
    let a = c.feed(&init[10..]).unwrap();
    assert!(a.contains(&Action::Connected("test".into())));
    assert!(a.contains(&Action::Resize {
        width: w,
        height: h
    }));
    let out = sent(&a);
    assert_eq!(&out[..8], &[0, 0, 0, 0, 32, 24, 0, 1]);
    assert_eq!(out[20], 2, "SetEncodings follows SetPixelFormat");
    assert_eq!(
        &out[out.len() - 10..out.len() - 4],
        &[3, 0, 0, 0, 0, 0],
        "full update request"
    );
    c
}

fn update(rects: &[(Rect, i32, Vec<u8>)]) -> Vec<u8> {
    let mut b = vec![0, 0];
    b.extend((rects.len() as u16).to_be_bytes());
    for (r, e, body) in rects {
        for v in [r.x, r.y, r.w, r.h] {
            b.extend(v.to_be_bytes());
        }
        b.extend(e.to_be_bytes());
        b.extend(body);
    }
    b
}

fn dirty(a: &[Action]) -> Vec<Rect> {
    a.iter()
        .filter_map(|a| {
            if let Action::Dirty(r) = a {
                Some(*r)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn rejects_authentication_only_servers() {
    let mut c = Client::new(Options::default());
    c.feed(b"RFB 003.008\n").unwrap();
    assert!(c.feed(&[1, 2]).is_err());
    assert!(c.feed(&[]).is_err(), "errors are sticky");
}

#[test]
fn rfb_33_handshake() {
    let mut c = Client::new(Options::default());
    assert_eq!(sent(&c.feed(b"RFB 003.003\n").unwrap()), b"RFB 003.003\n");
    assert_eq!(sent(&c.feed(&[0, 0, 0, 1]).unwrap())[..1], [1]);
    c.feed(&server_init(4, 4)).unwrap();
    assert!(c.is_connected());
}

#[test]
fn raw_streamed_byte_by_byte_then_copyrect() {
    let mut c = connected(4, 2);
    let px: Vec<u8> = (0..8u8).flat_map(|i| [i * 10, i, 0, 0]).collect();
    let msg = update(&[
        (Rect::new(0, 0, 4, 2), encoding::RAW, px),
        (Rect::new(2, 0, 2, 1), encoding::COPY_RECT, vec![0, 0, 0, 1]),
    ]);
    let mut actions = Vec::new();
    for b in &msg {
        actions.extend(c.feed(&[*b]).unwrap());
    }
    assert_eq!(
        dirty(&actions),
        [Rect::new(0, 0, 4, 2), Rect::new(2, 0, 2, 1)]
    );
    let fb = c.framebuffer();
    assert_eq!(fb.pixel(1, 1), [50, 5, 0, 255]);
    assert_eq!(fb.pixel(2, 0), [40, 4, 0, 255], "copied from (0,1)");
    assert_eq!(fb.pixel(3, 0), [50, 5, 0, 255]);
    // Incremental request after the update.
    assert_eq!(sent(&actions), [3, 1, 0, 0, 0, 0, 0, 4, 0, 2]);
}

#[test]
fn zrle_and_tight_keep_zlib_streams() {
    let mut c = connected(8, 8);
    let solid = [1u8, 9, 8, 7];
    let raw: Vec<u8> = std::iter::once(0)
        .chain((0..4u8).flat_map(|i| [i, i, i]))
        .collect();
    let z = zlib_chunks(&[&solid, &raw]);
    let zrle = |d: &Vec<u8>| {
        let mut b = (d.len() as u32).to_be_bytes().to_vec();
        b.extend(d);
        b
    };
    let a = c
        .feed(&update(&[
            (Rect::new(0, 0, 8, 8), encoding::ZRLE, zrle(&z[0])),
            (Rect::new(6, 6, 2, 2), encoding::ZRLE, zrle(&z[1])),
        ]))
        .unwrap();
    assert_eq!(dirty(&a).len(), 2);
    assert_eq!(c.framebuffer().pixel(0, 0), [9, 8, 7, 255]);
    assert_eq!(c.framebuffer().pixel(7, 7), [3, 3, 3, 255]);

    // Tight basic copy filter on stream 1, zlib-compressed (48 bytes).
    let rgb: Vec<u8> = (0..16u8).flat_map(|i| [i, 100, 200]).collect();
    let tz = zlib_chunks(&[&rgb]);
    let mut body = vec![0x10 | 0x02]; // stream 1, reset stream 1
    body.push(tz[0].len() as u8);
    body.extend(&tz[0]);
    let a = c
        .feed(&update(&[(Rect::new(0, 4, 4, 4), encoding::TIGHT, body)]))
        .unwrap();
    assert_eq!(dirty(&a), [Rect::new(0, 4, 4, 4)]);
    assert_eq!(c.framebuffer().pixel(3, 7), [15, 100, 200, 255]);
}

#[test]
fn tight_jpeg_pauses_until_decoded() {
    let mut c = connected(4, 4);
    let mut msg = update(&[
        (
            Rect::new(0, 0, 2, 2),
            encoding::TIGHT,
            vec![0x90, 3, 0xff, 0xd8, 0xff],
        ),
        (Rect::new(2, 2, 2, 2), encoding::TIGHT, vec![0x80, 5, 6, 7]),
    ]);
    msg.push(2); // Bell, after the update
    let a = c.feed(&msg).unwrap();
    assert_eq!(
        a,
        [Action::DecodeImage {
            rect: Rect::new(0, 0, 2, 2),
            format: ImageFormat::Jpeg,
            data: vec![0xff, 0xd8, 0xff]
        }]
    );
    assert!(c.feed(&[]).unwrap().is_empty(), "still paused");
    let a = c.image_decoded(Some(&[1; 16])).unwrap();
    assert_eq!(dirty(&a), [Rect::new(0, 0, 2, 2), Rect::new(2, 2, 2, 2)]);
    assert!(a.contains(&Action::Bell));
    assert_eq!(c.framebuffer().pixel(1, 1), [1, 1, 1, 255]);
    assert_eq!(c.framebuffer().pixel(3, 3), [5, 6, 7, 255]);
}

#[test]
fn pseudo_encodings() {
    let mut c = connected(4, 4);
    let mut cursor: Vec<u8> = vec![255; 2 * 2 * 4];
    cursor.extend([0b1000_0000, 0b0100_0000]);
    let a = c
        .feed(&update(&[
            (
                Rect::new(0, 0, 0, 0),
                encoding::QEMU_EXTENDED_KEY_EVENT,
                vec![],
            ),
            (Rect::new(1, 0, 2, 2), encoding::CURSOR, cursor),
            (Rect::new(0, 0, 0, 0), encoding::QEMU_LED_STATE, vec![4]),
            (Rect::new(0, 0, 16, 8), encoding::DESKTOP_SIZE, vec![]),
            (Rect::new(0, 0, 0, 0), encoding::LAST_RECT, vec![]),
        ]))
        .unwrap();
    let Some(Action::Cursor(cur)) = a.iter().find(|a| matches!(a, Action::Cursor(_))) else {
        panic!("no cursor action in {a:?}")
    };
    assert_eq!((cur.hotspot_x, cur.width), (1, 2));
    assert_eq!(
        cur.rgba
            .iter()
            .skip(3)
            .step_by(4)
            .copied()
            .collect::<Vec<_>>(),
        [255, 0, 0, 255]
    );
    assert!(a.contains(&Action::Leds(4)));
    assert!(a.contains(&Action::Resize {
        width: 16,
        height: 8
    }));
    // After a resize the next request is non-incremental, full size.
    assert_eq!(sent(&a), [3, 0, 0, 0, 0, 0, 0, 16, 0, 8]);
    assert!(c.has_extended_keys());
    // Right Ctrl (0xE0 0x1D) is sent as QEMU keycode 0x9D.
    let k = sent(&c.key(true, 0xffe4, 0xe01d));
    assert_eq!(k, [255, 0, 0, 1, 0, 0, 0xff, 0xe4, 0, 0, 0, 0x9d]);
    let p = sent(&c.pointer(100, 3, 1));
    assert_eq!(p, [5, 1, 0, 15, 0, 3], "clamped to the framebuffer");
}

#[test]
fn extended_desktop_size_and_bounds() {
    let mut c = connected(4, 4);
    let mut body = vec![1, 0, 0, 0];
    body.extend([0; 16]);
    let a = c
        .feed(&update(&[(
            Rect::new(0, 0, 32, 16),
            encoding::EXTENDED_DESKTOP_SIZE,
            body,
        )]))
        .unwrap();
    assert!(a.contains(&Action::Resize {
        width: 32,
        height: 16
    }));
    assert!(
        c.feed(&update(&[(
            Rect::new(30, 0, 4, 1),
            encoding::RAW,
            vec![0; 16]
        )]))
        .is_err()
    );
    let mut c = connected(4, 4);
    assert!(
        c.feed(&update(&[(
            Rect::new(0, 0, 9000, 1),
            encoding::DESKTOP_SIZE,
            vec![]
        )]))
        .is_err()
    );
}
