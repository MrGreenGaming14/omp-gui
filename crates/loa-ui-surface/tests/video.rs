//! A tab over a live surface coded as video: the renderer negotiates H.264,
//! decodes it in process, and its header names the codec and encoder.

use futures::StreamExt;
use iced::Task;
use iced_runtime::Action;
use loa_contracts::surface::{Encoding, Input, InputEvent, Modifiers, MouseButton, Surface};
use loa_surface_bench::encode::probe;
use loa_surface_bench::live::{LiveVideo, VideoSurface};
use loa_surface_bench::synthetic::{SyntheticConfig, SyntheticSurface};
use loa_ui_surface::{H264Decoder, Message, Options, State, stream_label};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

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

/// Feeds messages to the state until `done` holds.
async fn run_until(state: &mut State, messages: &mut mpsc::UnboundedReceiver<Message>, out: &mpsc::UnboundedSender<Message>, done: impl Fn(&State) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !done(state) {
        let message = tokio::time::timeout_at(deadline, messages.recv()).await.expect("in time").unwrap();
        let (task, _) = state.update(message);
        spawn(task, out.clone());
    }
}

fn video_surface() -> Arc<VideoSurface> {
    let source: Arc<dyn Surface> = Arc::new(SyntheticSurface::new(SyntheticConfig::default()).unwrap());
    VideoSurface::new(source, Arc::new(LiveVideo::with_probed(None, probe(None))), true).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tab_with_a_decoder_shows_h264_and_names_the_encoder() {
    let surface = video_surface();
    let options = Options { video: Some(Box::new(H264Decoder::new())), ..Options::default() };
    let (mut state, task) = State::open(surface.clone(), options);
    let (out, mut messages) = mpsc::unbounded_channel();
    spawn(task, out.clone());

    run_until(&mut state, &mut messages, &out, |s| s.shown_seq().is_some() && s.stream().is_some_and(|st| st.encoding == Encoding::H264)).await;
    let stream = state.stream().unwrap();
    assert_eq!(stream_label(&stream), "H.264 · rusty_h264 · software");
    assert_eq!(state.notice(), None);

    // A click in the surface comes back as decoded video.
    let click = InputEvent {
        seq: 1,
        sent_us: 0,
        input: Input::MouseButton { x: 50.0, y: 50.0, button: MouseButton::Left, pressed: true, modifiers: Modifiers::default() },
    };
    let shown = state.shown_seq();
    surface.input(click).await.unwrap();
    run_until(&mut state, &mut messages, &out, |s| s.shown_seq() > shown).await;
    assert_eq!(state.notice(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tab_without_a_decoder_gets_stills_from_the_same_surface() {
    let (mut state, task) = State::open(video_surface(), Options::default());
    let (out, mut messages) = mpsc::unbounded_channel();
    spawn(task, out.clone());
    run_until(&mut state, &mut messages, &out, |s| s.shown_seq().is_some()).await;
    let stream = state.stream().unwrap();
    assert_eq!(stream.encoding, Encoding::Jpeg);
    assert_eq!(stream_label(&stream), "JPEG");
}
