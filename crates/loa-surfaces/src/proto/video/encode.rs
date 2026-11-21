//! RGBA pictures to H.264/HEVC access units: the [`VideoEncoder`] port, the
//! in-process software H.264 encoder and [`LiveEncoder`], which skips
//! unchanged pictures and stamps frames.

use super::{ffmpeg, video_error};
use crate::proto::Result;
use bytes::Bytes;
use loa_contracts::surface::{Encoding, Frame, Rect};
use rusty_h264::{Encoder, EncoderConfig, YuvPlanes};
use std::ops::RangeInclusive;

/// Pictures per second every encoder is set up for.
pub(crate) const FPS: u32 = 60;
/// Frames between periodic keyframes; requested keyframes come on top.
pub(crate) const KEYFRAME_INTERVAL: u32 = 600;
/// Constant quantizer, 0 (best) to 51.
pub(crate) const QP: u8 = 26;

/// Video codec of an access unit stream.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
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

    pub(crate) fn nal(self) -> &'static Nal {
        match self {
            Self::H264 => &H264_NAL,
            Self::Hevc => &HEVC_NAL,
        }
    }
}

/// What a codec's NAL unit types mean.
pub(crate) struct Nal {
    /// The codec's name in ffmpeg.
    pub name: &'static str,
    /// Right shift and mask of the type in a NAL unit's first byte.
    shift: u8,
    mask: u8,
    /// Access unit delimiter.
    pub aud: u8,
    /// IDR (H.264) or IRAP (HEVC) slices.
    keyframe: RangeInclusive<u8>,
    /// Coded slices of any kind.
    picture: RangeInclusive<u8>,
    /// SPS and PPS, plus VPS for HEVC.
    parameter_sets: &'static [u8],
}

static H264_NAL: Nal =
    Nal { name: "h264", shift: 0, mask: 0x1f, aud: 9, keyframe: 5..=5, picture: 1..=5, parameter_sets: &[7, 8] };
static HEVC_NAL: Nal =
    Nal { name: "hevc", shift: 1, mask: 0x3f, aud: 35, keyframe: 16..=23, picture: 0..=31, parameter_sets: &[32, 33, 34] };

impl Nal {
    pub fn type_of(&self, header: u8) -> u8 {
        (header >> self.shift) & self.mask
    }
}

/// Where an encoder runs.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Accel {
    Vaapi,
    Nvenc,
    Qsv,
    V4l2M2m,
    Software,
}

impl Accel {
    pub fn is_hardware(self) -> bool {
        self != Self::Software
    }
}

/// What an encoder is, e.g. `ffmpeg h264_vaapi` or `rusty_h264`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderInfo {
    pub name: String,
    pub codec: Codec,
    pub accel: Accel,
}

/// Checks that `width`x`height` suits 4:2:0 video.
pub(crate) fn check_video_size(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) || width > 8192 || height > 8192 {
        return Err(video_error(format!("{width}x{height}: 4:2:0 video needs an even, nonzero size up to 8192")));
    }
    Ok(())
}

/// Checks that `rgba` holds a tight `width`x`height` picture.
pub(crate) fn check_picture(rgba: &[u8], width: u32, height: u32) -> Result<()> {
    if rgba.len() != width as usize * height as usize * 4 {
        return Err(video_error(format!("{} bytes for a {width}x{height} picture", rgba.len())));
    }
    Ok(())
}

/// One coded picture: an Annex B access unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessUnit {
    pub data: Bytes,
    /// An IDR (H.264) or IRAP (HEVC) picture with its parameter sets.
    pub keyframe: bool,
}

/// The encoder port: RGBA in, one access unit per picture out, no B-frames.
pub trait VideoEncoder: Send {
    fn encode(&mut self, rgba: &[u8], keyframe: bool) -> Result<AccessUnit>;
}

/// Codes a surface's pictures into frames: skips unchanged pictures and
/// honours keyframe requests.
pub struct LiveEncoder {
    size: (u32, u32),
    codec: Codec,
    encoder: Box<dyn VideoEncoder>,
    last: Vec<u8>,
    keyframe_pending: bool,
    next_seq: u64,
}

impl LiveEncoder {
    pub fn new(size: (u32, u32), codec: Codec, encoder: Box<dyn VideoEncoder>) -> Self {
        Self { size, codec, encoder, last: Vec::new(), keyframe_pending: true, next_seq: 1 }
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The next picture goes out whole and decodable on its own.
    pub fn request_keyframe(&mut self) {
        self.keyframe_pending = true;
    }

    /// Codes `rgba` (the encoder's size) into a frame stamped with
    /// `input_seq` and `captured_us`; `None` when nothing changed.
    pub fn encode(&mut self, rgba: &[u8], input_seq: u64, captured_us: u64) -> Result<Option<Frame>> {
        let (width, height) = self.size;
        check_picture(rgba, width, height)?;
        let keyframe = std::mem::take(&mut self.keyframe_pending);
        if !keyframe && self.last == rgba {
            return Ok(None);
        }
        self.last.clear();
        self.last.extend_from_slice(rgba);
        let unit = self.encoder.encode(rgba, keyframe)?;
        self.next_seq += 1;
        Ok(Some(Frame {
            seq: self.next_seq - 1,
            input_seq,
            captured_us,
            width,
            height,
            region: Rect { x: 0, y: 0, width, height },
            encoding: self.codec.encoding(),
            keyframe: unit.keyframe,
            data: unit.data,
        }))
    }
}

/// Outcome of trying one encoder at runtime; `error` says why it can't code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeResult {
    pub info: EncoderInfo,
    pub error: Option<String>,
}

impl ProbeResult {
    pub fn works(&self) -> bool {
        self.error.is_none()
    }
}

/// Every encoder this machine could use, each tried for real; the
/// in-process software encoder always comes last.
pub fn probe(ffmpeg: Option<&ffmpeg::FfmpegConfig>) -> Vec<ProbeResult> {
    let mut found = ffmpeg.map(ffmpeg::probe).unwrap_or_default();
    found.push(ProbeResult { info: software_info(), error: None });
    found
}

/// Start codes of an Annex B buffer: where each code starts and where its NAL unit starts.
pub(crate) fn start_codes(stream: &[u8]) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut from = 0;
    std::iter::from_fn(move || {
        let at = from + stream.get(from..)?.windows(3).position(|w| w == [0, 0, 1])?;
        from = at + 3;
        Some((if at > 0 && stream[at - 1] == 0 { at - 1 } else { at }, at + 3))
    })
}

/// NAL unit types of an access unit.
pub(crate) fn nal_types(codec: Codec, access_unit: &[u8]) -> impl Iterator<Item = u8> + '_ {
    let mut codes = start_codes(access_unit).peekable();
    std::iter::from_fn(move || {
        let (_, begin) = codes.next()?;
        let end = codes.peek().map_or(access_unit.len(), |&(code, _)| code);
        Some(access_unit[begin..end].iter().rposition(|b| *b != 0).map(|_| access_unit[begin]))
    })
    .flatten()
    .map(move |header| codec.nal().type_of(header))
}

/// Whether the access unit holds an H.264 IDR slice or an HEVC IRAP picture.
pub(crate) fn is_keyframe(codec: Codec, access_unit: &[u8]) -> bool {
    nal_types(codec, access_unit).any(|t| codec.nal().keyframe.contains(&t))
}

/// Whether the access unit holds a coded picture at all.
pub(crate) fn has_picture(codec: Codec, access_unit: &[u8]) -> bool {
    nal_types(codec, access_unit).any(|t| codec.nal().picture.contains(&t))
}

/// Whether the access unit carries its parameter sets.
pub(crate) fn has_parameter_sets(codec: Codec, access_unit: &[u8]) -> bool {
    let types: Vec<_> = nal_types(codec, access_unit).collect();
    codec.nal().parameter_sets.iter().all(|t| types.contains(t))
}

pub fn software_info() -> EncoderInfo {
    EncoderInfo { name: "rusty_h264".into(), codec: Codec::H264, accel: Accel::Software }
}

/// In-process Constrained Baseline H.264 for when no hardware encoder works.
pub struct SoftwareH264 {
    encoder: Encoder,
    width: usize,
    height: usize,
    planes: [Vec<u8>; 3],
    out: Vec<u8>,
}

impl SoftwareH264 {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        check_video_size(width, height)?;
        let (width, height) = (width as usize, height as usize);
        // Low latency: CAVLC, no B-frames, no lookahead, one reference.
        let mut config = EncoderConfig::baseline(width, height);
        config.gop_size = KEYFRAME_INTERVAL;
        config.min_keyint = 1;
        config.qp = QP;
        config.bitrate = 0;
        config.framerate = FPS as f32;
        // Smallest level whose frame size limit (in macroblocks) fits.
        config.level_idc = match width.div_ceil(16) * height.div_ceil(16) {
            0..=1620 => 30,
            1621..=3600 => 31,
            3601..=5120 => 32,
            5121..=8192 => 41,
            8193..=22080 => 51,
            _ => 52,
        };
        let encoder = Encoder::new(config).map_err(|e| video_error(e.to_string()))?;
        let luma = width * height;
        let planes = [vec![0; luma], vec![0; luma / 4], vec![0; luma / 4]];
        // An IDR at low QP can approach the raw 4:2:0 size.
        Ok(Self { encoder, width, height, planes, out: vec![0; luma * 3 / 2 + 4096] })
    }
}

impl VideoEncoder for SoftwareH264 {
    fn encode(&mut self, rgba: &[u8], keyframe: bool) -> Result<AccessUnit> {
        let (w, h) = (self.width, self.height);
        check_picture(rgba, w as u32, h as u32)?;
        let [y, u, v] = &mut self.planes;
        rgba_to_i420(rgba, w, h, y, u, v);
        let planes = YuvPlanes::new(w, h, y, u, v, w, w / 2).ok_or_else(|| video_error("planes don't match the picture"))?;
        if keyframe {
            self.encoder.request_keyframe();
        }
        let len = self.encoder.encode_into(&planes, &mut self.out).map_err(|e| video_error(e.to_string()))?;
        let data = &self.out[..len];
        Ok(AccessUnit { keyframe: is_keyframe(Codec::H264, data), data: Bytes::copy_from_slice(data) })
    }
}

/// Tightly packed RGBA to I420 (BT.601, limited range); even sizes only.
fn rgba_to_i420(rgba: &[u8], width: usize, height: usize, y: &mut [u8], u: &mut [u8], v: &mut [u8]) {
    for (px, out) in rgba.as_chunks::<4>().0.iter().zip(y.iter_mut()) {
        let (r, g, b) = (i32::from(px[0]), i32::from(px[1]), i32::from(px[2]));
        *out = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
    }
    let cw = width / 2;
    for crow in 0..height / 2 {
        for cx in 0..cw {
            let mut sum = [0i32; 3];
            for row in [2 * crow, 2 * crow + 1] {
                let at = (row * width + 2 * cx) * 4;
                for px in rgba[at..at + 8].as_chunks::<4>().0 {
                    for (s, &c) in sum.iter_mut().zip(px) {
                        *s += i32::from(c);
                    }
                }
            }
            let [r, g, b] = sum.map(|s| (s + 2) >> 2);
            u[crow * cw + cx] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8;
            v[crow * cw + cx] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pixels::RgbaImage as Canvas;
    use crate::proto::video::decode::FrameDecoder;

    #[test]
    fn annex_b_parsing_and_software_h264() {
        let au = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0x88, 0x84];
        assert_eq!(nal_types(Codec::H264, &au).collect::<Vec<_>>(), vec![7, 8, 5]);
        assert!(is_keyframe(Codec::H264, &au) && has_parameter_sets(Codec::H264, &au));
        let p_slice = [0, 0, 0, 1, 0x41, 0x9a, 0x02];
        assert!(!is_keyframe(Codec::H264, &p_slice) && has_picture(Codec::H264, &p_slice));
        assert!(is_keyframe(Codec::Hevc, &[0, 0, 0, 1, 0x40, 0x01, 0x0c, 0, 0, 0, 1, 0x26, 0x01, 0xaf]));
        assert!(!is_keyframe(Codec::Hevc, &[0, 0, 0, 1, 0x02, 0x01, 0xd0]));
        let aud_split = [0, 0, 0, 1, 9, 0xf0, 0, 0, 1, 0x65, 1, 0, 0, 0, 1, 9, 0xf0];
        assert_eq!(ffmpeg::next_access_unit(Codec::H264, &aud_split), Some(11));

        // Round trip, unchanged pictures skipped, keyframes on request.
        let (w, h) = (96u32, 64u32);
        assert!(SoftwareH264::new(63, 64).is_err());
        let mut live = LiveEncoder::new((w, h), Codec::H264, Box::new(SoftwareH264::new(w, h).unwrap()));
        let (mut decoder, mut canvas) = (FrameDecoder::new(), Canvas::new(w, h));
        let mut rgba = vec![30u8; (w * h * 4) as usize];
        let first = live.encode(&rgba, 1, 10).unwrap().unwrap();
        assert!(first.keyframe && first.encoding == Encoding::H264);
        decoder.apply(&first, &mut canvas).unwrap();
        assert!(live.encode(&rgba, 1, 11).unwrap().is_none());
        rgba[..w as usize * 16 * 4].fill(230);
        let second = live.encode(&rgba, 2, 20).unwrap().unwrap();
        assert!(!second.keyframe && second.input_seq == 2);
        decoder.apply(&second, &mut canvas).unwrap();
        assert!(canvas.data[0].abs_diff(230) <= 4 && canvas.data[rgba.len() - 4].abs_diff(30) <= 4);
        live.request_keyframe();
        let third = live.encode(&rgba, 2, 30).unwrap().unwrap();
        assert!(third.keyframe);
        FrameDecoder::new().apply(&third, &mut Canvas::new(w, h)).unwrap();
    }
}
