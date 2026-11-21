//! The add-server, add-host and host limits forms: kit [`Form`]s for editing and
//! [`FormSpec`]s for validation, keyed by field name.

use super::model::{HostLimits, RemoteHost};
use loa_contracts::harness::{McpServerSpec, McpTransport};
use loa_ui_kit::widgets::{Field, Form, FormSpec};
use std::collections::BTreeMap;

/// A field of a tools form, by its key.
pub type FieldId = &'static str;

/// Why a field was rejected; the text shows under the field.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FieldError {
    #[error("required")]
    Empty,
    #[error("use letters, digits, '-' or '_'")]
    BadName,
    #[error("a server with this name already exists")]
    DuplicateServer,
    #[error("a host with this alias already exists")]
    DuplicateHost,
    #[error("unbalanced quote")]
    Unbalanced,
    #[error("only environment assignments, no command")]
    NoCommand,
    #[error("URL must start with http:// or https://")]
    BadScheme,
    #[error("URL has no host")]
    NoHost,
    #[error("must not contain spaces")]
    Whitespace,
    #[error("port must be a number from 1 to 65535")]
    BadPort,
    #[error("path must start with '/' or '~'")]
    RelativePath,
    #[error("path is already allowed")]
    DuplicatePath,
}

type Errors = BTreeMap<FieldId, String>;

/// How a server being added is reached.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransportKind {
    #[default]
    Stdio,
    Http,
}

/// Fields of the add-server form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddServerForm {
    pub name: String,
    pub kind: TransportKind,
    /// The stdio command line, with optional leading `KEY=value` assignments.
    pub command: String,
    pub url: String,
}

const NAME: Field = Field { key: "name", label: "Name", placeholder: "my-server" };
const STDIO_FIELDS: &[Field] = &[NAME, Field { key: "command", label: "Command", placeholder: "npx -y @scope/server" }];
const HTTP_FIELDS: &[Field] = &[NAME, Field { key: "url", label: "URL", placeholder: "https://host/mcp" }];

impl AddServerForm {
    fn slot(&mut self, key: &str) -> Option<&mut String> {
        Some(match key {
            "name" => &mut self.name,
            "command" => &mut self.command,
            "url" => &mut self.url,
            _ => return None,
        })
    }
}

impl Form for AddServerForm {
    fn fields(&self) -> &[Field] { if self.kind == TransportKind::Stdio { STDIO_FIELDS } else { HTTP_FIELDS } }
    fn get(&self, key: &str) -> String { self.clone().slot(key).cloned().unwrap_or_default() }
    fn set(&mut self, key: &str, value: &str) -> Result<(), String> { self.slot(key).map(|s| *s = value.into()).ok_or_else(|| format!("no field {key}")) }
}

impl FormSpec for AddServerForm {
    type Field = FieldId;
    type Output = McpServerSpec;

    /// Checks the name against `existing` ones and builds the spec.
    fn validate(&self, existing: &[String]) -> Result<McpServerSpec, Errors> {
        let name = self.name.trim();
        let name_error = match name {
            "" => Some(FieldError::Empty),
            n if !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') => Some(FieldError::BadName),
            n if existing.iter().any(|e| e == n) => Some(FieldError::DuplicateServer),
            _ => None,
        };
        let transport = match self.kind {
            TransportKind::Stdio => parse_stdio(&self.command).map_err(|e| ("command", e)),
            TransportKind::Http => validate_url(&self.url).map(|url| McpTransport::Http { url: url.into(), headers: BTreeMap::new() }).map_err(|e| ("url", e)),
        };
        match (name_error, transport) {
            (None, Ok(transport)) => Ok(McpServerSpec { name: name.into(), transport }),
            (name, target) => Err(name.map(|e| ("name", e)).into_iter().chain(target.err()).map(|(k, e)| (k, e.to_string())).collect()),
        }
    }
}

/// A shell-split command line: leading `KEY=value` words become the environment.
fn parse_stdio(line: &str) -> Result<McpTransport, FieldError> {
    let mut words = shell_words::split(line).map_err(|_| FieldError::Unbalanced)?.into_iter().peekable();
    let is_env = |k: &str| k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let mut env = BTreeMap::new();
    while let Some((k, v)) = words.peek().and_then(|w| w.split_once('=')).filter(|(k, _)| is_env(k)).map(|(k, v)| (k.to_owned(), v.to_owned())) {
        env.insert(k, v);
        words.next();
    }
    let command = words.next().ok_or(if env.is_empty() { FieldError::Empty } else { FieldError::NoCommand })?;
    Ok(McpTransport::Stdio { command, args: words.collect(), env })
}

/// An HTTP(S) server URL, trimmed.
pub fn validate_url(url: &str) -> Result<&str, FieldError> {
    let url = url.trim();
    if url.is_empty() { return Err(FieldError::Empty) }
    if url.contains(char::is_whitespace) { return Err(FieldError::Whitespace) }
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let rest = ["https://", "http://"].iter().find(|p| lower.starts_with(**p)).map(|p| &url[p.len()..]).ok_or(FieldError::BadScheme)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = match host_port.strip_prefix('[') {
        Some(v6) => v6.split_once(']').map(|(h, after)| (h, after.strip_prefix(':'))).ok_or(FieldError::NoHost)?,
        None => host_port.split_once(':').map_or((host_port, None), |(h, p)| (h, Some(p))),
    };
    if host.is_empty() { return Err(FieldError::NoHost) }
    port.map(parse_port).transpose().map(|_| url)
}

fn parse_port(port: &str) -> Result<u16, FieldError> { port.parse::<u16>().ok().filter(|&p| p != 0).ok_or(FieldError::BadPort) }

/// Fields of the add-host form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddHostForm {
    pub alias: String,
    pub hostname: String,
    pub user: String,
    pub port: String,
}

const HOST_FIELDS: &[Field] = &[
    Field { key: "alias", label: "Alias", placeholder: "build" },
    Field { key: "hostname", label: "Hostname", placeholder: "build.internal" },
    Field { key: "user", label: "User", placeholder: "optional" },
    Field { key: "port", label: "Port", placeholder: "optional" },
];

impl AddHostForm {
    fn slot(&mut self, key: &str) -> Option<&mut String> {
        Some(match key {
            "alias" => &mut self.alias,
            "hostname" => &mut self.hostname,
            "user" => &mut self.user,
            "port" => &mut self.port,
            _ => return None,
        })
    }
}

impl Form for AddHostForm {
    fn fields(&self) -> &[Field] { HOST_FIELDS }
    fn get(&self, key: &str) -> String { self.clone().slot(key).cloned().unwrap_or_default() }
    fn set(&mut self, key: &str, value: &str) -> Result<(), String> { self.slot(key).map(|s| *s = value.into()).ok_or_else(|| format!("no field {key}")) }
}

impl FormSpec for AddHostForm {
    type Field = FieldId;
    type Output = RemoteHost;

    /// A host with the safe default limits: no paths, read only, no commands.
    fn validate(&self, existing: &[String]) -> Result<RemoteHost, Errors> {
        let [alias, hostname, user, port] = [&self.alias, &self.hostname, &self.user, &self.port].map(|s| s.trim());
        let word = |v: &str| if v.is_empty() { Err(FieldError::Empty) } else if v.contains(char::is_whitespace) { Err(FieldError::Whitespace) } else { Ok(()) };
        let alias_error = word(alias).err().or_else(|| existing.iter().any(|e| e == alias).then_some(FieldError::DuplicateHost));
        let port_value = (!port.is_empty()).then(|| parse_port(port)).transpose();
        let errors: Errors = [("alias", alias_error), ("hostname", word(hostname).err()), ("user", (!user.is_empty()).then(|| word(user).err()).flatten()), ("port", port_value.clone().err())]
            .into_iter().filter_map(|(k, e)| e.map(|e| (k, e.to_string()))).collect();
        if !errors.is_empty() { return Err(errors) }
        let limits = HostLimits { read_only: true, ..Default::default() };
        Ok(RemoteHost { alias: alias.into(), hostname: hostname.into(), user: (!user.is_empty()).then(|| user.into()), port: port_value.ok().flatten(), proxy_jump: None, limits })
    }
}

/// Edits to one host's limits before they are saved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LimitsDraft {
    pub host: String,
    pub limits: HostLimits,
    /// The text of the add-path input.
    pub new_path: String,
    pub path_error: Option<FieldError>,
}

impl LimitsDraft {
    pub fn new(host: &RemoteHost) -> Self { Self { host: host.alias.clone(), limits: host.limits.clone(), ..Default::default() } }

    /// Moves `new_path` into the allowed paths, or records why it can't.
    pub fn add_path(&mut self) {
        let path = self.new_path.trim();
        self.path_error = match path {
            "" => Some(FieldError::Empty),
            p if !(p.starts_with('/') || p.starts_with('~')) => Some(FieldError::RelativePath),
            p if self.limits.allowed_paths.iter().any(|a| a == p) => Some(FieldError::DuplicatePath),
            p => {
                self.limits.allowed_paths.push(p.to_owned());
                self.new_path.clear();
                None
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errors(list: &[(FieldId, FieldError)]) -> Errors { list.iter().map(|(k, e)| (*k, e.to_string())).collect() }

    #[test]
    fn server_forms_validate_per_field() {
        let form = |name: &str, kind, target: &str| AddServerForm { name: name.into(), kind, command: target.into(), url: target.into() };
        let existing = ["git".to_owned()];
        use FieldError::*;
        use TransportKind::{Http, Stdio};
        let bad: [(AddServerForm, Errors); 11] = [
            (form("fs", Stdio, "   "), errors(&[("command", Empty)])),
            (form("fs", Stdio, "npx 'oops"), errors(&[("command", Unbalanced)])),
            (form("fs", Stdio, "A=1 B=2"), errors(&[("command", NoCommand)])),
            (form("  ", Stdio, "x"), errors(&[("name", Empty)])),
            (form("my server", Stdio, "x"), errors(&[("name", BadName)])),
            (form("", Stdio, "'x"), errors(&[("name", Empty), ("command", Unbalanced)])),
            (form("git", Http, "localhost:3000"), errors(&[("name", DuplicateServer), ("url", BadScheme)])),
            (form("w", Http, "https:///mcp"), errors(&[("url", NoHost)])),
            (form("w", Http, "https://host:99999/"), errors(&[("url", BadPort)])),
            (form("w", Http, "https://host:/"), errors(&[("url", BadPort)])),
            (form("w", Http, "https://ho st/"), errors(&[("url", Whitespace)])),
        ];
        for (f, want) in bad { assert_eq!(f.validate(&existing).unwrap_err(), want, "{f:?}") }
        let spec = form("fs", Stdio, r#"API_KEY=a=b npx -y "@scope/server fs" '/tmp/my dir' a\ b"#).validate(&existing).unwrap();
        let McpTransport::Stdio { command, args, env } = spec.transport else { panic!("stdio") };
        assert_eq!((command.as_str(), args, env.get("API_KEY").map(String::as_str)), ("npx", vec!["-y".to_owned(), "@scope/server fs".into(), "/tmp/my dir".into(), "a b".into()], Some("a=b")));
        for url in [" https://mcp.example.com:8443/sse ", "HTTP://[::1]:3000/mcp", "http://user@localhost/mcp"] { assert!(form("w", Http, url).validate(&existing).is_ok(), "{url}") }
    }

    #[test]
    fn host_forms_and_limit_paths_validate() {
        let host = |a: &str, h: &str, u: &str, p: &str| AddHostForm { alias: a.into(), hostname: h.into(), user: u.into(), port: p.into() };
        let ok = host("build", "build.internal", "ci", "2222").validate(&["other".into()]).unwrap();
        assert_eq!((ok.port, ok.user.as_deref(), ok.limits.read_only, ok.limits.commands_allowed), (Some(2222), Some("ci"), true, false));
        use FieldError::*;
        let want = errors(&[("alias", DuplicateHost), ("hostname", Empty), ("user", Whitespace), ("port", BadPort)]);
        assert_eq!(host("build", "", "a b", "0").validate(&["build".into()]).unwrap_err(), want);
        let mut draft = LimitsDraft::new(&RemoteHost { alias: "h".into(), hostname: "h".into(), user: None, port: None, proxy_jump: None, limits: HostLimits { allowed_paths: vec!["/srv".into()], ..Default::default() } });
        for (path, error) in [("var/log", Some(RelativePath)), ("/srv", Some(DuplicatePath)), (" ~/work ", None)] {
            draft.new_path = path.into();
            draft.add_path();
            assert_eq!(draft.path_error, error);
        }
        assert_eq!((draft.limits.allowed_paths, draft.new_path), (vec!["/srv".to_owned(), "~/work".into()], String::new()));
    }
}
