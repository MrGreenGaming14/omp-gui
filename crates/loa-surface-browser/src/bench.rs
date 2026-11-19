//! Input to frame latency of a browser tab, measured on this machine.
//!
//! A page repaints its whole background on every mouse press. Each sample
//! sends one press and waits for the first frame whose pixels show the new
//! color. The latency is the time from just before the input is handed to
//! the tab to the moment that frame reaches a subscriber, so it covers CDP
//! dispatch, Chrome's render, capture, JPEG encode and transport to us. It
//! leaves out the viewer's decode and display.

use crate::{BrowserError, BrowserServer, Result};
use loa_surface::{Encoding, Frame, Input, InputEvent, Modifiers, MouseButton, Surface};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::{self, error::RecvError};
use jpeg_decoder::{Decoder as JpegDecoder, PixelFormat};

/// How long one sample may wait for its frame.
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(5);

/// A page that paints its background red and blue in turn on each press.
pub const REPAINT_PAGE: &str = "data:text/html,<!doctype html><html><head><title>latency</title><style>\
html,body{margin:0;height:100%;background:rgb(255,0,0)}</style></head><body><script>\
let blue=false;addEventListener('mousedown',()=>{blue=!blue;\
document.body.style.background=blue?'rgb(0,0,255)':'rgb(255,0,0)'});</script></body></html>";

/// Latencies of every sample and how honest the frames' `input_seq` was.
#[derive(Clone, Debug)]
pub struct LatencyReport {
    /// Sorted ascending.
    pub samples: Vec<Duration>,
    /// Samples whose first repainted frame carried the input's seq.
    pub stamped: usize,
    /// Frames stamped with an input whose effect they did not show.
    pub overclaimed: usize,
}

impl LatencyReport {
    /// The value at quantile `q` (0 to 1), nearest rank.
    pub fn quantile(&self, q: f64) -> Duration {
        if self.samples.is_empty() {
            return Duration::ZERO;
        }
        let rank = (q * self.samples.len() as f64).ceil() as usize;
        self.samples[rank.clamp(1, self.samples.len()) - 1]
    }

    pub fn median(&self) -> Duration {
        self.quantile(0.5)
    }

    pub fn p95(&self) -> Duration {
        self.quantile(0.95)
    }
}

impl std::fmt::Display for LatencyReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        write!(
            f,
            "{} samples: median {:.1} ms, p95 {:.1} ms, min {:.1} ms, max {:.1} ms; \
             input_seq on the first repainted frame {}/{}, overclaimed {}",
            self.samples.len(),
            ms(self.median()),
            ms(self.p95()),
            ms(self.quantile(0.0)),
            ms(self.quantile(1.0)),
            self.stamped,
            self.samples.len(),
            self.overclaimed,
        )
    }
}

/// Opens a tab on [`REPAINT_PAGE`], takes `samples` press-to-frame
/// measurements, and closes the tab.
pub async fn click_latency(server: &BrowserServer, samples: usize) -> Result<LatencyReport> {
    let tab = server.open_tab(REPAINT_PAGE).await?;
    let result = measure(tab.as_ref(), samples).await;
    server.close_tab(tab.uri()).await?;
    result
}

async fn measure(surface: &dyn Surface, samples: usize) -> Result<LatencyReport> {
    let mut frames = surface.frames()?;
    let info = surface.info();
    let (x, y) = (info.width as f32 / 2.0, info.height as f32 / 2.0);
    let mut seq = 0;
    let mut send = async |input: Input| {
        seq += 1;
        let event = InputEvent { seq, sent_us: crate::frames::now_us(), input };
        surface.input(event).await.map(|()| seq)
    };
    // The page starts red; wait for that before measuring.
    wait_for_color(&mut frames, 0, Rgb::Red).await?;

    let mut report = LatencyReport { samples: Vec::with_capacity(samples), stamped: 0, overclaimed: 0 };
    let mut blue = false;
    for _ in 0..samples {
        blue = !blue;
        let want = if blue { Rgb::Blue } else { Rgb::Red };
        let started = Instant::now();
        let pressed = Input::MouseButton { x, y, button: MouseButton::Left, pressed: true, modifiers: Modifiers::default() };
        let seq = send(pressed).await?;
        let (frame, overclaimed) = wait_for_color(&mut frames, seq, want).await?;
        report.samples.push(started.elapsed());
        report.stamped += usize::from(frame.input_seq >= seq);
        report.overclaimed += overclaimed;
        let released = Input::MouseButton { x, y, button: MouseButton::Left, pressed: false, modifiers: Modifiers::default() };
        send(released).await?;
    }
    report.samples.sort();
    Ok(report)
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Rgb {
    Red,
    Blue,
}

/// Waits for a frame whose center is `want`. Also counts frames that claim
/// to contain input `seq` but do not show it yet.
async fn wait_for_color(
    frames: &mut broadcast::Receiver<Arc<Frame>>,
    seq: u64,
    want: Rgb,
) -> Result<(Arc<Frame>, usize)> {
    let deadline = tokio::time::Instant::now() + SAMPLE_TIMEOUT;
    let mut overclaimed = 0;
    loop {
        let frame = match tokio::time::timeout_at(deadline, frames.recv()).await {
            Ok(Ok(frame)) => frame,
            Ok(Err(RecvError::Lagged(_))) => continue,
            Ok(Err(RecvError::Closed)) => return Err(BrowserError::Closed),
            Err(_) => return Err(BrowserError::Script(format!("no {want:?} frame within {SAMPLE_TIMEOUT:?}"))),
        };
        let shows = center_color(&frame)? == Some(want);
        if shows {
            return Ok((frame, overclaimed));
        }
        if seq > 0 && frame.input_seq >= seq {
            overclaimed += 1;
        }
    }
}

/// Color at the center of a whole-surface frame.
fn center_color(frame: &Frame) -> Result<Option<Rgb>> {
    let decoded;
    let (rgba, width, height): (&[u8], usize, usize) = match frame.encoding {
        Encoding::Jpeg => {
            let mut decoder = JpegDecoder::new(frame.data.as_ref());
            let rgb = decoder.decode().map_err(|e| BrowserError::Script(format!("frame jpeg: {e}")))?;
            let info = decoder.info().ok_or_else(|| BrowserError::Script("frame jpeg has no header".into()))?;
            if info.pixel_format != PixelFormat::RGB24 {
                return Ok(None);
            }
            decoded = rgb.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect::<Vec<_>>();
            (&decoded, usize::from(info.width), usize::from(info.height))
        }
        Encoding::Rgba if frame.keyframe => (&frame.data, frame.width as usize, frame.height as usize),
        _ => return Ok(None),
    };
    let i = (height / 2 * width + width / 2) * 4;
    let Some(&[r, g, b, _]) = rgba.get(i..i + 4) else {
        return Ok(None);
    };
    let near = |v: u8, t: u8| v.abs_diff(t) < 40;
    Ok(if near(r, 255) && near(g, 0) && near(b, 0) {
        Some(Rgb::Red)
    } else if near(r, 0) && near(g, 0) && near(b, 255) {
        Some(Rgb::Blue)
    } else {
        None
    })
}
