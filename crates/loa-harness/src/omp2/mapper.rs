//! omp2 DOM records to app patches: a mirror of omp2's tree (structure and props, not streamed text) whose handles
//! bind to app nodes, recorded as [`HANDLE_ATTR`]. Names map through the tables below; the rest lands in `x-` attributes or `<ext>`.

use super::HARNESS;
use super::wire::{BODY, DomEvent, Frame, Handle, Input, NodeSpec, Op, ROOT, Reply, Snapshot, SnapshotNode, StreamOp, Tag};
use crate::kit::rpc::id_key;
use crate::kit::{EventMapper, TurnLedger};
use loa_contracts::harness::{AdapterError, Answer, Prompt, Result};
use loa_core::schema::{self, TEXT};
use loa_core::{Attrs, DecisionKind, DecisionStatus, Dom, DomWriter, Draft, ElementKind as K, NodeId, TurnStatus, Value, ValueType, answer, attr};
use serde_json::Value as Json;
use std::collections::{BTreeMap, HashMap, HashSet};

/// The omp2 handle an app node was made from.
pub const HANDLE_ATTR: &str = "x-omp2-handle";
/// omp2's journal order, rewritten on every append: not worth a patch per token.
const DROPPED: &str = "order";

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Element { Turn, User, Block, Call, Input, Result, Usage, Todo, Job, Subagent, Compaction }
use Element as E;

impl Element {
    fn kind(self) -> K {
        match self {
            E::Turn => K::Turn, E::User | E::Block => K::Message, E::Call => K::Call, E::Input => K::Input, E::Result => K::Result,
            E::Usage => K::Usage, E::Todo => K::Todo, E::Job | E::Subagent => K::Job, E::Compaction => K::Compaction,
        }
    }
}

/// The session root, a container flattened into its parent, the `<assistant>` whose content blocks become messages,
/// an element, or `<ext>`.
#[derive(Copy, Clone, PartialEq)]
enum Rule { Root, Flatten, Assistant, Element(Element), Ext }

const ELEMENTS: [(&str, Element); 9] = [("turn", E::Turn), ("user", E::User), ("item", E::Todo), ("job", E::Job), ("subagent", E::Subagent), ("compaction", E::Compaction), ("input", E::Input), ("result", E::Result), ("usage", E::Usage)];

fn rule(tag: &Tag, parent_tag: &Tag, parent: Option<K>) -> Rule {
    match (parent, tag.custom, tag.name.as_str()) {
        (None, ..) => Rule::Ext,
        (Some(K::Turn), true, "assistant-content") if parent_tag.is("assistant") => Rule::Element(E::Block),
        (Some(K::Turn), true, _) if parent_tag.is("turn") => Rule::Element(E::Call),
        (_, true, _) => Rule::Ext,
        (_, _, "session") => Rule::Root,
        (_, _, "meta" | "body" | "todo" | "jobs") => Rule::Flatten,
        (_, _, "assistant") => Rule::Assistant,
        (_, _, name) => ELEMENTS.iter().find(|(n, _)| *n == name).map_or(Rule::Ext, |(_, e)| Rule::Element(*e)),
    }
}

/// The attribute (or [`TEXT`]) `prop` of `e` maps to; `None` keeps it as `x-<prop>`.
fn dest(e: Element, prop: &str) -> Option<&'static str> {
    Some(match (e, prop) {
        (E::Turn | E::User | E::Job | E::Subagent, "id") => attr::ID,
        (E::User | E::Block | E::Input | E::Result, "text") | (E::Todo, "label") | (E::Compaction, "summary") => TEXT,
        (E::User, "author") => attr::BY,
        (E::Block, "kind") => attr::KIND,
        (E::Call, "id") => attr::CALL_ID,
        (E::Call | E::Job | E::Subagent, "status") => attr::STATUS,
        (E::Call, "i") | (E::Job | E::Subagent, "label") => attr::TITLE,
        (E::Job | E::Subagent, "agent") => attr::AGENT,
        (E::Job | E::Subagent, "exit") => attr::EXIT_CODE,
        (E::Result, "truncated") => attr::TRUNCATED,
        (E::Usage, "tokens-in") => attr::INPUT_TOKENS,
        (E::Usage, "tokens-out") => attr::OUTPUT_TOKENS,
        (E::Usage, "cache-read") => attr::CACHE_READ_TOKENS,
        (E::Usage, "cache-write") => attr::CACHE_WRITE_TOKENS,
        (E::Usage, "cost-nano-usd") => attr::COST,
        (E::Usage, "duration-ms") => attr::DURATION_MS,
        (E::Usage, "model") => attr::MODEL,
        (E::Todo, "status") => attr::STATE,
        (E::Todo, "phase") => attr::PHASE,
        (E::Compaction, "tokens-before") => attr::TOKENS_BEFORE,
        _ => return None,
    })
}

/// The schema value of an omp2 enum value.
fn enum_value(e: Element, name: &str, raw: &str) -> Option<&'static str> {
    let table: &[(&str, &'static str)] = match (e, name) {
        (E::Call, attr::STATUS) => &[("arguments", "pending"), ("running", "running"), ("ok", "done"), ("error", "error"), ("cancelled", "cancelled"), ("aborted", "cancelled")],
        (E::Todo, attr::STATE) => &[("pending", "pending"), ("blocked", "pending"), ("in_progress", "in_progress"), ("completed", "done"), ("abandoned", "cancelled")],
        (E::Job | E::Subagent, attr::STATUS) => &[("running", "running"), ("completed", "done"), ("done", "done"), ("ok", "done"), ("failed", "error"), ("error", "error"), ("cancelled", "cancelled"), ("killed", "cancelled"), ("aborted", "cancelled")],
        (E::Block, attr::KIND) => &[("text", "text"), ("thinking", "thinking")],
        _ => &[],
    };
    table.iter().find(|(k, _)| *k == raw).map(|(_, v)| *v)
}

fn fixed_attrs(e: Element, tag: &Tag) -> Attrs {
    let fixed: &[(&str, &str)] = match e {
        E::Turn => &[(attr::STATUS, "running")],
        E::User => &[(attr::ROLE, "user"), (attr::STATUS, "done")],
        E::Block => &[(attr::ROLE, "assistant"), (attr::STATUS, "streaming")],
        E::Call => &[(attr::TOOL, tag.name.as_str()), (attr::STATUS, "running")],
        E::Input => &[(attr::FORMAT, "json")],
        E::Job => &[(attr::KIND, "shell")],
        E::Subagent => &[(attr::KIND, "subagent")],
        _ => &[],
    };
    fixed.iter().map(|(k, v)| ((*k).to_owned(), Value::from(*v))).collect()
}

/// A content block's final status given the assistant's `stop-reason`.
fn stop_status(reason: &str) -> &'static str {
    match reason { "aborted" | "cancelled" => "cancelled", "error" => "error", _ => "done" }
}

fn to_value(v: &Json) -> Option<Value> {
    Some(match v {
        Json::Null => return None,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => n.as_i64().map_or_else(|| Value::Float(n.as_f64().unwrap_or(f64::NAN)), Value::Int),
        Json::String(s) => Value::Str(s.clone()),
        other => Value::Str(other.to_string()),
    })
}

/// The attribute (or [`TEXT`]) and value `prop = v` of `e` writes; values that don't fit the schema stay `x-<prop>`.
fn element_write(e: Element, prop: &str, v: &Json) -> (String, Option<Value>) {
    let mapped = match dest(e, prop) {
        Some(name) if v.is_null() => Some((name, None)),
        Some(TEXT) => v.as_str().map(|t| (TEXT, Some(t.into()))),
        Some(attr::COST) => v.as_f64().map(|nano| (attr::COST, Some(Value::Float(nano / 1e9)))),
        Some(name) => e.kind().spec().attr(name).and_then(|spec| match (v, spec.ty) {
            _ if !spec.values.is_empty() => v.as_str().and_then(|raw| enum_value(e, name, raw)).map(Value::from),
            (Json::Bool(_), ValueType::Bool) | (Json::String(_) | Json::Array(_) | Json::Object(_), ValueType::Str) => to_value(v),
            (Json::Number(n), ValueType::Int) => n.as_i64().map(Value::Int),
            (Json::Number(n), ValueType::Float) => n.as_f64().map(Value::Float),
            _ => None,
        }).map(|value| (name, Some(value))),
        None => None,
    };
    mapped.map_or_else(|| (x(prop), to_value(v)), |(name, value)| (name.to_owned(), value))
}

fn x(prop: &str) -> String { format!("{}{prop}", schema::EXTENSION_PREFIX) }
fn handle_value(h: Handle) -> Value { Value::Int(i64::try_from(h.0).unwrap_or(i64::MAX)) }
fn bad(what: &str) -> AdapterError { AdapterError::Protocol(format!("malformed omp2 event: {what}")) }
fn unknown(h: Handle) -> AdapterError { AdapterError::Protocol(format!("omp2 named node {}, which it never inserted", h.0)) }

/// Where an omp2 node went: not yet anywhere, the root, flattened, the assistant, or an app node (`None` is `<ext>`).
#[derive(Copy, Clone, Debug, PartialEq)]
enum Target { Unset, Root, Flat, Assistant, Node(NodeId, Option<Element>) }

struct Node { tag: Tag, parent: Option<Handle>, kids: Vec<Handle>, props: Vec<(String, Json)>, content: Option<String>, target: Target }

impl Node {
    fn new(spec: NodeSpec, parent: Option<Handle>) -> Self {
        let props = spec.props.into_iter().map(|(p, v)| (p.0, v)).filter(|(p, _)| p != DROPPED).collect();
        Node { tag: spec.tag, parent, kids: spec.kids, props, content: spec.content, target: Target::Unset }
    }

    fn prop(&self, name: &str) -> Option<&Json> { self.props.iter().find(|(key, _)| key == name).map(|(_, v)| v) }
}

/// Where a stream's appends go: node text, a prop set on close, or nowhere.
enum Stream { Text(NodeId), Prop(Handle, String, String), Ignored }

enum Pending { Approval(String), Select { id: Json, options: Vec<String> } }

/// The DOM side of one omp2 session.
pub struct Omp2Mapper {
    ledger: TurnLedger<String>,
    high_water: u64,
    tree: HashMap<Handle, Node>,
    ids: HashMap<Handle, NodeId>,
    /// App node to its handle and app parent.
    back: HashMap<NodeId, (Handle, NodeId)>,
    streams: HashMap<u32, Stream>,
    /// Open stream text by (node, prop) while a snapshot is materialized.
    seed: HashMap<(Handle, String), String>,
    /// The turn and user message `begin` inserted, until omp2's own bind to them.
    pending: Option<(NodeId, Option<NodeId>)>,
    /// The latest turn.
    turn: Option<NodeId>,
    /// Streaming messages (`false`) and unfinished calls (`true`).
    open: BTreeMap<NodeId, bool>,
    calls: HashMap<String, NodeId>,
    decisions: HashMap<NodeId, Pending>,
}

impl Omp2Mapper {
    /// A mapper for a new session whose `<session>` is `root`.
    pub fn new(root: NodeId) -> Self { Self::with_ledger(TurnLedger::new(root)) }

    fn with_ledger(ledger: TurnLedger<String>) -> Self {
        let (tree, ids, back, streams, seed, calls, decisions) = Default::default();
        Self { ledger, high_water: 0, tree, ids, back, streams, seed, pending: None, turn: None, open: BTreeMap::new(), calls, decisions }
    }

    /// Continues `dom`, a session this adapter wrote; omp2 must send a snapshot first.
    pub fn from_dom(dom: &Dom) -> Self {
        let mut mapper = Self::with_ledger(TurnLedger::from_dom(dom));
        for node in dom.iter() {
            if let Some(h) = node.int_attr(HANDLE_ATTR).and_then(|h| u64::try_from(h).ok()) {
                mapper.bind(Handle(h), node.id, node.parent);
            }
            if let (K::Call, Some(call_id)) = (node.kind, node.str_attr(attr::CALL_ID)) {
                mapper.calls.insert(call_id.to_owned(), node.id);
            }
        }
        mapper.turn = dom.root().and_then(|root| dom.children_of_kind(root.id, K::Turn).last().map(|n| n.id));
        mapper
    }

    /// Inserts the turn and user message of a prompt about to be sent.
    pub fn begin(&mut self, w: &DomWriter, prompt: &Prompt) -> Result<NodeId> {
        let attrs = Attrs::from([(attr::STATUS.to_owned(), Value::from("running")), (attr::STARTED.to_owned(), Value::Int(w.now()))]);
        let turn = w.commit(Draft::Insert { parent: self.ledger.session, index: self.turn_index(), kind: K::Turn, attrs, text: String::new() })?;
        let user = w.insert(turn, K::Message, [(attr::ROLE, "user"), (attr::STATUS, "done"), (attr::BY, prompt.by.as_str())], prompt.text.as_str())?;
        (self.pending, self.turn) = (Some((turn, Some(user))), Some(turn));
        self.ledger.adopt(turn);
        Ok(turn)
    }

    /// Ends the latest turn: open messages and calls close to match `status`, then the turn.
    pub fn finish(&mut self, w: &DomWriter, status: TurnStatus) -> Result<()> {
        self.pending = None;
        if self.running_turn().is_none() {
            return Ok(());
        }
        let (message, call) = match status { TurnStatus::Done => ("done", "cancelled"), TurnStatus::Error => ("error", "error"), _ => ("cancelled", "cancelled") };
        for (id, is_call) in std::mem::take(&mut self.open) {
            if is_call { w.close(id, call)? } else { w.set(id, attr::STATUS, message)? }
        }
        Ok(self.ledger.end_turn(w, status, |_| {})?)
    }

    /// The latest turn, opened in the ledger when omp2 started it.
    fn running_turn(&mut self) -> Option<NodeId> {
        if self.ledger.turn().is_none() && let Some(turn) = self.turn {
            self.ledger.adopt(turn);
        }
        self.turn
    }

    fn turn_seen(&mut self, id: NodeId) {
        self.turn = Some(id);
        if self.ledger.turn().is_some_and(|t| t.node != id) {
            self.ledger.adopt(id);
        }
    }

    fn frame(&mut self, w: &DomWriter, frame: Frame) -> Result<()> {
        match frame {
            Frame::AgentEnd { cancelled, error } => {
                for (decision, _) in std::mem::take(&mut self.decisions) {
                    w.close(decision, DecisionStatus::Cancelled)?;
                    self.ledger.answered(decision);
                }
                if let (Some(error), Some(turn)) = (&error, self.turn) {
                    w.ext(turn, HARNESS, "error", error)?;
                }
                self.finish(w, if error.is_some() { TurnStatus::Error } else if cancelled { TurnStatus::Cancelled } else { TurnStatus::Done })
            }
            Frame::ToolApprovalRequest { prompt_id, tool_call_id, title, body, timeout_ms } => {
                let call = tool_call_id.and_then(|c| self.calls.get(&c).copied());
                let mut attrs = vec![(attr::KIND, Value::from(DecisionKind::Approval)), (attr::REQUEST_ID, prompt_id.as_str().into())];
                attrs.extend(call.map(|c| (attr::CALL, c.into())).into_iter().chain(title.map(|t| (attr::TITLE, t.into()))).chain(timeout_ms.map(|t| (attr::TIMEOUT_MS, t.into()))));
                self.decide(w, call, attrs, body.unwrap_or_default(), Pending::Approval(prompt_id))
            }
            Frame::ExtensionUiRequest { id, method, title, options, multi } if method == "select" => {
                let attrs = vec![(attr::KIND, Value::from(DecisionKind::Question)), (attr::REQUEST_ID, id_key(&id).into()), (attr::OPTIONS, options.clone().into()), (attr::MULTI, multi.into())];
                self.decide(w, None, attrs, title.unwrap_or_default(), Pending::Select { id, options })
            }
            _ => Ok(()),
        }
    }

    fn decide(&mut self, w: &DomWriter, call: Option<NodeId>, mut attrs: Vec<(&str, Value)>, text: String, pending: Pending) -> Result<()> {
        let turn = self.running_turn();
        attrs.extend([(attr::STATUS, DecisionStatus::Waiting.into()), (attr::CREATED, Value::Int(w.now()))]);
        let decision = w.insert(call.or(turn).unwrap_or(self.ledger.session), K::Decision, attrs, text)?;
        self.decisions.insert(decision, pending);
        if let Some(turn) = turn {
            w.set(turn, attr::STATUS, TurnStatus::Waiting)?;
            self.ledger.wait(decision);
        }
        Ok(())
    }

    fn dom(&mut self, w: &DomWriter, event: DomEvent) -> Result<()> {
        if self.tree.is_empty() && !matches!(event, DomEvent::Snapshot { .. }) {
            if !self.ids.is_empty() {
                return Err(bad("a resumed session must start with a snapshot"));
            }
            // The `<session><meta/><body/><queues/></session>` every omp2 DOM starts with.
            let node = |h, tag: &str, kids: &[u64]| SnapshotNode { handle: Handle(h), parent: (h > 1).then_some(ROOT), node: NodeSpec { tag: tag.to_owned().into(), props: Vec::new(), kids: kids.iter().copied().map(Handle).collect(), content: None } };
            let nodes = vec![node(1, "session", &[2, 3, 4]), node(2, "meta", &[]), node(3, "body", &[]), node(4, "queues", &[])];
            self.reset(w, Snapshot { high_water: 4, nodes, streams: Vec::new() })?;
        }
        match event {
            DomEvent::Patch { data } => data.ops.into_iter().try_for_each(|op| self.op(w, op)),
            DomEvent::Stream { sid, op, node, prop, text } => self.stream(w, sid, op, node.zip(prop.map(|p| p.0)), text),
            DomEvent::Snapshot { data } => self.reset(w, data),
        }
    }

    fn node(&self, h: Handle) -> Result<&Node> { self.tree.get(&h).ok_or_else(|| unknown(h)) }
    fn kids(&self, h: Handle) -> &[Handle] { self.tree.get(&h).map_or(&[], |n| &n.kids) }
    fn target(&self, h: Handle) -> Option<Target> { self.tree.get(&h).map(|n| n.target) }
    fn flat(&self, h: Handle) -> bool { matches!(self.target(h), Some(Target::Flat | Target::Assistant)) }

    fn set_target(&mut self, h: Handle, target: Target) {
        if let Some(node) = self.tree.get_mut(&h) {
            node.target = target;
        }
    }

    fn bind(&mut self, h: Handle, id: NodeId, parent: NodeId) {
        self.ids.insert(h, id);
        self.back.insert(id, (h, parent));
    }

    /// The app node children of `at` go under, and its kind (`None` for `<ext>`).
    fn app_parent_of(&self, mut at: Handle) -> Result<(NodeId, Option<K>)> {
        loop {
            let node = self.node(at)?;
            match node.target {
                Target::Root => return Ok((self.ledger.session, Some(K::Session))),
                Target::Node(id, e) => return Ok((id, e.map(Element::kind))),
                Target::Flat | Target::Assistant => at = node.parent.ok_or_else(|| bad("a container without a parent"))?,
                Target::Unset => return Err(unknown(at)),
            }
        }
    }

    /// Position of `h`'s app node among its app siblings; `None` appends.
    fn app_index(&self, h: Handle) -> Option<u32> {
        let mut origin = self.tree.get(&h).and_then(|n| n.parent);
        while let Some(at) = origin.filter(|at| self.flat(*at)) {
            origin = self.tree[&at].parent;
        }
        let mut stack: Vec<Handle> = self.kids(origin.unwrap_or(ROOT)).iter().rev().copied().collect();
        let (mut count, mut seen) = (0, false);
        while let Some(at) = stack.pop() {
            match self.target(at) {
                _ if at == h => seen = true,
                Some(Target::Node(..)) if seen => return Some(count),
                Some(Target::Node(..)) => count += 1,
                _ if self.flat(at) => stack.extend(self.kids(at).iter().rev()),
                _ => {}
            }
        }
        None
    }

    /// The outermost app nodes of `h`'s subtree.
    fn app_nodes(&self, h: Handle, out: &mut Vec<NodeId>) {
        match self.target(h) {
            Some(Target::Node(id, _)) => out.push(id),
            Some(Target::Flat | Target::Assistant | Target::Root) => self.kids(h).iter().for_each(|k| self.app_nodes(*k, out)),
            _ => {}
        }
    }

    /// Where a new turn goes: after what came from `<meta>` and `<body>`, before `<queues>`.
    fn turn_index(&self) -> Option<u32> {
        let kids = self.kids(ROOT);
        let body = kids.iter().position(|k| *k == BODY)?;
        let count = |hs: &[Handle]| hs.iter().map(|h| { let mut out = Vec::new(); self.app_nodes(*h, &mut out); out.len() }).sum::<usize>();
        let index = u32::try_from(count(&kids[..=body])).ok();
        index.filter(|_| count(&kids[body + 1..]) > 0)
    }

    /// True when `id` or an app ancestor of it is in `set`.
    fn under(&self, id: NodeId, set: &HashSet<NodeId>) -> bool {
        std::iter::successors(Some(id), |n| self.back.get(n).map(|(_, p)| *p)).any(|n| set.contains(&n))
    }

    fn forget(&mut self, gone: &HashSet<NodeId>) {
        self.open.retain(|id, _| !gone.contains(id));
        self.calls.retain(|_, id| !gone.contains(id));
        self.streams.retain(|_, s| !matches!(s, Stream::Text(id) if gone.contains(id)));
        self.turn = self.turn.filter(|t| !gone.contains(t));
    }

    /// Removes the app nodes of `h`'s subtree and unlinks it, keeping its mirror.
    fn detach(&mut self, w: &DomWriter, h: Handle) -> Result<()> {
        let mut doomed = Vec::new();
        self.app_nodes(h, &mut doomed);
        doomed.iter().try_for_each(|id| w.remove(*id))?;
        let (mut stack, mut gone) = (vec![h], HashSet::new());
        while let Some(at) = stack.pop() {
            gone.extend(self.ids.remove(&at).inspect(|id| { self.back.remove(id); }));
            if let Some(node) = self.tree.get_mut(&at) {
                gone.extend(match node.target { Target::Node(id, _) => Some(id), _ => None });
                node.target = Target::Unset;
                stack.extend(node.kids.iter().copied());
            }
        }
        self.forget(&gone);
        if let Some(parent) = self.node(h)?.parent.and_then(|p| self.tree.get_mut(&p)) {
            parent.kids.retain(|k| *k != h);
        }
        Ok(())
    }

    /// Puts `h` after `after` among `parent`'s kids; `None` or an unknown anchor is first.
    fn link(&mut self, h: Handle, parent: Handle, after: Option<Handle>) -> Result<()> {
        let node = self.tree.get_mut(&parent).ok_or_else(|| unknown(parent))?;
        let at = after.and_then(|a| node.kids.iter().position(|k| *k == a)).map_or(0, |i| i + 1);
        node.kids.insert(at, h);
        Ok(())
    }

    fn op(&mut self, w: &DomWriter, op: Op) -> Result<()> {
        match op {
            Op::Ins(_, parent, after, spec) => {
                self.node(parent)?;
                self.high_water += 1;
                let h = Handle(self.high_water);
                self.tree.insert(h, Node::new(spec, Some(parent)));
                self.link(h, parent, after)?;
                self.materialize(w, h, true)
            }
            Op::Rm(_, h) => {
                self.node(h)?;
                self.detach(w, h)?;
                let mut stack = vec![h];
                while let Some(at) = stack.pop() {
                    stack.extend(self.tree.remove(&at).into_iter().flat_map(|n| n.kids));
                }
                Ok(())
            }
            Op::Set(_, h, prop, value) => self.set_prop(w, h, prop.0, value),
            // The app has no move: the subtree is rebuilt under the new parent.
            Op::Mv(_, h, parent, after) => {
                self.node(h)?;
                self.node(parent)?;
                self.detach(w, h)?;
                self.tree.get_mut(&h).ok_or_else(|| unknown(h))?.parent = Some(parent);
                self.link(h, parent, after)?;
                self.materialize(w, h, false)
            }
        }
    }

    fn set_prop(&mut self, w: &DomWriter, h: Handle, prop: String, value: Json) -> Result<()> {
        let node = self.tree.get_mut(&h).ok_or_else(|| unknown(h))?;
        if prop == DROPPED {
            return Ok(());
        }
        match node.props.iter_mut().find(|(key, _)| *key == prop) {
            Some((_, current)) => current.clone_from(&value),
            None => node.props.push((prop.clone(), value.clone())),
        }
        self.on_set(w, h, &prop, &value)
    }

    /// The rule for `h` under its parent; elements the schema refuses there become `<ext>`.
    fn rule_of(&self, h: Handle) -> Result<Rule> {
        let node = self.node(h)?;
        let Some(parent) = node.parent else { return if h == ROOT { Ok(Rule::Root) } else { Err(bad("a node without a parent")) } };
        let (_, kind) = self.app_parent_of(parent)?;
        Ok(match rule(&node.tag, &self.node(parent)?.tag, kind) {
            Rule::Root => Rule::Ext,
            Rule::Element(e) if !kind.is_some_and(|k| e.kind().spec().parents.contains(&k)) => Rule::Ext,
            rule => rule,
        })
    }

    /// Creates the app node of `h` (mirrored already) and its subtree; `live` is a fresh insert about to stream.
    fn materialize(&mut self, w: &DomWriter, h: Handle, live: bool) -> Result<()> {
        match self.rule_of(h)? {
            rule @ (Rule::Root | Rule::Flatten) => {
                self.set_target(h, if rule == Rule::Root { Target::Root } else { Target::Flat });
                for (prop, value) in self.node(h)?.props.clone() {
                    self.on_set(w, h, &prop, &value)?;
                }
            }
            Rule::Assistant => self.set_target(h, Target::Assistant),
            Rule::Element(e) => self.insert_element(w, h, e, live)?,
            Rule::Ext => self.insert_ext(w, h)?,
        }
        self.node(h)?.kids.clone().into_iter().try_for_each(|kid| self.reconcile(w, kid))
    }

    fn insert_element(&mut self, w: &DomWriter, h: Handle, e: Element, live: bool) -> Result<()> {
        let parent = self.node(h)?.parent.ok_or_else(|| bad("an element without a parent"))?;
        let (app_parent, parent_kind) = self.app_parent_of(parent)?;
        if let Some(bound) = self.bind_pending(w, h, e, app_parent)? {
            self.set_target(h, Target::Node(bound, Some(e)));
            return Ok(());
        }
        let node = self.node(h)?;
        let mut attrs = fixed_attrs(e, &node.tag);
        if matches!(e, E::Turn | E::Call) {
            attrs.insert(attr::STARTED.to_owned(), Value::Int(w.now()));
        }
        if e == E::Block {
            let assistant = self.node(parent)?;
            for (prop, name) in [("model", attr::MODEL), ("provider", "x-provider"), ("route", "x-route")] {
                attrs.extend(assistant.prop(prop).and_then(to_value).map(|v| (name.to_owned(), v)));
            }
            if !live && !self.seed.contains_key(&(h, "text".to_owned())) {
                attrs.insert(attr::STATUS.to_owned(), assistant.prop("stop-reason").and_then(Json::as_str).map_or("done", stop_status).into());
            }
        }
        let mut text = node.content.clone().unwrap_or_default();
        for (prop, value) in &node.props {
            match element_write(e, prop, value) {
                (name, Some(Value::Str(t))) if name == TEXT => text = t,
                (name, Some(v)) if name != TEXT => { attrs.insert(name, v); }
                _ => {}
            }
            if dest(e, prop) == Some(TEXT) && let Some(seeded) = self.seed.get(&(h, prop.clone())) {
                text.clone_from(seeded);
            }
        }
        attrs.insert(HANDLE_ATTR.to_owned(), handle_value(h));
        if schema::validate_insert(parent_kind, e.kind(), attrs.iter().map(|(k, v)| (k.as_str(), v))).is_err() {
            return self.insert_ext(w, h);
        }
        let status = attrs.get(attr::STATUS).and_then(Value::as_str).map(str::to_owned);
        let call_id = attrs.get(attr::CALL_ID).and_then(Value::as_str).map(str::to_owned);
        let id = w.commit(Draft::Insert { parent: app_parent, index: self.app_index(h), kind: e.kind(), attrs, text })?;
        self.bind(h, id, app_parent);
        self.set_target(h, Target::Node(id, Some(e)));
        match (e, status.as_deref()) {
            (E::Turn, _) => self.turn_seen(id),
            (E::Block, Some("streaming")) | (E::Call, Some("pending" | "running")) => { self.open.insert(id, e == E::Call); }
            _ => {}
        }
        self.calls.extend(call_id.filter(|_| e == E::Call).map(|c| (c, id)));
        Ok(())
    }

    /// Binds omp2's `<turn>` and `<user>` to the nodes `begin` inserted.
    fn bind_pending(&mut self, w: &DomWriter, h: Handle, e: Element, app_parent: NodeId) -> Result<Option<NodeId>> {
        let id = match (e, self.pending) {
            (E::Turn, Some((turn, _))) if app_parent == self.ledger.session => turn,
            (E::User, Some((turn, Some(user)))) if app_parent == turn => {
                self.pending = Some((turn, None));
                user
            }
            _ => return Ok(None),
        };
        if self.back.contains_key(&id) {
            return Ok(None);
        }
        self.bind(h, id, app_parent);
        w.set(id, HANDLE_ATTR, handle_value(h))?;
        for (prop, value) in self.node(h)?.props.clone() {
            if !matches!(dest(e, &prop), Some(TEXT | attr::BY | attr::STATUS)) {
                self.write(w, id, e, &prop, &value)?;
            }
        }
        Ok(Some(id))
    }

    fn insert_ext(&mut self, w: &DomWriter, h: Handle) -> Result<()> {
        let node = self.node(h)?;
        let (app_parent, _) = self.app_parent_of(node.parent.ok_or_else(|| bad("an element without a parent"))?)?;
        let mut attrs = Attrs::from([(attr::HARNESS.to_owned(), Value::from(HARNESS)), (attr::TYPE.to_owned(), Value::from(node.tag.name.as_str())), (HANDLE_ATTR.to_owned(), handle_value(h))]);
        attrs.extend(node.props.iter().filter_map(|(prop, v)| Some((x(prop), to_value(v)?))));
        let text = node.content.clone().unwrap_or_default();
        let id = w.commit(Draft::Insert { parent: app_parent, index: self.app_index(h), kind: K::Ext, attrs, text })?;
        self.bind(h, id, app_parent);
        self.set_target(h, Target::Node(id, None));
        Ok(())
    }

    /// Commits `prop = v` on element `id`, then what its status implies.
    fn write(&mut self, w: &DomWriter, id: NodeId, e: Element, prop: &str, v: &Json) -> Result<()> {
        let (mut name, mut value) = element_write(e, prop, v);
        if schema::validate_set(e.kind(), &name, value.as_ref()).is_err() {
            (name, value) = (x(prop), to_value(v));
        }
        w.commit(Draft::Set { node: id, name: name.clone(), value: value.clone() })?;
        let status = match value { Some(Value::Str(status)) if name == attr::STATUS => status, _ => return Ok(()) };
        let terminal = matches!(status.as_str(), "done" | "error" | "cancelled");
        match e {
            E::Call if terminal => {
                self.open.remove(&id);
                w.set(id, attr::ENDED, w.now())?;
                if let Some(result) = self.result_of(id).filter(|_| status == "error") {
                    w.set(result, attr::IS_ERROR, true)?;
                }
            }
            E::Call => { self.open.insert(id, true); }
            E::Job | E::Subagent if terminal => w.set(id, attr::ENDED, w.now())?,
            _ => {}
        }
        Ok(())
    }

    fn result_of(&self, call: NodeId) -> Option<NodeId> {
        let (h, _) = self.back.get(&call)?;
        self.kids(*h).iter().find_map(|k| match self.target(*k)? { Target::Node(id, Some(E::Result)) => Some(id), _ => None })
    }

    /// Applies `prop = value` (mirrored already) of `h` to the app.
    fn on_set(&mut self, w: &DomWriter, h: Handle, prop: &str, value: &Json) -> Result<()> {
        let node = self.node(h)?;
        let (target, name) = match node.target {
            Target::Unset => return Err(unknown(h)),
            Target::Root if prop == "name" && matches!(value, Json::String(_) | Json::Null) => (self.ledger.session, attr::TITLE.to_owned()),
            Target::Root => (self.ledger.session, x(prop)),
            Target::Flat => (self.app_parent_of(node.parent.ok_or_else(|| bad("a container without a parent"))?)?.0, x(&format!("{}-{prop}", node.tag.name))),
            Target::Assistant => return self.assistant_set(w, h, prop, value),
            Target::Node(id, Some(e)) => return self.write(w, id, e, prop, value),
            Target::Node(id, None) => (id, x(prop)),
        };
        w.commit(Draft::Set { node: target, name, value: to_value(value) }).map(drop).map_err(Into::into)
    }

    /// Props of `<assistant>` reach its content blocks; its `text` and `thinking` aggregate them and are skipped.
    fn assistant_set(&mut self, w: &DomWriter, h: Handle, prop: &str, value: &Json) -> Result<()> {
        if matches!(prop, "text" | "thinking") {
            return Ok(());
        }
        let blocks: Vec<NodeId> = self.kids(h).iter().filter_map(|k| match self.target(*k)? { Target::Node(id, Some(E::Block)) => Some(id), _ => None }).collect();
        let name = if prop == "model" { attr::MODEL.to_owned() } else { x(prop) };
        let status = value.as_str().map_or("done", stop_status);
        for id in blocks {
            // A closed stream marked the block done already; an abort or error overrides that.
            if prop == "stop-reason" && (self.open.remove(&id).is_some() || status != "done") {
                w.set(id, attr::STATUS, status)?;
            }
            w.commit(Draft::Set { node: id, name: name.clone(), value: to_value(value) })?;
        }
        Ok(())
    }

    fn stream(&mut self, w: &DomWriter, sid: u32, op: StreamOp, bound: Option<(Handle, String)>, text: Option<String>) -> Result<()> {
        let missing = || AdapterError::Protocol(format!("omp2 used stream {sid}, which isn't open"));
        match op {
            StreamOp::Open => {
                let (h, prop) = bound.ok_or_else(|| bad("stream open without node and prop"))?;
                let stream = self.stream_target(h, &prop, None)?;
                self.streams.insert(sid, stream);
            }
            StreamOp::Append => match (self.streams.get_mut(&sid).ok_or_else(missing)?, text.ok_or_else(|| bad("stream append without text"))?) {
                (Stream::Text(id), text) => w.append(*id, text)?,
                (Stream::Prop(_, _, buf), text) => buf.push_str(&text),
                (Stream::Ignored, _) => {}
            },
            StreamOp::Close => match self.streams.remove(&sid).ok_or_else(missing)? {
                Stream::Text(id) if self.open.remove(&id).is_some() => w.set(id, attr::STATUS, "done")?,
                Stream::Prop(h, prop, buf) if self.tree.contains_key(&h) => self.set_prop(w, h, prop, Json::String(buf))?,
                _ => {}
            },
        }
        Ok(())
    }

    /// Where a stream on `prop` of `h` writes; `text` is what it holds already (from a snapshot).
    fn stream_target(&self, h: Handle, prop: &str, text: Option<&str>) -> Result<Stream> {
        let node = self.node(h)?;
        Ok(match node.target {
            Target::Node(id, Some(e)) if dest(e, prop) == Some(TEXT) => Stream::Text(id),
            Target::Node(..) => Stream::Prop(h, prop.to_owned(), text.or_else(|| node.prop(prop).and_then(Json::as_str)).unwrap_or_default().to_owned()),
            Target::Unset => return Err(unknown(h)),
            Target::Root | Target::Flat | Target::Assistant => Stream::Ignored,
        })
    }

    /// Replaces the mirror with `snapshot` and brings the app DOM in line: nodes omp2 no longer has go, new ones
    /// are inserted, known ones rewritten.
    fn reset(&mut self, w: &DomWriter, snapshot: Snapshot) -> Result<()> {
        self.high_water = snapshot.high_water;
        self.tree = snapshot.nodes.into_iter().map(|n| (n.handle, Node::new(n.node, n.parent))).collect();
        if !self.tree.contains_key(&ROOT) {
            return Err(bad("a snapshot without the session root"));
        }
        self.seed = snapshot.streams.iter().map(|s| ((s.node, s.prop.0.clone()), s.text.clone())).collect();
        self.streams.clear();
        self.open.retain(|_, is_call| *is_call);
        let stale: HashSet<NodeId> = self.ids.iter().filter(|(h, _)| !self.tree.contains_key(h)).map(|(_, id)| *id).collect();
        for id in stale.iter().filter(|id| !self.back.get(id).is_some_and(|(_, parent)| self.under(*parent, &stale))) {
            w.remove(*id)?;
        }
        let gone: HashSet<NodeId> = self.back.keys().copied().filter(|id| self.under(*id, &stale)).collect();
        for (h, _) in gone.iter().filter_map(|id| self.back.remove(id)) {
            self.ids.remove(&h);
        }
        self.forget(&gone);
        self.reconcile(w, ROOT)?;
        for stream in &snapshot.streams {
            let target = self.stream_target(stream.node, &stream.prop.0, Some(&stream.text))?;
            if let Stream::Text(id) = target && matches!(self.target(stream.node), Some(Target::Node(_, Some(E::Block)))) {
                self.open.insert(id, false);
            }
            self.streams.insert(stream.sid, target);
        }
        self.seed.clear();
        Ok(())
    }

    /// Visits `h` during a reset or below an insert: a bound node is kept and rewritten, anything else materialized.
    fn reconcile(&mut self, w: &DomWriter, h: Handle) -> Result<()> {
        let target = match self.ids.get(&h).copied() {
            None if h != ROOT => return self.materialize(w, h, false),
            None => Target::Root,
            Some(id) => match self.rule_of(h)? { Rule::Element(e) => Target::Node(id, Some(e)), Rule::Assistant => Target::Assistant, _ => Target::Node(id, None) },
        };
        self.set_target(h, target);
        let node = self.node(h)?;
        let (props, kids) = (node.props.clone(), node.kids.clone());
        for (prop, value) in &props {
            self.on_set(w, h, prop, value)?;
        }
        if let Target::Node(id, Some(e)) = target {
            if e == E::Turn {
                self.turn_seen(id);
            }
            for (prop, value) in &props {
                match (dest(e, prop), self.seed.get(&(h, prop.clone())), value.as_str()) {
                    (Some(TEXT), Some(text), _) => w.set_text(id, text.clone())?,
                    (Some(attr::CALL_ID), _, Some(call_id)) => { self.calls.insert(call_id.to_owned(), id); }
                    _ => {}
                }
            }
        }
        kids.into_iter().try_for_each(|kid| self.reconcile(w, kid))
    }
}

impl EventMapper for Omp2Mapper {
    type Event = Input;
    type Reply = Reply;

    fn apply(&mut self, input: Input, w: &DomWriter) -> Result<Vec<Reply>> {
        match input {
            Input::Dom(event) => self.dom(w, event)?,
            Input::Frame(frame) => self.frame(w, frame)?,
        }
        Ok(Vec::new())
    }

    fn answer(&mut self, decision: NodeId, answer: &Answer, w: &DomWriter) -> Result<Option<Reply>> {
        let reply = match self.decisions.remove(&decision).ok_or(AdapterError::UnknownDecision(decision))? {
            Pending::Approval(prompt_id) => {
                let (approved, scope) = match answer.values.first().map(String::as_str) {
                    Some(answer::ALLOW_ONCE) => (true, "once"),
                    Some(answer::ALLOW_SESSION) => (true, "session"),
                    _ => (false, "once"),
                };
                Reply::Approve { prompt_id, approved, scope, reason: answer.reason.clone() }
            }
            Pending::Select { id, options } => {
                let (values, typed): (Vec<String>, Vec<String>) = answer.values.iter().cloned().partition(|v| options.contains(v));
                Reply::ExtensionUiResponse { id, values, custom_input: (!typed.is_empty()).then(|| typed.join("\n")) }
            }
        };
        w.set(decision, attr::ANSWER, answer.values.clone())?;
        w.set(decision, attr::BY, answer.by.as_str())?;
        if let Some(reason) = &answer.reason {
            w.set(decision, attr::REASON, reason.as_str())?;
        }
        w.close(decision, DecisionStatus::Answered)?;
        self.ledger.answered(decision);
        if self.decisions.is_empty() && let Some(turn) = self.turn {
            w.set(turn, attr::STATUS, TurnStatus::Running)?;
        }
        Ok(Some(reply))
    }

    fn ledger(&self) -> &TurnLedger<String> { &self.ledger }
    fn ledger_mut(&mut self) -> &mut TurnLedger<String> { &mut self.ledger }
}
