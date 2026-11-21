//! Login state and strategies shared by the service and sign-in screen.
use std::sync::Arc;
use tokio::sync::mpsc;
use crate::rpc::{LoginProvider, LoginUiRequest, OmpRpc, UiResponse};
pub use loa_contracts::harness::{LoginControl, LoginStrategy, LoginUpdate};

/// Opens a URL without blocking on the browser.
pub trait UrlOpener: Send + Sync {
    fn open_url(&self, url: &str) -> std::result::Result<(), String>;
}

/// Identifies one row: the harness that owns the login and its provider id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProviderKey {
    pub harness: String,
    pub provider: String,
}

impl ProviderKey {
    pub fn new(harness: impl Into<String>, provider: impl Into<String>) -> Self {
        Self { harness: harness.into(), provider: provider.into() }
    }
}

/// How a provider authenticates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    /// Subscription login (OAuth) run by the harness.
    Login { signed_in: bool, account: Option<String> },
    /// Provider authenticated by an API key the harness reads itself.
    ApiKey {
        /// Where the key was found, e.g. an environment variable name or a
        /// config key. `None` when no key is configured.
        configured_via: Option<String>,
        /// Tells the user where to put the key.
        instructions: String,
    },
    /// No sign in can be offered now: the harness is not installed, its
    /// status could not be read, or it offers no login for the provider.
    Unavailable { reason: String },
}

/// One provider row as the app reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderEntry {
    pub key: ProviderKey,
    /// Display name of the provider, e.g. "Anthropic".
    pub name: String,
    /// Display name of the owning harness, e.g. "omp" or "Claude Code". Rows
    /// are grouped under it.
    pub harness_name: String,
    pub auth: Auth,
}

/// What a row shows about its login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginState {
    Idle,
    /// Sign in requested, waiting for the harness to send the URL. Progress
    /// messages arriving before the URL are kept in `progress` and carried
    /// into [`LoginState::Browser`]; prompts arriving before the URL are
    /// ignored, since the user cannot have a code before visiting the URL.
    Starting { progress: Option<String> },
    /// Browser step: URL shown and copyable, waiting for the user to finish.
    Browser {
        url: String,
        /// The URL the Copy button writes: the launch URL when the harness
        /// sent one, the full URL otherwise.
        copy_url: String,
        instructions: Option<String>,
        /// The latest progress message.
        progress: Option<String>,
        prompt: Option<LoginPrompt>,
    },
    Succeeded { account: Option<String> },
    Failed { message: String },
    Cancelled,
}

impl LoginState {
    /// Whether a login is running for the row.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Starting { .. } | Self::Browser { .. })
    }
}

/// A pending prompt of a running login and what the user typed so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginPrompt {
    pub message: String,
    pub placeholder: Option<String>,
    pub input: String,
}


/// Runs omp's OAuth protocol and publishes the same updates as CLI logins.
pub struct OmpOAuthLogin {
    pub rpc: Arc<dyn OmpRpc>,
    pub opener: Arc<dyn UrlOpener>,
    pub provider: LoginProvider,
}

#[async_trait::async_trait]
impl LoginStrategy for OmpOAuthLogin {
    async fn run(self: Box<Self>, mut controls: mpsc::Receiver<LoginControl>, updates: mpsc::Sender<LoginUpdate>) {
        if !self.provider.available {
            let _ = updates.send(LoginUpdate::Failed(format!("omp offers no login for {} now", self.provider.name))).await;
            return;
        }
        let (ui_tx, mut ui_rx) = mpsc::unbounded_channel();
        let login = self.rpc.login(&self.provider.id, ui_tx);
        let mut login = std::pin::pin!(login);
        let mut pending: Option<String> = None;
        let mut controls_open = true;
        loop {
            tokio::select! {
                biased;
                Some(request) = ui_rx.recv() => {
                    if !handle_ui(request, &*self.opener, &updates, &mut pending).await { return; }
                }
                control = controls.recv(), if controls_open => match control {
                    Some(LoginControl::SubmitInput(value)) => {
                        if let Some(id) = pending.take()
                            && let Err(err) = self.rpc.respond_ui(UiResponse::Value { id, value }).await
                        {
                            let _ = updates.send(LoginUpdate::Failed(err.to_string())).await;
                            return;
                        }
                    }
                    Some(LoginControl::Cancel) => {
                        if let Some(id) = pending.take() {
                            let _ = self.rpc.respond_ui(UiResponse::Cancelled { id, cancelled: true }).await;
                        }
                        let _ = updates.send(LoginUpdate::Cancelled).await;
                        return;
                    }
                    None => controls_open = false,
                },
                result = &mut login => {
                    while let Ok(request) = ui_rx.try_recv() {
                        if !handle_ui(request, &*self.opener, &updates, &mut pending).await { return; }
                    }
                    let update = match result {
                        Ok(_) => LoginUpdate::Succeeded { account: None },
                        Err(err) => LoginUpdate::Failed(err.to_string()),
                    };
                    let _ = updates.send(update).await;
                    return;
                }
            }
        }
    }
}

async fn handle_ui(request: LoginUiRequest, opener: &dyn UrlOpener, updates: &mpsc::Sender<LoginUpdate>, pending: &mut Option<String>) -> bool {
    let update = match request {
        LoginUiRequest::OpenUrl { url, launch_url, instructions, .. } => {
            let error = opener.open_url(&url).err();
            if updates.send(LoginUpdate::Url { url, launch_url, instructions }).await.is_err() { return false; }
            error.map(|message| LoginUpdate::Progress(format!("Could not open browser: {message}")))
        }
        LoginUiRequest::Notify { message, .. } => Some(LoginUpdate::Progress(message)),
        LoginUiRequest::Input { id, title, placeholder, .. } => {
            *pending = Some(id);
            Some(LoginUpdate::Prompt { message: title, placeholder })
        }
        LoginUiRequest::Cancel { target_id, .. } => {
            if pending.as_ref() == Some(&target_id) { *pending = None; }
            None
        }
    };
    match update { Some(update) => updates.send(update).await.is_ok(), None => true }
}

#[cfg(test)]
mod oauth_tests {
    use super::*;
    use crate::testing::{FakeOmp, LoginScript};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Opener(Mutex<Vec<String>>);
    impl UrlOpener for Opener {
        fn open_url(&self, url: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(url.to_owned());
            Ok(())
        }
    }
    fn provider() -> LoginProvider {
        LoginProvider { id: "anthropic".into(), name: "Anthropic".into(), available: true, authenticated: false }
    }
    fn frames() -> Vec<LoginUiRequest> {
        vec![
            LoginUiRequest::OpenUrl { id: "1".into(), url: "https://auth.example".into(), launch_url: Some("http://localhost/login".into()), instructions: None },
            LoginUiRequest::Input { id: "2".into(), title: "Paste code".into(), placeholder: None, timeout: None },
        ]
    }
    #[tokio::test]
    async fn relays_oauth_input_and_success() {
        let mut fake = FakeOmp::default();
        fake.login_frames = frames();
        fake.script = LoginScript::AwaitInput("code".into());
        let rpc = Arc::new(fake);
        let opener = Arc::new(Opener::default());
        let (ctl_tx, ctl_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(16);
        let strategy = Box::new(OmpOAuthLogin { rpc: rpc.clone(), opener: opener.clone(), provider: provider() });
        let drive = async {
            assert_eq!(out_rx.recv().await, Some(LoginUpdate::Url { url: "https://auth.example".into(), launch_url: Some("http://localhost/login".into()), instructions: None }));
            assert!(matches!(out_rx.recv().await, Some(LoginUpdate::Prompt { .. })));
            ctl_tx.send(LoginControl::SubmitInput("code".into())).await.unwrap();
            assert_eq!(out_rx.recv().await, Some(LoginUpdate::Succeeded { account: None }));
        };
        tokio::join!(strategy.run(ctl_rx, out_tx), drive);
        assert_eq!(*opener.0.lock().unwrap(), ["https://auth.example"]);
        assert_eq!(*rpc.responses.lock().unwrap(), [UiResponse::Value { id: "2".into(), value: "code".into() }]);
    }
    #[tokio::test]
    async fn cancellation_withdraws_pending_input() {
        let mut fake = FakeOmp::default();
        fake.login_frames = frames();
        fake.script = LoginScript::Hang;
        let rpc = Arc::new(fake);
        let (ctl_tx, ctl_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(16);
        let strategy = Box::new(OmpOAuthLogin { rpc: rpc.clone(), opener: Arc::new(Opener::default()), provider: provider() });
        let drive = async {
            assert!(matches!(out_rx.recv().await, Some(LoginUpdate::Url { .. })));
            assert!(matches!(out_rx.recv().await, Some(LoginUpdate::Prompt { .. })));
            ctl_tx.send(LoginControl::Cancel).await.unwrap();
            assert_eq!(out_rx.recv().await, Some(LoginUpdate::Cancelled));
        };
        tokio::join!(strategy.run(ctl_rx, out_tx), drive);
        assert_eq!(*rpc.responses.lock().unwrap(), [UiResponse::Cancelled { id: "2".into(), cancelled: true }]);
    }
}


mod cli {
//! Driving a harness CLI login without a terminal.
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{self, UnboundedSender, unbounded_channel};
use crate::login::{LoginControl, LoginStrategy, LoginUpdate};
use crate::process::{ProcessReader, RunningProcess};

/// A started CLI login process.
pub struct CliLogin { pub process: Box<dyn RunningProcess> }

#[async_trait::async_trait]
impl LoginStrategy for CliLogin {
    async fn run(mut self: Box<Self>, mut controls: mpsc::Receiver<LoginControl>, updates: mpsc::Sender<LoginUpdate>) {
        let mut stdin = self.process.take_stdin();
        let (text_tx, mut text_rx) = unbounded_channel::<String>();
        for reader in [self.process.take_stdout(), self.process.take_stderr()].into_iter().flatten() {
            tokio::spawn(pump(reader, text_tx.clone()));
        }
        drop(text_tx);
        let mut lines = LineSplitter::default();
        let mut url_sent = false;
        let mut controls_open = true;
        let mut last = None;
        loop {
            tokio::select! {
                chunk = text_rx.recv() => match chunk {
                    Some(chunk) => for line in lines.push(&chunk) {
                        if !publish(&updates, line, &mut url_sent, &mut last).await { return; }
                    },
                    None => {
                        for line in lines.finish() {
                            if !publish(&updates, line, &mut url_sent, &mut last).await { return; }
                        }
                        break;
                    }
                },
                control = controls.recv(), if controls_open => match control {
                    Some(LoginControl::SubmitInput(text)) => {
                        if let Some(pipe) = stdin.as_mut() {
                            let line = format!("{}\n", text.trim_end_matches(['\r', '\n']));
                            if pipe.write_all(line.as_bytes()).await.is_err() || pipe.flush().await.is_err() { stdin = None; }
                        }
                    }
                    Some(LoginControl::Cancel) => {
                        let update = match self.process.kill().await {
                            Ok(()) => LoginUpdate::Cancelled,
                            Err(err) => LoginUpdate::Failed(err.to_string()),
                        };
                        let _ = updates.send(update).await;
                        return;
                    }
                    None => controls_open = false,
                }
            }
        }
        drop(stdin);
        let update = match self.process.wait().await {
            Ok(Some(0)) => LoginUpdate::Succeeded { account: None },
            Ok(Some(code)) => LoginUpdate::Failed(match last {
                Some(line) => format!("login exited with code {code}: {line}"),
                None => format!("login exited with code {code}"),
            }),
            Ok(None) => LoginUpdate::Failed("login was stopped".to_owned()),
            Err(err) => LoginUpdate::Failed(err.to_string()),
        };
        let _ = updates.send(update).await;
    }
}

async fn publish(updates: &mpsc::Sender<LoginUpdate>, line: Line, url_sent: &mut bool, last: &mut Option<String>) -> bool {
    let update = match line {
        Line::Text(text) => match first_url(&text) {
            Some(url) if !*url_sent => {
                *url_sent = true;
                LoginUpdate::Url { url: url.to_owned(), launch_url: None, instructions: None }
            }
            _ => { *last = Some(text.clone()); LoginUpdate::Progress(text) }
        },
        Line::Prompt(message) => LoginUpdate::Prompt { message, placeholder: None },
    };
    updates.send(update).await.is_ok()
}

async fn pump(mut reader: ProcessReader, out: UnboundedSender<String>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.send(String::from_utf8_lossy(&buf[..n]).into_owned()).is_err() {
                    break;
                }
            }
        }
    }
}

/// The first `http://` or `https://` URL in `text`.
fn first_url(text: &str) -> Option<&str> {
    let start = text.find("https://").or_else(|| text.find("http://"))?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']'))
        .unwrap_or(rest.len());
    Some(rest[..end].trim_end_matches(['.', ',', ';']))
}

#[derive(Debug, PartialEq, Eq)]
enum Line {
    Text(String),
    Prompt(String),
}

/// Splits output into lines, dropping terminal escape sequences, and reports
/// an unterminated last line that ends like a question as a prompt.
#[derive(Default)]
struct LineSplitter {
    partial: String,
}

impl LineSplitter {
    fn push(&mut self, chunk: &str) -> Vec<Line> {
        self.partial.push_str(chunk);
        let mut out = Vec::new();
        while let Some(end) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=end).collect();
            let line = clean(&line);
            if !line.is_empty() {
                out.push(Line::Text(line));
            }
        }
        let pending = clean(&self.partial);
        if pending.ends_with([':', '?', '>']) {
            self.partial.clear();
            out.push(Line::Prompt(pending));
        }
        out
    }

    fn finish(&mut self) -> Vec<Line> {
        let rest = clean(&std::mem::take(&mut self.partial));
        if rest.is_empty() { Vec::new() } else { vec![Line::Text(rest)] }
    }
}

/// Removes ANSI escape sequences and carriage returns, then trims.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() || c == '~' {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // Operating system command: ends at BEL or ESC \.
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.peek() == Some(&'\\')) {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            },
            '\r' => {}
            c if c.is_control() && c != '\t' && c != '\n' => {}
            c => out.push(c),
        }
    }
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::testutil::fake_program;
    use crate::process::{CommandSpec, ProcessRunner, TokioProcessRunner};

    #[test]
    fn splits_lines_strips_escapes_and_spots_prompts() {
        let mut lines = LineSplitter::default();
        assert_eq!(lines.push("\u{1b}[1mOpening\u{1b}[0m https://auth.example/x?y=1.\r\nPaste code"), vec![Line::Text("Opening https://auth.example/x?y=1.".into())]);
        assert_eq!(lines.push(" here: "), vec![Line::Prompt("Paste code here:".into())]);
        assert_eq!(first_url("Opening https://auth.example/x?y=1."), Some("https://auth.example/x?y=1"));
    }

    #[tokio::test]
    async fn relays_url_prompt_and_input() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_program(dir.path(), "claude", "echo 'Visit https://claude.example/oauth?code=abc to sign in'\nprintf 'Paste code here: '\nread code\necho \"Logged in with $code\"\nexit 0");
        let process = TokioProcessRunner.spawn(&CommandSpec::new(&program, ["auth", "login"])).await.unwrap();
        let (control_tx, control_rx) = mpsc::channel(16);
        let (update_tx, mut update_rx) = mpsc::channel(16);
        let run = tokio::spawn(Box::new(CliLogin { process }).run(control_rx, update_tx));
        assert_eq!(update_rx.recv().await, Some(LoginUpdate::Url { url: "https://claude.example/oauth?code=abc".into(), launch_url: None, instructions: None }));
        assert_eq!(update_rx.recv().await, Some(LoginUpdate::Prompt { message: "Paste code here:".into(), placeholder: None }));
        control_tx.send(LoginControl::SubmitInput("XYZ".into())).await.unwrap();
        assert_eq!(update_rx.recv().await, Some(LoginUpdate::Progress("Logged in with XYZ".into())));
        assert_eq!(update_rx.recv().await, Some(LoginUpdate::Succeeded { account: None }));
        run.await.unwrap();
    }

    #[tokio::test]
    async fn cancel_kills_the_login_command() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_program(dir.path(), "codex", "printf 'Code: '\nread code\nexit 0");
        let process = TokioProcessRunner.spawn(&CommandSpec::new(&program, ["login"])).await.unwrap();
        let (control_tx, control_rx) = mpsc::channel(16);
        let (update_tx, mut update_rx) = mpsc::channel(16);
        let run = tokio::spawn(Box::new(CliLogin { process }).run(control_rx, update_tx));
        assert!(matches!(update_rx.recv().await, Some(LoginUpdate::Prompt { .. })));
        control_tx.send(LoginControl::Cancel).await.unwrap();
        assert_eq!(update_rx.recv().await, Some(LoginUpdate::Cancelled));
        run.await.unwrap();
    }
}


}
pub use cli::CliLogin;
