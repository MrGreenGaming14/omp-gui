//! Low-latency ffmpeg hardware encoders, selected by actually coding pictures.
//!
//! Raw Annex B output is not packet framed. An AUD is inserted for every
//! picture; a short quiet period after receiving a complete picture allows
//! returning it without waiting for the *next* input frame.

use super::{Accel, AccessUnit, Codec, EncoderInfo, EncoderSettings, ProbeResult, VideoEncoder, nal};
use crate::{BenchError, Result};
use bytes::Bytes;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

pub const NAME_PREFIX: &str = "ffmpeg ";

#[derive(Copy, Clone)]
pub struct KnownEncoder {
    pub name: &'static str,
    pub codec: Codec,
    pub accel: Accel,
}

pub const KNOWN: &[KnownEncoder] = &[
    KnownEncoder { name: "h264_vaapi", codec: Codec::H264, accel: Accel::Vaapi },
    KnownEncoder { name: "hevc_vaapi", codec: Codec::Hevc, accel: Accel::Vaapi },
    KnownEncoder { name: "h264_nvenc", codec: Codec::H264, accel: Accel::Nvenc },
    KnownEncoder { name: "hevc_nvenc", codec: Codec::Hevc, accel: Accel::Nvenc },
    KnownEncoder { name: "h264_qsv", codec: Codec::H264, accel: Accel::Qsv },
    KnownEncoder { name: "hevc_qsv", codec: Codec::Hevc, accel: Accel::Qsv },
    KnownEncoder { name: "h264_v4l2m2m", codec: Codec::H264, accel: Accel::V4l2M2m },
    KnownEncoder { name: "hevc_v4l2m2m", codec: Codec::Hevc, accel: Accel::V4l2M2m },
];

#[derive(Clone, Debug)]
pub struct FfmpegConfig {
    pub binary: PathBuf,
}

impl FfmpegConfig {
    pub fn new(binary: impl Into<PathBuf>) -> Self { Self { binary: binary.into() } }

    /// `ffmpeg` on the search path in `path_var` (a `$PATH` value).
    pub fn find(path_var: Option<&std::ffi::OsStr>) -> Option<Self> {
        use std::os::unix::fs::PermissionsExt;
        std::env::split_paths(path_var?)
            .map(|dir| dir.join("ffmpeg"))
            .find(|path| std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
            .map(Self::new)
    }

    /// `ffmpeg` on `$PATH`.
    pub fn discover() -> Option<Self> {
        Self::find(std::env::var_os("PATH").as_deref())
    }
}

/// Tries two pictures, not just `ffmpeg -encoders`: drivers and devices may be
/// absent even though the binary advertises the encoder.
pub fn probe(config: &FfmpegConfig) -> Vec<ProbeResult> {
    // HEVC hardware encoders refuse pictures under 128 pixels on a side.
    let settings = EncoderSettings::new(256, 256);
    KNOWN.iter().map(|known| {
        let info = EncoderInfo { name: format!("{NAME_PREFIX}{}", known.name), codec: known.codec, accel: known.accel };
        let error = (|| -> Result<()> {
            let mut encoder = FfmpegEncoder::spawn(config, known.name, &settings)?;
            let first = vec![64u8; settings.frame_len()];
            let second = vec![128u8; settings.frame_len()];
            let initial = encoder.encode(&first, false)?;
            if !initial.keyframe || !has_parameter_sets(known.codec, &initial.data) {
                return Err(BenchError::Encode("first picture lacks a keyframe or parameter sets".into()));
            }
            let following = encoder.encode(&second, false)?;
            if !nal::has_picture(known.codec, &following.data) {
                return Err(BenchError::Encode("second picture has no coded slice".into()));
            }
            Ok(())
        })().err().map(|error| error.to_string());
        ProbeResult { info, error }
    }).collect()
}

fn has_parameter_sets(codec: Codec, data: &[u8]) -> bool {
    let types: Vec<_> = nal::units(data).filter_map(|unit| nal::unit_type(codec, unit)).collect();
    match codec {
        Codec::H264 => types.contains(&7) && types.contains(&8),
        Codec::Hevc => types.contains(&32) && types.contains(&33) && types.contains(&34),
    }
}

pub struct FfmpegEncoder {
    config: FfmpegConfig,
    known: KnownEncoder,
    settings: EncoderSettings,
    child: Child,
    stdin: ChildStdin,
    output: Receiver<std::io::Result<Vec<u8>>>,
    pending: Vec<u8>,
    frames: u64,
}

impl FfmpegEncoder {
    pub fn spawn(config: &FfmpegConfig, name: &str, settings: &EncoderSettings) -> Result<Self> {
        settings.check_video_size()?;
        let known = *KNOWN.iter().find(|known| known.name == name)
            .ok_or_else(|| BenchError::Encode(format!("unknown ffmpeg encoder {name}")))?;
        let (child, stdin, output) = start(&config.binary, known, settings)?;
        Ok(Self { config: config.clone(), known, settings: settings.clone(), child, stdin, output, pending: Vec::new(), frames: 0 })
    }

    fn restart(&mut self) -> Result<()> {
        self.stop();
        let (child, stdin, output) = start(&self.config.binary, self.known, &self.settings)?;
        self.child = child;
        self.stdin = stdin;
        self.output = output;
        self.frames = 0;
        Ok(())
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn read_picture(&mut self) -> Result<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut last_data = Instant::now();
        loop {
            if let Some(boundary) = next_access_unit(self.known.codec, &self.pending) {
                return Ok(self.pending.drain(..boundary).collect());
            }
            if nal::has_picture(self.known.codec, &self.pending)
                && last_data.elapsed() >= Duration::from_millis(4)
            {
                return Ok(std::mem::take(&mut self.pending));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(BenchError::Encode(format!("{}: timed out waiting for a coded picture", self.known.name)));
            }
            let wait = if nal::has_picture(self.known.codec, &self.pending) {
                Duration::from_millis(4).saturating_sub(last_data.elapsed())
            } else {
                deadline - now
            };
            match self.output.recv_timeout(wait.max(Duration::from_micros(100))) {
                Ok(Ok(chunk)) => { self.pending.extend_from_slice(&chunk); last_data = Instant::now(); }
                Ok(Err(error)) => return Err(BenchError::Encode(format!("{}: {error}", self.known.name))),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(BenchError::Encode(format!("{}: ffmpeg exited before producing a picture", self.known.name)));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

impl VideoEncoder for FfmpegEncoder {
    fn info(&self) -> EncoderInfo {
        EncoderInfo { name: format!("{NAME_PREFIX}{}", self.known.name), codec: self.known.codec, accel: self.known.accel }
    }

    fn encode(&mut self, rgba: &[u8], keyframe: bool) -> Result<AccessUnit> {
        if rgba.len() != self.settings.frame_len() {
            return Err(BenchError::Encode(format!("{} bytes for a {}x{} picture", rgba.len(), self.settings.width, self.settings.height)));
        }
        // ffmpeg's rawvideo pipe offers no per-picture force-IDR control.
        // A fresh process gives an independently decodable IDR/IRAP with new
        // parameter sets, without forcing *all* subsequent frames to be intra.
        if keyframe && self.frames != 0 { self.restart()?; }
        self.stdin.write_all(rgba).map_err(|error| BenchError::Encode(format!("{}: {error}", self.known.name)))?;
        let data = self.read_picture()?;
        self.frames += 1;
        let keyframe = nal::is_keyframe(self.known.codec, &data) && has_parameter_sets(self.known.codec, &data);
        Ok(AccessUnit { data: Bytes::from(data), keyframe })
    }
}

impl Drop for FfmpegEncoder {
    fn drop(&mut self) { self.stop(); }
}

type FfmpegProcess = (Child, ChildStdin, Receiver<std::io::Result<Vec<u8>>>);

fn start(binary: &Path, known: KnownEncoder, settings: &EncoderSettings)
    -> Result<FfmpegProcess>
{
    let size = format!("{}x{}", settings.width, settings.height);
    let mut command = Command::new(binary);
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(["-f", "rawvideo", "-pixel_format", "rgba", "-video_size", &size])
        .args(["-framerate", &settings.fps.max(1).to_string(), "-i", "pipe:0"]);
    if known.accel == Accel::Vaapi {
        let device = std::fs::read_dir("/dev/dri")
            .ok().and_then(|entries| entries.flatten().map(|entry| entry.path())
                .find(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("renderD"))));
        let device = device.ok_or_else(|| BenchError::Encode("no VA-API render device in /dev/dri".into()))?;
        command.arg("-vaapi_device").arg(device)
            .args(["-vf", "format=nv12,hwupload"]);
        // Default parallelism holds each picture back until the next one
        // comes in: a frame of delay on a live surface.
        command.args(["-async_depth", "1"]);
    } else if known.accel == Accel::Qsv {
        command.args(["-vf", "format=nv12", "-async_depth", "1"]);
    } else if known.accel == Accel::Nvenc {
        command.args(["-delay", "0", "-zerolatency", "1"]);
    } else if known.accel == Accel::V4l2M2m {
        command.args(["-vf", "format=yuv420p"]);
    }
    // Every picture goes out as it comes, never held to pace a frame rate.
    command.args(["-fps_mode", "passthrough"]);
    // The renderer decodes H.264 in process, which takes Constrained
    // Baseline (CAVLC, no B-frames) only.
    if known.codec == Codec::H264 {
        match known.accel {
            Accel::Vaapi => command.args(["-profile:v", "constrained_baseline"]),
            Accel::Nvenc | Accel::Qsv => command.args(["-profile:v", "baseline"]),
            Accel::V4l2M2m | Accel::Software => &mut command,
        };
    }
    let bitstream_filter = if known.codec == Codec::H264 { "h264_metadata=aud=insert" } else { "hevc_metadata=aud=insert" };
    let format = if known.codec == Codec::H264 { "h264" } else { "hevc" };
    command.args(["-an", "-c:v", known.name, "-g", &settings.keyframe_interval.max(1).to_string(),
        "-bf", "0", "-q:v", &settings.qp.to_string(), "-flags", "+low_delay",
        "-bsf:v", bitstream_filter, "-flush_packets", "1", "-f", format, "pipe:1"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = command.spawn().map_err(|error| BenchError::Encode(format!("starting {}: {error}", known.name)))?;
    let stdin = child.stdin.take().ok_or_else(|| BenchError::Encode("ffmpeg has no stdin".into()))?;
    let mut stdout = child.stdout.take().ok_or_else(|| BenchError::Encode("ffmpeg has no stdout".into()))?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0u8; 65536];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => break,
                Ok(len) => if sender.send(Ok(buffer[..len].to_vec())).is_err() { break; },
                Err(error) => { let _ = sender.send(Err(error)); break; }
            }
        }
    });
    Ok((child, stdin, receiver))
}

/// Beginning of the second picture's AUD, if already present in the buffer.
fn next_access_unit(codec: Codec, data: &[u8]) -> Option<usize> {
    let mut first = false;
    let mut index = 0;
    while index + 3 < data.len() {
        let code_len = if data[index..].starts_with(&[0, 0, 0, 1]) { 4 }
            else if data[index..].starts_with(&[0, 0, 1]) { 3 }
            else {
                index += 1;
                continue;
            };
        let unit = &data[index + code_len..];
        let is_aud = match codec {
            Codec::H264 => unit.first().is_some_and(|byte| byte & 0x1f == 9),
            Codec::Hevc => unit.first().is_some_and(|byte| (byte >> 1) & 0x3f == 35),
        };
        if is_aud {
            if first { return Some(index); }
            first = true;
        }
        index += code_len + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aud_delimits_annex_b_pictures() {
        let stream = [0, 0, 0, 1, 9, 0xf0, 0, 0, 1, 0x65, 1, 0, 0, 0, 1, 9, 0xf0];
        assert_eq!(next_access_unit(Codec::H264, &stream), Some(11));
    }
}
