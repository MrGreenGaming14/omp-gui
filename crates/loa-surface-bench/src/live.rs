//! Video for live surfaces: [`VideoSurface`] codes another surface's
//! pictures as H.264 or HEVC, with the hardware encoder [`LiveVideo`] found
//! on this machine when it has one, the in-process software encoder
//! otherwise, and what the viewer negotiated.

use crate::canvas::Canvas;
use crate::decode::FrameDecoder;
use crate::encode::{
    self, Codec, EncoderChoice, EncoderInfo, EncoderSettings, LiveEncoder, ProbeResult, ffmpeg::FfmpegConfig, software,
};
use async_trait::async_trait;
use bytes::Bytes;
use loa_surface::{Document, Encoding, Frame, InputEvent, StreamInfo, Surface, SurfaceInfo};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use tokio::sync::broadcast::{self, error::TryRecvError};

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

    pub fn ffmpeg(&self) -> Option<&FfmpegConfig> {
        self.ffmpeg.as_ref()
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
        hardware.or_else(|| accepted.contains(&Encoding::H264).then(software::info))
    }

    fn open(&self, info: &EncoderInfo, width: u32, height: u32) -> crate::Result<LiveEncoder> {
        let choice = match info.name.strip_prefix(encode::ffmpeg::NAME_PREFIX) {
            Some(name) => EncoderChoice::Ffmpeg(name.to_owned()),
            None => EncoderChoice::SoftwareH264,
        };
        encode::open(&choice, &EncoderSettings::new(width, height), self.ffmpeg.as_ref(), &self.probed)
    }
}

/// What a [`StreamInfo`] says about an encoder.
pub fn stream_info(info: &EncoderInfo) -> StreamInfo {
    StreamInfo {
        encoding: info.codec.encoding(),
        encoder: info.name.trim_start_matches(encode::ffmpeg::NAME_PREFIX).to_owned(),
        hardware: info.accel.is_hardware(),
    }
}

/// Encoder name of frames passed on as the source made them.
pub const SOURCE: &str = "source";

struct Shared {
    live: Arc<LiveVideo>,
    /// The encoder viewers negotiated; `None` passes source frames on.
    wanted: Mutex<Option<EncoderInfo>>,
    stream: Mutex<Option<StreamInfo>>,
    keyframe: AtomicBool,
    stop: AtomicBool,
}

impl Shared {
    fn wanted(&self) -> Option<EncoderInfo> {
        lock(&self.wanted).clone()
    }

    fn set_stream(&self, stream: StreamInfo) {
        *lock(&self.stream) = Some(stream);
    }
}

/// A pixel surface whose pictures are coded as video. Input, screenshots
/// and info go to the wrapped surface; frames come from a worker thread
/// that rebuilds the picture from the source frames and codes it.
pub struct VideoSurface {
    inner: Arc<dyn Surface>,
    shared: Arc<Shared>,
    frames: broadcast::Sender<Arc<Frame>>,
}

impl VideoSurface {
    /// Wraps the pixel surface `inner`. Until a viewer negotiates video it
    /// passes the source frames on. Needs a Tokio runtime (keyframe requests
    /// to `inner` run on it).
    pub fn new(inner: Arc<dyn Surface>, live: Arc<LiveVideo>) -> loa_surface::Result<Arc<Self>> {
        let source = inner.frames()?;
        let shared = Arc::new(Shared {
            live,
            wanted: Mutex::new(None),
            stream: Mutex::new(None),
            keyframe: AtomicBool::new(true),
            stop: AtomicBool::new(false),
        });
        let (frames, _) = broadcast::channel(8);
        let worker = Worker {
            shared: shared.clone(),
            out: frames.clone(),
            inner: Arc::downgrade(&inner),
            runtime: tokio::runtime::Handle::current(),
            decoder: FrameDecoder::new(),
            canvas: Canvas::new(0, 0),
            encoder: None,
            seq: 0,
        };
        std::thread::Builder::new()
            .name("surface-video".into())
            .spawn(move || worker.run(source))
            .map_err(|e| loa_surface::SurfaceError::Backend(format!("starting the video encoder thread: {e}")))?;
        Ok(Arc::new(Self { inner, shared, frames }))
    }

    /// The wrapped surface.
    pub fn inner(&self) -> &Arc<dyn Surface> {
        &self.inner
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

    fn frames(&self) -> loa_surface::Result<broadcast::Receiver<Arc<Frame>>> {
        Ok(self.frames.subscribe())
    }

    async fn request_keyframe(&self) -> loa_surface::Result<()> {
        self.shared.keyframe.store(true, Ordering::Release);
        // The worker codes on source frames: have the source send its
        // current picture even when nothing changes on it.
        self.inner.request_keyframe().await
    }

    async fn input(&self, event: InputEvent) -> loa_surface::Result<()> {
        self.inner.input(event).await
    }

    async fn document(&self) -> loa_surface::Result<Document> {
        self.inner.document().await
    }

    async fn screenshot(&self) -> loa_surface::Result<Bytes> {
        self.inner.screenshot().await
    }

    async fn negotiate(&self, accepted: &[Encoding]) -> loa_surface::Result<Option<StreamInfo>> {
        let chosen = self.shared.live.choose(accepted);
        let stream = match &chosen {
            Some(info) => stream_info(info),
            None => {
                let encoding = self.inner.stream().map(|s| s.encoding).or_else(|| {
                    lock(&self.shared.stream).as_ref().filter(|s| s.encoder == SOURCE).map(|s| s.encoding)
                });
                StreamInfo { encoding: encoding.unwrap_or(Encoding::Rgba), encoder: SOURCE.to_owned(), hardware: false }
            }
        };
        *lock(&self.shared.wanted) = chosen;
        self.shared.set_stream(stream.clone());
        self.request_keyframe().await?;
        Ok(Some(stream))
    }

    fn stream(&self) -> Option<StreamInfo> {
        lock(&self.shared.stream).clone()
    }
}

struct Worker {
    shared: Arc<Shared>,
    out: broadcast::Sender<Arc<Frame>>,
    inner: Weak<dyn Surface>,
    runtime: tokio::runtime::Handle,
    decoder: FrameDecoder,
    /// The source picture, rebuilt from its frames.
    canvas: Canvas,
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
                    self.source_keyframe();
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
                    Err(TryRecvError::Lagged(_)) => self.source_keyframe(),
                    Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                }
            }
            self.handle(&batch);
        }
    }

    fn source_keyframe(&self) {
        if let Some(inner) = self.inner.upgrade() {
            self.runtime.spawn(async move {
                if let Err(error) = inner.request_keyframe().await {
                    tracing::debug!(%error, "asking the source surface for a keyframe");
                }
            });
        }
    }

    fn handle(&mut self, batch: &[Arc<Frame>]) {
        for frame in batch {
            if let Err(error) = self.decoder.apply(frame, &mut self.canvas) {
                tracing::debug!(%error, "source frame");
                self.source_keyframe();
            }
        }
        let Some(last) = batch.last() else { return };
        let even = self.canvas.width() % 2 == 0 && self.canvas.height() % 2 == 0 && self.canvas.width() > 0;
        match self.shared.wanted().filter(|_| even) {
            Some(wanted) => self.encode(wanted, last),
            None => {
                self.encoder = None;
                for frame in batch {
                    self.pass_on(frame);
                }
            }
        }
    }

    fn pass_on(&mut self, frame: &Frame) {
        self.shared.set_stream(StreamInfo { encoding: frame.encoding, encoder: SOURCE.to_owned(), hardware: false });
        self.seq += 1;
        let _ = self.out.send(Arc::new(Frame { seq: self.seq, ..frame.clone() }));
    }

    fn encode(&mut self, wanted: EncoderInfo, last: &Frame) {
        let size = (self.canvas.width(), self.canvas.height());
        let reopen = self.encoder.as_ref().is_none_or(|(info, encoder)| *info != wanted || encoder.size() != size);
        if reopen {
            self.encoder = None;
            match self.shared.live.open(&wanted, size.0, size.1) {
                Ok(encoder) => {
                    self.shared.set_stream(stream_info(&wanted));
                    self.encoder = Some((wanted, encoder));
                }
                Err(error) => return self.fall_back(&wanted, &error.to_string(), last),
            }
        }
        let Some((info, encoder)) = self.encoder.as_mut() else { return };
        if self.shared.keyframe.swap(false, Ordering::AcqRel) {
            encoder.request_keyframe();
        }
        match encoder.encode(self.canvas.rgba(), last.input_seq, last.captured_us) {
            Ok(encoded) => {
                for frame in encoded.frames {
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

    /// `failed` can't code: hardware falls back to software H.264, software
    /// to the source frames.
    fn fall_back(&mut self, failed: &EncoderInfo, error: &str, last: &Frame) {
        tracing::warn!(encoder = %failed.name, %error, "live video encoder failed");
        let next = (failed.accel.is_hardware() && failed.codec == Codec::H264).then(software::info);
        *lock(&self.shared.wanted) = next.clone();
        self.shared.keyframe.store(true, Ordering::Release);
        match next {
            Some(next) => self.encode(next, last),
            None => {
                // The viewer needs a whole picture in the new encoding.
                self.source_keyframe();
                self.pass_on(last);
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
