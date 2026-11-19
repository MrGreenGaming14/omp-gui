//! Decoding H.264 and HEVC access units into RGBA pictures.
//!
//! H.264 decodes in process (`rusty_h264`). HEVC goes through an `ffmpeg`
//! process when one is installed: Annex B in, raw RGBA out, one picture per
//! access unit.

use crate::encode::{Codec, ffmpeg::FfmpegConfig, nal};
use crate::{BenchError, Result, yuv};
use loa_surface::{Encoding, Frame, Rect};
use rusty_h264::Decoder;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

/// How long the HEVC decoder may take for one picture. The first one also
/// waits for ffmpeg to start.
const HEVC_PICTURE_TIMEOUT: Duration = Duration::from_secs(3);

/// The HEVC access unit delimiter (type 35, any picture type). Sent after
/// each access unit so ffmpeg's parser hands the unit on without waiting
/// for the next one.
const HEVC_AUD: [u8; 7] = [0, 0, 0, 1, 0x46, 0x01, 0x50];

/// Stateful video decoder: references persist between access units of one
/// stream until [`reset`](Self::reset).
pub struct VideoDecoder {
    h264: Decoder,
    ffmpeg: Option<FfmpegConfig>,
    hevc: Option<HevcProcess>,
}

impl VideoDecoder {
    /// H.264 always; HEVC when `ffmpeg` is given.
    pub fn new(ffmpeg: Option<FfmpegConfig>) -> Self {
        Self { h264: Decoder::new(), ffmpeg, hevc: None }
    }

    pub fn supports(&self, encoding: Encoding) -> bool {
        match encoding {
            Encoding::H264 => true,
            Encoding::Hevc => self.ffmpeg.is_some(),
            Encoding::Rgba | Encoding::Jpeg | Encoding::Png => false,
        }
    }

    /// The video encodings this decoder takes, best first: in-process H.264
    /// has the least delay.
    pub fn encodings(&self) -> Vec<Encoding> {
        [Encoding::H264, Encoding::Hevc].into_iter().filter(|&e| self.supports(e)).collect()
    }

    /// Decodes one access unit into `out`, a tightly packed RGBA picture of
    /// `frame.width` by `frame.height`. `Ok(false)` when the unit carried no
    /// picture.
    pub fn decode(&mut self, frame: &Frame, out: &mut [u8]) -> Result<bool> {
        let full = Rect { x: 0, y: 0, width: frame.width, height: frame.height };
        if frame.region != full {
            return Err(BenchError::Invalid("video frames must cover the whole picture".into()));
        }
        if out.len() != frame.width as usize * frame.height as usize * 4 {
            return Err(BenchError::Invalid(format!(
                "{} bytes for a {}x{} RGBA picture",
                out.len(),
                frame.width,
                frame.height
            )));
        }
        match frame.encoding {
            Encoding::H264 => self.decode_h264(frame, out),
            Encoding::Hevc => self.decode_hevc(frame, out),
            other => Err(BenchError::NoDecoder(other)),
        }
    }

    fn decode_h264(&mut self, frame: &Frame, out: &mut [u8]) -> Result<bool> {
        let error = |message: String| BenchError::Decode { encoding: Encoding::H264, message };
        let Some(decoded) = self.h264.decode(&frame.data).map_err(|e| error(e.to_string()))? else {
            return Ok(false);
        };
        if decoded.width != frame.width as usize || decoded.height != frame.height as usize {
            let message = format!("picture is {}x{}, the frame {}x{}", decoded.width, decoded.height, frame.width, frame.height);
            self.h264.recycle_frame(decoded);
            return Err(error(message));
        }
        yuv::i420_to_rgba(
            yuv::I420 {
                width: decoded.width,
                height: decoded.height,
                y: &decoded.y,
                u: &decoded.u,
                v: &decoded.v,
                stride_y: decoded.width,
                stride_c: decoded.width / 2,
            },
            out,
        );
        self.h264.recycle_frame(decoded);
        Ok(true)
    }

    fn decode_hevc(&mut self, frame: &Frame, out: &mut [u8]) -> Result<bool> {
        let config = self.ffmpeg.as_ref().ok_or(BenchError::NoDecoder(Encoding::Hevc))?;
        if self.hevc.as_ref().is_some_and(|p| p.size != (frame.width, frame.height)) {
            self.hevc = None;
        }
        if self.hevc.is_none() {
            if !frame.keyframe {
                return Ok(false);
            }
            self.hevc = Some(HevcProcess::start(config, frame.width, frame.height)?);
        }
        let Some(process) = self.hevc.as_mut() else { return Ok(false) };
        let result = process.decode(&frame.data, out);
        if result.is_err() {
            self.hevc = None;
        }
        result.map(|()| true)
    }

    /// Forgets reference pictures; the next unit fed must be a keyframe.
    pub fn reset(&mut self) {
        self.h264 = Decoder::new();
        self.hevc = None;
    }
}

/// One `ffmpeg` HEVC decoder for pictures of one size.
struct HevcProcess {
    size: (u32, u32),
    child: Child,
    stdin: ChildStdin,
    pictures: Receiver<std::io::Result<Vec<u8>>>,
}

impl HevcProcess {
    fn start(config: &FfmpegConfig, width: u32, height: u32) -> Result<Self> {
        let error = |message: String| BenchError::Decode { encoding: Encoding::Hevc, message };
        let mut child = Command::new(&config.binary)
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-threads", "1"])
            .args(["-flags", "low_delay", "-probesize", "32", "-analyzeduration", "0", "-fflags", "nobuffer"])
            .args(["-f", "hevc", "-i", "pipe:0", "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| error(format!("starting {}: {e}", config.binary.display())))?;
        let stdin = child.stdin.take().ok_or_else(|| error("ffmpeg has no stdin".into()))?;
        let mut stdout = child.stdout.take().ok_or_else(|| error("ffmpeg has no stdout".into()))?;
        let len = width as usize * height as usize * 4;
        let (sender, pictures) = mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let mut picture = vec![0; len];
                let read = stdout.read_exact(&mut picture).map(|()| picture);
                let end = read.is_err();
                if sender.send(read).is_err() || end {
                    break;
                }
            }
        });
        Ok(Self { size: (width, height), child, stdin, pictures })
    }

    fn decode(&mut self, unit: &[u8], out: &mut [u8]) -> Result<()> {
        let error = |message: String| BenchError::Decode { encoding: Encoding::Hevc, message };
        // The unit's own leading delimiter is dropped: the previous unit was
        // already closed by the one written after it.
        let mut stream = Vec::with_capacity(unit.len() + HEVC_AUD.len() + 16);
        for nal_unit in nal::units(unit).filter(|u| nal::unit_type(Codec::Hevc, u) != Some(35)) {
            stream.extend_from_slice(&[0, 0, 0, 1]);
            stream.extend_from_slice(nal_unit);
        }
        stream.extend_from_slice(&HEVC_AUD);
        self.stdin.write_all(&stream).and_then(|()| self.stdin.flush()).map_err(|e| error(e.to_string()))?;
        match self.pictures.recv_timeout(HEVC_PICTURE_TIMEOUT) {
            Ok(Ok(picture)) => {
                out.copy_from_slice(&picture);
                Ok(())
            }
            Ok(Err(e)) => Err(error(format!("ffmpeg stopped: {e}"))),
            Err(RecvTimeoutError::Timeout) => Err(error("ffmpeg gave no picture in time".into())),
            Err(RecvTimeoutError::Disconnected) => Err(error("ffmpeg exited".into())),
        }
    }
}

impl Drop for HevcProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
