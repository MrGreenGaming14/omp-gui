//! Codec tests against the testkit reference encoder, and the client against a fake RFB server on loopback.

use crate::pixels::RgbaImage;
use crate::testkit::rfb::{self, Zrle, rect, update};
use crate::vnc::decode::{Decoders, Update};
use crate::vnc::proto::{Msg, vnc_response};
use crate::vnc::{ConnectionState, RfbEndpoint, RfbOptions, RfbServer, RfbSurface};
use flate2::{Compress, Compression};
use loa_contracts::surface::{Encoding, Frame, Input, InputEvent, Modifiers, MouseButton, Rect, Surface, SurfaceError, SurfaceServer, SurfaceUri};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

const R: [u8; 4] = [255, 0, 0, 255];
const G: [u8; 4] = [0, 255, 0, 255];
const B: [u8; 4] = [0, 0, 255, 255];
const W: [u8; 4] = [255, 255, 255, 255];
const K: [u8; 4] = [0, 0, 0, 255];
const LIMIT: Duration = Duration::from_secs(10);

fn picture(width: u32, height: u32, px: impl Fn(u32, u32) -> [u8; 4]) -> RgbaImage {
    let px = &px;
    RgbaImage { width, height, data: (0..height).flat_map(|y| (0..width).flat_map(move |x| px(x, y))).collect() }
}

fn flat(pixels: &[[u8; 4]]) -> Vec<u8> {
    pixels.concat()
}

#[tokio::test]
async fn every_encoding_decodes_what_the_reference_encoder_wrote() {
    let palette = [R, G, B, W, K, [9, 9, 9, 255]];
    let pictures = [
        picture(70, 20, |x, y| [x as u8 * 3, y as u8 * 12, 7, 255]),
        picture(70, 5, |x, y| palette[((x + y) % 2) as usize]),
        picture(18, 17, |x, y| palette[((x / 3 + y) % 3) as usize]),
        picture(9, 3, |x, y| palette[((x + y) % 5) as usize]),
        picture(300, 2, |x, _| if x < 290 { R } else { G }),
        picture(5, 4, |_, _| B),
    ];
    let mut decoders = Decoders::new();
    let mut z = Compress::new(Compression::default(), true);
    for (n, img) in pictures.iter().enumerate() {
        let r = img.full();
        let mut cases = vec![(0, rfb::raw(img)), (2, rfb::rre(img)), (5, rfb::hextile(img))];
        for mode in [Zrle::Raw, Zrle::Packed, Zrle::PlainRle, Zrle::PaletteRle] {
            cases.push((16, rfb::zrle(img, mode, &mut z)));
        }
        for (encoding, payload) in cases {
            let (decoder, scratch) = decoders.get(encoding).unwrap();
            let mut wire = payload.as_slice();
            assert_eq!(decoder.decode(r, &mut wire, scratch).await.unwrap(), Update::Blit);
            assert!(wire.is_empty(), "picture {n}, encoding {encoding}: bytes left over");
            assert!(scratch.data == img.data, "picture {n}, encoding {encoding}");
        }
    }
    let (cursor, scratch) = decoders.get(-239).unwrap();
    let wire = [rfb::raw(&picture(2, 2, |x, y| [R, G, B, W][(y * 2 + x) as usize])), vec![0b1000_0000, 0b0100_0000]].concat();
    let Update::Cursor(c) = cursor.decode(Rect { x: 1, y: 0, width: 2, height: 2 }, &mut wire.as_slice(), scratch).await.unwrap() else { panic!() };
    assert_eq!((c.hotspot_x, c.hotspot_y, c.width, c.height), (1, 0, 2, 2));
    assert_eq!(c.rgba, flat(&[R, [0, 255, 0, 0], [0, 0, 255, 0], W]));
    // A subrectangle outside its rectangle is a protocol error.
    let bad = [&1u32.to_be_bytes()[..], &rfb::pixel(&R), &rfb::pixel(&G), &[0, 1, 0, 0, 0, 1, 0, 1]].concat();
    let (rre, scratch) = decoders.get(2).unwrap();
    assert!(rre.decode(Rect { x: 0, y: 0, width: 1, height: 1 }, &mut bad.as_slice(), scratch).await.is_err());
}

enum Security {
    None,
    Vnc(&'static str),
}

/// The server side of one connection.
struct Fake(TcpStream);

impl Fake {
    async fn accept(listener: &TcpListener) -> Self {
        Self(tokio::time::timeout(LIMIT, listener.accept()).await.unwrap().unwrap().0)
    }

    async fn send(&mut self, bytes: &[u8]) {
        self.0.write_all(bytes).await.unwrap();
    }

    async fn read(&mut self, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        tokio::time::timeout(LIMIT, self.0.read_exact(&mut buf)).await.unwrap().unwrap();
        buf
    }

    async fn expect(&mut self, want: &[u8]) {
        assert_eq!(self.read(want.len()).await, want);
    }

    async fn expect_request(&mut self, width: u16, height: u16) {
        self.expect(&Msg::UpdateRequest { incremental: true, width, height }.bytes()).await;
    }

    /// RFB 3.8 up to ServerInit; false when the client's VNC response was wrong.
    async fn handshake(&mut self, security: Security, width: u16, height: u16) -> bool {
        self.send(b"RFB 003.008\n").await;
        self.expect(b"RFB 003.008\n").await;
        match security {
            Security::None => {
                self.send(&[1, 1]).await;
                self.expect(&[1]).await;
            }
            Security::Vnc(password) => {
                self.send(&[2, 16, 2]).await;
                self.expect(&[2]).await;
                let challenge: [u8; 16] = std::array::from_fn(|i| (i as u8).wrapping_mul(37));
                self.send(&challenge).await;
                if self.read(16).await != vnc_response(password.as_bytes(), &challenge) {
                    self.send(&[&1u32.to_be_bytes()[..], &12u32.to_be_bytes(), b"bad password"].concat()).await;
                    return false;
                }
            }
        }
        self.send(&0u32.to_be_bytes()).await;
        self.expect(&[1]).await; // ClientInit, shared
        self.server_init(width, height, b"fake desk").await;
        true
    }

    async fn server_init(&mut self, width: u16, height: u16, name: &[u8]) {
        // Server native format: 32 bpp big endian, blue in the low byte.
        let format = [32, 24, 1, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0];
        let init = [&width.to_be_bytes()[..], &height.to_be_bytes(), &format, &(name.len() as u32).to_be_bytes(), name].concat();
        self.send(&init).await;
        let encodings = [1i32, 16, 5, 2, 0, -239, -223];
        let setup = [Msg::SetPixelFormat, Msg::SetEncodings(&encodings), Msg::UpdateRequest { incremental: false, width, height }];
        self.expect(&setup.iter().flat_map(|m| m.bytes()).collect::<Vec<_>>()).await;
    }
}

async fn next(rx: &mut broadcast::Receiver<Arc<Frame>>) -> Arc<Frame> {
    tokio::time::timeout(LIMIT, rx.recv()).await.unwrap().unwrap()
}

fn uri() -> SurfaceUri {
    SurfaceUri::new("vm", "desk").unwrap()
}

struct Harness {
    listener: TcpListener,
    server: Arc<RfbServer>,
    password_calls: Arc<AtomicUsize>,
}

impl Harness {
    async fn new(password: Option<&'static str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let password_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&password_calls);
        let passwords = move |endpoint: &str| {
            assert_eq!(endpoint, "desk");
            calls.fetch_add(1, Ordering::SeqCst);
            password.map(str::to_owned)
        };
        let options = RfbOptions { backoff_initial: Duration::from_millis(1), backoff_max: Duration::from_millis(20), ..RfbOptions::default() };
        let server = Arc::new(RfbServer::new(vec![RfbEndpoint { name: "desk".into(), address }], Arc::new(passwords), options));
        Self { listener, server, password_calls }
    }

    async fn open(&self) -> tokio::task::JoinHandle<crate::vnc::Result<Arc<RfbSurface>, SurfaceError>> {
        let server = Arc::clone(&self.server);
        tokio::spawn(async move { server.open_rfb(&uri()).await })
    }

    /// Opens `surface://vm/desk` against a fake that completes the handshake.
    async fn connect(&self, security: Security, width: u16, height: u16) -> (Arc<RfbSurface>, Fake) {
        let open = self.open().await;
        let mut fake = Fake::accept(&self.listener).await;
        assert!(fake.handshake(security, width, height).await);
        (open.await.unwrap().unwrap(), fake)
    }
}

fn region(x: u32, y: u32, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}

#[tokio::test]
async fn updates_become_frames_keyframes_and_cursor_shapes() {
    let h = Harness::new(None).await;
    let (surface, mut fake) = h.connect(Security::None, 4, 2).await;
    let info = surface.info();
    assert_eq!((info.name.as_str(), info.width, info.height), ("fake desk", 4, 2));
    let mut rx = surface.frames().unwrap();
    let raw = |px: &[[u8; 4]]| rfb::raw(&RgbaImage { width: px.len() as u32, height: 1, data: flat(px) });

    // Only part of the screen arrives, yet the first frame is the whole picture.
    fake.send(&update(&[rect(1, 0, 2, 1, 0, &raw(&[R, G]))])).await;
    let frame = next(&mut rx).await;
    assert_eq!((frame.keyframe, frame.seq, frame.width, frame.height, frame.encoding), (true, 1, 4, 2, Encoding::Rgba));
    assert_eq!((frame.region, frame.data.to_vec()), (region(0, 0, 4, 2), flat(&[K, R, G, K, K, K, K, K])));
    fake.expect_request(4, 2).await;

    // CopyRect overlapping its source, after cut text, a bell and a colour map are skipped.
    let skipped = [&[3, 0, 0, 0, 0, 0, 0, 5][..], b"hello", &[2], &[1, 0, 0, 0, 0, 1], &[0; 6]].concat();
    fake.send(&[skipped, update(&[rect(1, 0, 3, 2, 1, &[0, 0, 0, 0])])].concat()).await;
    let frame = next(&mut rx).await;
    assert_eq!((frame.keyframe, frame.region, frame.data.to_vec()), (false, region(1, 0, 3, 2), flat(&[K, R, G, K, K, K])));
    fake.expect_request(4, 2).await;
    surface.request_keyframe().await.unwrap();
    assert_eq!(next(&mut rx).await.data, flat(&[K, K, R, G, K, K, K, K]));

    // A cursor-only update changes no pixels, so no frame; a resize is a keyframe.
    let mut cursor = surface.cursor();
    fake.send(&update(&[rect(0, 0, 1, 1, -239, &[raw(&[W]), vec![0x80]].concat())])).await;
    tokio::time::timeout(LIMIT, cursor.wait_for(Option::is_some)).await.unwrap().unwrap();
    fake.expect_request(4, 2).await;
    fake.send(&update(&[rect(0, 0, 3, 1, -223, &[]), rect(0, 0, 3, 1, 0, &raw(&[G, B, W]))])).await;
    let frame = next(&mut rx).await;
    assert_eq!((frame.keyframe, frame.width, frame.height, frame.data.to_vec()), (true, 3, 1, flat(&[G, B, W])));
    assert_eq!((surface.info().width, surface.info().height), (3, 1));
    fake.expect_request(3, 1).await;

    let png = crate::pixels::decode_png(&surface.screenshot().await.unwrap()).unwrap();
    assert_eq!((png.width, png.height, png.data), (3, 1, flat(&[G, B, W])));
    assert!(matches!(surface.document().await, Err(SurfaceError::Mode { .. })));
}

#[tokio::test]
async fn input_goes_out_as_rfb_messages_and_stamps_the_next_frame() {
    let h = Harness::new(None).await;
    let (surface, mut fake) = h.connect(Security::None, 4, 2).await;
    let mut rx = surface.frames().unwrap();
    let none = Modifiers::default();
    let enter = Input::Key { key: "Enter".into(), code: "Enter".into(), pressed: true, modifiers: none };
    let key = |down, keysym| Msg::Key { down, keysym }.bytes();
    let ptr = |mask, x, y| Msg::Pointer { mask, x, y }.bytes();
    let cases: [(u64, Input, Vec<u8>); 4] = [
        (7, Input::MouseButton { x: 2.0, y: 1.0, button: MouseButton::Left, pressed: true, modifiers: none }, ptr(1, 2, 1)),
        (8, enter, key(true, 0xff0d)),
        (9, Input::Text { text: "Hi".into() }, [key(true, 0x48), key(false, 0x48), key(true, 0x69), key(false, 0x69)].concat()),
        (10, Input::Scroll { x: 3.0, y: 0.0, dx: 0.0, dy: -40.0, modifiers: none }, [ptr(1, 3, 0), ptr(1 | 8, 3, 0), ptr(1, 3, 0)].concat()),
    ];
    for (seq, input, wire) in cases {
        surface.input(InputEvent { seq, sent_us: 0, input }).await.unwrap();
        fake.expect(&wire).await;
        fake.expect_request(4, 2).await;
    }
    // Resize has no RFB message; the next update carries the last input's number.
    surface.input(InputEvent { seq: 11, sent_us: 0, input: Input::Resize { width: 10, height: 10 } }).await.unwrap();
    fake.send(&update(&[rect(0, 0, 1, 1, 0, &[1, 2, 3, 0])])).await;
    assert_eq!(next(&mut rx).await.input_seq, 10);
}

#[tokio::test]
async fn vnc_auth_reconnects_after_a_drop_and_wrong_passwords_fail() {
    let h = Harness::new(Some("hunter2")).await;
    let (surface, fake) = h.connect(Security::Vnc("hunter2"), 2, 1).await;
    let mut rx = surface.frames().unwrap();
    let mut state = surface.state();
    assert_eq!(h.password_calls.load(Ordering::SeqCst), 1);
    drop(fake);
    tokio::time::timeout(LIMIT, state.wait_for(|s| matches!(s, ConnectionState::Reconnecting { .. }))).await.unwrap().unwrap();
    let mut fake = Fake::accept(&h.listener).await;
    assert!(fake.handshake(Security::Vnc("hunter2"), 2, 1).await);
    // The password was asked for again, not kept from the first handshake.
    assert_eq!(h.password_calls.load(Ordering::SeqCst), 2);
    tokio::time::timeout(LIMIT, state.wait_for(|s| *s == ConnectionState::Connected)).await.unwrap().unwrap();
    // The first update after reconnecting is a keyframe again; input goes to the new connection.
    fake.send(&update(&[rect(1, 0, 1, 1, 0, &[0, 0, 255, 0])])).await;
    let frame = next(&mut rx).await;
    assert_eq!((frame.keyframe, frame.data.to_vec()), (true, flat(&[K, B])));
    fake.expect_request(2, 1).await;
    surface.input(InputEvent { seq: 1, sent_us: 0, input: Input::MouseMove { x: 1.0, y: 0.0 } }).await.unwrap();
    fake.expect(&Msg::Pointer { mask: 0, x: 1, y: 0 }.bytes()).await;

    let h = Harness::new(Some("wrong")).await;
    let open = h.open().await;
    assert!(!Fake::accept(&h.listener).await.handshake(Security::Vnc("right"), 2, 1).await);
    let err = open.await.unwrap().err().expect("open fails");
    assert!(err.to_string().contains("bad password"), "{err}");
}

#[tokio::test]
async fn an_rfb_3_3_server_with_no_auth_and_no_desktop_name() {
    let h = Harness::new(None).await;
    let open = h.open().await;
    let mut fake = Fake::accept(&h.listener).await;
    fake.send(b"RFB 003.003\n").await;
    fake.expect(b"RFB 003.003\n").await;
    fake.send(&1u32.to_be_bytes()).await;
    fake.expect(&[1]).await;
    fake.server_init(1, 1, b"").await;
    assert_eq!(open.await.unwrap().unwrap().info().name, "desk");
}

#[tokio::test]
async fn endpoints_are_listed_opened_once_and_removed() {
    let h = Harness::new(None).await;
    let listed = h.server.list().await.unwrap();
    assert_eq!((listed.len(), listed[0].uri.as_str(), listed[0].width), (1, "surface://vm/desk", 0));
    for missing in [SurfaceUri::new("vm", "nope").unwrap(), SurfaceUri::new("browser", "desk").unwrap()] {
        assert!(matches!(h.server.open(&missing).await, Err(SurfaceError::NotFound(_))));
    }
    let (surface, _fake) = h.connect(Security::None, 4, 2).await;
    // A second open reuses the live connection instead of dialing again.
    assert!(Arc::ptr_eq(&surface, &h.server.open_rfb(&uri()).await.unwrap()));
    let listed = h.server.list().await.unwrap();
    assert_eq!((listed[0].width, listed[0].height, listed[0].name.as_str()), (4, 2, "fake desk"));
    let other = RfbEndpoint { name: "other".into(), address: "127.0.0.1:1".into() };
    assert_eq!(h.server.add_endpoint(other).unwrap().as_str(), "surface://vm/other");
    assert_eq!(h.server.list().await.unwrap().len(), 2);
    assert!(h.server.remove_endpoint("desk") && !h.server.remove_endpoint("desk"));
    assert!(matches!(h.server.open(&uri()).await, Err(SurfaceError::NotFound(_))));
}
