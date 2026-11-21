//! Rules and advisor as hooks around each turn, for harnesses that lack
//! them ([`loa_contracts::harness::Feature::AdvisorRules`] missing).
//!
//! What the app runs:
//! - Rules are the omp rule files: `~/.omp/agent/rules/*.md` and the
//!   project's `.omp/rules/*.md` (a project rule overrides a global one of
//!   the same name). A rule with `alwaysApply: true` or no front matter is
//!   included whole; any other rule is listed by its `description` with the
//!   path to read when it applies. The rules text goes into the session's
//!   context at start ([`TurnHooks::start`]); before each turn the hook
//!   checks the files again and, when they changed, sends the new text with
//!   the prompt.
//! - The advisor reviews each finished turn with one small-model call
//!   ([`Advisor::review`]) against the rules. A note is recorded in the turn
//!   as `<message role="system" by="advisor">` and sent to the agent with the
//!   next prompt. A turn that needs no remark gets no note.

use crate::smol::OneShot;
use crate::{Result, turn_transcript};
use loa_contracts::harness::SessionConfig;
use loa_core::{Dom, Draft, ElementKind, MessageStatus, NodeId, PatchSink, Role, Value, attr};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `by` of advisor notes.
pub const ADVISOR: &str = "advisor";

/// One rule file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// File stem.
    pub name: String,
    pub path: PathBuf,
    pub description: Option<String>,
    /// Included whole in every turn's context.
    pub always: bool,
    /// Text after the front matter.
    pub body: String,
}

/// The rule files that apply in `cwd`, global first, sorted by name.
pub fn discover_rules(cwd: &Path, home: &Path) -> Vec<Rule> {
    let mut rules = BTreeMap::new();
    for dir in [home.join(".omp/agent/rules"), cwd.join(".omp/rules")] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "md") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            rules.insert(name.clone(), parse_rule(name, path, &text));
        }
    }
    rules.into_values().collect()
}

fn parse_rule(name: String, path: PathBuf, text: &str) -> Rule {
    let Some((front, body)) = text.strip_prefix("---").and_then(|rest| rest.split_once("\n---")) else {
        return Rule { name, path, description: None, always: true, body: text.trim().to_owned() };
    };
    let mut description = None;
    let mut always = false;
    for line in front.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim().trim_matches(['"', '\'']).trim();
        match key.trim() {
            "description" if !value.is_empty() => description = Some(value.to_owned()),
            "alwaysApply" => always = value == "true",
            _ => {}
        }
    }
    let body = body.split_once('\n').map_or("", |(_, rest)| rest).trim().to_owned();
    Rule { name, path, description, always, body }
}

/// The context text for `rules`, or `None` without rules.
pub fn rules_context(rules: &[Rule]) -> Option<String> {
    if rules.is_empty() {
        return None;
    }
    let mut out = String::from("Rules for this project. Follow them.\n");
    for rule in rules.iter().filter(|r| r.always) {
        out.push_str(&format!("\n## Rule: {}\n{}\n", rule.name, rule.body));
    }
    let listed: Vec<_> = rules.iter().filter(|r| !r.always).collect();
    if !listed.is_empty() {
        out.push_str("\nRules to read when they apply:\n");
        for rule in listed {
            let what = rule.description.as_deref().unwrap_or("no description");
            out.push_str(&format!("- {}: {what} ({})\n", rule.name, rule.path.display()));
        }
    }
    Some(out)
}

/// The per session state of the hooks.
#[derive(Debug, Default)]
pub struct TurnHooks {
    /// Rules text the agent has.
    rules: Option<String>,
    /// Advisor note not sent yet.
    note: Option<String>,
}

impl TurnHooks {
    /// Hooks of a session starting with `rules`, which go into its context.
    pub fn start(config: &mut SessionConfig, rules: Option<String>) -> Self {
        if let Some(rules) = &rules {
            let context = config.context.get_or_insert_with(String::new);
            if !context.is_empty() {
                context.push_str("\n\n");
            }
            context.push_str(rules);
        }
        Self { rules, note: None }
    }

    /// Hooks of a session resumed or reopened whose context already carries
    /// `rules`.
    pub fn resumed(rules: Option<String>) -> Self {
        Self { rules, note: None }
    }

    /// The text to send for the user's `text`, given the rules as they are
    /// now: changed rules and the pending advisor note go first.
    pub fn before_turn(&mut self, rules: Option<String>, text: &str) -> String {
        let mut out = String::new();
        if rules != self.rules {
            match &rules {
                Some(rules) => out.push_str(&format!("[The project rules changed]\n{rules}\n\n")),
                None => out.push_str("[The project rules were removed]\n\n"),
            }
            self.rules = rules;
        }
        if let Some(note) = self.note.take() {
            out.push_str(&format!("[Advisor note on your last turn]\n{note}\n\n"));
        }
        if out.is_empty() {
            return text.to_owned();
        }
        out.push_str(text);
        out
    }

    /// Queues an advisor note for the next prompt.
    pub fn advise(&mut self, note: String) {
        self.note = Some(note);
    }

    pub fn pending_note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    pub fn rules(&self) -> Option<&str> {
        self.rules.as_deref()
    }
}

/// Reviews finished turns with one small-model call each.
#[derive(Clone)]
pub struct Advisor {
    model: OneShot,
    /// Model or role, e.g. `@smol` or `@slow`.
    role: String,
}

/// The advisor's answer for a turn that needs no note.
const NO_NOTE: &str = "OK";

impl Advisor {
    pub fn new(model: OneShot, role: impl Into<String>) -> Self {
        Self { model, role: role.into() }
    }

    /// What the advisor model is asked about `turn`. Plain headings, no XML:
    /// omp treats some tags in a prompt (`<rules>`, `<turn>`) as its own.
    pub fn prompt(dom: &Dom, turn: NodeId, rules: Option<&str>) -> String {
        let mut prompt = String::from(
            "You are the advisor of a coding agent. Check the agent's last turn against the project rules and \
             the user's request: broken rules, bugs, missed parts of the request, risky actions, wrong \
             assumptions. If you find a problem, write a short note to the agent (at most five sentences) that \
             names it and says what to do. Only if there is no problem at all, answer with the single word OK.\n",
        );
        if let Some(rules) = rules {
            prompt.push_str("\nProject rules:\n");
            prompt.push_str(rules.trim_end());
            prompt.push('\n');
        }
        prompt.push_str("\nThe agent's last turn:\n");
        prompt.push_str(&turn_transcript(dom, turn));
        prompt.push_str("\nEnd of the turn. Now give your note to the agent, or the single word OK.\n");
        prompt
    }

    /// The note on `turn`, or `None` when it needs none.
    pub async fn review(&self, dom: &Dom, turn: NodeId, rules: Option<&str>) -> Result<Option<String>> {
        let answer = self.model.ask(&self.role, &Self::prompt(dom, turn, rules)).await?;
        let note = answer.trim().trim_end_matches('.').trim();
        Ok((!note.is_empty() && !note.eq_ignore_ascii_case(NO_NOTE)).then(|| answer.trim().to_owned()))
    }
}

/// Records an advisor note at the end of `turn`.
pub fn record_note(sink: &dyn PatchSink, turn: NodeId, note: &str) -> Result<NodeId> {
    Ok(sink
        .commit(Draft::insert(
            turn,
            ElementKind::Message,
            [
                (attr::ROLE, Value::from(Role::System)),
                (attr::BY, Value::from(ADVISOR)),
                (attr::STATUS, Value::from(MessageStatus::Done)),
            ],
            note,
        ))?
        .node)
}
