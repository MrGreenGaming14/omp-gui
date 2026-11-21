//! Codec tests against the testkit reference encoder, and the client against a fake RFB server on loopback.

use crate::pixels::RgbaImage;
use crate::testkit::rfb::{self, FakeRfb, Zrle, rect, update};
use crate::testkit::next_frame;
use crate::vnc::decode::{Decoders, Update};
use crate::vnc::proto::Msg;
use crate::vnc::{RfbEndpoint, RfbOptions, RfbServer, RfbSurface};
use flate2::{Compress, Compression};
use loa_contracts::surface::{Encoding, Input, InputEvent, Modifiers, MouseButton, Rect, Surface, SurfaceError, SurfaceUri};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const R: [u8; 4] = [255, 0, 0, 255];
const G: [u8; 4] = [0, 255, 0, 255];
const B: [u8; 4] = [0, 0, 255, 255];
const W: [u8; 4] = [255, 255, 255, 255];
const K: [u8; 4] = [0, 0, 0, 255];

fn picture(width: u32, height: u32, px: impl Fn(u32, u32) -> [u8; 4]) -> RgbaImage {
    let px = &px;
    RgbaImage { width, height, data: (0..height).flat_map(|y| (0..width).flat_map(move |x| px(x, y))).collect() }
}

/// One row of pixels as a raw rectangle payload.
fn raw(px: &[[u8; 4]]) -> Vec<u8> {
    rfb::raw(&RgbaImage { width: px.len() as u32, height: 1, data: px.concat() })
}

fn region(x: u32, y: u32, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
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
        let mut cases = vec![(0, rfb::raw(img)), (2, rfb::rre(img)), (5, rfb::hextile(img))];
        for mode in [Zrle::Raw, Zrle::Packed, Zrle::PlainRle, Zrle::PaletteRle] {
            cases.push((16, rfb::zrle(img, mode, &mut z)));
        }
        for (encoding, payload) in cases {
            let (decoder, scratch) = decoders.get(encoding).unwrap();
            let mut wire = payload.as_slice();
            assert_eq!(decoder.decode(img.full(), &mut wire, scratch).await.unwrap(), Update::Blit);
            assert!(wire.is_empty() && scratch.data == img.data, "picture {n}, encoding {encoding}");
        }
    }
    // Cursor rows have independently byte-padded, most-significant-bit-first masks.
    let (cursor, scratch) = decoders.get(-239).unwrap();
    let wire = [rfb::raw(&picture(9, 2, |_, _| W)), vec![0x80, 0x80, 0x40, 0]].concat();
    let mut rest = wire.as_slice();
    let Update::Cursor(shape) = cursor.decode(region(8, 1, 9, 2), &mut rest, scratch).await.unwrap() else {
        panic!("expected cursor shape");
    };
    assert!(rest.is_empty());
    assert_eq!((shape.hotspot_x, shape.hotspot_y, shape.width, shape.height), (8, 1, 9, 2));
    for (i, px) in shape.rgba.as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(px, &[255, 255, 255, if [0, 8, 10].contains(&i) { 255 } else { 0 }]);
    }
    // A subrectangle outside its rectangle is a protocol error.
    let bad = [&1u32.to_be_bytes()[..], &rfb::pixel(&R), &rfb::pixel(&G), &[0, 1, 0, 0, 0, 1, 0, 1]].concat();
    let (rre, scratch) = decoders.get(2).unwrap();
    assert!(rre.decode(region(0, 0, 1, 1), &mut bad.as_slice(), scratch).await.is_err());
}

struct Harness {
    listener: TcpListener,
    server: Arc<RfbServer>,
    password_calls: Arc<AtomicUsize>,
}

impl Harness {
    /// An [`RfbServer`] with the one endpoint `desk` on a loopback listener.
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

    fn open(&self) -> JoinHandle<Result<Arc<RfbSurface>, SurfaceError>> {
        let server = Arc::clone(&self.server);
        tokio::spawn(async move { server.open_rfb(&uri()).await })
    }

    /// Opens `surface://vm/desk` against a fake that completes the handshake.
    async fn connect(&self, password: Option<&str>, width: u16, height: u16) -> (Arc<RfbSurface>, FakeRfb) {
        let open = self.open();
        let mut fake = FakeRfb::accept(&self.listener).await;
        assert!(fake.handshake(password, width, height).await);
        (open.await.unwrap().unwrap(), fake)
    }
}

fn uri() -> SurfaceUri {
    SurfaceUri::new("vm", "desk").unwrap()
}

#[tokio::test]
async fn updates_become_frames_and_keyframes() {
    let h = Harness::new(None).await;
    let (surface, mut fake) = h.connect(None, 4, 2).await;
    let info = surface.info();
    assert_eq!((info.name.as_str(), info.width, info.height), ("fake desk", 4, 2));
    let mut rx = surface.frames().unwrap();

    // Only part of the screen arrives, yet the first frame is the whole picture.
    fake.send(&update(&[rect(1, 0, 2, 1, 0, &raw(&[R, G]))])).await;
    let frame = next_frame(&mut rx).await;
    assert_eq!((frame.keyframe, frame.seq, frame.width, frame.height, frame.encoding), (true, 1, 4, 2, Encoding::Rgba));
    assert_eq!((frame.region, frame.data.to_vec()), (region(0, 0, 4, 2), [K, R, G, K, K, K, K, K].concat()));
    fake.expect_request(4, 2).await;

    // CopyRect overlapping its source, after cut text, a bell and a colour map are skipped.
    let skipped = [&[3, 0, 0, 0, 0, 0, 0, 5][..], b"hello", &[2], &[1, 0, 0, 0, 0, 1], &[0; 6]].concat();
    fake.send(&[skipped, update(&[rect(1, 0, 3, 2, 1, &[0, 0, 0, 0])])].concat()).await;
    let frame = next_frame(&mut rx).await;
    assert_eq!((frame.keyframe, frame.region, frame.data.to_vec()), (false, region(1, 0, 3, 2), [K, R, G, K, K, K].concat()));
    fake.expect_request(4, 2).await;
    surface.request_keyframe().await.unwrap();
    assert_eq!(next_frame(&mut rx).await.data, [K, K, R, G, K, K, K, K].concat());

    // A cursor-only update changes no pixels, so no frame; a resize is a keyframe.
    fake.send(&update(&[rect(0, 0, 1, 1, -239, &[raw(&[W]), vec![0x80]].concat())])).await;
    fake.expect_request(4, 2).await;
    fake.send(&update(&[rect(0, 0, 3, 1, -223, &[]), rect(0, 0, 3, 1, 0, &raw(&[G, B, W]))])).await;
    let frame = next_frame(&mut rx).await;
    assert_eq!((frame.keyframe, frame.width, frame.height, frame.data.to_vec()), (true, 3, 1, [G, B, W].concat()));
    assert_eq!((surface.info().width, surface.info().height), (3, 1));
    fake.expect_request(3, 1).await;

    let png = crate::pixels::decode_png(&surface.screenshot().await.unwrap()).unwrap();
    assert_eq!((png.width, png.height, png.data), (3, 1, [G, B, W].concat()));
    assert!(matches!(surface.document().await, Err(SurfaceError::Mode { .. })));
}

#[tokio::test]
async fn cursor_shapes_are_published_without_changing_the_framebuffer() {
    let h = Harness::new(None).await;
    let (surface, mut fake) = h.connect(None, 4, 2).await;
    let mut frames = surface.frames().unwrap();
    let mut cursors = surface.cursor();
    assert!(cursors.borrow().is_none());
    let payload = [raw(&[R, G, B, W]), vec![0x80, 0x40]].concat();
    fake.send(&update(&[
        rect(8, 1, 2, 2, -239, &payload),
        rect(0, 0, 1, 1, 0, &raw(&[G])),
    ])).await;
    let frame = next_frame(&mut frames).await;
    assert_eq!(frame.data.to_vec(), [G, K, K, K, K, K, K, K].concat());
    tokio::time::timeout(Duration::from_secs(10), cursors.wait_for(Option::is_some)).await.unwrap().unwrap();
    let cursor = cursors.borrow_and_update().clone().unwrap();
    assert_eq!((cursor.hotspot_x, cursor.hotspot_y, cursor.width, cursor.height), (8, 1, 2, 2));
    assert_eq!(cursor.rgba, [R, [0, 255, 0, 0], [0, 0, 255, 0], W].concat());
    fake.expect_request(4, 2).await;
    fake.send(&update(&[rect(0, 0, 0, 0, -239, &[])])).await;
    tokio::time::timeout(Duration::from_secs(10), cursors.changed()).await.unwrap().unwrap();
    let cursor = cursors.borrow().clone().unwrap();
    assert_eq!((cursor.width, cursor.height), (0, 0));
    assert!(cursor.rgba.is_empty());
}

#[tokio::test]
async fn input_goes_out_as_rfb_messages_and_stamps_the_next_frame() {
    let h = Harness::new(None).await;
    let (surface, mut fake) = h.connect(None, 4, 2).await;
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
    assert_eq!(next_frame(&mut rx).await.input_seq, 10);
}

#[tokio::test]
async fn vnc_auth_reconnects_after_a_drop_and_wrong_passwords_fail() {
    let h = Harness::new(Some("hunter2")).await;
    let (surface, fake) = h.connect(Some("hunter2"), 2, 1).await;
    let mut rx = surface.frames().unwrap();
    assert_eq!(h.password_calls.load(Ordering::SeqCst), 1);
    drop(fake);
    let mut fake = FakeRfb::accept(&h.listener).await;
    assert!(fake.handshake(Some("hunter2"), 2, 1).await);
    // The password was asked for again, not kept from the first handshake.
    assert_eq!(h.password_calls.load(Ordering::SeqCst), 2);
    // The first update after reconnecting is a keyframe again; input goes to the new connection.
    fake.send(&update(&[rect(1, 0, 1, 1, 0, &[0, 0, 255, 0])])).await;
    let frame = next_frame(&mut rx).await;
    assert_eq!((frame.keyframe, frame.data.to_vec()), (true, [K, B].concat()));
    fake.expect_request(2, 1).await;
    surface.input(InputEvent { seq: 1, sent_us: 0, input: Input::MouseMove { x: 1.0, y: 0.0 } }).await.unwrap();
    fake.expect(&Msg::Pointer { mask: 0, x: 1, y: 0 }.bytes()).await;

    let h = Harness::new(Some("wrong")).await;
    let open = h.open();
    assert!(!FakeRfb::accept(&h.listener).await.handshake(Some("right"), 2, 1).await);
    let err = open.await.unwrap().err().expect("open fails");
    assert!(err.to_string().contains("bad password"), "{err}");

    // An RFB 3.3 server with no auth and no desktop name: the endpoint names the surface.
    let h = Harness::new(None).await;
    let open = h.open();
    let mut fake = FakeRfb::accept(&h.listener).await;
    fake.send(b"RFB 003.003\n").await;
    fake.expect(b"RFB 003.003\n").await;
    fake.send(&1u32.to_be_bytes()).await;
    fake.expect(&[1]).await;
    fake.server_init(1, 1, b"").await;
    assert_eq!(open.await.unwrap().unwrap().info().name, "desk");
}
