//! Deterministic software-rendered, actually encoded reference surface.

use loa_util::LockExt;
use crate::{bench::{StageTimingSource, StageTimings}, canvas::Canvas, encode::{region::RegionEncoder, LiveEncoder, Strategy}, BenchError, Result};
use async_trait::async_trait;
use bytes::Bytes;
use loa_surface::{Document, Frame, Input, InputEvent, Surface, SurfaceError, SurfaceInfo, SurfaceMode, SurfaceUri};
use std::{collections::HashMap, sync::{Arc, Mutex}, time::{Duration, Instant}};
use tokio::sync::{broadcast, Mutex as AsyncMutex};

#[derive(Clone, Debug)]
pub struct SyntheticConfig {
    pub width: u32,
    pub height: u32,
    pub render_delay: Duration,
    pub encode_delay: Duration,
}

impl Default for SyntheticConfig {
    fn default() -> Self {
        Self { width: 320, height: 180, render_delay: Duration::ZERO, encode_delay: Duration::ZERO }
    }
}

struct Picture {
    canvas: Canvas,
    encoder: LiveEncoder,
    inverted: bool,
    last_input_seq: u64,
}

/// Clicks paint a high-contrast marker at the clicked coordinates; a JPEG
/// keyframe establishes the baseline and later clicks send changed regions.
pub struct SyntheticSurface {
    info: SurfaceInfo,
    picture: AsyncMutex<Picture>,
    sender: broadcast::Sender<Arc<Frame>>,
    stages: Mutex<HashMap<u64, StageTimings>>,
    epoch: Instant,
    render_delay: Duration,
    encode_delay: Duration,
}

impl SyntheticSurface {
    pub fn new(config: SyntheticConfig) -> Result<Self> {
        let pixels = u64::from(config.width).checked_mul(u64::from(config.height))
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| usize::try_from(n).ok());
        if config.width == 0 || config.height == 0 || pixels.is_none() {
            return Err(BenchError::Size { width: config.width, height: config.height, reason: "invalid RGBA dimensions" });
        }
        let uri: SurfaceUri = "surface://bench/synthetic".parse()?;
        let info = SurfaceInfo {
            uri, name: "Synthetic input-to-pixels".into(),
            width: config.width, height: config.height,
            mode: SurfaceMode::Pixel, mime: None,
        };
        let mut canvas = Canvas::new(config.width, config.height);
        paint_background(&mut canvas);
        let encoder = LiveEncoder::new(config.width, config.height, Strategy::Regions(RegionEncoder::new(95)));
        let (sender, _) = broadcast::channel(256);
        Ok(Self {
            info,
            picture: AsyncMutex::new(Picture { canvas, encoder, inverted: false, last_input_seq: 0 }),
            sender,
            stages: Mutex::new(HashMap::new()),
            epoch: Instant::now(),
            render_delay: config.render_delay,
            encode_delay: config.encode_delay,
        })
    }

    fn emit(&self, picture: &mut Picture) -> loa_surface::Result<()> {
        let timestamp = crate::stats::saturating_us(self.epoch.elapsed());
        let encoded = picture.encoder.encode(picture.canvas.rgba(), picture.last_input_seq, timestamp)
            .map_err(|e| SurfaceError::Backend(e.to_string()))?;
        for frame in encoded.frames {
            // No subscribers is normal (e.g. before the first benchmark subscription).
            let _ = self.sender.send(Arc::new(frame));
        }
        Ok(())
    }
}

fn paint_background(canvas: &mut Canvas) {
    for pixel in canvas.rgba_mut().as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&[25, 205, 55, 255]);
    }
}

fn paint_marker(canvas: &mut Canvas, x: u32, y: u32, inverted: bool) {
    let color = if inverted { [225, 45, 195, 255] } else { [25, 205, 55, 255] };
    let width = canvas.width() as usize;
    let right = (x + 48).min(canvas.width());
    let bottom = (y + 48).min(canvas.height());
    for row in y..bottom {
        for col in x..right {
            let offset = (row as usize * width + col as usize) * 4;
            canvas.rgba_mut()[offset..offset + 4].copy_from_slice(&color);
        }
    }
}

impl StageTimingSource for SyntheticSurface {
    fn take_stage_timings(&self, input_seq: u64) -> Option<StageTimings> {
        self.stages.locked().remove(&input_seq)
    }
}

#[async_trait]
impl Surface for SyntheticSurface {
    fn info(&self) -> SurfaceInfo { self.info.clone() }

    fn frames(&self) -> loa_surface::Result<broadcast::Receiver<Arc<Frame>>> {
        Ok(self.sender.subscribe())
    }

    async fn request_keyframe(&self) -> loa_surface::Result<()> {
        let mut picture = self.picture.lock().await;
        picture.encoder.request_keyframe();
        self.emit(&mut picture)
    }

    async fn input(&self, event: InputEvent) -> loa_surface::Result<()> {
        let Input::MouseButton { x, y, button: loa_surface::MouseButton::Left, pressed: true, .. } = event.input else {
            return Ok(());
        };
        if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 || x >= self.info.width as f32 || y >= self.info.height as f32 {
            return Err(SurfaceError::Backend("click outside synthetic surface".into()));
        }
        let mut picture = self.picture.lock().await;
        let render_started = Instant::now();
        tokio::time::sleep(self.render_delay).await;
        picture.inverted = !picture.inverted;
        let inverted = picture.inverted;
        paint_marker(&mut picture.canvas, x as u32, y as u32, inverted);
        picture.last_input_seq = event.seq;
        let render = render_started.elapsed();
        let encode_started = Instant::now();
        tokio::time::sleep(self.encode_delay).await;
        self.emit(&mut picture)?;
        let encode = encode_started.elapsed();
        self.stages.locked()
            .insert(event.seq, StageTimings { render, encode });
        Ok(())
    }

    async fn document(&self) -> loa_surface::Result<Document> {
        Err(SurfaceError::Mode { uri: self.info.uri.clone(), mode: self.info.mode })
    }

    async fn screenshot(&self) -> loa_surface::Result<Bytes> {
        let picture = self.picture.lock().await;
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, self.info.width, self.info.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().map_err(|e| SurfaceError::Backend(e.to_string()))?;
            writer.write_image_data(picture.canvas.rgba()).map_err(|e| SurfaceError::Backend(e.to_string()))?;
        }
        Ok(Bytes::from(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::{run, BenchConfig};
    use loa_surface::Rect;

    #[tokio::test]
    async fn clicks_are_encoded_decoded_and_measured_with_delays() {
        let surface = SyntheticSurface::new(SyntheticConfig {
            width: 48, height: 32,
            render_delay: Duration::from_millis(3), encode_delay: Duration::from_millis(4),
        }).unwrap();
        let config = BenchConfig { iterations: 3, timeout: Duration::from_secs(2), ..BenchConfig::default() };
        let result = run(&surface, &config, Some(&surface)).await.unwrap();
        assert_eq!(result.samples.len(), 3);
        assert!(result.samples[0].frame_seq > result.baseline_seq);
        for sample in &result.samples {
            assert!(sample.pixel_difference > config.change_threshold);
            let stages = sample.stages.unwrap();
            assert!(stages.render >= Duration::from_millis(3));
            assert!(stages.encode >= Duration::from_millis(4));
            assert!(sample.latency >= stages.render + stages.encode);
        }
        assert!(result.samples.windows(2).all(|pair| pair[0].frame_seq < pair[1].frame_seq));
        assert!(surface.screenshot().await.unwrap().starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[tokio::test]
    async fn marker_changes_clicked_pixels_not_unrelated_pixels() {
        let surface = SyntheticSurface::new(SyntheticConfig {
            width: 96, height: 96, ..SyntheticConfig::default()
        }).unwrap();
        let mut frames = surface.frames().unwrap();
        surface.request_keyframe().await.unwrap();
        let baseline = frames.recv().await.unwrap();
        surface.input(InputEvent {
            seq: 1, sent_us: 0,
            input: Input::MouseButton {
                x: 60.0, y: 60.0, button: loa_surface::MouseButton::Left,
                pressed: true, modifiers: Default::default(),
            },
        }).await.unwrap();
        let changed = frames.recv().await.unwrap();
        assert_eq!(changed.input_seq, 1);
        assert!(changed.region.x >= 48 && changed.region.y >= 48);
        let mut decoder = crate::decode::FrameDecoder::new();
        let mut canvas = Canvas::default();
        decoder.apply(&baseline, &mut canvas).unwrap();
        let before = canvas.clone();
        decoder.apply(&changed, &mut canvas).unwrap();
        assert_eq!(&before.rgba()[0..4], &canvas.rgba()[0..4]);
        let clicked = Rect { x: 60, y: 60, width: 1, height: 1 };
        assert!(canvas.mean_abs_diff(clicked, &before.crop(clicked)) > 100.0);
    }

    #[tokio::test]
    async fn no_click_effect_times_out_instead_of_counting_the_echo() {
        let surface = SyntheticSurface::new(SyntheticConfig::default()).unwrap();
        let config = BenchConfig { iterations: 1, timeout: Duration::from_millis(40), change_threshold: 255.0, ..BenchConfig::default() };
        let err = run(&surface, &config, None).await.unwrap_err();
        assert!(matches!(err, BenchError::Timeout { iteration: 0, input_seq: 1, .. }));
    }

    #[tokio::test]
    async fn keyframe_reestablishes_the_picture_after_a_click() {
        let surface = SyntheticSurface::new(SyntheticConfig::default()).unwrap();
        let mut frames = surface.frames().unwrap();
        surface.request_keyframe().await.unwrap();
        let first = frames.recv().await.unwrap();
        assert!(first.keyframe);
        assert_eq!(first.region, Rect { x: 0, y: 0, width: 320, height: 180 });
        surface.input(InputEvent {
            seq: 7, sent_us: 0,
            input: Input::MouseButton { x: 1.0, y: 1.0, button: loa_surface::MouseButton::Left,
                pressed: true, modifiers: Default::default() },
        }).await.unwrap();
        let changed = frames.recv().await.unwrap();
        assert_eq!(changed.input_seq, 7);
        assert!(changed.seq > first.seq);
        surface.request_keyframe().await.unwrap();
        let recovered = frames.recv().await.unwrap();
        assert!(recovered.keyframe && recovered.seq > changed.seq);
        assert_eq!(recovered.input_seq, 7);
    }
}
