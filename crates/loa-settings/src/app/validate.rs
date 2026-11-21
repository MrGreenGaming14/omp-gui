//! The rules serde can't express, as one table. A rule's key may hold `*`,
//! which its check fills with the map key it failed on.

use super::{Config, DecisionDefault, Relay, Result, invalid};
use loa_contracts::harness::McpTransport;
use loa_core::answer;

/// What failed: the text for `*` and the reason.
type Check = std::result::Result<(), (String, String)>;

struct Rule {
    key: &'static str,
    check: fn(&Config) -> Check,
}

fn when(bad: bool, reason: impl Into<String>) -> Check {
    if bad { Err((String::new(), reason.into())) } else { Ok(()) }
}

/// The first entry of `map` that `bad` rejects.
fn each<K: AsRef<str>, T, R: Into<String>>(map: impl IntoIterator<Item = (K, T)>, bad: impl Fn(T) -> Option<R>) -> Check {
    map.into_iter().find_map(|(k, v)| bad(v).map(|r| (k.as_ref().to_owned(), r.into()))).map_or(Ok(()), Err)
}

fn blank(s: &str) -> bool {
    s.trim().is_empty()
}

fn not_http(s: &str) -> bool {
    !["https://", "http://"].iter().any(|scheme| s.strip_prefix(scheme).is_some_and(|rest| !rest.is_empty()))
}

const PORT: &str = "must be between 1 and 65535";
const POSITIVE: &str = "must be at least 1";
const EMPTY: &str = "must not be empty";

#[rustfmt::skip]
static RULES: &[Rule] = &[
    Rule { key: "harness.default", check: |c| when(blank(&c.harness.default), "must name a harness") },
    Rule { key: "harness.executables.*", check: |c| each(&c.harness.executables, |p| p.as_os_str().is_empty().then_some(EMPTY)) },
    Rule { key: "network.port", check: |c| when(c.network.port == 0, PORT) },
    Rule { key: "network.relay", check: |c| match &c.network.relay {
        Relay::Url(url) => when(not_http(url), format!("must be \"default\", \"off\" or an http(s) URL, found {url:?}")),
        _ => Ok(()),
    } },
    Rule { key: "updater.check_interval_secs", check: |c| when(c.updater.check_interval_secs == 0, POSITIVE) },
    Rule { key: "updater.pins.*", check: |c| each(&c.updater.pins, |v| blank(v).then_some("must be a version")) },
    Rule { key: "unattended.*.timeout_secs", check: |c| each(unattended(c), |(d, _)| (d.timeout_secs == 0).then_some(POSITIVE)) },
    Rule { key: "unattended.*.answer", check: |c| each(unattended(c), |(d, rule)| rule.check(&d.answer).err()) },
    Rule { key: "mcp_servers", check: |c| when(c.mcp_servers.keys().any(|k| blank(k)), "server names must not be empty") },
    Rule { key: "mcp_servers.*.command", check: |c| each(&c.mcp_servers, |s| match &s.transport {
        McpTransport::Stdio { command, .. } => blank(command).then_some(EMPTY),
        McpTransport::Http { .. } => None,
    }) },
    Rule { key: "mcp_servers.*.url", check: |c| each(&c.mcp_servers, |s| match &s.transport {
        McpTransport::Http { url, .. } => not_http(url).then(|| format!("must be an http(s) URL, found {url:?}")),
        McpTransport::Stdio { .. } => None,
    }) },
    Rule { key: "remote_hosts.*.host", check: |c| each(&c.remote_hosts, |h| blank(&h.host).then_some(EMPTY)) },
    Rule { key: "remote_hosts.*.port", check: |c| each(&c.remote_hosts, |h| (h.port == Some(0)).then_some(PORT)) },
    Rule { key: "remote_hosts.*.allowed_paths", check: |c| each(&c.remote_hosts, |h| {
        let bad = h.allowed_paths.iter().find(|p| !p.starts_with('/') || p.split('/').any(|part| part == ".."))?;
        Some(format!("must hold absolute paths without `..`, found {bad:?}"))
    }) },
    Rule { key: "projects.*.digest_role", check: |c| each(&c.projects, |p| blank(&p.digest_role).then_some("must name a model role")) },
    Rule { key: "projects.*.token_budget", check: |c| each(&c.projects, |p| (p.token_budget == 0).then_some(POSITIVE)) },
    Rule { key: "projects.*.pinned_files", check: |c| each(&c.projects, |p| {
        p.pinned_files.iter().any(|f| f.as_os_str().is_empty()).then_some("must not hold empty paths")
    }) },
];

/// The configured unattended defaults, by kind, with the rule their answer follows.
fn unattended(c: &Config) -> impl Iterator<Item = (&'static str, (&DecisionDefault, AnswerRule))> {
    let u = &c.unattended;
    [
        ("approval", &u.approval, AnswerRule::OneOf(&[answer::ALLOW_ONCE, answer::ALLOW_SESSION, answer::DENY])),
        ("question", &u.question, AnswerRule::NonEmpty),
        ("plan", &u.plan, AnswerRule::OneOf(&[answer::APPROVE, answer::REJECT])),
        ("form", &u.form, AnswerRule::JsonObject),
    ]
    .into_iter()
    .filter_map(|(kind, default, rule)| Some((kind, (default.as_ref()?, rule))))
}

/// How an unattended default answer is checked.
enum AnswerRule {
    OneOf(&'static [&'static str]),
    NonEmpty,
    JsonObject,
}

impl AnswerRule {
    fn check(&self, answer: &str) -> std::result::Result<(), String> {
        match self {
            Self::OneOf(allowed) if !allowed.contains(&answer) => {
                Err(format!("must be one of {}, found {answer:?}", allowed.join(", ")))
            }
            Self::NonEmpty if blank(answer) => Err(EMPTY.to_owned()),
            Self::JsonObject => serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(answer)
                .map(drop)
                .map_err(|e| format!("must be a JSON object: {e}")),
            _ => Ok(()),
        }
    }
}

impl Config {
    /// Checks [`RULES`] in order. The error names the offending key.
    pub fn validate(&self) -> Result<()> {
        RULES.iter().try_for_each(|rule| {
            (rule.check)(self).map_err(|(name, reason)| invalid(rule.key.replacen('*', &name, 1), reason))
        })
    }
}
