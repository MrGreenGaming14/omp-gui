//! The tab driven like the iced runtime would, against an in-process remote host.

use super::*;
use crate::Error;
use crate::emulator::ViewportCell;
use alacritty_terminal::index::Side;
use alacritty_terminal::selection::SelectionType;
use futures::StreamExt;
use futures::stream::{BoxStream, SelectAll};
use iced::keyboard::{Key, Modifiers};
use iced_runtime::{Action, clipboard::Action as Clip};
use std::time::Duration;

/// What the pane sent to the remote side.
#[derive(Debug, PartialEq)]
enum Sent {
    Data(Bytes),
    Resize(GridSize),
    Close,
}

/// The remote end of one channel: sees what the pane sends and answers.
struct Session {
    host: String,
    size: GridSize,
    sent: mpsc::UnboundedReceiver<Sent>,
    out: mpsc::UnboundedSender<Result<ChannelRead>>,
}

struct Loopback(mpsc::UnboundedSender<Session>);
struct Reader(mpsc::UnboundedReceiver<Result<ChannelRead>>);
struct Writer(mpsc::UnboundedSender<Sent>);

#[async_trait::async_trait]
impl RemotePty for Loopback {
    async fn open_pty(&self, host: &str, size: GridSize) -> Result<PtyChannel> {
        let ((sent_tx, sent), (out, out_rx)) = (mpsc::unbounded_channel(), mpsc::unbounded_channel());
        self.0.send(Session { host: host.into(), size, sent, out }).map_err(|_| Error::Remote { host: host.into(), message: "connection refused".into() })?;
        Ok(PtyChannel { reader: Box::new(Reader(out_rx)), writer: Box::new(Writer(sent_tx)) })
    }
}

#[async_trait::async_trait]
impl ChannelReader for Reader {
    async fn read(&mut self) -> Result<ChannelRead> { self.0.recv().await.unwrap_or(Err(Error::Closed)) }
}

#[async_trait::async_trait]
impl ChannelWriter for Writer {
    async fn write(&mut self, b: Bytes) -> Result<()> { self.0.send(Sent::Data(b)).map_err(|_| Error::Closed) }
    async fn resize(&mut self, s: GridSize) -> Result<()> { self.0.send(Sent::Resize(s)).map_err(|_| Error::Closed) }
    async fn close(&mut self) -> Result<()> { self.0.send(Sent::Close).map_err(|_| Error::Closed) }
}

struct Harness {
    state: State,
    tasks: SelectAll<BoxStream<'static, Action<Message>>>,
    clipboard: Option<String>,
    events: Vec<Event>,
    sessions: mpsc::UnboundedReceiver<Session>,
}

impl Harness {
    fn new() -> Self {
        let (tx, sessions) = mpsc::unbounded_channel();
        let metrics = Metrics { font: iced::Font::MONOSPACE, font_size: 14.0, cell: iced::Size::new(8.0, 16.0) };
        let state = State::new(metrics, LocalShell::login_shell(std::env::temp_dir())).with_remote(Arc::new(Loopback(tx)));
        Self { state, tasks: SelectAll::new(), clipboard: None, events: Vec::new(), sessions }
    }

    fn send(&mut self, message: Message) {
        let (task, event) = self.state.update(message);
        self.tasks.extend(iced_runtime::task::into_stream(task));
        self.events.extend(event);
    }

    fn open(&mut self, host: &str) -> TabId {
        let (id, task) = self.state.open(Target::Remote { host: host.into() });
        self.tasks.extend(iced_runtime::task::into_stream(task));
        id
    }

    fn key(&mut self, id: TabId, key: &str, modifiers: Modifiers) { self.send(Message::Canvas(id, CanvasEvent::Key { key: Key::Character(key.into()), modifiers, text: Some(key.into()) })) }

    /// Applies the next task action; clipboard actions hit an in-memory clipboard.
    async fn step(&mut self) {
        match self.tasks.next().await {
            Some(Action::Output(m)) => self.send(m),
            Some(Action::Clipboard(Clip::Write { contents, target: iced::advanced::clipboard::Kind::Standard })) => self.clipboard = Some(contents),
            Some(Action::Clipboard(Clip::Read { channel, .. })) => { let _ = channel.send(self.clipboard.clone()); }
            Some(_) => {}
            None => futures::future::pending().await,
        }
    }

    /// Runs tasks until `f` yields, rechecking every few ms (a write loop makes progress
    /// without yielding), panicking after a while.
    async fn until<T>(&mut self, mut f: impl FnMut(&mut Self) -> Option<T>) -> T {
        let tick = || tokio::time::sleep(Duration::from_millis(5));
        let run = async {
            loop {
                if let Some(t) = f(self) { return t }
                tokio::select! { () = self.step() => {}, () = tick() => {} }
            }
        };
        tokio::time::timeout(Duration::from_secs(10), run).await.expect("in time")
    }

    async fn accept(&mut self) -> Session { self.until(|h| h.sessions.try_recv().ok()).await }

    async fn sent(&mut self, s: &mut Session) -> Sent { self.until(|_| s.sent.try_recv().ok()).await }

    async fn screen(&mut self, id: TabId, needle: &str) { self.until(|h| h.state.emulator(id).is_some_and(|e| e.screen_text().join("\n").contains(needle)).then_some(())).await }
}

#[tokio::test]
async fn remote_tab_streams_both_ways_and_ends_with_the_program() {
    let mut h = Harness::new();
    h.send(Message::Canvas(TabId(0), CanvasEvent::Resized(GridSize { cols: 100, rows: 30, cell_width: 8, cell_height: 16 })));
    let id = h.open("build-box");
    let mut s = h.accept().await;
    assert_eq!((s.host.as_str(), s.size.cols, s.size.rows, h.state.title(id)), ("build-box", 100, 30, Some("build-box")));
    s.out.send(Ok(ChannelRead::Data(Bytes::from_static(b"\x1b[?2004hhello world")))).unwrap();
    h.screen(id, "hello world").await;
    h.key(id, "l", Modifiers::empty());
    h.key(id, "c", Modifiers::CTRL);
    assert_eq!((h.sent(&mut s).await, h.sent(&mut s).await), (Sent::Data(Bytes::from_static(b"l")), Sent::Data(Bytes::from_static(b"\x03"))));
    h.send(Message::Canvas(id, CanvasEvent::Resized(GridSize::new(60, 20))));
    assert_eq!(h.sent(&mut s).await, Sent::Resize(GridSize::new(60, 20)));
    s.out.send(Ok(ChannelRead::Data(Bytes::from_static(b"\x1b[6n")))).unwrap();
    assert_eq!(h.sent(&mut s).await, Sent::Data(Bytes::from_static(b"\x1b[1;12R")), "a cursor query gets its answer");
    h.send(Message::Canvas(id, CanvasEvent::SelectStart { cell: ViewportCell { col: 0, row: 0 }, side: Side::Left, kind: SelectionType::Simple }));
    h.send(Message::Canvas(id, CanvasEvent::SelectUpdate { cell: ViewportCell { col: 4, row: 0 }, side: Side::Right }));
    let chord = if cfg!(target_os = "macos") { Modifiers::LOGO } else { Modifiers::CTRL | Modifiers::SHIFT };
    h.key(id, "C", chord);
    assert_eq!(h.until(|h| h.clipboard.clone()).await, "hello");
    h.clipboard = Some("make\ntest".into());
    h.key(id, "V", chord);
    assert_eq!(h.sent(&mut s).await, Sent::Data(Bytes::from_static(b"\x1b[200~make\ntest\x1b[201~")));
    s.out.send(Ok(ChannelRead::Closed { exit_code: Some(0) })).unwrap();
    assert_eq!(h.until(|h| h.events.pop()).await, Event::Exited { tab: id, exit_code: Some(0) });
    assert!(h.state.is_empty());
}

#[tokio::test]
async fn closing_one_tab_hangs_up_only_it_and_failures_keep_the_screen() {
    let mut h = Harness::new();
    let (first, second) = (h.open("a"), h.open("b"));
    let (mut a, b) = (h.accept().await, h.accept().await);
    h.until(|h| (h.state.status(second) == Some(&Status::Running)).then_some(())).await;
    h.send(Message::Select(first));
    h.send(Message::Close(first));
    assert_eq!((h.events.pop(), h.state.tabs().collect::<Vec<_>>(), h.state.active()), (Some(Event::Closed { tab: first }), vec![second], Some(second)));
    assert_eq!(h.sent(&mut a).await, Sent::Close);
    b.out.send(Ok(ChannelRead::Data(Bytes::from_static(b"last words")))).unwrap();
    h.screen(second, "last words").await;
    b.out.send(Err(Error::Remote { host: "b".into(), message: "broken pipe".into() })).unwrap();
    assert!(matches!(h.until(|h| h.events.pop()).await, Event::Failed { error, .. } if error.contains("broken pipe")));
    h.screen(second, "last words").await;
    drop(b);
    h.sessions.close();
    let down = h.open("down");
    assert!(matches!(h.until(|h| h.events.pop()).await, Event::Failed { tab, error } if tab == down && error.contains("down")));
    let mut ui = iced_test::simulator(h.state.view());
    ui.click("down").expect("tab title shown");
    assert!(ui.into_messages().any(|m| matches!(m, Message::Select(id) if id == down)));
}
