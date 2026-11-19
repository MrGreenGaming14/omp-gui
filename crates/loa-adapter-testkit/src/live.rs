//! Live session contract driver.
//!
//! Starts a session of an adapter (pointed at a fake harness executable
//! through [`SessionConfig::executable`]), runs a list of [`Action`]s and
//! checks the DOM conventions after each one. Waits on DOM changes, never on
//! time; the timeout only stops a run whose harness hangs.

use crate::caps::{check_produced, expect_supported, expect_unsupported};
use crate::check::{Phase, check_dom_in, check_history, is_open};
use crate::violation::{Rule, Violation, write_list};
use loa_adapter::{Adapter, AdapterError, Answer, Feature, HarnessSession, ModelSelection, ModelSwitch, Prompt, SessionConfig, SessionState};
use loa_core::{
    CommitError, DecisionStatus, Dom, ElementKind, MemorySink, MessageStatus, Node, NodeId, Patch, PatchSink, Role,
    SessionMode, TurnStatus, Value, attr,
};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// How long one step may wait for the harness by default.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// One step of a live run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Sends a prompt. The turn and the user message must be in the DOM when
    /// `send` returns.
    Send(Prompt),
    /// Sends a prompt while a turn runs. Must be `Busy` and leave the DOM
    /// without a new turn unless the harness declares `queue_mid_turn`.
    SendWhileBusy(Prompt),
    /// Waits until the session is idle. Everything must have ended.
    WaitIdle,
    /// Waits for a waiting decision under a waiting turn, then answers it.
    /// The answer must be recorded and the turn must stop waiting.
    Answer(Answer),
    /// Waits until a streaming message contains `after`, interrupts and waits
    /// until idle. The message and turn must end cancelled. The fake harness
    /// must hold its stream open after writing this text.
    InterruptWhileStreaming { after: String },
    /// Interrupts and waits until idle. A turn that was running must end cancelled.
    Interrupt,
    /// Calls `fork`, `set_model`, `set_mode` and `compact`: each must return
    /// `Unsupported` exactly when the capabilities don't declare it. Run it
    /// while idle and against a fake harness only.
    ProbeCapabilities,
}

impl Action {
    fn name(&self) -> &'static str {
        match self {
            Action::Send(_) => "send",
            Action::SendWhileBusy(_) => "send while busy",
            Action::WaitIdle => "wait idle",
            Action::Answer(_) => "answer",
            Action::InterruptWhileStreaming { .. } => "interrupt while streaming",
            Action::Interrupt => "interrupt",
            Action::ProbeCapabilities => "probe capabilities",
        }
    }
}

/// What a live run found. Clean when `violations` is empty.
#[derive(Debug)]
pub struct LiveReport {
    /// The DOM at the end of the run.
    pub dom: Dom,
    pub patches: Vec<Patch>,
    pub violations: Vec<Violation>,
}

impl LiveReport {
    pub fn is_clean(&self) -> bool {
        self.violations.is_empty()
    }
}

impl fmt::Display for LiveReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "live run, {} patches:", self.patches.len())?;
        write_list(f, &self.violations)
    }
}

/// A [`MemorySink`] that announces every commit, so the driver can wait for
/// the DOM to reach a state.
struct WatchedSink {
    inner: MemorySink,
    seq: watch::Sender<u64>,
}

impl PatchSink for WatchedSink {
    fn commit_at(&self, draft: loa_core::Draft, ts: i64) -> Result<Patch, CommitError> {
        let patch = self.inner.commit_at(draft, ts)?;
        self.seq.send_replace(patch.seq);
        Ok(patch)
    }
}

/// Runs live contract steps against one adapter.
pub struct LiveDriver {
    adapter: Arc<dyn Adapter>,
    config: SessionConfig,
    timeout: Duration,
}

/// A failed step: the run stops after it.
struct Stop;

type Step<T = ()> = Result<T, Stop>;

/// Per-run state.
struct Run<'a> {
    driver: &'a LiveDriver,
    sink: Arc<WatchedSink>,
    session: Arc<dyn HarnessSession>,
    violations: Vec<Violation>,
    step: String,
}

impl LiveDriver {
    /// `config` is passed to `start`; point its `executable` at the fake harness.
    pub fn new(adapter: Arc<dyn Adapter>, config: SessionConfig) -> Self {
        Self { adapter, config, timeout: DEFAULT_TIMEOUT }
    }

    /// How long one step may wait for the harness.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Starts a session, runs `actions` and shuts it down. Stops at the first
    /// adapter error or timeout.
    pub async fn run(&self, actions: &[Action]) -> LiveReport {
        let (seq, _) = watch::channel(0);
        let sink = Arc::new(WatchedSink { inner: MemorySink::new(), seq });
        let started = tokio::time::timeout(self.timeout, self.adapter.start(self.config.clone(), sink.clone())).await;
        let session = match started {
            Ok(Ok(session)) => session,
            Ok(Err(err)) => return report(&sink, vec![Violation::new(Rule::Adapter, None, format!("start: {err}"))]),
            Err(_) => return report(&sink, vec![Violation::new(Rule::Adapter, None, "start timed out")]),
        };
        let session = Arc::from(session);
        let mut run = Run { driver: self, sink: sink.clone(), session, violations: Vec::new(), step: "start".into() };
        run.after_start();
        for (i, action) in actions.iter().enumerate() {
            run.step = format!("step {} ({})", i + 1, action.name());
            if run.action(action).await.is_err() {
                break;
            }
        }
        run.step = "shutdown".into();
        let shutdown = run.within(run.session.shutdown()).await;
        if let Ok(Err(err)) = shutdown {
            run.fail(Rule::Adapter, None, format!("{err}"));
        }
        let patches = sink.inner.patches();
        let dom = sink.inner.dom();
        let mut violations = run.violations;
        violations.extend(check_history(&patches));
        violations.extend(check_produced(&dom, self.adapter.harness(), &self.adapter.capabilities()));
        LiveReport { dom, patches, violations }
    }
}

fn report(sink: &WatchedSink, violations: Vec<Violation>) -> LiveReport {
    LiveReport { dom: sink.inner.dom(), patches: sink.inner.patches(), violations }
}

/// The last `<turn>` of the session.
fn last_turn(dom: &Dom) -> Option<&Node> {
    let root = dom.root()?;
    dom.children_of_kind(root.id, ElementKind::Turn).last()
}

fn turn_count(dom: &Dom) -> usize {
    dom.root().map_or(0, |r| dom.children_of_kind(r.id, ElementKind::Turn).count())
}

/// The turn a node sits in.
fn turn_of(dom: &Dom, id: NodeId) -> Option<&Node> {
    dom.ancestors(id).find(|n| n.kind == ElementKind::Turn)
}

impl Run<'_> {
    fn fail(&mut self, rule: Rule, node: impl Into<Option<NodeId>>, detail: impl fmt::Display) {
        let detail = format!("{}: {detail}", self.step);
        self.violations.push(Violation::new(rule, node, detail));
    }

    fn check(&mut self, phase: Phase) {
        let found = self.sink.inner.with_dom(|dom| check_dom_in(dom, phase));
        for mut v in found {
            v.detail = format!("{}: {}", self.step, v.detail);
            self.violations.push(v);
        }
    }

    async fn within<F: Future>(&self, future: F) -> Result<F::Output, tokio::time::error::Elapsed> {
        tokio::time::timeout(self.driver.timeout, future).await
    }

    /// Runs an adapter call with the timeout; errors and timeouts stop the run.
    async fn call<T>(&mut self, what: &str, future: impl Future<Output = loa_adapter::Result<T>>) -> Step<T> {
        match self.within(future).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => {
                self.fail(Rule::Adapter, None, format!("{what}: {err}"));
                Err(Stop)
            }
            Err(_) => {
                self.fail(Rule::Adapter, None, format!("{what} timed out"));
                Err(Stop)
            }
        }
    }

    /// Waits until `found` returns `Some` for the DOM.
    async fn wait_for<T>(&mut self, what: &str, found: impl Fn(&Dom) -> Option<T>) -> Step<T> {
        let sink = self.sink.clone();
        let mut changes = sink.seq.subscribe();
        let wait = async {
            loop {
                if let Some(value) = sink.inner.with_dom(&found) {
                    return Some(value);
                }
                // The sender lives in `sink`, which this future holds.
                changes.changed().await.ok()?;
            }
        };
        match self.within(wait).await {
            Ok(Some(value)) => Ok(value),
            _ => {
                self.fail(Rule::Step, None, format!("timed out waiting for {what}"));
                Err(Stop)
            }
        }
    }

    fn after_start(&mut self) {
        let config = &self.driver.config;
        let root = self.sink.inner.with_dom(|dom| dom.root().map(|r| (r.id, r.kind, r.attrs.clone())));
        match root {
            None => self.fail(Rule::Root, None, "start didn't insert the <session> root"),
            Some((id, ElementKind::Session, attrs)) => {
                let str_of = |name: &str| attrs.get(name).and_then(Value::as_str).map(str::to_owned);
                if str_of(attr::ID).as_deref() != Some(&config.session_id) {
                    self.fail(Rule::SessionAttrs, id, format!("<session id> isn't the configured {:?}", config.session_id));
                }
                if str_of(attr::NODE).as_deref() != Some(&config.node) {
                    self.fail(Rule::SessionAttrs, id, format!("<session node> isn't the configured {:?}", config.node));
                }
                if config.project.is_some() && str_of(attr::PROJECT) != config.project {
                    self.fail(Rule::SessionAttrs, id, "<session project> isn't the configured project");
                }
            }
            Some(_) => {}
        }
        self.check(Phase::Live);
    }

    async fn action(&mut self, action: &Action) -> Step {
        match action {
            Action::Send(prompt) => self.send(prompt).await,
            Action::SendWhileBusy(prompt) => self.send_while_busy(prompt).await,
            Action::WaitIdle => self.wait_idle().await,
            Action::Answer(answer) => self.answer(answer).await,
            Action::InterruptWhileStreaming { after } => self.interrupt_while_streaming(after).await,
            Action::Interrupt => self.interrupt().await,
            Action::ProbeCapabilities => self.probe().await,
        }
    }

    async fn send(&mut self, prompt: &Prompt) -> Step {
        let before = self.sink.inner.with_dom(turn_count);
        let session = Arc::clone(&self.session);
        self.call("send", session.send(prompt.clone())).await?;
        let (count, user) = self.sink.inner.with_dom(|dom| {
            let user = last_turn(dom).and_then(|t| {
                dom.children_of_kind(t.id, ElementKind::Message)
                    .next()
                    .filter(|m| m.str_attr(attr::ROLE) == Some(Role::User.as_str()))
                    .map(|m| (m.id, m.text.clone(), m.str_attr(attr::BY).map(str::to_owned)))
            });
            (turn_count(dom), user)
        });
        if count != before + 1 {
            self.fail(Rule::Step, None, format!("{} turns after send, expected {}", count, before + 1));
        }
        match user {
            None => self.fail(Rule::TurnShape, None, "the new turn has no user message"),
            Some((id, text, _)) if text != prompt.text => {
                self.fail(Rule::Step, id, format!("the user message is {text:?}, the prompt was {:?}", prompt.text));
            }
            Some((id, _, by)) if !prompt.by.is_empty() && by.as_deref() != Some(&prompt.by) => {
                self.fail(Rule::Step, id, format!("<message by={by:?}>, the prompt is by {:?}", prompt.by));
            }
            Some(_) => {}
        }
        self.check(Phase::Live);
        Ok(())
    }

    async fn send_while_busy(&mut self, prompt: &Prompt) -> Step {
        if !matches!(self.session.state(), SessionState::Running | SessionState::Waiting) {
            self.fail(Rule::Step, None, "no turn is running");
            return Err(Stop);
        }
        let queues = self.driver.adapter.capabilities().queue_mid_turn;
        let before = self.sink.inner.with_dom(turn_count);
        let sent = self.within(self.session.send(prompt.clone())).await;
        match (queues, sent) {
            (_, Err(_)) => {
                self.fail(Rule::Adapter, None, "send timed out");
                return Err(Stop);
            }
            (false, Ok(Err(AdapterError::Busy))) => {
                if self.sink.inner.with_dom(turn_count) != before {
                    self.fail(Rule::Step, None, "a Busy send still inserted a turn");
                }
            }
            (false, Ok(other)) => {
                let got = other.map_or_else(|e| e.to_string(), |()| "Ok".to_owned());
                self.fail(Rule::Capability, None, format!("queue_mid_turn is false but send returned {got}, not Busy"));
            }
            (true, Ok(Err(err))) => {
                self.fail(Rule::Capability, None, format!("queue_mid_turn is declared but send returned `{err}`"));
            }
            (true, Ok(Ok(()))) => {}
        }
        self.check(Phase::Live);
        Ok(())
    }

    async fn wait_idle(&mut self) -> Step {
        let session = Arc::clone(&self.session);
        self.call("wait_idle", session.wait_idle()).await?;
        let state = self.session.state();
        if state != SessionState::Idle {
            self.fail(Rule::Step, None, format!("state() is {state:?} after wait_idle"));
        }
        self.check(Phase::Settled);
        Ok(())
    }

    async fn answer(&mut self, answer: &Answer) -> Step {
        let waiting = |dom: &Dom| {
            dom.iter()
                .filter(|n| {
                    n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str())
                })
                .find(|n| {
                    turn_of(dom, n.id).is_some_and(|t| t.str_attr(attr::STATUS) == Some(TurnStatus::Waiting.as_str()))
                })
                .map(|n| n.id)
        };
        let decision = self.wait_for("a waiting decision under a waiting turn", waiting).await?;
        self.check(Phase::Live);
        let session = Arc::clone(&self.session);
        self.call("answer", session.answer(decision, answer.clone())).await?;

        let recorded = self.sink.inner.with_dom(|dom| dom.get(decision).cloned());
        let Some(node) = recorded else {
            self.fail(Rule::Step, decision, "the decision is gone after answer");
            return Err(Stop);
        };
        if node.str_attr(attr::STATUS) != Some(DecisionStatus::Answered.as_str()) {
            self.fail(Rule::DecisionAnswer, decision, format!("status is {:?} after answer", node.str_attr(attr::STATUS)));
        }
        let values: Vec<&str> = node.attr(attr::ANSWER).map(|v| v.str_items().collect()).unwrap_or_default();
        if values != answer.values {
            self.fail(Rule::DecisionAnswer, decision, format!("answer is {values:?}, sent {:?}", answer.values));
        }
        if !answer.by.is_empty() && node.str_attr(attr::BY) != Some(&answer.by) {
            self.fail(Rule::DecisionAnswer, decision, format!("by is {:?}, sent {:?}", node.str_attr(attr::BY), answer.by));
        }
        if node.str_attr(attr::REASON) != answer.reason.as_deref() {
            self.fail(Rule::DecisionAnswer, decision, format!("reason is {:?}, sent {:?}", node.str_attr(attr::REASON), answer.reason));
        }
        let no_longer_waiting = |dom: &Dom| {
            turn_of(dom, decision).filter(|t| t.str_attr(attr::STATUS) != Some(TurnStatus::Waiting.as_str())).map(|_| ())
        };
        self.wait_for("the turn to stop waiting", no_longer_waiting).await?;
        self.check(Phase::Live);
        Ok(())
    }

    async fn interrupt_while_streaming(&mut self, after: &str) -> Step {
        let streaming = |dom: &Dom| {
            dom.iter()
                .find(|n| {
                    n.kind == ElementKind::Message
                        && n.str_attr(attr::STATUS) == Some(MessageStatus::Streaming.as_str())
                        && n.text.contains(after)
                })
                .map(|n| n.id)
        };
        let message = self.wait_for("a streaming message containing the expected text", streaming).await?;
        self.check(Phase::Live);
        let session = Arc::clone(&self.session);
        self.call("interrupt", session.interrupt()).await?;
        self.call("wait_idle", session.wait_idle()).await?;
        let (message_status, turn) = self.sink.inner.with_dom(|dom| {
            let status = dom.get(message).and_then(|m| m.str_attr(attr::STATUS)).map(str::to_owned);
            let turn = turn_of(dom, message).map(|t| (t.id, t.str_attr(attr::STATUS).map(str::to_owned)));
            (status, turn)
        });
        if message_status.as_deref() != Some(MessageStatus::Cancelled.as_str()) {
            self.fail(Rule::Step, message, format!("the interrupted message is {message_status:?}, not cancelled"));
        }
        match turn {
            Some((id, status)) if status.as_deref() != Some(TurnStatus::Cancelled.as_str()) => {
                self.fail(Rule::Step, id, format!("the interrupted turn is {status:?}, not cancelled"));
            }
            None => self.fail(Rule::Step, message, "the interrupted message has no turn"),
            _ => {}
        }
        self.check(Phase::Settled);
        Ok(())
    }

    async fn interrupt(&mut self) -> Step {
        let open = self.sink.inner.with_dom(|dom| last_turn(dom).filter(|t| is_open(t)).map(|t| t.id));
        let session = Arc::clone(&self.session);
        self.call("interrupt", session.interrupt()).await?;
        self.call("wait_idle", session.wait_idle()).await?;
        if let Some(turn) = open {
            let status = self.sink.inner.with_dom(|dom| dom.get(turn).and_then(|t| t.str_attr(attr::STATUS)).map(str::to_owned));
            if status.as_deref() != Some(TurnStatus::Cancelled.as_str()) {
                self.fail(Rule::Step, turn, format!("the interrupted turn is {status:?}, not cancelled"));
            }
        }
        self.check(Phase::Settled);
        Ok(())
    }

    async fn probe(&mut self) -> Step {
        let driver = self.driver;
        let adapter = &driver.adapter;
        let caps = adapter.capabilities();
        let harness = adapter.harness();
        let mut found = Vec::new();

        let model = self.driver.config.model.clone().unwrap_or_else(|| "contract-probe".to_owned());
        let selection = ModelSelection { model, role: None };
        let result = self.within(self.session.set_model(selection)).await;
        self.probed("set_model", caps.model_switch != ModelSwitch::Never, harness, result, &mut found);

        let result = self.within(self.session.set_mode(SessionMode::Normal)).await;
        self.probed("set_mode", caps.has(Feature::PlanMode), harness, result, &mut found);

        let result = self.within(self.session.compact(None)).await;
        self.probed("compact", caps.has(Feature::Compaction), harness, result, &mut found);

        let dom = self.sink.inner.dom();
        let fork_sink: Arc<dyn PatchSink> = Arc::new(MemorySink::from_dom(dom.clone()));
        let forked = self.within(adapter.fork(self.driver.config.clone(), &dom, fork_sink)).await;
        let forked = match forked {
            Ok(Ok(session)) => {
                // The probe only needs the fork to start; its shutdown result says nothing about the contract.
                let _ = self.within(session.shutdown()).await;
                Ok(Ok(()))
            }
            Ok(Err(err)) => Ok(Err(err)),
            Err(elapsed) => Err(elapsed),
        };
        self.probed("fork", caps.has(Feature::Fork), harness, forked, &mut found);

        for mut v in found {
            v.detail = format!("{}: {}", self.step, v.detail);
            self.violations.push(v);
        }
        Ok(())
    }

    fn probed<T>(
        &mut self,
        what: &str,
        declared: bool,
        harness: &str,
        result: Result<loa_adapter::Result<T>, tokio::time::error::Elapsed>,
        found: &mut Vec<Violation>,
    ) {
        match result {
            Err(_) => self.fail(Rule::Adapter, None, format!("{what} timed out")),
            Ok(result) if declared => expect_supported(what, &result, found),
            Ok(result) => expect_unsupported(harness, what, &result, found),
        }
    }
}
