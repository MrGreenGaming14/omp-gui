//! Digest prompts and answers: a bounded transcript of the session (recent turns whole,
//! older ones abbreviated, tool output clipped) goes to a [`Summarizer`]; its strict JSON
//! answer is validated and its turn labels mapped back to node ids.

use crate::digest::{DigestContent, FileNote};
use crate::{Error, Result};
use loa_core::{CallStatus, DecisionStatus, Dom, ElementKind as K, MessageKind, Node, NodeId, Role, TodoState, attr};
use loa_util::text::{clip, clip_marked};
use std::fmt::Write as _;
use std::sync::Arc;

pub use loa_contracts::Summarizer;

/// Picks the [`Summarizer`] for a model or role (`None`: the default) and the model name
/// recorded as `<digest model>`.
pub trait Summarizers: Send + Sync {
    fn on(&self, model: Option<&str>) -> (Arc<dyn Summarizer>, Option<String>);
}

/// `omp -p` on the given model, `@smol` by default.
impl Summarizers for loa_harness::omp::OneShot {
    fn on(&self, model: Option<&str>) -> (Arc<dyn Summarizer>, Option<String>) {
        let model = self.pick(model).to_owned();
        let name = model.trim_start_matches('@').to_owned();
        (Arc::new(self.clone().with_model(model)), Some(name))
    }
}

/// One summarizer for every model.
impl Summarizers for Arc<dyn Summarizer> {
    fn on(&self, _: Option<&str>) -> (Arc<dyn Summarizer>, Option<String>) {
        (self.clone(), None)
    }
}

/// Size bounds of the digest prompt, in bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptLimits {
    /// Whole transcript; older turns are dropped past it.
    pub max_chars: usize,
    /// Newest turns rendered whole.
    pub full_turns: usize,
    pub message_chars: usize,
    /// A tool input or result in a whole turn.
    pub tool_chars: usize,
    /// A message in an abbreviated turn.
    pub brief_chars: usize,
}

impl Default for PromptLimits {
    fn default() -> Self {
        Self { max_chars: 24_000, full_turns: 4, message_chars: 2_000, tool_chars: 400, brief_chars: 200 }
    }
}

/// A rendered transcript and its `T<n>` turn labels.
#[derive(Clone, Debug)]
pub struct DigestPrompt {
    pub text: String,
    labels: Vec<NodeId>,
    pub source_seq: u64,
}

/// A digest ready for [`crate::write_drafts`].
#[derive(Clone, Debug, PartialEq)]
pub struct BuiltDigest {
    pub content: DigestContent,
    pub source_seq: u64,
    pub model: Option<String>,
}

/// Turns session DOMs into digests with one summarizer call each.
#[derive(Clone)]
pub struct Digester {
    summarizers: Arc<dyn Summarizers>,
    limits: PromptLimits,
}

impl Digester {
    pub fn new(summarizers: Arc<dyn Summarizers>, limits: PromptLimits) -> Self {
        Self { summarizers, limits }
    }

    /// `model` is the project's digest model, if it sets one.
    pub async fn digest(&self, dom: &Dom, model: Option<&str>) -> Result<BuiltDigest> {
        let prompt = build_prompt(dom, &self.limits)?;
        let (summarizer, model) = self.summarizers.on(model);
        let answer = summarizer.summarize(INSTRUCTIONS, &prompt.text).await.map_err(Error::Summarizer)?;
        Ok(BuiltDigest { content: parse_answer(&answer, &prompt)?, source_seq: prompt.source_seq, model })
    }
}

pub const INSTRUCTIONS: &str = r#"You write the digest of one coding session, so other sessions in the same project know what it did.
Answer with exactly one JSON object and nothing else, in this shape:
{"summary": "two or three sentences on what the session did",
 "decisions": ["each decision that was made, one short line"],
 "files": [{"path": "a file the session read or changed", "note": "one line on what happened to it"}],
 "open_items": ["unfinished todos and unanswered questions"],
 "pointers": ["labels like T3 of the turns the digest is based on"]}
Every key is required; use [] when there is nothing. Pointers must be labels that appear below.
"#;

/// The transcript of `dom`; [`Error::NothingToDigest`] without turns.
pub fn build_prompt(dom: &Dom, limits: &PromptLimits) -> Result<DigestPrompt> {
    let root = dom.root().ok_or(Error::NothingToDigest)?;
    let turns: Vec<&Node> = dom.children_of_kind(root.id, K::Turn).collect();
    if turns.is_empty() {
        return Err(Error::NothingToDigest);
    }
    let mut text = String::from("\n# Session\n");
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
    for (age, (i, turn)) in turns.iter().enumerate().rev().enumerate() {
        let section = render_turn(dom, turn, i + 1, age < limits.full_turns, limits);
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
    sections.iter().rev().for_each(|s| text.push_str(s));
    Ok(DigestPrompt { text, labels: turns.iter().map(|t| t.id).collect(), source_seq: dom.seq() })
}

fn render_open_items(dom: &Dom, root: &Node, limits: &PromptLimits, out: &mut String) {
    let open = |n: &&Node| match n.kind {
        K::Todo => n.str_attr(attr::STATE).and_then(TodoState::parse).is_none_or(|s| matches!(s, TodoState::Pending | TodoState::InProgress)),
        K::Decision => n.str_attr(attr::STATUS).and_then(DecisionStatus::parse) == Some(DecisionStatus::Waiting),
        _ => false,
    };
    let mut items: Vec<&Node> = dom.descendants(root.id).filter(open).collect();
    items.sort_by_key(|n| n.kind != K::Todo);
    if !items.is_empty() {
        out.push_str("\n## Open\n");
    }
    for n in items {
        let (what, text) = if n.kind == K::Todo { ("todo", n.text.as_str()) } else { ("waiting for an answer", n.str_attr(attr::TITLE).unwrap_or(&n.text)) };
        let _ = writeln!(out, "- {what}: {}", clip(text, limits.brief_chars));
    }
}

fn render_turn(dom: &Dom, turn: &Node, label: usize, full: bool, limits: &PromptLimits) -> String {
    let mut out = format!("\n### T{label}{}\n", if full { "" } else { " (abbreviated)" });
    let message_chars = if full { limits.message_chars } else { limits.brief_chars };
    let mut tools: Vec<String> = Vec::new();
    for node in dom.children(turn.id) {
        match node.kind {
            K::Message if node.str_attr(attr::KIND).and_then(MessageKind::parse) == Some(MessageKind::Thinking) => {}
            K::Message => {
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
                tools.push(node.str_attr(attr::TITLE).map_or_else(|| tool.to_owned(), |t| format!("{tool} ({})", clip(t, 80))));
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
    let _ = write!(out, "Tool {}", call.str_attr(attr::TOOL).unwrap_or("tool"));
    if let Some(title) = call.str_attr(attr::TITLE) {
        let _ = write!(out, " ({})", clip(title, 120));
    }
    if call.str_attr(attr::STATUS).and_then(CallStatus::parse) == Some(CallStatus::Error) {
        out.push_str(" [error]");
    }
    out.push('\n');
    for child in dom.children(call.id) {
        let part = match child.kind {
            K::Input => "input",
            K::Result => "result",
            K::Decision => {
                render_decision(child, limits.tool_chars, out);
                continue;
            }
            _ => continue,
        };
        let _ = writeln!(out, "  {part}: {}", clip_marked(child.text.trim(), limits.tool_chars));
    }
}

fn render_decision(decision: &Node, max: usize, out: &mut String) {
    let kind = decision.str_attr(attr::KIND).unwrap_or("decision");
    let answer: Vec<&str> = decision.attr(attr::ANSWER).map(|a| a.str_items().collect()).unwrap_or_default();
    let what = decision.str_attr(attr::TITLE).unwrap_or(&decision.text);
    let answer = if answer.is_empty() { "unanswered".to_owned() } else { answer.join(", ") };
    let _ = writeln!(out, "Decision ({kind}): {} -> {answer}", clip(what.trim(), max));
}

#[derive(serde::Deserialize)]
struct Answer {
    summary: String,
    decisions: Vec<String>,
    files: Vec<FileNote>,
    open_items: Vec<String>,
    pointers: Vec<String>,
}

/// Parses and validates an answer to `prompt`, ignoring text around the JSON object.
pub fn parse_answer(text: &str, prompt: &DigestPrompt) -> Result<DigestContent> {
    let bad = |why: String| Error::InvalidAnswer(why);
    let json = match (text.find('{'), text.rfind('}')) {
        (Some(start), Some(end)) if start < end => &text[start..=end],
        _ => return Err(bad("no JSON object in the answer".into())),
    };
    let answer: Answer = serde_json::from_str(json).map_err(|e| bad(e.to_string()))?;
    let summary = answer.summary.trim();
    if summary.is_empty() {
        return Err(bad("empty summary".into()));
    }
    let lines = |items: Vec<String>| items.into_iter().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
    let mut files = Vec::new();
    for file in answer.files {
        let path = file.path.trim();
        if path.is_empty() {
            return Err(bad("a file entry has an empty path".into()));
        }
        files.push(FileNote { path: path.to_owned(), note: file.note.trim().to_owned() });
    }
    let mut pointers = Vec::new();
    for label in &answer.pointers {
        let label = label.trim();
        let index = label.strip_prefix(['T', 't']).and_then(|n| n.parse::<usize>().ok());
        let id = index.and_then(|n| prompt.labels.get(n.checked_sub(1)?)).copied();
        let id = id.ok_or_else(|| bad(format!("pointer `{label}` names no turn")))?;
        if !pointers.contains(&id) {
            pointers.push(id);
        }
    }
    Ok(DigestContent { summary: summary.to_owned(), decisions: lines(answer.decisions), files, open_items: lines(answer.open_items), pointers })
}
