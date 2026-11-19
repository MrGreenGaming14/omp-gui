//! Plain data describing a registered MCP server.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Headers the streamable HTTP client sets itself.
const RESERVED_HEADERS: &[&str] = &["accept", "mcp-session-id", "last-event-id", "mcp-protocol-version"];

/// One registered server, as the central config stores it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSpec {
    /// Unique key; tools are shown and addressed under it.
    pub name: String,
    /// A disabled server is kept in the config but never started.
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    #[serde(flatten)]
    pub transport: Transport,
}

fn enabled_default() -> bool {
    true
}

/// How the client reaches the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Transport {
    /// A child process speaking JSON-RPC lines on stdin and stdout.
    Stdio(StdioSpec),
    /// A streamable HTTP endpoint.
    Http(HttpSpec),
}

/// A child process server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StdioSpec {
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Added to the app's own environment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Working directory; the app's own when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
}

/// A streamable HTTP server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpSpec {
    pub url: String,
    /// Sent with every request, e.g. `Authorization`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

impl ServerSpec {
    pub fn new(name: impl Into<String>, transport: Transport) -> Result<Self> {
        let spec = Self { name: name.into(), enabled: true, transport };
        spec.validate()?;
        Ok(spec)
    }

    /// Checks the name, and the url and headers of an HTTP server. Specs that
    /// come from a store or an import go through this before they are used.
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.name)?;
        match &self.transport {
            Transport::Stdio(stdio) if stdio.command.trim().is_empty() => Err(Error::EmptyCommand),
            Transport::Stdio(_) => Ok(()),
            Transport::Http(http) => http.validate(),
        }
    }

    /// Whether both specs reach the same server, whatever their names.
    pub fn same_server(&self, other: &Self) -> bool {
        match (&self.transport, &other.transport) {
            (Transport::Stdio(a), Transport::Stdio(b)) => a.command == b.command && a.args == b.args && a.cwd == b.cwd,
            (Transport::Http(a), Transport::Http(b)) => same_url(&a.url, &b.url),
            _ => false,
        }
    }
}

fn same_url(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Server names key the config and prefix tool names in harnesses, so they
/// are limited to letters, digits, `-`, `_` and `.`.
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok { Ok(()) } else { Err(Error::InvalidName(name.to_owned())) }
}

impl StdioSpec {
    /// Parses a shell style command line such as
    /// `GITHUB_TOKEN=abc npx -y "@scope/server" --flag`. Leading
    /// `NAME=value` words become environment variables.
    pub fn parse(line: &str) -> Result<Self> {
        let words = shlex::split(line).ok_or(Error::UnclosedQuote)?;
        let mut words = words.into_iter().peekable();
        let mut env = BTreeMap::new();
        while let Some((name, value)) = words.peek().and_then(|word| env_assignment(word)) {
            env.insert(name.to_owned(), value.to_owned());
            words.next();
        }
        let command = words.next().ok_or(Error::EmptyCommand)?;
        Ok(Self { command, args: words.collect(), env, cwd: None })
    }

    /// The command line [`StdioSpec::parse`] reads back into this spec, for
    /// the edit form. `None` when a word holds a NUL byte.
    pub fn command_line(&self) -> Option<String> {
        let mut line = String::new();
        for (name, value) in &self.env {
            line.push_str(name);
            line.push('=');
            line.push_str(&shlex::try_quote(value).ok()?);
            line.push(' ');
        }
        let words = std::iter::once(self.command.as_str()).chain(self.args.iter().map(String::as_str));
        line.push_str(&shlex::try_join(words).ok()?);
        Some(line)
    }
}

/// `NAME=value` with a shell variable name.
fn env_assignment(word: &str) -> Option<(&str, &str)> {
    let (name, value) = word.split_once('=')?;
    let mut chars = name.chars();
    let first = chars.next()?;
    let valid = (first.is_ascii_alphabetic() || first == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then_some((name, value))
}

impl HttpSpec {
    /// An HTTP server at `url` with extra request headers.
    pub fn new(url: impl Into<String>, headers: impl IntoIterator<Item = (String, String)>) -> Result<Self> {
        let spec = Self { url: url.into().trim().to_owned(), headers: headers.into_iter().collect() };
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<()> {
        let bad = |reason: &str| Error::Url { url: self.url.clone(), reason: reason.to_owned() };
        let url = url::Url::parse(&self.url).map_err(|e| bad(&e.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(bad("the scheme must be http or https"));
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err(bad("there is no host"));
        }
        for (name, value) in &self.headers {
            let header = http::HeaderName::from_bytes(name.as_bytes()).map_err(|_| Error::Header(name.clone()))?;
            if RESERVED_HEADERS.contains(&header.as_str()) {
                return Err(Error::ReservedHeader(name.clone()));
            }
            http::HeaderValue::from_str(value).map_err(|_| Error::Header(format!("{name}: {value}")))?;
        }
        Ok(())
    }
}

/// Parses one `Name: value` header line from the add server form.
pub fn parse_header(line: &str) -> Result<(String, String)> {
    let (name, value) = line.split_once(':').ok_or_else(|| Error::Header(line.to_owned()))?;
    let (name, value) = (name.trim(), value.trim());
    if http::HeaderName::from_bytes(name.as_bytes()).is_err() || http::HeaderValue::from_str(value).is_err() {
        return Err(Error::Header(line.to_owned()));
    }
    Ok((name.to_owned(), value.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_splits_env_quotes_and_args() {
        let spec = StdioSpec::parse(r#"TOKEN=a\ b DEBUG=1 npx -y "@scope/server name" --dir='/tmp/x y'"#).unwrap();
        assert_eq!(spec.command, "npx");
        assert_eq!(spec.args, ["-y", "@scope/server name", "--dir=/tmp/x y"]);
        assert_eq!(spec.env.get("TOKEN").map(String::as_str), Some("a b"));
        assert_eq!(spec.env.get("DEBUG").map(String::as_str), Some("1"));
    }

    #[test]
    fn assignment_after_the_command_stays_an_argument() {
        let spec = StdioSpec::parse("server KEY=value").unwrap();
        assert_eq!(spec.command, "server");
        assert_eq!(spec.args, ["KEY=value"]);
        assert!(spec.env.is_empty());
    }

    #[test]
    fn bad_command_lines_are_rejected() {
        assert!(matches!(StdioSpec::parse("  "), Err(Error::EmptyCommand)));
        assert!(matches!(StdioSpec::parse("A=1"), Err(Error::EmptyCommand)));
        assert!(matches!(StdioSpec::parse("npx 'open"), Err(Error::UnclosedQuote)));
    }

    #[test]
    fn command_line_round_trips_for_editing() {
        let spec = StdioSpec::parse(r#"K='v w' run "a b" c"#).unwrap();
        assert_eq!(StdioSpec::parse(&spec.command_line().unwrap()).unwrap(), spec);
    }

    #[test]
    fn http_urls_and_headers_are_checked() {
        assert!(HttpSpec::new("https://mcp.example.com/mcp", []).is_ok());
        assert!(matches!(HttpSpec::new("ftp://example.com", []), Err(Error::Url { .. })));
        assert!(matches!(HttpSpec::new("not a url", []), Err(Error::Url { .. })));
        let reserved = [("Mcp-Session-Id".to_owned(), "x".to_owned())];
        assert!(matches!(HttpSpec::new("http://127.0.0.1:1/mcp", reserved), Err(Error::ReservedHeader(_))));
        let bad = [("Bad Header".to_owned(), "x".to_owned())];
        assert!(matches!(HttpSpec::new("http://127.0.0.1:1/mcp", bad), Err(Error::Header(_))));
    }

    #[test]
    fn header_lines_parse() {
        assert_eq!(
            parse_header("Authorization:  Bearer abc ").unwrap(),
            ("Authorization".to_owned(), "Bearer abc".to_owned())
        );
        assert!(parse_header("no colon").is_err());
    }

    #[test]
    fn names_are_restricted() {
        assert!(validate_name("github-mcp_2.0").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("has space").is_err());
        assert!(validate_name("a/b").is_err());
    }

    #[test]
    fn spec_serializes_flat_with_a_type_tag() {
        let spec = ServerSpec::new("fs", Transport::Stdio(StdioSpec::parse("mcp-fs /tmp").unwrap())).unwrap();
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json, serde_json::json!({"name": "fs", "enabled": true, "type": "stdio", "command": "mcp-fs", "args": ["/tmp"]}));
        assert_eq!(serde_json::from_value::<ServerSpec>(json).unwrap(), spec);
    }
}
