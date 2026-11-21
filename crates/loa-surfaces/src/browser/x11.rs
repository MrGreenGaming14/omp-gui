//! Raw frame capture from an X server (the virtual display fallback).
//!
//! Frames are read with core `GetImage` on the root window, converted to
//! RGBA in place, and diffed against the previous frame so a mostly static
//! page sends only the band of rows that changed.

use crate::browser::tab::{Capture, set_viewport};
use crate::browser::{BrowserError, Result};
use crate::feed::{FrameFeed, InputLog};
use crate::pixels::changed_rows;
use bytes::Bytes;
use loa_contracts::surface::{Encoding, Frame, Rect};
use loa_util::LockExt;
use loa_util::time::now_us;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, ImageFormat, ImageOrder};
use x11rb::rust_connection::{DefaultStream, RustConnection};

/// Byte offsets of red, green and blue inside one 32 bit server pixel.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct PixelLayout {
    red: usize,
    green: usize,
    blue: usize,
}

/// A connection to an X server that captures its first screen.
pub struct X11Capture {
    conn: RustConnection,
    root: u32,
    screen_width: u32,
    screen_height: u32,
    layout: PixelLayout,
    /// Last full frame as `(width, height, rgba)`.
    previous: Option<(u32, u32, Bytes)>,
}

impl X11Capture {
    /// Connects over the server's Unix socket (e.g. `/tmp/.X11-unix/X99`).
    pub fn connect(socket: &Path) -> Result<Self> {
        let (stream, _peer) = DefaultStream::from_unix_stream(UnixStream::connect(socket)?)?;
        let conn = RustConnection::connect_to_stream(stream, 0)?;
        let setup = conn.setup();
        let screen = setup.roots.first().ok_or_else(|| BrowserError::X11("the server has no screen".into()))?;
        let bits = setup
            .pixmap_formats
            .iter()
            .find(|f| f.depth == screen.root_depth)
            .map(|f| f.bits_per_pixel)
            .ok_or_else(|| BrowserError::X11(format!("no pixmap format for depth {}", screen.root_depth)))?;
        let visual = screen
            .allowed_depths
            .iter()
            .flat_map(|d| d.visuals.iter())
            .find(|v| v.visual_id == screen.root_visual)
            .ok_or_else(|| BrowserError::X11("root visual not described".into()))?;
        if bits != 32 {
            return Err(BrowserError::X11(format!("{bits} bits per pixel, only 32 is supported")));
        }
        let msb = setup.image_byte_order == ImageOrder::MSB_FIRST;
        let layout = PixelLayout {
            red: channel_byte(visual.red_mask, msb)?,
            green: channel_byte(visual.green_mask, msb)?,
            blue: channel_byte(visual.blue_mask, msb)?,
        };
        let (root, screen_width, screen_height) =
            (screen.root, u32::from(screen.width_in_pixels), u32::from(screen.height_in_pixels));
        Ok(Self { conn, root, screen_width, screen_height, layout, previous: None })
    }

    /// Size of the X screen.
    pub fn screen_size(&self) -> (u32, u32) {
        (self.screen_width, self.screen_height)
    }

    /// Captures the top left `width` x `height` (clipped to the screen) as an
    /// RGBA frame without numbering or timing. Returns `None` when nothing
    /// changed since the last capture, a band of changed rows otherwise, and
    /// the whole picture when `keyframe` is set or the size changed.
    pub fn grab(&mut self, width: u32, height: u32, keyframe: bool) -> Result<Option<Frame>> {
        let (width, height) = (width.min(self.screen_width), height.min(self.screen_height));
        if width == 0 || height == 0 {
            return Ok(None);
        }
        // Both fit in u16: they are clipped to the screen size, which X sends as u16.
        let reply = self
            .conn
            .get_image(ImageFormat::Z_PIXMAP, self.root, 0, 0, width as u16, height as u16, !0)?
            .reply()?;
        let mut data = reply.data;
        let row = width as usize * 4;
        if data.len() != row * height as usize {
            return Err(BrowserError::X11(format!("GetImage sent {} bytes for {width}x{height}", data.len())));
        }
        to_rgba(&mut data, self.layout);
        let full = Bytes::from(data);

        let band = match &self.previous {
            Some((w, h, previous)) if !keyframe && (*w, *h) == (width, height) => {
                let Some(band) = changed_rows(previous, &full, width) else { return Ok(None) };
                Some(band)
            }
            _ => None,
        };
        self.previous = Some((width, height, full.clone()));
        let (region, keyframe, data) = match band {
            Some((first, last)) if first > 0 || last + 1 < height => {
                let rows = first as usize * row..(last as usize + 1) * row;
                (Rect { x: 0, y: first, width, height: last + 1 - first }, false, full.slice(rows))
            }
            _ => (Rect { x: 0, y: 0, width, height }, true, full),
        };
        let (seq, captured_us, input_seq, encoding) = (0, 0, 0, Encoding::Rgba);
        Ok(Some(Frame { seq, captured_us, input_seq, width, height, region, encoding, keyframe, data }))
    }
}

/// Byte index of the 8 bit channel selected by `mask` within a 32 bit pixel.
fn channel_byte(mask: u32, msb_first: bool) -> Result<usize> {
    let shift = mask.trailing_zeros();
    if mask >> shift != 0xff || !shift.is_multiple_of(8) {
        return Err(BrowserError::X11(format!("unsupported channel mask {mask:#x}")));
    }
    let index = (shift / 8) as usize;
    Ok(if msb_first { 3 - index } else { index })
}

/// Rewrites server pixels as RGBA, in place.
fn to_rgba(data: &mut [u8], layout: PixelLayout) {
    for pixel in data.as_chunks_mut::<4>().0 {
        *pixel = [pixel[layout.red], pixel[layout.green], pixel[layout.blue], 255];
    }
}

/// What the capture thread needs from its tab.
pub(crate) struct CaptureTarget {
    pub hub: Arc<FrameFeed>,
    pub log: Arc<Mutex<InputLog>>,
    /// Surface size; the capture is clipped to it.
    pub size: Arc<Mutex<(u32, u32)>>,
    pub stop: AtomicBool,
    pub keyframe: AtomicBool,
    pub interval: Duration,
}

/// The headed [`Capture`] strategy: a tab's virtual display grabbed by [`spawn_capture`].
pub(crate) struct X11Display {
    pub target: Arc<CaptureTarget>,
    /// The display's screen; the viewport can't outgrow it.
    pub screen: (u32, u32),
}

#[async_trait::async_trait]
impl Capture for X11Display {
    fn fit(&self, width: u32, height: u32) -> (u32, u32) {
        (width.min(self.screen.0), height.min(self.screen.1))
    }

    async fn resize(&self, page: &chromiumoxide::Page, width: u32, height: u32) -> Result<()> {
        set_viewport(page, width, height).await?;
        *self.target.size.locked() = (width, height);
        self.target.keyframe.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Grabs are diffed, so only an explicit request needs a whole picture.
    fn keyframe(&self, _: &FrameFeed, subscribe: bool) {
        if !subscribe {
            self.target.keyframe.store(true, Ordering::Relaxed);
        }
    }

    fn stop(&self) {
        self.target.stop.store(true, Ordering::Relaxed);
    }
}

/// Captures on a thread of its own (x11rb is blocking) until `stop` is set
/// or the server goes away. The first capture is always a keyframe.
pub(crate) fn spawn_capture(mut capture: X11Capture, target: Arc<CaptureTarget>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        target.keyframe.store(true, Ordering::Relaxed);
        while !target.stop.load(Ordering::Relaxed) {
            let started = Instant::now();
            let (width, height) = *target.size.locked();
            let captured_us = now_us();
            let input_seq = target.log.locked().seq_before(captured_us);
            match capture.grab(width, height, target.keyframe.swap(false, Ordering::Relaxed)) {
                Ok(Some(frame)) => target.hub.publish(Frame { captured_us, input_seq, ..frame }),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, "virtual display capture stopped");
                    return;
                }
            }
            // Pacing, not synchronization: caps the capture rate.
            if let Some(rest) = target.interval.checked_sub(started.elapsed()) {
                std::thread::sleep(rest);
            }
        }
    })
}
