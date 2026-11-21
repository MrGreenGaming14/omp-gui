//! End to end `omp/surface/1` sessions over the in-memory transport, with a
//! lossy datagram channel, plus live video negotiation.

use async_trait::async_trait;
use bytes::Bytes;
use loa_contracts::surface::{
    Document, Encoding, Frame, Input, InputEvent, MouseButton, Rect, Surface, SurfaceError, SurfaceInfo, SurfaceMode,
    SurfaceUri,
};
use loa_surfaces::proto::fragment::fragment;
use loa_surfaces::proto::transport::{DatagramError, Datagrams};
use loa_surfaces::proto::video::{LiveVideo, SOURCE, VideoSurface, decode::VideoDecoder, encode::probe};
use loa_surfaces::proto::wire::{Accept, Body, Close, DocumentChunk, DocumentHeader, Open, StreamMessage};
use loa_surfaces::proto::{
    Opened, PixelViewer, ProtoError, RejectCode, Transport, ViewerEvent, ViewerOptions, memory_pair, open, serve,
};
use prost::Message;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
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

fn fake(name: &str, document: Option<Bytes>) -> (Arc<FakeSurface>, Probe) {
    let mode = if document.is_some() { SurfaceMode::Document } else { SurfaceMode::Pixel };
    let info = SurfaceInfo { uri: uri(name), name: name.into(), width: 64, height: 32, mode, mime: None };
    let document = document.map(|data| Document { name: name.into(), mime: "application/pdf".into(), data });
    let (frames, _) = broadcast::channel(64);
    let (inputs_tx, inputs) = mpsc::unbounded_channel();
    let (keyframes_tx, keyframes) = mpsc::unbounded_channel();
    let surface =
        FakeSurface { info, frames: Mutex::new(Some(frames.clone())), inputs: inputs_tx, keyframes: keyframes_tx, document };
    (Arc::new(surface), Probe { frames, inputs, keyframes })
}

#[async_trait]
impl Surface for FakeSurface {
    fn info(&self) -> SurfaceInfo {
        self.info.clone()
    }

    fn frames(&self) -> loa_contracts::surface::Result<broadcast::Receiver<Arc<Frame>>> {
        self.frames.lock().unwrap().as_ref().map(broadcast::Sender::subscribe).ok_or(SurfaceError::Closed)
    }

    async fn request_keyframe(&self) -> loa_contracts::surface::Result<()> {
        let _ = self.keyframes.send(());
        Ok(())
    }

    async fn input(&self, event: InputEvent) -> loa_contracts::surface::Result<()> {
        let _ = self.inputs.send(event);
        Ok(())
    }

    async fn document(&self) -> loa_contracts::surface::Result<Document> {
        self.document.clone().ok_or(SurfaceError::Mode { uri: self.info.uri.clone(), mode: self.info.mode })
    }

    async fn screenshot(&self) -> loa_contracts::surface::Result<Bytes> {
        Ok(Bytes::new())
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

/// Serves `surface` on `server_end` and opens it on `viewer_end` with
/// `encodings`; waits for the server's opening keyframe request.
async fn view(surface: Arc<dyn Surface>, (server_end, viewer_end): (Transport, Transport), encodings: &[Encoding]) -> PixelViewer {
    let uri = surface.info().uri;
    tokio::spawn(serve(surface, server_end));
    let Opened::Pixel(viewer) = open(viewer_end, &uri, ViewerOptions::new(encodings.to_vec())).await.unwrap() else {
        panic!("expected a pixel surface");
    };
    viewer
}

async fn next_frame(viewer: &mut PixelViewer) -> Frame {
    let event = tokio::time::timeout(Duration::from_secs(10), viewer.next_event()).await.unwrap().unwrap();
    match event {
        ViewerEvent::Frame(frame) => frame,
        other => panic!("expected a frame, got {other:?}"),
    }
}

#[tokio::test]
async fn big_frames_arrive_whole_resizes_come_first_and_input_arrives_in_order() {
    let (surface, mut probe) = fake("tab1", None);
    let mut viewer = view(surface.clone(), memory_pair(Some(1200)), &ALL).await;
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

/// Server side datagrams that lose or hold back datagrams by send index.
struct Faulty {
    inner: Arc<dyn Datagrams>,
    sent: AtomicUsize,
    drop: Vec<usize>,
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
        self.inner.send(datagram)?;
        for (_, d) in self.held.lock().unwrap().extract_if(.., |(release, _)| *release == i) {
            self.inner.send(d)?;
        }
        Ok(())
    }

    async fn recv(&self) -> Result<Bytes, DatagramError> {
        self.inner.recv().await
    }
}

fn faulty(max: usize, drop: Vec<usize>, hold_until: Vec<(usize, usize)>) -> (Transport, Transport) {
    let (mut server_end, viewer_end) = memory_pair(Some(max));
    let inner = server_end.datagrams.clone();
    server_end.datagrams = Arc::new(Faulty { inner, sent: AtomicUsize::new(0), drop, hold_until, held: Mutex::new(Vec::new()) });
    (server_end, viewer_end)
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
    // The second fragment of frame 2 never arrives; frame 1's last fragment
    // comes after frame 2's first.
    let (surface, mut probe) = fake("video", None);
    let mut viewer = view(surface, faulty(MAX, vec![per_frame + 1], vec![(per_frame - 1, per_frame)]), &ALL).await;
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

#[tokio::test]
async fn an_overtaken_frame_is_never_shown_and_without_datagrams_frames_use_the_stream() {
    let frames: Vec<_> = (1..=3).map(|seq| frame(seq, Encoding::Jpeg, true, 100)).collect();
    for (transport, shown) in [(faulty(1200, vec![], vec![(0, 1)]), &frames[1..]), (memory_pair(None), &frames[..])] {
        let (surface, mut probe) = fake("jpeg", None);
        let mut viewer = view(surface, transport, &ALL).await;
        probe.keyframes.recv().await.unwrap();
        for f in &frames {
            probe.frames.send(Arc::new(f.clone())).unwrap();
        }
        for f in shown {
            assert_eq!(&next_frame(&mut viewer).await, f);
        }
    }
}

#[tokio::test]
async fn sessions_close_with_a_reason_on_undecodable_frames_and_closed_surfaces() {
    let (surface, mut probe) = fake("hevc", None);
    let mut viewer = view(surface.clone(), memory_pair(Some(1200)), &[Encoding::Jpeg]).await;
    probe.keyframes.recv().await.unwrap();
    probe.frames.send(Arc::new(frame(1, Encoding::Hevc, true, 10))).unwrap();
    assert!(matches!(viewer.next_event().await, Err(ProtoError::Closed(reason)) if reason.contains("Hevc")));

    let (surface, mut probe) = fake("tab1", None);
    let mut viewer = view(surface.clone(), memory_pair(Some(1200)), &ALL).await;
    probe.keyframes.recv().await.unwrap();
    surface.frames.lock().unwrap().take();
    drop(probe.frames);
    assert!(matches!(viewer.next_event().await, Err(ProtoError::Closed(reason)) if reason == "the surface closed"));
}

#[tokio::test]
async fn a_document_arrives_once_over_the_stream_in_chunks() {
    let data: Bytes = (0..200_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>().into();
    let (server_end, viewer_end) = memory_pair(Some(1200));
    let (surface, _probe) = fake("report.pdf", Some(data.clone()));
    let server = tokio::spawn(serve(surface.clone(), server_end));
    let Opened::Document(viewer) = open(viewer_end, &surface.info.uri, ViewerOptions::new(vec![])).await.unwrap() else {
        panic!("expected a document surface");
    };
    assert_eq!(viewer.receive().await.unwrap().data, data);
    server.await.unwrap().unwrap();
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
async fn corrupted_truncated_and_oversized_documents_are_refused() {
    let data = Bytes::from_static(b"%PDF-1.7 hello");
    let crc = crc32fast::hash(&data);
    let cases: [(u64, u32, Option<&str>, usize); 3] =
        [(data.len() as u64, crc ^ 1, None, 1 << 20), (data.len() as u64 + 10, crc, Some("crashed"), 1 << 20), (10_000, 0, None, 1000)];
    for (len, crc32, close, max) in cases {
        let doc = SurfaceInfo { mode: SurfaceMode::Document, ..fake("a.pdf", None).0.info.clone() };
        let mut script: Vec<StreamMessage> = vec![
            Body::Accept(Accept { version: 1, info: Some((&doc).into()), datagrams: false, stream: None }).into(),
            Body::DocumentHeader(DocumentHeader { name: "a.pdf".into(), mime: "application/pdf".into(), length: len, crc32 }).into(),
            Body::DocumentChunk(DocumentChunk { offset: 0, data: data.clone() }).into(),
        ];
        script.extend(close.map(|reason| Body::Close(Close { reason: reason.into() }).into()));
        let (server_end, viewer_end) = memory_pair(None);
        tokio::spawn(scripted_server(server_end, script));
        let options = ViewerOptions { encodings: vec![], max_document: max as u64 };
        let Opened::Document(viewer) = open(viewer_end, &uri("a.pdf"), options).await.unwrap() else { panic!() };
        match (viewer.receive().await, close) {
            (Err(ProtoError::Closed(reason)), Some(close)) => assert_eq!(reason, close),
            (Err(ProtoError::Checksum), None) if max > 1000 => {}
            (Err(ProtoError::TooLarge { len: 10_000, max: 1000 }), None) => {}
            (other, _) => panic!("{len} bytes, crc {crc32:x}: {other:?}"),
        }
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
    for (accepted, encoding) in [(&ALL[..], Encoding::H264), (&ALL[..3], Encoding::Rgba)] {
        let (source, src) = fake("live", None);
        let video = VideoSurface::new(source, Arc::new(LiveVideo::with_probed(None, probe(None))), true).unwrap();
        let mut viewer = view(video, memory_pair(Some(1200)), accepted).await;
        let stream = viewer.stream().cloned().expect("the server says what it streams");
        let expected = if encoding == Encoding::H264 { "rusty_h264" } else { SOURCE };
        assert_eq!(stream.encoder, expected);
        let picture = Frame { encoding: Encoding::Rgba, ..frame(1, Encoding::Rgba, true, 64 * 32 * 4) };
        // The worker codes on source frames; send until the viewer shows one.
        let shown = loop {
            src.frames.send(Arc::new(picture.clone())).unwrap();
            if let Ok(Ok(ViewerEvent::Frame(f))) = tokio::time::timeout(Duration::from_millis(200), viewer.next_event()).await {
                break f;
            }
        };
        assert_eq!(shown.encoding, encoding);
        if encoding == Encoding::H264 {
            let mut rgba = vec![0; 64 * 32 * 4];
            assert!(shown.keyframe && VideoDecoder::new().decode(&shown, &mut rgba).unwrap());
        }
    }
}
