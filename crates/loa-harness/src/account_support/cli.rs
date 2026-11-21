//! Driving a harness CLI login without a terminal.
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{self, UnboundedSender, unbounded_channel};
use loa_contracts::harness::{LoginControl, LoginStrategy, LoginUpdate};
use std::sync::Arc;
use super::process::{CommandSpec, ProcessReader, ProcessRunner, RunningProcess};

/// Starts a CLI login when its strategy is run.
pub struct CliLoginCommand {
    pub runner: Arc<dyn ProcessRunner>,
    pub command: CommandSpec,
}

#[async_trait::async_trait]
impl LoginStrategy for CliLoginCommand {
    async fn run(self: Box<Self>, controls: mpsc::Receiver<LoginControl>, updates: mpsc::Sender<LoginUpdate>) {
        match self.runner.spawn(&self.command).await {
            Ok(process) => Box::new(CliLogin { process }).run(controls, updates).await,
            Err(err) => { let _ = updates.send(LoginUpdate::Failed(err.to_string())).await; }
        }
    }
}

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
    use crate::account_support::process::testutil::fake_program;
    use crate::account_support::process::{CommandSpec, ProcessRunner, TokioProcessRunner};

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


