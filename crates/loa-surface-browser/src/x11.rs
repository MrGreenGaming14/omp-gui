//! Raw frame capture from an X server (the virtual display fallback).
//!
//! Frames are read with core `GetImage` on the root window, converted to
//! RGBA in place, and diffed against the previous frame so a mostly static
//! page sends only the band of rows that changed.

use loa_util::LockExt;
use crate::frames::{FrameHub, InputLog, Picture, now_us};
use crate::{BrowserError, Result};
use bytes::Bytes;
use loa_surface::{Encoding, Rect};
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

/// One capture: the full surface size, the part that changed, and its RGBA rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grab {
    pub width: u32,
    pub height: u32,
    pub region: Rect,
    pub keyframe: bool,
    pub data: Bytes,
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

    /// Captures the top left `width` x `height` (clipped to the screen).
    /// Returns `None` when nothing changed since the last capture, a band of
    /// changed rows otherwise, and the whole picture when `keyframe` is set
    /// or the size changed.
    pub fn grab(&mut self, width: u32, height: u32, keyframe: bool) -> Result<Option<Grab>> {
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
                let changed = |r: &usize| full[r * row..(r + 1) * row] != previous[r * row..(r + 1) * row];
                let Some(first) = (0..height as usize).find(changed) else {
                    return Ok(None);
                };
                // `first` changed, so a last changed row exists at or after it.
                let last = (first..height as usize).rev().find(changed).unwrap_or(first);
                Some((first, last))
            }
            _ => None,
        };
        self.previous = Some((width, height, full.clone()));
        let grab = match band {
            Some((first, last)) if first > 0 || last + 1 < height as usize => Grab {
                width,
                height,
                region: Rect { x: 0, y: first as u32, width, height: (last + 1 - first) as u32 },
                keyframe: false,
                data: full.slice(first * row..(last + 1) * row),
            },
            _ => Grab { width, height, region: Rect { x: 0, y: 0, width, height }, keyframe: true, data: full },
        };
        Ok(Some(grab))
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
    pub hub: Arc<FrameHub>,
    pub log: Arc<Mutex<InputLog>>,
    /// Surface size; the capture is clipped to it.
    pub size: Arc<Mutex<(u32, u32)>>,
    pub stop: AtomicBool,
    pub keyframe: AtomicBool,
    pub interval: Duration,
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
                Ok(Some(grab)) => target.hub.publish(Picture {
                    captured_us,
                    input_seq,
                    width: grab.width,
                    height: grab.height,
                    region: grab.region,
                    encoding: Encoding::Rgba,
                    keyframe: grab.keyframe,
                    data: grab.data,
                }),
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

#[cfg(test)]
pub(crate) mod fake {
    //! An in-process X server that answers connection setup and `GetImage`
    //! from a shared framebuffer, speaking the real wire protocol.

    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use x11rb::protocol::xproto::{
        BackingStore, Depth, EventMask, Format, GetImageReply, ImageOrder, Screen, Setup, VisualClass, Visualtype,
    };
    use x11rb::x11_utils::Serialize;

    /// BGRX framebuffer, `width * height * 4` bytes.
    pub struct Framebuffer {
        pub width: u16,
        pub height: u16,
        pub pixels: Mutex<Vec<u8>>,
    }

    impl Framebuffer {
        pub fn new(width: u16, height: u16) -> Arc<Self> {
            Arc::new(Self { width, height, pixels: Mutex::new(vec![0; width as usize * height as usize * 4]) })
        }

        /// Sets one pixel to an RGB color, stored the way an LSB-first 24 bit TrueColor server stores it.
        pub fn set(&self, x: usize, y: usize, rgb: [u8; 3]) {
            let i = (y * self.width as usize + x) * 4;
            self.pixels.lock().unwrap()[i..i + 4].copy_from_slice(&[rgb[2], rgb[1], rgb[0], 0]);
        }
    }

    fn setup(fb: &Framebuffer) -> Vec<u8> {
        let visual = Visualtype {
            visual_id: 0x21,
            class: VisualClass::TRUE_COLOR,
            bits_per_rgb_value: 8,
            colormap_entries: 256,
            red_mask: 0xff0000,
            green_mask: 0xff00,
            blue_mask: 0xff,
        };
        let mut setup = Setup {
            status: 1,
            protocol_major_version: 11,
            protocol_minor_version: 0,
            length: 0,
            release_number: 1,
            resource_id_base: 0x0020_0000,
            resource_id_mask: 0x001f_ffff,
            motion_buffer_size: 256,
            maximum_request_length: 0xffff,
            image_byte_order: ImageOrder::LSB_FIRST,
            bitmap_format_bit_order: ImageOrder::LSB_FIRST,
            bitmap_format_scanline_unit: 32,
            bitmap_format_scanline_pad: 32,
            min_keycode: 8,
            max_keycode: 255,
            vendor: b"fake".to_vec(),
            pixmap_formats: vec![Format { depth: 24, bits_per_pixel: 32, scanline_pad: 32 }],
            roots: vec![Screen {
                root: 0x100,
                default_colormap: 0x20,
                white_pixel: 0xffffff,
                black_pixel: 0,
                current_input_masks: EventMask::NO_EVENT,
                width_in_pixels: fb.width,
                height_in_pixels: fb.height,
                width_in_millimeters: 300,
                height_in_millimeters: 200,
                min_installed_maps: 1,
                max_installed_maps: 1,
                root_visual: 0x21,
                backing_stores: BackingStore::NOT_USEFUL,
                save_unders: false,
                root_depth: 24,
                allowed_depths: vec![Depth { depth: 24, visuals: vec![visual] }],
            }],
        };
        let length = setup.serialize().len();
        setup.length = ((length - 8) / 4) as u16;
        setup.serialize()
    }

    /// Serves `fb` on a Unix socket at `path` until the client hangs up.
    pub fn serve(path: &Path, fb: Arc<Framebuffer>) -> std::thread::JoinHandle<()> {
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let mut head = [0u8; 12];
            stream.read_exact(&mut head).unwrap();
            assert_eq!(head[0], b'l', "client byte order");
            let pad = |n: usize| n.div_ceil(4) * 4;
            let name = u16::from_le_bytes([head[6], head[7]]) as usize;
            let data = u16::from_le_bytes([head[8], head[9]]) as usize;
            let mut auth = vec![0; pad(name) + pad(data)];
            stream.read_exact(&mut auth).unwrap();
            stream.write_all(&setup(&fb)).unwrap();

            let mut sequence: u16 = 0;
            loop {
                let mut header = [0u8; 4];
                if stream.read_exact(&mut header).is_err() {
                    return;
                }
                sequence = sequence.wrapping_add(1);
                let words = u16::from_le_bytes([header[2], header[3]]) as usize;
                let mut body = vec![0; words * 4 - 4];
                stream.read_exact(&mut body).unwrap();
                assert_eq!(header[0], 73, "only GetImage is served");
                let x = i16::from_le_bytes([body[4], body[5]]) as usize;
                let y = i16::from_le_bytes([body[6], body[7]]) as usize;
                let w = u16::from_le_bytes([body[8], body[9]]) as usize;
                let h = u16::from_le_bytes([body[10], body[11]]) as usize;
                let pixels = fb.pixels.lock().unwrap();
                let stride = fb.width as usize * 4;
                let mut out = Vec::with_capacity(w * h * 4);
                for row in y..y + h {
                    out.extend_from_slice(&pixels[row * stride + x * 4..row * stride + (x + w) * 4]);
                }
                drop(pixels);
                let reply = GetImageReply { depth: 24, sequence, visual: 0x21, data: out };
                stream.write_all(&reply.serialize()).unwrap();
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Framebuffer, serve};
    use super::*;

    #[test]
    fn captures_rgba_then_only_the_changed_rows_then_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("X7");
        let fb = Framebuffer::new(8, 6);
        fb.set(0, 0, [10, 20, 30]);
        let server = serve(&socket, fb.clone());

        let mut capture = X11Capture::connect(&socket).unwrap();
        assert_eq!(capture.screen_size(), (8, 6));
        let first = capture.grab(4, 3, false).unwrap().unwrap();
        assert!(first.keyframe);
        assert_eq!(first.region, Rect { x: 0, y: 0, width: 4, height: 3 });
        assert_eq!(&first.data[..4], &[10, 20, 30, 255]);
        assert_eq!(first.data.len(), 4 * 3 * 4);

        assert_eq!(capture.grab(4, 3, false).unwrap(), None, "unchanged picture sends nothing");

        fb.set(2, 1, [255, 0, 0]);
        let band = capture.grab(4, 3, false).unwrap().unwrap();
        assert!(!band.keyframe);
        assert_eq!(band.region, Rect { x: 0, y: 1, width: 4, height: 1 });
        assert_eq!(&band.data[2 * 4..3 * 4], &[255, 0, 0, 255]);

        let key = capture.grab(4, 3, true).unwrap().unwrap();
        assert!(key.keyframe);
        assert_eq!(key.data.len(), 4 * 3 * 4);

        let clipped = capture.grab(100, 100, false).unwrap().unwrap();
        assert_eq!((clipped.width, clipped.height, clipped.keyframe), (8, 6, true));
        drop(capture);
        server.join().unwrap();
    }

    #[test]
    fn capture_thread_stamps_frames_with_inputs_sent_before_the_capture() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("X8");
        let fb = Framebuffer::new(4, 4);
        let _server = serve(&socket, fb.clone());
        let capture = X11Capture::connect(&socket).unwrap();

        let hub = Arc::new(FrameHub::new());
        let mut frames = hub.subscribe();
        let log = Arc::new(Mutex::new(InputLog::default()));
        log.lock().unwrap().record(9, now_us());
        let target = Arc::new(CaptureTarget {
            hub: hub.clone(),
            log: log.clone(),
            size: Arc::new(Mutex::new((4, 4))),
            stop: AtomicBool::new(false),
            keyframe: AtomicBool::new(false),
            interval: Duration::from_millis(1),
        });
        let thread = spawn_capture(capture, target.clone());

        let first = frames.blocking_recv().unwrap();
        assert!(first.keyframe);
        assert_eq!((first.encoding, first.input_seq, first.seq), (Encoding::Rgba, 9, 1));

        log.lock().unwrap().record(10, now_us());
        fb.set(1, 3, [1, 2, 3]);
        let changed = frames.blocking_recv().unwrap();
        assert_eq!(changed.region, Rect { x: 0, y: 3, width: 4, height: 1 });
        assert_eq!(changed.input_seq, 10);
        assert_eq!(&changed.data[4..8], &[1, 2, 3, 255]);

        target.keyframe.store(true, Ordering::Relaxed);
        let key = frames.blocking_recv().unwrap();
        assert!(key.keyframe);
        target.stop.store(true, Ordering::Relaxed);
        thread.join().unwrap();
    }
}
