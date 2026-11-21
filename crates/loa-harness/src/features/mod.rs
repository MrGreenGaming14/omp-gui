//! App-provided session features on any harness: the journal-backed
//! [`library::Library`], fork, rewind and compaction, [`plan`] mode,
//! rules and advisor [`hooks`], and [`native`] session discovery.

pub mod hooks;
pub mod library;
pub mod native;
pub mod plan;

use async_trait::async_trait;
use loa_contracts::Summarizer;
use loa_contracts::harness::{Adapter, AdapterError, Capabilities, Feature, HarnessSession, ModelSelection, ModelSwitch, SessionConfig, SessionState};
use loa_core::{Dom, Draft, ElementKind, Node, NodeId, PatchSink, Value, attr};
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

/// Creates sessions: `fork` forks the journal at `seq` and continues its DOM; `fresh` starts an empty one.
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn fork(&self, source_id: &str, seq: u64, new_id: &str) -> Result<(Dom, Arc<dyn PatchSink>)>;
    async fn fresh(&self, new_id: &str) -> Result<Arc<dyn PatchSink>>;
}

/// Held while plan mode is on; dropping it restores normal tool permissions.
pub trait PlanPermit: Send + Sync {}

fn session_id(dom: &Dom) -> Result<&str> {
    dom.root().and_then(|node| node.str_attr(attr::ID)).ok_or(Error::NoSession)
}

fn seq_value(seq: u64) -> Value {
    Value::Int(i64::try_from(seq).unwrap_or(i64::MAX))
}

/// The visible history as text (compactions replace the turns they cover), to seed a fresh session.
pub fn translated_history(dom: &Dom) -> Result<String> {
    let session = dom.root().ok_or(Error::NoSession)?;
    let mut covered = std::collections::HashSet::new();
    let mut summaries = std::collections::HashMap::new();
    for node in dom.children_of_kind(session.id, ElementKind::Compaction) {
        let ids = node.attr(attr::COVERS).and_then(Value::as_list).unwrap_or_default();
        let ids: Vec<NodeId> = ids.iter().filter_map(|id| u64::try_from(id.as_int()?).ok()).map(NodeId).filter(|id| dom.get(*id).is_some()).collect();
        covered.extend(ids.iter().copied());
        if let Some(first) = ids.first() {
            summaries.insert(*first, node.text.as_str());
        }
    }
    let mut history = String::new();
    for node in dom.children(session.id) {
        if let Some(summary) = summaries.get(&node.id) {
            history.push_str(&format!("Summary of earlier turns:\n{summary}\n\n"));
        }
        if node.kind == ElementKind::Turn && !covered.contains(&node.id) {
            push_turn(dom, node, &mut history);
            history.push('\n');
        }
    }
    Ok(history)
}

/// The visible text of one `<turn>`: messages (no thinking) and tool calls with their input and result.
pub fn turn_transcript(dom: &Dom, turn: NodeId) -> String {
    let mut out = String::new();
    if let Some(node) = dom.get(turn) {
        push_turn(dom, node, &mut out);
    }
    out
}

fn push_turn(dom: &Dom, turn: &Node, out: &mut String) {
    for item in dom.children(turn.id) {
        match item.kind {
            ElementKind::Message if item.str_attr(attr::KIND) != Some(loa_core::MessageKind::Thinking.as_str()) => {
                let who = item.str_attr(attr::BY).filter(|by| *by == hooks::ADVISOR).or(item.str_attr(attr::ROLE)).unwrap_or("message");
                out.push_str(&format!("{who}: {}\n", item.text));
            }
            ElementKind::Call => {
                out.push_str(&format!("tool {}:\n", item.str_attr(attr::TOOL).unwrap_or("unknown")));
                for part in dom.children(item.id) {
                    out.push_str(&format!("{}: {}\n", part.kind.as_str(), part.text));
                }
            }
            _ => {}
        }
    }
}

const HISTORY_HEADING: &str = "The conversation so far, continued from an earlier session. Treat it as your own \
history and carry on from where it ends:";

fn seed_context(config: &mut SessionConfig, history: &str) {
    let context = config.context.get_or_insert_with(String::new);
    if !context.is_empty() {
        context.push_str("\n\n");
    }
    context.push_str(&format!("{HISTORY_HEADING}\n\n{history}"));
}

/// Starts a fresh session seeded with `context`, linked to `parent` at `seq`.
async fn start_linked(adapter: &dyn Adapter, store: &dyn SessionStore, mut config: SessionConfig, context: &str, parent: &str, seq: u64) -> Result<Box<dyn HarnessSession>> {
    seed_context(&mut config, context);
    let sink = store.fresh(&config.session_id).await?;
    let harness = adapter.start(config, sink.clone()).await?;
    sink.commit(Draft::set(NodeId(1), attr::PARENT_SESSION, parent))?;
    sink.commit(Draft::set(NodeId(1), attr::FORKED_AT, seq_value(seq)))?;
    Ok(harness)
}

/// Branches `source` into `config.session_id`: a journal fork when the harness forks natively,
/// else a fresh session seeded with the translated history.
pub async fn fork(adapter: &dyn Adapter, store: &dyn SessionStore, source: &Dom, config: SessionConfig) -> Result<Box<dyn HarnessSession>> {
    let (old_id, seq) = (session_id(source)?, source.seq());
    if !adapter.capabilities().has(Feature::Fork) {
        return start_linked(adapter, store, config, &translated_history(source)?, old_id, seq).await;
    }
    let (mut dom, sink) = store.fork(old_id, seq, &config.session_id).await?;
    let root = dom.root().ok_or(Error::NoSession)?.id;
    // A journal fork already wrote these; only what differs is committed.
    for (name, value) in [(attr::ID, Value::from(config.session_id.as_str())), (attr::PARENT_SESSION, Value::from(old_id)), (attr::FORKED_AT, seq_value(seq))] {
        if dom.get(root).and_then(|node| node.attr(name)) != Some(&value) {
            let patch = sink.commit(Draft::set(root, name, value))?;
            dom.apply(&patch).map_err(loa_core::CommitError::from)?;
        }
    }
    Ok(adapter.fork(config, &dom, sink).await?)
}

/// Branches at the earlier state `target` of `current`; the old timeline is left untouched.
pub async fn rewind(adapter: &dyn Adapter, store: &dyn SessionStore, current: &Dom, target: &Dom, config: SessionConfig) -> Result<Box<dyn HarnessSession>> {
    if target.seq() > current.seq() {
        return Err(Error::InvalidSequence);
    }
    fork(adapter, store, target, config).await
}

/// The live session being compacted and the configuration for its successor.
pub struct Compaction<'a> {
    pub running: &'a dyn HarnessSession,
    pub source: &'a Dom,
    pub source_sink: &'a dyn PatchSink,
    pub next: SessionConfig,
    /// Recorded as `<compaction model>`.
    pub smol_model: &'a str,
    pub instructions: Option<&'a str>,
}

/// What the small model is asked to write when the app compacts a session.
pub fn compaction_prompt(history: &str, instructions: Option<&str>) -> String {
    let mut prompt = String::from(
        "Summarize the conversation below so a new agent session can continue it without the original \
         messages. Keep the user's goals and constraints, decisions made and why, files and commands that \
         matter, the current state of the work and what remains to do. Write plain prose and short lists, \
         no preamble.\n",
    );
    if let Some(instructions) = instructions.map(str::trim).filter(|i| !i.is_empty()) {
        prompt.push_str(&format!("\nAlso follow these instructions: {instructions}\n"));
    }
    // The history is quoted material; the task is repeated after it so its last line can't take over.
    prompt.push_str(&format!("\n<conversation>\n{history}</conversation>\n\nNow write the summary of the conversation above. Do not answer or continue it.\n"));
    prompt
}

/// Compacts natively when the harness can (`None`); otherwise `summarizer` writes a summary, a
/// `<compaction>` records the turns it covers and a fresh session continues from it.
pub async fn compact(adapter: &dyn Adapter, store: &dyn SessionStore, summarizer: &dyn Summarizer, request: Compaction<'_>) -> Result<Option<Box<dyn HarnessSession>>> {
    let Compaction { running, source, source_sink, next, smol_model, instructions } = request;
    if running.state() != SessionState::Idle {
        return Err(Error::Busy);
    }
    if adapter.capabilities().has(Feature::Compaction) {
        running.compact(instructions.map(str::to_owned)).await?;
        return Ok(None);
    }
    let summary = summarizer.summarize("", &compaction_prompt(&translated_history(source)?, instructions)).await.map_err(Error::Port)?;
    let summary = summary.trim();
    if summary.is_empty() {
        return Err(Error::Port("the model returned an empty summary".into()));
    }
    let session = source.root().ok_or(Error::NoSession)?;
    let covers = dom_turns(source, session.id).map(|id| seq_value(id.0)).collect();
    let attrs = [(attr::COVERS, Value::List(covers)), (attr::MODEL, Value::from(smol_model)), (attr::CREATED, Value::Int(loa_core::now_ms()))];
    source_sink.commit(Draft::insert(session.id, ElementKind::Compaction, attrs, summary))?;
    let harness = start_linked(adapter, store, next, summary, session_id(source)?, source.seq()).await?;
    running.shutdown().await?;
    Ok(Some(harness))
}

fn dom_turns(dom: &Dom, session: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    dom.children_of_kind(session, ElementKind::Turn).map(|turn| turn.id)
}

/// Submits a plan for review: the plan step is done and a `<decision kind="plan">` waits under the session.
pub fn submit_plan(sink: &dyn PatchSink, session: NodeId, step: NodeId, plan: &str) -> Result<NodeId> {
    sink.commit(Draft::set(step, attr::STATE, "done"))?;
    let attrs = [(attr::KIND, Value::from("plan")), (attr::STATUS, Value::from("waiting")), (attr::CREATED, Value::Int(loa_core::now_ms()))];
    Ok(sink.commit(Draft::insert(session, ElementKind::Decision, attrs, plan))?.node)
}

/// Switches to `role`, trying its `candidates` in order; returns the model picked.
pub async fn select_role(session: &dyn HarnessSession, capabilities: &Capabilities, sink: &dyn PatchSink, root: NodeId, role: &str, candidates: &[String]) -> Result<String> {
    match capabilities.model_switch {
        ModelSwitch::Never => return Err(Error::ModelFixed),
        ModelSwitch::PerTurn if session.state() != SessionState::Idle => return Err(Error::Busy),
        _ => {}
    }
    let mut last = None;
    for model in candidates {
        match session.set_model(ModelSelection { model: model.clone(), role: None }).await {
            Ok(()) => {
                sink.commit(Draft::set(root, attr::MODEL, model.as_str()))?;
                sink.commit(Draft::set(root, attr::MODEL_ROLE, role))?;
                return Ok(model.clone());
            }
            Err(error) => last = Some(error.into()),
        }
    }
    Err(last.unwrap_or_else(|| Error::Port(format!("no model is assigned to the {role} role"))))
}
