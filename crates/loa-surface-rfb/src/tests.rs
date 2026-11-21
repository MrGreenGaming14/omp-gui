//! End to end tests against an in-process fake RFB server on loopback.

use crate::{ConnectionState, RfbEndpoint, RfbOptions, RfbServer, RfbSurface, des};
use flate2::{Compress, Compression, FlushCompress};
use loa_contracts::surface::{
    Encoding, Frame, Input, InputEvent, Modifiers, MouseButton, Rect, Surface, SurfaceError,
    SurfaceServer, SurfaceUri,
};
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

enum Security {
    None,
    Vnc(&'static str),
}

/// The server side of one connection.
struct Fake {
    stream: TcpStream,
}

impl Fake {
    async fn accept(listener: &TcpListener) -> Self {
        let (stream, _) = tokio::time::timeout(LIMIT, listener.accept())
            .await
            .unwrap()
            .unwrap();
        Self { stream }
    }

    async fn send(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.unwrap();
    }

    async fn read(&mut self, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        tokio::time::timeout(LIMIT, self.stream.read_exact(&mut buf))
            .await
            .unwrap()
            .unwrap();
        buf
    }

    async fn expect(&mut self, want: &[u8]) {
        assert_eq!(self.read(want.len()).await, want);
    }

    /// RFB 3.8 up to ServerInit. Returns false when the client's VNC
    /// response was wrong (the failure has been sent).
    async fn handshake(&mut self, security: Security, width: u16, height: u16) -> bool {
        self.send(b"RFB 003.008\n").await;
        self.expect(b"RFB 003.008\n").await;
        match security {
            Security::None => {
                self.send(&[1, 1]).await;
                self.expect(&[1]).await;
                self.send(&0u32.to_be_bytes()).await;
            }
            Security::Vnc(password) => {
                self.send(&[2, 16, 2]).await;
                self.expect(&[2]).await;
                let challenge: [u8; 16] = std::array::from_fn(|i| (i as u8).wrapping_mul(37));
                self.send(&challenge).await;
                let response = self.read(16).await;
                if response != des::vnc_response(password.as_bytes(), &challenge) {
                    let reason = b"bad password";
                    self.send(&1u32.to_be_bytes()).await;
                    self.send(&(reason.len() as u32).to_be_bytes()).await;
                    self.send(reason).await;
                    return false;
                }
                self.send(&0u32.to_be_bytes()).await;
            }
        }
        self.expect(&[1]).await; // ClientInit, shared
        let mut init = Vec::new();
        init.extend_from_slice(&width.to_be_bytes());
        init.extend_from_slice(&height.to_be_bytes());
        // Server native format: 32 bpp big endian, blue in the low byte.
        init.extend_from_slice(&[32, 24, 1, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
        let name = b"fake desk";
        init.extend_from_slice(&(name.len() as u32).to_be_bytes());
        init.extend_from_slice(name);
        self.send(&init).await;
        true
    }

    /// SetPixelFormat, SetEncodings and the full update request.
    async fn expect_setup(&mut self, width: u16, height: u16) {
        self.expect(&[
            0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 0, 8, 16, 0, 0, 0,
        ])
        .await;
        let mut enc = vec![2, 0, 0, 7];
        for e in [1i32, 16, 5, 2, 0, -239, -223] {
            enc.extend_from_slice(&e.to_be_bytes());
        }
        self.expect(&enc).await;
        self.expect(&request(false, width, height)).await;
    }
}

fn request(incremental: bool, width: u16, height: u16) -> Vec<u8> {
    let mut v = vec![3, u8::from(incremental), 0, 0, 0, 0];
    v.extend_from_slice(&width.to_be_bytes());
    v.extend_from_slice(&height.to_be_bytes());
    v
}

fn rect(x: u16, y: u16, w: u16, h: u16, encoding: i32, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    for f in [x, y, w, h] {
        v.extend_from_slice(&f.to_be_bytes());
    }
    v.extend_from_slice(&encoding.to_be_bytes());
    v.extend_from_slice(payload);
    v
}

fn update(rects: &[Vec<u8>]) -> Vec<u8> {
    let mut v = vec![0, 0];
    v.extend_from_slice(&(rects.len() as u16).to_be_bytes());
    for r in rects {
        v.extend_from_slice(r);
    }
    v
}

/// A pixel in the client's requested format, with a junk padding byte.
fn wire(px: [u8; 4]) -> [u8; 4] {
    [px[0], px[1], px[2], 0x77]
}

fn raw(pixels: &[[u8; 4]]) -> Vec<u8> {
    pixels.iter().flat_map(|&p| wire(p)).collect()
}

fn cpixel(px: [u8; 4]) -> [u8; 3] {
    [px[0], px[1], px[2]]
}

/// A ZRLE payload: the tiles through the connection's zlib stream.
fn zrle(z: &mut Compress, tiles: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tiles.len() * 2 + 64);
    z.compress_vec(tiles, &mut out, FlushCompress::Sync)
        .unwrap();
    assert!(
        out.len() < out.capacity(),
        "compressed output was cut short"
    );
    let mut v = (out.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(&out);
    v
}

fn flat(pixels: &[[u8; 4]]) -> Vec<u8> {
    pixels.concat()
}

async fn next(rx: &mut broadcast::Receiver<Arc<Frame>>) -> Arc<Frame> {
    tokio::time::timeout(LIMIT, rx.recv())
        .await
        .unwrap()
        .unwrap()
}

struct Harness {
    listener: TcpListener,
    server: Arc<RfbServer>,
    password_calls: Arc<AtomicUsize>,
}

fn uri() -> SurfaceUri {
    SurfaceUri::new("vm", "desk").unwrap()
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
        let options = RfbOptions {
            backoff_initial: Duration::from_millis(1),
            backoff_max: Duration::from_millis(20),
            ..RfbOptions::default()
        };
        let endpoints = vec![RfbEndpoint {
            name: "desk".into(),
            address,
        }];
        let server = Arc::new(RfbServer::new(endpoints, Arc::new(passwords), options));
        Self {
            listener,
            server,
            password_calls,
        }
    }

    /// Opens `surface://vm/desk` against a fake that completes the handshake.
    async fn connect(
        &self,
        security: Security,
        width: u16,
        height: u16,
    ) -> (Arc<RfbSurface>, Fake) {
        let server = Arc::clone(&self.server);
        let open = tokio::spawn(async move { server.open_rfb(&uri()).await });
        let mut fake = Fake::accept(&self.listener).await;
        assert!(fake.handshake(security, width, height).await);
        fake.expect_setup(width, height).await;
        (open.await.unwrap().unwrap(), fake)
    }
}

/// A 4x2 surface showing `FIRST` after its first (raw) update.
const FIRST: [[u8; 4]; 8] = [R, G, B, W, K, R, G, B];

async fn connected_4x2() -> (
    Harness,
    Arc<RfbSurface>,
    Fake,
    broadcast::Receiver<Arc<Frame>>,
) {
    let h = Harness::new(None).await;
    let (surface, mut fake) = h.connect(Security::None, 4, 2).await;
    let mut rx = surface.frames().unwrap();
    fake.send(&update(&[rect(0, 0, 4, 2, 0, &raw(&FIRST))]))
        .await;
    let frame = next(&mut rx).await;
    assert_eq!(frame.data, flat(&FIRST));
    fake.expect(&request(true, 4, 2)).await;
    (h, surface, fake, rx)
}

#[tokio::test]
async fn first_raw_update_is_a_full_keyframe() {
    let h = Harness::new(None).await;
    let (surface, mut fake) = h.connect(Security::None, 4, 2).await;
    let info = surface.info();
    assert_eq!(
        (info.name.as_str(), info.width, info.height),
        ("fake desk", 4, 2)
    );
    let mut rx = surface.frames().unwrap();
    // Only part of the screen arrives, yet the first frame is the whole picture.
    fake.send(&update(&[rect(1, 0, 2, 1, 0, &raw(&[R, G]))]))
        .await;
    let frame = next(&mut rx).await;
    assert!(frame.keyframe);
    assert_eq!(
        (frame.seq, frame.width, frame.height, frame.encoding),
        (1, 4, 2, Encoding::Rgba)
    );
    assert_eq!(
        frame.region,
        Rect {
            x: 0,
            y: 0,
            width: 4,
            height: 2
        }
    );
    assert_eq!(frame.data, flat(&[K, R, G, K, K, K, K, K]));
    fake.expect(&request(true, 4, 2)).await;
}

#[tokio::test]
async fn raw_then_copy_rect_with_overlap() {
    let (_h, _surface, mut fake, mut rx) = connected_4x2().await;
    // Shift the right three columns one pixel right, source overlapping target.
    fake.send(&update(&[rect(1, 0, 3, 2, 1, &[0, 0, 0, 0])]))
        .await;
    let frame = next(&mut rx).await;
    assert!(!frame.keyframe);
    assert_eq!(frame.seq, 2);
    assert_eq!(
        frame.region,
        Rect {
            x: 1,
            y: 0,
            width: 3,
            height: 2
        }
    );
    assert_eq!(frame.data, flat(&[R, G, B, K, R, G]));
}

#[tokio::test]
async fn rre_background_and_subrects() {
    let (_h, surface, mut fake, mut rx) = connected_4x2().await;
    let mut payload = 2u32.to_be_bytes().to_vec();
    payload.extend_from_slice(&wire(W));
    for (px, [x, y, w, h]) in [(B, [0u16, 0, 1, 2]), (G, [2, 1, 1, 1])] {
        payload.extend_from_slice(&wire(px));
        for f in [x, y, w, h] {
            payload.extend_from_slice(&f.to_be_bytes());
        }
    }
    fake.send(&update(&[rect(1, 0, 3, 2, 2, &payload)])).await;
    let frame = next(&mut rx).await;
    assert_eq!(frame.data, flat(&[B, W, W, B, W, G]));
    surface.request_keyframe().await.unwrap();
    let key = next(&mut rx).await;
    assert!(key.keyframe);
    assert_eq!(key.data, flat(&[R, B, W, W, K, B, W, G]));
}

#[tokio::test]
async fn hextile_all_subencodings() {
    let h = Harness::new(None).await;
    let (surface, mut fake) = h.connect(Security::None, 18, 17).await;
    let mut rx = surface.frames().unwrap();
    let gradient = |x: u32, y: u32| [x as u8 * 10, y as u8 * 10, 7, 255];

    let mut p = Vec::new();
    // Tile (0,0) 16x16: raw.
    p.push(1);
    for y in 0..16 {
        for x in 0..16 {
            p.extend_from_slice(&wire(gradient(x, y)));
        }
    }
    // Tile (16,0) 2x16: background red, foreground green, one subrect.
    p.push(2 | 4 | 8);
    p.extend_from_slice(&wire(R));
    p.extend_from_slice(&wire(G));
    p.extend_from_slice(&[1, 0x12, 0x02]); // at (1,2), 1x3
    // Tile (0,16) 16x1: background and foreground carried over.
    p.push(8);
    p.extend_from_slice(&[1, 0x30, 0x10]); // at (3,0), 2x1
    // Tile (16,16) 2x1: coloured subrect.
    p.push(8 | 16);
    p.push(1);
    p.extend_from_slice(&wire(B));
    p.extend_from_slice(&[0x00, 0x10]); // at (0,0), 2x1
    fake.send(&update(&[rect(0, 0, 18, 17, 5, &p)])).await;

    let frame = next(&mut rx).await;
    let want = |x: u32, y: u32| match (x, y) {
        (0..16, 0..16) => gradient(x, y),
        (17, 2..5) => G,
        (16.., 0..16) => R,
        (3 | 4, 16) => G,
        (0..16, 16) => R,
        _ => B,
    };
    let expected: Vec<u8> = (0..17)
        .flat_map(|y| (0..18).flat_map(move |x| want(x, y)))
        .collect();
    assert_eq!(frame.data, expected);
}

#[tokio::test]
async fn zrle_tiles_across_one_zlib_stream() {
    let h = Harness::new(None).await;
    let (surface, mut fake) = h.connect(Security::None, 70, 5).await;
    let mut rx = surface.frames().unwrap();
    let mut z = Compress::new(Compression::default(), true);

    // Update 1: a 64x5 plain RLE tile (runs of 300 and 20) and a 6x5 packed
    // palette tile with 1 bit indices.
    let mut t = vec![128];
    t.extend_from_slice(&cpixel(R));
    t.extend_from_slice(&[255, 44]); // 1 + 255 + 44 = 300
    t.extend_from_slice(&cpixel(G));
    t.push(19);
    t.push(2);
    t.extend_from_slice(&cpixel(B));
    t.extend_from_slice(&cpixel(W));
    t.extend_from_slice(&[0b1010_1000, 0b0101_0100, 0b1111_1100, 0, 0b1000_0100]);
    fake.send(&update(&[rect(0, 0, 70, 5, 16, &zrle(&mut z, &t))]))
        .await;
    let frame = next(&mut rx).await;
    let mut want = Vec::new();
    for y in 0..5usize {
        for x in 0..64usize {
            want.push(if y * 64 + x < 300 { R } else { G });
        }
        let bits = [0b101010, 0b010101, 0b111111, 0, 0b100001][y];
        for x in 0..6 {
            want.push(if bits & (0b100000 >> x) != 0 { W } else { B });
        }
    }
    assert_eq!(frame.data, flat(&want));
    fake.expect(&request(true, 70, 5)).await;

    // Update 2, same zlib stream: solid, raw, palette RLE, 2 and 4 bit packed palettes.
    let solid = [&[1u8][..], &cpixel(B)].concat();
    let raw_tile = [&[0u8][..], &cpixel(R), &cpixel(G), &cpixel(W), &cpixel(K)].concat();
    let palette_rle = [&[130u8][..], &cpixel(R), &cpixel(G), &[0x80, 2, 1, 0x81, 1]].concat();
    let packed4 = [
        &[5u8][..],
        &cpixel(R),
        &cpixel(G),
        &cpixel(B),
        &cpixel(W),
        &cpixel(K),
        &[0x01, 0x23, 0x40],
    ]
    .concat();
    let packed2 = [
        &[3u8][..],
        &cpixel(R),
        &cpixel(G),
        &cpixel(B),
        &[0b1001_0000],
    ]
    .concat();
    fake.send(&update(&[
        rect(0, 0, 2, 2, 16, &zrle(&mut z, &solid)),
        rect(2, 0, 2, 2, 16, &zrle(&mut z, &raw_tile)),
        rect(4, 0, 3, 2, 16, &zrle(&mut z, &palette_rle)),
        rect(7, 0, 5, 1, 16, &zrle(&mut z, &packed4)),
        rect(0, 2, 3, 1, 16, &zrle(&mut z, &packed2)),
    ]))
    .await;
    let frame = next(&mut rx).await;
    assert_eq!(
        frame.region,
        Rect {
            x: 0,
            y: 0,
            width: 12,
            height: 3
        }
    );
    let rows = [
        [B, B, R, G, R, R, R, R, G, B, W, K],
        [B, B, W, K, G, G, G, R, R, R, R, R],
        [B, G, R, R, R, R, R, R, R, R, R, R],
    ];
    assert_eq!(frame.data, flat(&rows.concat()));
}

#[tokio::test]
async fn desktop_size_resizes_and_publishes_a_keyframe() {
    let (_h, surface, mut fake, mut rx) = connected_4x2().await;
    fake.send(&update(&[
        rect(0, 0, 3, 1, -223, &[]),
        rect(0, 0, 3, 1, 0, &raw(&[G, B, W])),
    ]))
    .await;
    let frame = next(&mut rx).await;
    assert!(frame.keyframe);
    assert_eq!((frame.width, frame.height), (3, 1));
    assert_eq!(frame.data, flat(&[G, B, W]));
    let info = surface.info();
    assert_eq!((info.width, info.height), (3, 1));
    // Later requests cover the new size.
    fake.expect(&request(true, 3, 1)).await;
}

#[tokio::test]
async fn cursor_shape_uses_the_mask_as_alpha() {
    let (_h, surface, mut fake, mut rx) = connected_4x2().await;
    let mut cursor_rx = surface.cursor();
    let mut payload = raw(&[R, G, B, W]);
    payload.extend_from_slice(&[0b1000_0000, 0b0100_0000]);
    fake.send(&update(&[rect(1, 0, 2, 2, -239, &payload)]))
        .await;
    tokio::time::timeout(LIMIT, cursor_rx.wait_for(Option::is_some))
        .await
        .unwrap()
        .unwrap();
    let cursor = cursor_rx.borrow().clone().unwrap();
    assert_eq!(
        (
            cursor.hotspot_x,
            cursor.hotspot_y,
            cursor.width,
            cursor.height
        ),
        (1, 0, 2, 2)
    );
    assert_eq!(cursor.rgba, flat(&[R, [0, 255, 0, 0], [0, 0, 255, 0], W]));
    // A cursor-only update changes no pixels, so no frame; the next one does.
    fake.expect(&request(true, 4, 2)).await;
    fake.send(&update(&[rect(0, 0, 1, 1, 0, &raw(&[W]))])).await;
    assert_eq!(next(&mut rx).await.seq, 2);
}

#[tokio::test]
async fn cut_text_bell_and_colour_map_are_skipped() {
    let (_h, _surface, mut fake, mut rx) = connected_4x2().await;
    let mut msgs = vec![3, 0, 0, 0, 0, 0, 0, 5];
    msgs.extend_from_slice(b"hello");
    msgs.push(2);
    msgs.extend_from_slice(&[1, 0, 0, 0, 0, 1]);
    msgs.extend_from_slice(&[0; 6]);
    msgs.extend(update(&[rect(3, 1, 1, 1, 0, &raw(&[G]))]));
    fake.send(&msgs).await;
    let frame = next(&mut rx).await;
    assert_eq!(
        frame.region,
        Rect {
            x: 3,
            y: 1,
            width: 1,
            height: 1
        }
    );
    assert_eq!(frame.data, flat(&[G]));
}

#[tokio::test]
async fn input_bytes_on_the_wire_and_input_seq_in_frames() {
    let (_h, surface, mut fake, mut rx) = connected_4x2().await;
    let none = Modifiers::default();
    let event = |seq, input| InputEvent {
        seq,
        sent_us: 0,
        input,
    };

    surface
        .input(event(
            7,
            Input::MouseButton {
                x: 2.0,
                y: 1.0,
                button: MouseButton::Left,
                pressed: true,
                modifiers: none,
            },
        ))
        .await
        .unwrap();
    fake.expect(&[5, 1, 0, 2, 0, 1]).await;
    fake.expect(&request(true, 4, 2)).await;

    surface
        .input(event(
            8,
            Input::Key {
                key: "Enter".into(),
                code: "Enter".into(),
                pressed: true,
                modifiers: none,
            },
        ))
        .await
        .unwrap();
    fake.expect(&[4, 1, 0, 0, 0, 0, 0xff, 0x0d]).await;
    fake.expect(&request(true, 4, 2)).await;

    fake.send(&update(&[rect(0, 0, 1, 1, 0, &raw(&[B]))])).await;
    assert_eq!(next(&mut rx).await.input_seq, 8);
    fake.expect(&request(true, 4, 2)).await;

    surface
        .input(event(9, Input::Text { text: "Hi".into() }))
        .await
        .unwrap();
    fake.expect(&[4, 1, 0, 0, 0, 0, 0, 0x48, 4, 0, 0, 0, 0, 0, 0, 0x48])
        .await;
    fake.expect(&[4, 1, 0, 0, 0, 0, 0, 0x69, 4, 0, 0, 0, 0, 0, 0, 0x69])
        .await;
    fake.expect(&request(true, 4, 2)).await;

    surface
        .input(event(
            10,
            Input::Scroll {
                x: 3.0,
                y: 0.0,
                dx: 0.0,
                dy: -40.0,
                modifiers: none,
            },
        ))
        .await
        .unwrap();
    fake.expect(&[5, 1, 0, 3, 0, 0, 5, 1 | 8, 0, 3, 0, 0, 5, 1, 0, 3, 0, 0])
        .await;
    fake.expect(&request(true, 4, 2)).await;

    // Resize has no RFB message and writes nothing.
    surface
        .input(event(
            11,
            Input::Resize {
                width: 10,
                height: 10,
            },
        ))
        .await
        .unwrap();
    fake.send(&update(&[rect(0, 0, 1, 1, 0, &raw(&[G]))])).await;
    assert_eq!(next(&mut rx).await.input_seq, 10);
}

#[tokio::test]
async fn screenshot_is_a_png_of_the_framebuffer() {
    let (_h, surface, _fake, _rx) = connected_4x2().await;
    let png = surface.screenshot().await.unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(
        (info.width, info.height, info.color_type),
        (4, 2, png::ColorType::Rgba)
    );
    assert_eq!(&buf[..info.buffer_size()], flat(&FIRST));
    assert!(matches!(
        surface.document().await,
        Err(SurfaceError::Mode { .. })
    ));
}

#[tokio::test]
async fn vnc_auth_and_reconnect_after_a_drop() {
    let h = Harness::new(Some("hunter2")).await;
    let (surface, mut fake) = h.connect(Security::Vnc("hunter2"), 2, 1).await;
    let mut rx = surface.frames().unwrap();
    let mut state = surface.state();
    fake.send(&update(&[rect(0, 0, 2, 1, 0, &raw(&[R, G]))]))
        .await;
    assert_eq!(next(&mut rx).await.data, flat(&[R, G]));
    assert_eq!(h.password_calls.load(Ordering::SeqCst), 1);

    drop(fake);
    tokio::time::timeout(
        LIMIT,
        state.wait_for(|s| matches!(s, ConnectionState::Reconnecting { .. })),
    )
    .await
    .unwrap()
    .unwrap();
    let mut fake = Fake::accept(&h.listener).await;
    assert!(fake.handshake(Security::Vnc("hunter2"), 2, 1).await);
    fake.expect_setup(2, 1).await;
    // The password was asked for again, not kept from the first handshake.
    assert_eq!(h.password_calls.load(Ordering::SeqCst), 2);
    tokio::time::timeout(LIMIT, state.wait_for(|s| *s == ConnectionState::Connected))
        .await
        .unwrap()
        .unwrap();

    // The first update after reconnecting is a keyframe again.
    fake.send(&update(&[rect(1, 0, 1, 1, 0, &raw(&[B]))])).await;
    let frame = next(&mut rx).await;
    assert!(frame.keyframe);
    assert_eq!(frame.data, flat(&[R, B]));

    // Input goes to the new connection.
    let move_to = InputEvent {
        seq: 1,
        sent_us: 0,
        input: Input::MouseMove { x: 1.0, y: 0.0 },
    };
    fake.expect(&request(true, 2, 1)).await;
    surface.input(move_to).await.unwrap();
    fake.expect(&[5, 0, 0, 1, 0, 0]).await;
}

#[tokio::test]
async fn wrong_password_fails_the_open() {
    let h = Harness::new(Some("wrong")).await;
    let server = Arc::clone(&h.server);
    let open = tokio::spawn(async move { server.open_rfb(&uri()).await });
    let mut fake = Fake::accept(&h.listener).await;
    assert!(!fake.handshake(Security::Vnc("right"), 2, 1).await);
    let Err(err) = open.await.unwrap() else {
        panic!("open succeeded")
    };
    assert!(err.to_string().contains("bad password"), "{err}");
}

#[tokio::test]
async fn rfb_3_3_server_with_no_auth() {
    let h = Harness::new(None).await;
    let server = Arc::clone(&h.server);
    let open = tokio::spawn(async move { server.open_rfb(&uri()).await });
    let mut fake = Fake::accept(&h.listener).await;
    fake.send(b"RFB 003.003\n").await;
    fake.expect(b"RFB 003.003\n").await;
    fake.send(&1u32.to_be_bytes()).await;
    fake.expect(&[1]).await;
    let mut init = vec![0, 1, 0, 1];
    init.extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
    init.extend_from_slice(&0u32.to_be_bytes());
    fake.send(&init).await;
    fake.expect_setup(1, 1).await;
    let surface = open.await.unwrap().unwrap();
    // No desktop name: the tab shows the endpoint name.
    assert_eq!(surface.info().name, "desk");
}

#[tokio::test]
async fn server_lists_and_opens_endpoints() {
    let h = Harness::new(None).await;
    let listed = h.server.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (listed[0].uri.as_str(), listed[0].width),
        ("surface://vm/desk", 0)
    );

    let missing = SurfaceUri::new("vm", "nope").unwrap();
    assert!(matches!(
        h.server.open(&missing).await,
        Err(SurfaceError::NotFound(_))
    ));
    let other = SurfaceUri::new("browser", "desk").unwrap();
    assert!(matches!(
        h.server.open(&other).await,
        Err(SurfaceError::NotFound(_))
    ));

    let (surface, _fake) = h.connect(Security::None, 4, 2).await;
    // A second open reuses the live connection instead of dialing again.
    let again = h.server.open_rfb(&uri()).await.unwrap();
    assert!(Arc::ptr_eq(&surface, &again));
    let listed = h.server.list().await.unwrap();
    assert_eq!(
        (listed[0].width, listed[0].height, listed[0].name.as_str()),
        (4, 2, "fake desk")
    );
}

#[tokio::test]
async fn endpoints_added_at_runtime_are_listed_opened_and_removed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let server = Arc::new(RfbServer::new(
        Vec::new(),
        Arc::new(|_: &str| -> Option<String> { None }),
        RfbOptions::default(),
    ));
    assert!(server.list().await.unwrap().is_empty());
    let added = server
        .add_endpoint(RfbEndpoint {
            name: "desk".into(),
            address,
        })
        .unwrap();
    assert_eq!(added, uri());
    assert_eq!(server.list().await.unwrap().len(), 1);

    let opener = Arc::clone(&server);
    let open = tokio::spawn(async move { opener.open_rfb(&uri()).await });
    let mut fake = Fake::accept(&listener).await;
    assert!(fake.handshake(Security::None, 4, 2).await);
    fake.expect_setup(4, 2).await;
    let surface = open.await.unwrap().unwrap();
    assert_eq!((surface.info().width, surface.info().height), (4, 2));

    assert!(server.remove_endpoint("desk"));
    assert!(!server.remove_endpoint("desk"));
    assert!(server.list().await.unwrap().is_empty());
    assert!(matches!(
        server.open(&uri()).await,
        Err(SurfaceError::NotFound(_))
    ));
}
