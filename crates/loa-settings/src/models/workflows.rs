//! Workflows stored as omp model roles and sections of the agent RULES.md.

use std::ops::Range;
use std::path::{Path, PathBuf};

use loa_util::fs::{self, Snapshot};
use super::config::OmpConfig;
use super::error::{Error, Result, SettingsValidator, load_snapshot, read_optional, save_snapshot};
use super::files::OmpFiles;
use super::selector::{Selector, validate_role_name};

const START: &str = "<!-- omp-gui:workflow ";
const START_PREFIX: &str = "<!-- omp-gui:workflow";
const END: &str = "<!-- /omp-gui:workflow -->";
const END_PREFIX: &str = "<!-- /omp-gui:workflow";
const WHEN: &str = "When the active model role is `";
const ALLOWED: &str = "- Allowed agents: ";
const DISALLOWED: &str = "- Disallowed agents: ";
const FIELD: &str = "<!-- loa:workflow-field -->";
const NOTES_FIELD: &str = "<!-- loa:workflow-notes -->";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManagedFields {
    pub description: String,
    pub routing: Vec<RoutingNote>,
    pub steps: Vec<String>,
    pub blocks: Vec<RuleBlock>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoutingNote {
    pub agent: String,
    pub note: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleBlock {
    pub heading: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workflow {
    pub role: String,
    pub models: Vec<Selector>,
    pub rules: Rules,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rules {
    /// A marker-delimited section; `name` is its display name.
    Managed { name: String, allowed: Vec<String>, disallowed: Vec<String>, notes: String, fields: ManagedFields },
    /// A marker-delimited body that cannot be represented by the structured editor.
    External { text: String },
    HandWritten { text: String },
    None,
}

/// Immutable load-time contents of both files, used to reject stale edits.
#[derive(Clone, Debug)]
pub struct WorkflowSnapshot {
    config: Snapshot,
    rules: Snapshot,
}

#[derive(Clone, Debug)]
pub struct WorkflowFiles {
    pub config: PathBuf,
    pub rules: PathBuf,
}

struct Block { role: String, span: Range<usize>, rules: Rules }
struct Parsed { blocks: Vec<Block>, crlf: bool }

fn malformed(reason: impl Into<String>) -> Error { Error::invalid("RULES.md", reason) }

/// Derives a role from a display name.
pub fn role_for(name: &str) -> Result<String> {
    let mut role = String::new();
    let mut gap = false;
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            if gap && !role.is_empty() { role.push('_'); }
            gap = false;
            role.push(c.to_ascii_uppercase());
        } else { gap = true; }
    }
    let role = role.trim_matches('_').to_owned();
    validate_role_name(&role).map_err(|_| Error::invalid("workflow name", "must contain letters or digits"))?;
    Ok(role)
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&level) && line.as_bytes().get(level) == Some(&b' ') {
        Some((level, line[level + 1..].trim()))
    } else { None }
}

fn backticked(line: &str) -> Vec<String> {
    line.split_once(". ").map_or(line, |(a, _)| a).split('`').skip(1).step_by(2).map(str::to_owned).collect()
}

fn structured_tail(lines: &[(usize, &str)]) -> Option<(String, ManagedFields)> {
    let boundary = |line| matches!(line, FIELD | NOTES_FIELD);
    let first = lines.iter().position(|(_, line)| boundary(*line)).unwrap_or(lines.len());
    let mut notes = lines[..first].iter().map(|(_, line)| *line).collect::<Vec<_>>().join("\n").trim_matches('\n').to_owned();
    let mut fields = ManagedFields::default();
    let mut i = first;
    while i < lines.len() {
        if lines[i].1 == NOTES_FIELD {
            if !notes.is_empty() || lines.get(i + 1)?.1 != "### Notes" || lines.get(i + 2)?.1 != "" { return None; }
            notes = lines[i + 3..].iter().map(|(_, line)| *line).collect::<Vec<_>>().join("\n");
            break;
        }
        let end = (i + 1..lines.len()).find(|&j| boundary(lines[j].1)).unwrap_or(lines.len());
        let section = &lines[i + 1..end];
        let heading = section.first()?.1.strip_prefix("### ")?;
        if section.get(1)?.1 != "" { return None; }
        let content_lines = &section[2..];
        let content_lines = if end < lines.len() && content_lines.last().is_some_and(|(_, line)| line.is_empty()) {
            &content_lines[..content_lines.len() - 1]
        } else { content_lines };
        let content = content_lines.iter().map(|(_, line)| *line).collect::<Vec<_>>().join("\n");
        let content = content.as_str();
        match heading {
            "When to use" if fields.description.is_empty() => fields.description = content.to_owned(),
            "Routing" if fields.routing.is_empty() => {
                for line in content.lines() {
                    let (agent, note) = line.strip_prefix("- `")?.split_once("`: ")?;
                    fields.routing.push(RoutingNote { agent: agent.to_owned(), note: note.to_owned() });
                }
            }
            "Steps" if fields.steps.is_empty() => {
                for (index, line) in content.lines().enumerate() {
                    fields.steps.push(line.strip_prefix(&format!("{}. ", index + 1))?.to_owned());
                }
            }
            "When to use" | "Routing" | "Steps" => return None,
            _ => fields.blocks.push(RuleBlock { heading: heading.to_owned(), text: content.to_owned() }),
        }
        i = end;
    }
    Some((notes, fields))
}

fn parse(text: &str) -> Result<Parsed> {
    let crlf = text.find('\n').is_some_and(|i| text[..i].ends_with('\r'));
    let mut lines = Vec::new();
    let mut offset = 0;
    for raw in text.split_inclusive('\n') {
        lines.push((offset, raw.trim_end_matches(['\r', '\n'])));
        offset += raw.len();
    }
    let mut blocks: Vec<Block> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let (start, line) = lines[i];
        let trimmed = line.trim();
        if trimmed.starts_with(END_PREFIX) { return Err(malformed(format!("line {} has an unmatched workflow end marker", i + 1))); }
        if trimmed.starts_with(START_PREFIX) {
            let role = trimmed.strip_prefix(START).and_then(|s| s.strip_suffix("-->"))
                .map(str::trim).filter(|s| !s.is_empty() && !s.contains(char::is_whitespace))
                .ok_or_else(|| malformed(format!("line {} has a malformed workflow start marker", i + 1)))?;
            let mut close = None;
            for (j, (_, line)) in lines.iter().enumerate().skip(i + 1) {
                let t = line.trim();
                if t.starts_with(START_PREFIX) { return Err(malformed("nested workflow start marker")); }
                if t.starts_with(END_PREFIX) {
                    if t != END { return Err(malformed("malformed workflow end marker")); }
                    close = Some(j); break;
                }
            }
            let close = close.ok_or_else(|| malformed("workflow start marker has no end marker"))?;
            let body = &lines[i + 1..close];
            // The first role declaration binds the marked section. Notes may quote other modes.
            let when = body.iter().position(|(_, line)| line.starts_with(WHEN));
            if let Some((named, _)) = when.and_then(|i| body[i].1[WHEN.len()..].split_once('`'))
                && named != role
            {
                return Err(malformed(format!("workflow marker `{role}` does not match its role line")));
            }
            let span = start..lines[close].0 + lines[close].1.len();
            let body_start = lines.get(i + 1).map_or(lines[close].0, |(offset, _)| *offset);
            let external = || Rules::External { text: text[body_start..lines[close].0].to_owned() };
            let candidate = when.and_then(|when| {
                let name = body.iter().find_map(|(_, l)| l.strip_prefix("## "))
                    .and_then(|s| s.trim().strip_suffix(" Mode")).unwrap_or(role).trim().to_owned();
                let (mut allowed, mut disallowed) = (Vec::new(), Vec::new());
                let mut n = when + 1;
                if body.get(n).is_some_and(|(_, line)| line.is_empty()) { n += 1; }
                loop {
                    match body.get(n).map(|(_, l)| *l) {
                        Some(l) if l.starts_with(ALLOWED) => allowed = backticked(l),
                        Some(l) if l.starts_with(DISALLOWED) => disallowed = backticked(l),
                        _ => break,
                    }
                    n += 1;
                }
                let (notes, fields) = structured_tail(&body[n..])?;
                Some(Rules::Managed { name, allowed, disallowed, notes, fields })
            });
            let rules = candidate.filter(|rules| {
                render(role, rules, false).is_ok_and(|rendered| rendered == text[span.clone()].replace("\r\n", "\n"))
            }).unwrap_or_else(external);
            blocks.push(Block { role: role.to_owned(), span, rules });
            i = close + 1;
            continue;
        }
        if let Some((level, title)) = heading(line)
            && let Some(_name) = title.strip_suffix(" Mode")
        {
            let mut end = i + 1;
            while end < lines.len() {
                let t = lines[end].1.trim();
                if t.starts_with(START_PREFIX) || t.starts_with(END_PREFIX) || heading(lines[end].1).is_some_and(|(l, _)| l <= level) { break; }
                end += 1;
            }
            let role = lines[i + 1..end].iter().find_map(|(_, l)| {
                let tail = l.split_once(WHEN)?.1;
                Some(tail.split_once('`')?.0)
            });
            if let Some(role) = role {
                let finish = lines.get(end).map_or(text.len(), |(p, _)| *p);
                blocks.push(Block { role: role.to_owned(), span: start..finish, rules: Rules::HandWritten { text: text[start..finish].to_owned() } });
            }
            i = end;
            continue;
        }
        i += 1;
    }
    for (i, block) in blocks.iter().enumerate() {
        if blocks[..i].iter().any(|b| b.role.eq_ignore_ascii_case(&block.role)) {
            return Err(malformed(format!("the role `{}` is defined more than once", block.role)));
        }
    }
    Ok(Parsed { blocks, crlf })
}

fn render(role: &str, rules: &Rules, crlf: bool) -> Result<String> {
    match rules {
        Rules::None => Ok(String::new()),
        Rules::HandWritten { text } => {
            if text.contains("omp-gui:workflow") { return Err(Error::invalid("rules", "hand-written rules must not contain workflow markers")); }
            // The editor keeps old lines' endings and gives typed lines its own: the section takes the file's.
            let lf = text.replace("\r\n", "\n");
            Ok(if crlf { lf.replace('\n', "\r\n") } else { lf })
        }
        Rules::External { .. } => Err(Error::invalid("rules", "external rules must retain their existing markers")),
        Rules::Managed { name, allowed, disallowed, notes, fields } => {
            if name.trim().is_empty() || name.trim() != name || name.contains(['\n', '\r', '`']) { return Err(Error::invalid("workflow name", "must be one line without backticks")); }
            if allowed.iter().chain(disallowed).any(|a| a.trim().is_empty() || a.contains(['\n', '\r', '`'])) { return Err(Error::invalid("agents", "invalid agent name")); }
            let has_workflow_marker = |s: &str| s.lines().map(str::trim_start).any(|line| line.starts_with(START_PREFIX) || line.starts_with(END_PREFIX));
            let valid_text = |s: &str| !has_workflow_marker(s) && !s.lines().any(|line| matches!(line, FIELD | NOTES_FIELD));
            if has_workflow_marker(notes) || !valid_text(&fields.description)
                || fields.blocks.iter().any(|b| !valid_text(&b.text))
                || fields.steps.iter().any(|s| s.is_empty() || s.contains(['\n', '\r']) || !valid_text(s))
                || fields.routing.iter().enumerate().any(|(index, r)| {
                    !allowed.contains(&r.agent) || r.note.is_empty() || r.note.contains(['\n', '\r']) || !valid_text(&r.note)
                        || fields.routing[..index].iter().any(|other| other.agent == r.agent)
                })
                || fields.blocks.iter().any(|b| b.heading.is_empty() || b.heading.trim() != b.heading || b.heading.contains(['\n', '\r']) || matches!(b.heading.as_str(), "When to use" | "Routing" | "Steps"))
            {
                return Err(Error::invalid("workflow fields", "invalid field text, routing agent, or block heading"));
            }
            let mut out = format!("{START}{role} -->\n## {name} Mode\n\n{WHEN}{role}`, operate in {name} Mode.\n");
            for (label, rule, agents) in [(ALLOWED, "Route work only to these agents.", allowed), (DISALLOWED, "Never route work to these agents.", disallowed)] {
                if !agents.is_empty() {
                    if out.ends_with("Mode.\n") { out.push('\n'); }
                    let list = agents.iter().map(|a| format!("`{a}`")).collect::<Vec<_>>().join(", ");
                    out.push_str(&format!("{label}{list}. {rule}\n"));
                }
            }
            let notes = notes.replace("\r\n", "\n");
            let notes = notes.trim_matches('\n');
            let marked_notes = notes.lines().any(|line| matches!(line, FIELD | NOTES_FIELD))
                || [ALLOWED, DISALLOWED].iter().any(|prefix| notes.starts_with(*prefix));
            if !notes.is_empty() && !marked_notes { out.push('\n'); out.push_str(notes); out.push('\n'); }
            let mut section = |heading: &str, content: &str| {
                out.push_str(&format!("\n{FIELD}\n### {heading}\n\n"));
                out.push_str(content);
                out.push('\n');
            };
            if !fields.description.is_empty() { section("When to use", &fields.description.replace("\r\n", "\n")); }
            if !fields.routing.is_empty() {
                let routing = fields.routing.iter().map(|r| format!("- `{}`: {}", r.agent, r.note)).collect::<Vec<_>>().join("\n");
                section("Routing", &routing);
            }
            if !fields.steps.is_empty() {
                let steps = fields.steps.iter().enumerate().map(|(i, step)| format!("{}. {step}", i + 1)).collect::<Vec<_>>().join("\n");
                section("Steps", &steps);
            }
            for block in &fields.blocks { section(&block.heading, &block.text.replace("\r\n", "\n")); }
            // When copied Markdown contains reserved lines, a terminal Notes field
            // takes the remainder verbatim instead of interpreting those lines.
            if marked_notes {
                out.push_str("\n");
                out.push_str(NOTES_FIELD);
                out.push_str("\n### Notes\n\n");
                out.push_str(notes);
                out.push('\n');
            }
            out.push_str(END);
            Ok(if crlf { out.replace('\n', "\r\n") } else { out })
        }
    }
}

/// Refuses `new_rules` unless every section other than the edited one reads as before (blank
/// lines that now separate it from a new section aside) and the edited one spans `edited`
/// (empty when it has no section).
fn check_blocks(text: &str, parsed: &Parsed, previous_role: Option<&str>, new_rules: &str, workflow: &Workflow, edited: Range<usize>) -> Result<()> {
    let after = parse(new_rules)?;
    let before = parsed.blocks.iter().filter(|b| Some(b.role.as_str()) != previous_role).map(|b| (b.role.as_str(), text[b.span.clone()].trim_end()));
    let kept = after.blocks.iter().filter(|b| b.role != workflow.role).map(|b| (b.role.as_str(), new_rules[b.span.clone()].trim_end()));
    let block = after.blocks.iter().find(|b| b.role == workflow.role);
    if matches!(workflow.rules, Rules::Managed { .. }) && !block.is_some_and(|b| matches!(b.rules, Rules::Managed { .. })) {
        return Err(Error::invalid("workflow fields", "these fields cannot be read back as structured rules"));
    }
    let span = block.map(|b| b.span.clone());
    if !before.eq(kept) || span.unwrap_or(edited.start..edited.start) != edited {
        return Err(Error::invalid("rules", "the edit would change other sections of RULES.md: keep the section's heading level"));
    }
    Ok(())
}

impl WorkflowFiles {
    pub fn new(files: &OmpFiles) -> Self { Self::in_agent_dir(&files.agent_dir) }
    pub fn in_agent_dir(dir: &Path) -> Self { Self { config: dir.join("config.yml"), rules: dir.join("RULES.md") } }

    /// Loads all config roles and an optimistic-concurrency snapshot of both files.
    pub fn load(&self) -> Result<(Vec<Workflow>, WorkflowSnapshot)> {
        let (rules, text) = load_snapshot(&self.rules)?;
        let parsed = parse(text.as_deref().unwrap_or(""))?;
        let (config_snapshot, _) = load_snapshot(&self.config)?;
        let config = OmpConfig::load(&self.config)?;
        self.check_one(&config_snapshot)?;
        let workflows = config.model_roles()?.into_iter().map(|assignment| {
            let rules = parsed.blocks.iter().find(|b| b.role == assignment.role).map_or(Rules::None, |b| b.rules.clone());
            Workflow { role: assignment.role, models: assignment.candidates, rules }
        }).collect();
        Ok((workflows, WorkflowSnapshot { config: config_snapshot, rules }))
    }

    fn check_one(&self, snapshot: &Snapshot) -> Result<()> {
        let current = fs::read_optional(snapshot.path()).map_err(|e| Error::io(snapshot.path(), e))?;
        if current.as_deref() != snapshot.contents() { return Err(Error::ChangedOnDisk { path: snapshot.path().to_owned() }); }
        Ok(())
    }
    fn check_snapshot(&self, snapshot: &WorkflowSnapshot) -> Result<()> {
        if snapshot.config.path() != self.config || snapshot.rules.path() != self.rules { return Err(Error::invalid("snapshot", "snapshot belongs to different workflow files")); }
        self.check_one(&snapshot.config)?;
        self.check_one(&snapshot.rules)
    }

    /// Creates, updates or renames a workflow. Existing handwritten sections remain handwritten.
    /// `previous_role` is `None` for a new workflow and `Some(old_role)` for
    /// an update or rename. Call [`Self::load`] again after a successful edit
    /// to obtain a fresh snapshot before the next edit.
    pub async fn save(&self, workflow: &Workflow, previous_role: Option<&str>, snapshot: &WorkflowSnapshot, validator: &dyn SettingsValidator) -> Result<()> {
        self.check_snapshot(snapshot)?;
        validate_role_name(&workflow.role)?;
        if workflow.role.eq_ignore_ascii_case("default") { return Err(Error::invalid("role", "`default` is reserved")); }
        let text = std::str::from_utf8(snapshot.rules.contents().unwrap_or_default()).map_err(|e| Error::invalid("RULES.md", e.to_string()))?;
        let parsed = parse(text)?;
        let mut config = OmpConfig::load(&self.config)?;
        self.check_snapshot(snapshot)?;
        let old = previous_role.map(|role| config.model_role(role)).transpose()?.flatten();
        if previous_role.is_some() && old.is_none() { return Err(Error::invalid("role", "workflow role no longer exists")); }
        if previous_role != Some(workflow.role.as_str()) {
            if config.model_roles()?.iter().any(|r| Some(r.role.as_str()) != previous_role && r.role.eq_ignore_ascii_case(&workflow.role)) || parsed.blocks.iter().any(|b| Some(b.role.as_str()) != previous_role && b.role.eq_ignore_ascii_case(&workflow.role)) {
                return Err(Error::invalid("role", "role already exists (case insensitive)"));
            }
        }
        let existing = previous_role.and_then(|r| parsed.blocks.iter().find(|b| b.role == r));
        if let Some(block) = existing {
            if std::mem::discriminant(&block.rules) != std::mem::discriminant(&workflow.rules) { return Err(Error::invalid("rules", "cannot convert between managed and hand-written sections")); }
        }
        let eol = if parsed.crlf { "\r\n" } else { "\n" };
        let mut replacement = if let Rules::External { text: body } = &workflow.rules {
            let block = existing.ok_or_else(|| Error::invalid("rules", "external rules must retain their existing marker"))?;
            if previous_role != Some(workflow.role.as_str()) {
                return Err(Error::invalid("role", "an externally edited workflow cannot be renamed"));
            }
            let original = &text[block.span.clone()];
            if block.rules == workflow.rules {
                original.to_owned()
            } else {
                if body.lines().any(|line| {
                    let line = line.trim_start();
                    line.starts_with(START_PREFIX) || line.starts_with(END_PREFIX)
                }) {
                    return Err(Error::invalid("rules", "the raw text must not contain workflow markers"));
                }
                let start = original.find('\n').ok_or_else(|| malformed("workflow marker has no body"))? + 1;
                let end = original.rfind('\n').ok_or_else(|| malformed("workflow marker has no end"))? + 1;
                let boundary = if body.is_empty() || body.ends_with('\n') { "" }
                    else if original[..start].ends_with("\r\n") { "\r\n" } else { "\n" };
                format!("{}{body}{boundary}{}", &original[..start], &original[end..])
            }
        } else if let Some(block) = existing
            && matches!(workflow.rules, Rules::Managed { .. })
            && previous_role == Some(workflow.role.as_str())
            && block.rules == workflow.rules
        {
            text[block.span.clone()].to_owned()
        } else {
            render(&workflow.role, &workflow.rules, parsed.crlf)?
        };
        if matches!(workflow.rules, Rules::HandWritten { .. }) {
            if let Some(previous) = previous_role && previous != workflow.role {
                replacement = replacement.replace(&format!("`{previous}`"), &format!("`{}`", workflow.role));
            }
            // Text that follows must still start on its own line.
            if !replacement.ends_with('\n') && existing.is_none_or(|block| block.span.end < text.len()) { replacement.push_str(eol); }
            let checked = parse(&replacement)?;
            if checked.blocks.len() != 1 || checked.blocks[0].role != workflow.role || checked.blocks[0].span != (0..replacement.len()) { return Err(Error::invalid("rules", "hand-written text must remain one mode section naming its role")); }
        }
        let (start, new_rules) = if let Some(block) = existing {
            (block.span.start, format!("{}{}{}", &text[..block.span.start], replacement, &text[block.span.end..]))
        } else if replacement.is_empty() { (text.len(), text.to_owned()) } else {
            let mut out = text.to_owned();
            if !out.is_empty() { if !out.ends_with('\n') { out.push_str(eol); } out.push_str(eol); }
            let start = out.len();
            out.push_str(&replacement);
            if !out.ends_with('\n') { out.push_str(eol); }
            (start, out)
        };
        check_blocks(text, &parsed, previous_role, &new_rules, workflow, start..start + replacement.len())?;
        let candidates = match previous_role {
            None if workflow.models.is_empty() => config.model_role("default")?.ok_or_else(|| Error::invalid("modelRoles", "there is no default role to copy"))?.candidates,
            _ => workflow.models.clone(),
        };
        if previous_role != Some(workflow.role.as_str()) || old.as_ref().is_some_and(|r| r.candidates != candidates) {
            config.set_model_role(&workflow.role, &candidates)?;
            if let Some(previous) = previous_role && previous != workflow.role { config.clear_model_role(previous)?; }
        }
        self.commit(config, snapshot, &new_rules, validator).await
    }

    pub async fn delete(&self, role: &str, snapshot: &WorkflowSnapshot, validator: &dyn SettingsValidator) -> Result<()> {
        self.check_snapshot(snapshot)?;
        if role.eq_ignore_ascii_case("default") { return Err(Error::invalid("role", "`default` is reserved")); }
        let text = std::str::from_utf8(snapshot.rules.contents().unwrap_or_default()).map_err(|e| Error::invalid("RULES.md", e.to_string()))?;
        let parsed = parse(text)?;
        let mut config = OmpConfig::load(&self.config)?;
        self.check_snapshot(snapshot)?;
        if config.model_role(role)?.is_none() { return Err(Error::invalid("role", "workflow role does not exist")); }
        config.clear_model_role(role)?;
        let new_rules = if let Some(block) = parsed.blocks.iter().find(|b| b.role == role) {
            let (mut start, mut end) = (block.span.start, block.span.end);
            if matches!(block.rules, Rules::Managed { .. }) {
                if text[end..].starts_with("\r\n") { end += 2; } else if text[end..].starts_with('\n') { end += 1; }
                let before = &text[..start];
                let len = if before.ends_with("\r\n") { 2 } else { usize::from(before.ends_with('\n')) };
                if len > 0 && before[..before.len() - len].ends_with('\n') { start -= len; }
            }
            format!("{}{}", &text[..start], &text[end..])
        } else { text.to_owned() };
        self.commit(config, snapshot, &new_rules, validator).await
    }

    async fn commit(&self, mut config: OmpConfig, snapshot: &WorkflowSnapshot, new_rules: &str, validator: &dyn SettingsValidator) -> Result<()> {
        self.check_snapshot(snapshot)?;
        let edited = config.is_edited();
        if edited { config.validate(validator).await?; self.check_snapshot(snapshot)?; config.save()?; }
        #[cfg(test)]
        if let Some(hook) = tests::BETWEEN_WRITES.with(|h| h.borrow_mut().take()) { hook(); }
        // A models-only edit leaves RULES.md alone, absent or not.
        if snapshot.rules.contents().unwrap_or_default() == new_rules.as_bytes() { return Ok(()); }
        let mut rules_snapshot = snapshot.rules.clone();
        if let Err(err) = save_snapshot(&mut rules_snapshot, new_rules) {
            if !edited { return Err(err); }
            match read_optional(&self.config) {
                Ok(current) if current.as_deref() == Some(config.text()) => {
                    let restored = match snapshot.config.contents() {
                        Some(bytes) => fs::write_atomic(&self.config, bytes, None),
                        None => std::fs::remove_file(&self.config),
                    };
                    return match restored { Ok(()) => Err(err), Err(e) => Err(Error::invalid("workflow", format!("writing RULES.md failed ({err}); restoring config.yml failed ({e})"))) };
                }
                _ => return Err(Error::invalid("workflow", format!("writing RULES.md failed ({err}); config.yml changed afterwards and was not restored"))),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        pub(super) static BETWEEN_WRITES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    }

    struct Accept;
    #[async_trait::async_trait]
    impl SettingsValidator for Accept {
        async fn check_config(&self, _: &str, _: &[(String, serde_json::Value)]) -> Result<()> { Ok(()) }
        async fn check_models(&self, _: &str) -> Result<()> { Ok(()) }
    }

    const CONFIG: &str = "modelRoles:\n  default: vendor/default\n  DEEP: vendor/model:high\n  OTHER: vendor/other\n";
    fn setup(rules: &str) -> (tempfile::TempDir, WorkflowFiles) {
        let dir = tempfile::tempdir().unwrap();
        let files = WorkflowFiles::in_agent_dir(dir.path());
        std::fs::write(&files.config, CONFIG).unwrap();
        std::fs::write(&files.rules, rules).unwrap();
        (dir, files)
    }
    fn deep(files: &WorkflowFiles) -> (Workflow, WorkflowSnapshot) {
        let (workflows, snapshot) = files.load().unwrap();
        (workflows.into_iter().find(|w| w.role == "DEEP").unwrap(), snapshot)
    }
    fn managed(role: &str) -> Workflow {
        Workflow { role: role.into(), models: vec![], rules: Rules::Managed {
            name: "Research".into(), allowed: vec!["scout".into()], disallowed: vec![],
            notes: "Cite sources.".into(), fields: ManagedFields::default(),
        } }
    }

    #[tokio::test]
    async fn handwritten_edit_preserves_outside_bytes_lf_and_crlf() {
        for eol in ["\n", "\r\n"] {
            let prefix = format!("# Rules{eol}{eol}Before{eol}{eol}");
            let section = format!("## Deep Mode{eol}{eol}When the active model role is `DEEP`, follow this.{eol}{eol}### Nested{eol}text{eol}{eol}");
            let suffix = format!("## Other{eol}{eol}After{eol}");
            let (_dir, files) = setup(&format!("{prefix}{section}{suffix}"));
            let (mut workflow, snapshot) = deep(&files);
            let replacement = section.replace("text", "changed");
            workflow.rules = Rules::HandWritten { text: replacement.clone() };
            files.save(&workflow, Some("DEEP"), &snapshot, &Accept).await.unwrap();
            assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), format!("{prefix}{replacement}{suffix}"));
        }
    }

    #[tokio::test]
    async fn handwritten_rename_only_changes_section_and_keeps_models() {
        let original = "outside `DEEP`\n\n## Deep Mode\nWhen the active model role is `DEEP`, mention `DEEP`.\n\n## Other\noutside `DEEP`\n";
        let (_dir, files) = setup(original);
        let (mut workflow, snapshot) = deep(&files);
        workflow.role = "STUDY".into();
        files.save(&workflow, Some("DEEP"), &snapshot, &Accept).await.unwrap();
        assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), "outside `DEEP`\n\n## Deep Mode\nWhen the active model role is `STUDY`, mention `STUDY`.\n\n## Other\noutside `DEEP`\n");
        let (list, _) = files.load().unwrap();
        assert_eq!(list.iter().find(|w| w.role == "STUDY").unwrap().models, vec![Selector::parse("vendor/model:high").unwrap()]);
    }

    #[tokio::test]
    async fn handwritten_delete_removes_exact_section() {
        for eol in ["\n", "\r\n"] {
            let prefix = format!("# Rules{eol}{eol}before{eol}{eol}");
            let section = format!("## Deep Mode{eol}When the active model role is `DEEP`, act.{eol}{eol}");
            let suffix = format!("## Other{eol}after{eol}");
            let (_dir, files) = setup(&format!("{prefix}{section}{suffix}"));
            let (_, snapshot) = deep(&files);
            files.delete("DEEP", &snapshot, &Accept).await.unwrap();
            assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), format!("{prefix}{suffix}"));
            assert!(!std::fs::read_to_string(&files.config).unwrap().contains("  DEEP:"));
        }
    }

    #[tokio::test]
    async fn changed_files_refuse_save_rename_and_delete() {
        for change_config in [false, true] {
            let (_dir, files) = setup("## Deep Mode\nWhen the active model role is `DEEP`, act.\n");
            let (mut workflow, snapshot) = deep(&files);
            let path = if change_config { &files.config } else { &files.rules };
            let changed = format!("{}# external\n", std::fs::read_to_string(path).unwrap());
            std::fs::write(path, &changed).unwrap();
            assert!(matches!(files.save(&workflow, Some("DEEP"), &snapshot, &Accept).await, Err(Error::ChangedOnDisk { .. })));
            workflow.role = "STUDY".into();
            assert!(matches!(files.save(&workflow, Some("DEEP"), &snapshot, &Accept).await, Err(Error::ChangedOnDisk { .. })));
            assert!(matches!(files.delete("DEEP", &snapshot, &Accept).await, Err(Error::ChangedOnDisk { .. })));
            assert_eq!(std::fs::read_to_string(path).unwrap(), changed);
        }
    }

    #[tokio::test]
    async fn managed_create_update_rename_delete_roundtrip() {
        let original = "# Rules\n\nUser text.\n\n";
        let (_dir, files) = setup(original);
        let (_, snapshot) = files.load().unwrap();
        files.save(&managed("RESEARCH"), None, &snapshot, &Accept).await.unwrap();
        let (list, snapshot) = files.load().unwrap();
        let mut item = list.into_iter().find(|w| w.role == "RESEARCH").unwrap();
        assert_eq!(item.models, vec![Selector::model("vendor/default")]);
        if let Rules::Managed { notes, .. } = &mut item.rules { *notes = "Changed".into(); }
        files.save(&item, Some("RESEARCH"), &snapshot, &Accept).await.unwrap();
        let (_, snapshot) = files.load().unwrap();
        item.role = "STUDY".into();
        files.save(&item, Some("RESEARCH"), &snapshot, &Accept).await.unwrap();
        let (_, snapshot) = files.load().unwrap();
        files.delete("STUDY", &snapshot, &Accept).await.unwrap();
        assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), original);
        assert_eq!(std::fs::read_to_string(&files.config).unwrap(), CONFIG);
    }

    #[tokio::test]
    async fn unsafe_markers_collisions_and_flow_maps_are_refused() {
        for rules in ["<!-- omp-gui:workflow X -->\n", "<!-- /omp-gui:workflow -->\n", "<!-- omp-gui:workflow -->\n"] {
            let (_dir, files) = setup(rules);
            assert!(files.load().is_err());
        }
        let (_dir, files) = setup("");
        let (_, snapshot) = files.load().unwrap();
        for role in ["default", "Default", "deep", "OTHER"] {
            assert!(files.save(&managed(role), None, &snapshot, &Accept).await.is_err());
        }
        std::fs::write(&files.config, "modelRoles: {default: vendor/default}\n").unwrap();
        let (_, snapshot) = files.load().unwrap();
        assert!(files.save(&managed("NEW"), None, &snapshot, &Accept).await.is_err());
        assert_eq!(std::fs::read_to_string(&files.config).unwrap(), "modelRoles: {default: vendor/default}\n");
    }
    struct Reject;
    #[async_trait::async_trait]
    impl SettingsValidator for Reject {
        async fn check_config(&self, _: &str, _: &[(String, serde_json::Value)]) -> Result<()> {
            Err(Error::Rejected { what: "config.yml".into(), message: "rejected".into() })
        }
        async fn check_models(&self, _: &str) -> Result<()> { Ok(()) }
    }

    #[tokio::test]
    async fn rejected_validation_does_not_write_files() {
        let (_dir, files) = setup("# Rules\n");
        let (_, snapshot) = files.load().unwrap();
        assert!(files.save(&managed("NEW"), None, &snapshot, &Reject).await.is_err());
        assert_eq!(std::fs::read_to_string(&files.config).unwrap(), CONFIG);
        assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), "# Rules\n");
    }

    #[tokio::test]
    async fn creation_requires_default_and_rejects_invalid_rules() {
        let (_dir, files) = setup("# Rules\n");
        std::fs::write(&files.config, "modelRoles:\n  OTHER: vendor/model\n").unwrap();
        let (_, snapshot) = files.load().unwrap();
        assert!(files.save(&managed("NEW"), None, &snapshot, &Accept).await.is_err());
        std::fs::write(&files.config, CONFIG).unwrap();
        let (_, snapshot) = files.load().unwrap();
        let mut item = managed("NEW");
        if let Rules::Managed { notes, .. } = &mut item.rules { *notes = "<!-- /omp-gui:workflow -->".into(); }
        assert!(files.save(&item, None, &snapshot, &Accept).await.is_err());
        assert_eq!(std::fs::read_to_string(&files.config).unwrap(), CONFIG);
        assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), "# Rules\n");
    }

    #[test]
    fn marker_mismatches_and_duplicates_are_refused() {
        let valid = render("NEW", &managed("NEW").rules, false).unwrap();
        for rules in [
            valid.replace("<!-- omp-gui:workflow NEW", "<!-- omp-gui:workflow WRONG"),
            format!("{valid}\n{valid}\n"),
            valid.replace(END, "<!-- /omp-gui:workflow bad -->"),
            format!("<!-- omp-gui:workflow OUTER -->\n{valid}\n{END}"),
        ] {
            assert!(parse(&rules).is_err());
        }
    }

    #[test]
    fn role_name_derivation() {
        assert_eq!(role_for("Deep Research").unwrap(), "DEEP_RESEARCH");
        assert_eq!(role_for("  my  weird!!name-1 ").unwrap(), "MY_WEIRD_NAME-1");
        assert!(role_for("!!!").is_err());
    }

    #[tokio::test]
    async fn role_without_section_is_editable_and_deletable() {
        let (_dir, files) = setup("# Rules\n");
        let (mut item, snapshot) = deep(&files);
        assert_eq!(item.rules, Rules::None);
        item.models = vec![Selector::model("vendor/changed")];
        files.save(&item, Some("DEEP"), &snapshot, &Accept).await.unwrap();
        let (_, snapshot) = files.load().unwrap();
        files.delete("DEEP", &snapshot, &Accept).await.unwrap();
        assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), "# Rules\n");
    }
    #[tokio::test]
    async fn failed_rules_write_restores_config_only_if_unchanged() {
        for change_config in [false, true] {
            let (_dir, files) = setup("# Rules\n");
            let (_, snapshot) = files.load().unwrap();
            let config = files.config.clone();
            let rules = files.rules.clone();
            BETWEEN_WRITES.with(|h| *h.borrow_mut() = Some(Box::new(move || {
                if change_config { std::fs::write(&config, "someone: else\n").unwrap(); }
                std::fs::write(&rules, "external change\n").unwrap();
            })));
            assert!(files.save(&managed("NEW"), None, &snapshot, &Accept).await.is_err());
            let expected = if change_config { "someone: else\n" } else { CONFIG };
            assert_eq!(std::fs::read_to_string(&files.config).unwrap(), expected);
            assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), "external change\n");
        }
    }
    #[tokio::test]
    async fn managed_crlf_and_user_blank_lines_survive_roundtrip() {
        let original = "# Rules\r\n\r\n\r\nkeep\r\n\r\n\r\n";
        let (_dir, files) = setup(original);
        let (_, snapshot) = files.load().unwrap();
        files.save(&managed("NEW"), None, &snapshot, &Accept).await.unwrap();
        let text = std::fs::read_to_string(&files.rules).unwrap();
        assert!(text.starts_with(original));
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());
        let (_, snapshot) = files.load().unwrap();
        files.delete("NEW", &snapshot, &Accept).await.unwrap();
        assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), original);
    }

    #[tokio::test]
    async fn deleting_middle_managed_section_preserves_neighbours() {
        let (_dir, files) = setup("# Rules\n");
        let (_, snapshot) = files.load().unwrap();
        files.save(&managed("FIRST"), None, &snapshot, &Accept).await.unwrap();
        let (_, snapshot) = files.load().unwrap();
        files.save(&managed("SECOND"), None, &snapshot, &Accept).await.unwrap();
        let (_, snapshot) = files.load().unwrap();
        files.delete("FIRST", &snapshot, &Accept).await.unwrap();
        let after = std::fs::read_to_string(&files.rules).unwrap();
        assert!(after.starts_with("# Rules\n"));
        assert!(!after.contains("workflow FIRST"));
        assert!(after.contains("workflow SECOND"));
    }

    #[tokio::test]
    async fn unrelated_section_cannot_be_converted_to_managed() {
        let text = "## Deep Mode\nWhen the active model role is `DEEP`, act.\n";
        let (_dir, files) = setup(text);
        let (mut item, snapshot) = deep(&files);
        item.rules = managed("DEEP").rules;
        assert!(files.save(&item, Some("DEEP"), &snapshot, &Accept).await.is_err());
        assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), text);
    }

    #[tokio::test]
    async fn raised_heading_cannot_swallow_the_following_sections() {
        let original = "## Deep Mode\nWhen the active model role is `DEEP`, act.\n\n## Other Mode\nWhen the active model role is `OTHER`, other.\n";
        let (_dir, files) = setup(original);
        let (mut item, snapshot) = deep(&files);
        item.rules = Rules::HandWritten { text: "# Deep Mode\nWhen the active model role is `DEEP`, act.\n\n".into() };
        assert!(files.save(&item, Some("DEEP"), &snapshot, &Accept).await.is_err());
        assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), original);
    }

    #[tokio::test]
    async fn edited_section_must_keep_its_exact_span_in_the_whole_file() {
        let original = "## Deep Mode\nWhen the active model role is `DEEP`, act.\n\n## Notes\nUser notes.\n";
        let (_dir, files) = setup(original);
        let (mut item, snapshot) = deep(&files);
        item.rules = Rules::HandWritten { text: "# Deep Mode\nWhen the active model role is `DEEP`, act.\n\n".into() };
        assert!(files.save(&item, Some("DEEP"), &snapshot, &Accept).await.is_err());
        assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), original);
    }

    #[tokio::test]
    async fn dropped_trailing_newline_keeps_the_following_section() {
        for eol in ["\n", "\r\n"] {
            let other = format!("## Other Mode{eol}When the active model role is `OTHER`, other.{eol}");
            let (_dir, files) = setup(&format!("## Deep Mode{eol}When the active model role is `DEEP`, act.{eol}{eol}{other}"));
            let (mut item, snapshot) = deep(&files);
            item.rules = Rules::HandWritten { text: format!("## Deep Mode{eol}When the active model role is `DEEP`, changed.") };
            files.save(&item, Some("DEEP"), &snapshot, &Accept).await.unwrap();
            assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), format!("## Deep Mode{eol}When the active model role is `DEEP`, changed.{eol}{other}"));
            let (list, _) = files.load().unwrap();
            assert_eq!(list.into_iter().find(|w| w.role == "OTHER").unwrap().rules, Rules::HandWritten { text: other });
        }
    }

    #[tokio::test]
    async fn edited_section_takes_the_files_line_endings() {
        let (_dir, files) = setup("# Rules\r\n\r\n## Deep Mode\r\nWhen the active model role is `DEEP`, act.\r\n");
        let (mut item, snapshot) = deep(&files);
        item.rules = Rules::HandWritten { text: "## Deep Mode\r\nWhen the active model role is `DEEP`, act.\nTyped line.\n".into() };
        files.save(&item, Some("DEEP"), &snapshot, &Accept).await.unwrap();
        assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), "# Rules\r\n\r\n## Deep Mode\r\nWhen the active model role is `DEEP`, act.\r\nTyped line.\r\n");
    }

    #[tokio::test]
    async fn models_only_edit_does_not_create_rules_file() {
        let (_dir, files) = setup("");
        std::fs::remove_file(&files.rules).unwrap();
        let (mut item, snapshot) = deep(&files);
        item.models = vec![Selector::model("vendor/changed")];
        files.save(&item, Some("DEEP"), &snapshot, &Accept).await.unwrap();
        assert!(!files.rules.exists());
        assert!(std::fs::read_to_string(&files.config).unwrap().contains("vendor/changed"));
    }

    #[tokio::test]
    async fn creation_uses_the_forms_models_when_given() {
        let (_dir, files) = setup("");
        let (_, snapshot) = files.load().unwrap();
        let mut item = managed("NEW");
        item.models = vec![Selector::model("vendor/other")];
        files.save(&item, None, &snapshot, &Accept).await.unwrap();
        let (list, _) = files.load().unwrap();
        assert_eq!(list.into_iter().find(|w| w.role == "NEW").unwrap().models, vec![Selector::model("vendor/other")]);
    }
    #[tokio::test]
    async fn structured_fields_round_trip_without_touching_neighbors() {
        for eol in ["\n", "\r\n"] {
            let prefix = format!("# Rules{eol}{eol}Keep this.{eol}{eol}");
            let suffix = format!("{eol}{eol}## Other Mode{eol}When the active model role is `OTHER`, act.{eol}");
            let legacy = render("DEEP", &managed("DEEP").rules, eol == "\r\n").unwrap();
            let (_dir, files) = setup(&format!("{prefix}{legacy}{suffix}"));
            let (mut item, snapshot) = deep(&files);
            assert_eq!(item.rules, managed("DEEP").rules);
            if let Rules::Managed { fields, .. } = &mut item.rules {
                *fields = ManagedFields {
                    description: "Use for research.\n\nMore detail.".into(),
                    routing: vec![RoutingNote { agent: "scout".into(), note: "Gather references.".into() }],
                    steps: vec!["Read sources.".into(), "Summarize findings.".into()],
                    blocks: vec![RuleBlock { heading: "Guidance".into(), text: "First paragraph.\n\n#### Detail\nKeep context.\n".into() }],
                };
            } else { panic!("legacy section was not managed"); }
            files.save(&item, Some("DEEP"), &snapshot, &Accept).await.unwrap();
            let expected = format!("{}\n{FIELD}\n### When to use\n\nUse for research.\n\nMore detail.\n\n{FIELD}\n### Routing\n\n- `scout`: Gather references.\n\n{FIELD}\n### Steps\n\n1. Read sources.\n2. Summarize findings.\n\n{FIELD}\n### Guidance\n\nFirst paragraph.\n\n#### Detail\nKeep context.\n\n{END}", legacy.replace("\r\n", "\n").strip_suffix(END).unwrap()).replace('\n', eol);
            let actual = std::fs::read_to_string(&files.rules).unwrap();
            assert_eq!(actual, format!("{prefix}{expected}{suffix}"));
            let (loaded, snapshot) = deep(&files);
            assert_eq!(loaded.rules, item.rules);
            files.save(&loaded, Some("DEEP"), &snapshot, &Accept).await.unwrap();
            assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), actual);
        }
    }

    #[tokio::test]
    async fn external_body_is_lossless_editable_and_cannot_be_renamed() {
        for eol in ["\n", "\r\n"] {
            let body = format!("Custom preamble{eol}{eol}When the active model role is `DEEP`, use custom instructions.{eol}Text changed outside.  {eol}{eol}");
            let original = format!("before{eol}  {START}DEEP -->{eol}{body}  {END}{eol}after{eol}");
            let (_dir, files) = setup(&original);
            let (mut item, snapshot) = deep(&files);
            assert_eq!(item.rules, Rules::External { text: body.clone() });
            files.save(&item, Some("DEEP"), &snapshot, &Accept).await.unwrap();
            assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), original);
            let (_, snapshot) = deep(&files);
            item.role = "STUDY".into();
            assert!(files.save(&item, Some("DEEP"), &snapshot, &Accept).await.is_err());
            item.role = "DEEP".into();
            let changed = body.replace("outside", "inside");
            item.rules = Rules::External { text: changed.clone() };
            files.save(&item, Some("DEEP"), &snapshot, &Accept).await.unwrap();
            assert_eq!(std::fs::read_to_string(&files.rules).unwrap(), original.replace(&body, &changed));
            assert_eq!(deep(&files).0.rules, Rules::External { text: changed });
        }
    }

    #[test]
    fn unsupported_managed_format_is_external_and_marker_conflicts_fail() {
        let canonical = render("DEEP", &managed("DEEP").rules, false).unwrap();
        for altered in [
            canonical.replace("Route work only to these agents.", "Use discretion."),
            canonical.replace("## Research Mode", "## Changed header"),
            canonical.replace("When the active model role is `DEEP`,", "This role"),
        ] {
            let block = parse(&altered).unwrap().blocks.remove(0);
            assert!(matches!(block.rules, Rules::External { .. }));
        }
        assert!(parse(&canonical.replace("role is `DEEP`", "role is `OTHER`")).is_err());
        assert!(parse(&canonical.replace(END, "<!-- /omp-gui:workflow broken -->")).is_err());
    }

    #[test]
    fn duplicate_agent_routing_opens_as_raw_markdown() {
        let mut item = managed("DEEP");
        let Rules::Managed { fields, .. } = &mut item.rules else { unreachable!() };
        let note = RoutingNote { agent: "scout".into(), note: "Look up references.".into() };
        fields.routing = vec![note.clone()];
        let canonical = render("DEEP", &item.rules, false).unwrap();
        let edited = canonical.replace(END, &format!("- `scout`: Read the glossary.\n{END}"));
        let expected = edited.split_once('\n').unwrap().1.strip_suffix(END).unwrap();
        assert_eq!(parse(&edited).unwrap().blocks.remove(0).rules, Rules::External { text: expected.into() });
        let Rules::Managed { fields, .. } = &mut item.rules else { unreachable!() };
        fields.routing.push(note);
        assert!(render("DEEP", &item.rules, false).is_err());
    }

    #[tokio::test]
    async fn notes_keep_whitespace_and_literal_field_markers() {
        for notes in ["  \nKeep this indentation.".to_owned(), format!("{ALLOWED}this is notes\n{FIELD}\n{NOTES_FIELD}\nKeep this.")] {
            let (_dir, files) = setup("");
            let (_, snapshot) = files.load().unwrap();
            let mut item = managed("NEW");
            let Rules::Managed { notes: draft, fields, .. } = &mut item.rules else { unreachable!() };
            *draft = notes;
            fields.description = "Read the glossary.".into();
            files.save(&item, None, &snapshot, &Accept).await.unwrap();
            let (list, _) = files.load().unwrap();
            assert_eq!(list.iter().find(|w| w.role == "NEW").unwrap().rules, item.rules);
        }
    }

    #[test]
    fn reserved_field_markers_and_headings_are_rejected() {
        let mut item = managed("DEEP");
        let Rules::Managed { fields, .. } = &mut item.rules else { unreachable!() };
        fields.blocks.push(RuleBlock { heading: "Routing".into(), text: "Text".into() });
        assert!(render("DEEP", &item.rules, false).is_err());
        let Rules::Managed { fields, .. } = &mut item.rules else { unreachable!() };
        fields.blocks[0].heading = "Notes".into();
        fields.blocks[0].text = format!("Before\n{FIELD}\nAfter");
        assert!(render("DEEP", &item.rules, false).is_err());
    }
}
