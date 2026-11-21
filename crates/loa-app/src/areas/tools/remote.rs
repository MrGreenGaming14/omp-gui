//! The app-hosted `remote` server: the SSH service every session reaches
//! (over loopback HTTP or as host tools), its host key and approval
//! questions as decision cards, the Remote hosts list kept in the central
//! config, the audit log and the SSH shell behind the Remote shell tab.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use loa_contracts::harness::Answer;
use loa_bus::SessionId;
use loa_core::{DecisionKind, DecisionStatus, Draft, ElementKind, NodeId, PatchSink, Value, answer, attr, now_ms};
use loa_remote::audit::{Action, AuditLog, AuditQuery, AuditRecord};
use loa_remote::hosts::HostRegistry;
use loa_remote::http::RemoteHttp;
use loa_remote::ports::{Approval, PendingPrompt, Prompt, PromptQueue};
use loa_remote::pty::{ShellRead, ShellReader, ShellWriter};
use loa_remote::ssh_config::SshConfig;
use loa_remote::{RemoteService, RemoteServiceConfig};
use loa_ui_terminal as terminal;
use loa_ui_tools as tools;

use crate::area::Ctx;
use crate::session::dom_turns;

/// Prefix of the `request_id` of the cards this module raises.
pub(super) const REQUEST_PREFIX: &str = "remote:";

/// Options of a host key card, as the user picks them.
const TRUST: &str = "Trust";
const REJECT: &str = "Reject";

/// Audit rows a host's log view shows.
const AUDIT_ROWS: usize = 200;

/// A decision card raised in a session for a waiting question.
#[derive(Clone, Debug)]
struct Card {
    session: SessionId,
    decision: NodeId,
}

pub(crate) struct Remote {
    pub(crate) service: Arc<RemoteService>,
    pub(crate) queue: Arc<PromptQueue>,
    /// The loopback HTTP server harnesses with MCP support connect to.
    pub(crate) http: Option<Arc<RemoteHttp>>,
    /// Why the HTTP server isn't serving, once it failed.
    pub(crate) http_error: Option<String>,
    /// Cards raised in sessions, by question id.
    cards: HashMap<u64, Card>,
    /// Questions already seen, to tell new ones.
    seen: BTreeSet<u64>,
}

impl Remote {
    /// The service over `config` (the machine's `known_hosts` and
    /// `ssh-agent`), with its audit log at `<data>/remote-audit.db`. Returns
    /// a warning when the log can't be opened and an in-memory one is used.
    pub(crate) fn new(data: &Path, config: RemoteServiceConfig) -> (Self, Option<String>) {
        let path = data.join("remote-audit.db");
        let (audit, warning) = match AuditLog::open(&path) {
            Ok(audit) => (audit, None),
            Err(error) => {
                let audit = AuditLog::in_memory().unwrap_or_else(|e| panic!("in-memory audit log: {e}"));
                (audit, Some(format!("Can't open the remote audit log {} ({error}); this run logs in memory", path.display())))
            }
        };
        let queue = Arc::new(PromptQueue::new());
        let service = RemoteService::new(
            Arc::new(HostRegistry::new()),
            Arc::new(audit),
            Arc::clone(&queue) as _,
            Arc::clone(&queue) as _,
            config,
        );
        let remote =
            Self { service: Arc::new(service), queue, http: None, http_error: None, cards: HashMap::new(), seen: BTreeSet::new() };
        (remote, warning)
    }

    /// A session ended: its waiting approvals are denied, its HTTP token
    /// revoked and its "allow for session" answers forgotten.
    pub(crate) fn forget(&self, session: &str) {
        self.queue.deny_session(session);
        self.service.end_session(session);
        if let Some(http) = &self.http {
            http.forget(session);
        }
    }

    /// Brings the cards in line with the waiting questions: raises a card
    /// for each new one, cancels the cards of questions that went away.
    /// Returns the questions for the panel and whether any is new.
    pub(crate) fn sync(&mut self, ctx: &Ctx) -> (Vec<tools::RemotePrompt>, bool) {
        let pending = self.queue.pending();
        let ids: BTreeSet<u64> = pending.iter().map(|prompt| prompt.id).collect();
        let gone: Vec<u64> = self.cards.keys().copied().filter(|id| !ids.contains(id)).collect();
        for id in gone {
            if let Some(card) = self.cards.remove(&id) {
                finish(ctx, &card, DecisionStatus::Cancelled, None);
            }
        }
        let mut new = false;
        for prompt in &pending {
            if self.seen.insert(prompt.id) {
                new = true;
                if let Some(card) = raise(prompt, ctx) {
                    self.cards.insert(prompt.id, card);
                }
            }
        }
        self.seen.retain(|id| ids.contains(id));
        (pending.iter().map(panel_prompt).collect(), new)
    }

    /// Answers question `id` from the panel; its card records the answer.
    pub(crate) fn answer_panel(&mut self, id: u64, answer: tools::PromptAnswer, ctx: &Ctx) -> bool {
        let (taken, value) = match answer {
            tools::PromptAnswer::Trust => (self.queue.answer_host_key(id, true), TRUST),
            tools::PromptAnswer::Reject => (self.queue.answer_host_key(id, false), REJECT),
            tools::PromptAnswer::AllowOnce => (self.queue.answer_approval(id, Approval::AllowOnce), answer::ALLOW_ONCE),
            tools::PromptAnswer::AllowSession => {
                (self.queue.answer_approval(id, Approval::AllowSession), answer::ALLOW_SESSION)
            }
            tools::PromptAnswer::Deny => (self.queue.answer_approval(id, Approval::Deny { reason: None }), answer::DENY),
        };
        if let Some(card) = self.cards.remove(&id) {
            let by = ctx.services.node.key.clone();
            let answer = Answer { values: vec![value.to_owned()], reason: None, by };
            let status = if taken { DecisionStatus::Answered } else { DecisionStatus::Cancelled };
            finish(ctx, &card, status, taken.then_some(&answer));
        }
        taken
    }

    /// Answers the question behind card `decision` of `session`. `Err` when
    /// it no longer waits; its card is then cancelled.
    pub(crate) fn answer_card(&mut self, session: &SessionId, decision: NodeId, answer: &Answer, ctx: &Ctx) -> Result<(), String> {
        let id = self.cards.iter().find(|(_, card)| &card.session == session && card.decision == decision).map(|(id, _)| *id);
        let card = Card { session: session.clone(), decision };
        let Some(id) = id else {
            finish(ctx, &card, DecisionStatus::Cancelled, None);
            return Err("This remote host question is no longer waiting".to_owned());
        };
        self.cards.remove(&id);
        let value = answer.values.first().map(String::as_str).unwrap_or_default();
        let kind = self.queue.pending().into_iter().find(|prompt| prompt.id == id).map(|prompt| prompt.prompt);
        let taken = match kind {
            Some(Prompt::HostKey(_)) => self.queue.answer_host_key(id, value == TRUST),
            Some(Prompt::Approval(_)) => {
                let approval =
                    Approval::from_answer(value, answer.reason.clone()).unwrap_or(Approval::Deny { reason: answer.reason.clone() });
                self.queue.answer_approval(id, approval)
            }
            None => false,
        };
        if !taken {
            finish(ctx, &card, DecisionStatus::Cancelled, None);
            return Err("This remote host question is no longer waiting".to_owned());
        }
        finish(ctx, &card, DecisionStatus::Answered, Some(answer));
        Ok(())
    }

    /// Cancels waiting remote cards of `session` this run didn't raise:
    /// leftovers of an earlier run, whose question died with it.
    pub(crate) fn cancel_stale(&self, session: &SessionId, ctx: &Ctx) {
        let Some(open) = ctx.sessions.get(session.as_str()) else { return };
        let stale: Vec<NodeId> = open.writer().with_dom(|dom| {
            dom.iter()
                .filter(|node| {
                    node.kind == ElementKind::Decision
                        && node.str_attr(attr::REQUEST_ID).is_some_and(|id| id.starts_with(REQUEST_PREFIX))
                        && node.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str())
                })
                .map(|node| node.id)
                .filter(|id| !self.cards.values().any(|card| &card.session == session && card.decision == *id))
                .collect()
        });
        for decision in stale {
            finish(ctx, &Card { session: session.clone(), decision }, DecisionStatus::Cancelled, None);
        }
    }

    /// The audit log of `host`, newest first, as the panel lists it.
    pub(crate) fn audit(&self, host: String) -> impl Future<Output = Result<Vec<tools::AuditRow>, String>> + Send + use<> {
        let audit = Arc::clone(self.service.audit());
        async move {
            tokio::task::spawn_blocking(move || {
                let query = AuditQuery { host: Some(host), limit: AUDIT_ROWS, ..AuditQuery::default() };
                audit.query(&query).map(|records| records.iter().map(audit_row).collect()).map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())?
        }
    }
}

/// The session a question belongs to: an approval's own, a host key
/// question's the active session's (else any session's) running turn.
fn target_session<'a>(prompt: &'a Prompt, ctx: &'a Ctx) -> Option<&'a crate::area::Session> {
    match prompt {
        Prompt::Approval(request) => ctx.sessions.get(&request.session_id),
        Prompt::HostKey(_) => ctx
            .sessions
            .active()
            .filter(|session| session.is_busy())
            .or_else(|| ctx.sessions.iter().find(|session| session.is_busy())),
    }
}

/// Writes a waiting card for `prompt` into its session, under its latest
/// turn.
fn raise(prompt: &PendingPrompt, ctx: &Ctx) -> Option<Card> {
    let session = target_session(&prompt.prompt, ctx)?;
    let parent = session.writer().with_dom(|dom| dom_turns(dom).last().map(|turn| turn.id).or_else(|| dom.root().map(|root| root.id)))?;
    let (kind, options) = match prompt.prompt {
        Prompt::HostKey(_) => (DecisionKind::Question, Some(Value::List(vec![TRUST.into(), REJECT.into()]))),
        Prompt::Approval(_) => (DecisionKind::Approval, None),
    };
    let mut attrs: Vec<(&str, Value)> = vec![
        (attr::KIND, kind.into()),
        (attr::STATUS, DecisionStatus::Waiting.into()),
        (attr::TITLE, prompt.prompt.title().into()),
        (attr::REQUEST_ID, format!("{REQUEST_PREFIX}{}", prompt.id).into()),
        (attr::CREATED, Value::Int(now_ms())),
    ];
    if let Some(options) = options {
        attrs.push((attr::OPTIONS, options));
    }
    let committed = session.writer().commit(Draft::insert(parent, ElementKind::Decision, attrs, prompt.prompt.body())).ok()?;
    Some(Card { session: session.id().clone(), decision: committed.node })
}

/// Closes a card: answered with `answer`, or cancelled. Leaves cards that
/// no longer wait alone.
fn finish(ctx: &Ctx, card: &Card, status: DecisionStatus, answer: Option<&Answer>) {
    let Some(session) = ctx.sessions.get(card.session.as_str()) else { return };
    let writer = session.writer();
    let waiting = writer.with_dom(|dom| {
        dom.get(card.decision).is_some_and(|node| node.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str()))
    });
    if !waiting {
        return;
    }
    let mut drafts = Vec::new();
    if let Some(answer) = answer {
        drafts.push(Draft::set(card.decision, attr::ANSWER, Value::List(answer.values.iter().map(|value| value.as_str().into()).collect())));
        drafts.push(Draft::set(card.decision, attr::BY, answer.by.as_str()));
        if let Some(reason) = &answer.reason {
            drafts.push(Draft::set(card.decision, attr::REASON, reason.as_str()));
        }
    }
    drafts.push(Draft::set(card.decision, attr::ENDED, Value::Int(now_ms())));
    drafts.push(Draft::set(card.decision, attr::STATUS, status));
    for draft in drafts {
        if writer.commit(draft).is_err() {
            return;
        }
    }
}

fn panel_prompt(prompt: &PendingPrompt) -> tools::RemotePrompt {
    let kind = match prompt.prompt {
        Prompt::HostKey(_) => tools::PromptKind::HostKey,
        Prompt::Approval(_) => tools::PromptKind::Approval,
    };
    tools::RemotePrompt { id: prompt.id, kind, title: prompt.prompt.title(), body: prompt.prompt.body() }
}

fn audit_row(record: &AuditRecord) -> tools::AuditRow {
    let action = match record.action {
        Action::Read => tools::AuditAction::Read,
        Action::List => tools::AuditAction::List,
        Action::Write => tools::AuditAction::Write,
        Action::Edit => tools::AuditAction::Edit,
        Action::Command => tools::AuditAction::Command,
        Action::Terminal => tools::AuditAction::Terminal,
    };
    let outcome = if record.detail.is_empty() {
        record.outcome.as_str().to_owned()
    } else {
        format!("{}: {}", record.outcome.as_str(), record.detail)
    };
    tools::AuditRow { when: utc(record.at), session: record.session_id.clone(), action, detail: record.target.clone(), outcome }
}

/// `YYYY-MM-DD HH:MM:SS` in UTC.
fn utc(at: SystemTime) -> String {
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC", rest / 3_600, rest / 60 % 60, rest % 60)
}

/// Where `~/.ssh/config` and the user name come from.
#[derive(Clone, Debug)]
pub(crate) struct SshPaths {
    pub(crate) home: PathBuf,
    pub(crate) user: String,
}

impl SshPaths {
    pub(crate) fn from_env() -> Self {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        let user = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_else(|_| "root".to_owned());
        Self { home, user }
    }

    pub(crate) fn ssh_config(&self) -> Result<SshConfig, String> {
        let path = self.home.join(".ssh").join("config");
        SshConfig::load_or_empty(&path, &self.home).map_err(|error| format!("Can't read {}: {error}", path.display()))
    }

    /// An empty ssh config, for when `~/.ssh/config` can't be read.
    pub(crate) fn empty(&self) -> Option<SshConfig> {
        SshConfig::parse("", &self.home.join(".ssh").join("config"), &self.home).ok()
    }
}

/// The Remote hosts of the central config as the panel lists them.
pub(crate) fn panel_hosts(entries: &BTreeMap<String, loa_config::RemoteHost>) -> Vec<tools::RemoteHost> {
    entries
        .iter()
        .map(|(name, entry)| tools::RemoteHost {
            alias: name.clone(),
            hostname: entry.host.clone(),
            user: entry.user.clone(),
            port: entry.port,
            proxy_jump: entry.proxy_jump.clone(),
            limits: tools::HostLimits {
                allowed_paths: entry.allowed_paths.clone(),
                read_only: entry.read_only,
                commands_allowed: entry.commands,
                auto_approve: entry.auto_approve,
            },
        })
        .collect()
}

/// A host typed into the panel's add form as a config entry.
pub(crate) fn config_host(host: &tools::RemoteHost) -> loa_config::RemoteHost {
    loa_config::RemoteHost {
        host: host.hostname.clone(),
        user: host.user.clone(),
        port: host.port,
        proxy_jump: host.proxy_jump.clone(),
        allowed_paths: host.limits.allowed_paths.clone(),
        read_only: host.limits.read_only,
        commands: host.limits.commands_allowed,
        auto_approve: host.limits.auto_approve,
    }
}

/// [`terminal::RemotePty`] over the service's SSH shells.
pub(crate) struct SshPty(pub(crate) Arc<RemoteService>);

#[async_trait]
impl terminal::RemotePty for SshPty {
    async fn open_pty(&self, host: &str, size: terminal::GridSize) -> terminal::Result<terminal::PtyChannel> {
        let shell = self.0.open_shell(host, size.cols, size.rows).await.map_err(|error| remote_error(host, error))?;
        Ok(terminal::PtyChannel { reader: Box::new(SshReader(shell.reader)), writer: Box::new(SshWriter(shell.writer)) })
    }
}

fn remote_error(host: &str, error: loa_remote::Error) -> terminal::Error {
    terminal::Error::Remote { host: host.to_owned(), message: error.to_string() }
}

struct SshReader(ShellReader);

#[async_trait]
impl terminal::ChannelReader for SshReader {
    async fn read(&mut self) -> terminal::Result<terminal::ChannelRead> {
        Ok(match self.0.read().await {
            ShellRead::Data(data) => terminal::ChannelRead::Data(data),
            ShellRead::Closed { exit_status } => terminal::ChannelRead::Closed { exit_code: exit_status },
        })
    }
}

struct SshWriter(ShellWriter);

#[async_trait]
impl terminal::ChannelWriter for SshWriter {
    async fn write(&mut self, bytes: Bytes) -> terminal::Result<()> {
        let host = self.0.host().to_owned();
        self.0.write(bytes).await.map_err(|error| remote_error(&host, error))
    }

    async fn resize(&mut self, size: terminal::GridSize) -> terminal::Result<()> {
        let host = self.0.host().to_owned();
        self.0.resize(size.cols, size.rows).await.map_err(|error| remote_error(&host, error))
    }

    async fn close(&mut self) -> terminal::Result<()> {
        let host = self.0.host().to_owned();
        self.0.close().await.map_err(|error| remote_error(&host, error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_times_read_as_utc_dates() {
        assert_eq!(utc(UNIX_EPOCH), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(UNIX_EPOCH + std::time::Duration::from_secs(1_763_548_245)), "2025-11-19 10:30:45 UTC");
    }
}
