use super::*;
use crate::{Error, QueueReader, emulator::ViewportCell};
use alacritty_terminal::{index::Side, selection::SelectionType};
use futures::StreamExt;
use iced::keyboard::{Key, Modifiers};
use iced_runtime::{Action, clipboard::Action as Clip, task::into_stream};

const ID: TabId = TabId(1);
fn state() -> State { State::new(Metrics { font: iced::Font::MONOSPACE, font_size: 14.0, cell: iced::Size::new(8.0, 16.0) }, LocalShell::login_shell(std::env::temp_dir())) }
fn remote(host: &str) -> Target { Target::Remote { host: host.into() } }
fn data(id: TabId, b: &'static [u8]) -> Message { Message::Output(id, Ok(ChannelRead::Data(Bytes::from_static(b)))) }
fn key(k: &str, modifiers: Modifiers) -> Message { Message::Canvas(ID, CanvasEvent::Key { key: Key::Character(k.into()), modifiers, text: Some(k.into()) }) }

/// Input and clipboard writes per message sequence, on a tab whose channel never opens so its input stays queued.
#[test]
fn messages_queue_input_and_write_the_clipboard() {
    let clip = if cfg!(target_os = "macos") { Modifiers::LOGO } else { Modifiers::CTRL | Modifiers::SHIFT };
    let canvas = |e| Message::Canvas(ID, e);
    let start = canvas(CanvasEvent::SelectStart { cell: ViewportCell { col: 0, row: 0 }, side: Side::Left, kind: SelectionType::Simple });
    let update = canvas(CanvasEvent::SelectUpdate { cell: ViewportCell { col: 4, row: 0 }, side: Side::Right });
    let bytes = |b: &[u8]| Input::Bytes(Bytes::copy_from_slice(b));
    let cases: Vec<(Vec<Message>, Vec<Input>, &[&str])> = vec![
        (vec![key("l", Modifiers::empty()), key("c", Modifiers::CTRL)], vec![bytes(b"l"), bytes(b"\x03")], &[]),
        (vec![data(ID, b"ab\x1b[6n")], vec![bytes(b"\x1b[1;3R")], &[]),
        (vec![canvas(CanvasEvent::Resized(GridSize::new(60, 20)))], vec![Input::Resize(GridSize::new(60, 20))], &[]),
        (vec![data(ID, b"\x1b[?1049h"), canvas(CanvasEvent::Scroll(2))], vec![bytes(b"\x1b[A\x1b[A")], &[]),
        (vec![data(ID, b"\x1b[?2004h"), Message::Paste(ID, Some("make\x1b\ntest".into()))], vec![bytes(b"\x1b[200~make\ntest\x1b[201~")], &[]),
        (vec![Message::Paste(ID, Some("a\nb\r\nc".into()))], vec![bytes(b"a\rb\rc")], &[]),
        (vec![data(ID, b"hello world"), start, update, canvas(CanvasEvent::SelectEnd), key("C", clip)], vec![], &["hello", "hello"]),
        (vec![data(ID, b"\x1b]52;c;aGk=\x07")], vec![], &["hi"]),
    ];
    for (messages, want_sent, want_clip) in cases {
        let (mut s, mut clipped) = (state(), Vec::new());
        let _ = s.open(remote("h"));
        for m in messages {
            let action = into_stream(s.update(m)).and_then(|mut actions| futures::executor::block_on(actions.next()));
            if let Some(Action::Clipboard(Clip::Write { contents, .. })) = action { clipped.push(contents) }
        }
        let pending = s.tabs[0].pending.as_mut().unwrap();
        assert_eq!((std::iter::from_fn(|| pending.try_recv().ok()).collect::<Vec<_>>(), clipped), (want_sent, want_clip.iter().map(ToString::to_string).collect()));
    }
}

#[test]
fn tabs_follow_titles_selection_closing_and_failures() {
    let mut s = state();
    let (a, b) = (s.open(remote("a")).0, s.open(remote("b")).0);
    for m in [data(b, b"\x1b]0;vim\x07last words"), Message::Select(a), Message::Close(a), Message::Output(b, Err("broken pipe".into())), Message::Output(b, Err("later".into()))] {
        let _ = s.update(m);
    }
    assert_eq!((s.tabs().collect::<Vec<_>>(), s.active(), s.title(b)), (vec![b], Some(b), Some("vim")));
    assert_eq!(s.status(b), Some(&Status::Failed("broken pipe".into())));
    assert!(s.tab(b).unwrap().emulator.screen_text().contains("last words"), "a failure keeps the screen");
    let mut ui = iced_test::simulator(s.view());
    ui.click("vim").expect("tab title shown");
    assert!(ui.into_messages().any(|m| matches!(m, Message::Select(id) if id == b)));
    let _ = s.update(Message::Output(b, Ok(ChannelRead::Closed { exit_code: Some(0) })));
    assert!(s.is_empty() && s.active().is_none());
}

/// Hands out one channel, then refuses.
struct Host(Mutex<Option<PtyChannel>>);
#[async_trait::async_trait]
impl RemotePty for Host {
    async fn open_pty(&self, host: &str, _: GridSize) -> Result<PtyChannel> { self.0.locked().take().ok_or_else(|| Error::Remote { host: host.into(), message: "refused".into() }) }
}

/// The writer side the test reads back: `Some` bytes, `None` on close.
struct Sent(mpsc::UnboundedSender<Option<Bytes>>);
#[async_trait::async_trait]
impl ChannelWriter for Sent {
    async fn write(&mut self, b: Bytes) -> Result<()> { self.0.send(Some(b)).map_err(|_| Error::Closed) }
    async fn resize(&mut self, _: GridSize) -> Result<()> { Ok(()) }
    async fn close(&mut self) -> Result<()> { self.0.send(None).map_err(|_| Error::Closed) }
}

#[tokio::test]
async fn remote_channel_streams_both_ways_until_the_program_ends() {
    let ((out, reader), (writer, mut sent)) = (tokio::sync::mpsc::channel(4), mpsc::unbounded_channel());
    let mut s = state().with_remote(Arc::new(Host(Mutex::new(Some(PtyChannel { reader: Box::new(QueueReader::new(reader)), writer: Box::new(Sent(writer)) })))));
    let ((first, open), (down, refused)) = (s.open(remote("box")), s.open(remote("down")));
    let _ = s.update(key("l", Modifiers::empty()));
    out.send(Ok(ChannelRead::Data(Bytes::from_static(b"\x1b[6n")))).await.unwrap();
    out.send(Ok(ChannelRead::Closed { exit_code: Some(0) })).await.unwrap();
    let mut tasks: futures::stream::SelectAll<_> = into_stream(open).into_iter().chain(into_stream(refused)).collect();
    while let Some(action) = tasks.next().await {
        if let Action::Output(m) = action { tasks.extend(into_stream(s.update(m))) }
    }
    let written: Vec<Option<Bytes>> = std::iter::from_fn(|| sent.try_recv().ok()).collect();
    assert_eq!(written, [Some(Bytes::from_static(b"l")), Some(Bytes::from_static(b"\x1b[1;1R")), None]);
    assert!(s.tab(first).is_none() && matches!(s.status(down), Some(Status::Failed(e)) if e.contains("refused")));
}
