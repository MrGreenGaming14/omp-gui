//! The X11 capture against an in-process X server that answers connection
//! setup and `GetImage` from a shared framebuffer, speaking the real wire
//! protocol.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use loa_contracts::surface::Rect;
use loa_surfaces::browser::X11Capture;
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
