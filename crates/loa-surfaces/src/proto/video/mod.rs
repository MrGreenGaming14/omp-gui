//! Live video for viewers across the network (`remote-video`):
//! [`VideoSurface`] codes another surface's pictures as H.264 or HEVC with
//! the hardware encoder [`LiveVideo`] found here, the in-process software
//! encoder otherwise, or passes source frames on when the viewer decodes no
//! video. A hardware encoder for a new size warms up on its own thread while
//! the software encoder bridges.

pub mod decode;
pub mod encode;
pub mod ffmpeg;

use crate::pixels::RgbaImage;
use decode::FrameDecoder;
use encode::{Codec, EncoderInfo, EncoderSettings, LiveEncoder, ProbeResult, SoftwareH264, software_info};
use ffmpeg::FfmpegConfig;
use loa_util::LockExt;
use async_trait::async_trait;
use bytes::Bytes;
use loa_contracts::surface::{Document, Encoding, Frame, Input, InputEvent, StreamInfo, Surface, SurfaceInfo};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::broadcast::{self, error::TryRecvError};

pub(crate) fn video_error(message: impl Into<String>) -> crate::proto::ProtoError {
    crate::proto::ProtoError::Video(message.into())
}

/// The encoders this machine has, each tried once for real.
#[derive(Clone, Debug)]
pub struct LiveVideo {
    ffmpeg: Option<FfmpegConfig>,
    probed: Vec<ProbeResult>,
}

impl LiveVideo {
    /// Tries every known ffmpeg hardware encoder (blocking: starts ffmpeg a
    /// few times). Software H.264 is always there.
    pub fn probe(ffmpeg: Option<FfmpegConfig>) -> Self {
        let probed = encode::probe(ffmpeg.as_ref());
        Self { ffmpeg, probed }
    }

    /// A machine with exactly these probe results, e.g. for tests.
    pub fn with_probed(ffmpeg: Option<FfmpegConfig>, probed: Vec<ProbeResult>) -> Self {
        Self { ffmpeg, probed }
    }

    /// Every encoder tried, working or not; software last.
    pub fn encoders(&self) -> &[ProbeResult] {
        &self.probed
    }

    /// The first working hardware encoder for `codec`.
    pub fn hardware(&self, codec: Codec) -> Option<&EncoderInfo> {
        self.probed.iter().find(|p| p.works() && p.info.accel.is_hardware() && p.info.codec == codec).map(|p| &p.info)
    }

    /// The encoder for a viewer that decodes `accepted` (best first): the
    /// first accepted codec with working hardware, else software H.264 when
    /// accepted, else `None` (frames go out as the source makes them).
    pub fn choose(&self, accepted: &[Encoding]) -> Option<EncoderInfo> {
        let codec = |encoding: &Encoding| match encoding {
            Encoding::H264 => Some(Codec::H264),
            Encoding::Hevc => Some(Codec::Hevc),
            _ => None,
        };
        let hardware = accepted.iter().filter_map(codec).find_map(|c| self.hardware(c)).cloned();
        hardware.or_else(|| accepted.contains(&Encoding::H264).then(software_info))
    }

    fn open(&self, info: &EncoderInfo, width: u32, height: u32) -> crate::proto::Result<LiveEncoder> {
        let settings = EncoderSettings::new(width, height);
        let encoder: Box<dyn encode::VideoEncoder> = match (info.name.strip_prefix(ffmpeg::NAME_PREFIX), &self.ffmpeg) {
            (Some(name), Some(config)) => Box::new(ffmpeg::FfmpegEncoder::spawn(config, name, &settings)?),
            (Some(name), None) => return Err(video_error(format!("{name} needs an ffmpeg binary"))),
            (None, _) => Box::new(SoftwareH264::new(&settings)?),
        };
        Ok(LiveEncoder::new(width, height, encoder))
    }
}

/// What a [`StreamInfo`] says about an encoder.
pub fn stream_info(info: &EncoderInfo) -> StreamInfo {
    StreamInfo {
        encoding: info.codec.encoding(),
        encoder: info.name.trim_start_matches(ffmpeg::NAME_PREFIX).to_owned(),
        hardware: info.accel.is_hardware(),
    }
}

/// Encoder name of frames passed on as the source made them.
pub const SOURCE: &str = "source";

struct Shared {
    live: Arc<LiveVideo>,
    /// The wrapped surface; its keyframe requests run on `runtime`.
    source: Weak<dyn Surface>,
    runtime: tokio::runtime::Handle,
    /// The encoder viewers negotiated; `None` passes source frames on.
    wanted: Mutex<Option<EncoderInfo>>,
    stream: Mutex<Option<StreamInfo>>,
    keyframe: AtomicBool,
    /// Video may be chosen; off passes source frames on to every viewer.
    enabled: AtomicBool,
    /// What the last viewer to negotiate decodes.
    accepted: Mutex<Vec<Encoding>>,
    /// The hardware encoder starting for the next picture size.
    warming: Mutex<Option<Warming>>,
    stop: AtomicBool,
}

/// An encoder starting on its own thread.
struct Warming {
    info: EncoderInfo,
    size: (u32, u32),
    ready: mpsc::Receiver<crate::proto::Result<LiveEncoder>>,
}

impl Shared {
    fn wanted(&self) -> Option<EncoderInfo> {
        self.wanted.locked().clone()
    }

    fn set_stream(&self, stream: StreamInfo) {
        *self.stream.locked() = Some(stream);
    }

    /// Has the source send its current picture, e.g. to code it again.
    fn source_keyframe(&self) {
        request_keyframe(&self.source, &self.runtime);
    }

    /// Starts `info` for `size` on its own thread, unless it is already
    /// starting. One size at a time: an encoder for another size still
    /// starting is dropped once it is up. Once up, the source is asked for
    /// its picture, so the encoder takes over without waiting for a change.
    fn warm(&self, info: &EncoderInfo, size: (u32, u32)) {
        let mut warming = self.warming.locked();
        if warming.as_ref().is_some_and(|w| w.info == *info && w.size == size) {
            return;
        }
        let (done, ready) = mpsc::channel();
        let (live, opening) = (self.live.clone(), info.clone());
        let (source, runtime) = (self.source.clone(), self.runtime.clone());
        let started = std::thread::Builder::new().name("surface-video-open".into()).spawn(move || {
            // A hardware encoder sets its device up on the first picture
            // (about 100 ms with VA-API): code a blank one here, so the
            // stream's first picture doesn't wait. The next one is a
            // keyframe either way.
            let primed = live.open(&opening, size.0, size.1).and_then(|mut encoder| {
                encoder.encode(&vec![0; size.0 as usize * size.1 as usize * 4], 0, 0)?;
                encoder.request_keyframe();
                Ok(encoder)
            });
            // The worker may have moved on to another size; then nobody waits.
            if done.send(primed).is_ok() {
                request_keyframe(&source, &runtime);
            }
        });
        match started {
            Ok(_) => *warming = Some(Warming { info: info.clone(), size, ready }),
            Err(error) => tracing::warn!(%error, "starting the video encoder opening thread"),
        }
    }

    /// The encoder [`warm`](Self::warm) started for `info` at `size`, once
    /// it is up (or failed).
    fn warmed(&self, info: &EncoderInfo, size: (u32, u32)) -> Option<crate::proto::Result<LiveEncoder>> {
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

/// A pixel surface whose pictures are coded as video, for viewers across
/// the network. Input, screenshots and info go to the wrapped surface;
/// frames come from a worker thread that rebuilds the picture from the
/// source frames and codes it while anyone watches.
pub struct VideoSurface {
    inner: Arc<dyn Surface>,
    shared: Arc<Shared>,
    frames: broadcast::Sender<Arc<Frame>>,
}

impl VideoSurface {
    /// Wraps the pixel surface `inner`. Until a viewer negotiates video (and
    /// while `enabled` is off) it passes the source frames on. Needs a Tokio
    /// runtime (keyframe requests to `inner` run on it).
    pub fn new(inner: Arc<dyn Surface>, live: Arc<LiveVideo>, enabled: bool) -> loa_contracts::surface::Result<Arc<Self>> {
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
            .map_err(|e| loa_contracts::surface::SurfaceError::Backend(format!("starting the video encoder thread: {e}")))?;
        Ok(Arc::new(Self { inner, shared, frames }))
    }

    /// The wrapped surface.
    pub fn inner(&self) -> &Arc<dyn Surface> {
        &self.inner
    }

    /// Turns video on or off for the viewers already watching: their last
    /// negotiation is redone.
    pub async fn set_video(&self, enabled: bool) -> loa_contracts::surface::Result<Option<StreamInfo>> {
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

    /// A new subscriber gets the current picture without asking: the
    /// source is asked for a keyframe.
    fn frames(&self) -> loa_contracts::surface::Result<broadcast::Receiver<Arc<Frame>>> {
        let frames = self.frames.subscribe();
        self.shared.keyframe.store(true, Ordering::Release);
        self.shared.source_keyframe();
        Ok(frames)
    }

    async fn request_keyframe(&self) -> loa_contracts::surface::Result<()> {
        self.shared.keyframe.store(true, Ordering::Release);
        // The worker codes on source frames: have the source send its
        // current picture even when nothing changes on it.
        self.inner.request_keyframe().await
    }

    /// A resize request starts the video encoder for the new size right
    /// away, so it is up (or nearly) when the first picture of that size
    /// comes.
    async fn input(&self, event: InputEvent) -> loa_contracts::surface::Result<()> {
        if let &Input::Resize { width, height } = &event.input
            && width.is_multiple_of(2)
            && height.is_multiple_of(2)
            && self.frames.receiver_count() > 0
            && let Some(wanted) = self.shared.wanted().filter(|w| w.accel.is_hardware())
        {
            self.shared.warm(&wanted, (width, height));
        }
        self.inner.input(event).await
    }

    async fn document(&self) -> loa_contracts::surface::Result<Document> {
        self.inner.document().await
    }

    async fn screenshot(&self) -> loa_contracts::surface::Result<Bytes> {
        self.inner.screenshot().await
    }

    async fn negotiate(&self, accepted: &[Encoding]) -> loa_contracts::surface::Result<Option<StreamInfo>> {
        *self.shared.accepted.locked() = accepted.to_vec();
        let enabled = self.shared.enabled.load(Ordering::Acquire);
        let chosen = if enabled { self.shared.live.choose(accepted) } else { None };
        let stream = match &chosen {
            Some(info) => stream_info(info),
            None => {
                let encoding = self.inner.stream().map(|s| s.encoding).or_else(|| {
                    self.shared.stream.locked().as_ref().filter(|s| s.encoder == SOURCE).map(|s| s.encoding)
                });
                StreamInfo { encoding: encoding.unwrap_or(Encoding::Rgba), encoder: SOURCE.to_owned(), hardware: false }
            }
        };
        *self.shared.wanted.locked() = chosen;
        self.shared.set_stream(stream.clone());
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
    /// The open encoder and what it is.
    encoder: Option<(EncoderInfo, LiveEncoder)>,
    seq: u64,
}

impl Worker {
    fn run(mut self, mut source: broadcast::Receiver<Arc<Frame>>) {
        loop {
            let first = match source.blocking_recv() {
                Ok(frame) => frame,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    self.shared.source_keyframe();
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return,
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
        if self.out.receiver_count() == 0 {
            // Nobody watches: no encoder runs. A new viewer's subscription
            // asks the source for a whole picture.
            self.encoder = None;
            return;
        }
        // 4:2:0 video needs an even size; odd surfaces go as they come.
        let even = last.width.is_multiple_of(2) && last.height.is_multiple_of(2) && last.width > 0;
        let Some(wanted) = self.shared.wanted().filter(|_| even) else {
            self.encoder = None;
            for frame in batch {
                self.pass_on(frame);
            }
            return;
        };
        // Video codes the whole picture: rebuild it from the source frames.
        // (Switching to video asks the source for a keyframe, so a picture
        // left stale while frames were passed on is replaced at once.)
        for frame in batch {
            if let Err(error) = self.decoder.apply(frame, &mut self.canvas) {
                tracing::debug!(%error, "source frame");
                self.shared.source_keyframe();
            }
        }
        self.encode(wanted, last);
    }

    fn pass_on(&mut self, frame: &Frame) {
        self.shared.set_stream(StreamInfo { encoding: frame.encoding, encoder: SOURCE.to_owned(), hardware: false });
        self.seq += 1;
        let _ = self.out.send(Arc::new(Frame { seq: self.seq, ..frame.clone() }));
    }

    fn encode(&mut self, wanted: EncoderInfo, last: &Frame) {
        let size = (self.canvas.width, self.canvas.height);
        if !self.encoder.as_ref().is_some_and(|(info, encoder)| *info == wanted && encoder.size() == size) {
            match self.switch(&wanted, size) {
                Ok(true) => {}
                Ok(false) => {
                    // No encoder this viewer decodes is up yet.
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
            Ok(frames) => {
                for frame in frames {
                    self.seq += 1;
                    let _ = self.out.send(Arc::new(Frame { seq: self.seq, ..frame }));
                }
            }
            Err(error) => {
                let info = info.clone();
                self.encoder = None;
                self.fall_back(&info, &error.to_string(), last);
            }
        }
    }

    /// Puts an encoder for `size` in place: `wanted` when it is up (the
    /// in-process software encoder opens at once), else the software
    /// encoder bridges while `wanted` starts on its own thread. `Ok(false)`
    /// when the viewer decodes no software H.264 and `wanted` isn't up yet.
    fn switch(&mut self, wanted: &EncoderInfo, size: (u32, u32)) -> crate::proto::Result<bool> {
        let opened = match self.shared.warmed(wanted, size) {
            Some(warmed) => Some(warmed?),
            None if !wanted.accel.is_hardware() => Some(self.shared.live.open(wanted, size.0, size.1)?),
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
        let encoder = self.shared.live.open(&bridge, size.0, size.1)?;
        self.use_encoder(bridge, encoder);
        Ok(true)
    }

    fn use_encoder(&mut self, info: EncoderInfo, encoder: LiveEncoder) {
        self.shared.set_stream(stream_info(&info));
        self.encoder = Some((info, encoder));
    }

    /// `failed` can't code: hardware falls back to software H.264, software
    /// to the source frames.
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

fn request_keyframe(source: &Weak<dyn Surface>, runtime: &tokio::runtime::Handle) {
    if let Some(source) = source.upgrade() {
        runtime.spawn(async move {
            if let Err(error) = source.request_keyframe().await {
                tracing::debug!(%error, "asking the source surface for a keyframe");
            }
        });
    }
}

