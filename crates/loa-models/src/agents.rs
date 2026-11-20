//! omp agent definition files: Markdown with a YAML frontmatter block and the
//! agent's system prompt as body.
//!
//! User agents live in `<agent_dir>/agents/`, project agents in
//! `<project>/.omp/agents/`. `<name>.md.disabled` is a disabled agent. The
//! frontmatter stays a [`YamlDocument`], so keys the app does not know (`output`,
//! anything omp adds later), key order, comments and quoting survive an edit, and
//! an unedited parse followed by [`AgentDefinition::to_text`] is byte identical.
//!
//! The app's own checks ([`validate`]) are the gate: omp has no command that
//! validates a single agent file without running it.

use std::path::{Path, PathBuf};

use crate::config::THINKING_LEVELS;
use crate::fsutil::write_atomic;
use crate::yaml::{Yaml, YamlDocument, ystr, ystrs};
use crate::{Error, Result};

const MAX_NAME_LEN: usize = 64;

/// The `model` key: one model or role alias, or a fallback chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSpec {
    One(String),
    Chain(Vec<String>),
}

impl ModelSpec {
    /// Every model entry in order.
    pub fn entries(&self) -> &[String] {
        match self {
            Self::One(m) => std::slice::from_ref(m),
            Self::Chain(c) => c,
        }
    }
}

/// A parsed agent file.
#[derive(Debug, Clone)]
pub struct AgentDefinition {
    /// `---` line including its line ending.
    open: String,
    /// Closing `---` line including its line ending, if any.
    close: String,
    crlf: bool,
    /// Frontmatter with LF line endings.
    front: YamlDocument,
    body: String,
}

impl AgentDefinition {
    /// A blank agent with only `name` and `description` set.
    pub fn new(name: &str, description: &str) -> Result<Self> {
        let mut def = Self {
            open: "---\n".into(),
            close: "---\n".into(),
            crlf: false,
            front: YamlDocument::parse("", "agent")?,
            body: String::new(),
        };
        def.set_name(name)?;
        def.set_description(description)?;
        Ok(def)
    }

    pub fn name(&self) -> &str {
        self.str_key("name").unwrap_or("")
    }

    pub fn description(&self) -> &str {
        self.str_key("description").unwrap_or("")
    }

    pub fn model(&self) -> Option<ModelSpec> {
        match self.front.get(&["model"])? {
            Yaml::String(s) => Some(ModelSpec::One(s.clone())),
            Yaml::Array(items) => Some(ModelSpec::Chain(
                items.iter().filter_map(|i| i.as_str().map(str::to_owned)).collect(),
            )),
            _ => None,
        }
    }

    pub fn thinking_level(&self) -> Option<&str> {
        self.str_key("thinkingLevel")
    }

    /// `None` when the key is absent (omp's default tools).
    pub fn tools(&self) -> Option<Vec<String>> {
        self.list_key("tools")
    }

    pub fn spawns(&self) -> Option<Vec<String>> {
        self.list_key("spawns")
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn set_name(&mut self, name: &str) -> Result<()> {
        self.front.set(&["name"], ystr(name))
    }

    pub fn set_description(&mut self, description: &str) -> Result<()> {
        self.front.set(&["description"], ystr(description))
    }

    pub fn set_model(&mut self, model: Option<ModelSpec>) -> Result<()> {
        match model {
            None => self.front.remove(&["model"]).map(|_| ()),
            Some(ModelSpec::One(m)) => self.front.set(&["model"], ystr(m)),
            Some(ModelSpec::Chain(c)) => self.front.set(&["model"], ystrs(c)),
        }
    }

    pub fn set_thinking_level(&mut self, level: Option<&str>) -> Result<()> {
        match level {
            None => self.front.remove(&["thinkingLevel"]).map(|_| ()),
            Some(l) => self.front.set(&["thinkingLevel"], ystr(l)),
        }
    }

    pub fn set_tools(&mut self, tools: Option<Vec<String>>) -> Result<()> {
        self.set_list("tools", tools)
    }

    pub fn set_spawns(&mut self, spawns: Option<Vec<String>>) -> Result<()> {
        self.set_list("spawns", spawns)
    }

    pub fn set_body(&mut self, body: impl Into<String>) {
        self.body = body.into();
    }

    /// The file text. Identical to the parsed input when nothing was edited.
    pub fn to_text(&self) -> String {
        let front = self.front.text();
        let mut out = String::with_capacity(
            self.open.len() + front.len() + self.close.len() + self.body.len() + 16,
        );
        out.push_str(&self.open);
        if self.crlf {
            out.push_str(&front.replace('\n', "\r\n"));
        } else {
            out.push_str(front);
        }
        out.push_str(&self.close);
        out.push_str(&self.body);
        out
    }

    fn str_key(&self, key: &str) -> Option<&str> {
        self.front.get(&[key])?.as_str()
    }

    fn list_key(&self, key: &str) -> Option<Vec<String>> {
        match self.front.get(&[key])? {
            Yaml::Array(items) => {
                Some(items.iter().filter_map(|i| i.as_str().map(str::to_owned)).collect())
            }
            Yaml::String(s) => Some(vec![s.clone()]),
            _ => None,
        }
    }

    fn set_list(&mut self, key: &str, list: Option<Vec<String>>) -> Result<()> {
        match list {
            None => self.front.remove(&[key]).map(|_| ()),
            Some(l) => self.front.set(&[key], ystrs(l)),
        }
    }
}

/// Parses an agent file.
pub fn parse(text: &str) -> Result<AgentDefinition> {
    let bad = |message: &str| Error::Parse {
        what: "agent file".into(),
        message: message.into(),
    };
    let mut lines = text.split_inclusive('\n');
    let open = lines.next().filter(|l| l.trim_end() == "---");
    let Some(open) = open else {
        return Err(bad("the first line must be `---`"));
    };
    let crlf = open.ends_with("\r\n");
    let mut offset = open.len();
    let mut front = String::new();
    let mut close = None;
    for line in lines {
        offset += line.len();
        if line.trim_end() == "---" {
            close = Some(line);
            break;
        }
        front.push_str(line);
    }
    let Some(close) = close else {
        return Err(bad("the frontmatter is not closed by a `---` line"));
    };
    let front = if crlf { front.replace("\r\n", "\n") } else { front };
    Ok(AgentDefinition {
        open: open.to_owned(),
        close: close.to_owned(),
        crlf,
        front: YamlDocument::parse(&front, "agent frontmatter")?,
        body: text[offset..].to_owned(),
    })
}

/// Where an agent file was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSource {
    User,
    Project,
}

/// An agent file on disk.
#[derive(Debug, Clone)]
pub struct AgentFile {
    pub definition: AgentDefinition,
    pub path: PathBuf,
    pub source: AgentSource,
    pub enabled: bool,
}

/// Lists agents in `<agent_dir>/agents` and, when given, `<project>/.omp/agents`,
/// sorted by name. Files that fail to read or parse are returned separately.
pub fn list(agent_dir: &Path, project: Option<&Path>) -> (Vec<AgentFile>, Vec<(PathBuf, Error)>) {
    let mut files = Vec::new();
    let mut broken = Vec::new();
    scan(&agent_dir.join("agents"), AgentSource::User, &mut files, &mut broken);
    if let Some(project) = project {
        scan(&project.join(".omp").join("agents"), AgentSource::Project, &mut files, &mut broken);
    }
    files.sort_by(|a, b| {
        a.definition
            .name()
            .to_lowercase()
            .cmp(&b.definition.name().to_lowercase())
            .then_with(|| a.path.cmp(&b.path))
    });
    broken.sort_by(|a, b| a.0.cmp(&b.0));
    (files, broken)
}

fn scan(
    dir: &Path,
    source: AgentSource,
    files: &mut Vec<AgentFile>,
    broken: &mut Vec<(PathBuf, Error)>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            broken.push((dir.to_owned(), Error::io(dir, e)));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let enabled = if file_name.ends_with(".md") {
            true
        } else if file_name.ends_with(".md.disabled") {
            false
        } else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
        let result = std::fs::read_to_string(&path)
            .map_err(|e| Error::io(&path, e))
            .and_then(|text| parse(&text));
        match result {
            Ok(definition) => files.push(AgentFile { definition, path, source, enabled }),
            Err(e) => broken.push((path, e)),
        }
    }
}

/// A problem with one field of an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
}

fn field(field: &'static str, message: impl Into<String>) -> FieldError {
    FieldError { field, message: message.into() }
}

fn safe_stem_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("is required".into());
    }
    if name.len() > MAX_NAME_LEN {
        return Some(format!("must be at most {MAX_NAME_LEN} characters"));
    }
    if name.starts_with('.') {
        return Some("must not start with '.'".into());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return Some("may only contain ASCII letters, digits, '-', '_' and '.'".into());
    }
    None
}

/// Checks an agent. Tool and agent names are checked only against non empty lists.
pub fn validate(
    def: &AgentDefinition,
    known_tools: &[String],
    known_agents: &[String],
) -> Vec<FieldError> {
    let mut errors = Vec::new();
    let name = def.name();
    if let Some(problem) = safe_stem_problem(name) {
        errors.push(field("name", format!("name {problem}")));
    }
    if def.description().trim().is_empty() {
        errors.push(field("description", "description is required"));
    }
    if let Some(tools) = def.tools()
        && !known_tools.is_empty()
    {
        for tool in tools.iter().filter(|t| !known_tools.contains(t)) {
            errors.push(field("tools", format!("unknown tool '{tool}'")));
        }
    }
    if let Some(spawns) = def.spawns() {
        for spawn in &spawns {
            if spawn == name {
                errors.push(field("spawns", "an agent cannot spawn itself"));
            } else if !known_agents.is_empty() && !known_agents.contains(spawn) {
                errors.push(field("spawns", format!("unknown agent '{spawn}'")));
            }
        }
    }
    if let Some(level) = def.thinking_level()
        && !THINKING_LEVELS.contains(&level)
    {
        errors.push(field("thinkingLevel", format!("unknown thinking level '{level}'")));
    }
    if let Some(model) = def.model()
        && (model.entries().is_empty() || model.entries().iter().any(|m| m.trim().is_empty()))
    {
        errors.push(field("model", "model entries must not be empty"));
    }
    errors
}

/// Whether [`write`] may replace an existing agent file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overwrite {
    No,
    Yes,
}

fn disabled_twin(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".disabled");
    PathBuf::from(s)
}

/// Writes `<dir>/<name>.md` atomically, creating `dir`. Refuses unsafe names and,
/// with [`Overwrite::No`], an existing file or `.md.disabled` twin.
pub fn write(dir: &Path, def: &AgentDefinition, overwrite: Overwrite) -> Result<PathBuf> {
    let name = def.name();
    if let Some(problem) = safe_stem_problem(name) {
        return Err(Error::invalid("name", problem));
    }
    let path = dir.join(format!("{name}.md"));
    if overwrite == Overwrite::No {
        for candidate in [path.clone(), disabled_twin(&path)] {
            if candidate.symlink_metadata().is_ok() {
                return Err(Error::AlreadyExists { path: candidate });
            }
        }
    }
    write_atomic(&path, &def.to_text())?;
    Ok(path)
}

/// Enables or disables an agent by renaming `.md` <-> `.md.disabled`. Returns the
/// new path; a file already in the requested state is left alone.
pub fn set_enabled(path: &Path, enabled: bool) -> Result<PathBuf> {
    let name = path.to_string_lossy();
    let target = if enabled {
        match name.strip_suffix(".disabled") {
            Some(stripped) if stripped.ends_with(".md") => PathBuf::from(stripped),
            _ => return Ok(path.to_owned()),
        }
    } else if name.ends_with(".md") {
        disabled_twin(path)
    } else {
        return Ok(path.to_owned());
    };
    if target.symlink_metadata().is_ok() {
        return Err(Error::AlreadyExists { path: target });
    }
    std::fs::rename(path, &target).map_err(|e| Error::io(path, e))?;
    Ok(target)
}

/// Deletes an agent file.
pub fn delete(path: &Path) -> Result<()> {
    std::fs::remove_file(path).map_err(|e| Error::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCOUT: &str = "---\nname: code-scout\ndescription: Read-only research specialist.\nmodel: anthropic/claude-haiku-5-5\nthinkingLevel: low\ntools:\n  - read\n  - grep\n---\n\nYou are Code Scout.\n- Do not modify files\n";
    const BUNDLED: &str = "---\nname: plan\n# why: planning only\ndescription: \"Software architect: plans\"\nmodel:\n  - \"@slow\"\n  - anthropic/claude-opus-5-5\nspawns:\n  - explore\ntools: [read, grep]\noutput:\n  properties:\n    plan:\n      type: string\n  optionalProperties:\n    notes:\n      type: array\n---\nNo trailing newline";

    fn strs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn round_trips_are_byte_identical() {
        for text in [SCOUT.to_owned(), BUNDLED.to_owned(), SCOUT.replace('\n', "\r\n"), BUNDLED.replace('\n', "\r\n")] {
            assert_eq!(parse(&text).unwrap().to_text(), text);
        }
        let def = parse(BUNDLED).unwrap();
        assert_eq!(def.model(), Some(ModelSpec::Chain(strs(&["@slow", "anthropic/claude-opus-5-5"]))));
        assert_eq!(def.spawns(), Some(strs(&["explore"])));
        assert_eq!(def.tools(), Some(strs(&["read", "grep"])));
        assert_eq!(parse(SCOUT).unwrap().model(), Some(ModelSpec::One("anthropic/claude-haiku-5-5".into())));
    }

    #[test]
    fn rejects_missing_frontmatter() {
        assert!(parse("name: x\n").is_err());
        assert!(parse("---\nname: x\n").is_err());
    }

    #[test]
    fn edits_keep_unknown_keys_and_comments() {
        let mut def = parse(BUNDLED).unwrap();
        def.set_tools(Some(strs(&["read", "bash"]))).unwrap();
        def.set_thinking_level(Some("high")).unwrap();
        def.set_spawns(None).unwrap();
        def.set_body("New body\n");
        let text = def.to_text();
        assert!(text.contains("# why: planning only"));
        assert!(text.contains("optionalProperties:\n    notes:\n      type: array\n"));
        let back = parse(&text).unwrap();
        assert_eq!(back.tools(), Some(strs(&["read", "bash"])));
        assert_eq!(back.thinking_level(), Some("high"));
        assert_eq!(back.spawns(), None);
        assert_eq!(back.body(), "New body\n");
        assert_eq!(back.model(), def.model());
    }

    #[test]
    fn crlf_edits_keep_crlf() {
        let mut def = parse(&SCOUT.replace('\n', "\r\n")).unwrap();
        def.set_thinking_level(Some("high")).unwrap();
        let text = def.to_text();
        assert!(text.contains("thinkingLevel: high\r\n"));
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());
    }

    #[test]
    fn new_agent_has_name_and_description() {
        let def = AgentDefinition::new("helper", "Does things: well").unwrap();
        let back = parse(&def.to_text()).unwrap();
        assert_eq!(back.name(), "helper");
        assert_eq!(back.description(), "Does things: well");
        assert_eq!(back.tools(), None);
    }

    #[test]
    fn list_reads_user_project_and_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("agent");
        let project = tmp.path().join("proj");
        let user = agent_dir.join("agents");
        let proj = project.join(".omp").join("agents");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(user.join("code-scout.md"), SCOUT).unwrap();
        std::fs::write(user.join("plan.md.disabled"), BUNDLED).unwrap();
        std::fs::write(user.join("broken.md"), "no frontmatter").unwrap();
        std::fs::write(user.join("notes.txt"), "ignored").unwrap();
        std::fs::write(proj.join("alpha.md"), "---\nname: alpha\ndescription: d\n---\n").unwrap();

        let (files, broken) = list(&agent_dir, Some(&project));
        let seen: Vec<_> = files.iter().map(|f| (f.definition.name().to_owned(), f.source, f.enabled)).collect();
        assert_eq!(
            seen,
            vec![
                ("alpha".into(), AgentSource::Project, true),
                ("code-scout".into(), AgentSource::User, true),
                ("plan".into(), AgentSource::User, false),
            ]
        );
        assert_eq!(broken.len(), 1);
        assert!(broken[0].0.ends_with("broken.md"));

        let (none, none_broken) = list(&tmp.path().join("missing"), None);
        assert!(none.is_empty() && none_broken.is_empty());
    }

    #[test]
    fn validate_cases() {
        let ok = parse(SCOUT).unwrap();
        assert!(validate(&ok, &[], &[]).is_empty());
        assert!(validate(&ok, &strs(&["read", "grep"]), &[]).is_empty());
        let errs = validate(&ok, &strs(&["read"]), &[]);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].field, "tools");

        let mut def = AgentDefinition::new("bad name", "").unwrap();
        def.set_thinking_level(Some("extreme")).unwrap();
        def.set_spawns(Some(strs(&["bad name", "ghost"]))).unwrap();
        def.set_model(Some(ModelSpec::Chain(strs(&["a", ""])))).unwrap();
        let fields: Vec<_> = validate(&def, &[], &strs(&["other"])).iter().map(|e| e.field).collect();
        for f in ["name", "description", "thinkingLevel", "spawns", "model"] {
            assert!(fields.contains(&f), "missing {f}: {fields:?}");
        }
        assert_eq!(fields.iter().filter(|f| **f == "spawns").count(), 2);

        for name in ["", ".hidden", "a/b", "../x", "x".repeat(65).as_str()] {
            let d = AgentDefinition::new(name, "d").unwrap();
            assert!(validate(&d, &[], &[]).iter().any(|e| e.field == "name"), "{name}");
        }
    }

    #[test]
    fn write_refuses_overwrite_and_unsafe_names() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents");
        let def = parse(SCOUT).unwrap();
        let path = write(&dir, &def, Overwrite::No).unwrap();
        assert_eq!(path, dir.join("code-scout.md"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SCOUT);
        assert!(matches!(write(&dir, &def, Overwrite::No), Err(Error::AlreadyExists { .. })));
        write(&dir, &def, Overwrite::Yes).unwrap();

        let disabled = set_enabled(&path, false).unwrap();
        assert!(matches!(write(&dir, &def, Overwrite::No), Err(Error::AlreadyExists { .. })));
        assert!(disabled.ends_with("code-scout.md.disabled"));

        for name in ["../x", "a/b", ".hidden", ""] {
            let d = AgentDefinition::new(name, "d").unwrap();
            assert!(matches!(write(&dir, &d, Overwrite::Yes), Err(Error::Invalid { .. })), "{name}");
        }
        assert!(!tmp.path().join("x.md").exists());
    }

    #[test]
    fn set_enabled_round_trip_and_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(tmp.path(), &parse(SCOUT).unwrap(), Overwrite::No).unwrap();
        let off = set_enabled(&path, false).unwrap();
        assert!(!path.exists() && off.exists());
        assert_eq!(set_enabled(&off, false).unwrap(), off);
        let on = set_enabled(&off, true).unwrap();
        assert_eq!(on, path);
        assert!(path.exists());
        delete(&path).unwrap();
        assert!(!path.exists());
    }
}
