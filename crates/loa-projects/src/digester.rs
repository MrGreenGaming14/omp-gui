//! Builds a digest of a session with a cheap model.
//!
//! The digester renders a bounded transcript of the session (recent turns in
//! full, older ones abbreviated, tool results truncated), asks the
//! [`Summarizer`] for one strict JSON object with the five digest parts,
//! validates it and maps the turn labels it cites back to node ids.
//!
//! The model call is a port. [`OmpSummarizer`] runs `omp -p` one shot on the
//! `smol` role with no session, no tools and memory off. The other omp route,
//! an RPC `btw` side question (`{"type":"btw","question":…}`, answer in the
//! final `btw_record` frame), runs over the session's own context and model,
//! and would be implemented by the omp RPC adapter behind the same trait.

use crate::digest::{DigestContent, FileNote};
use crate::text::{clip, clip_marked};
use crate::{Error, Result};
use async_trait::async_trait;
use loa_core::{
    CallStatus, DecisionStatus, Dom, ElementKind as K, MessageKind, Node, NodeId, Role, TodoState, attr,
};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// What a summarizer answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryAnswer {
    pub text: String,
    /// Model or role that answered, recorded as `<digest model>`.
    pub model: Option<String>,
}

/// One model call: prompt in, text out. No tools, no session history.
#[async_trait]
pub trait Summarizer: Send + Sync {
    /// `model` overrides the summarizer's own model or role for this call,
    /// e.g. a project's digest model.
    async fn summarize(&self, prompt: &str, model: Option<&str>) -> Result<SummaryAnswer>;
}

/// Size bounds of the digest prompt, in bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptLimits {
    /// Total transcript size. Older turns are dropped past it.
    pub max_chars: usize,
    /// The newest turns rendered in full.
    pub full_turns: usize,
    /// One message in a full turn.
    pub message_chars: usize,
    /// A tool input or result in a full turn.
    pub tool_chars: usize,
    /// A message in an abbreviated turn.
    pub brief_chars: usize,
}

impl Default for PromptLimits {
    fn default() -> Self {
        Self { max_chars: 24_000, full_turns: 4, message_chars: 2_000, tool_chars: 400, brief_chars: 200 }
    }
}

/// A rendered digest prompt and the turn labels it uses.
#[derive(Clone, Debug)]
pub struct DigestPrompt {
    pub text: String,
    /// `T<n>` label of every turn in the session, in document order.
    labels: Vec<(String, NodeId)>,
    /// DOM sequence the prompt was rendered from.
    pub source_seq: u64,
}

/// A digest ready to write with [`crate::write_drafts`].
#[derive(Clone, Debug, PartialEq)]
pub struct BuiltDigest {
    pub content: DigestContent,
    pub source_seq: u64,
    pub model: Option<String>,
}

/// Turns session DOMs into digests through a [`Summarizer`].
#[derive(Clone)]
pub struct Digester {
    summarizer: Arc<dyn Summarizer>,
    limits: PromptLimits,
}

impl Digester {
    pub fn new(summarizer: Arc<dyn Summarizer>, limits: PromptLimits) -> Self {
        Self { summarizer, limits }
    }

    /// Digests `dom` with one summarizer call. `model` overrides the
    /// summarizer's model (a project's own digest model).
    pub async fn digest(&self, dom: &Dom, model: Option<&str>) -> Result<BuiltDigest> {
        let prompt = build_prompt(dom, &self.limits)?;
        let answer = self.summarizer.summarize(&prompt.text, model).await?;
        let content = parse_answer(&answer.text, &prompt)?;
        Ok(BuiltDigest { content, source_seq: prompt.source_seq, model: answer.model })
    }
}

const INSTRUCTIONS: &str = r#"You write the digest of one coding session, so other sessions in the same project know what it did.
Answer with exactly one JSON object and nothing else, in this shape:
{"summary": "two or three sentences on what the session did",
 "decisions": ["each decision that was made, one short line"],
 "files": [{"path": "a file the session read or changed", "note": "one line on what happened to it"}],
 "open_items": ["unfinished todos and unanswered questions"],
 "pointers": ["labels like T3 of the turns the digest is based on"]}
Every key is required; use [] when there is nothing. Pointers must be labels that appear below.
"#;

/// Renders the digest prompt for `dom`. [`Error::NothingToDigest`] when the
/// session has no turns.
pub fn build_prompt(dom: &Dom, limits: &PromptLimits) -> Result<DigestPrompt> {
    let root = dom.root().ok_or(Error::NothingToDigest)?;
    let turns: Vec<&Node> = dom.children_of_kind(root.id, K::Turn).collect();
    if turns.is_empty() {
        return Err(Error::NothingToDigest);
    }
    let labels: Vec<(String, NodeId)> =
        turns.iter().enumerate().map(|(i, t)| (format!("T{}", i + 1), t.id)).collect();

    let mut text = String::with_capacity(limits.max_chars + INSTRUCTIONS.len() + 1024);
    text.push_str(INSTRUCTIONS);
    text.push_str("\n# Session\n");
    if let Some(title) = root.str_attr(attr::TITLE) {
        let _ = writeln!(text, "Title: {}", clip(title, limits.brief_chars));
    }
    render_open_items(dom, root, limits, &mut text);
    if let Some(compaction) = dom.children_of_kind(root.id, K::Compaction).last() {
        let _ = writeln!(text, "\n## Earlier summary\n{}", clip_marked(&compaction.text, limits.message_chars));
    }

    // Newest first until the budget runs out, then emitted oldest first.
    let mut sections: Vec<String> = Vec::new();
    let mut used = 0;
    for (age, (turn, (label, _))) in turns.iter().zip(&labels).rev().enumerate() {
        let full = age < limits.full_turns;
        let section = render_turn(dom, turn, label, full, limits);
        let room = limits.max_chars.saturating_sub(used);
        if section.len() > room {
            if sections.is_empty() {
                sections.push(clip_marked(&section, room).into_owned());
            }
            break;
        }
        used += section.len();
        sections.push(section);
    }
    text.push_str("\n## Turns\n");
    let omitted = turns.len() - sections.len();
    if omitted > 0 {
        let _ = writeln!(text, "[{omitted} earlier turns omitted]");
    }
    for section in sections.iter().rev() {
        text.push_str(section);
    }
    Ok(DigestPrompt { text, labels, source_seq: dom.seq() })
}

fn render_open_items(dom: &Dom, root: &Node, limits: &PromptLimits, out: &mut String) {
    let todos: Vec<&Node> = dom
        .descendants(root.id)
        .filter(|n| n.kind == K::Todo)
        .filter(|n| {
            n.str_attr(attr::STATE)
                .and_then(TodoState::parse)
                .is_none_or(|s| matches!(s, TodoState::Pending | TodoState::InProgress))
        })
        .collect();
    let waiting: Vec<&Node> = dom
        .descendants(root.id)
        .filter(|n| n.kind == K::Decision)
        .filter(|n| n.str_attr(attr::STATUS).and_then(DecisionStatus::parse) == Some(DecisionStatus::Waiting))
        .collect();
    if todos.is_empty() && waiting.is_empty() {
        return;
    }
    out.push_str("\n## Open\n");
    for todo in todos {
        let _ = writeln!(out, "- todo: {}", clip(&todo.text, limits.brief_chars));
    }
    for decision in waiting {
        let what = decision.str_attr(attr::TITLE).unwrap_or(&decision.text);
        let _ = writeln!(out, "- waiting for an answer: {}", clip(what, limits.brief_chars));
    }
}

fn render_turn(dom: &Dom, turn: &Node, label: &str, full: bool, limits: &PromptLimits) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\n### {label}{}", if full { "" } else { " (abbreviated)" });
    let message_chars = if full { limits.message_chars } else { limits.brief_chars };
    let mut tools: Vec<String> = Vec::new();
    for node in dom.children(turn.id) {
        match node.kind {
            K::Message => {
                if node.str_attr(attr::KIND).and_then(MessageKind::parse) == Some(MessageKind::Thinking) {
                    continue;
                }
                let who = match node.str_attr(attr::ROLE).and_then(Role::parse) {
                    Some(Role::User) => "User",
                    Some(Role::Assistant) => "Assistant",
                    _ => "System",
                };
                let _ = writeln!(out, "{who}: {}", clip_marked(node.text.trim(), message_chars));
            }
            K::Call if full => render_call(dom, node, limits, &mut out),
            K::Call => {
                let tool = node.str_attr(attr::TOOL).unwrap_or("tool");
                tools.push(match node.str_attr(attr::TITLE) {
                    Some(title) => format!("{tool} ({})", clip(title, 80)),
                    None => tool.to_owned(),
                });
            }
            K::Decision => render_decision(node, message_chars, &mut out),
            _ => {}
        }
    }
    if !tools.is_empty() {
        let _ = writeln!(out, "Tools: {}", tools.join(", "));
    }
    out
}

fn render_call(dom: &Dom, call: &Node, limits: &PromptLimits, out: &mut String) {
    let tool = call.str_attr(attr::TOOL).unwrap_or("tool");
    let status = call.str_attr(attr::STATUS).and_then(CallStatus::parse);
    let _ = write!(out, "Tool {tool}");
    if let Some(title) = call.str_attr(attr::TITLE) {
        let _ = write!(out, " ({})", clip(title, 120));
    }
    if status == Some(CallStatus::Error) {
        out.push_str(" [error]");
    }
    out.push('\n');
    for child in dom.children(call.id) {
        match child.kind {
            K::Input => {
                let _ = writeln!(out, "  input: {}", clip_marked(child.text.trim(), limits.tool_chars));
            }
            K::Result => {
                let _ = writeln!(out, "  result: {}", clip_marked(child.text.trim(), limits.tool_chars));
            }
            K::Decision => render_decision(child, limits.tool_chars, out),
            _ => {}
        }
    }
}

fn render_decision(decision: &Node, max: usize, out: &mut String) {
    let kind = decision.str_attr(attr::KIND).unwrap_or("decision");
    let answer: Vec<&str> = decision.attr(attr::ANSWER).map(|a| a.str_items().collect()).unwrap_or_default();
    let what = decision.str_attr(attr::TITLE).unwrap_or(&decision.text);
    let _ = write!(out, "Decision ({kind}): {}", clip(what.trim(), max));
    if answer.is_empty() {
        out.push_str(" -> unanswered\n");
    } else {
        let _ = writeln!(out, " -> {}", answer.join(", "));
    }
}

#[derive(serde::Deserialize)]
struct Answer {
    summary: String,
    decisions: Vec<String>,
    files: Vec<FileNote>,
    open_items: Vec<String>,
    pointers: Vec<String>,
}

/// Parses and validates a summarizer answer for `prompt`. Text around the
/// JSON object (a code fence, a preamble) is ignored.
pub fn parse_answer(text: &str, prompt: &DigestPrompt) -> Result<DigestContent> {
    let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) else {
        return Err(Error::InvalidAnswer("no JSON object in the answer".into()));
    };
    if end < start {
        return Err(Error::InvalidAnswer("no JSON object in the answer".into()));
    }
    let answer: Answer =
        serde_json::from_str(&text[start..=end]).map_err(|e| Error::InvalidAnswer(e.to_string()))?;
    let summary = answer.summary.trim();
    if summary.is_empty() {
        return Err(Error::InvalidAnswer("empty summary".into()));
    }
    let lines = |items: Vec<String>| -> Vec<String> {
        items.into_iter().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect()
    };
    let mut files = Vec::with_capacity(answer.files.len());
    for file in answer.files {
        let path = file.path.trim();
        if path.is_empty() {
            return Err(Error::InvalidAnswer("a file entry has an empty path".into()));
        }
        files.push(FileNote { path: path.to_owned(), note: file.note.trim().to_owned() });
    }
    let mut pointers = Vec::with_capacity(answer.pointers.len());
    for label in &answer.pointers {
        let label = label.trim();
        let id = prompt
            .labels
            .iter()
            .find(|(l, _)| l.eq_ignore_ascii_case(label))
            .map(|(_, id)| *id)
            .ok_or_else(|| Error::InvalidAnswer(format!("pointer `{label}` names no turn")))?;
        if !pointers.contains(&id) {
            pointers.push(id);
        }
    }
    Ok(DigestContent {
        summary: summary.to_owned(),
        decisions: lines(answer.decisions),
        files,
        open_items: lines(answer.open_items),
        pointers,
    })
}

/// [`Summarizer`] that runs `omp -p` once per digest.
///
/// The prompt goes in on stdin and the answer is the printed text. The run
/// uses the configured model role (`@smol` unless set per project), saves no
/// session, loads no tools, context files, rules, skills or extensions, and
/// turns omp's memory off through a `--config` overlay so digests never
/// reach omp's own history or memory.
#[derive(Clone, Debug)]
pub struct OmpSummarizer {
    executable: PathBuf,
    cwd: PathBuf,
    model: String,
    timeout: Duration,
}

/// `--config` overlay that switches omp's memory off for the run. `"off"` is
/// quoted: a YAML 1.1 parser reads a bare `off` as the boolean `false`.
const MEMORY_OFF: &str = "memory:\n  backend: \"off\"\n";

impl OmpSummarizer {
    /// `executable` is the omp binary, `cwd` the directory omp starts in.
    pub fn new(executable: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            cwd: cwd.into(),
            model: "@smol".to_owned(),
            timeout: Duration::from_secs(180),
        }
    }

    /// Model or role passed to `--model`, e.g. `@smol` or a model id for a
    /// project that sets its own digest model.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The command line after the executable.
    fn args(&self, model: &str, overlay: &std::path::Path) -> Vec<std::ffi::OsString> {
        let mut args: Vec<std::ffi::OsString> = [
            "-p",
            "--mode",
            "text",
            "--no-session",
            "--no-tools",
            "--no-title",
            "--no-context-files",
            "--no-rules",
            "--no-skills",
            "--no-extensions",
            "--no-lsp",
            "--model",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        args.push(model.into());
        args.push("--config".into());
        args.push(overlay.into());
        args.push("--cwd".into());
        args.push(self.cwd.clone().into());
        args
    }
}

#[async_trait]
impl Summarizer for OmpSummarizer {
    async fn summarize(&self, prompt: &str, model: Option<&str>) -> Result<SummaryAnswer> {
        let model = model.map(str::trim).filter(|m| !m.is_empty()).unwrap_or(&self.model);
        let mut overlay = tempfile::Builder::new().prefix("loa-digest-").suffix(".yml").tempfile()?;
        std::io::Write::write_all(&mut overlay, MEMORY_OFF.as_bytes())?;
        let mut child = tokio::process::Command::new(&self.executable)
            .args(self.args(model, overlay.path()))
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Summarizer(format!("can't run {}: {e}", self.executable.display())))?;
        let mut stdin = child.stdin.take().ok_or_else(|| Error::Summarizer("no stdin pipe".into()))?;
        let feed = async move {
            stdin.write_all(prompt.as_bytes()).await?;
            stdin.shutdown().await
        };
        let run = async { tokio::join!(feed, child.wait_with_output()) };
        let (fed, output) = tokio::time::timeout(self.timeout, run)
            .await
            .map_err(|_| Error::Summarizer(format!("omp gave no answer within {:?}", self.timeout)))?;
        let output = output?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Summarizer(format!("omp exited with {}: {}", output.status, clip(stderr.trim(), 2000))));
        }
        fed.map_err(|e| Error::Summarizer(format!("writing the prompt to omp: {e}")))?;
        let text = String::from_utf8(output.stdout).map_err(|_| Error::Summarizer("omp printed invalid UTF-8".into()))?;
        Ok(SummaryAnswer { text, model: Some(model.trim_start_matches('@').to_owned()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::testing::Builder;
    use loa_core::{Draft, Value};

    fn session() -> (Builder, Vec<NodeId>) {
        let b = Builder::new("s1", "p", 1);
        let mut turns = Vec::new();
        for i in 0..8 {
            let turn = b.turn(&format!("prompt number {i} {}", "x".repeat(600)), &format!("reply {i}"), i);
            b.call(turn, "read", &format!("{{\"path\":\"src/f{i}.rs\"}}"), &"R".repeat(5000));
            turns.push(turn);
        }
        (b, turns)
    }

    #[test]
    fn prompt_keeps_recent_turns_whole_and_shortens_old_ones() {
        let (b, _) = session();
        let limits = PromptLimits::default();
        let prompt = build_prompt(&b.dom(), &limits).unwrap();
        // Recent turns show tool inputs, abbreviated ones only list tools.
        assert!(prompt.text.contains("input: {\"path\":\"src/f7.rs\"}"));
        assert!(!prompt.text.contains("src/f0.rs"));
        assert!(prompt.text.contains("### T1 (abbreviated)"));
        assert!(prompt.text.contains("Tools: read"));
        // Tool results are truncated, not dumped.
        assert!(!prompt.text.contains(&"R".repeat(limits.tool_chars + 1)));
        assert!(prompt.text.contains("more bytes]"));
        // Old prompts are cut to the brief size, recent ones are whole.
        let t1 = &prompt.text[prompt.text.find("### T1 ").unwrap()..prompt.text.find("### T2 ").unwrap()];
        assert!(!t1.contains(&"x".repeat(limits.brief_chars + 1)));
        assert!(prompt.text.contains(&"x".repeat(600)));
    }

    #[test]
    fn prompt_drops_oldest_turns_past_the_budget() {
        let (b, _) = session();
        let limits = PromptLimits { max_chars: 3_000, ..PromptLimits::default() };
        let prompt = build_prompt(&b.dom(), &limits).unwrap();
        assert!(prompt.text.len() < 3_000 + INSTRUCTIONS.len() + 512);
        assert!(prompt.text.contains("earlier turns omitted]"));
        assert!(prompt.text.contains("### T8"));
        assert!(!prompt.text.contains("### T1"));
    }

    #[test]
    fn prompt_lists_open_todos_and_waiting_questions() {
        let b = Builder::new("s1", "p", 1);
        let turn = b.turn("hi", "hello", 2);
        b.commit(Draft::insert(b.root, K::Todo, [(attr::STATE, Value::from("pending"))], "wire the theme switch"));
        b.commit(Draft::insert(b.root, K::Todo, [(attr::STATE, Value::from("done"))], "finished thing"));
        b.commit(Draft::insert(
            turn,
            K::Decision,
            [(attr::KIND, Value::from("question")), (attr::STATUS, "waiting".into())],
            "Which port?",
        ));
        let text = build_prompt(&b.dom(), &PromptLimits::default()).unwrap().text;
        assert!(text.contains("todo: wire the theme switch"));
        assert!(!text.contains("finished thing"));
        assert!(text.contains("waiting for an answer: Which port?"));
    }

    #[test]
    fn empty_session_has_nothing_to_digest() {
        let b = Builder::new("s1", "p", 1);
        assert!(matches!(build_prompt(&b.dom(), &PromptLimits::default()), Err(Error::NothingToDigest)));
    }

    #[test]
    fn answer_in_a_fence_parses_and_maps_pointers_to_turns() {
        let (b, turns) = session();
        let prompt = build_prompt(&b.dom(), &PromptLimits::default()).unwrap();
        let answer = "Here it is:\n```json\n{\"summary\":\" Read files. \",\"decisions\":[\"use iced\",\"\"],\
            \"files\":[{\"path\":\"src/f1.rs\",\"note\":\"read\"}],\"open_items\":[],\"pointers\":[\"T2\",\"t8\",\"T2\"]}\n```";
        let content = parse_answer(answer, &prompt).unwrap();
        assert_eq!(content.summary, "Read files.");
        assert_eq!(content.decisions, vec!["use iced"]);
        assert_eq!(content.files, vec![FileNote { path: "src/f1.rs".into(), note: "read".into() }]);
        assert_eq!(content.pointers, vec![turns[1], turns[7]]);
    }

    #[test]
    fn answer_with_missing_parts_or_unknown_turns_is_rejected() {
        let (b, _) = session();
        let prompt = build_prompt(&b.dom(), &PromptLimits::default()).unwrap();
        let missing = r#"{"summary":"x","decisions":[],"files":[],"pointers":[]}"#;
        assert!(matches!(parse_answer(missing, &prompt), Err(Error::InvalidAnswer(_))));
        let unknown = r#"{"summary":"x","decisions":[],"files":[],"open_items":[],"pointers":["T99"]}"#;
        assert!(matches!(parse_answer(unknown, &prompt), Err(Error::InvalidAnswer(_))));
        let empty = r#"{"summary":" ","decisions":[],"files":[],"open_items":[],"pointers":[]}"#;
        assert!(matches!(parse_answer(empty, &prompt), Err(Error::InvalidAnswer(_))));
        assert!(matches!(parse_answer("I can't do that", &prompt), Err(Error::InvalidAnswer(_))));
    }

    /// A fake `omp` that records its arguments, stdin and the overlay, then
    /// prints a fixed answer.
    fn fake_omp(dir: &std::path::Path, body: &str) -> PathBuf {
        let path = dir.join("omp");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{d}/args\"\ncat > \"{d}/stdin\"\n\
             while [ $# -gt 0 ]; do if [ \"$1\" = --config ]; then cp \"$2\" \"{d}/overlay\"; fi; shift; done\n{body}\n",
            d = dir.display()
        );
        loa_exec::write_executable(&path, script).unwrap();
        path
    }

    #[tokio::test]
    async fn omp_summarizer_runs_one_shot_smol_without_session_or_memory() {
        let dir = tempfile::tempdir().unwrap();
        let answer = r#"{"summary":"Did things.","decisions":[],"files":[],"open_items":[],"pointers":["T1"]}"#;
        let exe = fake_omp(dir.path(), &format!("printf '%s' '{answer}'"));
        let summarizer = Arc::new(OmpSummarizer::new(&exe, dir.path()));
        let b = Builder::new("s1", "p", 1);
        let turn = b.turn("build the thing", "done", 2);
        let digester = Digester::new(summarizer, PromptLimits::default());
        let built = digester.digest(&b.dom(), None).await.unwrap();
        assert_eq!(built.content.summary, "Did things.");
        assert_eq!(built.content.pointers, vec![turn]);
        assert_eq!(built.model.as_deref(), Some("smol"));
        assert_eq!(built.source_seq, b.dom().seq());

        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        for flag in ["-p", "--no-session", "--no-tools", "--no-title"] {
            assert!(args.contains(&flag), "missing {flag} in {args:?}");
        }
        let model_at = args.iter().position(|a| *a == "--model").unwrap();
        assert_eq!(args[model_at + 1], "@smol");
        assert_eq!(std::fs::read_to_string(dir.path().join("overlay")).unwrap(), MEMORY_OFF);
        let stdin = std::fs::read_to_string(dir.path().join("stdin")).unwrap();
        assert!(stdin.contains("User: build the thing"));

        // A project's digest model replaces the role for that call.
        let built = digester.digest(&b.dom(), Some("anthropic/claude-haiku-5-5")).await.unwrap();
        assert_eq!(built.model.as_deref(), Some("anthropic/claude-haiku-5-5"));
        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        assert!(args.lines().any(|a| a == "anthropic/claude-haiku-5-5"), "{args}");
    }

    #[tokio::test]
    async fn omp_failure_is_reported_with_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let exe = fake_omp(dir.path(), "echo 'no smol model configured' >&2; exit 2");
        let err = OmpSummarizer::new(&exe, dir.path()).summarize("hi", None).await.unwrap_err();
        assert!(matches!(&err, Error::Summarizer(m) if m.contains("no smol model configured")), "{err}");
    }

    #[tokio::test]
    async fn missing_omp_is_a_summarizer_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = OmpSummarizer::new(dir.path().join("absent"), dir.path()).summarize("hi", None).await.unwrap_err();
        assert!(matches!(err, Error::Summarizer(_)));
    }
}
