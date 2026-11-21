//! Low-latency ffmpeg hardware encoders, selected by actually coding pictures.
//!
//! Raw Annex B output is not packet framed. An AUD is inserted for every
//! picture; a short quiet period after receiving a complete picture allows
//! returning it without waiting for the *next* input frame.

use super::encode::{
    AccessUnit, Accel, Codec, EncoderInfo, FPS, KEYFRAME_INTERVAL, ProbeResult, QP, VideoEncoder, check_picture,
    check_video_size, has_parameter_sets, has_picture, is_keyframe, start_codes,
};
use super::video_error;
use crate::proto::{ProtoError, Result};
use bytes::Bytes;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

pub const NAME_PREFIX: &str = "ffmpeg ";

/// The hardware encoders tried, by ffmpeg name.
const KNOWN: [(&str, Codec, Accel); 8] = [
    ("h264_vaapi", Codec::H264, Accel::Vaapi),
    ("hevc_vaapi", Codec::Hevc, Accel::Vaapi),
    ("h264_nvenc", Codec::H264, Accel::Nvenc),
    ("hevc_nvenc", Codec::Hevc, Accel::Nvenc),
    ("h264_qsv", Codec::H264, Accel::Qsv),
    ("hevc_qsv", Codec::Hevc, Accel::Qsv),
    ("h264_v4l2m2m", Codec::H264, Accel::V4l2M2m),
    ("hevc_v4l2m2m", Codec::Hevc, Accel::V4l2M2m),
];

/// A picture still incomplete this long after its last bytes is returned as is.
const QUIET: Duration = Duration::from_millis(4);

#[derive(Clone, Debug)]
pub struct FfmpegConfig {
    pub binary: PathBuf,
}

impl FfmpegConfig {
    /// `ffmpeg` on `$PATH`.
    pub fn discover() -> Option<Self> {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join("ffmpeg"))
            .find(|path| loa_util::exec::is_executable(path))
            .map(|binary| Self { binary })
    }
}

fn info((name, codec, accel): (&str, Codec, Accel)) -> EncoderInfo {
    EncoderInfo { name: format!("{NAME_PREFIX}{name}"), codec, accel }
}

/// Tries two pictures, not just `ffmpeg -encoders`: drivers and devices may be
/// absent even though the binary advertises the encoder. Every encoder is
/// tried at once; one that never answers costs its timeout only once.
pub fn probe(config: &FfmpegConfig) -> Vec<ProbeResult> {
    // HEVC hardware encoders refuse pictures under 128 pixels on a side.
    const SIZE: u32 = 256;
    let try_one = |name: &str, codec: Codec| -> Result<()> {
        let mut encoder = FfmpegEncoder::spawn(config, name, (SIZE, SIZE))?;
        let picture = |value: u8| vec![value; (SIZE * SIZE * 4) as usize];
        let initial = encoder.encode(&picture(64), false)?;
        if !initial.keyframe || !has_parameter_sets(codec, &initial.data) {
            return Err(video_error("first picture lacks a keyframe or parameter sets"));
        }
        if !has_picture(codec, &encoder.encode(&picture(128), false)?.data) {
            return Err(video_error("second picture has no coded slice"));
        }
        Ok(())
    };
    thread::scope(|scope| {
        let tried: Vec<_> = KNOWN.iter().map(|&(name, codec, _)| scope.spawn(move || try_one(name, codec))).collect();
        let errors = tried.into_iter().map(|tried| match tried.join() {
            Ok(result) => result.err().map(|error| error.to_string()),
            Err(_) => Some("the probe panicked".to_owned()),
        });
        KNOWN.into_iter().zip(errors).map(|(known, error)| ProbeResult { info: info(known), error }).collect()
    })
}

/// One running ffmpeg; dropping it kills the process.
struct Process {
    child: Child,
    stdin: ChildStdin,
    output: Receiver<std::io::Result<Vec<u8>>>,
}

impl Process {
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct FfmpegEncoder {
    binary: PathBuf,
    name: &'static str,
    codec: Codec,
    accel: Accel,
    size: (u32, u32),
    process: Process,
    pending: Vec<u8>,
    frames: u64,
}

impl FfmpegEncoder {
    pub fn spawn(config: &FfmpegConfig, name: &str, size: (u32, u32)) -> Result<Self> {
        check_video_size(size.0, size.1)?;
        let &(name, codec, accel) = KNOWN
            .iter()
            .find(|known| known.0 == name)
            .ok_or_else(|| video_error(format!("unknown ffmpeg encoder {name}")))?;
        let process = start(&config.binary, name, codec, accel, size)?;
        Ok(Self { binary: config.binary.clone(), name, codec, accel, size, process, pending: Vec::new(), frames: 0 })
    }

    fn error(&self, what: impl std::fmt::Display) -> ProtoError {
        video_error(format!("{}: {what}", self.name))
    }

    fn read_picture(&mut self) -> Result<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut last_data = Instant::now();
        loop {
            if let Some(boundary) = next_access_unit(self.codec, &self.pending) {
                return Ok(self.pending.drain(..boundary).collect());
            }
            let picture = has_picture(self.codec, &self.pending);
            if picture && last_data.elapsed() >= QUIET {
                return Ok(std::mem::take(&mut self.pending));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(self.error("timed out waiting for a coded picture"));
            }
            let wait = if picture { QUIET.saturating_sub(last_data.elapsed()) } else { deadline - now };
            match self.process.output.recv_timeout(wait.max(Duration::from_micros(100))) {
                Ok(Ok(chunk)) => {
                    self.pending.extend_from_slice(&chunk);
                    last_data = Instant::now();
                }
                Ok(Err(error)) => return Err(self.error(error)),
                Err(RecvTimeoutError::Disconnected) => return Err(self.error("ffmpeg exited before producing a picture")),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

impl VideoEncoder for FfmpegEncoder {
    fn encode(&mut self, rgba: &[u8], keyframe: bool) -> Result<AccessUnit> {
        check_picture(rgba, self.size.0, self.size.1)?;
        // ffmpeg's rawvideo pipe offers no per-picture force-IDR control.
        // A fresh process gives an independently decodable IDR/IRAP with new
        // parameter sets, without forcing *all* subsequent frames to be intra.
        if keyframe && self.frames != 0 {
            // Stopped first: a device may take one session at a time.
            self.process.stop();
            self.process = start(&self.binary, self.name, self.codec, self.accel, self.size)?;
            self.frames = 0;
        }
        self.process.stdin.write_all(rgba).map_err(|error| self.error(error))?;
        let data = self.read_picture()?;
        self.frames += 1;
        let keyframe = is_keyframe(self.codec, &data) && has_parameter_sets(self.codec, &data);
        Ok(AccessUnit { data: Bytes::from(data), keyframe })
    }
}

fn start(binary: &Path, name: &str, codec: Codec, accel: Accel, (width, height): (u32, u32)) -> Result<Process> {
    let mut command = Command::new(binary);
    command
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(["-f", "rawvideo", "-pixel_format", "rgba", "-video_size", &format!("{width}x{height}")])
        .args(["-framerate", &FPS.to_string(), "-i", "pipe:0"]);
    if accel == Accel::Vaapi {
        let device = std::fs::read_dir("/dev/dri").ok().and_then(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .find(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("renderD")))
        });
        command.arg("-vaapi_device").arg(device.ok_or_else(|| video_error("no VA-API render device in /dev/dri"))?);
    }
    // Default parallelism (VA-API, QSV) holds each picture back until the
    // next one comes in: a frame of delay on a live surface.
    command.args(match accel {
        Accel::Vaapi => &["-vf", "format=nv12,hwupload", "-async_depth", "1"][..],
        Accel::Qsv => &["-vf", "format=nv12", "-async_depth", "1"],
        Accel::Nvenc => &["-delay", "0", "-zerolatency", "1"],
        Accel::V4l2M2m => &["-vf", "format=yuv420p"],
        Accel::Software => &[],
    });
    // Every picture goes out as it comes, never held to pace a frame rate.
    command.args(["-fps_mode", "passthrough"]);
    // The renderer decodes H.264 in process, which takes Constrained
    // Baseline (CAVLC, no B-frames) only.
    if codec == Codec::H264 {
        command.args(match accel {
            Accel::Vaapi => &["-profile:v", "constrained_baseline"][..],
            Accel::Nvenc | Accel::Qsv => &["-profile:v", "baseline"],
            Accel::V4l2M2m | Accel::Software => &[],
        });
    }
    let format = codec.nal().name;
    let bitstream_filter = format!("{format}_metadata=aud=insert");
    command
        .args(["-an", "-c:v", name, "-g", &KEYFRAME_INTERVAL.to_string(), "-bf", "0", "-q:v", &QP.to_string()])
        .args(["-flags", "+low_delay", "-bsf:v", &bitstream_filter, "-flush_packets", "1", "-f", format, "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|error| video_error(format!("starting {name}: {error}")))?;
    let stdin = child.stdin.take().ok_or_else(|| video_error("ffmpeg has no stdin"))?;
    let mut stdout = child.stdout.take().ok_or_else(|| video_error("ffmpeg has no stdout"))?;
    let (sender, output) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0u8; 65536];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => break,
                Ok(len) if sender.send(Ok(buffer[..len].to_vec())).is_ok() => {}
                Ok(_) => break,
                Err(error) => {
                    let _ = sender.send(Err(error));
                    break;
                }
            }
        }
    });
    Ok(Process { child, stdin, output })
}

/// Beginning of the second picture's AUD, if already present in the buffer.
pub(crate) fn next_access_unit(codec: Codec, data: &[u8]) -> Option<usize> {
    let nal = codec.nal();
    start_codes(data)
        .filter(|&(_, at)| data.get(at).is_some_and(|&header| nal.type_of(header) == nal.aud))
        .nth(1)
        .map(|(code, _)| code)
}
