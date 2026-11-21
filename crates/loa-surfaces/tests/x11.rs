//! The X11 capture against an in-process X server that answers connection
//! setup and `GetImage` from a shared framebuffer, speaking the real wire protocol.
#![cfg(target_os = "linux")]

use loa_contracts::surface::Rect;
use loa_surfaces::browser::X11Capture;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};
use x11rb::protocol::xproto::{
    BackingStore, Depth, EventMask, Format, GetImageReply, ImageOrder, Screen, Setup, VisualClass, Visualtype,
};
use x11rb::x11_utils::Serialize;

const WIDTH: usize = 8;
const HEIGHT: usize = 6;

/// BGRX framebuffer as an LSB-first 24 bit TrueColor server stores it.
type Framebuffer = Arc<Mutex<Vec<u8>>>;

fn set(fb: &Framebuffer, x: usize, y: usize, rgb: [u8; 3]) {
    let i = (y * WIDTH + x) * 4;
    fb.lock().unwrap()[i..i + 4].copy_from_slice(&[rgb[2], rgb[1], rgb[0], 0]);
}

fn setup() -> Vec<u8> {
    let visual = Visualtype {
        visual_id: 0x21,
        class: VisualClass::TRUE_COLOR,
        bits_per_rgb_value: 8,
        colormap_entries: 256,
        red_mask: 0xff0000,
        green_mask: 0xff00,
        blue_mask: 0xff,
    };
    let screen = Screen {
        root: 0x100,
        default_colormap: 0x20,
        white_pixel: 0xffffff,
        black_pixel: 0,
        current_input_masks: EventMask::NO_EVENT,
        width_in_pixels: WIDTH as u16,
        height_in_pixels: HEIGHT as u16,
        width_in_millimeters: 300,
        height_in_millimeters: 200,
        min_installed_maps: 1,
        max_installed_maps: 1,
        root_visual: 0x21,
        backing_stores: BackingStore::NOT_USEFUL,
        save_unders: false,
        root_depth: 24,
        allowed_depths: vec![Depth { depth: 24, visuals: vec![visual] }],
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
        roots: vec![screen],
    };
    setup.length = ((setup.serialize().len() - 8) / 4) as u16;
    setup.serialize()
}

/// Serves `fb` on the Unix socket `listener` until the client hangs up.
fn serve(listener: UnixListener, fb: Framebuffer) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else { return };
        let mut head = [0u8; 12];
        stream.read_exact(&mut head).unwrap();
        assert_eq!(head[0], b'l', "client byte order");
        let le = |b: &[u8], i: usize| usize::from(u16::from_le_bytes([b[i], b[i + 1]]));
        let mut auth = vec![0; le(&head, 6).div_ceil(4) * 4 + le(&head, 8).div_ceil(4) * 4];
        stream.read_exact(&mut auth).unwrap();
        stream.write_all(&setup()).unwrap();
        for sequence in 1u16.. {
            let mut header = [0u8; 4];
            if stream.read_exact(&mut header).is_err() {
                return;
            }
            let mut body = vec![0; le(&header, 2) * 4 - 4];
            stream.read_exact(&mut body).unwrap();
            assert_eq!(header[0], 73, "only GetImage is served");
            let (x, y, w, h) = (le(&body, 4), le(&body, 6), le(&body, 8), le(&body, 10));
            let pixels = fb.lock().unwrap();
            let data = (y..y + h).flat_map(|row| &pixels[(row * WIDTH + x) * 4..(row * WIDTH + x + w) * 4]).copied().collect();
            drop(pixels);
            stream.write_all(&GetImageReply { depth: 24, sequence, visual: 0x21, data }.serialize()).unwrap();
        }
    })
}

#[test]
fn captures_rgba_then_only_the_changed_rows_then_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("X7");
    let fb: Framebuffer = Arc::new(Mutex::new(vec![0; WIDTH * HEIGHT * 4]));
    set(&fb, 0, 0, [10, 20, 30]);
    let server = serve(UnixListener::bind(&socket).unwrap(), fb.clone());

    let mut capture = X11Capture::connect(&socket).unwrap();
    assert_eq!(capture.screen_size(), (8, 6));
    let first = capture.grab(4, 3, false).unwrap().unwrap();
    assert_eq!((first.keyframe, first.region, first.data.len()), (true, Rect { x: 0, y: 0, width: 4, height: 3 }, 4 * 3 * 4));
    assert_eq!(&first.data[..4], &[10, 20, 30, 255]);
    assert_eq!(capture.grab(4, 3, false).unwrap(), None, "unchanged picture sends nothing");

    set(&fb, 2, 1, [255, 0, 0]);
    let band = capture.grab(4, 3, false).unwrap().unwrap();
    assert_eq!((band.keyframe, band.region), (false, Rect { x: 0, y: 1, width: 4, height: 1 }));
    assert_eq!(&band.data[2 * 4..3 * 4], &[255, 0, 0, 255]);

    let key = capture.grab(4, 3, true).unwrap().unwrap();
    assert_eq!((key.keyframe, key.data.len()), (true, 4 * 3 * 4));
    let clipped = capture.grab(100, 100, false).unwrap().unwrap();
    assert_eq!((clipped.width, clipped.height, clipped.keyframe), (8, 6, true));
    drop(capture);
    server.join().unwrap();
}
