//! The app-hosted `remote` server: the SSH service every session reaches
//! (over loopback HTTP or as host tools), its host key and approval
//! questions as decision cards, the Remote hosts list kept in the central
//! config, the audit log and the SSH shell behind the Remote shell tab.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use loa_contracts::harness::Answer;
use loa_bus::SessionId;
use loa_core::{DecisionKind, DecisionStatus, Draft, ElementKind, NodeId, PatchSink, Value, answer, attr, now_ms};
use loa_contracts::tools::ToolSet;
use loa_remote::audit::{Action, AuditQuery, AuditRecord, AuditSink, MemoryAudit, SqliteAudit};
use loa_remote::hosts::HostRegistry;
use loa_remote::http::RemoteHttp;
use loa_remote::known_hosts::host_name;
use loa_remote::ports::{Approval, HostKeyChange, Prompt, PromptQueue, PromptView};
use loa_remote::resolve::{OpenSshCli, SshResolver};
use loa_remote::{RemoteService, RemoteServiceConfig};
use loa_view::panel;

use crate::area::Ctx;
use loa_runtime::session::{Session, dom_turns};

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
    /// `remote_bash` and `remote_file` over the service, as host tools and over HTTP.
    pub(crate) tools: ToolSet,
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
    /// `ssh-agent`), with its audit log at `<data>/remote-audit.db`.
    pub(crate) fn new(data: &Path, config: RemoteServiceConfig) -> (Self, Option<String>) {
        let path = data.join("remote-audit.db");
        let (audit, warning): (Arc<dyn AuditSink>, _) = match SqliteAudit::open(&path) {
            Ok(audit) => (Arc::new(audit), None),
            Err(error) => {
                let warning = format!("Can't open the remote audit log {} ({error}); this run logs in memory", path.display());
                (Arc::new(MemoryAudit::default()), Some(warning))
            }
        };
        let queue = Arc::new(PromptQueue::new());
        let service = RemoteService::new(
            Arc::new(HostRegistry::new()),
            audit,
            Arc::clone(&queue) as _,
            Arc::clone(&queue) as _,
            config,
        );
        let service = Arc::new(service);
        let tools = loa_remote::tools::tool_set(Arc::clone(&service));
        let remote = Self { service, queue, tools, http: None, http_error: None, cards: HashMap::new(), seen: BTreeSet::new() };
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
    pub(crate) fn sync(&mut self, ctx: &Ctx) -> (Vec<panel::RemotePrompt>, bool) {
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
    pub(crate) fn answer_panel(&mut self, id: u64, answer: panel::PromptAnswer, ctx: &Ctx) -> bool {
        let (taken, value) = match answer {
            panel::PromptAnswer::Trust => (self.queue.answer(id, true), TRUST),
            panel::PromptAnswer::Reject => (self.queue.answer(id, false), REJECT),
            panel::PromptAnswer::AllowOnce => (self.queue.answer(id, Approval::AllowOnce), answer::ALLOW_ONCE),
            panel::PromptAnswer::AllowSession => (self.queue.answer(id, Approval::AllowSession), answer::ALLOW_SESSION),
            panel::PromptAnswer::Deny => (self.queue.answer(id, Approval::Deny { reason: None }), answer::DENY),
        };
        if let Some(card) = self.cards.remove(&id) {
            let by = ctx.services.node.key.clone();
            let answer = Answer { values: vec![value.to_owned()], reason: None, by };
            let status = if taken { DecisionStatus::Answered } else { DecisionStatus::Cancelled };
            finish(ctx, &card, status, taken.then_some(&answer));
        }
        taken
    }

    /// Answers the question behind card `decision` of `session`.
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
            Some(Prompt::HostKey(_)) => self.queue.answer(id, value == TRUST),
            Some(Prompt::Approval(_)) => {
                let approval =
                    Approval::from_answer(value, answer.reason.clone()).unwrap_or(Approval::Deny { reason: answer.reason.clone() });
                self.queue.answer(id, approval)
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
        let Some(open) = ctx.runtime.sessions().get(session.as_str()) else { return };
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
    pub(crate) fn audit(&self, host: String) -> impl Future<Output = Result<Vec<panel::AuditRow>, String>> + Send + use<> {
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
fn target_session<'a>(prompt: &'a Prompt, ctx: &'a Ctx) -> Option<&'a Session> {
    match prompt {
        Prompt::Approval(request) => ctx.runtime.sessions().get(&request.session_id),
        Prompt::HostKey(_) => ctx.runtime.sessions()
            .active()
            .filter(|session| session.is_busy())
            .or_else(|| ctx.runtime.sessions().iter().find(|session| session.is_busy())),
    }
}

/// Writes a waiting card for `prompt` into its session, under its latest
/// turn.
fn raise(prompt: &PromptView, ctx: &Ctx) -> Option<Card> {
    let session = target_session(&prompt.prompt, ctx)?;
    let parent = session.writer().with_dom(|dom| dom_turns(dom).last().map(|turn| turn.id).or_else(|| dom.root().map(|root| root.id)))?;
    let (kind, options) = match prompt.prompt {
        Prompt::HostKey(_) => (DecisionKind::Question, Some(Value::List(vec![TRUST.into(), REJECT.into()]))),
        Prompt::Approval(_) => (DecisionKind::Approval, None),
    };
    let mut attrs: Vec<(&str, Value)> = vec![
        (attr::KIND, kind.into()),
        (attr::STATUS, DecisionStatus::Waiting.into()),
        (attr::TITLE, title(&prompt.prompt).into()),
        (attr::REQUEST_ID, format!("{REQUEST_PREFIX}{}", prompt.id).into()),
        (attr::CREATED, Value::Int(now_ms())),
    ];
    if let Some(options) = options {
        attrs.push((attr::OPTIONS, options));
    }
    let committed = session.writer().commit(Draft::insert(parent, ElementKind::Decision, attrs, body(&prompt.prompt))).ok()?;
    Some(Card { session: session.id().clone(), decision: committed.node })
}

/// Closes a card: answered with `answer`, or cancelled.
fn finish(ctx: &Ctx, card: &Card, status: DecisionStatus, answer: Option<&Answer>) {
    let Some(session) = ctx.runtime.sessions().get(card.session.as_str()) else { return };
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

/// One line for the card header.
fn title(prompt: &Prompt) -> String {
    match prompt {
        Prompt::HostKey(p) if matches!(p.change, HostKeyChange::New) => format!("Trust the host key of {}?", p.host),
        Prompt::HostKey(p) => format!("The host key of {} CHANGED", p.host),
        Prompt::Approval(r) => match r.action {
            Action::Command => format!("Run a command on {}?", r.host),
            Action::Write => format!("Write a file on {}?", r.host),
            Action::Edit => format!("Edit a file on {}?", r.host),
            other => format!("Allow {other} on {}?", r.host),
        },
    }
}

/// The card body: the fingerprint and keys on file, or the target and its detail.
fn body(prompt: &Prompt) -> String {
    match prompt {
        Prompt::HostKey(p) => {
            let mut body = format!("{} presented {} key\n{}", host_name(&p.hostname, p.port), p.key_type, p.fingerprint);
            match &p.change {
                HostKeyChange::New => body.push_str("\nNo key is on file for it yet."),
                HostKeyChange::Changed { previous } => {
                    body.push_str("\nThis may be a man in the middle. Keys on file:");
                    for key in previous {
                        body.push_str(&format!("\n{} {} ({}:{})", key.key_type, key.fingerprint, key.file.display(), key.line));
                    }
                }
            }
            body
        }
        Prompt::Approval(r) if r.detail.is_empty() => r.target.clone(),
        Prompt::Approval(r) => format!("{}\n{}", r.target, r.detail),
    }
}

fn panel_prompt(prompt: &PromptView) -> panel::RemotePrompt {
    let kind = match prompt.prompt {
        Prompt::HostKey(_) => panel::PromptKind::HostKey,
        Prompt::Approval(_) => panel::PromptKind::Approval,
    };
    panel::RemotePrompt { id: prompt.id, kind, title: title(&prompt.prompt), body: body(&prompt.prompt) }
}

fn audit_row(record: &AuditRecord) -> panel::AuditRow {
    let action = match record.action {
        Action::Read => panel::AuditAction::Read,
        Action::List => panel::AuditAction::List,
        Action::Write => panel::AuditAction::Write,
        Action::Edit => panel::AuditAction::Edit,
        Action::Command => panel::AuditAction::Command,
        Action::Terminal => panel::AuditAction::Terminal,
    };
    let outcome = if record.detail.is_empty() {
        record.outcome.as_str().to_owned()
    } else {
        format!("{}: {}", record.outcome.as_str(), record.detail)
    };
    panel::AuditRow { when: utc(record.at), session: record.session_id.clone(), action, detail: record.target.clone(), outcome }
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

/// `$HOME` and how Remote hosts resolve (`ssh -G` over `~/.ssh/config`).
#[derive(Clone)]
pub(crate) struct SshPaths {
    pub(crate) home: PathBuf,
    pub(crate) resolver: Arc<dyn SshResolver>,
}

impl SshPaths {
    pub(crate) fn from_env() -> Self {
        let home = loa_util::fs::home().unwrap_or_else(|| PathBuf::from("/"));
        Self { resolver: Arc::new(OpenSshCli::new(home.join(".ssh").join("config"))), home }
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
