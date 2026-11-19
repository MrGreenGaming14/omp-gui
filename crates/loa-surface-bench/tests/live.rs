//! Live surfaces coded as video: negotiation, hardware and software
//! encoders, the fallback, and decoding what comes out.

use loa_surface::{Encoding, Frame, Input, InputEvent, Modifiers, MouseButton, Surface};
use loa_surface_bench::encode::ffmpeg::FfmpegConfig;
use loa_surface_bench::encode::software::SoftwareH264;
use loa_surface_bench::encode::{
    Accel, AccessUnit, Codec, EncoderInfo, EncoderSettings, LiveEncoder, ProbeResult, Strategy, VideoEncoder, probe,
};
use loa_surface_bench::live::{LiveVideo, Opener, SOURCE, VideoSurface};
use loa_surface_bench::synthetic::{SyntheticConfig, SyntheticSurface};
use loa_surface_bench::video::VideoDecoder;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

const BACKGROUND: [u8; 3] = [25, 205, 55];
const MARKER: [u8; 3] = [225, 45, 195];

fn synthetic() -> Arc<dyn Surface> {
    Arc::new(SyntheticSurface::new(SyntheticConfig::default()).unwrap())
}

fn software_only() -> Arc<LiveVideo> {
    Arc::new(LiveVideo::with_probed(None, probe(None)))
}

async fn next(frames: &mut broadcast::Receiver<Arc<Frame>>) -> Arc<Frame> {
    tokio::time::timeout(Duration::from_secs(10), frames.recv()).await.expect("a frame in time").unwrap()
}

/// Frames until one of `encoding` with `input_seq` at least `input`.
async fn until(frames: &mut broadcast::Receiver<Arc<Frame>>, encoding: Encoding, input: u64) -> Vec<Arc<Frame>> {
    let mut seen = Vec::new();
    loop {
        let frame = next(frames).await;
        let done = frame.encoding == encoding && frame.input_seq >= input;
        seen.push(frame);
        if done {
            return seen;
        }
    }
}

fn click(seq: u64, x: f32, y: f32) -> InputEvent {
    let input = Input::MouseButton { x, y, button: MouseButton::Left, pressed: true, modifiers: Modifiers::default() };
    InputEvent { seq, sent_us: 0, input }
}

fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 3] {
    let at = ((y * width + x) * 4) as usize;
    [rgba[at], rgba[at + 1], rgba[at + 2]]
}

/// Lossy video at the default quantizer moves colors a little.
fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 40)
}

type Decode<'a> = &'a mut dyn FnMut(&Frame, &mut [u8]) -> bool;

/// Codes the synthetic surface with what `accepted` negotiates, decodes the
/// stream with `decode` and checks a click shows up in the pixels.
async fn round_trip(live: Arc<LiveVideo>, accepted: &[Encoding], decode: Decode<'_>) -> loa_surface::StreamInfo {
    let video = VideoSurface::new(synthetic(), live, true).unwrap();
    let mut frames = video.frames().unwrap();
    let stream = video.negotiate(accepted).await.unwrap().unwrap();
    assert_eq!(video.stream(), Some(stream.clone()));
    let (width, height) = (320, 180);
    let mut picture = vec![0; width as usize * height as usize * 4];

    let first = until(&mut frames, stream.encoding, 0).await;
    let first = first.last().unwrap();
    assert!(first.keyframe, "the first video frame is a keyframe");
    assert!(decode(first, &mut picture));
    assert!(near(pixel(&picture, width, 100, 100), BACKGROUND), "{:?}", pixel(&picture, width, 100, 100));

    video.input(click(1, 100.0, 100.0)).await.unwrap();
    let mut shown = false;
    for frame in until(&mut frames, stream.encoding, 1).await {
        if frame.encoding == stream.encoding {
            shown |= decode(&frame, &mut picture);
        }
    }
    assert!(shown);
    assert!(near(pixel(&picture, width, 110, 110), MARKER), "{:?}", pixel(&picture, width, 110, 110));
    assert!(near(pixel(&picture, width, 20, 20), BACKGROUND));
    stream
}

fn in_process(decoder: &mut VideoDecoder) -> impl FnMut(&Frame, &mut [u8]) -> bool + '_ {
    |frame, out| decoder.decode(frame, out).unwrap()
}

/// Decodes the whole HEVC stream so far with ffmpeg and keeps its last
/// picture: checks the coded bits, not live decoding.
fn hevc_with_ffmpeg() -> impl FnMut(&Frame, &mut [u8]) -> bool {
    let mut stream = Vec::new();
    move |frame, out| {
        use std::io::Write;
        stream.extend_from_slice(&frame.data);
        let mut child = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "hevc", "-i", "pipe:0", "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&stream).unwrap();
        let pictures = child.wait_with_output().unwrap().stdout;
        assert!(pictures.len() >= out.len(), "ffmpeg decoded {} bytes", pictures.len());
        out.copy_from_slice(&pictures[pictures.len() - out.len()..]);
        true
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn software_h264_round_trips_when_no_hardware_encoder_works() {
    let failed = ProbeResult {
        info: EncoderInfo { name: "ffmpeg h264_vaapi".into(), codec: Codec::H264, accel: Accel::Vaapi },
        error: Some("no VA-API render device in /dev/dri".into()),
    };
    let mut probed = vec![failed];
    probed.extend(probe(None));
    let live = Arc::new(LiveVideo::with_probed(None, probed));
    assert_eq!(live.choose(&[Encoding::Hevc, Encoding::H264]).unwrap().name, "rusty_h264");

    let stream = round_trip(live, &[Encoding::H264, Encoding::Jpeg], &mut in_process(&mut VideoDecoder::new())).await;
    assert_eq!((stream.encoding, stream.encoder.as_str(), stream.hardware), (Encoding::H264, "rusty_h264", false));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hardware_encoder_that_fails_at_runtime_falls_back_to_software() {
    // The probe said VA-API works, but its ffmpeg is gone by the time a
    // viewer arrives.
    let working = ProbeResult {
        info: EncoderInfo { name: "ffmpeg h264_vaapi".into(), codec: Codec::H264, accel: Accel::Vaapi },
        error: None,
    };
    let mut probed = vec![working];
    probed.extend(probe(None));
    let live = Arc::new(LiveVideo::with_probed(Some(FfmpegConfig::new("/nonexistent/ffmpeg")), probed));
    let video = VideoSurface::new(synthetic(), live, true).unwrap();
    let mut frames = video.frames().unwrap();
    let negotiated = video.negotiate(&[Encoding::H264]).await.unwrap().unwrap();
    assert!(negotiated.hardware);
    let frame = until(&mut frames, Encoding::H264, 0).await.pop().unwrap();
    assert!(frame.keyframe);
    let mut picture = vec![0; 320 * 180 * 4];
    assert!(VideoDecoder::new().decode(&frame, &mut picture).unwrap());
    let stream = video.stream().unwrap();
    assert_eq!((stream.encoder.as_str(), stream.hardware), ("rusty_h264", false));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_without_video_gets_the_source_frames() {
    let video = VideoSurface::new(synthetic(), software_only(), true).unwrap();
    let mut frames = video.frames().unwrap();
    let stream = video.negotiate(&[Encoding::Jpeg, Encoding::Png, Encoding::Rgba]).await.unwrap().unwrap();
    assert_eq!(stream.encoder, SOURCE);
    let frame = next(&mut frames).await;
    assert_eq!(frame.encoding, Encoding::Jpeg);
    assert_eq!(video.stream().unwrap().encoding, Encoding::Jpeg);

    // A later viewer asks for video: the stream switches.
    let stream = video.negotiate(&[Encoding::H264, Encoding::Jpeg]).await.unwrap().unwrap();
    assert_eq!(stream.encoding, Encoding::H264);
    until(&mut frames, Encoding::H264, 0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn turning_video_off_and_on_switches_the_watching_viewer() {
    let video = VideoSurface::new(synthetic(), software_only(), false).unwrap();
    let mut frames = video.frames().unwrap();
    let stream = video.negotiate(&[Encoding::H264, Encoding::Jpeg]).await.unwrap().unwrap();
    assert_eq!(stream.encoder, SOURCE, "off: the viewer gets source frames though it decodes H.264");
    until(&mut frames, Encoding::Jpeg, 0).await;

    let stream = video.set_video(true).await.unwrap().unwrap();
    assert_eq!(stream.encoding, Encoding::H264);
    until(&mut frames, Encoding::H264, 0).await;

    let stream = video.set_video(false).await.unwrap().unwrap();
    assert_eq!(stream.encoder, SOURCE);
    until(&mut frames, Encoding::Jpeg, 0).await;
}

/// The VA-API, NVENC, Quick Sync or V4L2 encoder this machine has, if any.
fn hardware() -> Option<Arc<LiveVideo>> {
    let live = LiveVideo::probe(FfmpegConfig::discover());
    let found = live.hardware(Codec::H264).is_some();
    if !found {
        eprintln!("no working hardware encoder here: {:?}", live.encoders());
    }
    found.then(|| Arc::new(live))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ffmpeg and a hardware video encoder (VA-API on this machine)"]
async fn hardware_h264_round_trips_through_the_in_process_decoder() {
    let live = hardware().expect("a hardware H.264 encoder");
    let stream = round_trip(live, &[Encoding::H264, Encoding::Jpeg], &mut in_process(&mut VideoDecoder::new())).await;
    assert_eq!(stream.encoding, Encoding::H264);
    assert!(stream.hardware, "{stream:?}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ffmpeg and a hardware HEVC encoder (VA-API on this machine)"]
async fn hardware_hevc_encodes_live_pictures_ffmpeg_decodes() {
    let live = hardware().expect("a hardware encoder");
    assert!(live.hardware(Codec::Hevc).is_some(), "{:?}", live.encoders());
    assert!(!VideoDecoder::new().supports(Encoding::Hevc));
    let stream = round_trip(live, &[Encoding::Hevc, Encoding::Jpeg], &mut hevc_with_ffmpeg()).await;
    assert_eq!(stream.encoding, Encoding::Hevc);
    assert!(stream.hardware, "{stream:?}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ffmpeg and a hardware video encoder; prints the input to pixels latency"]
async fn hardware_video_keeps_the_click_to_pixels_budget() {
    let live = hardware().expect("a hardware H.264 encoder");
    let video = VideoSurface::new(synthetic(), live, true).unwrap();
    let stream = video.negotiate(&[Encoding::H264]).await.unwrap().unwrap();
    assert!(stream.hardware);
    let config = loa_surface_bench::bench::BenchConfig { iterations: 20, ..Default::default() };
    let result = loa_surface_bench::bench::run(video.as_ref(), &config, None).await.unwrap();
    let report = loa_surface_bench::report::LatencyReport::from_result(&result, Duration::from_millis(50)).unwrap();
    println!("{}", report.table());
    assert!(report.passed, "p95 {} us", report.input_to_pixels.p95_us);
}

/// A pixel surface whose whole RGBA pictures the test sends, at any size.
struct Feed {
    frames: broadcast::Sender<Arc<Frame>>,
    resized: std::sync::Mutex<Vec<(u32, u32)>>,
}

impl Feed {
    fn send(&self, input_seq: u64, (width, height): (u32, u32), gray: u8) -> std::time::Instant {
        let mut data = vec![gray; width as usize * height as usize * 4];
        data.iter_mut().skip(3).step_by(4).for_each(|alpha| *alpha = 255);
        let frame = Frame {
            seq: input_seq,
            captured_us: 0,
            input_seq,
            width,
            height,
            region: loa_surface::Rect { x: 0, y: 0, width, height },
            encoding: Encoding::Rgba,
            keyframe: true,
            data: data.into(),
        };
        let sent = std::time::Instant::now();
        self.frames.send(Arc::new(frame)).unwrap();
        sent
    }
}

#[async_trait::async_trait]
impl Surface for Feed {
    fn info(&self) -> loa_surface::SurfaceInfo {
        loa_surface::SurfaceInfo {
            uri: "surface://bench/feed".parse().unwrap(),
            name: "Feed".into(),
            width: 640,
            height: 480,
            mode: loa_surface::SurfaceMode::Pixel,
            mime: None,
        }
    }

    fn frames(&self) -> loa_surface::Result<broadcast::Receiver<Arc<Frame>>> {
        Ok(self.frames.subscribe())
    }

    async fn request_keyframe(&self) -> loa_surface::Result<()> {
        Ok(())
    }

    async fn input(&self, event: InputEvent) -> loa_surface::Result<()> {
        if let Input::Resize { width, height } = event.input {
            self.resized.lock().unwrap().push((width, height));
        }
        Ok(())
    }

    async fn document(&self) -> loa_surface::Result<loa_surface::Document> {
        Err(loa_surface::SurfaceError::Backend("pixel surface".into()))
    }

    async fn screenshot(&self) -> loa_surface::Result<bytes::Bytes> {
        Err(loa_surface::SurfaceError::Backend("no screenshots".into()))
    }
}

/// A hardware encoder as the stream sees it, coding with the software one.
struct FakeHardware(SoftwareH264);

fn fake_hardware() -> EncoderInfo {
    EncoderInfo { name: "ffmpeg h264_test".into(), codec: Codec::H264, accel: Accel::Vaapi }
}

impl VideoEncoder for FakeHardware {
    fn info(&self) -> EncoderInfo {
        fake_hardware()
    }

    fn encode(&mut self, rgba: &[u8], keyframe: bool) -> loa_surface_bench::Result<AccessUnit> {
        self.0.encode(rgba, keyframe)
    }
}

/// The coded frame of the picture sent as `input_seq`.
async fn coded(frames: &mut broadcast::Receiver<Arc<Frame>>, input_seq: u64) -> Arc<Frame> {
    loop {
        let frame = next(frames).await;
        if frame.input_seq == input_seq {
            return frame;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resize_starts_one_encoder_and_no_picture_waits_for_it() {
    // Each hardware encoder start is counted, then blocks until the test
    // lets it finish: a picture that waited for one would never come.
    let spawned = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (permit, permits) = std::sync::mpsc::channel::<()>();
    let permits = Arc::new(std::sync::Mutex::new(permits));
    let opener: Opener = {
        let spawned = spawned.clone();
        Arc::new(move |info: &EncoderInfo, width, height| {
            let software = SoftwareH264::new(&EncoderSettings::new(width, height))?;
            let encoder: Box<dyn VideoEncoder> = if info.accel.is_hardware() {
                spawned.lock().unwrap().push((width, height));
                permits.lock().unwrap().recv().unwrap();
                Box::new(FakeHardware(software))
            } else {
                Box::new(software)
            };
            Ok(LiveEncoder::new(width, height, Strategy::Video(encoder)))
        })
    };
    let mut probed = vec![ProbeResult { info: fake_hardware(), error: None }];
    probed.extend(probe(None));
    let feed = Arc::new(Feed { frames: broadcast::channel(16).0, resized: Default::default() });
    let video = VideoSurface::new(feed.clone(), Arc::new(LiveVideo::with_opener(probed, opener)), true).unwrap();
    let mut frames = video.frames().unwrap();
    let negotiated = video.negotiate(&[Encoding::H264]).await.unwrap().unwrap();
    assert_eq!((negotiated.encoder.as_str(), negotiated.hardware), ("h264_test", true));
    let mut decoder = VideoDecoder::new();
    let mut check = |frame: &Frame, gray: u8| {
        assert_eq!(frame.encoding, Encoding::H264);
        let mut picture = vec![0; frame.width as usize * frame.height as usize * 4];
        assert!(decoder.decode(frame, &mut picture).unwrap());
        let center = pixel(&picture, frame.width, frame.width / 2, frame.height / 2);
        assert!(near(center, [gray; 3]), "decoded {center:?}, sent {gray}");
    };
    let mut seq = 0;
    let mut send = |size, gray| {
        seq += 1;
        feed.send(seq, size, gray);
        seq
    };

    // The hardware encoder is starting (and held): the first picture is
    // coded in process instead of waiting.
    let frame = coded(&mut frames, send((640, 480), 40)).await;
    assert!(frame.keyframe);
    check(&frame, 40);
    assert_eq!(video.stream().unwrap().encoder, "rusty_h264");
    // Once it is up, the hardware encoder takes over with a keyframe.
    permit.send(()).unwrap();
    let frame = loop {
        let frame = coded(&mut frames, send((640, 480), 80)).await;
        if video.stream().unwrap().encoder == "h264_test" {
            break frame;
        }
        tokio::task::yield_now().await;
    };
    assert!(frame.keyframe);
    check(&frame, 80);
    assert_eq!(*spawned.lock().unwrap(), [(640, 480)]);

    // The viewer resizes: its request starts the 1000x700 encoder, and
    // pictures of that size go out while it is held.
    let resize = Input::Resize { width: 1000, height: 700 };
    video.input(InputEvent { seq: 0, sent_us: 0, input: resize }).await.unwrap();
    assert_eq!(*feed.resized.lock().unwrap(), [(1000, 700)]);
    for gray in [120, 130, 140] {
        let frame = coded(&mut frames, send((1000, 700), gray)).await;
        assert_eq!((frame.width, frame.height), (1000, 700));
        check(&frame, gray);
    }
    assert_eq!(video.stream().unwrap().encoder, "rusty_h264");
    permit.send(()).unwrap();
    let frame = loop {
        let frame = coded(&mut frames, send((1000, 700), 160)).await;
        if video.stream().unwrap().encoder == "h264_test" {
            break frame;
        }
        tokio::task::yield_now().await;
    };
    assert_eq!((frame.width, frame.height, frame.keyframe), (1000, 700, true));
    check(&frame, 160);
    // Exactly one encoder start for the resize, not one per picture.
    assert_eq!(*spawned.lock().unwrap(), [(640, 480), (1000, 700)]);
}
