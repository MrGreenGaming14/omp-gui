//! Reading the MCP server configs of Claude Code, Cursor and Codex. The files
//! are only ever read; every entry ends up imported, a duplicate, or
//! unsupported with the reason.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::spec::{HttpSpec, ServerSpec, StdioSpec, Transport};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum App {
    Claude,
    Cursor,
    Codex,
}

/// A named server entry: its spec, or why it can't be used.
pub type Entry = (String, Result<ServerSpec, String>);

/// A config file of another app, read through its format's adapter.
pub trait ImportSource: Send + Sync {
    fn app(&self) -> App;
    fn path(&self) -> &Path;
    /// The `(name, entry)` pairs in the file's text.
    fn entries(&self, text: &str) -> Result<Vec<Entry>, String>;

    /// The server entries in the file; a missing file has none.
    fn read(&self) -> Result<Vec<Found>, String> {
        let text = match std::fs::read_to_string(self.path()) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            read => read.map_err(|e| format!("reading {}: {e}", self.path().display()))?,
        };
        let entries = self.entries(&text).map_err(|e| format!("parsing {}: {e}", self.path().display()))?;
        Ok(entries.into_iter().map(|(name, spec)| Found { app: self.app(), file: self.path().to_owned(), name, spec }).collect())
    }
}

/// Format-neutral access to one server entry; JSON objects and TOML tables implement it.
pub trait ConfigValue {
    /// The value at `key` as JSON; absent and null are both `None`.
    fn json(&self, key: &str) -> Option<Value>;

    fn str(&self, key: &str) -> Result<Option<String>, String> {
        match self.json(key) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(_) => Err(format!("`{key}` isn't a string")),
        }
    }

    fn strings(&self, key: &str) -> Result<Vec<String>, String> {
        match self.json(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => {
                items.into_iter().map(|item| item.as_str().map(str::to_owned).ok_or_else(|| format!("`{key}` holds a non string"))).collect()
            }
            Some(_) => Err(format!("`{key}` isn't a list")),
        }
    }

    /// A map of strings; numbers and booleans are written as text.
    fn map(&self, key: &str) -> Result<BTreeMap<String, String>, String> {
        let scalar = |(name, value): (String, Value)| match value {
            Value::String(text) => Ok((name, text)),
            Value::Number(_) | Value::Bool(_) => Ok((name, value.to_string())),
            _ => Err(format!("`{key}.{name}` isn't a string")),
        };
        match self.json(key) {
            None => Ok(BTreeMap::new()),
            Some(Value::Object(items)) => items.into_iter().map(scalar).collect(),
            Some(_) => Err(format!("`{key}` isn't a map")),
        }
    }

    fn flag(&self, key: &str) -> Option<bool> {
        self.json(key)?.as_bool()
    }
}

impl ConfigValue for serde_json::Map<String, Value> {
    fn json(&self, key: &str) -> Option<Value> {
        self.get(key).filter(|value| !value.is_null()).cloned()
    }
}

impl ConfigValue for toml::Table {
    fn json(&self, key: &str) -> Option<Value> {
        serde_json::to_value(self.get(key)?).ok()
    }
}

/// `mcpServers` of a Claude (`~/.claude.json`, a project's `.mcp.json`) or Cursor JSON file.
/// `project` also reads the local scope servers under `projects.<project>`.
pub struct JsonServers {
    pub app: App,
    pub path: PathBuf,
    pub project: Option<PathBuf>,
}

impl ImportSource for JsonServers {
    fn app(&self) -> App {
        self.app
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn entries(&self, text: &str) -> Result<Vec<Entry>, String> {
        let root: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let local = self.project.as_ref().map(|project| &root["projects"][project.to_string_lossy().as_ref()]);
        let mut out = Vec::new();
        for scope in std::iter::once(&root).chain(local) {
            let Some(servers) = scope["mcpServers"].as_object() else { continue };
            let mut entries: Vec<_> = servers.iter().map(|(name, entry)| (name.clone(), json_entry(name, entry))).collect();
            entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            out.extend(entries);
        }
        Ok(out)
    }
}

fn json_entry(name: &str, entry: &Value) -> Result<ServerSpec, String> {
    entry_spec(name, entry.as_object().ok_or("the entry isn't an object")?)
}

/// `[mcp_servers.<name>]` tables of `~/.codex/config.toml`.
pub struct CodexToml(pub PathBuf);

impl ImportSource for CodexToml {
    fn app(&self) -> App {
        App::Codex
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn entries(&self, text: &str) -> Result<Vec<Entry>, String> {
        let root: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
        let Some(servers) = root.get("mcp_servers").and_then(toml::Value::as_table) else { return Ok(Vec::new()) };
        let entry = |name: &str, entry: &toml::Value| entry_spec(name, entry.as_table().ok_or("the entry isn't a table")?);
        Ok(servers.iter().map(|(name, value)| (name.clone(), entry(name, value))).collect())
    }
}

/// One entry in any of the dialects: Claude/Cursor `type`/`headers`/`disabled`,
/// Codex `http_headers`/`bearer_token`/`enabled`.
fn entry_spec(name: &str, entry: &dyn ConfigValue) -> Result<ServerSpec, String> {
    let kind = entry.str("type")?;
    let http = matches!(kind.as_deref(), Some("http" | "streamable-http" | "streamableHttp"));
    match kind.as_deref() {
        Some("sse") => return Err("SSE servers aren't supported, only streamable HTTP".into()),
        Some(kind) if kind != "stdio" && !http => return Err(format!("transport `{kind}` isn't supported")),
        _ => {}
    }
    if entry.json("envFile").is_some() {
        return Err("`envFile` isn't supported".into());
    }
    if let Some(var) = entry.str("bearer_token_env_var")? {
        return Err(format!("the bearer token comes from the environment variable `{var}`, which isn't supported"));
    }
    if !entry.map("env_http_headers")?.is_empty() {
        return Err("headers taken from environment variables (`env_http_headers`) aren't supported".into());
    }
    let transport = match (entry.str("command")?, entry.str("url")?) {
        (Some(command), _) if !http => {
            let cwd = entry.str("cwd")?.map(PathBuf::from);
            Transport::Stdio(StdioSpec { command, args: entry.strings("args")?, env: entry.map("env")?, cwd })
        }
        (_, Some(url)) => {
            let mut headers = entry.map("headers")?;
            headers.extend(entry.map("http_headers")?);
            if let Some(token) = entry.str("bearer_token")? {
                headers.insert("Authorization".into(), format!("Bearer {token}"));
            }
            Transport::Http(HttpSpec { url, headers })
        }
        _ => return Err("the entry needs a `command` or a `url`".into()),
    };
    let enabled = entry.flag("enabled").unwrap_or(true) && !entry.flag("disabled").unwrap_or(false);
    let spec = ServerSpec { name: name.to_owned(), enabled, transport };
    spec.validate().map_err(|e| e.to_string())?;
    Ok(spec)
}

/// The usual config locations under a home directory and, optionally, a project directory.
pub fn defaults(home: &Path, project: Option<&Path>) -> Vec<Box<dyn ImportSource>> {
    let json = |app, path: PathBuf, project: Option<&Path>| -> Box<dyn ImportSource> {
        Box::new(JsonServers { app, path, project: project.map(Path::to_path_buf) })
    };
    let mut files = vec![json(App::Claude, home.join(".claude.json"), project)];
    if let Some(project) = project {
        files.push(json(App::Claude, project.join(".mcp.json"), None));
    }
    files.push(json(App::Cursor, home.join(".cursor/mcp.json"), None));
    if let Some(project) = project {
        files.push(json(App::Cursor, project.join(".cursor/mcp.json"), None));
    }
    files.push(Box::new(CodexToml(home.join(".codex/config.toml"))));
    files
}

/// One server entry found in a config file: its spec, or why it can't be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub app: App,
    pub file: PathBuf,
    pub name: String,
    pub spec: Result<ServerSpec, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Imported(ServerSpec),
    /// Already registered (or imported earlier in the run) as `existing`, by name or by reaching the same server.
    Duplicate { existing: String },
    Unsupported { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportEntry {
    pub app: App,
    pub file: PathBuf,
    pub name: String,
    pub outcome: Outcome,
}

/// A file that couldn't be read or parsed; the other files still import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileError {
    pub file: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub entries: Vec<ImportEntry>,
    pub file_errors: Vec<FileError>,
}

impl ImportReport {
    /// The specs to register.
    pub fn imported(&self) -> impl Iterator<Item = &ServerSpec> {
        self.entries.iter().filter_map(|entry| if let Outcome::Imported(spec) = &entry.outcome { Some(spec) } else { None })
    }
}

/// Decides read entries (the ones the user picked) against the `existing` servers.
pub fn plan_found(existing: &[ServerSpec], found: impl IntoIterator<Item = Found>) -> ImportReport {
    let mut known = existing.to_vec();
    let entries = found.into_iter().map(|Found { app, file, name, spec }| {
        let outcome = match spec {
            Err(reason) => Outcome::Unsupported { reason },
            Ok(spec) => match known.iter().find(|other| other.same_server(&spec) || other.name == spec.name) {
                Some(other) => Outcome::Duplicate { existing: other.name.clone() },
                None => {
                    known.push(spec.clone());
                    Outcome::Imported(spec)
                }
            },
        };
        ImportEntry { app, file, name, outcome }
    });
    ImportReport { entries: entries.collect(), file_errors: Vec::new() }
}

/// Every entry of `files`, for the import list, and the files that failed.
pub fn scan(files: &[Box<dyn ImportSource>]) -> (Vec<Found>, Vec<FileError>) {
    let (mut found, mut errors) = (Vec::new(), Vec::new());
    for file in files {
        match file.read() {
            Ok(entries) => found.extend(entries),
            Err(message) => errors.push(FileError { file: file.path().to_owned(), message }),
        }
    }
    (found, errors)
}
