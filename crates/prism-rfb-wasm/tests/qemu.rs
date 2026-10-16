//! Drives the client against a live QEMU VNC server.
//!
//! Ignored by default. Run it with `--ignored` and set `PRISM_RFB_TEST_SOCKET`
//! to the path of QEMU's VNC UNIX socket, for example after
//! `qemu-system-x86_64 -display none -vnc unix:/tmp/vnc.sock -cdrom some.iso`.
//! Add `--nocapture` to see per-encoding decode timings, and set
//! `PRISM_RFB_TEST_DUMP` to a directory to write each framebuffer as a PPM.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use prism_rfb_wasm::{Action, Client, Options, encoding};

struct Session {
    client: Client,
    sock: UnixStream,
    buf: Vec<u8>,
    decode_time: Duration,
    images: usize,
}

impl Session {
    fn connect(path: &str, opts: Options) -> Self {
        let sock = UnixStream::connect(path).expect("connect to VNC socket");
        sock.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        Self {
            client: Client::new(opts),
            sock,
            buf: vec![0; 1 << 16],
            decode_time: Duration::ZERO,
            images: 0,
        }
    }

    fn act(&mut self, actions: Vec<Action>) -> u64 {
        let mut dirty = 0;
        for a in actions {
            match a {
                Action::Send(b) => self.sock.write_all(&b).unwrap(),
                Action::Dirty(r) => dirty += r.area() as u64,
                Action::DecodeImage { rect, .. } => {
                    // No image decoder natively; leave the area as is.
                    self.images += 1;
                    dirty += rect.area() as u64;
                    let more = self.client.image_decoded(None).unwrap();
                    dirty += self.act(more);
                }
                _ => {}
            }
        }
        dirty
    }

    /// Reads once from the socket; returns the dirty pixel count.
    fn pump(&mut self) -> u64 {
        let n = self.sock.read(&mut self.buf).expect("read from VNC socket");
        assert!(n > 0, "server closed the connection");
        let start = Instant::now();
        let actions = self.client.feed(&self.buf[..n]).expect("protocol error");
        self.decode_time += start.elapsed();
        self.act(actions)
    }

    fn fb_area(&self) -> u64 {
        u64::from(self.client.framebuffer().width()) * u64::from(self.client.framebuffer().height())
    }

    /// Requests full refreshes until `frames` complete screens arrived.
    fn full_frames(&mut self, frames: u32) -> Duration {
        self.decode_time = Duration::ZERO;
        for _ in 0..frames {
            let a = self.client.refresh();
            self.act(a);
            let mut got = 0;
            while got < self.fb_area() {
                got += self.pump();
            }
        }
        self.decode_time
    }

    fn lit_pixels(&self) -> usize {
        self.client
            .framebuffer()
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] > 32 || p[1] > 32 || p[2] > 32)
            .count()
    }
}

#[test]
#[ignore = "needs a QEMU VNC socket in PRISM_RFB_TEST_SOCKET"]
fn qemu_vnc_end_to_end() {
    let path = std::env::var("PRISM_RFB_TEST_SOCKET")
        .expect("set PRISM_RFB_TEST_SOCKET to QEMU's VNC UNIX socket path");
    let cases: [(&str, &[i32]); 5] = [
        ("Raw", &[encoding::RAW]),
        ("Hextile", &[encoding::HEXTILE]),
        ("ZRLE", &[encoding::ZRLE]),
        ("Tight", &[encoding::TIGHT, encoding::COPY_RECT]),
        ("TightPNG", &[encoding::TIGHT_PNG]),
    ];
    for (name, encs) in cases {
        let mut s = Session::connect(&path, Options::with_encodings(encs, None, Some(6)));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !s.client.is_connected() || s.fb_area() == 0 {
            s.pump();
        }
        // Wait for the initial full update and some content.
        let mut got = 0;
        while got < s.fb_area() && Instant::now() < deadline {
            got += s.pump();
        }
        assert!(got >= s.fb_area(), "{name}: no complete first frame");
        let frames = 10;
        let t = s.full_frames(frames);
        let fb = s.client.framebuffer();
        let lit = s.lit_pixels();
        eprintln!(
            "{name:>8}: {}x{}, {lit} lit pixels, {frames} full frames decoded in {t:?} ({:.0} frames/s decode-only), {} images",
            fb.width(),
            fb.height(),
            f64::from(frames) / t.as_secs_f64(),
            s.images,
        );
        if let Ok(dir) = std::env::var("PRISM_RFB_TEST_DUMP") {
            let mut ppm = format!("P6 {} {} 255\n", fb.width(), fb.height()).into_bytes();
            ppm.extend(
                fb.pixels()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[0], p[1], p[2]]),
            );
            std::fs::create_dir_all(&dir).expect("create the dump directory");
            std::fs::write(format!("{dir}/{name}.ppm"), ppm).expect("write the PPM dump");
        }
        if name == "TightPNG" {
            // QEMU sends PNG rectangles, which only the browser decodes.
            assert!(s.images > 0, "{name}: no PNG rectangles");
        } else {
            assert!(lit > 100, "{name}: framebuffer is blank");
        }
        assert!(
            s.client.has_extended_keys(),
            "{name}: QEMU extended key events not acknowledged"
        );
    }
}
