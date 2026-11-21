//! The surface tab driven by a fake `Surface`, through `State::update` and
//! the tasks it returns.

use bytes::Bytes;
use futures::StreamExt;
use iced::{Point, Size, Task};
use iced_runtime::Action;
use loa_contracts::surface::{
    Document, Encoding, Frame, Input, InputEvent, Modifiers, MouseButton, Rect,
    SurfaceInfo, SurfaceMode, SurfaceUri,
};
use loa_ui_surface::{
    Clock, DecodeError, Event, KeyInput, Message, Options, PageError, PageRenderer, PageRenderers,
    PointerInput, RgbaPage, State, VideoDecoder,
};
use loa_surfaces::testkit::{FakeSurface, Probe, solid};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};

#[derive(Default)]
struct FakeClock(AtomicU64);

impl FakeClock {
    fn set(&self, us: u64) {
        self.0.store(us, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now_us(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A tab over a fake surface with its tasks running on the test runtime.
struct Harness {
    state: State,
    surface: Arc<FakeSurface>,
    frames: broadcast::Sender<Arc<Frame>>,
    clock: Arc<FakeClock>,
    messages: mpsc::UnboundedReceiver<Message>,
    inputs: mpsc::UnboundedReceiver<InputEvent>,
}

fn spawn(task: Task<Message>, out: mpsc::UnboundedSender<Message>) {
    if let Some(mut stream) = iced_runtime::task::into_stream(task) {
        tokio::spawn(async move {
            while let Some(action) = stream.next().await {
                if let Action::Output(message) = action {
                    let _ = out.send(message);
                }
            }
        });
    }
}

impl Harness {
    fn open(
        mode: SurfaceMode,
        width: u32,
        height: u32,
        document: Option<Document>,
        options: Options,
    ) -> Self {
        let info = SurfaceInfo {
            uri: SurfaceUri::new("fake", "one").unwrap(),
            name: "Fake".into(),
            width,
            height,
            mode,
            mime: document.as_ref().map(|d| d.mime.clone()),
        };
        let (surface, Probe { frames, inputs, .. }) = FakeSurface::new(info, document);
        let clock = Arc::new(FakeClock::default());
        let options = Options {
            clock: clock.clone(),
            ..options
        };
        let (state, task) = State::open(surface.clone(), options);
        let (out, messages) = mpsc::unbounded_channel();
        spawn(task, out);
        Self {
            state,
            surface,
            frames,
            clock,
            messages,
            inputs,
        }
    }

    fn pixel(width: u32, height: u32) -> Self {
        Self::open(SurfaceMode::Pixel, width, height, None, Options::default())
    }

    /// Updates and runs the task the update returned to completion.
    fn update(&mut self, message: Message) -> Option<Event> {
        let (task, event) = self.state.update(message);
        if let Some(stream) = iced_runtime::task::into_stream(task) {
            let outputs: Vec<_> = futures::executor::block_on(stream.collect());
            for action in outputs {
                if let Action::Output(Message::Failed(e)) = action {
                    panic!("surface call failed: {e}");
                }
            }
        }
        event
    }

    async fn next_message(&mut self) -> Message {
        self.messages.recv().await.unwrap()
    }

    async fn next_input(&mut self) -> InputEvent {
        self.inputs.recv().await.unwrap()
    }

    /// Inputs the surface received, up to and including `last`. Delivery is
    /// ordered, so anything sent before `last` is in the list.
    async fn inputs_until(&mut self, last: &Input) -> Vec<Input> {
        let mut out = Vec::new();
        loop {
            let input = self.next_input().await.input;
            let done = input == *last;
            out.push(input);
            if done {
                return out;
            }
        }
    }

    fn pixel_at(&self, x: u32, y: u32) -> [u8; 4] {
        let fb = self.state.framebuffer().unwrap();
        let i = ((y * fb.width() + x) * 4) as usize;
        fb.pixels()[i..i + 4].try_into().unwrap()
    }

    fn pointer(&mut self, panel: Size, input: PointerInput) {
        self.update(Message::Pointer { panel, input });
    }
}

fn frame(
    seq: u64,
    width: u32,
    height: u32,
    region: Rect,
    encoding: Encoding,
    data: Vec<u8>,
) -> Arc<Frame> {
    Arc::new(Frame {
        seq,
        captured_us: 0,
        input_seq: 0,
        width,
        height,
        region,
        encoding,
        keyframe: region
            == Rect {
                x: 0,
                y: 0,
                width,
                height,
            },
        data: Bytes::from(data),
    })
}

fn full(width: u32, height: u32) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width,
        height,
    }
}


#[tokio::test]
async fn streamed_frames_are_drawn_and_input_reaches_the_surface_in_order() {
    let mut h = Harness::pixel(4, 2);
    h.frames
        .send(frame(
            1,
            4,
            2,
            full(4, 2),
            Encoding::Rgba,
            solid(4, 2, [10, 20, 30, 255]),
        ))
        .unwrap();
    let message = h.next_message().await;
    assert!(matches!(message, Message::Frame(_)));
    h.update(message);
    assert_eq!(h.state.shown_seq(), Some(1));
    assert_eq!(h.pixel_at(3, 1), [10, 20, 30, 255]);

    // 4x2 surface in a 400x200 panel: scale 100, no bars.
    let panel = Size::new(400.0, 200.0);
    h.clock.set(1_000);
    h.pointer(panel, PointerInput::Moved(Point::new(150.0, 50.0)));
    h.clock.set(2_000);
    h.pointer(
        panel,
        PointerInput::Button {
            at: Point::new(150.0, 50.0),
            button: MouseButton::Left,
            pressed: true,
        },
    );
    h.pointer(
        panel,
        PointerInput::Button {
            at: Point::new(250.0, 150.0),
            button: MouseButton::Left,
            pressed: false,
        },
    );

    let resize = h.next_input().await;
    assert_eq!(
        (resize.seq, resize.input),
        (
            1,
            Input::Resize {
                width: 400,
                height: 200
            }
        )
    );
    let moved = h.next_input().await;
    assert_eq!(
        (moved.seq, moved.sent_us, moved.input),
        (2, 1_000, Input::MouseMove { x: 1.5, y: 0.5 })
    );
    let down = h.next_input().await;
    assert_eq!(down.seq, 3);
    assert_eq!(down.sent_us, 2_000);
    let Input::MouseButton {
        x,
        y,
        button: MouseButton::Left,
        pressed: true,
        ..
    } = down.input
    else {
        panic!("expected a press, got {:?}", down.input);
    };
    assert_eq!((x, y), (1.5, 0.5));
    let up = h.next_input().await;
    assert!(
        matches!(
            up.input,
            Input::MouseButton {
                x: 2.5,
                y: 1.5,
                pressed: false,
                ..
            }
        ),
        "{up:?}"
    );
    assert_eq!(up.seq, 4);
}

#[tokio::test]
async fn region_updates_land_on_the_retained_picture() {
    let mut h = Harness::pixel(4, 4);
    h.update(Message::Frame(frame(
        1,
        4,
        4,
        full(4, 4),
        Encoding::Rgba,
        solid(4, 4, [1, 1, 1, 255]),
    )));
    let region = Rect {
        x: 2,
        y: 1,
        width: 2,
        height: 2,
    };
    h.update(Message::Frame(frame(
        2,
        4,
        4,
        region,
        Encoding::Rgba,
        solid(2, 2, [9, 9, 9, 255]),
    )));
    assert_eq!(h.pixel_at(2, 1), [9, 9, 9, 255]);
    assert_eq!(h.pixel_at(3, 2), [9, 9, 9, 255]);
    assert_eq!(h.pixel_at(1, 1), [1, 1, 1, 255]);
    assert_eq!(h.pixel_at(2, 3), [1, 1, 1, 255]);
}

#[tokio::test]
async fn frames_older_than_the_shown_one_are_dropped() {
    let mut h = Harness::pixel(2, 2);
    h.update(Message::Frame(frame(
        5,
        2,
        2,
        full(2, 2),
        Encoding::Rgba,
        solid(2, 2, [5, 5, 5, 255]),
    )));
    h.update(Message::Frame(frame(
        3,
        2,
        2,
        full(2, 2),
        Encoding::Rgba,
        solid(2, 2, [3, 3, 3, 255]),
    )));
    h.update(Message::Frame(frame(
        5,
        2,
        2,
        full(2, 2),
        Encoding::Rgba,
        solid(2, 2, [6, 6, 6, 255]),
    )));
    assert_eq!(h.state.shown_seq(), Some(5));
    assert_eq!(h.pixel_at(0, 0), [5, 5, 5, 255]);
}

#[tokio::test]
async fn png_and_jpeg_frames_are_decoded_into_their_region() {
    let mut h = Harness::pixel(8, 8);
    let png = loa_surfaces::pixels::RgbaImage::filled(8, 8, [0, 200, 0, 255]).to_png().unwrap();
    h.update(Message::Frame(frame(
        1,
        8,
        8,
        full(8, 8),
        Encoding::Png,
        png,
    )));
    assert_eq!(h.pixel_at(7, 7), [0, 200, 0, 255]);

    let mut jpeg = Vec::new();
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 4, image::Rgb([200, 0, 0])))
        .write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
    h.update(Message::Frame(frame(
        2,
        8,
        8,
        Rect {
            x: 4,
            y: 4,
            width: 4,
            height: 4,
        },
        Encoding::Jpeg,
        jpeg,
    )));
    let [r, g, b, a] = h.pixel_at(5, 5);
    assert!(
        r > 180 && g < 30 && b < 30 && a == 255,
        "{:?}",
        [r, g, b, a]
    );
    assert_eq!(h.pixel_at(3, 3), [0, 200, 0, 255]);
}

#[tokio::test]
async fn video_without_a_decoder_says_so_and_asks_for_a_still_encoding() {
    let mut h = Harness::pixel(2, 2);
    let event = h.update(Message::Frame(frame(
        1,
        2,
        2,
        full(2, 2),
        Encoding::H264,
        vec![0, 0, 0, 1],
    )));
    assert_eq!(
        event,
        Some(Event::EncodingUnsupported {
            uri: h.state.info().uri.clone(),
            encoding: Encoding::H264,
            supported: vec![Encoding::Rgba, Encoding::Png, Encoding::Jpeg],
        })
    );
    assert!(h.state.tab().notice.unwrap().contains("H.264"));
    // Asked once, not on every frame.
    assert_eq!(
        h.update(Message::Frame(frame(
            2,
            2,
            2,
            full(2, 2),
            Encoding::H264,
            vec![0, 0, 0, 1]
        ))),
        None
    );
    // Once the server switches, the picture shows and the notice goes.
    h.update(Message::Frame(frame(
        3,
        2,
        2,
        full(2, 2),
        Encoding::Rgba,
        solid(2, 2, [7, 7, 7, 255]),
    )));
    assert_eq!(h.pixel_at(1, 1), [7, 7, 7, 255]);
    assert_eq!(h.state.tab().notice, None);
}

/// Fills the picture with the first byte of each access unit, fails on 0xFF.
struct FakeDecoder {
    resets: Arc<AtomicUsize>,
}

impl VideoDecoder for FakeDecoder {
    fn supports(&self, encoding: Encoding) -> bool {
        encoding == Encoding::H264
    }

    fn decode(&mut self, frame: &Frame, out: &mut [u8]) -> Result<bool, DecodeError> {
        match frame.data[0] {
            0xFF => Err(DecodeError::Video("corrupt".into())),
            v => {
                out.fill(v);
                Ok(true)
            }
        }
    }

    fn reset(&mut self) {
        self.resets.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn video_goes_through_the_decoder_and_recovers_on_a_keyframe() {
    let resets = Arc::new(AtomicUsize::new(0));
    let options = Options {
        video: Some(Box::new(FakeDecoder {
            resets: resets.clone(),
        })),
        ..Options::default()
    };
    let mut h = Harness::open(SurfaceMode::Pixel, 2, 2, None, options);
    let video = |seq, byte, keyframe| {
        Arc::new(Frame {
            keyframe,
            ..(*frame(seq, 2, 2, full(2, 2), Encoding::H264, vec![byte])).clone()
        })
    };
    h.update(Message::Frame(video(1, 40, true)));
    assert_eq!(h.pixel_at(0, 0), [40; 4]);

    // A broken unit: reset, ask for a keyframe, skip deltas until it comes.
    h.update(Message::Frame(video(2, 0xFF, false)));
    assert_eq!(resets.load(Ordering::SeqCst), 1);
    h.update(Message::Frame(video(3, 50, false)));
    assert_eq!(h.pixel_at(0, 0), [40; 4]);
    h.update(Message::Frame(video(4, 60, true)));
    assert_eq!(h.pixel_at(0, 0), [60; 4]);
    h.update(Message::Frame(video(5, 70, false)));
    assert_eq!(h.pixel_at(0, 0), [70; 4]);
    assert_eq!(h.surface.keyframes_requested(), 1);
}

#[tokio::test]
async fn lost_frames_ask_for_a_keyframe() {
    let mut h = Harness::pixel(2, 2);
    h.update(Message::Frame(frame(
        1,
        2,
        2,
        full(2, 2),
        Encoding::Rgba,
        solid(2, 2, [1; 4]),
    )));
    h.update(Message::Lagged(3));
    assert_eq!(h.surface.keyframes_requested(), 1);
}

#[tokio::test]
async fn pointer_maps_through_letterboxing_and_scale() {
    let mut h = Harness::pixel(800, 600);
    h.update(Message::Modifiers(Modifiers {
        shift: true,
        ..Modifiers::default()
    }));
    // 800x600 in 1000x300: scale 0.5, picture at x 300..700.
    let panel = Size::new(1000.0, 300.0);
    h.pointer(panel, PointerInput::Moved(Point::new(100.0, 150.0)));
    h.pointer(panel, PointerInput::Moved(Point::new(500.0, 150.0)));
    h.pointer(
        panel,
        PointerInput::Scroll {
            at: Point::new(500.0, 150.0),
            dx: 0.0,
            dy: 40.0,
        },
    );
    h.pointer(
        panel,
        PointerInput::Scroll {
            at: Point::new(900.0, 150.0),
            dx: 0.0,
            dy: 40.0,
        },
    );
    let last = Input::MouseMove { x: 0.0, y: 0.0 };
    h.pointer(panel, PointerInput::Moved(Point::new(300.0, 0.0)));
    assert_eq!(
        h.inputs_until(&last).await,
        [
            Input::Resize {
                width: 1000,
                height: 300
            },
            // The move and the scroll over the bars sent nothing.
            Input::MouseMove { x: 400.0, y: 300.0 },
            Input::Scroll {
                x: 400.0,
                y: 300.0,
                dx: 0.0,
                dy: 80.0,
                modifiers: Modifiers {
                    shift: true,
                    ..Modifiers::default()
                }
            },
            last,
        ]
    );
}

#[tokio::test]
async fn the_cursor_follows_the_pointer_with_no_frame_delivered() {
    let mut h = Harness::pixel(800, 600);
    let panel = Size::new(800.0, 600.0);
    h.pointer(panel, PointerInput::Moved(Point::new(10.0, 20.0)));
    let cursor = h.state.cursor().unwrap();
    assert_eq!(
        (cursor.at, cursor.on_surface, cursor.pressed),
        (Point::new(10.0, 20.0), Some(Point::new(10.0, 20.0)), false)
    );
    h.pointer(
        panel,
        PointerInput::Button {
            at: Point::new(30.0, 40.0),
            button: MouseButton::Left,
            pressed: true,
        },
    );
    h.pointer(panel, PointerInput::Moved(Point::new(31.0, 41.0)));
    let cursor = h.state.cursor().unwrap();
    assert_eq!((cursor.at, cursor.pressed), (Point::new(31.0, 41.0), true));
    // Nothing came from the stream.
    assert_eq!(h.state.shown_seq(), None);
    assert!(h.state.framebuffer().is_none());
    h.pointer(
        panel,
        PointerInput::Button {
            at: Point::new(31.0, 41.0),
            button: MouseButton::Left,
            pressed: false,
        },
    );
    h.pointer(panel, PointerInput::Left);
    assert_eq!(h.state.cursor(), None);
}

fn key(text: &str, pressed: bool) -> Message {
    Message::Key(KeyInput {
        key: text.into(),
        code: "KeyX".into(),
        pressed,
        modifiers: Modifiers::default(),
        text: pressed.then(|| text.into()),
    })
}

#[tokio::test]
async fn keys_go_to_the_surface_only_while_it_has_focus() {
    let mut h = Harness::pixel(100, 100);
    let panel = Size::new(100.0, 100.0);
    h.update(key("a", true));
    h.pointer(
        panel,
        PointerInput::Button {
            at: Point::new(5.0, 5.0),
            button: MouseButton::Left,
            pressed: true,
        },
    );
    h.pointer(
        panel,
        PointerInput::Button {
            at: Point::new(5.0, 5.0),
            button: MouseButton::Left,
            pressed: false,
        },
    );
    h.update(key("b", true));
    h.update(key("b", false));
    h.update(Message::Blur);
    h.update(key("c", true));
    h.pointer(panel, PointerInput::Moved(Point::new(7.0, 7.0)));
    let typed: Vec<Input> = h
        .inputs_until(&Input::MouseMove { x: 7.0, y: 7.0 })
        .await
        .into_iter()
        .filter(|i| matches!(i, Input::Text { .. } | Input::Key { .. }))
        .collect();
    assert_eq!(typed, [Input::Text { text: "b".into() }]);
}

#[tokio::test]
async fn latency_meter_pairs_clicks_with_the_frame_that_shows_them() {
    let mut h = Harness::pixel(10, 10);
    // Seqs: 1 is the resize, then a press and a release per round; only the
    // press is measured.
    fn round(h: &mut Harness, press_seq: u64, sent: u64, arrived: u64) {
        let panel = Size::new(10.0, 10.0);
        let at = Point::new(1.0, 1.0);
        h.clock.set(sent);
        h.pointer(
            panel,
            PointerInput::Button {
                at,
                button: MouseButton::Left,
                pressed: true,
            },
        );
        h.pointer(
            panel,
            PointerInput::Button {
                at,
                button: MouseButton::Left,
                pressed: false,
            },
        );
        h.clock.set(arrived);
        let mut f = (*frame(
            press_seq,
            10,
            10,
            full(10, 10),
            Encoding::Rgba,
            solid(10, 10, [0; 4]),
        ))
        .clone();
        f.input_seq = press_seq;
        h.update(Message::Frame(Arc::new(f)));
    }
    round(&mut h, 2, 1_000, 13_000);
    round(&mut h, 4, 20_000, 30_000);
    let r = h.state.tab().latency.unwrap();
    assert_eq!(
        (r.samples, r.median_us, r.p95_us, r.warning),
        (2, 10_000, 12_000, false)
    );
    assert_eq!(loa_ui_surface::latency_label(Some(r)), "10 ms, p95 12 ms");
    round(&mut h, 6, 40_000, 120_000);
    assert!(h.state.tab().latency.unwrap().warning);
}

/// Pages of 100x200 that record what is rendered.
struct FakeRenderers {
    renders: Arc<Mutex<Vec<(usize, f32)>>>,
}

struct FakePages {
    renders: Arc<Mutex<Vec<(usize, f32)>>>,
}

impl PageRenderers for FakeRenderers {
    fn open(&self, document: &Document) -> Result<Box<dyn PageRenderer>, PageError> {
        match document.mime.as_str() {
            "application/pdf" => Ok(Box::new(FakePages {
                renders: self.renders.clone(),
            })),
            other => Err(PageError::Unsupported(other.into())),
        }
    }
}

impl PageRenderer for FakePages {
    fn page_count(&self) -> usize {
        10
    }

    fn page_size(&self, index: usize) -> Option<Size> {
        (index < 10).then_some(Size::new(100.0, 200.0))
    }

    fn render(&self, index: usize, scale: f32) -> Result<RgbaPage, PageError> {
        self.renders.lock().unwrap().push((index, scale));
        let (w, h) = ((100.0 * scale) as u32, (200.0 * scale) as u32);
        Ok(RgbaPage {
            width: w,
            height: h,
            pixels: vec![255; (w * h * 4) as usize],
        })
    }
}

fn pdf() -> Document {
    Document {
        name: "a.pdf".into(),
        mime: "application/pdf".into(),
        data: Bytes::from_static(b"%PDF-1.7"),
    }
}

#[tokio::test]
async fn paged_documents_scroll_and_zoom_without_touching_the_surface() {
    let renders = Arc::new(Mutex::new(Vec::new()));
    let options = Options {
        documents: Some(Arc::new(FakeRenderers {
            renders: renders.clone(),
        })),
        ..Options::default()
    };
    let mut h = Harness::open(SurfaceMode::Document, 100, 200, Some(pdf()), options);
    let loaded = h.next_message().await;
    assert!(matches!(loaded, Message::Document(Ok(_))));
    h.update(loaded);
    assert_eq!(h.state.pages().unwrap().sizes().len(), 10);
    // First page fills the initial view, one more is rendered ahead.
    assert_eq!(
        std::mem::take(&mut *renders.lock().unwrap()),
        [(0, 1.0), (1, 1.0)]
    );

    // Pages are 208 apart; scroll to page 5.
    h.update(Message::DocumentScrolled {
        top: 1040.0,
        viewport: Size::new(300.0, 100.0),
    });
    assert_eq!(
        std::mem::take(&mut *renders.lock().unwrap()),
        [(4, 1.0), (5, 1.0), (6, 1.0)]
    );
    h.update(Message::ZoomIn);
    let zoomed = std::mem::take(&mut *renders.lock().unwrap());
    assert!(
        zoomed.iter().all(|&(_, scale)| scale == 1.25) && !zoomed.is_empty(),
        "{zoomed:?}"
    );

    // Document mode has no input channel: any send would show "closed".
    assert_eq!(h.state.tab().notice, None);
    assert_eq!(h.surface.keyframes_requested(), 0);
}

#[tokio::test]
async fn image_documents_render_without_a_page_renderer() {
    let png = loa_surfaces::pixels::RgbaImage::filled(30, 20, [1, 2, 3, 255]).to_png().unwrap();
    let doc = Document {
        name: "a.png".into(),
        mime: "image/png".into(),
        data: Bytes::from(png),
    };
    let mut h = Harness::open(SurfaceMode::Document, 30, 20, Some(doc), Options::default());
    let loaded = h.next_message().await;
    h.update(loaded);
    assert_eq!(h.state.page_count(), Some(1));
    assert_eq!(h.state.tab().notice, None);
}

#[tokio::test]
async fn documents_without_a_renderer_say_so() {
    let doc = Document {
        name: "a.docx".into(),
        mime: "application/x-unknown".into(),
        data: Bytes::new(),
    };
    let renderers = Arc::new(FakeRenderers {
        renders: Arc::default(),
    });
    let options = Options {
        documents: Some(renderers),
        ..Options::default()
    };
    let mut h = Harness::open(SurfaceMode::Document, 30, 20, Some(doc), options);
    let loaded = h.next_message().await;
    h.update(loaded);
    assert_eq!(h.state.page_count(), None);
    assert_eq!(
        h.state.tab().notice.as_deref(),
        Some("no viewer for application/x-unknown")
    );
}

/// The canvas run headlessly: what it publishes for raw iced events, and
/// what it draws.
mod headless {
    use super::*;
    use iced::mouse;
    use iced_test::{Simulator, simulator};

    const WINDOW: Size = Size::new(400.0, 300.0);

    fn ui(state: &State) -> Simulator<'_, Message> {
        Simulator::with_size(iced::Settings::default(), WINDOW, state.view())
    }

    /// Runs `events` with the pointer at `at` and feeds the messages back.
    fn interact(
        h: &mut Harness,
        at: Point,
        events: impl IntoIterator<Item = iced::Event>,
    ) -> Vec<Message> {
        let messages: Vec<Message> = {
            let mut ui = ui(&h.state);
            ui.point_at(at);
            ui.simulate(events);
            ui.into_messages().collect()
        };
        for message in messages.clone() {
            h.update(message);
        }
        messages
    }

    /// The drawn window as RGBA at twice the logical size.
    fn render(state: &State, dir: &std::path::Path, name: &str) -> image::RgbaImage {
        let dir = dir.join(name);
        // A missing reference image is written, which is how pixels get out.
        // iced_test adds the renderer's name to the file name.
        assert!(
            ui(state)
                .snapshot(&iced::Theme::Dark)
                .unwrap()
                .matches_image(dir.join("shot.png"))
                .unwrap()
        );
        let written = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        image::open(written).unwrap().into_rgba8()
    }

    #[tokio::test]
    async fn a_frame_is_drawn_scaled_into_the_panel() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = Harness::pixel(4, 3);
        h.update(Message::Frame(frame(
            1,
            4,
            3,
            full(4, 3),
            Encoding::Rgba,
            solid(4, 3, [250, 10, 10, 255]),
        )));
        let shot = render(&h.state, dir.path(), "frame");
        // A 4x3 picture fills the middle of the canvas, far from its edges.
        let [r, g, b, _] = shot.get_pixel(400, 340).0;
        assert!(r > 230 && g < 30 && b < 30, "{:?}", [r, g, b]);
    }

    fn moved(at: Point) -> iced::Event {
        iced::Event::Mouse(mouse::Event::CursorMoved { position: at })
    }

    #[tokio::test]
    async fn a_mouse_move_draws_the_cursor_with_no_frame_delivered() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = Harness::pixel(400, 300);
        let before = render(&h.state, dir.path(), "before");

        let at = Point::new(200.0, 200.0);
        let messages = interact(&mut h, at, [moved(at)]);
        assert!(messages.iter().any(|m| matches!(
            m,
            Message::Pointer {
                input: PointerInput::Moved(_),
                ..
            }
        )));
        let cursor = h.state.cursor().unwrap();
        assert!(cursor.on_surface.is_some());
        assert_eq!(h.state.shown_seq(), None);

        let after = render(&h.state, dir.path(), "after");
        // The arrow's body just below and right of the tip, in physical pixels.
        let changed = (0..20u32)
            .flat_map(|dy| (0..12u32).map(move |dx| (400 + dx, 400 + dy)))
            .filter(|&(x, y)| before.get_pixel(x, y) != after.get_pixel(x, y))
            .count();
        assert!(
            changed > 20,
            "only {changed} pixels changed under the cursor"
        );
    }

    #[tokio::test]
    async fn clicking_focuses_the_surface_and_typing_reaches_it() {
        let mut h = Harness::pixel(400, 300);
        let at = Point::new(100.0, 150.0);
        interact(&mut h, at, simulator::click());
        assert!(h.state.focused());
        interact(&mut h, at, simulator::typewrite("hi"));
        let last = Input::Text { text: "i".into() };
        let inputs = h.inputs_until(&last).await;
        let presses: Vec<&Input> = inputs
            .iter()
            .filter(|i| matches!(i, Input::MouseButton { .. }))
            .collect();
        assert_eq!(presses.len(), 2, "{inputs:?}");
        assert!(matches!(
            presses[0],
            Input::MouseButton {
                button: MouseButton::Left,
                pressed: true,
                ..
            }
        ));
        let typed: Vec<&Input> = inputs
            .iter()
            .filter(|i| matches!(i, Input::Text { .. }))
            .collect();
        assert_eq!(typed, [&Input::Text { text: "h".into() }, &last]);
    }

    #[tokio::test]
    async fn wheel_over_the_picture_scrolls_the_surface() {
        let mut h = Harness::pixel(400, 300);
        let at = Point::new(100.0, 150.0);
        let wheel = mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 },
        };
        interact(&mut h, at, [moved(at), iced::Event::Mouse(wheel)]);
        let last = h.state.cursor().unwrap().on_surface.unwrap();
        let inputs = h
            .inputs_until(&Input::MouseMove {
                x: last.x,
                y: last.y,
            })
            .await;
        assert!(inputs.iter().all(|i| !matches!(i, Input::Scroll { .. })));
        let scroll = h.next_input().await.input;
        let Input::Scroll { dy, .. } = scroll else {
            panic!("expected a scroll, got {scroll:?}")
        };
        // One line down, scaled to surface pixels; the content moves down.
        assert!(dy > 0.0, "{dy}");
    }
}
