//! Input forms of the panel and their validation.

use std::collections::BTreeMap;

use loa_contracts::harness::{McpServerSpec, McpTransport};

use crate::model::{HostLimits, RemoteHost};

/// Why a form field was rejected. The `Display` text is shown under the field.
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
    #[error("unterminated {0} quote")]
    UnterminatedQuote(char),
    #[error("trailing backslash")]
    TrailingBackslash,
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

/// How a server being added is reached.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransportKind {
    #[default]
    Stdio,
    Http,
}

/// The "add server" form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddServerForm {
    pub name: String,
    pub kind: TransportKind,
    /// Command line for stdio, with optional leading `KEY=value` assignments.
    pub command: String,
    pub url: String,
}

/// Per field errors of [`AddServerForm`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddServerErrors {
    pub name: Option<FieldError>,
    /// Error on the command or the URL, whichever the transport uses.
    pub target: Option<FieldError>,
}

impl AddServerForm {
    /// Checks the form against the names already registered and builds the spec.
    pub fn validate<'a>(
        &self,
        mut existing: impl Iterator<Item = &'a str>,
    ) -> Result<McpServerSpec, AddServerErrors> {
        let name = self.name.trim();
        let name_error = if name.is_empty() {
            Some(FieldError::Empty)
        } else if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            Some(FieldError::BadName)
        } else if existing.any(|other| other == name) {
            Some(FieldError::DuplicateServer)
        } else {
            None
        };
        let transport = match self.kind {
            TransportKind::Stdio => parse_stdio(&self.command),
            TransportKind::Http => validate_url(&self.url).map(|url| McpTransport::Http {
                url: url.to_owned(),
                headers: BTreeMap::new(),
            }),
        };
        match (name_error, transport) {
            (None, Ok(transport)) => Ok(McpServerSpec {
                name: name.to_owned(),
                transport,
            }),
            (name, target) => Err(AddServerErrors {
                name,
                target: target.err(),
            }),
        }
    }
}

/// Splits a stdio command line into env assignments, command and args.
fn parse_stdio(line: &str) -> Result<McpTransport, FieldError> {
    let words = split_words(line)?;
    let mut env = BTreeMap::new();
    let mut words = words.into_iter().peekable();
    while let Some(word) = words.peek() {
        let Some((key, value)) = word.split_once('=') else {
            break;
        };
        if !is_env_name(key) {
            break;
        }
        env.insert(key.to_owned(), value.to_owned());
        words.next();
    }
    let Some(command) = words.next() else {
        return Err(if env.is_empty() {
            FieldError::Empty
        } else {
            FieldError::NoCommand
        });
    };
    Ok(McpTransport::Stdio {
        command,
        args: words.collect(),
        env,
    })
}

fn is_env_name(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Shell style word splitting: whitespace separates, single quotes are
/// literal, double quotes allow `\"` and `\\`, a backslash outside quotes
/// escapes the next character.
pub fn split_words(line: &str) -> Result<Vec<String>, FieldError> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err(FieldError::UnterminatedQuote('\'')),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\')) => current.push(c),
                            Some(c) => {
                                current.push('\\');
                                current.push(c);
                            }
                            None => return Err(FieldError::UnterminatedQuote('"')),
                        },
                        Some(c) => current.push(c),
                        None => return Err(FieldError::UnterminatedQuote('"')),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(c) => current.push(c),
                    None => return Err(FieldError::TrailingBackslash),
                }
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

/// Checks an HTTP(S) server URL and returns it trimmed.
pub fn validate_url(url: &str) -> Result<&str, FieldError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(FieldError::Empty);
    }
    if url.chars().any(char::is_whitespace) {
        return Err(FieldError::Whitespace);
    }
    let rest = strip_prefix_ignore_case(url, "https://")
        .or_else(|| strip_prefix_ignore_case(url, "http://"))
        .ok_or(FieldError::BadScheme)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let (host, port) = if let Some(bracketed) = host_port.strip_prefix('[') {
        match bracketed.split_once(']') {
            Some((host, after)) => (host, after.strip_prefix(':')),
            None => return Err(FieldError::NoHost),
        }
    } else {
        match host_port.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_port, None),
        }
    };
    if host.is_empty() {
        return Err(FieldError::NoHost);
    }
    if let Some(port) = port {
        parse_port(port)?;
    }
    Ok(url)
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

fn parse_port(port: &str) -> Result<u16, FieldError> {
    match port.parse::<u16>() {
        Ok(0) | Err(_) => Err(FieldError::BadPort),
        Ok(port) => Ok(port),
    }
}

/// The "add host by hand" form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddHostForm {
    pub alias: String,
    pub hostname: String,
    pub user: String,
    pub port: String,
}

/// Per field errors of [`AddHostForm`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddHostErrors {
    pub alias: Option<FieldError>,
    pub hostname: Option<FieldError>,
    pub user: Option<FieldError>,
    pub port: Option<FieldError>,
}

impl AddHostForm {
    /// Builds the host with the safe default limits: no paths, read only, no commands.
    pub fn validate<'a>(
        &self,
        mut existing: impl Iterator<Item = &'a str>,
    ) -> Result<RemoteHost, AddHostErrors> {
        let alias = self.alias.trim();
        let hostname = self.hostname.trim();
        let user = self.user.trim();
        let port = self.port.trim();
        let mut errors = AddHostErrors {
            alias: single_word(alias).err(),
            hostname: single_word(hostname).err(),
            user: if user.is_empty() {
                None
            } else {
                single_word(user).err()
            },
            port: None,
        };
        if errors.alias.is_none() && existing.any(|other| other == alias) {
            errors.alias = Some(FieldError::DuplicateHost);
        }
        let port = if port.is_empty() {
            None
        } else {
            match parse_port(port) {
                Ok(port) => Some(port),
                Err(error) => {
                    errors.port = Some(error);
                    None
                }
            }
        };
        if errors != AddHostErrors::default() {
            return Err(errors);
        }
        Ok(RemoteHost {
            alias: alias.to_owned(),
            hostname: hostname.to_owned(),
            user: (!user.is_empty()).then(|| user.to_owned()),
            port,
            proxy_jump: None,
            limits: HostLimits {
                allowed_paths: Vec::new(),
                read_only: true,
                commands_allowed: false,
                auto_approve: false,
            },
        })
    }
}

fn single_word(value: &str) -> Result<(), FieldError> {
    if value.is_empty() {
        Err(FieldError::Empty)
    } else if value.chars().any(char::is_whitespace) {
        Err(FieldError::Whitespace)
    } else {
        Ok(())
    }
}

/// Edits to one host's limits before they are saved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LimitsDraft {
    pub host: String,
    pub limits: HostLimits,
    /// Text of the "add path" input.
    pub new_path: String,
    pub path_error: Option<FieldError>,
}

impl LimitsDraft {
    pub fn new(host: &RemoteHost) -> Self {
        Self {
            host: host.alias.clone(),
            limits: host.limits.clone(),
            new_path: String::new(),
            path_error: None,
        }
    }

    /// Moves `new_path` into the allowed paths, or records why it can't be.
    pub fn add_path(&mut self) {
        let path = self.new_path.trim();
        self.path_error = if path.is_empty() {
            Some(FieldError::Empty)
        } else if !(path.starts_with('/') || path.starts_with('~')) {
            Some(FieldError::RelativePath)
        } else if self.limits.allowed_paths.iter().any(|p| p == path) {
            Some(FieldError::DuplicatePath)
        } else {
            self.limits.allowed_paths.push(path.to_owned());
            self.new_path.clear();
            None
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(command: &str) -> AddServerForm {
        AddServerForm {
            name: "fs".into(),
            command: command.into(),
            ..Default::default()
        }
    }

    fn http(url: &str) -> AddServerForm {
        AddServerForm {
            name: "web".into(),
            kind: TransportKind::Http,
            url: url.into(),
            ..Default::default()
        }
    }

    #[test]
    fn stdio_command_splits_quotes_and_env() {
        let spec = stdio(r#"API_KEY=a=b npx -y "@scope/server fs" '/tmp/my dir' a\ b"#)
            .validate(std::iter::empty())
            .unwrap();
        let McpTransport::Stdio { command, args, env } = spec.transport else {
            panic!("stdio")
        };
        assert_eq!(command, "npx");
        assert_eq!(args, ["-y", "@scope/server fs", "/tmp/my dir", "a b"]);
        assert_eq!(env.get("API_KEY").map(String::as_str), Some("a=b"));
    }

    #[test]
    fn stdio_command_errors_are_reported_on_the_target_field() {
        let errors = |cmd: &str| stdio(cmd).validate(std::iter::empty()).unwrap_err().target;
        assert_eq!(errors("   "), Some(FieldError::Empty));
        assert_eq!(
            errors("npx 'oops"),
            Some(FieldError::UnterminatedQuote('\''))
        );
        assert_eq!(
            errors("npx \"oops"),
            Some(FieldError::UnterminatedQuote('"'))
        );
        assert_eq!(errors("npx oops\\"), Some(FieldError::TrailingBackslash));
        assert_eq!(errors("A=1 B=2"), Some(FieldError::NoCommand));
    }

    #[test]
    fn name_must_be_valid_and_unique() {
        let mut form = stdio("server");
        form.name = "  ".into();
        assert_eq!(
            form.validate(std::iter::empty()).unwrap_err().name,
            Some(FieldError::Empty)
        );
        form.name = "my server".into();
        assert_eq!(
            form.validate(std::iter::empty()).unwrap_err().name,
            Some(FieldError::BadName)
        );
        form.name = "fs".into();
        let errors = form.validate(["git", "fs"].into_iter()).unwrap_err();
        assert_eq!(errors.name, Some(FieldError::DuplicateServer));
        assert_eq!(errors.target, None);
    }

    #[test]
    fn both_fields_report_at_once() {
        let form = AddServerForm {
            name: String::new(),
            command: "'x".into(),
            ..Default::default()
        };
        let errors = form.validate(std::iter::empty()).unwrap_err();
        assert_eq!(errors.name, Some(FieldError::Empty));
        assert_eq!(errors.target, Some(FieldError::UnterminatedQuote('\'')));
    }

    #[test]
    fn http_url_validation() {
        let ok = |url: &str| http(url).validate(std::iter::empty()).map(|s| s.transport);
        assert_eq!(
            ok(" https://mcp.example.com:8443/sse "),
            Ok(McpTransport::Http {
                url: "https://mcp.example.com:8443/sse".into(),
                headers: BTreeMap::new()
            })
        );
        assert!(ok("HTTP://[::1]:3000/mcp").is_ok());
        assert!(ok("http://user@localhost/mcp").is_ok());
        let err = |url: &str| http(url).validate(std::iter::empty()).unwrap_err().target;
        assert_eq!(err(""), Some(FieldError::Empty));
        assert_eq!(err("ftp://example.com"), Some(FieldError::BadScheme));
        assert_eq!(err("example.com/mcp"), Some(FieldError::BadScheme));
        assert_eq!(err("https:///mcp"), Some(FieldError::NoHost));
        assert_eq!(err("https://host:99999/"), Some(FieldError::BadPort));
        assert_eq!(err("https://host:/"), Some(FieldError::BadPort));
        assert_eq!(err("https://ho st/"), Some(FieldError::Whitespace));
    }

    #[test]
    fn host_form_defaults_to_locked_down_limits() {
        let form = AddHostForm {
            alias: "build".into(),
            hostname: "build.internal".into(),
            user: "ci".into(),
            port: "2222".into(),
        };
        let host = form.validate(["other"].into_iter()).unwrap();
        assert_eq!(host.port, Some(2222));
        assert_eq!(host.user.as_deref(), Some("ci"));
        assert!(host.limits.read_only);
        assert!(!host.limits.commands_allowed);
        assert!(host.limits.allowed_paths.is_empty());
    }

    #[test]
    fn host_form_errors() {
        let form = AddHostForm {
            alias: "build".into(),
            hostname: "".into(),
            user: "a b".into(),
            port: "0".into(),
        };
        let errors = form.validate(["build"].into_iter()).unwrap_err();
        assert_eq!(errors.alias, Some(FieldError::DuplicateHost));
        assert_eq!(errors.hostname, Some(FieldError::Empty));
        assert_eq!(errors.user, Some(FieldError::Whitespace));
        assert_eq!(errors.port, Some(FieldError::BadPort));
    }

    #[test]
    fn limits_draft_only_accepts_new_absolute_paths() {
        let host = RemoteHost {
            alias: "h".into(),
            hostname: "h".into(),
            user: None,
            port: None,
            proxy_jump: None,
            limits: HostLimits {
                allowed_paths: vec!["/srv".into()],
                ..Default::default()
            },
        };
        let mut draft = LimitsDraft::new(&host);
        draft.new_path = "var/log".into();
        draft.add_path();
        assert_eq!(draft.path_error, Some(FieldError::RelativePath));
        draft.new_path = "/srv".into();
        draft.add_path();
        assert_eq!(draft.path_error, Some(FieldError::DuplicatePath));
        draft.new_path = " ~/work ".into();
        draft.add_path();
        assert_eq!(draft.path_error, None);
        assert_eq!(draft.limits.allowed_paths, ["/srv", "~/work"]);
        assert!(draft.new_path.is_empty());
    }
}
