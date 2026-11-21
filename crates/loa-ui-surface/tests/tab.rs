//! A surface tab over a fake `Surface`, driven through `State::update` and the tasks it returns.

use bytes::Bytes;
use futures::StreamExt;
use iced::{Point, Size};
use iced_runtime::Action;
use loa_contracts::surface::{Document, Encoding, Frame, Input, InputEvent, Modifiers, MouseButton, Rect, SurfaceInfo, SurfaceMode, SurfaceUri};
use loa_surfaces::pixels::RgbaImage;
use loa_surfaces::testkit::{FakeSurface, Probe, solid};
use loa_ui_surface::{Clock, DecodeError, Event, FrameDecoder, Framebuffer, KeyInput, Message, Options, PointerInput, State};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tokio::sync::{broadcast, mpsc};

#[derive(Default)]
struct FakeClock(AtomicU64);

impl Clock for FakeClock {
    fn now_us(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Harness {
    state: State,
    surface: Arc<FakeSurface>,
    frames: broadcast::Sender<Arc<Frame>>,
    clock: Arc<FakeClock>,
    messages: mpsc::UnboundedReceiver<Message>,
    inputs: mpsc::UnboundedReceiver<InputEvent>,
}

fn full(width: u32, height: u32) -> Rect {
    Rect { x: 0, y: 0, width, height }
}

fn frame(seq: u64, (width, height): (u32, u32), region: Rect, encoding: Encoding, data: Vec<u8>) -> Arc<Frame> {
    let keyframe = region == full(width, height);
    Arc::new(Frame { seq, captured_us: 0, input_seq: 0, width, height, region, encoding, keyframe, data: Bytes::from(data) })
}

fn button(at: Point, pressed: bool) -> PointerInput {
    PointerInput::Button { at, button: MouseButton::Left, pressed }
}

impl Harness {
    fn open(mode: SurfaceMode, (width, height): (u32, u32), document: Option<Document>, options: Options) -> Self {
        let mime = document.as_ref().map(|d| d.mime.clone());
        let info = SurfaceInfo { uri: SurfaceUri::new("fake", "one").unwrap(), name: "Fake".into(), width, height, mode, mime };
        let (surface, Probe { frames, inputs, .. }) = FakeSurface::new(info, document);
        let clock = Arc::new(FakeClock::default());
        let (state, task) = State::open(surface.clone(), &Options { clock: clock.clone(), ..options });
        let (out, messages) = mpsc::unbounded_channel();
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        tokio::spawn(async move {
            while let Some(action) = stream.next().await {
                if let Action::Output(message) = action {
                    let _ = out.send(message);
                }
            }
        });
        Self { state, surface, frames, clock, messages, inputs }
    }

    fn pixel(width: u32, height: u32) -> Self {
        Self::open(SurfaceMode::Pixel, (width, height), None, Options::default())
    }

    /// Updates and runs the returned task to completion; surface calls must go through.
    fn update(&mut self, message: Message) -> Option<Event> {
        let (task, event) = self.state.update(message);
        let actions = iced_runtime::task::into_stream(task).map(|s| futures::executor::block_on(s.collect::<Vec<_>>()));
        for action in actions.unwrap_or_default() {
            if let Action::Output(Message::Failed(e)) = action {
                panic!("surface call failed: {e}");
            }
        }
        event
    }

    fn pointer(&mut self, panel: Size, input: PointerInput) {
        self.update(Message::Pointer { panel, input });
    }

    /// Inputs the surface received up to `last`; delivery is ordered.
    async fn inputs_until(&mut self, last: &Input) -> Vec<Input> {
        let mut out = Vec::new();
        while out.last() != Some(last) {
            out.push(self.inputs.recv().await.unwrap().input);
        }
        out
    }

    fn pixel_at(&self, x: u32, y: u32) -> [u8; 4] {
        let fb = self.state.pixel().unwrap().framebuffer().unwrap();
        let i = ((y * fb.width() + x) * 4) as usize;
        fb.pixels()[i..i + 4].try_into().unwrap()
    }
}

#[tokio::test]
async fn streamed_frames_are_drawn_and_input_reaches_the_surface_in_order() {
    let mut h = Harness::pixel(4, 2);
    h.frames.send(frame(1, (4, 2), full(4, 2), Encoding::Rgba, solid(4, 2, [10, 20, 30, 255]))).unwrap();
    let message = h.messages.recv().await.unwrap();
    assert!(matches!(message, Message::Frame(_)));
    h.update(message);
    assert_eq!((h.state.pixel().unwrap().shown_seq(), h.pixel_at(3, 1)), (Some(1), [10, 20, 30, 255]));

    // 4x2 surface in a 400x200 panel: scale 100, no bars.
    let panel = Size::new(400.0, 200.0);
    h.clock.0.store(1_000, Ordering::SeqCst);
    h.pointer(panel, PointerInput::Moved(Point::new(150.0, 50.0)));
    h.clock.0.store(2_000, Ordering::SeqCst);
    h.pointer(panel, button(Point::new(150.0, 50.0), true));
    h.pointer(panel, button(Point::new(250.0, 150.0), false));
    let left = MouseButton::Left;
    let mut sent = Vec::new();
    for _ in 0..4 {
        let e = h.inputs.recv().await.unwrap();
        sent.push((e.seq, e.sent_us, e.input));
    }
    let press = |x, y, pressed| Input::MouseButton { x, y, button: left, pressed, modifiers: Modifiers::default() };
    assert_eq!(
        sent,
        [
            (1, 1_000, Input::Resize { width: 400, height: 200 }),
            (2, 1_000, Input::MouseMove { x: 1.5, y: 0.5 }),
            (3, 2_000, press(1.5, 0.5, true)),
            (4, 2_000, press(2.5, 1.5, false)),
        ]
    );
}

#[tokio::test]
async fn still_frames_land_on_the_retained_picture_in_order() {
    let mut h = Harness::pixel(8, 8);
    let mut jpeg = Vec::new();
    let red = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 4, image::Rgb([200, 0, 0])));
    red.write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
    let rect = |x, y, w, h| Rect { x, y, width: w, height: h };
    // (seq, region, encoding, data): later regions overwrite, older frames are dropped.
    let frames = [
        (5, full(8, 8), Encoding::Rgba, solid(8, 8, [1, 1, 1, 255])),
        (6, rect(2, 1, 2, 2), Encoding::Rgba, solid(2, 2, [9, 9, 9, 255])),
        (3, full(8, 8), Encoding::Rgba, solid(8, 8, [3, 3, 3, 255])),
        (7, rect(0, 4, 4, 4), Encoding::Png, RgbaImage::filled(4, 4, [0, 200, 0, 255]).to_png().unwrap()),
        (8, rect(4, 4, 4, 4), Encoding::Jpeg, jpeg),
    ];
    for (seq, region, encoding, data) in frames {
        assert_eq!(h.update(Message::Frame(frame(seq, (8, 8), region, encoding, data))), None);
    }
    assert_eq!(h.state.pixel().unwrap().shown_seq(), Some(8));
    assert_eq!([h.pixel_at(2, 1), h.pixel_at(3, 2), h.pixel_at(1, 1), h.pixel_at(0, 7)], [[9, 9, 9, 255], [9, 9, 9, 255], [1, 1, 1, 255], [0, 200, 0, 255]]);
    let [r, g, b, a] = h.pixel_at(5, 5);
    assert!(r > 180 && g < 30 && b < 30 && a == 255, "{:?}", [r, g, b, a]);
    assert_eq!(h.state.stream().map(|s| loa_ui_surface::stream_label(&s)).as_deref(), Some("JPEG"));
}

#[tokio::test]
async fn video_without_a_decoder_asks_once_for_a_still_encoding() {
    let mut h = Harness::pixel(2, 2);
    let h264 = |seq| frame(seq, (2, 2), full(2, 2), Encoding::H264, vec![0, 0, 0, 1]);
    let uri = h.state.info().uri.clone();
    let supported = vec![Encoding::Rgba, Encoding::Png, Encoding::Jpeg];
    assert_eq!(h.update(Message::Frame(h264(1))), Some(Event::EncodingUnsupported { uri, encoding: Encoding::H264, supported }));
    assert!(h.state.tab().notice.unwrap().contains("H.264"));
    assert_eq!(h.update(Message::Frame(h264(2))), None, "asked once, not on every frame");
    // Once the server switches, the picture shows and the notice goes.
    h.update(Message::Frame(frame(3, (2, 2), full(2, 2), Encoding::Rgba, solid(2, 2, [7, 7, 7, 255]))));
    assert_eq!((h.pixel_at(1, 1), h.state.tab().notice), ([7, 7, 7, 255], None));
}

/// Fills the picture with the first byte of each access unit, fails on 0xFF.
struct FakeDecoder(Arc<AtomicUsize>);

impl FrameDecoder for FakeDecoder {
    fn accepts(&self) -> &[Encoding] {
        &[Encoding::H264]
    }

    fn apply(&mut self, frame: &Frame, fb: &mut Framebuffer) -> Result<bool, DecodeError> {
        match frame.data[0] {
            0xFF => Err(DecodeError::Video("corrupt".into())),
            v => {
                fb.pixels_mut().fill(v);
                Ok(true)
            }
        }
    }

    fn reset(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn the_decoder_chain_recovers_video_on_a_keyframe_and_lag_asks_for_one() {
    let resets = Arc::new(AtomicUsize::new(0));
    let decoder = resets.clone();
    let options = Options { decoder: Some(Arc::new(move || Box::new(FakeDecoder(decoder.clone())))), ..Options::default() };
    let mut h = Harness::open(SurfaceMode::Pixel, (2, 2), None, options);
    let video = |seq, byte, keyframe| Arc::new(Frame { keyframe, ..(*frame(seq, (2, 2), full(2, 2), Encoding::H264, vec![byte])).clone() });
    // A broken unit resets and asks for a keyframe; deltas are skipped until it comes.
    for (seq, byte, keyframe, shown) in [(1, 40, true, 40), (2, 0xFF, false, 40), (3, 50, false, 40), (4, 60, true, 60), (5, 70, false, 70)] {
        h.update(Message::Frame(video(seq, byte, keyframe)));
        assert_eq!(h.pixel_at(0, 0), [shown; 4], "frame {seq}");
    }
    assert_eq!((resets.load(Ordering::SeqCst), h.surface.keyframes_requested()), (1, 1));
    h.update(Message::Lagged(3));
    assert_eq!((resets.load(Ordering::SeqCst), h.surface.keyframes_requested()), (2, 2));
}

#[tokio::test]
async fn pointer_and_wheel_map_through_letterboxing_and_scale() {
    let mut h = Harness::pixel(800, 600);
    let shift = Modifiers { shift: true, ..Modifiers::default() };
    h.update(Message::Modifiers(shift));
    // 800x600 in 1000x300: scale 0.5, picture at x 300..700. Over the bars nothing is sent.
    let panel = Size::new(1000.0, 300.0);
    h.pointer(panel, PointerInput::Moved(Point::new(100.0, 150.0)));
    h.pointer(panel, PointerInput::Moved(Point::new(500.0, 150.0)));
    h.pointer(panel, PointerInput::Scroll { at: Point::new(500.0, 150.0), dx: 0.0, dy: 40.0 });
    h.pointer(panel, PointerInput::Scroll { at: Point::new(900.0, 150.0), dx: 0.0, dy: 40.0 });
    h.pointer(panel, PointerInput::Moved(Point::new(300.0, 0.0)));
    let last = Input::MouseMove { x: 0.0, y: 0.0 };
    let scroll = Input::Scroll { x: 400.0, y: 300.0, dx: 0.0, dy: 80.0, modifiers: shift };
    let sent = h.inputs_until(&last).await;
    assert_eq!(sent, [Input::Resize { width: 1000, height: 300 }, Input::MouseMove { x: 400.0, y: 300.0 }, scroll, last]);
}

#[tokio::test]
async fn the_cursor_follows_the_pointer_and_keys_go_out_only_with_focus() {
    let mut h = Harness::pixel(100, 100);
    let panel = Size::new(100.0, 100.0);
    let key = |text: &str, pressed| Message::Key(KeyInput { key: text.into(), code: "KeyX".into(), pressed, modifiers: Modifiers::default(), text: pressed.then(|| text.into()) });
    h.update(key("a", true));
    h.pointer(panel, PointerInput::Moved(Point::new(10.0, 20.0)));
    let cursor = |h: &Harness| h.state.pixel().unwrap().cursor().map(|c| (c.at, c.on_surface, c.pressed));
    assert_eq!(cursor(&h), Some((Point::new(10.0, 20.0), Some(Point::new(10.0, 20.0)), false)));
    h.pointer(panel, button(Point::new(5.0, 5.0), true));
    h.pointer(panel, PointerInput::Moved(Point::new(31.0, 41.0)));
    assert_eq!(cursor(&h).map(|c| c.2), Some(true), "dragging");
    h.pointer(panel, button(Point::new(31.0, 41.0), false));
    h.update(key("b", true));
    h.update(key("b", false));
    h.update(Message::Blur);
    h.update(key("c", true));
    h.pointer(panel, PointerInput::Left);
    assert_eq!((cursor(&h), h.state.pixel().unwrap().shown_seq()), (None, None), "no frame was needed");
    h.pointer(panel, PointerInput::Moved(Point::new(7.0, 7.0)));
    let sent = h.inputs_until(&Input::MouseMove { x: 7.0, y: 7.0 }).await;
    let typed: Vec<_> = sent.into_iter().filter(|i| matches!(i, Input::Text { .. } | Input::Key { .. })).collect();
    assert_eq!(typed, [Input::Text { text: "b".into() }]);
}

#[tokio::test]
async fn the_latency_meter_pairs_clicks_with_the_frame_that_shows_them() {
    let mut h = Harness::pixel(10, 10);
    // Seq 1 is the resize, then a press and a release per round; only the press is measured.
    let mut round = |press_seq: u64, sent: u64, arrived: u64| {
        let (panel, at) = (Size::new(10.0, 10.0), Point::new(1.0, 1.0));
        h.clock.0.store(sent, Ordering::SeqCst);
        h.pointer(panel, button(at, true));
        h.pointer(panel, button(at, false));
        h.clock.0.store(arrived, Ordering::SeqCst);
        let shown = Frame { input_seq: press_seq, ..(*frame(press_seq, (10, 10), full(10, 10), Encoding::Rgba, solid(10, 10, [0; 4]))).clone() };
        h.update(Message::Frame(Arc::new(shown)));
        h.state.tab().latency.unwrap()
    };
    round(2, 1_000, 13_000);
    let r = round(4, 20_000, 30_000);
    assert_eq!((r.samples, r.median_us, r.p95_us, r.warning), (2, 10_000, 12_000, false));
    assert_eq!(loa_ui_surface::latency_label(Some(r)), "10 ms, p95 12 ms");
    assert!(round(6, 40_000, 120_000).warning);
}

#[tokio::test]
async fn documents_render_here_and_zoom_without_touching_the_surface() {
    let mut book = rust_xlsxwriter::Workbook::new();
    book.add_worksheet().write_string(0, 0, "cell").unwrap();
    let xlsx = Document { name: "a.xlsx".into(), mime: loa_surfaces::docs::Format::Xlsx.mime().into(), data: Bytes::from(book.save_to_buffer().unwrap()) };
    let png = Document { name: "a.png".into(), mime: "image/png".into(), data: RgbaImage::filled(30, 20, [1, 2, 3, 255]).to_png().unwrap().into() };
    let unknown = Document { name: "a.bin".into(), mime: "application/x-unknown".into(), data: Bytes::new() };
    // (document, pages, notice)
    for (document, pages, notice) in [(xlsx, Some(1), None), (png, Some(1), None), (unknown, None, Some("no viewer for application/x-unknown"))] {
        let mut h = Harness::open(SurfaceMode::Document, (300, 200), Some(document), Options::default());
        let loaded = h.messages.recv().await.unwrap();
        h.update(loaded);
        h.update(Message::ZoomIn);
        let doc = h.state.document().unwrap();
        assert_eq!((doc.page_count(), h.state.notice().as_deref(), doc.zoom()), (pages, notice, 1.25));
        if let Some(pages) = doc.pages() {
            assert_eq!(pages.rendered(0).map(|(zoom, _)| *zoom), Some(1.25), "the visible page renders at the new zoom");
        }
        assert_eq!((h.surface.keyframes_requested(), h.state.pixel().is_none()), (0, true));
    }
}

/// The canvas run headlessly: what it draws and what it publishes for raw iced events.
#[tokio::test]
async fn the_canvas_draws_frames_and_the_cursor_and_takes_clicks_and_typing() {
    use iced_test::{Simulator, simulator};
    let dir = tempfile::tempdir().unwrap();
    let mut h = Harness::pixel(4, 3);
    fn ui(state: &State) -> Simulator<'_, Message> {
        Simulator::with_size(iced::Settings::default(), Size::new(400.0, 300.0), state.view())
    }
    // A missing reference image is written, which is how pixels get out; iced_test adds the
    // renderer's name to the file name.
    let render = |state: &State, name: &str| {
        assert!(ui(state).snapshot(&iced::Theme::Dark).unwrap().matches_image(dir.path().join(name).join("shot.png")).unwrap());
        image::open(std::fs::read_dir(dir.path().join(name)).unwrap().next().unwrap().unwrap().path()).unwrap().into_rgba8()
    };
    let before = render(&h.state, "before");

    let at = Point::new(200.0, 200.0);
    let run = |h: &mut Harness, events: Vec<iced::Event>| {
        let messages: Vec<Message> = {
            let mut ui = ui(&h.state);
            ui.point_at(at);
            ui.simulate(events);
            ui.into_messages().collect()
        };
        for message in messages {
            h.update(message);
        }
    };
    run(&mut h, vec![iced::Event::Mouse(iced::mouse::Event::CursorMoved { position: at })]);
    assert!(h.state.pixel().unwrap().cursor().is_some_and(|c| c.on_surface.is_some()));
    let after = render(&h.state, "after");
    // The arrow's body just below and right of the tip, in physical pixels.
    let changed = (0..20u32).flat_map(|dy| (0..12u32).map(move |dx| (400 + dx, 400 + dy))).filter(|&(x, y)| before.get_pixel(x, y) != after.get_pixel(x, y)).count();
    assert!(changed > 20, "only {changed} pixels changed under the cursor");
    // A 4x3 picture fills the middle of the canvas.
    h.update(Message::Frame(frame(1, (4, 3), full(4, 3), Encoding::Rgba, solid(4, 3, [250, 10, 10, 255]))));
    let shown = render(&h.state, "frame");
    let [r, g, b, _] = shown.get_pixel(400, 340).0;
    assert!(r > 230 && g < 30 && b < 30, "{:?}", [r, g, b]);

    run(&mut h, simulator::click().collect());
    assert!(h.state.pixel().unwrap().focused());
    run(&mut h, simulator::typewrite("hi").collect());
    let inputs = h.inputs_until(&Input::Text { text: "i".into() }).await;
    let presses = inputs.iter().filter(|i| matches!(i, Input::MouseButton { button: MouseButton::Left, .. })).count();
    let typed: Vec<_> = inputs.iter().filter(|i| matches!(i, Input::Text { .. })).collect();
    assert_eq!((presses, typed), (2, vec![&Input::Text { text: "h".into() }, &Input::Text { text: "i".into() }]));
}
