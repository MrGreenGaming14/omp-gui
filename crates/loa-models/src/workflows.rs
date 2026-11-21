//! Custom workflows: named modes that tell the manager model which agents to use.
//!
//! A workflow is a role in omp's `config.yml` (`modelRoles.<ROLE>`) plus a section
//! in `RULES.md` that says when that role is active and which agents it may route
//! to. The app owns only the sections it wrote, fenced by marker comments; every
//! other byte of `RULES.md` and `config.yml` is left exactly as it was. Hand-written
//! `## <Name> Mode` sections that name their role are listed read-only.

use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::config::{OmpConfig, validate_role_name};
use crate::fsutil::{Snapshot, read_optional, write_atomic};
use crate::validate::SettingsValidator;
use crate::{Error, Result};

const START: &str = "<!-- omp-gui:workflow ";
const END: &str = "<!-- /omp-gui:workflow -->";
const WHEN: &str = "When the active model role is `";
const ALLOWED: &str = "- Allowed agents: ";
const DISALLOWED: &str = "- Disallowed agents: ";

/// One workflow: a role, its display name, agent routing and free-form notes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workflow {
    pub role: String,
    pub name: String,
    pub allowed: Vec<String>,
    pub disallowed: Vec<String>,
    pub notes: String,
    /// True when the app wrote the section and may edit it; false for a
    /// hand-written mode.
    pub managed: bool,
}

/// Derives a role name from a display name: `Deep Research` becomes `DEEP_RESEARCH`.
pub fn role_for(name: &str) -> Result<String> {
    let mut role = String::new();
    let mut gap = false;
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            if gap && !role.is_empty() {
                role.push('_');
            }
            gap = false;
            role.push(c.to_ascii_uppercase());
        } else {
            gap = true;
        }
    }
    let role = role.trim_matches('_').to_owned();
    validate_role_name(&role).map_err(|_| {
        Error::invalid("workflow name", format!("`{name}` has no letters or digits to build a role from"))
    })?;
    Ok(role)
}

/// The two files a workflow lives in.
#[derive(Clone, Debug)]
pub struct WorkflowFiles {
    pub config: PathBuf,
    pub rules: PathBuf,
}


struct Block {
    span: Range<usize>,
    workflow: Workflow,
}

struct Parsed {
    crlf: bool,
    blocks: Vec<Block>,
    handwritten: Vec<Workflow>,
}

const START_PREFIX: &str = "<!-- omp-gui:workflow";
const END_PREFIX: &str = "<!-- /omp-gui:workflow";

fn malformed(reason: impl Into<String>) -> Error {
    Error::invalid("RULES.md", reason)
}

/// Parses every managed block strictly. Any malformed marker structure is an
/// error, so nothing is ever written next to a block the app cannot account for.
fn parse(text: &str) -> Result<Parsed> {
    let crlf = text.find('\n').is_some_and(|i| text[..i].ends_with('\r'));
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0;
    for raw in text.split_inclusive('\n') {
        lines.push((offset, raw.trim_end_matches(['\n', '\r'])));
        offset += raw.len();
    }
    let mut blocks: Vec<Block> = Vec::new();
    let mut handwritten = Vec::new();
    // (name, role) of the hand-written section being scanned.
    let mut section: Option<(String, Option<String>)> = None;
    let mut finish = |section: &mut Option<(String, Option<String>)>| {
        if let Some((name, Some(role))) = section.take() {
            handwritten.push(Workflow { role, name, allowed: vec![], disallowed: vec![], notes: String::new(), managed: false });
        }
        *section = None;
    };
    let mut i = 0;
    while i < lines.len() {
        let (start, line) = lines[i];
        let trimmed = line.trim();
        if trimmed.starts_with(END_PREFIX) {
            return Err(malformed(format!("line {} closes a workflow block that was never opened", i + 1)));
        }
        if trimmed.starts_with(START_PREFIX) {
            let role = trimmed
                .strip_prefix(START)
                .and_then(|r| r.strip_suffix("-->"))
                .map(str::trim)
                .filter(|r| !r.is_empty() && !r.contains(char::is_whitespace))
                .ok_or_else(|| malformed(format!("line {} has a malformed workflow start marker", i + 1)))?;
            let mut close = None;
            for (j, l) in lines.iter().enumerate().skip(i + 1) {
                let t = l.1.trim();
                if t.starts_with(START_PREFIX) {
                    return Err(malformed(format!("the workflow block for `{role}` (line {}) is not closed before another starts (line {})", i + 1, j + 1)));
                }
                if t.starts_with(END_PREFIX) {
                    if t != END {
                        return Err(malformed(format!("line {} has a malformed workflow end marker", j + 1)));
                    }
                    close = Some(j);
                    break;
                }
            }
            let close = close.ok_or_else(|| malformed(format!("the workflow block for `{role}` (line {}) has no end marker", i + 1)))?;
            finish(&mut section);
            let body: Vec<&str> = lines[i + 1..close].iter().map(|l| l.1).collect();
            let (close_start, close_line) = lines[close];
            blocks.push(Block {
                span: start..close_start + close_line.len(),
                workflow: parse_block(role, &body)?,
            });
            i = close + 1;
            continue;
        }
        if line.starts_with("## ") || line.starts_with("# ") {
            finish(&mut section);
            if let Some(name) = line.strip_prefix("## ").and_then(|h| h.trim().strip_suffix(" Mode")) {
                section = Some((name.trim().to_owned(), None));
            }
        } else if let Some((_, role @ None)) = section.as_mut()
            && let Some(at) = line.find(WHEN)
            && let Some(len) = line[at + WHEN.len()..].find('`')
        {
            *role = Some(line[at + WHEN.len()..at + WHEN.len() + len].to_owned());
        }
        i += 1;
    }
    finish(&mut section);
    let mut seen: Vec<&str> = Vec::new();
    for role in blocks.iter().map(|b| &b.workflow).chain(&handwritten).map(|w| w.role.as_str()) {
        if seen.contains(&role) {
            return Err(malformed(format!("the role `{role}` is defined more than once")));
        }
        seen.push(role);
    }
    Ok(Parsed { crlf, blocks, handwritten })
}

fn parse_block(role: &str, body: &[&str]) -> Result<Workflow> {
    let when = body
        .iter()
        .position(|l| l.starts_with(WHEN))
        .ok_or_else(|| malformed(format!("the workflow block for `{role}` has no `{WHEN}...` line")))?;
    let line = body[when];
    let named = line[WHEN.len()..].find('`').map(|len| &line[WHEN.len()..WHEN.len() + len]);
    if named != Some(role) {
        return Err(malformed(format!(
            "the workflow block marked `{role}` says its role is `{}`",
            named.unwrap_or("")
        )));
    }
    let name = body
        .iter()
        .find_map(|l| l.strip_prefix("## "))
        .and_then(|h| h.trim().strip_suffix(" Mode"))
        .unwrap_or(role)
        .trim()
        .to_owned();
    let mut i = when + 1;
    let (mut allowed, mut disallowed) = (vec![], vec![]);
    loop {
        while i < body.len() && body[i].trim().is_empty() {
            i += 1;
        }
        match body.get(i) {
            Some(l) if l.starts_with(ALLOWED) => allowed = backticked(l),
            Some(l) if l.starts_with(DISALLOWED) => disallowed = backticked(l),
            _ => break,
        }
        i += 1;
    }
    let notes = body[i.min(body.len())..].join("\n").trim_matches('\n').to_owned();
    Ok(Workflow { role: role.to_owned(), name, allowed, disallowed, notes, managed: true })
}



/// The backticked names before the first `. ` sentence break.
fn backticked(line: &str) -> Vec<String> {
    let list = line.split_once(". ").map_or(line, |(a, _)| a);
    list.split('`').skip(1).step_by(2).map(str::to_owned).collect()
}

fn render(w: &Workflow) -> String {
    let mut out = format!("{START}{} -->\n## {} Mode\n\n{WHEN}{}`, operate in {} Mode.\n", w.role, w.name, w.role, w.name);
    for (label, rule, agents) in [(ALLOWED, "Route work only to these agents.", &w.allowed), (DISALLOWED, "Never route work to these agents.", &w.disallowed)] {
        if !agents.is_empty() {
            let list = agents.iter().map(|a| format!("`{a}`")).collect::<Vec<_>>().join(", ");
            if out.ends_with("Mode.\n") {
                out.push('\n');
            }
            out.push_str(&format!("{label}{list}. {rule}\n"));
        }
    }
    let notes = w.notes.replace("\r\n", "\n");
    let notes = notes.trim_matches('\n');
    if !notes.is_empty() {
        out.push('\n');
        out.push_str(notes);
        out.push('\n');
    }
    out.push_str(END);
    out
}

fn check(w: &Workflow) -> Result<()> {
    validate_role_name(&w.role)?;
    let name = w.name.trim();
    if name.is_empty() || name != w.name || name.contains(['\n', '\r', '`']) {
        return Err(Error::invalid("workflow name", "must be one line of text without backticks"));
    }
    for agent in w.allowed.iter().chain(&w.disallowed) {
        if agent.trim().is_empty() || agent.contains(['\n', '\r', '`']) {
            return Err(Error::invalid("agents", format!("`{agent}` is not a valid agent name")));
        }
    }
    if w.notes.contains("omp-gui:workflow") {
        return Err(Error::invalid("notes", "must not contain the workflow marker text"));
    }
    let first = w.notes.trim_matches(['\n', '\r']);
    if first.starts_with(ALLOWED) || first.starts_with(DISALLOWED) {
        return Err(Error::invalid("notes", "must not begin like the agent list lines"));
    }
    Ok(())
}

fn with_eol(text: &str, crlf: bool) -> String {
    if crlf { text.replace('\n', "\r\n") } else { text.to_owned() }
}

impl WorkflowFiles {
    /// The files inside omp's agent directory.
    pub fn in_agent_dir(dir: &Path) -> Self {
        Self { config: dir.join("config.yml"), rules: dir.join("RULES.md") }
    }

    /// Managed workflows in file order, then hand-written modes. Missing files
    /// are empty. A `RULES.md` with malformed workflow markers is an error.
    pub fn list(&self) -> Result<Vec<Workflow>> {
        let text = read_optional(&self.rules)?.unwrap_or_default();
        let parsed = parse(&text)?;
        Ok(parsed.blocks.into_iter().map(|b| b.workflow).chain(parsed.handwritten).collect())
    }

    /// Creates (`previous_role` None), updates or renames a managed workflow.
    /// Writes `config.yml` first, then `RULES.md`, restoring `config.yml` if the
    /// second write fails and nobody else changed it meanwhile.
    pub async fn save(&self, workflow: &Workflow, previous_role: Option<&str>, validator: &dyn SettingsValidator) -> Result<()> {
        check(workflow)?;
        let (mut snapshot, text) = Snapshot::load(&self.rules)?;
        let text = text.unwrap_or_default();
        let parsed = parse(&text)?;
        let mut config = OmpConfig::load(&self.config)?;
        let role = workflow.role.as_str();
        if role.eq_ignore_ascii_case("default") {
            return Err(Error::invalid("role", "`default` is reserved for omp's default model"));
        }

        let previous = match previous_role {
            Some(p) => {
                if parsed.handwritten.iter().any(|h| h.role == p) {
                    return Err(Error::invalid("role", format!("`{p}` is a hand-written mode; edit RULES.md instead")));
                }
                let found = parsed.blocks.iter().position(|b| b.workflow.role == p);
                Some(found.ok_or_else(|| Error::invalid("role", format!("no workflow with role `{p}`")))?)
            }
            None => None,
        };
        if previous_role != Some(role) {
            let config_roles = config.model_roles()?;
            let taken = parsed
                .blocks
                .iter()
                .map(|b| &b.workflow)
                .chain(&parsed.handwritten)
                .map(|w| w.role.as_str())
                .chain(config_roles.iter().map(|r| r.role.as_str()))
                .any(|r| Some(r) != previous_role && r.eq_ignore_ascii_case(role));
            if taken {
                return Err(Error::invalid("role", format!("`{role}` already exists (role names are compared ignoring case)")));
            }
            let source = match previous_role.map(|p| config.model_role(p)).transpose()?.flatten() {
                Some(own) => own,
                None => config.model_role("default")?.ok_or_else(|| {
                    Error::invalid("modelRoles", "there is no `default` role to copy models from; set a default model first")
                })?,
            };
            config.set_model_role(role, &source.candidates)?;
            if let Some(p) = previous_role {
                config.clear_model_role(p)?;
            }
        }

        let block = with_eol(&render(workflow), parsed.crlf);
        let eol = if parsed.crlf { "\r\n" } else { "\n" };
        let new_rules = match previous {
            Some(i) => {
                let span = &parsed.blocks[i].span;
                format!("{}{block}{}", &text[..span.start], &text[span.end..])
            }
            None => {
                let mut out = text.clone();
                let separate = !out.is_empty();
                if separate && !out.ends_with('\n') {
                    out.push_str(eol);
                }
                if separate {
                    out.push_str(eol);
                }
                format!("{out}{block}{eol}")
            }
        };
        self.commit(&mut config, &mut snapshot, &new_rules, validator).await
    }

    /// Removes a managed workflow's section and its role assignment.
    pub async fn delete(&self, role: &str, validator: &dyn SettingsValidator) -> Result<()> {
        let (mut snapshot, text) = Snapshot::load(&self.rules)?;
        let text = text.unwrap_or_default();
        let parsed = parse(&text)?;
        if parsed.handwritten.iter().any(|h| h.role == role) {
            return Err(Error::invalid("role", format!("`{role}` is a hand-written mode; edit RULES.md instead")));
        }
        let block = parsed
            .blocks
            .iter()
            .find(|b| b.workflow.role == role)
            .ok_or_else(|| Error::invalid("role", format!("no workflow with role `{role}`")))?;
        let mut config = OmpConfig::load(&self.config)?;
        config.clear_model_role(role)?;

        // The span, the newline the app wrote after it, and the one separator
        // newline the app wrote before it (only when a blank line precedes the
        // block). Every other byte, including user blank lines, stays.
        let (mut start, mut end) = (block.span.start, block.span.end);
        if text[end..].starts_with("\r\n") {
            end += 2;
        } else if text[end..].starts_with('\n') {
            end += 1;
        }
        let before = &text[..start];
        let eol_len = if before.ends_with("\r\n") { 2 } else { usize::from(before.ends_with('\n')) };
        if eol_len > 0 && before[..before.len() - eol_len].ends_with('\n') {
            start -= eol_len;
        }
        let new_rules = format!("{}{}", &text[..start], &text[end..]);
        self.commit(&mut config, &mut snapshot, &new_rules, validator).await
    }

    async fn commit(&self, config: &mut OmpConfig, rules: &mut Snapshot, new_rules: &str, validator: &dyn SettingsValidator) -> Result<()> {
        let previous = read_optional(&self.config)?;
        let wrote_config = config.is_edited();
        if wrote_config {
            config.save_validated(validator).await?;
        }
        #[cfg(test)]
        if let Some(hook) = tests::BETWEEN_WRITES.with(|h| h.borrow_mut().take()) {
            hook();
        }
        let Err(err) = rules.save(new_rules) else { return Ok(()) };
        if !wrote_config {
            return Err(err);
        }
        let current = read_optional(&self.config);
        match current {
            Ok(current) if current.as_deref() == Some(config.text()) => {
                let restored = match previous {
                    Some(bytes) => write_atomic(&self.config, &bytes),
                    None => std::fs::remove_file(&self.config).map_err(|e| Error::io(&self.config, e)),
                };
                match restored {
                    Ok(()) => Err(err),
                    Err(restore) => Err(Error::invalid(
                        "workflow",
                        format!("writing RULES.md failed ({err}) and restoring config.yml failed ({restore}); config.yml still holds the new role"),
                    )),
                }
            }
            Ok(_) => Err(Error::invalid(
                "workflow",
                format!("writing RULES.md failed ({err}); config.yml was changed by something else after this edit wrote it, so it was left as is and may still hold the new role"),
            )),
            Err(read) => Err(Error::invalid(
                "workflow",
                format!("writing RULES.md failed ({err}) and config.yml could not be checked for restoring ({read})"),
            )),
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::cell::RefCell;

    thread_local! {
        /// Runs once between the `config.yml` write and the `RULES.md` write.
        pub(super) static BETWEEN_WRITES: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    }

    fn between_writes(hook: impl FnOnce() + 'static) {
        BETWEEN_WRITES.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    }

    struct Reject;
    #[async_trait]
    impl SettingsValidator for Reject {
        async fn check_config(&self, _: &str, _: &[(String, serde_json::Value)]) -> Result<()> {
            Err(Error::Rejected { what: "config.yml".into(), message: "no".into() })
        }
        async fn check_models(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    struct Accept;
    #[async_trait]
    impl SettingsValidator for Accept {
        async fn check_config(&self, _: &str, _: &[(String, serde_json::Value)]) -> Result<()> {
            Ok(())
        }
        async fn check_models(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    const CONFIG: &str = "# my settings\ntheme:\n  dark: titanium # keep\n\nmodelRoles:\n  default: anthropic/claude-sonnet-4-5 # day to day\n  DEEP: openai/gpt-5\n  CYBER: openai/gpt-5-codex\n  CODE: anthropic/claude-opus-4-5:high\n\nretry:\n  maxRetries: 5\n";
    const RULES: &str = "# Rules\n\nBe kind.\n\n## Normal Mode\n\nWhen the active model role is `DEEP`, operate in Normal Mode.\n\n- keep it short\n\n## Deep Mode\n\nWhen the active model role is `CYBER`, go deep.\n\n### Routing\n\nUse `a`.\n\n## Other\n\nUnrelated text.\n";

    fn setup(config: &str, rules: &str) -> (tempfile::TempDir, WorkflowFiles) {
        let dir = tempfile::tempdir().unwrap();
        let files = WorkflowFiles::in_agent_dir(dir.path());
        std::fs::write(&files.config, config).unwrap();
        std::fs::write(&files.rules, rules).unwrap();
        (dir, files)
    }

    fn research() -> Workflow {
        Workflow {
            role: "DEEP_RESEARCH".into(),
            name: "Deep Research".into(),
            allowed: vec!["code-scout".into(), "claude-critic".into()],
            disallowed: vec!["cyber-red".into()],
            notes: "Cite sources.\n\nPrefer primary ones.".into(),
            managed: true,
        }
    }

    fn rules(f: &WorkflowFiles) -> String {
        std::fs::read_to_string(&f.rules).unwrap()
    }
    fn config(f: &WorkflowFiles) -> String {
        std::fs::read_to_string(&f.config).unwrap()
    }

    #[test]
    fn role_names_are_derived_and_validated() {
        assert_eq!(role_for("Deep Research").unwrap(), "DEEP_RESEARCH");
        assert_eq!(role_for("  my  weird!!name-1 ").unwrap(), "MY_WEIRD_NAME-1");
        assert!(role_for("   ").is_err());
        assert!(role_for("!!!").is_err());
        assert!(role_for("日本語").is_err());
    }

    #[test]
    fn lists_hand_written_modes_read_only() {
        let (_d, f) = setup(CONFIG, RULES);
        let list = f.list().unwrap();
        let got: Vec<_> = list.iter().map(|w| (w.name.as_str(), w.role.as_str(), w.managed)).collect();
        assert_eq!(got, [("Normal", "DEEP", false), ("Deep", "CYBER", false)]);
        let empty = WorkflowFiles::in_agent_dir(Path::new("does-not-exist-dir"));
        assert!(empty.list().unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_update_rename_delete_round_trip() {
        let (_d, f) = setup(CONFIG, RULES);
        f.save(&research(), None, &Accept).await.unwrap();
        let after_create = rules(&f);
        assert!(after_create.starts_with(RULES));
        assert_eq!(&after_create[RULES.len()..RULES.len() + 1], "\n");
        assert!(after_create.contains("- Allowed agents: `code-scout`, `claude-critic`. Route work only to these agents.\n- Disallowed agents: `cyber-red`. Never route work to these agents.\n\nCite sources."));
        let cfg = config(&f);
        assert!(cfg.contains("DEEP_RESEARCH: anthropic/claude-sonnet-4-5"));
        assert!(cfg.contains("# my settings") && cfg.contains("# day to day") && cfg.contains("# keep"));

        let list = f.list().unwrap();
        assert_eq!(list[0], research());
        assert_eq!(list.len(), 3);

        let mut updated = research();
        updated.allowed = vec![];
        updated.notes = String::new();
        f.save(&updated, Some("DEEP_RESEARCH"), &Accept).await.unwrap();
        let after_update = rules(&f);
        assert!(after_update.starts_with(RULES));
        assert!(!after_update.contains("Allowed agents"));
        assert_eq!(config(&f), cfg);
        assert_eq!(f.list().unwrap()[0], updated);

        let mut renamed = updated.clone();
        renamed.role = "STUDY".into();
        renamed.name = "Study".into();
        f.save(&renamed, Some("DEEP_RESEARCH"), &Accept).await.unwrap();
        assert!(rules(&f).starts_with(RULES));
        assert!(config(&f).contains("STUDY: anthropic/claude-sonnet-4-5"));
        assert!(!config(&f).contains("DEEP_RESEARCH"));
        assert_eq!(f.list().unwrap()[0].name, "Study");

        f.delete("STUDY", &Accept).await.unwrap();
        assert_eq!(rules(&f), RULES);
        assert_eq!(config(&f), CONFIG);
    }

    #[tokio::test]
    async fn middle_block_update_and_delete_leave_neighbours_identical() {
        let (_d, f) = setup(CONFIG, RULES);
        f.save(&research(), None, &Accept).await.unwrap();
        let mut second = research();
        second.role = "TWO".into();
        second.name = "Two".into();
        f.save(&second, None, &Accept).await.unwrap();
        let both = rules(&f);
        f.delete("DEEP_RESEARCH", &Accept).await.unwrap();
        let only_two = rules(&f);
        assert!(only_two.starts_with(RULES) && only_two.contains("## Two Mode") && !only_two.contains("Deep Research"));
        assert_eq!(both.matches(END).count(), 2);
        f.delete("TWO", &Accept).await.unwrap();
        assert_eq!(rules(&f), RULES);
    }

    #[tokio::test]
    async fn crlf_rules_stay_crlf() {
        let crlf = RULES.replace('\n', "\r\n");
        let (_d, f) = setup(CONFIG, &crlf);
        f.save(&research(), None, &Accept).await.unwrap();
        let text = rules(&f);
        assert!(text.starts_with(&crlf));
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());
        assert_eq!(f.list().unwrap()[0], research());
        let mut updated = research();
        updated.notes = "one\ntwo".into();
        f.save(&updated, Some("DEEP_RESEARCH"), &Accept).await.unwrap();
        let text = rules(&f);
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());
        assert!(text.contains("one\r\ntwo\r\n<!-- /omp-gui"));
        f.delete("DEEP_RESEARCH", &Accept).await.unwrap();
        assert_eq!(rules(&f), crlf);
    }

    #[tokio::test]
    async fn refuses_hand_written_and_existing_roles() {
        let (_d, f) = setup(CONFIG, RULES);
        let mut w = research();
        w.role = "DEEP".into();
        assert!(f.save(&w, None, &Accept).await.is_err());
        w.role = "CODE".into(); // exists only in config.yml
        assert!(f.save(&w, None, &Accept).await.is_err());
        assert!(f.save(&research(), Some("CYBER"), &Accept).await.is_err());
        assert!(f.delete("CYBER", &Accept).await.is_err());
        assert!(f.delete("NOPE", &Accept).await.is_err());
        f.save(&research(), None, &Accept).await.unwrap();
        let mut renamed = research();
        renamed.role = "DEEP".into();
        assert!(f.save(&renamed, Some("DEEP_RESEARCH"), &Accept).await.is_err());
        let mut dup = research();
        dup.name = "Again".into();
        assert!(f.save(&dup, None, &Accept).await.is_err());
        assert_eq!(config(&f).matches("DEEP: openai/gpt-5\n").count(), 1);
        assert!(rules(&f).starts_with(RULES));
    }

    #[tokio::test]
    async fn create_needs_a_default_role() {
        let (_d, f) = setup("theme:\n  dark: titanium\n", RULES);
        let err = f.save(&research(), None, &Accept).await.unwrap_err();
        assert!(err.to_string().contains("default"));
        assert_eq!(rules(&f), RULES);
    }

    #[tokio::test]
    async fn invalid_input_is_rejected_before_writing() {
        let (_d, f) = setup(CONFIG, RULES);
        let mut w = research();
        w.name = "Bad\nName".into();
        assert!(f.save(&w, None, &Accept).await.is_err());
        let mut w = research();
        w.notes = "x\n<!-- /omp-gui:workflow -->".into();
        assert!(f.save(&w, None, &Accept).await.is_err());
        let mut w = research();
        w.role = "has space".into();
        assert!(f.save(&w, None, &Accept).await.is_err());
        assert_eq!(config(&f), CONFIG);
        assert_eq!(rules(&f), RULES);
    }

    #[tokio::test]
    async fn rules_file_that_cannot_be_written_leaves_config_untouched() {
        let (_d, f) = setup(CONFIG, RULES);
        std::fs::remove_file(&f.rules).unwrap();
        std::fs::create_dir(&f.rules).unwrap();
        assert!(f.save(&research(), None, &Accept).await.is_err());
        assert_eq!(config(&f), CONFIG);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn config_is_restored_when_the_rules_write_fails() {
        let (_d, f) = setup(CONFIG, RULES);
        let mut perms = std::fs::metadata(&f.rules).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&f.rules, perms).unwrap();
        let result = f.save(&research(), None, &Accept).await;
        let mut perms = std::fs::metadata(&f.rules).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&f.rules, perms).unwrap();
        assert!(result.is_err());
        assert_eq!(config(&f), CONFIG);
        assert_eq!(rules(&f), RULES);
    }

    #[tokio::test]
    async fn creates_files_from_nothing_when_a_default_exists() {
        let dir = tempfile::tempdir().unwrap();
        let f = WorkflowFiles::in_agent_dir(dir.path());
        std::fs::write(&f.config, "modelRoles:\n  default: a/b\n").unwrap();
        f.save(&research(), None, &Accept).await.unwrap();
        assert!(rules(&f).starts_with("<!-- omp-gui:workflow DEEP_RESEARCH -->\n## Deep Research Mode\n\nWhen the active model role is `DEEP_RESEARCH`, operate in Deep Research Mode.\n\n- Allowed"));
        f.delete("DEEP_RESEARCH", &Accept).await.unwrap();
        assert_eq!(rules(&f), "");
    }

    fn block_text(role: &str) -> String {
        render(&Workflow { role: role.into(), ..research() })
    }

    fn assert_untouched(f: &WorkflowFiles, rules_before: &str) {
        assert_eq!(rules(f), rules_before);
        assert_eq!(config(f), CONFIG);
    }

    #[tokio::test]
    async fn malformed_markers_refuse_every_operation_and_write_nothing() {
        let good = block_text("DEEP_RESEARCH");
        let cases = [
            ("missing end", format!("{RULES}\n{}\n", good.replace(END, ""))),
            ("nested start", format!("{RULES}\n<!-- omp-gui:workflow OUTER -->\n{good}\n")),
            ("duplicate roles", format!("{RULES}\n{good}\n\n{good}\n")),
            ("role mismatch", format!("{RULES}\n{}\n", good.replacen("<!-- omp-gui:workflow DEEP_RESEARCH", "<!-- omp-gui:workflow OTHER", 1))),
            ("stray end", format!("{RULES}\n{END}\n")),
            ("malformed start", format!("{RULES}\n<!-- omp-gui:workflow -->\n")),
        ];
        for (label, rules_text) in cases {
            let (_d, f) = setup(CONFIG, &rules_text);
            assert!(f.list().is_err(), "{label}: list");
            let mut fresh = research();
            fresh.role = "FRESH".into();
            assert!(f.save(&fresh, None, &Accept).await.is_err(), "{label}: create");
            assert!(f.save(&research(), Some("DEEP_RESEARCH"), &Accept).await.is_err(), "{label}: update");
            assert!(f.delete("DEEP_RESEARCH", &Accept).await.is_err(), "{label}: delete");
            assert_untouched(&f, &rules_text);
        }
    }

    #[tokio::test]
    async fn user_blank_lines_survive_create_update_and_delete() {
        let base = "# Rules\n\n\n\nkeep\n\n\n";
        let (_d, f) = setup(CONFIG, base);
        f.save(&research(), None, &Accept).await.unwrap();
        let created = rules(&f);
        assert_eq!(created, format!("{base}\n{}\n", block_text("DEEP_RESEARCH")));
        let mut updated = research();
        updated.notes = "changed".into();
        f.save(&updated, Some("DEEP_RESEARCH"), &Accept).await.unwrap();
        let after = rules(&f);
        assert!(after.starts_with(&format!("{base}\n<!--")) && after.ends_with("-->\n"));
        f.delete("DEEP_RESEARCH", &Accept).await.unwrap();
        assert_eq!(rules(&f), base);

        // A user blank line after the block is not the app's.
        let with_tail = format!("{base}\n{}\n\n\ntail\n", block_text("DEEP_RESEARCH"));
        let (_d, f) = setup(CONFIG, &with_tail);
        f.delete("DEEP_RESEARCH", &Accept).await.unwrap();
        assert_eq!(rules(&f), format!("{base}\n\ntail\n"));
    }

    #[tokio::test]
    async fn flow_style_model_roles_are_refused_untouched() {
        let cfg = "# keep me\nmodelRoles: {default: a/b, DEEP: c/d} # flow\n";
        let (_d, f) = setup(cfg, RULES);
        let err = f.save(&research(), None, &Accept).await.unwrap_err();
        assert!(err.to_string().contains("flow style"), "{err}");
        assert_eq!(config(&f), cfg);
        assert_eq!(rules(&f), RULES);
    }

    #[tokio::test]
    async fn rollback_is_skipped_when_config_changed_underneath() {
        let (_d, f) = setup(CONFIG, RULES);
        let (cfg_path, rules_path) = (f.config.clone(), f.rules.clone());
        between_writes(move || {
            std::fs::write(&cfg_path, "someone: else\n").unwrap();
            std::fs::remove_file(&rules_path).unwrap();
            std::fs::create_dir(&rules_path).unwrap();
        });
        let err = f.save(&research(), None, &Accept).await.unwrap_err().to_string();
        assert!(err.contains("RULES.md") && err.contains("config.yml"), "{err}");
        assert_eq!(config(&f), "someone: else\n");
    }

    #[tokio::test]
    async fn config_is_restored_when_rules_fail_after_the_initial_read() {
        let (_d, f) = setup(CONFIG, RULES);
        let rules_path = f.rules.clone();
        between_writes(move || {
            std::fs::remove_file(&rules_path).unwrap();
            std::fs::create_dir(&rules_path).unwrap();
        });
        assert!(f.save(&research(), None, &Accept).await.is_err());
        assert_eq!(config(&f), CONFIG);
    }

    #[tokio::test]
    async fn rename_keeps_the_roles_own_models() {
        let (_d, f) = setup(CONFIG, RULES);
        f.save(&research(), None, &Accept).await.unwrap();
        let text = config(&f).replace("DEEP_RESEARCH: anthropic/claude-sonnet-4-5", "DEEP_RESEARCH: openai/gpt-5-mini:low");
        std::fs::write(&f.config, text).unwrap();
        let mut renamed = research();
        renamed.role = "STUDY".into();
        f.save(&renamed, Some("DEEP_RESEARCH"), &Accept).await.unwrap();
        let cfg = config(&f);
        assert!(cfg.contains("STUDY: openai/gpt-5-mini:low"), "{cfg}");
        assert!(!cfg.contains("DEEP_RESEARCH"));
    }

    #[tokio::test]
    async fn reserved_and_case_colliding_roles_are_rejected() {
        let (_d, f) = setup(CONFIG, RULES);
        for role in ["Default", "default", "deep", "Cyber", "code"] {
            let mut w = research();
            w.role = role.into();
            assert!(f.save(&w, None, &Accept).await.is_err(), "{role}");
        }
        assert_untouched(&f, RULES);
        f.save(&research(), None, &Accept).await.unwrap();
        let mut renamed = research();
        renamed.role = "Deep".into();
        assert!(f.save(&renamed, Some("DEEP_RESEARCH"), &Accept).await.is_err());
        let mut clash = research();
        clash.role = "deep_research".into();
        assert!(f.save(&clash, None, &Accept).await.is_err());
    }

    #[tokio::test]
    async fn rejecting_validator_leaves_both_files_identical() {
        let (_d, f) = setup(CONFIG, RULES);
        assert!(f.save(&research(), None, &Reject).await.is_err());
        assert_untouched(&f, RULES);
        f.save(&research(), None, &Accept).await.unwrap();
        let (cfg, rl) = (config(&f), rules(&f));
        let mut renamed = research();
        renamed.role = "STUDY".into();
        assert!(f.save(&renamed, Some("DEEP_RESEARCH"), &Reject).await.is_err());
        assert!(f.delete("DEEP_RESEARCH", &Reject).await.is_err());
        assert_eq!((config(&f), rules(&f)), (cfg, rl));
    }
}
