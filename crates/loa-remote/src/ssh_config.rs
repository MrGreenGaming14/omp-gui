//! `~/.ssh/config` import (#95).
//!
//! Supports `Host` blocks with wildcard and negated patterns, `HostName`,
//! `User`, `Port`, `IdentityAgent`, `ProxyJump` (chains, resolved through the
//! config recursively) and `Include` (globs, relative to the top level
//! config's directory). `Match` blocks and `ProxyCommand` are not evaluated:
//! they are reported per host as [`Unsupported`] so the UI can show them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// The longest ProxyJump chain the resolver follows.
pub const MAX_JUMPS: usize = 16;

/// How deep `Include` may nest.
const MAX_INCLUDE_DEPTH: usize = 16;

/// Default SSH port.
pub const DEFAULT_PORT: u16 = 22;

/// Where a line of the config came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub file: PathBuf,
    pub line: usize,
}

/// A config line that applies (or may apply) to a host but is not acted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    pub location: Location,
    pub text: String,
}

/// Which `ssh-agent` socket authenticates a hop.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AgentSocket {
    /// The socket the app was given (normally `SSH_AUTH_SOCK`).
    #[default]
    Default,
    /// `IdentityAgent <path>`.
    Path(PathBuf),
    /// `IdentityAgent none`: no agent, so no way to authenticate.
    Disabled,
}

/// One SSH hop: where to connect and as whom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The name the hop was referred to by (config alias or host name).
    pub alias: String,
    pub hostname: String,
    pub port: u16,
    pub user: String,
    pub agent: AgentSocket,
}

impl Endpoint {
    /// `host` or `[host]:port`, the form used by `known_hosts`.
    pub fn known_hosts_name(&self) -> String {
        crate::known_hosts::host_name(&self.hostname, self.port)
    }
}

/// A host with its ProxyJump chain fully resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHost {
    pub alias: String,
    pub target: Endpoint,
    /// Hops in connection order: connect to `jumps[0]` first.
    pub jumps: Vec<Endpoint>,
    /// The `Host` line that named this alias, when it came from a file.
    pub location: Option<Location>,
    pub unsupported: Vec<Unsupported>,
}

/// One concrete alias of the config and the outcome of resolving it.
#[derive(Debug)]
pub struct Imported {
    pub alias: String,
    pub host: Result<ResolvedHost>,
}

#[derive(Debug, Clone)]
struct HostPattern {
    negated: bool,
    pattern: String,
}

#[derive(Debug, Clone)]
enum Condition {
    All,
    Host(Vec<HostPattern>),
    Match,
}

#[derive(Debug)]
struct Directive {
    key: String,
    args: Vec<String>,
    location: Location,
}

#[derive(Debug)]
struct Block {
    condition: Condition,
    location: Location,
    directives: Vec<Directive>,
}

/// A parsed ssh config, including everything it `Include`s.
#[derive(Debug)]
pub struct SshConfig {
    blocks: Vec<Block>,
    home: PathBuf,
}

#[derive(Default)]
struct Options<'a> {
    hostname: Option<&'a Directive>,
    user: Option<&'a Directive>,
    port: Option<&'a Directive>,
    identity_agent: Option<&'a Directive>,
    proxy_jump: Option<&'a Directive>,
    unsupported: Vec<Unsupported>,
}

/// Parses `path` and resolves every concrete alias in it.
///
/// `home` expands `~`; `default_user` fills hops with no `User`.
pub fn import_hosts(path: &Path, home: &Path, default_user: &str) -> Result<Vec<Imported>> {
    let config = SshConfig::load(path, home)?;
    Ok(config
        .aliases()
        .into_iter()
        .map(|alias| {
            let host = config.resolve(&alias, default_user);
            Imported { alias, host }
        })
        .collect())
}

impl SshConfig {
    /// Reads `path` and its includes. `home` expands `~`.
    pub fn load(path: &Path, home: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text, path, home)
    }

    /// Parses config text that was read from `file` (used for locations and
    /// as the base directory of relative includes).
    pub fn parse(text: &str, file: &Path, home: &Path) -> Result<Self> {
        let base = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut parser = Parser {
            blocks: vec![Block {
                condition: Condition::All,
                location: Location {
                    file: file.to_path_buf(),
                    line: 0,
                },
                directives: Vec::new(),
            }],
            base,
            home: home.to_path_buf(),
        };
        parser.parse_text(text, file, 0)?;
        Ok(Self {
            blocks: parser.blocks,
            home: home.to_path_buf(),
        })
    }

    /// Concrete aliases (no wildcards, not negated) in file order.
    pub fn aliases(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for block in &self.blocks {
            if let Condition::Host(patterns) = &block.condition {
                for p in patterns {
                    if !p.negated
                        && !p.pattern.contains(['*', '?'])
                        && seen.insert(p.pattern.clone())
                    {
                        out.push(p.pattern.clone());
                    }
                }
            }
        }
        out
    }

    /// Resolves `alias` and its ProxyJump chain.
    pub fn resolve(&self, alias: &str, default_user: &str) -> Result<ResolvedHost> {
        let mut stack = Vec::new();
        let (target, jumps, mut unsupported) =
            self.resolve_chain(alias, None, None, default_user, &mut stack)?;
        let mut seen = Vec::with_capacity(unsupported.len());
        unsupported.retain(|u| {
            let new = !seen.contains(u);
            if new {
                seen.push(u.clone());
            }
            new
        });
        Ok(ResolvedHost {
            alias: alias.to_string(),
            target,
            jumps,
            location: self.host_location(alias),
            unsupported,
        })
    }

    fn host_location(&self, alias: &str) -> Option<Location> {
        let alias = alias.to_ascii_lowercase();
        self.blocks.iter().find_map(|b| match &b.condition {
            Condition::Host(patterns) if patterns.iter().any(|p| !p.negated && p.pattern == alias) => {
                Some(b.location.clone())
            }
            _ => None,
        })
    }

    fn resolve_chain(
        &self,
        alias: &str,
        user_override: Option<&str>,
        port_override: Option<u16>,
        default_user: &str,
        stack: &mut Vec<String>,
    ) -> Result<(Endpoint, Vec<Endpoint>, Vec<Unsupported>)> {
        let key = alias.to_ascii_lowercase();
        if stack.contains(&key) {
            return Err(Error::ProxyJumpCycle(stack[0].clone()));
        }
        if stack.len() > MAX_JUMPS {
            return Err(Error::ProxyJumpTooDeep(stack[0].clone()));
        }
        let mut options = self.options(alias);
        let target = self.endpoint(alias, &options, user_override, port_override, default_user)?;
        let mut jumps = Vec::new();
        let mut unsupported = std::mem::take(&mut options.unsupported);
        if let Some(directive) = options.proxy_jump {
            let list = directive.args.first().map(String::as_str).unwrap_or("none");
            if !list.eq_ignore_ascii_case("none") {
                let specs = list
                    .split(',')
                    .map(|s| parse_jump(s.trim(), &directive.location))
                    .collect::<Result<Vec<_>>>()?;
                let mut specs = specs.into_iter();
                if let Some(first) = specs.next() {
                    stack.push(key.clone());
                    let (first_target, first_jumps, first_unsupported) = self.resolve_chain(
                        &first.host,
                        first.user.as_deref(),
                        first.port,
                        default_user,
                        stack,
                    )?;
                    stack.pop();
                    jumps.extend(first_jumps);
                    jumps.push(first_target);
                    unsupported.extend(first_unsupported);
                }
                for spec in specs {
                    let opts = self.options(&spec.host);
                    jumps.push(self.endpoint(
                        &spec.host,
                        &opts,
                        spec.user.as_deref(),
                        spec.port,
                        default_user,
                    )?);
                    unsupported.extend(opts.unsupported);
                }
                if jumps.len() > MAX_JUMPS {
                    return Err(Error::ProxyJumpTooDeep(alias.to_string()));
                }
            }
        }
        Ok((target, jumps, unsupported))
    }

    /// First value wins, in file order, over every block matching `alias`.
    fn options(&self, alias: &str) -> Options<'_> {
        let host = alias.to_ascii_lowercase();
        let mut o = Options::default();
        for block in &self.blocks {
            match &block.condition {
                Condition::Match => {
                    o.unsupported.push(Unsupported {
                        location: block.location.clone(),
                        text: "Match blocks are not evaluated".to_string(),
                    });
                    continue;
                }
                Condition::Host(patterns) if !host_matches(patterns, &host) => continue,
                _ => {}
            }
            for d in &block.directives {
                let slot = match d.key.as_str() {
                    "hostname" => &mut o.hostname,
                    "user" => &mut o.user,
                    "port" => &mut o.port,
                    "identityagent" => &mut o.identity_agent,
                    "proxyjump" => &mut o.proxy_jump,
                    "proxycommand" => {
                        if o.proxy_jump.is_none() {
                            o.unsupported.push(Unsupported {
                                location: d.location.clone(),
                                text: format!("ProxyCommand {} is not run", d.args.join(" ")),
                            });
                        }
                        continue;
                    }
                    _ => continue,
                };
                if slot.is_none() {
                    *slot = Some(d);
                }
            }
        }
        o
    }

    fn endpoint(
        &self,
        alias: &str,
        o: &Options<'_>,
        user_override: Option<&str>,
        port_override: Option<u16>,
        default_user: &str,
    ) -> Result<Endpoint> {
        let hostname = match o.hostname.and_then(|d| d.args.first()) {
            Some(h) => expand_host_tokens(h, alias),
            None => alias.to_string(),
        };
        let port = match port_override {
            Some(p) => p,
            None => match o.port {
                Some(d) => parse_port(d.args.first().map(String::as_str), &d.location)?,
                None => DEFAULT_PORT,
            },
        };
        let user = user_override
            .map(str::to_string)
            .or_else(|| o.user.and_then(|d| d.args.first().cloned()))
            .unwrap_or_else(|| default_user.to_string());
        let agent = match o.identity_agent.and_then(|d| d.args.first()) {
            None => AgentSocket::Default,
            Some(v) if v.eq_ignore_ascii_case("none") => AgentSocket::Disabled,
            Some(v) if v == "SSH_AUTH_SOCK" || v == "$SSH_AUTH_SOCK" => AgentSocket::Default,
            Some(v) => AgentSocket::Path(expand_tilde(v, &self.home)),
        };
        Ok(Endpoint {
            alias: alias.to_string(),
            hostname,
            port,
            user,
            agent,
        })
    }
}

struct Parser {
    blocks: Vec<Block>,
    base: PathBuf,
    home: PathBuf,
}

impl Parser {
    fn parse_text(&mut self, text: &str, file: &Path, depth: usize) -> Result<()> {
        for (index, raw) in text.lines().enumerate() {
            let location = Location {
                file: file.to_path_buf(),
                line: index + 1,
            };
            let Some((key, args)) = tokenize(raw, &location)? else {
                continue;
            };
            match key.as_str() {
                "host" => {
                    let patterns = args
                        .iter()
                        .map(|a| match a.strip_prefix('!') {
                            Some(p) => HostPattern {
                                negated: true,
                                pattern: p.to_ascii_lowercase(),
                            },
                            None => HostPattern {
                                negated: false,
                                pattern: a.to_ascii_lowercase(),
                            },
                        })
                        .collect();
                    self.blocks.push(Block {
                        condition: Condition::Host(patterns),
                        location,
                        directives: Vec::new(),
                    });
                }
                "match" => self.blocks.push(Block {
                    condition: Condition::Match,
                    location,
                    directives: Vec::new(),
                }),
                "include" => {
                    if depth >= MAX_INCLUDE_DEPTH {
                        return Err(Error::SshConfig {
                            file: location.file,
                            line: location.line,
                            message: "Include nests too deep".to_string(),
                        });
                    }
                    let condition = self.current_condition();
                    for pattern in &args {
                        let pattern = expand_tilde(pattern, &self.home);
                        let pattern = if pattern.is_absolute() {
                            pattern
                        } else {
                            self.base.join(pattern)
                        };
                        for path in expand_glob(&pattern) {
                            let text =
                                std::fs::read_to_string(&path).map_err(|source| Error::Io {
                                    path: path.clone(),
                                    source,
                                })?;
                            self.parse_text(&text, &path, depth + 1)?;
                        }
                    }
                    // Lines after the Include stay under the block it sat in.
                    self.blocks.push(Block {
                        condition,
                        location,
                        directives: Vec::new(),
                    });
                }
                _ => {
                    if let Some(block) = self.blocks.last_mut() {
                        block.directives.push(Directive {
                            key,
                            args,
                            location,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn current_condition(&self) -> Condition {
        self.blocks
            .last()
            .map(|b| b.condition.clone())
            .unwrap_or(Condition::All)
    }
}

/// Splits a config line into a lowercase keyword and its arguments.
fn tokenize(line: &str, location: &Location) -> Result<Option<(String, Vec<String>)>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let split = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let key = line[..split].to_ascii_lowercase();
    let mut rest = line[split..].trim_start();
    if let Some(r) = rest.strip_prefix('=') {
        rest = r.trim_start();
    }
    let mut args = Vec::new();
    let mut chars = rest.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '#' {
            break;
        }
        let mut arg = String::new();
        if c == '"' {
            chars.next();
            let mut closed = false;
            for c in chars.by_ref() {
                if c == '"' {
                    closed = true;
                    break;
                }
                arg.push(c);
            }
            if !closed {
                return Err(Error::SshConfig {
                    file: location.file.clone(),
                    line: location.line,
                    message: "unterminated quote".to_string(),
                });
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                arg.push(c);
                chars.next();
            }
        }
        args.push(arg);
    }
    Ok(Some((key, args)))
}

fn host_matches(patterns: &[HostPattern], host: &str) -> bool {
    let mut matched = false;
    for p in patterns {
        if wildcard_match(&p.pattern, host) {
            if p.negated {
                return false;
            }
            matched = true;
        }
    }
    matched
}

/// `*` and `?` glob match, as OpenSSH's `match_pattern`.
pub(crate) fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

struct JumpSpec {
    host: String,
    user: Option<String>,
    port: Option<u16>,
}

fn parse_jump(spec: &str, location: &Location) -> Result<JumpSpec> {
    let bad = |message: &str| Error::SshConfig {
        file: location.file.clone(),
        line: location.line,
        message: format!("ProxyJump `{spec}`: {message}"),
    };
    let s = spec.strip_prefix("ssh://").unwrap_or(spec);
    let (user, hostport) = match s.rsplit_once('@') {
        Some((u, h)) => (Some(u.to_string()), h),
        None => (None, s),
    };
    let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or_else(|| bad("missing `]`"))?;
        match after.strip_prefix(':') {
            Some(p) => (host, Some(p)),
            None if after.is_empty() => (host, None),
            None => return Err(bad("junk after `]`")),
        }
    } else {
        match hostport.split_once(':') {
            Some((h, p)) if !p.contains(':') => (h, Some(p)),
            Some(_) => (hostport, None),
            None => (hostport, None),
        }
    };
    if host.is_empty() {
        return Err(bad("empty host"));
    }
    let port = match port {
        Some(p) => Some(parse_port(Some(p), location)?),
        None => None,
    };
    Ok(JumpSpec {
        host: host.to_string(),
        user,
        port,
    })
}

fn parse_port(value: Option<&str>, location: &Location) -> Result<u16> {
    value
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|&p| p != 0)
        .ok_or_else(|| Error::SshConfig {
            file: location.file.clone(),
            line: location.line,
            message: format!("bad port `{}`", value.unwrap_or("")),
        })
}

fn expand_host_tokens(value: &str, alias: &str) -> String {
    if !value.contains('%') {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + alias.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('h') => out.push_str(alias),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

fn expand_tilde(value: &str, home: &Path) -> PathBuf {
    if value == "~" {
        home.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(value)
    }
}

/// Expands `*` and `?` in any component of `pattern`, sorted like glob(3).
/// Patterns that match nothing expand to nothing, as in OpenSSH.
fn expand_glob(pattern: &Path) -> Vec<PathBuf> {
    let mut current = vec![PathBuf::new()];
    for component in pattern.components() {
        let part = component.as_os_str().to_string_lossy();
        if !part.contains(['*', '?']) {
            for p in &mut current {
                p.push(component.as_os_str());
            }
            continue;
        }
        let mut next = Vec::new();
        for dir in &current {
            let Ok(entries) = std::fs::read_dir(if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir.as_path()
            }) else {
                continue;
            };
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.') && wildcard_match(&part, n))
                .collect();
            names.sort();
            next.extend(names.into_iter().map(|n| dir.join(n)));
        }
        current = next;
    }
    current.into_iter().filter(|p| p.is_file()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str, dir: &Path) -> SshConfig {
        SshConfig::parse(text, &dir.join("config"), Path::new("/home/u")).unwrap()
    }

    #[test]
    fn first_value_wins_across_wildcard_and_negated_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let c = parse(
            "User global\n\
             Host web-* !web-legacy\n  User deploy\n  Port 2200\n\
             Host web-1\n  HostName 10.0.0.1\n  User ignored\n\
             Host web-legacy\n  HostName old.example\n\
             Host *\n  Port 22\n",
            dir.path(),
        );
        let web = c.resolve("web-1", "me").unwrap();
        assert_eq!(web.target.hostname, "10.0.0.1");
        assert_eq!(web.target.user, "global");
        assert_eq!(web.target.port, 2200);
        let legacy = c.resolve("web-legacy", "me").unwrap();
        assert_eq!(legacy.target.hostname, "old.example");
        assert_eq!(legacy.target.port, 22, "negated pattern must exclude web-*");
        assert_eq!(c.aliases(), vec!["web-1", "web-legacy"]);
    }

    #[test]
    fn proxyjump_chains_resolve_recursively_with_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let c = parse(
            "Host target\n  HostName 10.1.1.1\n  ProxyJump inner,ops@edge:2022\n\
             Host inner\n  HostName inner.lan\n  ProxyJump outer\n\
             Host outer\n  HostName outer.example\n  User jumper\n\
             Host edge\n  HostName edge.lan\n  ProxyJump ignored-for-later-hops\n",
            dir.path(),
        );
        let t = c.resolve("target", "me").unwrap();
        let hops: Vec<_> = t
            .jumps
            .iter()
            .map(|e| format!("{}@{}:{}", e.user, e.hostname, e.port))
            .collect();
        assert_eq!(
            hops,
            vec![
                "jumper@outer.example:22",
                "me@inner.lan:22",
                "ops@edge.lan:2022"
            ]
        );
        assert_eq!(t.target.hostname, "10.1.1.1");
    }

    #[test]
    fn proxyjump_cycle_is_an_error_not_a_hang() {
        let dir = tempfile::tempdir().unwrap();
        let c = parse(
            "Host a\n  ProxyJump b\nHost b\n  ProxyJump a\n",
            dir.path(),
        );
        assert!(matches!(c.resolve("a", "me"), Err(Error::ProxyJumpCycle(_))));
    }

    #[test]
    fn include_globs_and_match_reported_per_entry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("conf.d")).unwrap();
        std::fs::write(
            dir.path().join("conf.d/10-db"),
            "Host db\n  HostName db.internal\n  ProxyCommand nc %h %p\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("conf.d/20-cache"), "Host cache\n  Port 6000\n").unwrap();
        std::fs::write(
            dir.path().join("config"),
            "Include conf.d/*\nMatch host db exec \"true\"\n  User matched\nHost db\n  User admin\n",
        )
        .unwrap();
        let hosts = import_hosts(&dir.path().join("config"), Path::new("/home/u"), "me").unwrap();
        let names: Vec<_> = hosts.iter().map(|h| h.alias.as_str()).collect();
        assert_eq!(names, vec!["db", "cache"]);
        let db = hosts[0].host.as_ref().unwrap();
        assert_eq!(db.target.hostname, "db.internal");
        assert_eq!(db.target.user, "admin", "Match block must not be guessed");
        let texts: Vec<_> = db.unsupported.iter().map(|u| u.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.starts_with("ProxyCommand")));
        assert!(texts.iter().any(|t| t.starts_with("Match")));
        let match_note = db.unsupported.iter().find(|u| u.text.starts_with("Match")).unwrap();
        assert_eq!(match_note.location.line, 2);
        assert_eq!(hosts[1].host.as_ref().unwrap().target.port, 6000);
    }

    #[test]
    fn identity_agent_and_equals_syntax() {
        let dir = tempfile::tempdir().unwrap();
        let c = parse(
            "Host a\n  IdentityAgent=~/.1password/agent.sock\nHost b\n  IdentityAgent none\nHost c\n  IdentityAgent SSH_AUTH_SOCK\n",
            dir.path(),
        );
        assert_eq!(
            c.resolve("a", "me").unwrap().target.agent,
            AgentSocket::Path(PathBuf::from("/home/u/.1password/agent.sock"))
        );
        assert_eq!(c.resolve("b", "me").unwrap().target.agent, AgentSocket::Disabled);
        assert_eq!(c.resolve("c", "me").unwrap().target.agent, AgentSocket::Default);
    }

    #[test]
    fn wildcard_matching() {
        assert!(wildcard_match("*.example.com", "a.b.example.com"));
        assert!(wildcard_match("h?st", "host"));
        assert!(!wildcard_match("h?st", "hoost"));
        assert!(wildcard_match("[10.0.0.*]:2222", "[10.0.0.7]:2222"));
        assert!(!wildcard_match("*.example.com", "example.com"));
    }

    #[test]
    fn jump_spec_forms() {
        let loc = Location {
            file: PathBuf::from("c"),
            line: 1,
        };
        let j = parse_jump("ssh://u@[::1]:2200", &loc).unwrap();
        assert_eq!((j.host.as_str(), j.user.as_deref(), j.port), ("::1", Some("u"), Some(2200)));
        let j = parse_jump("bastion", &loc).unwrap();
        assert_eq!((j.host.as_str(), j.user, j.port), ("bastion", None, None));
        assert!(parse_jump("h:notaport", &loc).is_err());
    }
}
