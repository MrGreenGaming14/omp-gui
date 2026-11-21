//! The add-server, add-host and host limits forms: kit [`Form`]s for editing, [`FormSpec`]s for validation.

use super::model::{HostLimits, RemoteHost};
use loa_contracts::harness::{McpServerSpec, McpTransport};
use loa_ui_kit::widgets::{Field, Form, FormSpec};
use std::collections::BTreeMap;

/// A field of a tools form, by its key.
pub type FieldId = &'static str;
type Errors = BTreeMap<FieldId, String>;

// Why a field was rejected; the text shows under the field.
pub const EMPTY: &str = "required";
pub const BAD_NAME: &str = "use letters, digits, '-' or '_'";
pub const DUPLICATE_SERVER: &str = "a server with this name already exists";
pub const DUPLICATE_HOST: &str = "a host with this alias already exists";
pub const UNBALANCED: &str = "unbalanced quote";
pub const NO_COMMAND: &str = "only environment assignments, no command";
pub const BAD_SCHEME: &str = "URL must start with http:// or https://";
pub const NO_HOST: &str = "URL has no host";
pub const WHITESPACE: &str = "must not contain spaces";
pub const BAD_PORT: &str = "port must be a number from 1 to 65535";
pub const RELATIVE_PATH: &str = "path must start with '/' or '~'";
pub const DUPLICATE_PATH: &str = "path is already allowed";

/// Text values by field key, settable for any of `fields`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Values(BTreeMap<FieldId, String>);

impl Values {
    fn get(&self, key: &str) -> &str { self.0.get(key).map_or("", |v| v.trim()) }
    fn set(&mut self, fields: &[Field], key: &str, value: &str) -> Result<(), String> {
        let field = fields.iter().find(|f| f.key == key).ok_or_else(|| format!("no field {key}"))?;
        self.0.insert(field.key, value.into());
        Ok(())
    }
}

/// How a server being added is reached.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransportKind { #[default] Stdio, Http }
/// The add-server form; `command` is a stdio command line with optional leading `KEY=value` assignments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddServerForm { pub kind: TransportKind, pub values: Values }

const NAME: Field = Field { key: "name", label: "Name", placeholder: "my-server" };
const COMMAND: Field = Field { key: "command", label: "Command", placeholder: "npx -y @scope/server" };
const URL: Field = Field { key: "url", label: "URL", placeholder: "https://host/mcp" };

impl Form for AddServerForm {
    fn fields(&self) -> &[Field] { if self.kind == TransportKind::Stdio { &[NAME, COMMAND] } else { &[NAME, URL] } }
    fn get(&self, key: &str) -> String { self.values.0.get(key).cloned().unwrap_or_default() }
    fn set(&mut self, key: &str, value: &str) -> Result<(), String> { self.values.set(&[NAME, COMMAND, URL], key, value) }
}

impl FormSpec for AddServerForm {
    type Field = FieldId;
    type Output = McpServerSpec;

    /// Checks the name against `existing` ones and builds the spec.
    fn validate(&self, existing: &[String]) -> Result<McpServerSpec, Errors> {
        let name = self.values.get("name");
        let name_error = match name {
            "" => Some(EMPTY),
            n if !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') => Some(BAD_NAME),
            n if existing.iter().any(|e| e == n) => Some(DUPLICATE_SERVER),
            _ => None,
        };
        let transport = match self.kind {
            TransportKind::Stdio => parse_stdio(self.values.get("command")).map_err(|e| ("command", e)),
            TransportKind::Http => validate_url(self.values.get("url")).map(|url| McpTransport::Http { url: url.into(), headers: BTreeMap::new() }).map_err(|e| ("url", e)),
        };
        match (name_error, transport) {
            (None, Ok(transport)) => Ok(McpServerSpec { name: name.into(), transport }),
            (name, target) => Err(name.map(|e| ("name", e)).into_iter().chain(target.err()).map(|(k, e)| (k, e.to_owned())).collect()),
        }
    }
}

/// A shell-split command line: leading `KEY=value` words become the environment.
fn parse_stdio(line: &str) -> Result<McpTransport, &'static str> {
    let mut words = shell_words::split(line).map_err(|_| UNBALANCED)?.into_iter().peekable();
    let is_env = |k: &str| k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let mut env = BTreeMap::new();
    while let Some(word) = words.next_if(|w| w.split_once('=').is_some_and(|(k, _)| is_env(k))) {
        let (k, v) = word.split_once('=').unwrap_or_default();
        env.insert(k.to_owned(), v.to_owned());
    }
    let command = words.next().ok_or(if env.is_empty() { EMPTY } else { NO_COMMAND })?;
    Ok(McpTransport::Stdio { command, args: words.collect(), env })
}

/// An HTTP(S) server URL.
fn validate_url(url: &str) -> Result<&str, &'static str> {
    if url.is_empty() { return Err(EMPTY) }
    if url.contains(char::is_whitespace) { return Err(WHITESPACE) }
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let rest = ["https://", "http://"].iter().find(|p| lower.starts_with(**p)).map(|p| &url[p.len()..]).ok_or(BAD_SCHEME)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = match host_port.strip_prefix('[') {
        Some(v6) => v6.split_once(']').map(|(h, after)| (h, after.strip_prefix(':'))).ok_or(NO_HOST)?,
        None => host_port.split_once(':').map_or((host_port, None), |(h, p)| (h, Some(p))),
    };
    if host.is_empty() { return Err(NO_HOST) }
    port.map(parse_port).transpose().map(|_| url)
}

fn parse_port(port: &str) -> Result<u16, &'static str> { port.parse::<u16>().ok().filter(|&p| p != 0).ok_or(BAD_PORT) }
/// The add-host form: alias, hostname, and optional user and port.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddHostForm(pub Values);
const HOST_FIELDS: &[Field] = &[
    Field { key: "alias", label: "Alias", placeholder: "build" }, Field { key: "hostname", label: "Hostname", placeholder: "build.internal" },
    Field { key: "user", label: "User", placeholder: "optional" }, Field { key: "port", label: "Port", placeholder: "optional" },
];
impl Form for AddHostForm {
    fn fields(&self) -> &[Field] { HOST_FIELDS }
    fn get(&self, key: &str) -> String { self.0.0.get(key).cloned().unwrap_or_default() }
    fn set(&mut self, key: &str, value: &str) -> Result<(), String> { self.0.set(HOST_FIELDS, key, value) }
}

impl FormSpec for AddHostForm {
    type Field = FieldId;
    type Output = RemoteHost;

    /// A host with the safe default limits: no paths, read only, no commands.
    fn validate(&self, existing: &[String]) -> Result<RemoteHost, Errors> {
        let [alias, hostname, user, port] = ["alias", "hostname", "user", "port"].map(|k| self.0.get(k));
        let word = |v: &str| if v.is_empty() { Some(EMPTY) } else { v.contains(char::is_whitespace).then_some(WHITESPACE) };
        let port_value = (!port.is_empty()).then(|| parse_port(port)).transpose();
        let alias_error = word(alias).or_else(|| existing.iter().any(|e| e == alias).then_some(DUPLICATE_HOST));
        let errors: Errors = [("alias", alias_error), ("hostname", word(hostname)), ("user", word(user).filter(|_| !user.is_empty())), ("port", port_value.err())]
            .into_iter().filter_map(|(k, e)| Some((k, e?.to_owned()))).collect();
        if !errors.is_empty() { return Err(errors) }
        let limits = HostLimits { read_only: true, ..Default::default() };
        Ok(RemoteHost { alias: alias.into(), hostname: hostname.into(), user: (!user.is_empty()).then(|| user.into()), port: port_value.ok().flatten(), proxy_jump: None, limits })
    }
}

/// Edits to one host's limits before they are saved; `new_path` is the add-path input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LimitsDraft { pub host: String, pub limits: HostLimits, pub new_path: String, pub path_error: Option<&'static str> }
impl LimitsDraft {
    pub fn new(host: &RemoteHost) -> Self { Self { host: host.alias.clone(), limits: host.limits.clone(), ..Default::default() } }
    /// Moves `new_path` into the allowed paths, or records why it can't.
    pub fn add_path(&mut self) {
        let path = self.new_path.trim();
        self.path_error = match path {
            "" => Some(EMPTY),
            p if !(p.starts_with('/') || p.starts_with('~')) => Some(RELATIVE_PATH),
            p if self.limits.allowed_paths.iter().any(|a| a == p) => Some(DUPLICATE_PATH),
            p => {
                self.limits.allowed_paths.push(p.to_owned());
                self.new_path.clear();
                None
            }
        };
    }
}
