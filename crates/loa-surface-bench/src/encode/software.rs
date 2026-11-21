//! In-process software H.264: Constrained Baseline through `rusty_h264`,
//! pure Rust, one access unit per picture, nothing buffered.

use super::{Accel, AccessUnit, Codec, EncoderInfo, EncoderSettings, VideoEncoder, nal};
use crate::{BenchError, Result, yuv};
use bytes::Bytes;
use rusty_h264::{Encoder, EncoderConfig, YuvPlanes};

pub fn info() -> EncoderInfo {
    EncoderInfo { name: "rusty_h264".into(), codec: Codec::H264, accel: Accel::Software }
}

/// Software H.264 for when no hardware encoder works.
pub struct SoftwareH264 {
    encoder: Encoder,
    width: usize,
    height: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    out: Vec<u8>,
}

impl SoftwareH264 {
    pub fn new(settings: &EncoderSettings) -> Result<Self> {
        settings.check_video_size()?;
        let (width, height) = (settings.width as usize, settings.height as usize);
        // The low latency configuration: Constrained Baseline, CAVLC, no
        // B-frames, no lookahead, no scene cut detection, one reference.
        let mut config = EncoderConfig::baseline(width, height);
        config.gop_size = settings.keyframe_interval.max(1);
        config.min_keyint = 1;
        config.qp = settings.qp.min(51);
        config.bitrate = 0;
        config.framerate = settings.fps.max(1) as f32;
        config.level_idc = level_for(width, height);
        let encoder = Encoder::new(config).map_err(|e| BenchError::Encode(e.to_string()))?;
        let luma = width * height;
        Ok(Self {
            encoder,
            width,
            height,
            y: vec![0; luma],
            u: vec![0; luma / 4],
            v: vec![0; luma / 4],
            // An IDR at low QP can approach the raw 4:2:0 size.
            out: vec![0; luma * 3 / 2 + 4096],
        })
    }
}

/// Smallest H.264 level whose frame size limit (in macroblocks) fits.
fn level_for(width: usize, height: usize) -> u8 {
    let mbs = width.div_ceil(16) * height.div_ceil(16);
    match mbs {
        0..=1620 => 30,
        1621..=3600 => 31,
        3601..=5120 => 32,
        5121..=8192 => 41,
        8193..=22080 => 51,
        _ => 52,
    }
}

impl VideoEncoder for SoftwareH264 {
    fn info(&self) -> EncoderInfo {
        info()
    }

    fn encode(&mut self, rgba: &[u8], keyframe: bool) -> Result<AccessUnit> {
        if rgba.len() != self.width * self.height * 4 {
            return Err(BenchError::Encode(format!(
                "{} bytes for a {}x{} picture",
                rgba.len(),
                self.width,
                self.height
            )));
        }
        yuv::rgba_to_i420(rgba, self.width, self.height, &mut self.y, &mut self.u, &mut self.v);
        let planes = YuvPlanes::new(self.width, self.height, &self.y, &self.u, &self.v, self.width, self.width / 2)
            .ok_or_else(|| BenchError::Encode("planes don't match the picture size".into()))?;
        if keyframe {
            self.encoder.request_keyframe();
        }
        let len = self
            .encoder
            .encode_into(&planes, &mut self.out)
            .map_err(|e| BenchError::Encode(e.to_string()))?;
        let data = &self.out[..len];
        Ok(AccessUnit { keyframe: nal::is_keyframe(Codec::H264, data), data: Bytes::copy_from_slice(data) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::FrameDecoder;
    use loa_contracts::surface::{Encoding, Frame, Rect};

    fn frame(unit: &AccessUnit, width: u32, height: u32) -> Frame {
        Frame {
            seq: 1,
            captured_us: 0,
            input_seq: 0,
            width,
            height,
            region: Rect { x: 0, y: 0, width, height },
            encoding: Encoding::H264,
            keyframe: unit.keyframe,
            data: unit.data.clone(),
        }
    }

    #[test]
    fn a_painted_square_survives_encode_and_decode_and_keyframes_come_on_request() {
        let (w, h) = (96u32, 64u32);
        let mut encoder = SoftwareH264::new(&EncoderSettings::new(w, h)).unwrap();
        let mut decoder = FrameDecoder::new();
        let mut canvas = crate::canvas::Canvas::new(w, h);
        let mut rgba = vec![30u8; (w * h * 4) as usize];

        let first = encoder.encode(&rgba, true).unwrap();
        assert!(first.keyframe);
        decoder.apply(&frame(&first, w, h), &mut canvas).unwrap();

        // Paint a bright square at (48..64, 16..32).
        let square = Rect { x: 48, y: 16, width: 16, height: 16 };
        let before = canvas.crop(square);
        for y in 16..32 {
            for x in 48..64 {
                let at = ((y * w + x) * 4) as usize;
                rgba[at..at + 4].copy_from_slice(&[230, 230, 230, 255]);
            }
        }
        let second = encoder.encode(&rgba, false).unwrap();
        assert!(!second.keyframe, "P-frames between keyframes");
        decoder.apply(&frame(&second, w, h), &mut canvas).unwrap();
        assert!(canvas.mean_abs_diff(square, &before) > 150.0);
        let untouched = Rect { x: 0, y: 0, width: 16, height: 16 };
        assert!(canvas.crop(untouched).as_chunks::<4>().0.iter().all(|p| p[0].abs_diff(30) <= 4));

        let third = encoder.encode(&rgba, true).unwrap();
        assert!(third.keyframe);
        // A fresh decoder joins at the requested keyframe.
        let mut late = FrameDecoder::new();
        let mut late_canvas = crate::canvas::Canvas::new(w, h);
        late.apply(&frame(&third, w, h), &mut late_canvas).unwrap();
        assert!(late_canvas.mean_abs_diff(square, &before) > 150.0);
    }

    #[test]
    fn odd_sizes_are_refused() {
        assert!(matches!(SoftwareH264::new(&EncoderSettings::new(63, 64)), Err(BenchError::Size { .. })));
    }
}
