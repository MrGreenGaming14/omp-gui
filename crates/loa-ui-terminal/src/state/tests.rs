use std::time::Duration;

use alacritty_terminal::index::Side;
use alacritty_terminal::selection::SelectionType;
use futures::StreamExt;
use futures::stream::{BoxStream, SelectAll};
use iced::keyboard::key::Named;
use iced::keyboard::{Key, Modifiers};
use iced::{Font, Size};
use iced_runtime::Action;
use iced_runtime::clipboard::Action as ClipboardAction;

use super::*;
use crate::emulator::ViewportCell;
use crate::loopback::{LoopbackHost, LoopbackSession, RemoteInput};

const LIMIT: Duration = Duration::from_secs(10);

fn metrics() -> Metrics {
    Metrics {
        font: Font::MONOSPACE,
        font_size: 14.0,
        cell: Size::new(8.0, 16.0),
    }
}

/// Runs the tab like the iced runtime would: every task's output is fed
/// back into `update`, clipboard actions hit an in-memory clipboard.
struct Harness {
    state: State,
    tasks: SelectAll<BoxStream<'static, Action<Message>>>,
    clipboard: Option<String>,
    primary: Option<String>,
    events: Vec<Event>,
}

impl Harness {
    fn new(state: State) -> Self {
        Self {
            state,
            tasks: SelectAll::new(),
            clipboard: None,
            primary: None,
            events: Vec::new(),
        }
    }

    fn spawn(&mut self, task: Task<Message>) {
        if let Some(stream) = iced_runtime::task::into_stream(task) {
            self.tasks.push(stream);
        }
    }

    fn send(&mut self, message: Message) {
        let (task, event) = self.state.update(message);
        self.spawn(task);
        self.events.extend(event);
    }

    fn open(&mut self, target: Target) -> TabId {
        let (id, task) = self.state.open(target);
        self.spawn(task);
        id
    }

    fn canvas(&mut self, id: TabId, event: CanvasEvent) {
        self.send(Message::Canvas(id, event));
    }

    fn key(&mut self, id: TabId, key: Key, modifiers: Modifiers, text: Option<&str>) {
        self.canvas(
            id,
            CanvasEvent::Key {
                key,
                modifiers,
                text: text.map(str::to_owned),
            },
        );
    }

    fn type_text(&mut self, id: TabId, text: &str) {
        for c in text.chars() {
            let s = c.to_string();
            let key = if c == '\r' {
                Key::Named(Named::Enter)
            } else {
                Key::Character(s.as_str().into())
            };
            self.key(id, key, Modifiers::empty(), Some(&s));
        }
    }

    /// Waits for the next task action and applies it. Never returns when no
    /// task is left.
    async fn step(&mut self) {
        match self.tasks.next().await {
            Some(Action::Output(message)) => self.send(message),
            Some(Action::Clipboard(ClipboardAction::Write { target, contents })) => match target {
                iced::advanced::clipboard::Kind::Standard => self.clipboard = Some(contents),
                iced::advanced::clipboard::Kind::Primary => self.primary = Some(contents),
            },
            Some(Action::Clipboard(ClipboardAction::Read { channel, .. })) => {
                let _ = channel.send(self.clipboard.clone());
            }
            Some(_) => {}
            None => futures::future::pending::<()>().await,
        }
    }

    /// Processes task output until `done` holds.
    async fn until(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let run = async {
            while !done(self) {
                self.step().await;
            }
        };
        if tokio::time::timeout(LIMIT, run).await.is_err() {
            panic!("timed out waiting for {what}");
        }
    }

    fn screen(&self, id: TabId) -> String {
        self.state
            .emulator(id)
            .map(|emu| emu.screen_text().join("\n"))
            .unwrap_or_default()
    }

    async fn until_screen(&mut self, id: TabId, needle: &str) {
        self.until(needle, |h| h.screen(id).contains(needle)).await;
    }
}

async fn accept(
    sessions: &mut tokio::sync::mpsc::UnboundedReceiver<LoopbackSession>,
    h: &mut Harness,
) -> LoopbackSession {
    let wait = async {
        loop {
            tokio::select! {
                session = sessions.recv() => return session.expect("host open"),
                () = h.step() => {}
            }
        }
    };
    tokio::time::timeout(LIMIT, wait).await.expect("session opened in time")
}

fn remote_state() -> (State, tokio::sync::mpsc::UnboundedReceiver<LoopbackSession>) {
    let (host, sessions) = LoopbackHost::new();
    let dir = std::env::temp_dir();
    let state = State::new(metrics(), LocalShell::login_shell(dir)).with_remote(Arc::new(host));
    (state, sessions)
}

#[tokio::test]
async fn local_shell_tab_runs_typed_commands_resizes_and_exits() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    let mut shell = LocalShell::program("/bin/sh", Vec::<std::ffi::OsString>::new(), &cwd);
    shell.env.push(("PS1".into(), "$ ".into()));
    let mut h = Harness::new(State::new(metrics(), shell.clone()));
    let id = h.open(Target::Local(shell));
    // Typed before the shell is up: must not be lost.
    h.type_text(id, "pwd\r");
    h.until_screen(id, cwd.to_str().unwrap()).await;
    assert_eq!(h.state.status(id), Some(&Status::Running));

    // The pane is laid out at 50x10 cells.
    h.canvas(id, CanvasEvent::Resized(metrics().grid_for(Size::new(400.0, 160.0))));
    h.type_text(id, "stty size\r");
    h.until_screen(id, "10 50").await;

    h.type_text(id, "exit 7\r");
    h.until("exit event", |h| !h.events.is_empty()).await;
    assert_eq!(h.events, [Event::Exited { tab: id, exit_code: Some(7) }]);
    assert!(h.state.is_empty());
}

#[tokio::test]
async fn remote_tab_streams_both_ways_through_the_port() {
    let (state, mut sessions) = remote_state();
    let mut h = Harness::new(state);
    h.canvas(TabId(0), CanvasEvent::Resized(GridSize { cols: 100, rows: 30, cell_width: 8, cell_height: 16 }));
    let id = h.open(Target::Remote { host: "build-box".into() });
    let mut session = accept(&mut sessions, &mut h).await;
    assert_eq!(session.host, "build-box");
    assert_eq!((session.size.cols, session.size.rows), (100, 30));
    assert_eq!(h.state.title(id), Some("build-box"));

    assert!(session.send(&b"remote$ "[..]));
    h.until_screen(id, "remote$").await;

    h.type_text(id, "ls");
    h.key(id, Key::Character("c".into()), Modifiers::CTRL, None);
    let mut typed = Vec::new();
    while typed.len() < 3 {
        match next_input_with(&mut h, &mut session).await {
            RemoteInput::Data(bytes) => typed.extend_from_slice(&bytes),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(typed, b"ls\x03");

    h.canvas(id, CanvasEvent::Resized(GridSize::new(60, 20)));
    assert_eq!(
        next_input_with(&mut h, &mut session).await,
        RemoteInput::Resize(GridSize::new(60, 20))
    );

    // A cursor position query from the remote program gets its answer.
    assert!(session.send(&b"\r\nab\x1b[6n"[..]));
    let reply = next_input_with(&mut h, &mut session).await;
    assert_eq!(reply, RemoteInput::Data(Bytes::from_static(b"\x1b[2;3R")));

    session.exit(Some(0));
    h.until("exit", |h| !h.events.is_empty()).await;
    assert_eq!(h.events, [Event::Exited { tab: id, exit_code: Some(0) }]);
}

/// Waits for remote input while still running the pane's tasks.
async fn next_input_with(h: &mut Harness, session: &mut LoopbackSession) -> RemoteInput {
    let wait = async {
        loop {
            tokio::select! {
                input = session.recv() => return input.expect("pane connected"),
                () = h.step() => {}
            }
        }
    };
    tokio::time::timeout(LIMIT, wait).await.expect("remote input in time")
}

#[tokio::test]
async fn closing_a_sub_tab_hangs_up_only_that_terminal() {
    let (state, mut sessions) = remote_state();
    let mut h = Harness::new(state);
    let first = h.open(Target::Remote { host: "a".into() });
    let mut a = accept(&mut sessions, &mut h).await;
    let second = h.open(Target::Remote { host: "b".into() });
    let mut b = accept(&mut sessions, &mut h).await;
    h.until("both running", |h| {
        h.state.status(first) == Some(&Status::Running)
            && h.state.status(second) == Some(&Status::Running)
    })
    .await;
    assert_eq!(h.state.tabs().collect::<Vec<_>>(), [first, second]);
    assert_eq!(h.state.active(), Some(second));

    h.send(Message::Select(first));
    h.send(Message::Close(first));
    assert_eq!(h.events, [Event::Closed { tab: first }]);
    assert_eq!(h.state.tabs().collect::<Vec<_>>(), [second]);
    assert_eq!(h.state.active(), Some(second));
    assert_eq!(next_input_with(&mut h, &mut a).await, RemoteInput::Close);

    h.type_text(second, "x");
    assert_eq!(next_input_with(&mut h, &mut b).await, RemoteInput::Data(Bytes::from_static(b"x")));
}

#[tokio::test]
async fn copy_and_paste_go_through_the_clipboard() {
    let (state, mut sessions) = remote_state();
    let mut h = Harness::new(state);
    let id = h.open(Target::Remote { host: "h".into() });
    let mut session = accept(&mut sessions, &mut h).await;
    // The program turns on bracketed paste and prints a line.
    assert!(session.send(&b"\x1b[?2004hhello world"[..]));
    h.until_screen(id, "hello world").await;

    h.canvas(id, CanvasEvent::SelectStart { cell: ViewportCell { col: 0, row: 0 }, side: Side::Left, kind: SelectionType::Simple });
    h.canvas(id, CanvasEvent::SelectUpdate { cell: ViewportCell { col: 4, row: 0 }, side: Side::Right });
    h.canvas(id, CanvasEvent::SelectEnd);
    let chord = if cfg!(target_os = "macos") { Modifiers::LOGO } else { Modifiers::CTRL | Modifiers::SHIFT };
    h.key(id, Key::Character("C".into()), chord, None);
    h.until("copy", |h| h.clipboard.is_some()).await;
    assert_eq!(h.clipboard.as_deref(), Some("hello"));
    assert_eq!(h.primary.as_deref(), Some("hello"));

    h.clipboard = Some("make\ntest".into());
    h.key(id, Key::Character("V".into()), chord, None);
    let pasted = next_input_with(&mut h, &mut session).await;
    assert_eq!(pasted, RemoteInput::Data(Bytes::from_static(b"\x1b[200~make\ntest\x1b[201~")));
}

#[tokio::test]
async fn wheel_scrolls_history_and_typing_returns_to_the_prompt() {
    let (state, mut sessions) = remote_state();
    let mut h = Harness::new(state);
    h.canvas(TabId(0), CanvasEvent::Resized(GridSize::new(20, 5)));
    let id = h.open(Target::Remote { host: "h".into() });
    let mut session = accept(&mut sessions, &mut h).await;
    let mut out = String::new();
    for n in 0..30 {
        out.push_str(&format!("line {n}\r\n"));
    }
    out.push_str("end");
    assert!(session.send(out.into_bytes()));
    h.until_screen(id, "end").await;

    h.canvas(id, CanvasEvent::Scroll(10));
    assert!(h.screen(id).starts_with("line 16"), "{}", h.screen(id));
    h.type_text(id, "q");
    assert!(h.screen(id).ends_with("end"), "{}", h.screen(id));
    assert_eq!(next_input_with(&mut h, &mut session).await, RemoteInput::Data(Bytes::from_static(b"q")));

    // In a full screen program the wheel becomes arrow keys.
    assert!(session.send(&b"\x1b[?1049h"[..]));
    h.until("alt screen", |h| {
        h.state.emulator(id).is_some_and(|emu| emu.mode().contains(TermMode::ALT_SCREEN))
    })
    .await;
    h.canvas(id, CanvasEvent::Scroll(-2));
    assert_eq!(
        next_input_with(&mut h, &mut session).await,
        RemoteInput::Data(Bytes::from_static(b"\x1b[B\x1b[B"))
    );
}

#[tokio::test]
async fn unreachable_or_unconfigured_remote_marks_the_tab_failed() {
    let mut h = Harness::new(State::new(metrics(), LocalShell::login_shell(std::env::temp_dir())));
    let id = h.open(Target::Remote { host: "x".into() });
    h.until("failure", |h| !h.events.is_empty()).await;
    assert!(matches!(&h.events[0], Event::Failed { tab, .. } if *tab == id));
    assert!(matches!(h.state.status(id), Some(Status::Failed(_))));

    let (state, sessions) = remote_state();
    drop(sessions);
    let mut h = Harness::new(state);
    let id = h.open(Target::Remote { host: "down".into() });
    h.until("failure", |h| !h.events.is_empty()).await;
    assert!(matches!(&h.events[0], Event::Failed { tab, error } if *tab == id && error.contains("down")));
}

#[tokio::test]
async fn connection_lost_mid_session_keeps_the_screen_and_reports() {
    let (state, mut sessions) = remote_state();
    let mut h = Harness::new(state);
    let id = h.open(Target::Remote { host: "flaky".into() });
    let session = accept(&mut sessions, &mut h).await;
    assert!(session.send(&b"last words"[..]));
    h.until_screen(id, "last words").await;
    session.fail("broken pipe");
    h.until("failure", |h| !h.events.is_empty()).await;
    assert!(matches!(&h.events[0], Event::Failed { error, .. } if error.contains("broken pipe")));
    assert!(h.screen(id).contains("last words"));
}

#[test]
fn view_lists_sub_tabs_and_selects_on_click() {
    use iced_test::simulator;

    let (mut state, _sessions) = remote_state();
    let (first, _) = state.open(Target::Remote { host: "alpha".into() });
    let (second, _) = state.open(Target::Remote { host: "beta".into() });
    assert_eq!(state.active(), Some(second));
    let mut ui = simulator(state.view());
    ui.click("alpha").expect("tab title shown");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert!(
        messages.iter().any(|m| matches!(m, Message::Select(id) if *id == first)),
        "{messages:?}"
    );
}

#[test]
fn view_routes_typing_into_the_focused_terminal() {
    use iced_test::simulator;

    let (mut state, _sessions) = remote_state();
    let (id, _) = state.open(Target::Remote { host: "alpha".into() });
    let mut ui = simulator(state.view());
    ui.point_at(iced::Point::new(500.0, 400.0));
    ui.simulate(iced_test::simulator::click());
    ui.typewrite("hi");
    let keys: Vec<String> = ui
        .into_messages()
        .filter_map(|m| match m {
            Message::Canvas(tab, CanvasEvent::Key { text, .. }) if tab == id => text,
            _ => None,
        })
        .collect();
    assert_eq!(keys, ["h", "i"]);
}
