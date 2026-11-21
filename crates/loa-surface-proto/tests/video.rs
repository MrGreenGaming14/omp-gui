//! Encoding negotiation over `omp/surface/1`: the viewer's `Open.encodings`
//! pick what a live surface streams, and `Accept` says what that is.

use loa_contracts::surface::{Encoding, Surface};
use loa_surface_bench::encode::probe;
use loa_surface_bench::live::{LiveVideo, SOURCE, VideoSurface};
use loa_surface_bench::synthetic::{SyntheticConfig, SyntheticSurface};
use loa_surface_bench::video::VideoDecoder;
use loa_surface_proto::{Opened, PixelViewer, ViewerEvent, ViewerOptions, memory_pair, open, serve};
use std::sync::Arc;
use std::time::Duration;

fn video_surface() -> Arc<VideoSurface> {
    let source: Arc<dyn Surface> = Arc::new(SyntheticSurface::new(SyntheticConfig::default()).unwrap());
    VideoSurface::new(source, Arc::new(LiveVideo::with_probed(None, probe(None))), true).unwrap()
}

async fn viewer(surface: Arc<VideoSurface>, encodings: Vec<Encoding>) -> PixelViewer {
    let (server_end, viewer_end) = memory_pair(Some(1200));
    let uri = surface.info().uri;
    tokio::spawn(serve(surface, server_end));
    let Opened::Pixel(viewer) = open(viewer_end, &uri, ViewerOptions::new(encodings)).await.unwrap() else {
        panic!("expected a pixel surface");
    };
    viewer
}

async fn next_frame(viewer: &mut PixelViewer) -> loa_contracts::surface::Frame {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), viewer.next_event()).await.unwrap().unwrap();
        if let ViewerEvent::Frame(frame) = event {
            return frame;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_that_decodes_h264_gets_video_and_is_told_the_encoder() {
    let mut viewer = viewer(video_surface(), vec![Encoding::H264, Encoding::Jpeg, Encoding::Png, Encoding::Rgba]).await;
    let stream = viewer.stream().cloned().expect("the server says what it streams");
    assert_eq!((stream.encoding, stream.encoder.as_str(), stream.hardware), (Encoding::H264, "rusty_h264", false));

    let frame = next_frame(&mut viewer).await;
    assert_eq!(frame.encoding, Encoding::H264);
    assert!(frame.keyframe);
    let mut picture = vec![0; frame.width as usize * frame.height as usize * 4];
    assert!(VideoDecoder::new().decode(&frame, &mut picture).unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_without_video_decoders_gets_still_frames() {
    let mut viewer = viewer(video_surface(), vec![Encoding::Jpeg, Encoding::Png, Encoding::Rgba]).await;
    let stream = viewer.stream().cloned().unwrap();
    assert_eq!(stream.encoder, SOURCE);
    assert_eq!(next_frame(&mut viewer).await.encoding, Encoding::Jpeg);
}
