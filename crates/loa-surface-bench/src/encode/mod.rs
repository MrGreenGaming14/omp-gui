//! Turning RGBA pictures into [`Frame`]s.
//!
//! Live content goes through a [`VideoEncoder`]: hardware H.264 or HEVC
//! through an `ffmpeg` process ([`ffmpeg`]) when [`probe`] finds a working
//! one, the in-process software H.264 encoder ([`software`]) otherwise.
//! Mostly static content sends only the rectangles that changed, as JPEG
//! ([`region`]). [`LiveEncoder`] wraps either strategy and stamps frames.

pub mod ffmpeg;
pub mod nal;
pub mod region;
pub mod software;

use crate::{BenchError, Result};
use bytes::Bytes;
use loa_surface::{Encoding, Frame, Rect};
use region::{DamageTracker, RegionEncoder};
use serde::Serialize;
use std::time::{Duration, Instant};

/// Video codec of an access unit stream.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Codec {
    H264,
    Hevc,
}

impl Codec {
    pub fn encoding(self) -> Encoding {
        match self {
            Self::H264 => Encoding::H264,
            Self::Hevc => Encoding::Hevc,
        }
    }
}

/// Fixed parameters of one encoder session.
#[derive(Clone, Debug, PartialEq)]
pub struct EncoderSettings {
    /// Even, in pixels.
    pub width: u32,
    /// Even, in pixels.
    pub height: u32,
    /// Nominal frame rate. Frames are still encoded as they come.
    pub fps: u32,
    /// Frames between periodic keyframes. Requested keyframes come on top.
    pub keyframe_interval: u32,
    /// Constant quantizer, 0 (best) to 51.
    pub qp: u8,
}

impl EncoderSettings {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, fps: 60, keyframe_interval: 600, qp: 26 }
    }

    pub(crate) fn frame_len(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    pub(crate) fn check_video_size(&self) -> Result<()> {
        let reason = if self.width == 0 || self.height == 0 {
            "a video frame can't be empty"
        } else if !self.width.is_multiple_of(2) || !self.height.is_multiple_of(2) {
            "4:2:0 video needs an even width and height"
        } else if self.width > 8192 || self.height > 8192 {
            "larger than 8192 pixels on a side"
        } else {
            return Ok(());
        };
        Err(BenchError::Size { width: self.width, height: self.height, reason })
    }
}

/// One coded picture: an Annex B access unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessUnit {
    pub data: Bytes,
    /// An IDR (H.264) or IRAP (HEVC) picture with its parameter sets.
    pub keyframe: bool,
}

/// Where an encoder runs.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Accel {
    /// VA-API (Intel, AMD).
    Vaapi,
    /// NVIDIA NVENC.
    Nvenc,
    /// Intel Quick Sync through oneVPL.
    Qsv,
    /// A V4L2 memory to memory encoder (SoCs, Raspberry Pi).
    V4l2M2m,
    /// CPU.
    Software,
}

impl Accel {
    pub fn is_hardware(self) -> bool {
        self != Self::Software
    }
}

/// What an encoder is, for reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EncoderInfo {
    /// e.g. `ffmpeg h264_vaapi` or `rusty_h264`.
    pub name: String,
    pub codec: Codec,
    pub accel: Accel,
}

/// The encoder port: RGBA pictures in, Annex B access units out, one per
/// picture, with no frame reordering (no B-frames) and no lookahead.
pub trait VideoEncoder: Send {
    fn info(&self) -> EncoderInfo;

    /// Codes one tightly packed RGBA picture of the session size. With
    /// `keyframe` the result is an IDR/IRAP picture carrying its parameter
    /// sets, decodable on its own.
    fn encode(&mut self, rgba: &[u8], keyframe: bool) -> Result<AccessUnit>;
}

/// Frames for one picture and the time it took to code them.
#[derive(Debug)]
pub struct Encoded {
    pub frames: Vec<Frame>,
    /// Damage detection plus coding, without any artificial delay.
    pub encode_time: Duration,
}

/// How a [`LiveEncoder`] codes pictures.
pub enum Strategy {
    /// Live content: every changed picture becomes one video access unit.
    Video(Box<dyn VideoEncoder>),
    /// Mostly static content: only the changed rectangles, as JPEG.
    Regions(RegionEncoder),
}

impl Strategy {
    /// Describes the strategy for reports.
    pub fn describe(&self) -> String {
        match self {
            Self::Video(encoder) => {
                let info = encoder.info();
                let place = if info.accel.is_hardware() { "hardware" } else { "software" };
                format!("{} ({:?}, {place})", info.name, info.codec)
            }
            Self::Regions(encoder) => format!("changed regions as JPEG (quality {})", encoder.quality()),
        }
    }
}

/// Codes a surface's pictures into frames: skips unchanged pictures, sends
/// only dirty rectangles in region mode, honours keyframe requests.
pub struct LiveEncoder {
    width: u32,
    height: u32,
    strategy: Strategy,
    damage: DamageTracker,
    keyframe_pending: bool,
    next_seq: u64,
}

struct FrameStamp<'a> {
    next_seq: &'a mut u64,
    width: u32,
    height: u32,
    input_seq: u64,
    captured_us: u64,
}

impl FrameStamp<'_> {
    fn frame(&mut self, region: Rect, encoding: Encoding, keyframe: bool, data: Bytes) -> Frame {
        let seq = *self.next_seq;
        *self.next_seq += 1;
        Frame {
            seq,
            input_seq: self.input_seq,
            captured_us: self.captured_us,
            width: self.width,
            height: self.height,
            region,
            encoding,
            keyframe,
            data,
        }
    }
}

impl LiveEncoder {
    pub fn new(width: u32, height: u32, strategy: Strategy) -> Self {
        Self {
            width,
            height,
            strategy,
            damage: DamageTracker::new(width, height),
            keyframe_pending: true,
            next_seq: 1,
        }
    }

    pub fn describe(&self) -> String {
        self.strategy.describe()
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The next picture goes out whole and decodable on its own.
    pub fn request_keyframe(&mut self) {
        self.keyframe_pending = true;
    }

    /// Codes `rgba` (tightly packed, the encoder's size). `input_seq` and
    /// `captured_us` are stamped on every frame. Returns no frames when
    /// nothing changed and no keyframe is pending.
    pub fn encode(&mut self, rgba: &[u8], input_seq: u64, captured_us: u64) -> Result<Encoded> {
        let expected = self.width as usize * self.height as usize * 4;
        if rgba.len() != expected {
            return Err(BenchError::Invalid(format!(
                "{} bytes for a {}x{} RGBA picture",
                rgba.len(),
                self.width,
                self.height
            )));
        }
        let started = Instant::now();
        let dirty = self.damage.update(rgba);
        let keyframe = std::mem::take(&mut self.keyframe_pending);
        let full = Rect { x: 0, y: 0, width: self.width, height: self.height };
        let mut frames = Vec::new();
        let mut stamp = FrameStamp {
            next_seq: &mut self.next_seq,
            width: self.width,
            height: self.height,
            input_seq,
            captured_us,
        };
        if keyframe || !dirty.is_empty() {
            match &mut self.strategy {
                Strategy::Video(encoder) => {
                    let unit = encoder.encode(rgba, keyframe)?;
                    let encoding = encoder.info().codec.encoding();
                    frames.push(stamp.frame(full, encoding, unit.keyframe, unit.data));
                }
                Strategy::Regions(encoder) => {
                    let rects = if keyframe { vec![full] } else { dirty };
                    for rect in rects {
                        let data = encoder.encode(rgba, self.width, rect)?;
                        let whole = rect == full;
                        frames.push(stamp.frame(rect, Encoding::Jpeg, whole, data));
                    }
                }
            }
        }
        Ok(Encoded { frames, encode_time: started.elapsed() })
    }

}

/// Which encoder to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncoderChoice {
    /// The first working hardware encoder for the codec from [`probe`], the
    /// in-process software H.264 encoder when there is none.
    Auto { codec: Codec },
    /// The in-process software H.264 encoder.
    SoftwareH264,
    /// Changed regions as JPEG.
    Regions,
    /// A named ffmpeg encoder, e.g. `h264_vaapi`.
    Ffmpeg(String),
}

impl std::str::FromStr for EncoderChoice {
    type Err = BenchError;

    fn from_str(text: &str) -> Result<Self> {
        Ok(match text {
            "auto" | "auto-h264" => Self::Auto { codec: Codec::H264 },
            "auto-hevc" => Self::Auto { codec: Codec::Hevc },
            "software" | "software-h264" => Self::SoftwareH264,
            "regions" | "jpeg" => Self::Regions,
            other => match other.strip_prefix("ffmpeg:") {
                Some(name) if ffmpeg::KNOWN.iter().any(|k| k.name == name) => Self::Ffmpeg(name.to_owned()),
                _ => {
                    return Err(BenchError::Invalid(format!(
                        "unknown encoder `{other}`: use auto, auto-hevc, software, regions or ffmpeg:<{}>",
                        ffmpeg::KNOWN.iter().map(|k| k.name).collect::<Vec<_>>().join("|")
                    )));
                }
            },
        })
    }
}

/// Outcome of trying one encoder at runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProbeResult {
    pub info: EncoderInfo,
    /// `None` when it coded test pictures, otherwise why it didn't.
    pub error: Option<String>,
}

impl ProbeResult {
    pub fn works(&self) -> bool {
        self.error.is_none()
    }
}

/// Lists every encoder this machine could use, each tried for real. The
/// in-process software encoder always comes last; it needs nothing.
pub fn probe(ffmpeg: Option<&ffmpeg::FfmpegConfig>) -> Vec<ProbeResult> {
    let mut found = ffmpeg.map(ffmpeg::probe).unwrap_or_default();
    found.push(ProbeResult { info: software::info(), error: None });
    found
}

/// Opens the encoder `choice` names. For [`EncoderChoice::Auto`], `probed`
/// is the output of [`probe`].
pub fn open(
    choice: &EncoderChoice,
    settings: &EncoderSettings,
    ffmpeg: Option<&ffmpeg::FfmpegConfig>,
    probed: &[ProbeResult],
) -> Result<LiveEncoder> {
    let strategy = match choice {
        EncoderChoice::Regions => Strategy::Regions(RegionEncoder::new(85)),
        EncoderChoice::SoftwareH264 => Strategy::Video(Box::new(software::SoftwareH264::new(settings)?)),
        EncoderChoice::Ffmpeg(name) => {
            let config = ffmpeg.ok_or_else(|| BenchError::Encode(format!("{name} needs an ffmpeg binary")))?;
            Strategy::Video(Box::new(ffmpeg::FfmpegEncoder::spawn(config, name, settings)?))
        }
        EncoderChoice::Auto { codec } => {
            let hardware = probed
                .iter()
                .find(|p| p.works() && p.info.accel.is_hardware() && p.info.codec == *codec);
            match (hardware, ffmpeg) {
                (Some(found), Some(config)) => {
                    let name = found.info.name.trim_start_matches(ffmpeg::NAME_PREFIX);
                    Strategy::Video(Box::new(ffmpeg::FfmpegEncoder::spawn(config, name, settings)?))
                }
                _ if *codec == Codec::Hevc => Strategy::Regions(RegionEncoder::new(85)),
                _ => Strategy::Video(Box::new(software::SoftwareH264::new(settings)?)),
            }
        }
    };
    Ok(LiveEncoder::new(settings.width, settings.height, strategy))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(width: u32, height: u32, fill: u8) -> Vec<u8> {
        vec![fill; width as usize * height as usize * 4]
    }

    #[test]
    fn an_unchanged_picture_sends_nothing_and_a_change_sends_only_its_tiles() {
        let mut live = LiveEncoder::new(64, 64, Strategy::Regions(RegionEncoder::new(90)));
        let mut rgba = picture(64, 64, 30);
        let first = live.encode(&rgba, 0, 0).unwrap();
        assert_eq!(first.frames.len(), 1);
        assert!(first.frames[0].keyframe);
        assert_eq!(first.frames[0].region, Rect { x: 0, y: 0, width: 64, height: 64 });

        assert!(live.encode(&rgba, 0, 1).unwrap().frames.is_empty());

        // One pixel inside the tile at (16, 32).
        let at = (40 * 64 + 20) * 4;
        rgba[at] = 250;
        let update = live.encode(&rgba, 7, 2).unwrap();
        assert_eq!(update.frames.len(), 1);
        let frame = &update.frames[0];
        assert_eq!(frame.region, Rect { x: 16, y: 32, width: 16, height: 16 });
        assert_eq!(frame.input_seq, 7);
        assert!(!frame.keyframe);
        assert!(frame.seq > first.frames[0].seq);
    }

    #[test]
    fn a_keyframe_request_resends_the_whole_static_picture() {
        let mut live = LiveEncoder::new(32, 32, Strategy::Regions(RegionEncoder::new(90)));
        let rgba = picture(32, 32, 30);
        live.encode(&rgba, 0, 0).unwrap();
        live.request_keyframe();
        let again = live.encode(&rgba, 0, 1).unwrap();
        assert_eq!(again.frames.len(), 1);
        assert!(again.frames[0].keyframe);
        assert_eq!(again.frames[0].region.width, 32);
    }

    #[test]
    fn auto_without_hardware_falls_back_to_software_h264() {
        let probed = probe(None);
        let live = open(&EncoderChoice::Auto { codec: Codec::H264 }, &EncoderSettings::new(64, 48), None, &probed)
            .unwrap();
        assert!(live.describe().contains("rusty_h264"), "{}", live.describe());
    }

    #[test]
    fn auto_skips_hardware_that_failed_its_probe() {
        let probed = vec![ProbeResult {
            info: EncoderInfo { name: "ffmpeg h264_vaapi".into(), codec: Codec::H264, accel: Accel::Vaapi },
            error: Some("no device".into()),
        }];
        let config = ffmpeg::FfmpegConfig::new("/nonexistent/ffmpeg");
        let live = open(
            &EncoderChoice::Auto { codec: Codec::H264 },
            &EncoderSettings::new(64, 48),
            Some(&config),
            &probed,
        )
        .unwrap();
        assert!(live.describe().contains("software"));
    }
    #[test]
    fn auto_hevc_without_hardware_sends_jpeg_regions_not_mislabeled_h264() {
        let mut live = open(
            &EncoderChoice::Auto { codec: Codec::Hevc },
            &EncoderSettings::new(64, 48),
            None,
            &probe(None),
        ).unwrap();
        let frame = live.encode(&picture(64, 48, 30), 1, 0).unwrap().frames.remove(0);
        assert_eq!(frame.encoding, Encoding::Jpeg);
        assert!(live.describe().contains("regions"));
    }
    #[test]
    fn fallback_h264_is_decodable_and_preserves_keyframe_requests() {
        let mut live = open(&EncoderChoice::Auto { codec: Codec::H264 }, &EncoderSettings::new(64, 48), None, &probe(None)).unwrap();
        let mut decoder = crate::decode::FrameDecoder::new();
        let mut canvas = crate::canvas::Canvas::new(64, 48);
        let first = live.encode(&picture(64, 48, 80), 1, 10).unwrap().frames.remove(0);
        assert_eq!(first.encoding, Encoding::H264);
        assert!(first.keyframe);
        decoder.apply(&first, &mut canvas).unwrap();
        let changed = live.encode(&picture(64, 48, 170), 2, 20).unwrap().frames.remove(0);
        assert_eq!(changed.encoding, Encoding::H264);
        assert_eq!(changed.input_seq, 2);
        decoder.apply(&changed, &mut canvas).unwrap();
        live.request_keyframe();
        let requested = live.encode(&picture(64, 48, 170), 2, 30).unwrap().frames.remove(0);
        assert!(requested.keyframe);
        crate::decode::FrameDecoder::new().apply(&requested, &mut canvas).unwrap();
    }

    #[test]
    fn failed_hevc_probe_falls_back_to_decodable_jpeg() {
        let failed = [ProbeResult {
            info: EncoderInfo { name: "ffmpeg hevc_vaapi".into(), codec: Codec::Hevc, accel: Accel::Vaapi },
            error: Some("no VA-API render device in /dev/dri".into()),
        }];
        let config = ffmpeg::FfmpegConfig::new("/nonexistent/ffmpeg");
        let mut live = open(&EncoderChoice::Auto { codec: Codec::Hevc }, &EncoderSettings::new(64, 48), Some(&config), &failed).unwrap();
        let frame = live.encode(&picture(64, 48, 90), 4, 42).unwrap().frames.remove(0);
        assert_eq!(frame.encoding, Encoding::Jpeg);
        assert_eq!(frame.input_seq, 4);
        assert!(frame.keyframe);
        crate::decode::FrameDecoder::new().apply(&frame, &mut crate::canvas::Canvas::new(64, 48)).unwrap();
    }

}
