//! App-provided session features. All changes to conversations go through their
//! single-writer sinks. The app supplies journal-backed sinks and tool gates;
//! this crate never talks to a model or an undocumented harness file.
//!
//! - [`library::Library`]: journal-backed live sessions and the [`SessionStore`] port
//! - [`native`]: native harness sessions on this machine, for import
//! - [`plan`]: the read-only tool gate and plan review of plan mode
//! - [`hooks`]: rules and advisor run around each turn for harnesses without them
//! - [`smol`]: the small-model port behind compaction summaries and advisor notes

pub mod hooks;
pub mod library;
pub mod native;
pub mod plan;
pub mod smol;

use async_trait::async_trait;
use loa_contracts::harness::{Adapter, AdapterError, Feature, HarnessSession, ModelSelection, ModelSwitch, Prompt, SessionConfig, SessionState};
use loa_core::{attr, Dom, Draft, ElementKind, NodeId, PatchSink, SessionMode, Value};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Adapter(#[from] AdapterError),
    #[error(transparent)]
    Commit(#[from] loa_core::CommitError),
    #[error("session DOM has no root")]
    NoSession,
    #[error("session is not idle")]
    Busy,
    #[error("requested patch sequence is beyond the current session")]
    InvalidSequence,
    #[error("the harness does not allow model changes at this point")]
    ModelFixed,
    #[error("feature port failed: {0}")]
    Port(String),
    #[error(transparent)]
    Journal(#[from] loa_bus::JournalError),
    #[error(transparent)]
    Bus(#[from] loa_bus::BusError),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Supplies a new journal and its writer. `fork` MUST call the journal's
/// `fork(seq, id)` and continue its replayed DOM with the returned writer.
/// `fresh` creates an empty journal/writer; both must be durable before use.
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn fork(&self, source_id: &str, seq: u64, new_id: &str) -> Result<(Dom, Arc<dyn PatchSink>)>;
    async fn fresh(&self, new_id: &str) -> Result<Arc<dyn PatchSink>>;
}

/// Summarizes the supplied conversation using the configured small model.
/// The implementation must make a real model call and return its actual text.
#[async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(&self, model: &str, history: &str, instructions: Option<&str>) -> Result<String>;
}

/// Enforces read-only execution at the tool boundary, including harness-native
/// tools and MCP calls (not just the app's host tools). Activation fails closed
/// when the harness cannot enforce this policy. The permit keeps it installed.
#[async_trait]
pub trait ReadOnlyTools: Send + Sync {
    async fn activate(&self, session_id: &str) -> Result<Box<dyn PlanPermit>>;
}

/// Dropping the permit restores normal tool permissions.
pub trait PlanPermit: Send + Sync {}

pub struct RunningSession {
    pub config: SessionConfig,
    pub sink: Arc<dyn PatchSink>,
    pub harness: Box<dyn HarnessSession>,
}

fn root(dom: &Dom) -> Result<NodeId> {
    dom.root().map(|node| node.id).ok_or(Error::NoSession)
}

fn session_id(dom: &Dom) -> Result<&str> {
    dom.root().and_then(|node| node.str_attr(attr::ID)).ok_or(Error::NoSession)
}

fn lineage(sink: &dyn PatchSink, root: NodeId, parent: &str, seq: u64) -> Result<()> {
    sink.commit(Draft::set(root, attr::PARENT_SESSION, parent))?;
    sink.commit(Draft::set(root, attr::FORKED_AT, i64::try_from(seq).unwrap_or(i64::MAX)))?;
    Ok(())
}

/// Labels a forked DOM as the new session. A journal fork already wrote
/// these attributes; only what differs is committed.
fn rename_fork(dom: &mut Dom, sink: &dyn PatchSink, new_id: &str, old_id: &str, seq: u64) -> Result<()> {
    let id = root(dom)?;
    for (name, value) in [
        (attr::ID, Value::from(new_id)),
        (attr::PARENT_SESSION, Value::from(old_id)),
        (attr::FORKED_AT, Value::Int(i64::try_from(seq).unwrap_or(i64::MAX))),
    ] {
        if dom.get(id).and_then(|node| node.attr(name)) == Some(&value) {
            continue;
        }
        let patch = sink.commit(Draft::set(id, name, value))?;
        dom.apply(&patch).map_err(loa_core::CommitError::from)?;
    }
    Ok(())
}

/// A textual, ordered translation of the visible history, never native
/// harness internals. Compactions supersede their covered turns. The caller
/// uses this as fresh-session context, not as an extra user turn.
pub fn translated_history(dom: &Dom) -> Result<String> {
    let session = dom.root().ok_or(Error::NoSession)?;
    let mut covered = std::collections::HashSet::new();
    let mut summaries = std::collections::HashMap::new();
    for &id in &session.children {
        let Some(node) = dom.get(id) else { continue };
        if node.kind == ElementKind::Compaction
            && let Some(Value::List(ids)) = node.attr(attr::COVERS)
        {
            let mut first = None;
            for id in ids {
                if let Some(id) = id.as_int().and_then(|id| u64::try_from(id).ok()) {
                    let id = NodeId(id);
                    if dom.get(id).is_some() {
                        covered.insert(id);
                        if first.is_none() { first = Some(id); }
                    }
                }
            }
            if let Some(first) = first {
                summaries.insert(first, node.text.as_str());
            }
        }
    }
    let mut history = String::new();
    for &id in &session.children {
        if let Some(summary) = summaries.get(&id) {
            history.push_str("Summary of earlier turns:\n");
            history.push_str(summary);
            history.push_str("\n\n");
        }
        let Some(node) = dom.get(id) else { continue };
        if node.kind == ElementKind::Turn && !covered.contains(&id) {
            push_turn(dom, node, &mut history);
            history.push('\n');
        }
    }
    Ok(history)
}

/// The visible text of one `<turn>`: its messages (thinking left out) and
/// tool calls with their input and result.
pub fn turn_transcript(dom: &Dom, turn: NodeId) -> String {
    let mut out = String::new();
    if let Some(node) = dom.get(turn) {
        push_turn(dom, node, &mut out);
    }
    out
}

fn push_turn(dom: &Dom, turn: &loa_core::Node, out: &mut String) {
    for &child in &turn.children {
        let Some(item) = dom.get(child) else { continue };
        match item.kind {
            ElementKind::Message if item.str_attr(attr::KIND) != Some(loa_core::MessageKind::Thinking.as_str()) => {
                out.push_str(item.str_attr(attr::BY).filter(|by| *by == hooks::ADVISOR).unwrap_or_else(|| {
                    item.str_attr(attr::ROLE).unwrap_or("message")
                }));
                out.push_str(": ");
                out.push_str(&item.text);
                out.push('\n');
            }
            ElementKind::Call => {
                out.push_str("tool ");
                out.push_str(item.str_attr(attr::TOOL).unwrap_or("unknown"));
                out.push_str(":\n");
                for &part in &item.children {
                    if let Some(part) = dom.get(part) {
                        out.push_str(part.kind.as_str());
                        out.push_str(": ");
                        out.push_str(&part.text);
                        out.push('\n');
                    }
                }
            }
            _ => {}
        }
    }
}

/// Heading of the history a fallback fork or rewind seeds its fresh session with.
const HISTORY_HEADING: &str = "The conversation so far, continued from an earlier session. Treat it as your own \
history and carry on from where it ends:";

fn seed_context(config: &mut SessionConfig, history: &str) {
    let context = config.context.get_or_insert_with(String::new);
    if !context.is_empty() { context.push_str("\n\n"); }
    context.push_str(HISTORY_HEADING);
    context.push_str("\n\n");
    context.push_str(history);
}

/// Native fork preserves the patch prefix and sequence numbers; fallback
/// starts a new journal and sends the translated DOM as project context.
/// The caller supplies a unique `new_id` in `config.session_id`.
pub async fn fork(
    adapter: &dyn Adapter,
    store: &dyn SessionStore,
    source: &Dom,
    mut config: SessionConfig,
) -> Result<RunningSession> {
    let old_id = session_id(source)?;
    let seq = source.seq();
    if adapter.capabilities().has(Feature::Fork) {
        let (mut dom, sink) = store.fork(old_id, seq, &config.session_id).await?;
        rename_fork(&mut dom, sink.as_ref(), &config.session_id, old_id, seq)?;
        let harness = adapter.fork(config.clone(), &dom, sink.clone()).await?;
        Ok(RunningSession { config, sink, harness })
    } else {
        seed_context(&mut config, &translated_history(source)?);
        let sink = store.fresh(&config.session_id).await?;
        let harness = adapter.start(config.clone(), sink.clone()).await?;
        lineage(sink.as_ref(), NodeId(1), old_id, seq)?;
        Ok(RunningSession { config, sink, harness })
    }
}


/// A rewind always creates a branch at the historical patch position. This
/// retains the old timeline, including still-running work, untouched. The
/// caller stops live work on the old branch only when choosing to abandon it.
pub async fn rewind(
    adapter: &dyn Adapter,
    store: &dyn SessionStore,
    current: &Dom,
    target: &Dom,
    config: SessionConfig,
) -> Result<(loa_core::rewind::RewindPlan, RunningSession)> {
    if target.seq() > current.seq() { return Err(Error::InvalidSequence); }
    let plan = loa_core::rewind::plan(current, target);
    let old_id = session_id(current)?;
    let config = config;
    if !adapter.capabilities().has(Feature::Fork) {
        return fallback_rewind(adapter, store, target, config, plan, old_id).await;
    }
    let (mut dom, sink) = store.fork(old_id, target.seq(), &config.session_id).await?;
    rename_fork(&mut dom, sink.as_ref(), &config.session_id, old_id, target.seq())?;
    let harness = adapter.fork(config.clone(), &dom, sink.clone()).await?;
    Ok((plan, RunningSession { config, sink, harness }))
}

async fn fallback_rewind(
    adapter: &dyn Adapter, store: &dyn SessionStore, target: &Dom,
    mut config: SessionConfig, plan: loa_core::rewind::RewindPlan, old_id: &str,
) -> Result<(loa_core::rewind::RewindPlan, RunningSession)> {
    seed_context(&mut config, &translated_history(target)?);
    let sink = store.fresh(&config.session_id).await?;
    let harness = adapter.start(config.clone(), sink.clone()).await?;
    lineage(sink.as_ref(), NodeId(1), old_id, target.seq())?;
    Ok((plan, RunningSession { config, sink, harness }))
}

/// The live session being compacted and the configuration for its successor.
pub struct Compaction<'a> {
    pub running: &'a dyn HarnessSession,
    pub source: &'a Dom,
    pub source_sink: &'a dyn PatchSink,
    pub next: SessionConfig,
    pub smol_model: &'a str,
    pub instructions: Option<&'a str>,
}

/// Compacts natively if supported; otherwise asks the real small-model port
/// for a summary, records precisely which turns it covers and starts a fresh
/// harness session with that summary as context. Old turns remain visible.
pub async fn compact(
    adapter: &dyn Adapter, store: &dyn SessionStore, summarizer: &dyn Summarizer, request: Compaction<'_>,
) -> Result<Option<RunningSession>> {
    let Compaction { running, source, source_sink, mut next, smol_model, instructions } = request;
    if running.state() != SessionState::Idle { return Err(Error::Busy); }
    if adapter.capabilities().has(Feature::Compaction) {
        running.compact(instructions.map(str::to_owned)).await?;
        return Ok(None);
    }
    let history = translated_history(source)?;
    let summary = summarizer.summarize(smol_model, &history, instructions).await?;
    let session = source.root().ok_or(Error::NoSession)?;
    let covers = session.children.iter().filter_map(|id| {
        dom_turn(source, *id).then_some(Value::Int(i64::try_from(id.0).unwrap_or(i64::MAX)))
    }).collect();
    source_sink.commit(Draft::insert(session.id, ElementKind::Compaction, [
        (attr::COVERS, Value::List(covers)),
        (attr::MODEL, Value::from(smol_model)),
        (attr::CREATED, Value::Int(loa_core::now_ms())),
    ], summary.as_str()))?;
    seed_context(&mut next, &summary);
    let sink = store.fresh(&next.session_id).await?;
    let harness = adapter.start(next.clone(), sink.clone()).await?;
    lineage(sink.as_ref(), NodeId(1), session_id(source)?, source.seq())?;
    running.shutdown().await?;
    Ok(Some(RunningSession { config: next, sink, harness }))
}

fn dom_turn(dom: &Dom, id: NodeId) -> bool {
    dom.get(id).is_some_and(|node| node.kind == ElementKind::Turn)
}

/// Creates and starts a linked child harness session. The parent job records
/// the child session id, while the child's root records the parent session.
pub async fn subagent(
    adapter: &dyn Adapter, store: &dyn SessionStore,
    parent: &Dom, parent_sink: &dyn PatchSink, parent_node: NodeId,
    config: SessionConfig, prompt: Prompt,
) -> Result<(NodeId, RunningSession)> {
    let parent_id = session_id(parent)?;
    let parent_kind = parent.get(parent_node).map(|n| n.kind);
    if !matches!(parent_kind, Some(ElementKind::Session | ElementKind::Turn | ElementKind::Call)) {
        return Err(Error::Port("subagent job parent must be a session, turn or call".into()));
    }
    let sink = store.fresh(&config.session_id).await?;
    let harness = adapter.start(config.clone(), sink.clone()).await?;
    lineage(sink.as_ref(), NodeId(1), parent_id, parent.seq())?;
    let job = parent_sink.commit(Draft::insert(parent_node, ElementKind::Job, [
        (attr::KIND, Value::from("subagent")),
        (attr::STATUS, Value::from("running")),
        (attr::SESSION, Value::from(config.session_id.as_str())),
        (attr::STARTED, Value::Int(loa_core::now_ms())),
    ], ""))?.node;
    if let Err(error) = harness.send(prompt).await {
        parent_sink.commit(Draft::set(job, attr::STATUS, "error"))?;
        parent_sink.commit(Draft::set(job, attr::ENDED, loa_core::now_ms()))?;
        return Err(error.into());
    }
    Ok((job, RunningSession { config, sink, harness }))
}

/// Finalizes a linked job after the child has stopped, without losing the
/// child's own conversation or assuming its final output is a single message.
pub async fn finish_subagent(child: &dyn HarnessSession, parent_sink: &dyn PatchSink, job: NodeId) -> Result<()> {
    child.wait_idle().await?;
    parent_sink.commit(Draft::set(job, attr::STATUS, "done"))?;
    parent_sink.commit(Draft::set(job, attr::ENDED, loa_core::now_ms()))?;
    Ok(())
}

/// Entering plan mode requires an actual read-only tool gate. The permit must
/// be held for the whole mode, including between turns.
pub async fn enter_plan(
    session: &dyn HarnessSession, dom: &Dom, sink: &dyn PatchSink,
    tools: &dyn ReadOnlyTools,
) -> Result<(Box<dyn PlanPermit>, NodeId)> {
    if session.state() != SessionState::Idle { return Err(Error::Busy); }
    let permit = tools.activate(session_id(dom)?).await?;
    if let Err(error) = session.set_mode(SessionMode::Plan).await {
        // A missing native plan mode is expected; the tool gate is the
        // operative fallback. Other errors must not be hidden.
        if !matches!(error, AdapterError::Unsupported { .. }) { return Err(error.into()); }
    }
    let todo = plan::begin(dom, sink)?;
    Ok((permit, todo))
}

/// Submit a plan for user review. The gate remains active until the user
/// answers; callers keep the permit until [`plan::answer_plan`].
pub fn submit_plan(sink: &dyn PatchSink, session: NodeId, todo: NodeId, plan: &str) -> Result<NodeId> {
    sink.commit(Draft::set(todo, attr::STATE, "done"))?;
    Ok(sink.commit(Draft::insert(session, ElementKind::Decision, [
        (attr::KIND, Value::from("plan")),
        (attr::STATUS, Value::from("waiting")),
        (attr::CREATED, Value::Int(loa_core::now_ms())),
    ], plan))?.node)
}

/// Assigns a model to a role, switching immediately before the appropriate
/// call. `PerTurn` can only switch while idle; `PerCall` works mid-turn.
pub async fn select_model(
    session: &dyn HarnessSession, capabilities: &loa_contracts::harness::Capabilities,
    sink: &dyn PatchSink, root: NodeId, role: &str, model: &str,
) -> Result<()> {
    match capabilities.model_switch {
        ModelSwitch::Never => return Err(Error::ModelFixed),
        ModelSwitch::PerTurn if session.state() != SessionState::Idle => return Err(Error::Busy),
        _ => {}
    }
    session.set_model(ModelSelection { model: model.into(), role: None }).await?;
    sink.commit(Draft::set(root, attr::MODEL, model))?;
    sink.commit(Draft::set(root, attr::MODEL_ROLE, role))?;
    Ok(())
}

/// Switches the session to `role`, trying its candidate models in order
/// (omp's `modelRoles.<role>`: the first available wins). Returns the model
/// picked. Model-switch errors of the harness move on to the next
/// candidate; [`Error::ModelFixed`] and [`Error::Busy`] stop at once.
pub async fn select_role(
    session: &dyn HarnessSession, capabilities: &loa_contracts::harness::Capabilities,
    sink: &dyn PatchSink, root: NodeId, role: &str, candidates: &[String],
) -> Result<String> {
    let mut last = None;
    for model in candidates {
        match select_model(session, capabilities, sink, root, role, model).await {
            Ok(()) => return Ok(model.clone()),
            Err(error @ (Error::ModelFixed | Error::Busy)) => return Err(error),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| Error::Port(format!("no model is assigned to the {role} role"))))
}
