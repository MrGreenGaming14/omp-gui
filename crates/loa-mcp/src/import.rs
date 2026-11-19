//! Reading the MCP server configs of Claude Code, Cursor and Codex.
//!
//! The files are only ever read. Every entry ends up in the report as
//! imported, skipped as a duplicate, or unsupported with the reason.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::spec::{HttpSpec, ServerSpec, StdioSpec, Transport, validate_name};
use crate::{Error, Result};

/// The app a config file belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum App {
    Claude,
    Cursor,
    Codex,
}

/// One config file to import from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigFile {
    /// `~/.claude.json`: the user `mcpServers`, plus the local scope servers
    /// stored under `projects.<project>.mcpServers` when `project` is given.
    ClaudeUser { path: PathBuf, project: Option<PathBuf> },
    /// A project's `.mcp.json`.
    ClaudeProject(PathBuf),
    /// `~/.cursor/mcp.json` or a project's `.cursor/mcp.json`.
    Cursor(PathBuf),
    /// `~/.codex/config.toml`, `[mcp_servers.<name>]` tables.
    Codex(PathBuf),
}

impl ConfigFile {
    /// The usual locations under a home directory and, optionally, a
    /// project directory.
    pub fn defaults(home: &Path, project: Option<&Path>) -> Vec<Self> {
        let mut files = vec![Self::ClaudeUser { path: home.join(".claude.json"), project: project.map(Path::to_path_buf) }];
        if let Some(project) = project {
            files.push(Self::ClaudeProject(project.join(".mcp.json")));
        }
        files.push(Self::Cursor(home.join(".cursor").join("mcp.json")));
        if let Some(project) = project {
            files.push(Self::Cursor(project.join(".cursor").join("mcp.json")));
        }
        files.push(Self::Codex(home.join(".codex").join("config.toml")));
        files
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::ClaudeUser { path, .. } | Self::ClaudeProject(path) | Self::Cursor(path) | Self::Codex(path) => path,
        }
    }

    pub fn app(&self) -> App {
        match self {
            Self::ClaudeUser { .. } | Self::ClaudeProject(_) => App::Claude,
            Self::Cursor(_) => App::Cursor,
            Self::Codex(_) => App::Codex,
        }
    }

    /// The server entries in the file. A missing file has none.
    pub fn read(&self) -> Result<Vec<Found>> {
        let path = self.path();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(Error::Read { path: path.to_owned(), source }),
        };
        let parse_error = |message: String| Error::Parse { path: path.to_owned(), message };
        let entries = match self {
            Self::ClaudeUser { project, .. } => {
                let root: Value = serde_json::from_str(&text).map_err(|e| parse_error(e.to_string()))?;
                let mut entries = json_servers(&root);
                if let Some(project) = project {
                    let key = project.to_string_lossy();
                    if let Some(local) = root.get("projects").and_then(|projects| projects.get(key.as_ref())) {
                        entries.extend(json_servers(local));
                    }
                }
                entries
            }
            Self::ClaudeProject(_) | Self::Cursor(_) => {
                let root: Value = serde_json::from_str(&text).map_err(|e| parse_error(e.to_string()))?;
                json_servers(&root)
            }
            Self::Codex(_) => {
                let root: toml::Table = toml::from_str(&text).map_err(|e| parse_error(e.to_string()))?;
                codex_servers(&root)
            }
        };
        let app = self.app();
        Ok(entries
            .into_iter()
            .map(|(name, spec)| Found { app, file: path.to_owned(), name, spec })
            .collect())
    }
}

/// One server entry found in a config file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub app: App,
    pub file: PathBuf,
    pub name: String,
    /// The spec, or why the entry can't be used.
    pub spec: Result<ServerSpec, String>,
}

/// What happened to one entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Imported(ServerSpec),
    /// Already registered (or imported earlier in the same run) as `existing`,
    /// either under the same name or reaching the same server.
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
        self.entries.iter().filter_map(|entry| match &entry.outcome {
            Outcome::Imported(spec) => Some(spec),
            _ => None,
        })
    }
}

/// Reads `files` and decides each entry against the `existing` servers.
pub fn plan(existing: &[ServerSpec], files: &[ConfigFile]) -> ImportReport {
    let mut report = ImportReport::default();
    let mut known: Vec<ServerSpec> = existing.to_vec();
    for file in files {
        let found = match file.read() {
            Ok(found) => found,
            Err(e) => {
                report.file_errors.push(FileError { file: file.path().to_owned(), message: e.to_string() });
                continue;
            }
        };
        for Found { app, file, name, spec } in found {
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
            report.entries.push(ImportEntry { app, file, name, outcome });
        }
    }
    report
}

/// `mcpServers` of a Claude or Cursor JSON object.
fn json_servers(root: &Value) -> Vec<(String, Result<ServerSpec, String>)> {
    let Some(servers) = root.get("mcpServers").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = servers.iter().map(|(name, entry)| (name.clone(), json_entry(name, entry))).collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    entries
}

fn json_entry(name: &str, entry: &Value) -> Result<ServerSpec, String> {
    let entry = entry.as_object().ok_or("the entry isn't an object")?;
    let text = |key: &str| -> Result<Option<String>, String> {
        match entry.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(_) => Err(format!("`{key}` isn't a string")),
        }
    };
    let kind = text("type")?;
    let command = text("command")?;
    let url = text("url")?;
    let http_kind = matches!(kind.as_deref(), Some("http" | "streamable-http" | "streamableHttp"));
    let transport = match (kind.as_deref(), command, url) {
        (Some("sse"), ..) => return Err("SSE servers aren't supported, only streamable HTTP".to_owned()),
        (Some(kind), ..) if kind != "stdio" && !http_kind => {
            return Err(format!("transport `{kind}` isn't supported"));
        }
        (None | Some("stdio"), Some(command), _) => {
            if entry.contains_key("envFile") {
                return Err("`envFile` isn't supported".to_owned());
            }
            Transport::Stdio(StdioSpec {
                command,
                args: json_strings(entry.get("args"), "args")?,
                env: json_map(entry.get("env"), "env")?,
                cwd: text("cwd")?.map(PathBuf::from),
            })
        }
        (kind, _, Some(url)) if kind.is_none() || http_kind => {
            Transport::Http(HttpSpec { url, headers: json_map(entry.get("headers"), "headers")? })
        }
        _ => return Err("the entry needs a `command` or a `url`".to_owned()),
    };
    let enabled = !entry.get("disabled").and_then(Value::as_bool).unwrap_or(false);
    spec(name, enabled, transport)
}

fn json_strings(value: Option<&Value>, key: &str) -> Result<Vec<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned).ok_or_else(|| format!("`{key}` holds a non string")))
            .collect(),
        Some(_) => Err(format!("`{key}` isn't a list")),
    }
}

fn json_map(value: Option<&Value>, key: &str) -> Result<BTreeMap<String, String>, String> {
    match value {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(Value::Object(items)) => items
            .iter()
            .map(|(name, value)| match value {
                Value::String(text) => Ok((name.clone(), text.clone())),
                Value::Number(number) => Ok((name.clone(), number.to_string())),
                Value::Bool(flag) => Ok((name.clone(), flag.to_string())),
                _ => Err(format!("`{key}.{name}` isn't a string")),
            })
            .collect(),
        Some(_) => Err(format!("`{key}` isn't an object")),
    }
}

/// `[mcp_servers.<name>]` tables of a Codex config.
fn codex_servers(root: &toml::Table) -> Vec<(String, Result<ServerSpec, String>)> {
    let Some(servers) = root.get("mcp_servers").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    servers.iter().map(|(name, entry)| (name.clone(), codex_entry(name, entry))).collect()
}

fn codex_entry(name: &str, entry: &toml::Value) -> Result<ServerSpec, String> {
    let entry = entry.as_table().ok_or("the entry isn't a table")?;
    let text = |key: &str| -> Result<Option<String>, String> {
        match entry.get(key) {
            None => Ok(None),
            Some(toml::Value::String(text)) => Ok(Some(text.clone())),
            Some(_) => Err(format!("`{key}` isn't a string")),
        }
    };
    let transport = if let Some(url) = text("url")? {
        if let Some(var) = text("bearer_token_env_var")? {
            return Err(format!("the bearer token comes from the environment variable `{var}`, which isn't supported"));
        }
        if entry.get("env_http_headers").and_then(toml::Value::as_table).is_some_and(|headers| !headers.is_empty()) {
            return Err("headers taken from environment variables (`env_http_headers`) aren't supported".to_owned());
        }
        let mut headers = toml_map(entry.get("http_headers"), "http_headers")?;
        if let Some(token) = text("bearer_token")? {
            headers.insert("Authorization".to_owned(), format!("Bearer {token}"));
        }
        Transport::Http(HttpSpec { url, headers })
    } else if let Some(command) = text("command")? {
        let args = match entry.get("args") {
            None => Vec::new(),
            Some(toml::Value::Array(items)) => items
                .iter()
                .map(|item| item.as_str().map(str::to_owned).ok_or_else(|| "`args` holds a non string".to_owned()))
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("`args` isn't a list".to_owned()),
        };
        Transport::Stdio(StdioSpec {
            command,
            args,
            env: toml_map(entry.get("env"), "env")?,
            cwd: text("cwd")?.map(PathBuf::from),
        })
    } else {
        return Err("the entry needs a `command` or a `url`".to_owned());
    };
    let enabled = entry.get("enabled").and_then(toml::Value::as_bool).unwrap_or(true);
    spec(name, enabled, transport)
}

fn toml_map(value: Option<&toml::Value>, key: &str) -> Result<BTreeMap<String, String>, String> {
    match value {
        None => Ok(BTreeMap::new()),
        Some(toml::Value::Table(items)) => items
            .iter()
            .map(|(name, value)| match value {
                toml::Value::String(text) => Ok((name.clone(), text.clone())),
                toml::Value::Integer(number) => Ok((name.clone(), number.to_string())),
                toml::Value::Boolean(flag) => Ok((name.clone(), flag.to_string())),
                _ => Err(format!("`{key}.{name}` isn't a string")),
            })
            .collect(),
        Some(_) => Err(format!("`{key}` isn't a table")),
    }
}

fn spec(name: &str, enabled: bool, transport: Transport) -> Result<ServerSpec, String> {
    validate_name(name).map_err(|e| e.to_string())?;
    let spec = ServerSpec { name: name.to_owned(), enabled, transport };
    spec.validate().map_err(|e| e.to_string())?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, relative: &str, text: &str) -> PathBuf {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn outcome<'a>(report: &'a ImportReport, app: App, name: &str) -> &'a Outcome {
        &report.entries.iter().find(|entry| entry.app == app && entry.name == name).unwrap().outcome
    }

    fn stdio(name: &str, command: &str, args: &[&str]) -> ServerSpec {
        ServerSpec {
            name: name.to_owned(),
            enabled: true,
            transport: Transport::Stdio(StdioSpec {
                command: command.to_owned(),
                args: args.iter().map(|arg| (*arg).to_owned()).collect(),
                ..StdioSpec::default()
            }),
        }
    }

    const CLAUDE_USER: &str = r#"{
        "numStartups": 12,
        "mcpServers": {
            "github": {"type": "http", "url": "https://api.example.com/mcp", "headers": {"Authorization": "Bearer t0k"}},
            "fs": {"type": "stdio", "command": "npx", "args": ["-y", "@example/fs", "/work"], "env": {"LOG": "debug"}},
            "legacy": {"type": "sse", "url": "https://sse.example.com/sse"}
        },
        "projects": {
            "/work/app": {"mcpServers": {"db": {"command": "db-mcp", "args": ["--ro"]}}},
            "/work/other": {"mcpServers": {"other": {"command": "other-mcp"}}}
        }
    }"#;

    #[test]
    fn claude_user_file_reads_user_and_local_scope() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), ".claude.json", CLAUDE_USER);
        let found = ConfigFile::ClaudeUser { path, project: Some("/work/app".into()) }.read().unwrap();
        let names: Vec<_> = found.iter().map(|found| found.name.as_str()).collect();
        assert_eq!(names, ["fs", "github", "legacy", "db"]);

        let github = found.iter().find(|found| found.name == "github").unwrap().spec.clone().unwrap();
        let Transport::Http(http) = github.transport else { panic!("github isn't http") };
        assert_eq!(http.url, "https://api.example.com/mcp");
        assert_eq!(http.headers["Authorization"], "Bearer t0k");

        let fs = found.iter().find(|found| found.name == "fs").unwrap().spec.clone().unwrap();
        let Transport::Stdio(fs) = fs.transport else { panic!("fs isn't stdio") };
        assert_eq!((fs.command.as_str(), fs.args.len(), fs.env["LOG"].as_str()), ("npx", 3, "debug"));

        let legacy = &found.iter().find(|found| found.name == "legacy").unwrap().spec;
        assert!(legacy.as_ref().unwrap_err().contains("SSE"));
    }

    #[test]
    fn cursor_and_claude_project_files_share_the_json_shape() {
        let dir = tempfile::tempdir().unwrap();
        let cursor = write(
            dir.path(),
            ".cursor/mcp.json",
            r#"{"mcpServers": {
                "remote": {"url": "http://127.0.0.1:9/mcp"},
                "local": {"command": "local-mcp", "env": {"PORT": 8080}},
                "off": {"command": "off-mcp", "disabled": true},
                "withfile": {"command": "x", "envFile": ".env"},
                "bad name": {"command": "y"}
            }}"#,
        );
        let found = ConfigFile::Cursor(cursor).read().unwrap();
        let get = |name: &str| found.iter().find(|found| found.name == name).unwrap().spec.clone();
        assert!(matches!(get("remote").unwrap().transport, Transport::Http(_)));
        let Transport::Stdio(local) = get("local").unwrap().transport else { panic!("local isn't stdio") };
        assert_eq!(local.env["PORT"], "8080");
        assert!(!get("off").unwrap().enabled);
        assert!(get("withfile").unwrap_err().contains("envFile"));
        assert!(get("bad name").unwrap_err().contains("server name"));

        let project = write(dir.path(), "proj/.mcp.json", r#"{"mcpServers": {"p": {"command": "p-mcp"}}}"#);
        let found = ConfigFile::ClaudeProject(project).read().unwrap();
        assert_eq!(found[0].spec.clone().unwrap(), stdio("p", "p-mcp", &[]));
    }

    #[test]
    fn codex_tables_import_with_their_quirks() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            ".codex/config.toml",
            r#"
model = "o3"

[mcp_servers.docs]
command = "docs-mcp"
args = ["--port", "0"]
env = { API_KEY = "k" }

[mcp_servers.paused]
command = "paused-mcp"
enabled = false

[mcp_servers.web]
url = "https://web.example.com/mcp"
bearer_token = "abc"
http_headers = { "X-Team" = "core" }

[mcp_servers.envtoken]
url = "https://env.example.com/mcp"
bearer_token_env_var = "ENV_TOKEN"

[mcp_servers.empty]
startup_timeout_sec = 10
"#,
        );
        let found = ConfigFile::Codex(path).read().unwrap();
        let get = |name: &str| found.iter().find(|found| found.name == name).unwrap().spec.clone();
        let Transport::Stdio(docs) = get("docs").unwrap().transport else { panic!("docs isn't stdio") };
        assert_eq!((docs.args.as_slice(), docs.env["API_KEY"].as_str()), (&["--port".to_owned(), "0".to_owned()][..], "k"));
        assert!(!get("paused").unwrap().enabled);
        let Transport::Http(web) = get("web").unwrap().transport else { panic!("web isn't http") };
        assert_eq!(web.headers["Authorization"], "Bearer abc");
        assert_eq!(web.headers["X-Team"], "core");
        assert!(get("envtoken").unwrap_err().contains("ENV_TOKEN"));
        assert!(get("empty").is_err());
    }

    #[test]
    fn plan_reports_imported_duplicates_and_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let project = dir.path().join("proj");
        write(&home, ".claude.json", r#"{"mcpServers": {"fs": {"command": "npx", "args": ["-y", "@example/fs"]}, "old": {"type": "sse", "url": "https://x.example.com"}}}"#);
        // Same server under another name, and a name collision with a different server.
        write(&home, ".cursor/mcp.json", r#"{"mcpServers": {"files": {"command": "npx", "args": ["-y", "@example/fs"]}, "git": {"command": "other-git"}}}"#);
        write(&home, ".codex/config.toml", "[mcp_servers.search]\ncommand = \"search-mcp\"\n");
        write(&project, ".cursor/mcp.json", "{ not json");

        let existing = [stdio("git", "git-mcp", &[])];
        let files = ConfigFile::defaults(&home, Some(&project));
        let report = plan(&existing, &files);

        assert!(matches!(outcome(&report, App::Claude, "fs"), Outcome::Imported(_)));
        assert!(matches!(outcome(&report, App::Claude, "old"), Outcome::Unsupported { .. }));
        assert_eq!(outcome(&report, App::Cursor, "files"), &Outcome::Duplicate { existing: "fs".to_owned() });
        assert_eq!(outcome(&report, App::Cursor, "git"), &Outcome::Duplicate { existing: "git".to_owned() });
        assert!(matches!(outcome(&report, App::Codex, "search"), Outcome::Imported(_)));
        let imported: Vec<_> = report.imported().map(|spec| spec.name.as_str()).collect();
        assert_eq!(imported, ["fs", "search"]);
        // The broken project file is reported, the missing ones are not.
        assert_eq!(report.file_errors.len(), 1);
        assert!(report.file_errors[0].file.ends_with("proj/.cursor/mcp.json"));
    }

    #[test]
    fn import_never_writes_the_source_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), ".claude.json", CLAUDE_USER);
        let before = std::fs::read(&path).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        plan(&[], &[ConfigFile::ClaudeUser { path: path.clone(), project: None }]);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), modified);
    }
}
