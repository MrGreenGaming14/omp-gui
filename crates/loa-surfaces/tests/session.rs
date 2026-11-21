//! End to end `omp/surface/1` sessions over the in-memory transport, with a
//! lossy datagram channel, plus live video negotiation.

use async_trait::async_trait;
use bytes::Bytes;
use loa_contracts::surface::{Document, Encoding, Frame, Input, MouseButton, Rect, Surface, SurfaceInfo, SurfaceMode, SurfaceUri};
use loa_surfaces::proto::fragment::fragment;
use loa_surfaces::proto::transport::{DatagramError, Datagrams};
use loa_surfaces::proto::video::{LiveVideo, SOURCE, VideoSurface, decode::VideoDecoder, encode::probe};
use loa_surfaces::proto::wire::{Accept, Body, Close, DocumentChunk, DocumentHeader, Open, StreamMessage};
use loa_surfaces::proto::{
    Opened, PixelViewer, ProtoError, RejectCode, Transport, ViewerEvent, ViewerOptions, memory_pair, open, serve,
};
use loa_surfaces::testkit::{FakeSurface, Probe};
use prost::Message;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ALL: [Encoding; 5] = [Encoding::Rgba, Encoding::Jpeg, Encoding::Png, Encoding::H264, Encoding::Hevc];

fn uri(name: &str) -> SurfaceUri {
    SurfaceUri::new("test", name).unwrap()
}

fn fake(name: &str, document: Option<Bytes>) -> (Arc<FakeSurface>, Probe) {
    let mode = if document.is_some() { SurfaceMode::Document } else { SurfaceMode::Pixel };
    let info = SurfaceInfo { uri: uri(name), name: name.into(), width: 64, height: 32, mode, mime: None };
    let document = document.map(|data| Document { name: name.into(), mime: "application/pdf".into(), data });
    FakeSurface::new(info, document)
}

fn frame(seq: u64, encoding: Encoding, keyframe: bool, len: usize) -> Frame {
    let region = Rect { x: 0, y: 0, width: 64, height: 32 };
    let data = (0..len).map(|i| (i as u64 * 31 + seq) as u8).collect::<Vec<_>>().into();
    Frame { seq, captured_us: seq * 16_667, input_seq: 0, width: 64, height: 32, region, encoding, keyframe, data }
}

/// Serves `surface` on `server_end` and opens it on `viewer_end` with `encodings`.
async fn view(surface: Arc<dyn Surface>, (server_end, viewer_end): (Transport, Transport), encodings: &[Encoding]) -> PixelViewer {
    let uri = surface.info().uri;
    tokio::spawn(serve(surface, server_end));
    let Ok(Opened::Pixel(viewer)) = open(viewer_end, &uri, ViewerOptions::new(encodings.to_vec())).await else {
        panic!("expected a pixel surface");
    };
    viewer
}

async fn next_frame(viewer: &mut PixelViewer) -> Frame {
    match tokio::time::timeout(Duration::from_secs(10), viewer.next_event()).await.unwrap().unwrap() {
        ViewerEvent::Frame(frame) => frame,
        other => panic!("expected a frame, got {other:?}"),
    }
}

#[tokio::test]
async fn big_frames_arrive_whole_resizes_come_first_and_input_arrives_in_order() {
    // Datagrams, then the stream fallback.
    for max_datagram in [Some(1200), None] {
        let (surface, mut probe) = fake("tab1", None);
        let mut viewer = view(surface.clone(), memory_pair(max_datagram), &ALL).await;
        probe.keyframes.recv().await.unwrap();
        assert_eq!(viewer.info(), &surface.info);
        let frames: Vec<_> = (1..=3).map(|seq| frame(seq, Encoding::Jpeg, true, 5000)).collect();
        for f in &frames {
            probe.frames.send(Arc::new(f.clone())).unwrap();
        }
        for f in &frames {
            assert_eq!(&next_frame(&mut viewer).await, f);
        }
        let big = Frame { width: 128, height: 96, ..frame(4, Encoding::Png, true, 100) };
        probe.frames.send(Arc::new(big.clone())).unwrap();
        assert_eq!(viewer.next_event().await.unwrap(), ViewerEvent::Resized { width: 128, height: 96 });
        assert_eq!(next_frame(&mut viewer).await, big);

        let sent = [
            Input::MouseMove { x: 10.0, y: 5.0 },
            Input::MouseButton { x: 10.0, y: 5.0, button: MouseButton::Left, pressed: true, modifiers: Default::default() },
            Input::Text { text: "hello".into() },
        ];
        for (i, event) in sent.iter().enumerate() {
            assert_eq!(viewer.input().send(event.clone()).unwrap(), i as u64 + 1);
        }
        for (i, event) in sent.iter().enumerate() {
            let got = probe.inputs.recv().await.unwrap();
            assert_eq!((got.seq, &got.input), (i as u64 + 1, event));
        }
    }
}

/// Server side datagrams that lose the one with send index `drop`.
struct Lossy {
    inner: Arc<dyn Datagrams>,
    sent: AtomicUsize,
    drop: usize,
}

#[async_trait]
impl Datagrams for Lossy {
    fn max_size(&self) -> Option<usize> {
        self.inner.max_size()
    }

    fn send(&self, datagram: Bytes) -> Result<(), DatagramError> {
        if self.sent.fetch_add(1, Ordering::SeqCst) == self.drop {
            return Ok(());
        }
        self.inner.send(datagram)
    }

    async fn recv(&self) -> Result<Bytes, DatagramError> {
        self.inner.recv().await
    }
}

#[tokio::test]
async fn a_lost_fragment_drops_its_frame_and_the_video_viewer_asks_for_a_keyframe() {
    const MAX: usize = 400;
    let frames: Vec<_> = (1..=5).map(|seq| frame(seq, Encoding::H264, seq == 1 || seq == 4, 1000)).collect();
    let mut per_frame = 0;
    fragment(&frames[0], MAX, |_| {
        per_frame += 1;
        Ok(())
    })
    .unwrap();
    // The second fragment of frame 2 never arrives.
    let (mut server_end, viewer_end) = memory_pair(Some(MAX));
    let inner = server_end.datagrams.clone();
    server_end.datagrams = Arc::new(Lossy { inner, sent: AtomicUsize::new(0), drop: per_frame + 1 });
    let (surface, mut probe) = fake("video", None);
    let mut viewer = view(surface, (server_end, viewer_end), &ALL).await;
    probe.keyframes.recv().await.unwrap();
    for f in &frames[..3] {
        probe.frames.send(Arc::new(f.clone())).unwrap();
    }
    assert_eq!(next_frame(&mut viewer).await, frames[0]);
    // Frames 2 and 3 are never shown; the keyframe request is answered by 4.
    let showing = async { (next_frame(&mut viewer).await, next_frame(&mut viewer).await) };
    let answering = async {
        probe.keyframes.recv().await.unwrap();
        for f in &frames[3..] {
            probe.frames.send(Arc::new(f.clone())).unwrap();
        }
    };
    let (shown, ()) = tokio::join!(showing, answering);
    assert_eq!(shown, (frames[3].clone(), frames[4].clone()));
}

/// Plays the server by hand: waits for the viewer to speak, then writes `messages`.
async fn scripted_server(transport: Transport, messages: Vec<StreamMessage>) {
    let (mut reader, mut writer) = (transport.reader, transport.writer);
    reader.read_exact(&mut [0u8; 1]).await.unwrap();
    for msg in messages {
        writer.write_all(&msg.encode_length_delimited_to_vec()).await.unwrap();
    }
    writer.flush().await.unwrap();
    let _ = reader.read_to_end(&mut Vec::new()).await;
}

#[tokio::test]
async fn documents_arrive_in_chunks_and_corrupted_truncated_or_oversized_ones_are_refused() {
    let data: Bytes = (0..200_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>().into();
    let (server_end, viewer_end) = memory_pair(Some(1200));
    let (surface, _probe) = fake("report.pdf", Some(data.clone()));
    let server = tokio::spawn(serve(surface.clone(), server_end));
    let Ok(Opened::Document(viewer)) = open(viewer_end, &surface.info.uri, ViewerOptions::new(vec![])).await else {
        panic!("expected a document surface");
    };
    assert_eq!(viewer.receive().await.unwrap().data, data);
    server.await.unwrap().unwrap();

    let data = Bytes::from_static(b"%PDF-1.7 hello");
    let (len, crc) = (data.len() as u64, crc32fast::hash(&data));
    // (header length, header crc, close after the chunk, viewer limit, expected error)
    let cases: [(u64, u32, Option<&str>, u64, fn(&ProtoError) -> bool); 3] = [
        (len, crc ^ 1, None, 1 << 20, |e| matches!(e, ProtoError::Checksum)),
        (len + 10, crc, Some("crashed"), 1 << 20, |e| matches!(e, ProtoError::Closed(r) if r == "crashed")),
        (10_000, 0, None, 1000, |e| matches!(e, ProtoError::TooLarge { len: 10_000, max: 1000 })),
    ];
    for (length, crc32, close, max_document, expected) in cases {
        let doc = SurfaceInfo { mode: SurfaceMode::Document, ..fake("a.pdf", None).0.info.clone() };
        let mut script: Vec<StreamMessage> = vec![
            Body::Accept(Accept { version: 1, info: Some((&doc).into()), datagrams: false, stream: None }).into(),
            Body::DocumentHeader(DocumentHeader { name: "a.pdf".into(), mime: "application/pdf".into(), length, crc32 }).into(),
            Body::DocumentChunk(DocumentChunk { offset: 0, data: data.clone() }).into(),
        ];
        script.extend(close.map(|reason| Body::Close(Close { reason: reason.into() }).into()));
        let (server_end, viewer_end) = memory_pair(None);
        tokio::spawn(scripted_server(server_end, script));
        let options = ViewerOptions { encodings: vec![], max_document };
        let Ok(Opened::Document(viewer)) = open(viewer_end, &uri("a.pdf"), options).await else { panic!() };
        let error = viewer.receive().await.unwrap_err();
        assert!(expected(&error), "{length} bytes, crc {crc32:x}: {error:?}");
    }
}

#[tokio::test]
async fn another_surface_or_protocol_version_zero_is_rejected() {
    let (surface, _probe) = fake("tab1", None);
    let (server_end, viewer_end) = memory_pair(Some(1200));
    tokio::spawn(serve(surface.clone(), server_end));
    let result = open(viewer_end, &uri("tab2"), ViewerOptions::new(ALL.to_vec())).await;
    assert!(matches!(result, Err(ProtoError::Rejected { code: RejectCode::NotFound, .. })));

    let (server_end, viewer_end) = memory_pair(Some(1200));
    let server = tokio::spawn(serve(surface.clone(), server_end));
    let (mut reader, mut writer) = (viewer_end.reader, viewer_end.writer);
    let open = Body::Open(Open { version: 0, uri: surface.info.uri.to_string(), encodings: vec![] });
    writer.write_all(&StreamMessage::from(open).encode_length_delimited_to_vec()).await.unwrap();
    server.await.unwrap().unwrap();
    let mut answer = Vec::new();
    reader.read_to_end(&mut answer).await.unwrap();
    let reject = StreamMessage::decode_length_delimited(answer.as_slice()).unwrap();
    assert!(matches!(reject.body, Some(Body::Reject(r)) if r.code == RejectCode::Version as i32));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_that_decodes_h264_gets_video_and_others_get_source_frames() {
    for (accepted, encoding, encoder) in [(&ALL[..], Encoding::H264, "rusty_h264"), (&ALL[..3], Encoding::Rgba, SOURCE)] {
        let (source, src) = fake("live", None);
        let video = VideoSurface::new(source, Arc::new(LiveVideo::with_probed(None, probe(None))), true).unwrap();
        let mut viewer = view(video, memory_pair(Some(1200)), accepted).await;
        assert_eq!(viewer.stream().expect("the server says what it streams").encoder, encoder);
        let picture = frame(1, Encoding::Rgba, true, 64 * 32 * 4);
        // The worker codes on source frames; send until the viewer shows one.
        let shown = loop {
            src.frames.send(Arc::new(picture.clone())).unwrap();
            if let Ok(Ok(ViewerEvent::Frame(f))) = tokio::time::timeout(Duration::from_millis(200), viewer.next_event()).await {
                break f;
            }
        };
        assert_eq!(shown.encoding, encoding);
        if encoding == Encoding::H264 {
            assert!(shown.keyframe && VideoDecoder::new().decode(&shown, &mut vec![0; 64 * 32 * 4]).unwrap());
        }
    }
}
