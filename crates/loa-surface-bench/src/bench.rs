//! Input-to-decoded-pixels measurement for any pixel surface.

use crate::{canvas::Canvas, decode::FrameDecoder, BenchError, Result};
use loa_surface::{Frame, Input, InputEvent, Modifiers, MouseButton, Rect, Surface};
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::RecvError;

#[derive(Clone, Debug)]
pub struct BenchConfig {
    pub iterations: usize,
    pub timeout: Duration,
    pub baseline_timeout: Duration,
    pub click_x: f32,
    pub click_y: f32,
    /// Region expected to change after a click. None compares the entire picture.
    pub region: Option<Rect>,
    /// Minimum mean absolute RGB difference from the preceding picture.
    pub change_threshold: f64,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            iterations: 20,
            timeout: Duration::from_millis(500),
            baseline_timeout: Duration::from_secs(2),
            click_x: 1.0,
            click_y: 1.0,
            region: None,
            change_threshold: 2.0,
        }
    }
}

/// Server-side measurements are optional: arbitrary surfaces need only implement Surface.
#[derive(Clone, Copy, Debug, Default)]
pub struct StageTimings {
    /// From accepting input until the new picture is rendered (including scheduling).
    pub render: Duration,
    /// From the rendered picture until encoded frames are ready (including scheduling).
    pub encode: Duration,
}

pub trait StageTimingSource: Send + Sync {
    /// Returns and consumes the timing for the given input, if the surface exposes it.
    fn take_stage_timings(&self, input_seq: u64) -> Option<StageTimings>;
}

#[derive(Clone, Debug)]
pub struct Sample {
    pub input_seq: u64,
    pub frame_seq: u64,
    /// From issuing the input to receiving and decoding the first changed picture.
    pub latency: Duration,
    pub decode: Duration,
    pub pixel_difference: f64,
    pub stages: Option<StageTimings>,
}

#[derive(Clone, Debug)]
pub struct BenchResult {
    pub baseline_seq: u64,
    pub samples: Vec<Sample>,
}

/// An input only counts when a newer frame echoes its sequence AND changes decoded RGB.
/// The receiver is subscribed before requesting a baseline keyframe; lag forces another
/// keyframe because video delta frames cannot safely be decoded after a gap.
pub async fn run(
    surface: &dyn Surface,
    config: &BenchConfig,
    stages: Option<&dyn StageTimingSource>,
) -> Result<BenchResult> {
    if config.iterations == 0 || config.timeout.is_zero() || config.baseline_timeout.is_zero()
        || !config.change_threshold.is_finite() || config.change_threshold < 0.0
        || !config.click_x.is_finite() || !config.click_y.is_finite()
    {
        return Err(BenchError::Invalid("invalid benchmark configuration".into()));
    }
    let info = surface.info();
    if info.width == 0 || info.height == 0 || config.click_x < 0.0 || config.click_y < 0.0
        || config.click_x >= info.width as f32 || config.click_y >= info.height as f32
    {
        return Err(BenchError::Invalid("click outside the surface".into()));
    }
    let mut frames = surface.frames()?;
    let epoch = Instant::now();
    surface.request_keyframe().await?;
    let mut decoder = FrameDecoder::new();
    let mut canvas = Canvas::default();
    let mut last_seq = 0;
    let baseline_deadline = Instant::now() + config.baseline_timeout;
    loop {
        let frame = match tokio::time::timeout_at(baseline_deadline.into(), frames.recv()).await {
            Ok(Ok(frame)) => frame,
            Ok(Err(RecvError::Lagged(_))) => {
                decoder = FrameDecoder::new();
                surface.request_keyframe().await?;
                continue;
            }
            Ok(Err(RecvError::Closed)) => return Err(BenchError::StreamClosed),
            Err(_) => return Err(BenchError::NoFirstFrame(config.baseline_timeout.as_millis() as u64)),
        };
        if frame.seq <= last_seq || !is_full_keyframe(&frame) { continue; }
        decoder.apply(&frame, &mut canvas)?;
        last_seq = frame.seq;
        break;
    }
    let mut awaiting_keyframe = false;
    let baseline_seq = last_seq;
    let mut region = config.region.unwrap_or(Rect {
        x: 0, y: 0, width: canvas.width(), height: canvas.height(),
    });
    if region.width == 0 || region.height == 0 || !canvas.contains(region) {
        return Err(BenchError::Invalid("comparison region outside the baseline frame".into()));
    }
    let mut baseline = canvas.crop(region);
    let mut samples = Vec::with_capacity(config.iterations);
    for iteration in 0..config.iterations {
        let input_seq = u64::try_from(iteration).ok().and_then(|i| i.checked_mul(2))
            .and_then(|i| i.checked_add(1))
            .ok_or_else(|| BenchError::Invalid("input sequence overflow".into()))?;
        let sent = Instant::now();
        let deadline = sent + config.timeout;
        let press = InputEvent {
            seq: input_seq,
            sent_us: crate::stats::saturating_us(epoch.elapsed()),
            input: Input::MouseButton {
                x: config.click_x, y: config.click_y, button: MouseButton::Left,
                pressed: true, modifiers: Modifiers::default(),
            },
        };
        match tokio::time::timeout_at(deadline.into(), surface.input(press)).await {
            Ok(result) => result?,
            Err(_) => return Err(timeout(iteration, input_seq, config.timeout)),
        }
        let sample = loop {
            let frame = match tokio::time::timeout_at(deadline.into(), frames.recv()).await {
                Ok(Ok(frame)) => frame,
                Ok(Err(RecvError::Lagged(_))) => {
                    decoder = FrameDecoder::new();
                    awaiting_keyframe = true;
                    surface.request_keyframe().await?;
                    continue;
                }
                Ok(Err(RecvError::Closed)) => return Err(BenchError::StreamClosed),
                Err(_) => return Err(timeout(iteration, input_seq, config.timeout)),
            };
            if frame.seq <= last_seq { continue; }
            if awaiting_keyframe && !is_full_keyframe(&frame) { continue; }
            if canvas.width() != frame.width || canvas.height() != frame.height {
                if !is_full_keyframe(&frame) {
                    surface.request_keyframe().await?;
                    continue;
                }
                // Resizes invalidate the previous picture, not evidence of the click.
                decoder = FrameDecoder::new();
                decoder.apply(&frame, &mut canvas)?;
                region = config.region.unwrap_or(Rect { x: 0, y: 0, width: canvas.width(), height: canvas.height() });
                if region.width == 0 || region.height == 0 || !canvas.contains(region) {
                    return Err(BenchError::Invalid("comparison region outside resized frame".into()));
                }
                baseline = canvas.crop(region);
                last_seq = frame.seq;
                continue;
            }
            let decode_started = Instant::now();
            decoder.apply(&frame, &mut canvas)?;
            awaiting_keyframe = false;
            let decode = decode_started.elapsed();
            last_seq = frame.seq;
            if frame.input_seq < input_seq {
                baseline = canvas.crop(region);
                continue;
            }
            let difference = canvas.mean_abs_diff(region, &baseline);
            if difference <= config.change_threshold { continue; }
            baseline = canvas.crop(region);
            break Sample {
                input_seq, frame_seq: frame.seq, latency: sent.elapsed(), decode,
                pixel_difference: difference,
                stages: stages.and_then(|source| source.take_stage_timings(input_seq)),
            };
        };
        samples.push(sample);
        // Complete the click only after observing the press, so a release cannot
        // overtake its marker in a coalescing frame stream.
        let release = InputEvent {
            seq: input_seq + 1,
            sent_us: crate::stats::saturating_us(epoch.elapsed()),
            input: Input::MouseButton {
                x: config.click_x, y: config.click_y, button: MouseButton::Left,
                pressed: false, modifiers: Modifiers::default(),
            },
        };
        surface.input(release).await?;
    }
    Ok(BenchResult { baseline_seq, samples })
}

fn timeout(iteration: usize, input_seq: u64, duration: Duration) -> BenchError {
    BenchError::Timeout { iteration, input_seq, timeout_ms: duration.as_millis().min(u128::from(u64::MAX)) as u64 }
}

fn is_full_keyframe(frame: &Frame) -> bool {
    frame.keyframe && frame.region == Rect { x: 0, y: 0, width: frame.width, height: frame.height }
}
