//! End to end `omp/surface/1` sessions over the in-memory transport.

use bytes::Bytes;
use loa_contracts::surface::{Document, Encoding, Frame, Input, MouseButton, Rect, Surface, SurfaceInfo, SurfaceMode, SurfaceUri};
use loa_surfaces::proto::video::{LiveVideo, SOURCE, VideoSurface, decode::VideoDecoder, encode::probe};
use loa_surfaces::proto::{
    Opened, PixelViewer, ProtoError, RejectCode, Transport, ViewerEvent, ViewerOptions, memory_pair, open, serve,
};
use loa_surfaces::testkit::{FakeSurface, Probe};
use std::sync::Arc;
use std::time::Duration;

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
async fn frames_input_documents_and_rejection() {
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

    // Documents arrive whole; another surface's uri is rejected.
    let data: Bytes = (0..200_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>().into();
    let (server_end, viewer_end) = memory_pair(Some(1200));
    let (surface, _probe) = fake("report.pdf", Some(data.clone()));
    let server = tokio::spawn(serve(surface.clone(), server_end));
    let Ok(Opened::Document(viewer)) = open(viewer_end, &surface.info.uri, ViewerOptions::new(vec![])).await else {
        panic!("expected a document surface");
    };
    assert_eq!(viewer.receive().await.unwrap().data, data);
    server.await.unwrap().unwrap();

    let (server_end, viewer_end) = memory_pair(Some(1200));
    tokio::spawn(serve(surface, server_end));
    let result = open(viewer_end, &uri("tab2"), ViewerOptions::new(ALL.to_vec())).await;
    assert!(matches!(result, Err(ProtoError::Rejected { code: RejectCode::NotFound, .. })));
}

#[tokio::test]
async fn closing_pixel_viewer_ends_session_with_cloned_input_sender() {
    let (surface, mut probe) = fake("close", None);
    let (server_end, viewer_end) = memory_pair(None);
    let server = tokio::spawn(serve(surface.clone(), server_end));
    let Ok(Opened::Pixel(viewer)) = open(viewer_end, &surface.info.uri, ViewerOptions::new(ALL.to_vec())).await else {
        panic!("expected a pixel surface");
    };
    probe.keyframes.recv().await.unwrap();
    let input = viewer.input();
    let event = Input::Text { text: "before close".into() };
    assert_eq!(input.send(event.clone()).unwrap(), 1);
    viewer.close();
    let received = probe.inputs.recv().await.unwrap();
    assert_eq!(received.input, event);
    tokio::time::timeout(Duration::from_secs(10), server).await.unwrap().unwrap().unwrap();
    drop(input);
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
            assert!(shown.keyframe && VideoDecoder::default().decode(&shown, &mut vec![0; 64 * 32 * 4]).unwrap());
        }
    }
}
