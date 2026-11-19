//! Drives a harness's own login command (`claude auth login`, `codex login`)
//! without a terminal: its output is read line by line, the first web URL it
//! prints becomes the sign in URL, a line waiting for input becomes a prompt,
//! and what the user types is written to its stdin.
//!
//! The CLI keeps the credential; nothing it prints is stored, and the output is
//! only passed on for display while the login runs.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::Result;
use crate::login::LoginControl;
use crate::process::{ProcessReader, RunningProcess};

/// What the login command showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliLoginUpdate {
    /// The first web URL the command printed: the page to sign in on.
    Url(String),
    /// A line of output, for progress display.
    Output(String),
    /// The command waits for the user to type something (for example a code
    /// shown in the browser). The text is the command's own prompt.
    Prompt(String),
}

/// How a login command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliLoginOutcome {
    /// The command exited with this code (`None` when a signal ended it).
    Exited(Option<i32>),
    /// The user cancelled; the command was killed.
    Cancelled,
}

/// Runs a started login command to its end, publishing what it shows on
/// `updates` and relaying input and cancellation from `controls`.
pub async fn run_cli_login(
    mut process: Box<dyn RunningProcess>,
    mut controls: UnboundedReceiver<LoginControl>,
    updates: UnboundedSender<CliLoginUpdate>,
) -> Result<CliLoginOutcome> {
    let mut stdin = process.take_stdin();
    let (text_tx, mut text_rx) = unbounded_channel::<String>();
    for reader in [process.take_stdout(), process.take_stderr()].into_iter().flatten() {
        tokio::spawn(pump(reader, text_tx.clone()));
    }
    drop(text_tx);

    let mut lines = LineSplitter::default();
    let mut url_sent = false;
    let mut controls_open = true;
    loop {
        tokio::select! {
            chunk = text_rx.recv() => match chunk {
                Some(chunk) => {
                    for update in lines.push(&chunk) {
                        publish(&updates, update, &mut url_sent);
                    }
                }
                None => {
                    for update in lines.finish() {
                        publish(&updates, update, &mut url_sent);
                    }
                    break;
                }
            },
            control = controls.recv(), if controls_open => match control {
                Some(LoginControl::SubmitInput(text)) => {
                    if let Some(pipe) = stdin.as_mut() {
                        let line = format!("{}\n", text.trim_end_matches(['\r', '\n']));
                        if pipe.write_all(line.as_bytes()).await.is_err() || pipe.flush().await.is_err() {
                            stdin = None;
                        }
                    }
                }
                Some(LoginControl::Cancel) => {
                    process.kill().await?;
                    return Ok(CliLoginOutcome::Cancelled);
                }
                None => controls_open = false,
            },
        }
    }
    drop(stdin);
    Ok(CliLoginOutcome::Exited(process.wait().await?))
}

fn publish(updates: &UnboundedSender<CliLoginUpdate>, update: Line, url_sent: &mut bool) {
    let update = match update {
        Line::Text(text) => match first_url(&text) {
            Some(url) if !*url_sent => {
                *url_sent = true;
                CliLoginUpdate::Url(url.to_owned())
            }
            _ => CliLoginUpdate::Output(text),
        },
        Line::Prompt(text) => CliLoginUpdate::Prompt(text),
    };
    let _ = updates.send(update);
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
        assert_eq!(
            lines.push("\u{1b}[1mOpening\u{1b}[0m https://auth.example/x?y=1.\r\nPaste code"),
            vec![Line::Text("Opening https://auth.example/x?y=1.".into())]
        );
        assert_eq!(lines.push(" here: "), vec![Line::Prompt("Paste code here:".into())]);
        assert_eq!(first_url("Opening https://auth.example/x?y=1."), Some("https://auth.example/x?y=1"));
        assert!(lines.finish().is_empty());
    }

    #[tokio::test]
    async fn relays_url_prompt_and_input_to_the_login_command() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_program(
            dir.path(),
            "claude",
            "echo 'Visit https://claude.example/oauth?code=abc to sign in'\nprintf 'Paste code here: '\nread code\necho \"Logged in with $code\"\nexit 0",
        );
        let process = TokioProcessRunner.spawn(&CommandSpec::new(&program, ["auth", "login"])).await.unwrap();
        let (control_tx, control_rx) = unbounded_channel();
        let (update_tx, mut update_rx) = unbounded_channel();
        let run = tokio::spawn(run_cli_login(process, control_rx, update_tx));
        assert_eq!(update_rx.recv().await, Some(CliLoginUpdate::Url("https://claude.example/oauth?code=abc".into())));
        assert_eq!(update_rx.recv().await, Some(CliLoginUpdate::Prompt("Paste code here:".into())));
        control_tx.send(LoginControl::SubmitInput("XYZ".into())).unwrap();
        assert_eq!(update_rx.recv().await, Some(CliLoginUpdate::Output("Logged in with XYZ".into())));
        assert_eq!(run.await.unwrap().unwrap(), CliLoginOutcome::Exited(Some(0)));
    }

    #[tokio::test]
    async fn cancel_kills_the_login_command() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_program(dir.path(), "codex", "printf 'Code: '\nread code\nexit 0");
        let process = TokioProcessRunner.spawn(&CommandSpec::new(&program, ["login"])).await.unwrap();
        let (control_tx, control_rx) = unbounded_channel();
        let (update_tx, mut update_rx) = unbounded_channel();
        let run = tokio::spawn(run_cli_login(process, control_rx, update_tx));
        assert_eq!(update_rx.recv().await, Some(CliLoginUpdate::Prompt("Code:".into())));
        control_tx.send(LoginControl::Cancel).unwrap();
        assert_eq!(run.await.unwrap().unwrap(), CliLoginOutcome::Cancelled);
    }
}
