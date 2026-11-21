//! Live video: [`VideoSurface`] codes a surface's pictures as H.264/HEVC with a
//! probed hardware encoder (software H.264 bridging), or passes them on.

pub mod decode;
pub mod encode;
pub mod ffmpeg;

use crate::pixels::RgbaImage;
use crate::proto::{ProtoError, Result};
use async_trait::async_trait;
use bytes::Bytes;
use decode::FrameDecoder;
use encode::{Codec, EncoderInfo, LiveEncoder, ProbeResult, SoftwareH264, VideoEncoder, software_info};
use ffmpeg::FfmpegConfig;
use loa_contracts::surface::{
    self as contract, Document, Encoding, Frame, Input, InputEvent, StreamInfo, Surface, SurfaceError, SurfaceInfo,
};
use loa_util::LockExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use tokio::sync::broadcast::{self, error::RecvError, error::TryRecvError};

pub(crate) fn video_error(message: impl Into<String>) -> ProtoError {
    ProtoError::Video(message.into())
}

/// The encoders this machine has, each tried once for real.
#[derive(Clone, Debug)]
pub struct LiveVideo {
    ffmpeg: Option<FfmpegConfig>,
    probed: Vec<ProbeResult>,
}

impl LiveVideo {
    /// Tries every known ffmpeg hardware encoder (blocking).
    pub fn probe(ffmpeg: Option<FfmpegConfig>) -> Self {
        Self { probed: encode::probe(ffmpeg.as_ref()), ffmpeg }
    }

    pub fn with_probed(ffmpeg: Option<FfmpegConfig>, probed: Vec<ProbeResult>) -> Self {
        Self { ffmpeg, probed }
    }

    pub fn encoders(&self) -> &[ProbeResult] {
        &self.probed
    }

    /// The first `accepted` codec with working hardware, else software H.264
    /// if accepted, else `None` (source frames pass on).
    pub fn choose(&self, accepted: &[Encoding]) -> Option<EncoderInfo> {
        let hardware = |codec| {
            let found = self.probed.iter().find(|p| p.works() && p.info.accel.is_hardware() && p.info.codec == codec);
            found.map(|p| p.info.clone())
        };
        let mut codecs = accepted.iter().filter_map(|&e| [Codec::H264, Codec::Hevc].into_iter().find(|c| c.encoding() == e));
        codecs.find_map(hardware).or_else(|| accepted.contains(&Encoding::H264).then(software_info))
    }

    fn open(&self, info: &EncoderInfo, size: (u32, u32)) -> Result<LiveEncoder> {
        let encoder: Box<dyn VideoEncoder> = match (info.name.strip_prefix(ffmpeg::NAME_PREFIX), &self.ffmpeg) {
            (Some(name), Some(config)) => Box::new(ffmpeg::FfmpegEncoder::spawn(config, name, size)?),
            (Some(name), None) => return Err(video_error(format!("{name} needs an ffmpeg binary"))),
            (None, _) => Box::new(SoftwareH264::new(size.0, size.1)?),
        };
        Ok(LiveEncoder::new(size, info.codec, encoder))
    }
}

pub fn stream_info(info: &EncoderInfo) -> StreamInfo {
    StreamInfo {
        encoding: info.codec.encoding(),
        encoder: info.name.trim_start_matches(ffmpeg::NAME_PREFIX).to_owned(),
        hardware: info.accel.is_hardware(),
    }
}

/// Encoder name of frames passed on as the source made them.
pub const SOURCE: &str = "source";

fn source_stream(encoding: Encoding) -> StreamInfo {
    StreamInfo { encoding, encoder: SOURCE.to_owned(), hardware: false }
}

struct Shared {
    live: Arc<LiveVideo>,
    /// Keyframe requests to it run on `runtime`.
    source: Weak<dyn Surface>,
    runtime: tokio::runtime::Handle,
    /// The negotiated encoder; `None` passes source frames on.
    wanted: Mutex<Option<EncoderInfo>>,
    stream: Mutex<Option<StreamInfo>>,
    keyframe: AtomicBool,
    enabled: AtomicBool,
    /// What the last viewer to negotiate decodes.
    accepted: Mutex<Vec<Encoding>>,
    warming: Mutex<Option<Warming>>,
    stop: AtomicBool,
}

struct Warming {
    info: EncoderInfo,
    size: (u32, u32),
    ready: mpsc::Receiver<Result<LiveEncoder>>,
}

impl Shared {
    /// Asks the source for its current picture.
    fn source_keyframe(&self) {
        if let Some(source) = self.source.upgrade() {
            self.runtime.spawn(async move {
                if let Err(error) = source.request_keyframe().await {
                    tracing::debug!(%error, "asking the source surface for a keyframe");
                }
            });
        }
    }

    /// Starts `info` for `size` on its own thread (one size at a time), then
    /// asks the source for its picture so the encoder takes over at once.
    fn warm(self: &Arc<Self>, info: &EncoderInfo, size: (u32, u32)) {
        let mut warming = self.warming.locked();
        if warming.as_ref().is_some_and(|w| w.info == *info && w.size == size) {
            return;
        }
        let (done, ready) = mpsc::channel();
        let (shared, opening) = (self.clone(), info.clone());
        let started = std::thread::Builder::new().name("surface-video-open".into()).spawn(move || {
            // Device setup happens on the first picture (~100 ms with VA-API): code a blank one.
            let primed = shared.live.open(&opening, size).and_then(|mut encoder| {
                encoder.encode(&vec![0; size.0 as usize * size.1 as usize * 4], 0, 0)?;
                encoder.request_keyframe();
                Ok(encoder)
            });
            // The worker may have moved on to another size; then nobody waits.
            if done.send(primed).is_ok() {
                shared.source_keyframe();
            }
        });
        match started {
            Ok(_) => *warming = Some(Warming { info: info.clone(), size, ready }),
            Err(error) => tracing::warn!(%error, "starting the video encoder opening thread"),
        }
    }

    /// The encoder [`warm`](Self::warm) started for `info` at `size`, once it is up (or failed).
    fn warmed(&self, info: &EncoderInfo, size: (u32, u32)) -> Option<Result<LiveEncoder>> {
        let mut warming = self.warming.locked();
        let current = warming.as_ref().filter(|w| w.info == *info && w.size == size)?;
        let result = match current.ready.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err(video_error("the encoder opening thread died")),
        };
        *warming = None;
        Some(result)
    }
}

/// Frames are rebuilt and coded by a worker thread; the rest goes to `inner`.
pub struct VideoSurface {
    inner: Arc<dyn Surface>,
    shared: Arc<Shared>,
    frames: broadcast::Sender<Arc<Frame>>,
}

impl VideoSurface {
    /// Passes source frames on until a viewer negotiates video while
    /// `enabled`. Needs a Tokio runtime.
    pub fn new(inner: Arc<dyn Surface>, live: Arc<LiveVideo>, enabled: bool) -> contract::Result<Arc<Self>> {
        let source = inner.frames()?;
        let shared = Arc::new(Shared {
            live,
            source: Arc::downgrade(&inner),
            runtime: tokio::runtime::Handle::current(),
            wanted: Mutex::new(None),
            stream: Mutex::new(None),
            keyframe: AtomicBool::new(true),
            enabled: AtomicBool::new(enabled),
            accepted: Mutex::new(Vec::new()),
            warming: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let (frames, _) = broadcast::channel(8);
        let worker = Worker {
            shared: shared.clone(),
            out: frames.clone(),
            decoder: FrameDecoder::new(),
            canvas: RgbaImage::default(),
            encoder: None,
            seq: 0,
        };
        std::thread::Builder::new()
            .name("surface-video".into())
            .spawn(move || worker.run(source))
            .map_err(|e| SurfaceError::Backend(format!("starting the video encoder thread: {e}")))?;
        Ok(Arc::new(Self { inner, shared, frames }))
    }

    /// Turns video on or off, redoing the last negotiation.
    pub async fn set_video(&self, enabled: bool) -> contract::Result<Option<StreamInfo>> {
        self.shared.enabled.store(enabled, Ordering::Release);
        let accepted = self.shared.accepted.locked().clone();
        if accepted.is_empty() {
            return Ok(self.stream());
        }
        self.negotiate(&accepted).await
    }
}

impl Drop for VideoSurface {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
    }
}

#[async_trait]
impl Surface for VideoSurface {
    fn info(&self) -> SurfaceInfo {
        self.inner.info()
    }

    /// A new subscriber gets the current picture without asking: the source is asked for a keyframe.
    fn frames(&self) -> contract::Result<broadcast::Receiver<Arc<Frame>>> {
        let frames = self.frames.subscribe();
        self.shared.keyframe.store(true, Ordering::Release);
        self.shared.source_keyframe();
        Ok(frames)
    }

    async fn request_keyframe(&self) -> contract::Result<()> {
        self.shared.keyframe.store(true, Ordering::Release);
        // The worker codes on source frames: the source must resend its picture.
        self.inner.request_keyframe().await
    }

    /// A resize warms the hardware encoder for the new size up right away.
    async fn input(&self, event: InputEvent) -> contract::Result<()> {
        if let &Input::Resize { width, height } = &event.input
            && width.is_multiple_of(2)
            && height.is_multiple_of(2)
            && self.frames.receiver_count() > 0
            && let Some(wanted) = self.shared.wanted.locked().clone().filter(|w| w.accel.is_hardware())
        {
            self.shared.warm(&wanted, (width, height));
        }
        self.inner.input(event).await
    }

    async fn document(&self) -> contract::Result<Document> {
        self.inner.document().await
    }

    async fn screenshot(&self) -> contract::Result<Bytes> {
        self.inner.screenshot().await
    }

    async fn negotiate(&self, accepted: &[Encoding]) -> contract::Result<Option<StreamInfo>> {
        *self.shared.accepted.locked() = accepted.to_vec();
        let chosen = self.shared.enabled.load(Ordering::Acquire).then(|| self.shared.live.choose(accepted)).flatten();
        let stream = match &chosen {
            Some(info) => stream_info(info),
            None => {
                let passed_on = || self.shared.stream.locked().as_ref().filter(|s| s.encoder == SOURCE).map(|s| s.encoding);
                source_stream(self.inner.stream().map(|s| s.encoding).or_else(passed_on).unwrap_or(Encoding::Rgba))
            }
        };
        *self.shared.wanted.locked() = chosen;
        *self.shared.stream.locked() = Some(stream.clone());
        self.request_keyframe().await?;
        Ok(Some(stream))
    }

    fn stream(&self) -> Option<StreamInfo> {
        self.shared.stream.locked().clone()
    }
}

struct Worker {
    shared: Arc<Shared>,
    out: broadcast::Sender<Arc<Frame>>,
    decoder: FrameDecoder,
    /// The source picture, rebuilt from its frames.
    canvas: RgbaImage,
    encoder: Option<(EncoderInfo, LiveEncoder)>,
    seq: u64,
}

impl Worker {
    fn run(mut self, mut source: broadcast::Receiver<Arc<Frame>>) {
        loop {
            let first = match source.blocking_recv() {
                Ok(frame) => frame,
                Err(RecvError::Lagged(_)) => {
                    self.shared.source_keyframe();
                    continue;
                }
                Err(RecvError::Closed) => return,
            };
            if self.shared.stop.load(Ordering::Acquire) {
                return;
            }
            // Everything already queued goes into this one picture.
            let mut batch = vec![first];
            loop {
                match source.try_recv() {
                    Ok(frame) => batch.push(frame),
                    Err(TryRecvError::Lagged(_)) => self.shared.source_keyframe(),
                    Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                }
            }
            self.handle(&batch);
        }
    }

    fn handle(&mut self, batch: &[Arc<Frame>]) {
        let Some(last) = batch.last() else { return };
        // Nobody watches: no encoder runs. A new subscriber asks for a keyframe.
        if self.out.receiver_count() == 0 {
            self.encoder = None;
            return;
        }
        // 4:2:0 video needs an even size; odd surfaces go as they come.
        let even = last.width.is_multiple_of(2) && last.height.is_multiple_of(2) && last.width > 0;
        let Some(wanted) = self.shared.wanted.locked().clone().filter(|_| even) else {
            self.encoder = None;
            for frame in batch {
                self.pass_on(frame);
            }
            return;
        };
        // Video codes the whole picture: rebuild it from the source frames.
        for frame in batch {
            if let Err(error) = self.decoder.apply(frame, &mut self.canvas) {
                tracing::debug!(%error, "source frame");
                self.shared.source_keyframe();
            }
        }
        self.encode(wanted, last);
    }

    fn send(&mut self, frame: Frame) {
        self.seq += 1;
        let _ = self.out.send(Arc::new(Frame { seq: self.seq, ..frame }));
    }

    fn pass_on(&mut self, frame: &Frame) {
        *self.shared.stream.locked() = Some(source_stream(frame.encoding));
        self.send(frame.clone());
    }

    fn encode(&mut self, wanted: EncoderInfo, last: &Frame) {
        let size = (self.canvas.width, self.canvas.height);
        if !self.encoder.as_ref().is_some_and(|(info, encoder)| *info == wanted && encoder.size() == size) {
            match self.switch(&wanted, size) {
                Ok(true) => {}
                // No encoder this viewer decodes is up yet.
                Ok(false) => {
                    self.encoder = None;
                    return self.pass_on(last);
                }
                Err(error) => return self.fall_back(&wanted, &error.to_string(), last),
            }
        }
        let Some((info, encoder)) = self.encoder.as_mut() else { return };
        if self.shared.keyframe.swap(false, Ordering::AcqRel) {
            encoder.request_keyframe();
        }
        match encoder.encode(&self.canvas.data, last.input_seq, last.captured_us) {
            Ok(Some(frame)) => self.send(frame),
            Ok(None) => {}
            Err(error) => {
                let info = info.clone();
                self.encoder = None;
                self.fall_back(&info, &error.to_string(), last);
            }
        }
    }

    /// Puts `wanted` in place for `size` once up, bridging with software
    /// H.264 meanwhile. `Ok(false)` when the viewer decodes no H.264 and
    /// `wanted` isn't up yet.
    fn switch(&mut self, wanted: &EncoderInfo, size: (u32, u32)) -> Result<bool> {
        let opened = match self.shared.warmed(wanted, size) {
            Some(warmed) => Some(warmed?),
            None if !wanted.accel.is_hardware() => Some(self.shared.live.open(wanted, size)?),
            None => None,
        };
        if let Some(encoder) = opened {
            self.use_encoder(wanted.clone(), encoder);
            return Ok(true);
        }
        self.shared.warm(wanted, size);
        let bridge = software_info();
        if self.encoder.as_ref().is_some_and(|(info, encoder)| *info == bridge && encoder.size() == size) {
            return Ok(true);
        }
        if !self.shared.accepted.locked().contains(&Encoding::H264) {
            return Ok(false);
        }
        let encoder = self.shared.live.open(&bridge, size)?;
        self.use_encoder(bridge, encoder);
        Ok(true)
    }

    fn use_encoder(&mut self, info: EncoderInfo, encoder: LiveEncoder) {
        *self.shared.stream.locked() = Some(stream_info(&info));
        self.encoder = Some((info, encoder));
    }

    /// `failed` can't code: hardware falls back to software H.264, software to the source frames.
    fn fall_back(&mut self, failed: &EncoderInfo, error: &str, last: &Frame) {
        tracing::warn!(encoder = %failed.name, %error, "live video encoder failed");
        let next = (failed.accel.is_hardware() && failed.codec == Codec::H264).then(software_info);
        *self.shared.wanted.locked() = next.clone();
        self.shared.keyframe.store(true, Ordering::Release);
        match next {
            Some(next) => self.encode(next, last),
            None => {
                // The viewer needs a whole picture in the new encoding.
                self.shared.source_keyframe();
                self.pass_on(last);
            }
        }
    }
}
