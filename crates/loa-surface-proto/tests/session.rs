//! End to end `omp/surface/1` sessions over the in-memory transport, with a
//! fault injecting datagram channel standing in for a lossy network.

use async_trait::async_trait;
use bytes::Bytes;
use loa_contracts::surface::{
    Document, Encoding, Frame, Input, InputEvent, MouseButton, Rect, Surface, SurfaceError, SurfaceInfo, SurfaceMode,
    SurfaceServer, SurfaceUri,
};
use loa_surface_proto::fragment::fragment;
use loa_surface_proto::mcp::{self, ToolCall, ToolRunner};
use loa_surface_proto::transport::{DatagramError, Datagrams};
use loa_surface_proto::wire::{Body, Close, DocumentChunk, DocumentHeader, StreamMessage};
use loa_surface_proto::{
    Opened, PixelViewer, ProtoError, RejectCode, Transport, ViewerEvent, ViewerOptions, memory_pair, open, serve,
    serve_from,
};
use prost::Message;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc};

const ALL: [Encoding; 5] = [Encoding::Rgba, Encoding::Jpeg, Encoding::Png, Encoding::H264, Encoding::Hevc];

/// A pixel or document surface the test drives by hand.
struct FakeSurface {
    info: SurfaceInfo,
    /// `None` once the surface closed.
    frames: Mutex<Option<broadcast::Sender<Arc<Frame>>>>,
    inputs: mpsc::UnboundedSender<InputEvent>,
    keyframes: mpsc::UnboundedSender<()>,
    document: Option<Document>,
}

struct Probe {
    frames: broadcast::Sender<Arc<Frame>>,
    inputs: mpsc::UnboundedReceiver<InputEvent>,
    keyframes: mpsc::UnboundedReceiver<()>,
}

fn uri(name: &str) -> SurfaceUri {
    SurfaceUri::new("test", name).unwrap()
}

fn pixel_surface(name: &str) -> (Arc<FakeSurface>, Probe) {
    let info = SurfaceInfo {
        uri: uri(name),
        name: format!("Tab {name}"),
        width: 64,
        height: 32,
        mode: SurfaceMode::Pixel,
        mime: None,
    };
    surface(info, None)
}

fn document_surface(data: Bytes) -> (Arc<FakeSurface>, Probe) {
    let info = SurfaceInfo {
        uri: uri("report.pdf"),
        name: "report.pdf".into(),
        width: 612,
        height: 792,
        mode: SurfaceMode::Document,
        mime: Some("application/pdf".into()),
    };
    let doc = Document { name: "report.pdf".into(), mime: "application/pdf".into(), data };
    surface(info, Some(doc))
}

fn surface(info: SurfaceInfo, document: Option<Document>) -> (Arc<FakeSurface>, Probe) {
    let (frames, _) = broadcast::channel(64);
    let (inputs_tx, inputs) = mpsc::unbounded_channel();
    let (keyframes_tx, keyframes) = mpsc::unbounded_channel();
    let surface = FakeSurface {
        info,
        frames: Mutex::new(Some(frames.clone())),
        inputs: inputs_tx,
        keyframes: keyframes_tx,
        document,
    };
    (Arc::new(surface), Probe { frames, inputs, keyframes })
}

#[async_trait]
impl Surface for FakeSurface {
    fn info(&self) -> SurfaceInfo {
        self.info.clone()
    }

    fn frames(&self) -> loa_contracts::surface::Result<broadcast::Receiver<Arc<Frame>>> {
        match self.info.mode {
            SurfaceMode::Pixel => {
                let frames = self.frames.lock().unwrap();
                frames.as_ref().map(broadcast::Sender::subscribe).ok_or(SurfaceError::Closed)
            }
            SurfaceMode::Document => Err(SurfaceError::Mode { uri: self.info.uri.clone(), mode: self.info.mode }),
        }
    }

    async fn request_keyframe(&self) -> loa_contracts::surface::Result<()> {
        let _ = self.keyframes.send(());
        Ok(())
    }

    async fn input(&self, event: InputEvent) -> loa_contracts::surface::Result<()> {
        if self.info.mode == SurfaceMode::Document {
            return Err(SurfaceError::Mode { uri: self.info.uri.clone(), mode: self.info.mode });
        }
        let _ = self.inputs.send(event);
        Ok(())
    }

    async fn document(&self) -> loa_contracts::surface::Result<Document> {
        self.document.clone().ok_or(SurfaceError::Mode { uri: self.info.uri.clone(), mode: self.info.mode })
    }

    async fn screenshot(&self) -> loa_contracts::surface::Result<Bytes> {
        Ok(Bytes::from_static(b"\x89PNG\r\n\x1a\nfake"))
    }
}

fn frame(seq: u64, encoding: Encoding, keyframe: bool, len: usize) -> Frame {
    Frame {
        seq,
        captured_us: seq * 16_667,
        input_seq: 0,
        width: 64,
        height: 32,
        region: Rect { x: 0, y: 0, width: 64, height: 32 },
        encoding,
        keyframe,
        data: (0..len).map(|i| (i as u64 * 31 + seq) as u8).collect::<Vec<_>>().into(),
    }
}

/// Opens a pixel surface and waits until the server asked for the opening
/// keyframe, so frames sent afterwards reach this viewer.
async fn open_pixel(viewer: Transport, uri: &SurfaceUri, probe: &mut Probe) -> PixelViewer {
    let Opened::Pixel(viewer) = open(viewer, uri, ViewerOptions::new(ALL.to_vec())).await.unwrap() else {
        panic!("expected a pixel surface");
    };
    probe.keyframes.recv().await.unwrap();
    viewer
}

async fn next_frame(viewer: &mut PixelViewer) -> Frame {
    match viewer.next_event().await.unwrap() {
        ViewerEvent::Frame(frame) => frame,
        other => panic!("expected a frame, got {other:?}"),
    }
}

#[tokio::test]
async fn frames_larger_than_a_datagram_arrive_whole_and_input_arrives_in_order() {
    let (server_end, viewer_end) = memory_pair(Some(1200));
    let (surface, mut probe) = pixel_surface("tab1");
    let server = tokio::spawn(serve(surface.clone(), server_end));
    let mut viewer = open_pixel(viewer_end, &surface.info.uri, &mut probe).await;
    assert_eq!(viewer.info(), &surface.info);

    let frames: Vec<_> = (1..=3).map(|seq| frame(seq, Encoding::Jpeg, true, 5000)).collect();
    for f in &frames {
        probe.frames.send(Arc::new(f.clone())).unwrap();
    }
    for f in &frames {
        assert_eq!(&next_frame(&mut viewer).await, f);
    }

    let input = viewer.input();
    let sent = [
        Input::MouseMove { x: 10.0, y: 5.0 },
        Input::MouseButton { x: 10.0, y: 5.0, button: MouseButton::Left, pressed: true, modifiers: Default::default() },
        Input::Text { text: "hello".into() },
    ];
    for (i, event) in sent.iter().enumerate() {
        assert_eq!(input.send(event.clone()).unwrap(), i as u64 + 1);
    }
    for (i, event) in sent.iter().enumerate() {
        let got = probe.inputs.recv().await.unwrap();
        assert_eq!((got.seq, &got.input), (i as u64 + 1, event));
    }

    viewer.close();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_size_change_reaches_the_viewer_before_the_resized_frame() {
    let (server_end, viewer_end) = memory_pair(Some(1200));
    let (surface, mut probe) = pixel_surface("tab1");
    let _server = tokio::spawn(serve(surface.clone(), server_end));
    let mut viewer = open_pixel(viewer_end, &surface.info.uri, &mut probe).await;

    let mut big = frame(1, Encoding::Png, true, 100);
    big.width = 128;
    big.height = 96;
    probe.frames.send(Arc::new(big.clone())).unwrap();
    assert_eq!(viewer.next_event().await.unwrap(), ViewerEvent::Resized { width: 128, height: 96 });
    assert_eq!(viewer.size(), (128, 96));
    assert_eq!(next_frame(&mut viewer).await, big);
}

/// Server side datagrams that lose, hold back and repeat datagrams by their
/// send index, like a bad network path.
struct Faulty {
    inner: Arc<dyn Datagrams>,
    sent: AtomicUsize,
    drop: Vec<usize>,
    duplicate: Vec<usize>,
    /// Held datagrams are sent after the one at the paired index.
    hold_until: Vec<(usize, usize)>,
    held: Mutex<Vec<(usize, Bytes)>>,
}

#[async_trait]
impl Datagrams for Faulty {
    fn max_size(&self) -> Option<usize> {
        self.inner.max_size()
    }

    fn send(&self, datagram: Bytes) -> Result<(), DatagramError> {
        let i = self.sent.fetch_add(1, Ordering::SeqCst);
        if self.drop.contains(&i) {
            return Ok(());
        }
        if let Some(&(_, release)) = self.hold_until.iter().find(|(held, _)| *held == i) {
            self.held.lock().unwrap().push((release, datagram));
            return Ok(());
        }
        self.inner.send(datagram.clone())?;
        if self.duplicate.contains(&i) {
            self.inner.send(datagram)?;
        }
        let mut held = self.held.lock().unwrap();
        for (_, d) in held.extract_if(.., |(release, _)| *release == i) {
            self.inner.send(d)?;
        }
        Ok(())
    }

    async fn recv(&self) -> Result<Bytes, DatagramError> {
        self.inner.recv().await
    }
}

fn faulty(transport: &mut Transport, drop: Vec<usize>, duplicate: Vec<usize>, hold_until: Vec<(usize, usize)>) {
    let inner = transport.datagrams.clone();
    transport.datagrams =
        Arc::new(Faulty { inner, sent: AtomicUsize::new(0), drop, duplicate, hold_until, held: Mutex::new(Vec::new()) });
}

fn fragments_of(f: &Frame, max: usize) -> usize {
    let mut n = 0;
    fragment(f, max, |_| {
        n += 1;
        Ok(())
    })
    .unwrap();
    n
}

#[tokio::test]
async fn a_lost_fragment_drops_its_frame_and_the_video_viewer_asks_for_a_keyframe() {
    const MAX: usize = 400;
    let frames = [
        frame(1, Encoding::H264, true, 1000),
        frame(2, Encoding::H264, false, 1000),
        frame(3, Encoding::H264, false, 1000),
        frame(4, Encoding::H264, true, 1000),
        frame(5, Encoding::H264, false, 1000),
    ];
    let per_frame = fragments_of(&frames[0], MAX);
    assert!(per_frame >= 3);
    // The second fragment of frame 2 never arrives; the first of frame 1 is
    // repeated; frame 1's last fragment comes after frame 2's first.
    let (mut server_end, viewer_end) = memory_pair(Some(MAX));
    faulty(&mut server_end, vec![per_frame + 1], vec![0], vec![(per_frame - 1, per_frame)]);
    let (surface, mut probe) = pixel_surface("video");
    let _server = tokio::spawn(serve(surface.clone(), server_end));
    let mut viewer = open_pixel(viewer_end, &surface.info.uri, &mut probe).await;

    for f in &frames[..3] {
        probe.frames.send(Arc::new(f.clone())).unwrap();
    }
    assert_eq!(next_frame(&mut viewer).await, frames[0]);
    // Frame 2 is incomplete and frame 3 depends on it: neither is shown,
    // and the viewer asks for a keyframe on the reliable stream. The
    // surface answers with keyframe 4.
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

#[tokio::test]
async fn a_frame_overtaken_by_a_newer_one_is_never_shown() {
    const MAX: usize = 1200;
    let f1 = frame(1, Encoding::Jpeg, true, 100);
    let f2 = frame(2, Encoding::Jpeg, true, 100);
    let f3 = frame(3, Encoding::Jpeg, true, 100);
    // Frame 1's only datagram is delivered after frame 2's.
    let (mut server_end, viewer_end) = memory_pair(Some(MAX));
    faulty(&mut server_end, vec![], vec![], vec![(0, 1)]);
    let (surface, mut probe) = pixel_surface("jpeg");
    let _server = tokio::spawn(serve(surface.clone(), server_end));
    let mut viewer = open_pixel(viewer_end, &surface.info.uri, &mut probe).await;

    for f in [&f1, &f2, &f3] {
        probe.frames.send(Arc::new(f.clone())).unwrap();
    }
    assert_eq!(next_frame(&mut viewer).await, f2);
    assert_eq!(next_frame(&mut viewer).await, f3);
}

#[tokio::test]
async fn without_datagrams_frames_fall_back_to_the_stream() {
    let (server_end, viewer_end) = memory_pair(None);
    let (surface, mut probe) = pixel_surface("tcp");
    let _server = tokio::spawn(serve(surface.clone(), server_end));
    let mut viewer = open_pixel(viewer_end, &surface.info.uri, &mut probe).await;
    let f = frame(1, Encoding::Rgba, true, 64 * 32 * 4);
    probe.frames.send(Arc::new(f.clone())).unwrap();
    assert_eq!(next_frame(&mut viewer).await, f);
}

#[tokio::test]
async fn a_frame_the_viewer_cannot_decode_closes_the_session_with_a_reason() {
    let (server_end, viewer_end) = memory_pair(Some(1200));
    let (surface, mut probe) = pixel_surface("hevc");
    let server = tokio::spawn(serve(surface.clone(), server_end));
    let Opened::Pixel(mut viewer) =
        open(viewer_end, &surface.info.uri, ViewerOptions::new(vec![Encoding::Jpeg])).await.unwrap()
    else {
        panic!("expected a pixel surface");
    };
    probe.keyframes.recv().await.unwrap();
    probe.frames.send(Arc::new(frame(1, Encoding::Hevc, true, 10))).unwrap();
    match viewer.next_event().await {
        Err(ProtoError::Closed(reason)) => assert!(reason.contains("Hevc"), "{reason}"),
        other => panic!("expected a close, got {other:?}"),
    }
    assert!(server.await.unwrap().is_err());
}

#[tokio::test]
async fn the_viewer_learns_when_the_surface_goes_away() {
    let (server_end, viewer_end) = memory_pair(Some(1200));
    let (surface, mut probe) = pixel_surface("tab1");
    let server = tokio::spawn(serve(surface.clone(), server_end));
    let mut viewer = open_pixel(viewer_end, &surface.info.uri, &mut probe).await;
    surface.frames.lock().unwrap().take();
    drop(probe.frames);
    match viewer.next_event().await {
        Err(ProtoError::Closed(reason)) => assert_eq!(reason, "the surface closed"),
        other => panic!("expected a close, got {other:?}"),
    }
    server.await.unwrap().unwrap();
}

/// Datagrams that fail the test if either side touches them.
struct Forbidden;

#[async_trait]
impl Datagrams for Forbidden {
    fn max_size(&self) -> Option<usize> {
        Some(1200)
    }

    fn send(&self, _: Bytes) -> Result<(), DatagramError> {
        panic!("document mode sent a datagram");
    }

    async fn recv(&self) -> Result<Bytes, DatagramError> {
        panic!("document mode read a datagram");
    }
}

#[tokio::test]
async fn a_document_arrives_once_over_the_stream_in_chunks_and_never_uses_datagrams() {
    let data: Bytes = (0..200_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>().into();
    let (mut server_end, mut viewer_end) = memory_pair(Some(1200));
    server_end.datagrams = Arc::new(Forbidden);
    viewer_end.datagrams = Arc::new(Forbidden);
    let (surface, _probe) = document_surface(data.clone());
    let server = tokio::spawn(serve(surface.clone(), server_end));
    let Opened::Document(viewer) = open(viewer_end, &surface.info.uri, ViewerOptions::new(vec![])).await.unwrap() else {
        panic!("expected a document surface");
    };
    assert_eq!(viewer.info().mime.as_deref(), Some("application/pdf"));
    let doc = viewer.receive().await.unwrap();
    assert_eq!(doc, Document { name: "report.pdf".into(), mime: "application/pdf".into(), data });
    server.await.unwrap().unwrap();
}

/// Plays the server by hand: reads `Open`, then writes `messages`.
async fn scripted_server(transport: Transport, messages: Vec<StreamMessage>) {
    let mut reader = transport.reader;
    let mut writer = transport.writer;
    let mut byte = [0u8; 1];
    // Enough of the Open to know the viewer spoke.
    reader.read_exact(&mut byte).await.unwrap();
    for msg in messages {
        writer.write_all(&msg.encode_length_delimited_to_vec()).await.unwrap();
    }
    writer.flush().await.unwrap();
    // Keep the stream open until the viewer is done.
    let mut rest = Vec::new();
    let _ = reader.read_to_end(&mut rest).await;
}

fn accept_document(len: u64, crc32: u32) -> Vec<StreamMessage> {
    let info = SurfaceInfo {
        uri: uri("a.pdf"),
        name: "a.pdf".into(),
        width: 1,
        height: 1,
        mode: SurfaceMode::Document,
        mime: Some("application/pdf".into()),
    };
    vec![
        Body::Accept(loa_surface_proto::wire::Accept {
            version: 1,
            info: Some((&info).into()),
            datagrams: false,
            stream: None,
        })
        .into(),
        Body::DocumentHeader(DocumentHeader { name: "a.pdf".into(), mime: "application/pdf".into(), length: len, crc32 })
            .into(),
    ]
}

#[tokio::test]
async fn a_corrupted_document_is_refused() {
    let data = Bytes::from_static(b"%PDF-1.7 hello");
    let mut script = accept_document(data.len() as u64, crc32fast_hash(&data) ^ 1);
    script.push(Body::DocumentChunk(DocumentChunk { offset: 0, data }).into());
    let (server_end, viewer_end) = memory_pair(None);
    tokio::spawn(scripted_server(server_end, script));
    let Opened::Document(viewer) = open(viewer_end, &uri("a.pdf"), ViewerOptions::new(vec![])).await.unwrap() else {
        panic!("expected a document surface");
    };
    assert!(matches!(viewer.receive().await, Err(ProtoError::Checksum)));
}

#[tokio::test]
async fn a_truncated_document_is_refused() {
    let data = Bytes::from_static(b"%PDF-1.7 hello");
    let mut script = accept_document(data.len() as u64 + 10, crc32fast_hash(&data));
    script.push(Body::DocumentChunk(DocumentChunk { offset: 0, data }).into());
    script.push(Body::Close(Close { reason: "crashed".into() }).into());
    let (server_end, viewer_end) = memory_pair(None);
    tokio::spawn(scripted_server(server_end, script));
    let Opened::Document(viewer) = open(viewer_end, &uri("a.pdf"), ViewerOptions::new(vec![])).await.unwrap() else {
        panic!("expected a document surface");
    };
    assert!(matches!(viewer.receive().await, Err(ProtoError::Closed(reason)) if reason == "crashed"));
}

#[tokio::test]
async fn a_document_over_the_viewer_limit_is_refused_before_download() {
    let (server_end, viewer_end) = memory_pair(None);
    tokio::spawn(scripted_server(server_end, accept_document(10_000, 0)));
    let options = ViewerOptions { encodings: vec![], max_document: 1000 };
    let Opened::Document(viewer) = open(viewer_end, &uri("a.pdf"), options).await.unwrap() else {
        panic!("expected a document surface");
    };
    assert!(matches!(viewer.receive().await, Err(ProtoError::TooLarge { len: 10_000, max: 1000 })));
}

fn crc32fast_hash(data: &[u8]) -> u32 {
    // Bitwise CRC-32 (IEEE), independent of the implementation under test.
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[tokio::test]
async fn unknown_messages_from_a_newer_viewer_are_skipped() {
    let (server_end, viewer_end) = memory_pair(Some(1200));
    let (surface, _probe) = pixel_surface("tab1");
    let server = tokio::spawn(serve(surface.clone(), server_end));
    let mut writer = viewer_end.writer;
    let mut reader = viewer_end.reader;
    // A message with body tag 15, which version 1 does not define.
    let unknown = [3u8, 15 << 3 | 2, 1, 0];
    writer.write_all(&unknown).await.unwrap();
    let open = Body::Open(loa_surface_proto::wire::Open {
        version: 7,
        uri: surface.info.uri.to_string(),
        encodings: vec![],
    });
    writer.write_all(&StreamMessage::from(open).encode_length_delimited_to_vec()).await.unwrap();
    writer.write_all(&unknown).await.unwrap();
    writer
        .write_all(&StreamMessage::from(Body::Close(Close { reason: "bye".into() })).encode_length_delimited_to_vec())
        .await
        .unwrap();
    server.await.unwrap().unwrap();
    let mut answer = Vec::new();
    reader.read_to_end(&mut answer).await.unwrap();
    let accept = StreamMessage::decode_length_delimited(answer.as_slice()).unwrap();
    // A newer viewer is answered with the version both speak.
    assert!(matches!(accept.body, Some(Body::Accept(a)) if a.version == 1));
}

#[tokio::test]
async fn opening_another_surface_than_the_served_one_is_rejected() {
    let (server_end, viewer_end) = memory_pair(Some(1200));
    let (surface, _probe) = pixel_surface("tab1");
    let server = tokio::spawn(serve(surface, server_end));
    let result = open(viewer_end, &uri("tab2"), ViewerOptions::new(ALL.to_vec())).await;
    assert!(matches!(result, Err(ProtoError::Rejected { code: RejectCode::NotFound, .. })));
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_viewer_with_protocol_version_zero_is_rejected() {
    let (server_end, viewer_end) = memory_pair(Some(1200));
    let (surface, _probe) = pixel_surface("tab1");
    let server = tokio::spawn(serve(surface.clone(), server_end));
    let mut writer = viewer_end.writer;
    let mut reader = viewer_end.reader;
    let open = Body::Open(loa_surface_proto::wire::Open { version: 0, uri: surface.info.uri.to_string(), encodings: vec![] });
    writer.write_all(&StreamMessage::from(open).encode_length_delimited_to_vec()).await.unwrap();
    server.await.unwrap().unwrap();
    let mut answer = Vec::new();
    reader.read_to_end(&mut answer).await.unwrap();
    let reject = StreamMessage::decode_length_delimited(answer.as_slice()).unwrap();
    assert!(matches!(reject.body, Some(Body::Reject(r)) if r.code == RejectCode::Version as i32));
}

/// Serves the fake surfaces by uri.
struct FakeServer(Vec<Arc<FakeSurface>>);

#[async_trait]
impl SurfaceServer for FakeServer {
    fn name(&self) -> &str {
        "test"
    }

    async fn list(&self) -> loa_contracts::surface::Result<Vec<SurfaceInfo>> {
        Ok(self.0.iter().map(|s| s.info.clone()).collect())
    }

    async fn open(&self, uri: &SurfaceUri) -> loa_contracts::surface::Result<Arc<dyn Surface>> {
        let found = self.0.iter().find(|s| &s.info.uri == uri).cloned();
        found.map(|s| s as Arc<dyn Surface>).ok_or_else(|| SurfaceError::NotFound(uri.clone()))
    }
}

#[tokio::test]
async fn a_surface_server_serves_the_surface_the_viewer_names() {
    let (tab1, _p1) = pixel_surface("tab1");
    let (doc, _p2) = document_surface(Bytes::from_static(b"abc"));
    let server = FakeServer(vec![tab1, doc.clone()]);

    let (server_end, viewer_end) = memory_pair(Some(1200));
    let serving = async { serve_from(&server, server_end).await.unwrap() };
    let viewing = async {
        let Opened::Document(viewer) = open(viewer_end, &doc.info.uri, ViewerOptions::new(vec![])).await.unwrap() else {
            panic!("expected a document surface");
        };
        viewer.receive().await.unwrap()
    };
    let ((), received) = tokio::join!(serving, viewing);
    assert_eq!(received.data, Bytes::from_static(b"abc"));

    let (server_end, viewer_end) = memory_pair(Some(1200));
    let serving = async { serve_from(&server, server_end).await.unwrap() };
    let missing = uri("missing");
    let viewing = open(viewer_end, &missing, ViewerOptions::new(ALL.to_vec()));
    let ((), result) = tokio::join!(serving, viewing);
    assert!(matches!(result, Err(ProtoError::Rejected { code: RejectCode::NotFound, .. })));
}

#[tokio::test]
async fn surface_resources_round_trip_and_other_resources_are_skipped() {
    let (tab1, _p1) = pixel_surface("tab1");
    let (doc, _p2) = document_surface(Bytes::new());
    let mut list = mcp::resource_list([&tab1.info, &doc.info]);
    list["resources"].as_array_mut().unwrap().push(json!({ "uri": "file:///notes.txt", "name": "notes" }));
    assert_eq!(list["resources"][0]["uri"], "surface://test/tab1");
    assert_eq!(list["resources"][0]["_meta"]["omp/surface"]["width"], 64);
    assert_eq!(list["resources"][1]["mimeType"], "application/pdf");
    assert_eq!(mcp::parse_resource_list(&list).unwrap(), vec![tab1.info.clone(), doc.info.clone()]);

    let no_meta = json!({ "uri": "surface://test/x", "name": "x" });
    assert!(matches!(mcp::parse_resource(&no_meta), Err(mcp::McpError::Meta { .. })));
}

#[tokio::test]
async fn agent_tools_drive_the_surface_with_numbered_input() {
    let (surface, mut probe) = pixel_surface("tab1");
    let names: Vec<_> = mcp::tools().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect();
    assert_eq!(names, [mcp::SCREENSHOT, mcp::CLICK, mcp::TYPE, mcp::SCROLL]);
    let runner = ToolRunner::new();

    let click = ToolCall::parse(
        mcp::CLICK,
        &json!({ "uri": "surface://test/tab1", "x": 12, "y": 8.5, "button": "right", "modifiers": ["ctrl"] }),
    )
    .unwrap()
    .unwrap();
    let result = runner.run(&click, &*surface).await;
    assert_eq!(result.get("isError"), None, "{result}");
    let mut events = Vec::new();
    for _ in 0..3 {
        events.push(probe.inputs.recv().await.unwrap());
    }
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(events[0].input, Input::MouseMove { x: 12.0, y: 8.5 });
    let ctrl = loa_contracts::surface::Modifiers { ctrl: true, ..Default::default() };
    assert_eq!(
        events[1].input,
        Input::MouseButton { x: 12.0, y: 8.5, button: MouseButton::Right, pressed: true, modifiers: ctrl }
    );
    assert!(matches!(events[2].input, Input::MouseButton { pressed: false, .. }));

    let typed = ToolCall::parse(mcp::TYPE, &json!({ "uri": "surface://test/tab1", "text": "hi", "submit": true }))
        .unwrap()
        .unwrap();
    runner.run(&typed, &*surface).await;
    assert_eq!(probe.inputs.recv().await.unwrap().input, Input::Text { text: "hi".into() });
    let enter = probe.inputs.recv().await.unwrap();
    assert!(matches!(enter.input, Input::Key { ref key, pressed: true, .. } if key == "Enter"));
    assert_eq!(enter.seq, 5);

    let shot = ToolCall::parse(mcp::SCREENSHOT, &json!({ "uri": "surface://test/tab1" })).unwrap().unwrap();
    let result = runner.run(&shot, &*surface).await;
    assert_eq!(result["content"][0]["type"], "image");
    assert_eq!(result["content"][0]["mimeType"], "image/png");
    assert_eq!(result["content"][0]["data"], "iVBORw0KGgpmYWtl");
}

#[tokio::test]
async fn agent_tool_misuse_is_reported_to_the_agent() {
    let (doc, _p) = document_surface(Bytes::new());
    let runner = ToolRunner::new();
    // Input on a document surface.
    let scroll = ToolCall::parse(mcp::SCROLL, &json!({ "uri": "surface://test/report.pdf", "x": 1, "y": 1, "dy": 50 }))
        .unwrap()
        .unwrap();
    assert_eq!(runner.run(&scroll, &*doc).await["isError"], true);
    // A call for another surface than the one passed.
    let other = ToolCall::parse(mcp::SCREENSHOT, &json!({ "uri": "surface://test/other" })).unwrap().unwrap();
    assert_eq!(runner.run(&other, &*doc).await["isError"], true);
    // Bad arguments never reach a surface.
    assert!(ToolCall::parse(mcp::CLICK, &json!({ "uri": "surface://test/a", "x": 1 })).is_err());
    assert!(ToolCall::parse(mcp::CLICK, &json!({ "uri": "surface://test/a", "x": 1, "y": 1, "count": 9 })).is_err());
    assert!(ToolCall::parse(mcp::TYPE, &json!({ "uri": "surface://test/a", "text": "x", "extra": 1 })).is_err());
    assert_eq!(ToolCall::parse("browser_dom", &json!({})).unwrap(), None);
}
