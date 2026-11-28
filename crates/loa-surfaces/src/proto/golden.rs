//! Golden files for the C++ port, written by `cargo test -p loa-surfaces
//! --features remote-video --lib write_golden -- --ignored` into
//! `$LOA_GOLDEN_OUT/surface`. Each `stream-*.bin` is one length delimited
//! [`StreamMessage`]; `datagram-fragment.bin` is one encoded [`Datagram`].

use super::*;
use prost::Message;
use std::fs;
use std::path::PathBuf;

#[test]
#[ignore = "writes golden files; run with LOA_GOLDEN_OUT set"]
fn write_golden_surface_wire() {
    let dir = PathBuf::from(std::env::var("LOA_GOLDEN_OUT").expect("LOA_GOLDEN_OUT")).join("surface");
    fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, bytes: Vec<u8>| fs::write(dir.join(name), bytes).unwrap();
    let stream = |body: Body| StreamMessage::from(body).encode_length_delimited_to_vec();
    assert_eq!(VERSION, 1);

    let info = SurfaceInfoMsg {
        uri: "x11://:0/0x1".into(),
        name: "Terminal ✓".into(),
        width: 1920,
        height: 1080,
        mode: WireMode::Pixel as i32,
        mime: None,
    };
    let header = FrameHeader {
        seq: 7,
        captured_us: 1_700_000_000_000_000,
        input_seq: 3,
        width: 1920,
        height: 1080,
        region_x: 10,
        region_y: 20,
        region_width: 300,
        region_height: 200,
        encoding: WireEncoding::H264 as i32,
        keyframe: true,
    };
    let bodies = [
        ("open", Body::Open(Open { version: VERSION, uri: "x11://:0/0x1".into(), encodings: vec![WireEncoding::H264 as i32, WireEncoding::Jpeg as i32, WireEncoding::Rgba as i32] })),
        ("accept", Body::Accept(Accept {
            version: VERSION,
            info: Some(info.clone()),
            datagrams: true,
            stream: Some(StreamInfoMsg { encoding: WireEncoding::H264 as i32, encoder: "openh264".into(), hardware: false }),
        })),
        ("accept-document", Body::Accept(Accept {
            version: VERSION,
            info: Some(SurfaceInfoMsg { mode: WireMode::Document as i32, mime: Some("application/pdf".into()), ..info }),
            datagrams: false,
            stream: None,
        })),
        ("reject", Body::Reject(Reject { code: RejectCode::NotFound as i32, reason: "no such surface".into() })),
        ("resize", Body::Resize(Resize { width: 1280, height: 720 })),
        ("close", Body::Close(Close { reason: "bye".into() })),
        ("keyframe-request", Body::KeyframeRequest(KeyframeRequest {})),
        ("frame", Body::Frame(FrameMsg { header: Some(header.clone()), data: Bytes::from_static(&[0, 0, 0, 1, 0x67, 0x42]) })),
        ("document-header", Body::DocumentHeader(DocumentHeader { name: "a.pdf".into(), mime: "application/pdf".into(), length: 5, crc32: 0x1234_5678 })),
        ("document-chunk", Body::DocumentChunk(DocumentChunk { offset: 0, data: Bytes::from_static(b"%PDF-") })),
    ];
    for (name, body) in bodies {
        write(&format!("stream-{name}.bin"), stream(body));
    }

    // InputKind tags 3 to 8; the message structs the issue names are the payloads here.
    let mods = 0b1011; // shift, alt, meta
    let inputs = [
        ("mouse-move", InputKind::MouseMove(MouseMoveMsg { x: 1.5, y: 2.25 })),
        ("mouse-button", InputKind::MouseButton(MouseButtonMsg { x: 3.0, y: 4.0, button: WireButton::Forward as i32, pressed: true, modifiers: mods })),
        ("scroll", InputKind::Scroll(ScrollMsg { x: 5.0, y: 6.0, dx: -1.0, dy: 120.0, modifiers: mods })),
        ("key", InputKind::Key(KeyMsg { key: "Enter".into(), code: "Enter".into(), pressed: false, modifiers: mods })),
        ("text", InputKind::Text(TextMsg { text: "héllo".into() })),
        ("resize", InputKind::Resize(Resize { width: 800, height: 600 })),
    ];
    for (i, (name, kind)) in inputs.into_iter().enumerate() {
        write(&format!("stream-input-{name}.bin"), stream(Body::Input(InputMsg { seq: i as u64 + 1, sent_us: 42, kind: Some(kind) })));
    }

    let datagram = Datagram { kind: Some(DatagramKind::Fragment(Fragment { seq: 7, index: 0, count: 2, header: Some(header), data: Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]) })) };
    write("datagram-fragment.bin", datagram.encode_to_vec());

    // A valid Close, then a message holding only field 99 (length delimited, 2 bytes):
    // the second decodes as `body: None`.
    let mut unknown = stream(Body::Close(Close { reason: "bye".into() }));
    let tail = [0x9a, 0x06, 2, 0xAA, 0xBB];
    unknown.push(tail.len() as u8);
    unknown.extend_from_slice(&tail);
    let mut cursor = unknown.as_slice();
    assert!(matches!(StreamMessage::decode_length_delimited(&mut cursor).unwrap().body, Some(Body::Close(_))));
    assert!(StreamMessage::decode_length_delimited(&mut cursor).unwrap().body.is_none());
    assert!(cursor.is_empty());
    write("stream-unknown-body.bin", unknown);
}
