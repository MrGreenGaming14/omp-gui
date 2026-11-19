//! Turns omp2 DOM events into app patches.
//!
//! omp2 already keeps the session as one tree changed by `patch@1`
//! transactions, so the translation is close to a passthrough: element and
//! property names go through the table in [`crate::map`], omp2 handles map to
//! app [`NodeId`]s through an [`IdTable`], and stream appends pass through as
//! `append` patches without copying the text.
//!
//! The translator keeps a mirror of the omp2 tree (structure and props, not
//! streamed text) to resolve parents, positions and inherited values.

use crate::map::{self, Dest, Element, HANDLE_ATTR, Rule};
use crate::wire::{self, BODY, DomEvent, Handle, META, Op, PropName, ROOT, Snapshot, StreamOp, Tag, Value as WValue};
use loa_core::schema::{self, TEXT};
use loa_core::{Attrs, CommitError, Dom, Draft, ElementKind, NodeId, PatchSink, TurnStatus, Value, attr};
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Debug, thiserror::Error)]
pub enum TranslateError {
    #[error(transparent)]
    Commit(#[from] CommitError),
    #[error("omp2 named node {0}, which it never inserted")]
    UnknownHandle(u64),
    #[error("omp2 appended to stream {0}, which isn't open")]
    UnknownStream(u32),
    #[error("malformed omp2 event: {0}")]
    Malformed(&'static str),
}

pub type Result<T, E = TranslateError> = std::result::Result<T, E>;

/// The bidirectional map between omp2 handles and the app nodes made from
/// them. Containers that are flattened have no entry.
#[derive(Clone, Debug, Default)]
pub struct IdTable {
    app: HashMap<Handle, NodeId>,
    /// App node to (omp2 handle, app parent).
    omp2: HashMap<NodeId, (Handle, NodeId)>,
}

impl IdTable {
    /// The app node made from omp2 handle `h`.
    pub fn app(&self, h: Handle) -> Option<NodeId> {
        self.app.get(&h).copied()
    }

    /// The omp2 handle app node `id` was made from.
    pub fn omp2(&self, id: NodeId) -> Option<Handle> {
        self.omp2.get(&id).map(|(h, _)| *h)
    }

    pub fn len(&self) -> usize {
        self.app.len()
    }

    pub fn is_empty(&self) -> bool {
        self.app.is_empty()
    }

    fn bind(&mut self, h: Handle, id: NodeId, parent: NodeId) {
        self.app.insert(h, id);
        self.omp2.insert(id, (h, parent));
    }

    fn unbind(&mut self, h: Handle) -> Option<NodeId> {
        let id = self.app.remove(&h)?;
        self.omp2.remove(&id);
        Some(id)
    }

    /// True when `id` or one of its app ancestors is in `set`.
    fn under(&self, id: NodeId, set: &HashSet<NodeId>) -> bool {
        let mut at = Some(id);
        while let Some(node) = at {
            if set.contains(&node) {
                return true;
            }
            at = self.omp2.get(&node).map(|(_, parent)| *parent);
        }
        false
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Target {
    /// Not materialized yet.
    Unset,
    Root,
    Flat,
    Assistant,
    /// An app node; `None` is `<ext>`.
    Node(NodeId, Option<Element>),
}

#[derive(Clone, Debug)]
struct Mirror {
    tag: Tag,
    parent: Option<Handle>,
    kids: Vec<Handle>,
    props: Vec<(PropName, WValue)>,
    content: Option<String>,
    target: Target,
}

impl Mirror {
    fn prop(&self, name: &str) -> Option<&WValue> {
        self.props.iter().find(|(key, _)| key.as_str() == name).map(|(_, value)| value)
    }

    fn put(&mut self, name: PropName, value: WValue) {
        match self.props.iter_mut().find(|(key, _)| *key == name) {
            Some((_, current)) => *current = value,
            None => self.props.push((name, value)),
        }
    }
}

enum Stream {
    /// Appends go to the node text.
    Text(NodeId),
    /// Appends collect here and are `set` as one attribute on close.
    Attr { node: NodeId, kind: ElementKind, name: String, buf: String },
    /// Bound to a container the app keeps no node for.
    Ignored,
}

/// One change to an app element's text or attribute.
enum Write {
    Text(Option<String>),
    Attr(String, Option<Value>),
}

/// Translates one omp2 session into one app session.
pub struct Translator {
    root: NodeId,
    high_water: u64,
    mirror: HashMap<Handle, Mirror>,
    /// The mirror before the snapshot being reconciled.
    old: HashMap<Handle, Mirror>,
    /// Open stream text by (node, prop) while a snapshot is materialized.
    seed: HashMap<(Handle, PropName), String>,
    ids: IdTable,
    streams: HashMap<u32, Stream>,
    /// Turn and user message `begin_turn` inserted, waiting for omp2's own.
    pending: Option<(NodeId, Option<NodeId>)>,
    turn: Option<NodeId>,
    open_messages: BTreeSet<NodeId>,
    open_calls: BTreeSet<NodeId>,
    calls: HashMap<String, NodeId>,
}

fn to_value(value: &WValue) -> Option<Value> {
    match value {
        WValue::Null => None,
        WValue::Bool(v) => Some(Value::Bool(*v)),
        WValue::Int(v) => Some(Value::Int(*v)),
        WValue::Float(v) => Some(Value::Float(*v)),
        WValue::Str(v) | WValue::Json(v) => Some(Value::Str(v.clone())),
    }
}

fn handle_value(h: Handle) -> Value {
    Value::Int(i64::try_from(h.0).unwrap_or(i64::MAX))
}

fn extra(prop: &str, value: &WValue) -> Write {
    Write::Attr(format!("{}{prop}", schema::EXTENSION_PREFIX), to_value(value))
}

/// Raw props on `<ext>` are prefixed so they remain distinct from schema
/// attributes (`harness`, `type`) and can be recovered without ambiguity.
fn ext_name(prop: &str) -> String {
    format!("{}{prop}", schema::EXTENSION_PREFIX)
}

/// How `prop = value` of an omp2 node that became `element` is written.
fn element_write(element: Element, prop: &str, value: &WValue) -> Option<Write> {
    match map::dest(element, prop) {
        Dest::Drop => None,
        Dest::Text => Some(match value {
            WValue::Str(text) => Write::Text(Some(text.clone())),
            WValue::Null => Write::Text(None),
            other => extra(prop, other),
        }),
        Dest::Cost => Some(match value {
            #[allow(clippy::cast_precision_loss, reason = "costs are far below 2^53 nano dollars")]
            WValue::Int(nano) => Write::Attr(attr::COST.to_owned(), Some(Value::Float(*nano as f64 / 1e9))),
            WValue::Float(nano) => Write::Attr(attr::COST.to_owned(), Some(Value::Float(nano / 1e9))),
            WValue::Null => Write::Attr(attr::COST.to_owned(), None),
            other => extra(prop, other),
        }),
        Dest::Attr(name) => {
            if matches!(value, WValue::Null) {
                return Some(Write::Attr(name.to_owned(), None));
            }
            let spec = element.kind().spec().attr(name);
            let mapped = match spec {
                Some(spec) if !spec.values.is_empty() => {
                    value.as_str().and_then(|raw| map::enum_value(element, name, raw)).map(Value::from)
                }
                Some(spec) => coerce(value, spec.ty),
                None => None,
            };
            Some(match mapped {
                Some(mapped) => Write::Attr(name.to_owned(), Some(mapped)),
                None => extra(prop, value),
            })
        }
        Dest::Extra => Some(extra(prop, value)),
    }
}

fn coerce(value: &WValue, ty: loa_core::ValueType) -> Option<Value> {
    use loa_core::ValueType as T;
    match (value, ty) {
        (WValue::Bool(v), T::Bool) => Some(Value::Bool(*v)),
        (WValue::Int(v), T::Int) => Some(Value::Int(*v)),
        #[allow(clippy::cast_precision_loss, reason = "a float attribute accepts the nearest value")]
        (WValue::Int(v), T::Float) => Some(Value::Float(*v as f64)),
        (WValue::Float(v), T::Float) => Some(Value::Float(*v)),
        (WValue::Str(v), T::Str) | (WValue::Json(v), T::Str) => Some(Value::Str(v.clone())),
        _ => None,
    }
}

/// The skeleton every omp2 DOM starts with (`Dom::new` in omp2).
fn skeleton() -> Snapshot {
    let node = |handle: Handle, parent: Option<Handle>, tag: &str, kids: Vec<Handle>| wire::SnapshotNode {
        handle,
        parent,
        node: wire::SnapshotBody { tag: Tag::parse(tag), props: Vec::new(), kids, content: None },
    };
    Snapshot {
        high_water: 4,
        nodes: vec![
            node(ROOT, None, "session", vec![META, BODY, wire::QUEUES]),
            node(META, Some(ROOT), "meta", Vec::new()),
            node(BODY, Some(ROOT), "body", Vec::new()),
            node(wire::QUEUES, Some(ROOT), "queues", Vec::new()),
        ],
        streams: Vec::new(),
    }
}

impl Translator {
    /// A translator for a new session whose `<session>` root is `root`.
    pub fn new(root: NodeId) -> Self {
        Self {
            root,
            high_water: 0,
            mirror: HashMap::new(),
            old: HashMap::new(),
            seed: HashMap::new(),
            ids: IdTable::default(),
            streams: HashMap::new(),
            pending: None,
            turn: None,
            open_messages: BTreeSet::new(),
            open_calls: BTreeSet::new(),
            calls: HashMap::new(),
        }
    }

    /// A translator continuing `dom`, a session this adapter wrote before.
    /// The handle table is rebuilt from [`HANDLE_ATTR`]; the next event must
    /// be a snapshot, which is reconciled against the DOM.
    pub fn from_dom(dom: &Dom) -> Result<Self> {
        let root = dom.root().ok_or(TranslateError::Malformed("the DOM has no session root"))?;
        let mut translator = Self::new(root.id);
        for node in dom.iter() {
            if let Some(h) = node.int_attr(HANDLE_ATTR).and_then(|h| u64::try_from(h).ok()) {
                translator.ids.bind(Handle(h), node.id, node.parent);
            }
            if let (ElementKind::Call, Some(call_id)) = (node.kind, node.str_attr(attr::CALL_ID)) {
                translator.calls.insert(call_id.to_owned(), node.id);
            }
        }
        translator.turn = dom.children_of_kind(root.id, ElementKind::Turn).last().map(|n| n.id);
        Ok(translator)
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn ids(&self) -> &IdTable {
        &self.ids
    }

    /// The turn that is running or ran last.
    pub fn current_turn(&self) -> Option<NodeId> {
        self.turn
    }

    /// The `<call>` with omp2 tool call id `call_id`.
    pub fn call(&self, call_id: &str) -> Option<NodeId> {
        self.calls.get(call_id).copied()
    }

    /// Applies one omp2 DOM event, committing app patches stamped `ts`.
    pub fn apply(&mut self, sink: &dyn PatchSink, event: DomEvent, ts: i64) -> Result<()> {
        if self.mirror.is_empty() && !matches!(event, DomEvent::Snapshot { .. }) {
            if !self.ids.is_empty() {
                return Err(TranslateError::Malformed("a resumed session must start with a snapshot"));
            }
            self.reset(sink, skeleton(), ts)?;
        }
        match event {
            DomEvent::Patch { data } => {
                for op in data.ops {
                    self.op(sink, op, ts)?;
                }
                Ok(())
            }
            DomEvent::Stream { sid, op, node, prop, text } => self.stream(sink, sid, op, node, prop, text, ts),
            DomEvent::Snapshot { data } => self.reset(sink, data, ts),
        }
    }

    /// Inserts the `<turn>` and user `<message>` for a prompt about to be sent.
    /// omp2's own `<turn>` and `<user>` are bound to them when they arrive.
    pub fn begin_turn(&mut self, sink: &dyn PatchSink, text: &str, by: &str, ts: i64) -> Result<(NodeId, NodeId)> {
        let attrs = Attrs::from([
            (attr::STATUS.to_owned(), Value::from("running")),
            (attr::STARTED.to_owned(), Value::Int(ts)),
        ]);
        let index = self.turn_index();
        let turn = sink
            .commit_at(
                Draft::Insert { parent: self.root, index, kind: ElementKind::Turn, attrs, text: String::new() },
                ts,
            )?
            .node;
        let user = sink
            .commit_at(
                Draft::insert(
                    turn,
                    ElementKind::Message,
                    [(attr::ROLE, Value::from("user")), (attr::STATUS, Value::from("done")), (attr::BY, Value::from(by))],
                    text,
                ),
                ts,
            )?
            .node;
        self.pending = Some((turn, Some(user)));
        self.turn = Some(turn);
        Ok((turn, user))
    }

    /// Ends the current turn: streaming messages and unfinished calls are
    /// closed to match `status`, then the turn is.
    pub fn finish_turn(&mut self, sink: &dyn PatchSink, status: TurnStatus, ts: i64) -> Result<()> {
        self.pending = None;
        let Some(turn) = self.turn else { return Ok(()) };
        let (message, call) = match status {
            TurnStatus::Done => ("done", "cancelled"),
            TurnStatus::Error => ("error", "error"),
            _ => ("cancelled", "cancelled"),
        };
        for id in std::mem::take(&mut self.open_messages) {
            sink.commit_at(Draft::set(id, attr::STATUS, message), ts)?;
        }
        for id in std::mem::take(&mut self.open_calls) {
            sink.commit_at(Draft::set(id, attr::STATUS, call), ts)?;
            sink.commit_at(Draft::set(id, attr::ENDED, ts), ts)?;
        }
        sink.commit_at(Draft::set(turn, attr::STATUS, status), ts)?;
        sink.commit_at(Draft::set(turn, attr::ENDED, ts), ts)?;
        Ok(())
    }

    fn node(&self, h: Handle) -> Result<&Mirror> {
        self.mirror.get(&h).ok_or(TranslateError::UnknownHandle(h.0))
    }

    fn kids(&self, h: Handle) -> &[Handle] {
        self.mirror.get(&h).map_or(&[], |m| &m.kids)
    }

    fn set_target(&mut self, h: Handle, target: Target) {
        if let Some(node) = self.mirror.get_mut(&h) {
            node.target = target;
        }
    }

    /// The app node children of `h` go under, and its kind (`None` for `<ext>`).
    fn app_parent_of(&self, h: Handle) -> Result<(NodeId, Option<ElementKind>)> {
        let mut at = h;
        loop {
            let node = self.node(at)?;
            match node.target {
                Target::Root => return Ok((self.root, Some(ElementKind::Session))),
                Target::Node(id, element) => return Ok((id, element.map(Element::kind))),
                Target::Flat | Target::Assistant => {
                    at = node.parent.ok_or(TranslateError::Malformed("a container without a parent"))?;
                }
                Target::Unset => return Err(TranslateError::UnknownHandle(at.0)),
            }
        }
    }

    /// The nearest ancestor of `h` that has an app node (or is the root).
    fn origin(&self, h: Handle) -> Handle {
        let mut at = self.mirror.get(&h).and_then(|m| m.parent);
        while let Some(handle) = at {
            match self.mirror.get(&handle) {
                Some(m) if matches!(m.target, Target::Flat | Target::Assistant) => at = m.parent,
                _ => return handle,
            }
        }
        ROOT
    }

    /// Position of the app node for `h` among its app siblings: `None`
    /// appends, which is the case for nearly every omp2 insert.
    fn app_index(&self, h: Handle) -> Option<u32> {
        let mut stack: Vec<Handle> = self.kids(self.origin(h)).iter().rev().copied().collect();
        let mut count = 0u32;
        let mut seen = false;
        while let Some(at) = stack.pop() {
            if at == h {
                seen = true;
                continue;
            }
            match self.mirror.get(&at).map(|m| m.target) {
                Some(Target::Node(..)) if seen => return Some(count),
                Some(Target::Node(..)) => count += 1,
                Some(Target::Flat | Target::Assistant) => stack.extend(self.kids(at).iter().rev()),
                _ => {}
            }
        }
        None
    }

    fn emitted_count(&self, h: Handle) -> u32 {
        match self.mirror.get(&h).map(|m| m.target) {
            Some(Target::Node(..)) => 1,
            Some(Target::Flat | Target::Assistant) => self.kids(h).iter().map(|k| self.emitted_count(*k)).sum(),
            _ => 0,
        }
    }

    /// Where a new turn goes under the session: after everything from
    /// `<meta>` and `<body>`, before what came from `<queues>`.
    fn turn_index(&self) -> Option<u32> {
        let kids = self.kids(ROOT);
        let body = kids.iter().position(|k| *k == BODY)?;
        let after: u32 = kids[body + 1..].iter().map(|k| self.emitted_count(*k)).sum();
        (after > 0).then(|| kids[..=body].iter().map(|k| self.emitted_count(*k)).sum())
    }

    /// Every app node made from `h` and its subtree, outermost only.
    fn app_nodes(&self, h: Handle, out: &mut Vec<NodeId>) {
        match self.mirror.get(&h).map(|m| m.target) {
            Some(Target::Node(id, _)) => out.push(id),
            Some(Target::Flat | Target::Assistant | Target::Root) => {
                if let Some(id) = self.ids.app(h) {
                    out.push(id);
                }
                for kid in self.kids(h) {
                    self.app_nodes(*kid, out);
                }
            }
            _ => {}
        }
    }

    /// Drops every binding of `h`'s subtree, keeping the mirror.
    fn unbind_subtree(&mut self, h: Handle) {
        let mut stack = vec![h];
        let mut gone = HashSet::new();
        while let Some(at) = stack.pop() {
            if let Some(id) = self.ids.unbind(at) {
                gone.insert(id);
            }
            if let Some(node) = self.mirror.get_mut(&at) {
                if let Target::Node(id, _) = node.target {
                    gone.insert(id);
                }
                node.target = Target::Unset;
                stack.extend(node.kids.iter().copied());
            }
        }
        self.forget_ids(&gone);
    }

    fn forget_ids(&mut self, gone: &HashSet<NodeId>) {
        if gone.is_empty() {
            return;
        }
        self.open_messages.retain(|id| !gone.contains(id));
        self.open_calls.retain(|id| !gone.contains(id));
        self.calls.retain(|_, id| !gone.contains(id));
        self.streams.retain(|_, s| match s {
            Stream::Text(id) | Stream::Attr { node: id, .. } => !gone.contains(id),
            Stream::Ignored => true,
        });
        if self.turn.is_some_and(|t| gone.contains(&t)) {
            self.turn = None;
        }
    }

    fn op(&mut self, sink: &dyn PatchSink, op: Op, ts: i64) -> Result<()> {
        match op {
            Op::Ins { parent, after, node } => {
                self.node(parent)?;
                self.high_water += 1;
                let h = Handle(self.high_water);
                let mirror = Mirror {
                    tag: node.tag,
                    parent: Some(parent),
                    kids: Vec::new(),
                    props: node.props,
                    content: node.content,
                    target: Target::Unset,
                };
                self.mirror.insert(h, mirror);
                self.link(h, parent, after);
                self.materialize(sink, h, true, ts)
            }
            Op::Rm(h) => {
                self.node(h)?;
                let mut doomed = Vec::new();
                self.app_nodes(h, &mut doomed);
                for id in doomed {
                    sink.commit_at(Draft::remove(id), ts)?;
                }
                self.unbind_subtree(h);
                self.unlink(h);
                let mut stack = vec![h];
                while let Some(at) = stack.pop() {
                    if let Some(node) = self.mirror.remove(&at) {
                        stack.extend(node.kids);
                    }
                }
                Ok(())
            }
            Op::Set { h, prop, value } => {
                self.mirror.get_mut(&h).ok_or(TranslateError::UnknownHandle(h.0))?.put(prop.clone(), value.clone());
                self.on_set(sink, h, prop.as_str(), &value, ts)
            }
            Op::Mv { h, parent, after } => {
                // The app has no move: the subtree is removed and rebuilt from
                // the mirror under the new parent, with new app ids.
                self.node(h)?;
                self.node(parent)?;
                let mut doomed = Vec::new();
                self.app_nodes(h, &mut doomed);
                for id in doomed {
                    sink.commit_at(Draft::remove(id), ts)?;
                }
                self.unbind_subtree(h);
                self.unlink(h);
                if let Some(node) = self.mirror.get_mut(&h) {
                    node.parent = Some(parent);
                }
                self.link(h, parent, after);
                self.materialize(sink, h, false, ts)
            }
        }
    }

    /// Puts `h` among `parent`'s kids after `after`; `None` (or an unknown
    /// anchor) is the first position, as in omp2.
    fn link(&mut self, h: Handle, parent: Handle, after: Option<Handle>) {
        if let Some(node) = self.mirror.get_mut(&parent) {
            let at = after.and_then(|a| node.kids.iter().position(|k| *k == a)).map_or(0, |i| i + 1);
            node.kids.insert(at, h);
        }
    }

    fn unlink(&mut self, h: Handle) {
        let parent = self.mirror.get(&h).and_then(|m| m.parent);
        if let Some(node) = parent.and_then(|p| self.mirror.get_mut(&p)) {
            node.kids.retain(|k| *k != h);
        }
    }

    /// Creates the app node for `h` (already in the mirror) and its subtree.
    /// `live` is a fresh insert, whose assistant block is about to stream.
    fn materialize(&mut self, sink: &dyn PatchSink, h: Handle, live: bool, ts: i64) -> Result<()> {
        let node = self.node(h)?;
        let rule = match node.parent {
            None if h == ROOT => Rule::Root,
            None => return Err(TranslateError::Malformed("a node without a parent")),
            Some(parent) => {
                let (_, parent_kind) = self.app_parent_of(parent)?;
                let parent_tag = &self.node(parent)?.tag;
                match map::rule(&node.tag, parent_tag, parent_kind) {
                    Rule::Root => Rule::Ext,
                    Rule::Element(element)
                        if !parent_kind.is_some_and(|kind| element.kind().spec().parents.contains(&kind)) =>
                    {
                        Rule::Ext
                    }
                    rule => rule,
                }
            }
        };
        match rule {
            Rule::Root => {
                self.set_target(h, Target::Root);
                let props = self.node(h)?.props.clone();
                for (prop, value) in &props {
                    self.on_set(sink, h, prop.as_str(), value, ts)?;
                }
            }
            Rule::Flatten => {
                self.set_target(h, Target::Flat);
                let props = self.node(h)?.props.clone();
                for (prop, value) in &props {
                    self.on_set(sink, h, prop.as_str(), value, ts)?;
                }
            }
            Rule::Assistant => {
                self.set_target(h, Target::Assistant);
                let legacy = self.node(h)?.prop("text").and_then(WValue::as_str).map(str::to_owned);
                if let Some(text) = legacy {
                    self.legacy_text(sink, h, text, ts)?;
                }
            }
            Rule::Element(element) => self.insert_element(sink, h, element, live, ts)?,
            Rule::Ext => self.insert_ext(sink, h, ts)?,
        }
        let kids = self.node(h)?.kids.clone();
        for kid in kids {
            self.reconcile(sink, kid, ts)?;
        }
        Ok(())
    }

    fn insert_element(&mut self, sink: &dyn PatchSink, h: Handle, element: Element, live: bool, ts: i64) -> Result<()> {
        let node = self.node(h)?;
        let parent = node.parent.ok_or(TranslateError::Malformed("an element without a parent"))?;
        let (app_parent, _) = self.app_parent_of(parent)?;
        if let Some(bound) = self.bind_pending(sink, h, element, app_parent, ts)? {
            self.set_target(h, Target::Node(bound, Some(element)));
            return Ok(());
        }
        let node = self.node(h)?;
        let mut attrs: Attrs = map::fixed_attrs(element, &node.tag).into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
        if matches!(element, Element::Turn | Element::Call) {
            attrs.insert(attr::STARTED.to_owned(), Value::Int(ts));
        }
        if element == Element::Block {
            let assistant = self.node(parent)?;
            for (prop, name) in [("model", attr::MODEL), ("provider", "x-provider"), ("route", "x-route")] {
                if let Some(value) = assistant.prop(prop).and_then(to_value) {
                    attrs.insert(name.to_owned(), value);
                }
            }
            let streaming = live || self.seed.contains_key(&(h, PropName("text".to_owned())));
            if !streaming {
                let status = assistant.prop("stop-reason").and_then(WValue::as_str).map_or("done", map::stop_status);
                attrs.insert(attr::STATUS.to_owned(), Value::from(status));
            }
        }
        let mut text = node.content.clone().unwrap_or_default();
        for (prop, value) in &node.props {
            match element_write(element, prop.as_str(), value) {
                Some(Write::Text(Some(t))) => text = t,
                Some(Write::Attr(name, Some(value))) => {
                    attrs.insert(name, value);
                }
                _ => {}
            }
            if map::dest(element, prop.as_str()) == Dest::Text
                && let Some(seeded) = self.seed.get(&(h, prop.clone()))
            {
                text.clone_from(seeded);
            }
        }
        attrs.insert(HANDLE_ATTR.to_owned(), handle_value(h));
        let kind = element.kind();
        let parent_kind = self.app_parent_of(parent)?.1;
        if schema::validate_insert(parent_kind, kind, attrs.iter().map(|(k, v)| (k.as_str(), v))).is_err() {
            return self.insert_ext(sink, h, ts);
        }
        let status = attrs.get(attr::STATUS).and_then(Value::as_str).map(str::to_owned);
        let call_id = attrs.get(attr::CALL_ID).and_then(Value::as_str).map(str::to_owned);
        let index = self.app_index(h);
        let id = sink.commit_at(Draft::Insert { parent: app_parent, index, kind, attrs, text }, ts)?.node;
        self.ids.bind(h, id, app_parent);
        self.set_target(h, Target::Node(id, Some(element)));
        match element {
            Element::Turn => self.turn = Some(id),
            Element::Block if status.as_deref() == Some("streaming") => {
                self.open_messages.insert(id);
            }
            Element::Call => {
                if matches!(status.as_deref(), Some("pending" | "running")) {
                    self.open_calls.insert(id);
                }
                if let Some(call_id) = call_id {
                    self.calls.insert(call_id, id);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Binds omp2's `<turn>` and `<user>` to the nodes `begin_turn` inserted.
    fn bind_pending(
        &mut self,
        sink: &dyn PatchSink,
        h: Handle,
        element: Element,
        app_parent: NodeId,
        ts: i64,
    ) -> Result<Option<NodeId>> {
        let id = match (element, self.pending) {
            (Element::Turn, Some((turn, _))) if app_parent == self.root => turn,
            (Element::User, Some((turn, Some(user)))) if app_parent == turn => {
                self.pending = Some((turn, None));
                user
            }
            _ => return Ok(None),
        };
        // The pending turn must not be bound twice.
        if self.ids.omp2(id).is_some() {
            return Ok(None);
        }
        self.ids.bind(h, id, app_parent);
        sink.commit_at(Draft::set(id, HANDLE_ATTR, handle_value(h)), ts)?;
        let props = self.node(h)?.props.clone();
        for (prop, value) in &props {
            let skip = match map::dest(element, prop.as_str()) {
                Dest::Text => true,
                Dest::Attr(name) => name == attr::BY || name == attr::STATUS,
                _ => false,
            };
            if !skip {
                self.write(sink, id, element, prop.as_str(), value, ts)?;
            }
        }
        Ok(Some(id))
    }

    fn insert_ext(&mut self, sink: &dyn PatchSink, h: Handle, ts: i64) -> Result<()> {
        let node = self.node(h)?;
        let parent = node.parent.ok_or(TranslateError::Malformed("an element without a parent"))?;
        let mut attrs = Attrs::from([
            (attr::HARNESS.to_owned(), Value::from(crate::HARNESS)),
            (attr::TYPE.to_owned(), Value::Str(node.tag.name.clone())),
            (HANDLE_ATTR.to_owned(), handle_value(h)),
        ]);
        for (prop, value) in &node.props {
            if let Some(value) = to_value(value) {
                attrs.insert(ext_name(prop.as_str()), value);
            }
        }
        let text = node.content.clone().unwrap_or_default();
        let (app_parent, _) = self.app_parent_of(parent)?;
        let index = self.app_index(h);
        let draft = Draft::Insert { parent: app_parent, index, kind: ElementKind::Ext, attrs, text };
        let id = sink.commit_at(draft, ts)?.node;
        self.ids.bind(h, id, app_parent);
        self.set_target(h, Target::Node(id, None));
        Ok(())
    }

    /// `<assistant>` text from before content blocks existed: kept as one
    /// finished message bound to the assistant's handle.
    fn legacy_text(&mut self, sink: &dyn PatchSink, h: Handle, text: String, ts: i64) -> Result<()> {
        let has_blocks = self.kids(h).iter().any(|k| self.mirror.get(k).is_some_and(|m| m.tag.custom));
        if has_blocks || self.ids.app(h).is_some() {
            return Ok(());
        }
        let node = self.node(h)?;
        let parent = node.parent.ok_or(TranslateError::Malformed("an assistant without a turn"))?;
        let (turn, kind) = self.app_parent_of(parent)?;
        if kind != Some(ElementKind::Turn) {
            return Ok(());
        }
        let mut attrs = Attrs::from([
            (attr::ROLE.to_owned(), Value::from("assistant")),
            (attr::STATUS.to_owned(), Value::from("done")),
            (HANDLE_ATTR.to_owned(), handle_value(h)),
        ]);
        if let Some(model) = node.prop("model").and_then(to_value) {
            attrs.insert(attr::MODEL.to_owned(), model);
        }
        let index = self.app_index(h);
        let draft = Draft::Insert { parent: turn, index, kind: ElementKind::Message, attrs, text };
        let id = sink.commit_at(draft, ts)?.node;
        self.ids.bind(h, id, turn);
        Ok(())
    }

    /// Commits `prop = value` on app element `id` made from `element`. A value
    /// the schema refuses under its mapped name is kept as `x-<prop>`.
    fn write(&mut self, sink: &dyn PatchSink, id: NodeId, element: Element, prop: &str, value: &WValue, ts: i64) -> Result<()> {
        let Some(write) = element_write(element, prop, value) else { return Ok(()) };
        let draft = match write {
            Write::Text(Some(text)) => Draft::set_text(id, text),
            Write::Text(None) => Draft::clear(id, TEXT),
            Write::Attr(name, mapped) => {
                if schema::validate_set(element.kind(), &name, mapped.as_ref()).is_ok() {
                    Draft::Set { node: id, name, value: mapped }
                } else {
                    Draft::Set { node: id, name: format!("{}{prop}", schema::EXTENSION_PREFIX), value: to_value(value) }
                }
            }
        };
        let status = match &draft {
            Draft::Set { name, value: Some(Value::Str(status)), .. } if name == attr::STATUS => Some(status.clone()),
            _ => None,
        };
        sink.commit_at(draft, ts)?;
        if let Some(status) = status {
            self.status_changed(sink, id, element, &status, ts)?;
        }
        Ok(())
    }

    fn status_changed(&mut self, sink: &dyn PatchSink, id: NodeId, element: Element, status: &str, ts: i64) -> Result<()> {
        let terminal = matches!(status, "done" | "error" | "cancelled");
        match element {
            Element::Call if terminal => {
                self.open_calls.remove(&id);
                sink.commit_at(Draft::set(id, attr::ENDED, ts), ts)?;
                if status == "error"
                    && let Some(result) = self.result_of(id)
                {
                    sink.commit_at(Draft::set(result, attr::IS_ERROR, true), ts)?;
                }
            }
            Element::Call => {
                self.open_calls.insert(id);
            }
            Element::Job | Element::Subagent if terminal => {
                sink.commit_at(Draft::set(id, attr::ENDED, ts), ts)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// The `<result>` under app call `call`.
    fn result_of(&self, call: NodeId) -> Option<NodeId> {
        let h = self.ids.omp2(call)?;
        self.kids(h).iter().find_map(|k| match self.mirror.get(k)?.target {
            Target::Node(id, Some(Element::Result)) => Some(id),
            _ => None,
        })
    }

    /// Applies `prop = value` (already in the mirror) of `h` to the app.
    fn on_set(&mut self, sink: &dyn PatchSink, h: Handle, prop: &str, value: &WValue, ts: i64) -> Result<()> {
        let node = self.node(h)?;
        match node.target {
            Target::Unset => Err(TranslateError::UnknownHandle(h.0)),
            Target::Root => {
                let name = match map::session_dest(prop) {
                    Dest::Attr(name) if matches!(value, WValue::Str(_) | WValue::Null) => name.to_owned(),
                    _ => format!("{}{prop}", schema::EXTENSION_PREFIX),
                };
                sink.commit_at(Draft::Set { node: self.root, name, value: to_value(value) }, ts)?;
                Ok(())
            }
            Target::Flat => {
                if map::DROPPED_PROPS.contains(&prop) {
                    return Ok(());
                }
                let name = format!("{}{}-{prop}", schema::EXTENSION_PREFIX, node.tag.name);
                let parent = node.parent.ok_or(TranslateError::Malformed("a container without a parent"))?;
                let (app_parent, _) = self.app_parent_of(parent)?;
                sink.commit_at(Draft::Set { node: app_parent, name, value: to_value(value) }, ts)?;
                Ok(())
            }
            Target::Assistant => self.assistant_set(sink, h, prop, value, ts),
            Target::Node(id, Some(element)) => self.write(sink, id, element, prop, value, ts),
            Target::Node(id, None) => {
                sink.commit_at(Draft::Set { node: id, name: ext_name(prop), value: to_value(value) }, ts)?;
                Ok(())
            }
        }
    }

    /// Props of `<assistant>` reach its content-block messages.
    fn assistant_set(&mut self, sink: &dyn PatchSink, h: Handle, prop: &str, value: &WValue, ts: i64) -> Result<()> {
        let blocks: Vec<NodeId> = self
            .kids(h)
            .iter()
            .filter_map(|k| match self.mirror.get(k)?.target {
                Target::Node(id, Some(Element::Block)) => Some(id),
                _ => None,
            })
            .chain(self.ids.app(h))
            .collect();
        match prop {
            "stop-reason" => {
                let status = value.as_str().map_or("done", map::stop_status);
                for id in blocks {
                    // A closed stream already marked the block done; an
                    // abort or error still overrides that.
                    if self.open_messages.remove(&id) || status != "done" {
                        sink.commit_at(Draft::set(id, attr::STATUS, status), ts)?;
                    }
                    sink.commit_at(Draft::Set { node: id, name: "x-stop-reason".to_owned(), value: to_value(value) }, ts)?;
                }
                Ok(())
            }
            // The final `text` and `thinking` are aggregates of the content
            // blocks; only an assistant without blocks needs them.
            "text" => match value {
                WValue::Str(text) if blocks.is_empty() => self.legacy_text(sink, h, text.clone(), ts),
                _ => Ok(()),
            },
            "thinking" => Ok(()),
            _ if map::DROPPED_PROPS.contains(&prop) => Ok(()),
            _ => {
                let name = if prop == "model" { attr::MODEL.to_owned() } else { format!("{}{prop}", schema::EXTENSION_PREFIX) };
                for id in blocks {
                    sink.commit_at(Draft::Set { node: id, name: name.clone(), value: to_value(value) }, ts)?;
                }
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_arguments, reason = "the fields of one stream@1 record")]
    fn stream(
        &mut self,
        sink: &dyn PatchSink,
        sid: u32,
        op: StreamOp,
        node: Option<Handle>,
        prop: Option<PropName>,
        text: Option<String>,
        ts: i64,
    ) -> Result<()> {
        match op {
            StreamOp::Open => {
                let (Some(h), Some(prop)) = (node, prop) else {
                    return Err(TranslateError::Malformed("stream open without node and prop"));
                };
                let stream = self.stream_target(h, &prop, None)?;
                self.streams.insert(sid, stream);
                Ok(())
            }
            StreamOp::Append => {
                let text = text.ok_or(TranslateError::Malformed("stream append without text"))?;
                match self.streams.get_mut(&sid) {
                    Some(Stream::Text(id)) => {
                        sink.commit_at(Draft::Append { node: *id, text }, ts)?;
                    }
                    Some(Stream::Attr { buf, .. }) => buf.push_str(&text),
                    Some(Stream::Ignored) => {}
                    None => return Err(TranslateError::UnknownStream(sid)),
                }
                Ok(())
            }
            StreamOp::Close => match self.streams.remove(&sid) {
                Some(Stream::Text(id)) => {
                    if self.open_messages.remove(&id) {
                        sink.commit_at(Draft::set(id, attr::STATUS, "done"), ts)?;
                    }
                    Ok(())
                }
                Some(Stream::Attr { node, kind, name, buf }) => {
                    let value = Some(Value::Str(buf));
                    if schema::validate_set(kind, &name, value.as_ref()).is_ok() {
                        sink.commit_at(Draft::Set { node, name, value }, ts)?;
                    }
                    Ok(())
                }
                Some(Stream::Ignored) => Ok(()),
                None => Err(TranslateError::UnknownStream(sid)),
            },
        }
    }

    /// Where a stream bound to `prop` of `h` writes. `text` is what the
    /// stream holds already (from a snapshot).
    fn stream_target(&self, h: Handle, prop: &PropName, text: Option<&str>) -> Result<Stream> {
        let node = self.node(h)?;
        let initial = || text.map(str::to_owned).or_else(|| node.prop(prop.as_str()).and_then(WValue::as_str).map(str::to_owned));
        Ok(match node.target {
            Target::Node(id, Some(element)) => {
                let kind = element.kind();
                match map::dest(element, prop.as_str()) {
                    Dest::Text => Stream::Text(id),
                    Dest::Drop => Stream::Ignored,
                    Dest::Attr(name)
                        if kind.spec().attr(name).is_some_and(|a| a.ty == loa_core::ValueType::Str && a.values.is_empty()) =>
                    {
                        Stream::Attr { node: id, kind, name: name.to_owned(), buf: initial().unwrap_or_default() }
                    }
                    _ => Stream::Attr {
                        node: id,
                        kind,
                        name: format!("{}{}", schema::EXTENSION_PREFIX, prop.as_str()),
                        buf: initial().unwrap_or_default(),
                    },
                }
            }
            Target::Node(id, None) => Stream::Attr {
                node: id,
                kind: ElementKind::Ext,
                name: ext_name(prop.as_str()),
                buf: initial().unwrap_or_default(),
            },
            Target::Unset => return Err(TranslateError::UnknownHandle(h.0)),
            Target::Root | Target::Flat | Target::Assistant => Stream::Ignored,
        })
    }

    /// Replaces the mirror with `snapshot` and brings the app DOM in line:
    /// nodes omp2 no longer has are removed, new ones inserted, known ones
    /// kept with changed props set.
    fn reset(&mut self, sink: &dyn PatchSink, snapshot: Snapshot, ts: i64) -> Result<()> {
        self.old = std::mem::take(&mut self.mirror);
        self.high_water = snapshot.high_water;
        for node in snapshot.nodes {
            let body = node.node;
            self.mirror.insert(
                node.handle,
                Mirror {
                    tag: body.tag,
                    parent: node.parent,
                    kids: body.kids,
                    props: body.props,
                    content: body.content,
                    target: Target::Unset,
                },
            );
        }
        if !self.mirror.contains_key(&ROOT) {
            self.old.clear();
            return Err(TranslateError::Malformed("a snapshot without the session root"));
        }
        self.seed = snapshot.streams.iter().map(|s| ((s.node, s.prop.clone()), s.text.clone())).collect();
        self.streams.clear();
        self.open_messages.clear();

        let stale: Vec<(Handle, NodeId)> =
            self.ids.app.iter().filter(|(h, _)| !self.mirror.contains_key(h)).map(|(h, id)| (*h, *id)).collect();
        let stale_ids: HashSet<NodeId> = stale.iter().map(|(_, id)| *id).collect();
        for (h, id) in &stale {
            let parent_stale = self.ids.omp2.get(id).is_some_and(|(_, parent)| self.ids.under(*parent, &stale_ids));
            if !parent_stale {
                sink.commit_at(Draft::remove(*id), ts)?;
            }
            self.ids.unbind(*h);
        }
        // Bindings under a removed node point at nodes that are gone too.
        let orphans: Vec<Handle> =
            self.ids.app.iter().filter(|(_, id)| self.ids.under(**id, &stale_ids)).map(|(h, _)| *h).collect();
        let mut gone = stale_ids;
        for h in orphans {
            gone.extend(self.ids.unbind(h));
        }
        self.forget_ids(&gone);

        let result = self.reconcile(sink, ROOT, ts);
        if result.is_ok() {
            for stream in &snapshot.streams {
                let target = self.stream_target(stream.node, &stream.prop, Some(&stream.text))?;
                if let Stream::Text(id) = target
                    && self.mirror.get(&stream.node).is_some_and(|m| matches!(m.target, Target::Node(_, Some(Element::Block))))
                {
                    self.open_messages.insert(id);
                }
                self.streams.insert(stream.sid, target);
            }
        }
        self.old.clear();
        self.seed.clear();
        result
    }

    /// Visits `h` during a reset (or below a fresh insert): a bound node is
    /// kept and updated, anything else is materialized.
    fn reconcile(&mut self, sink: &dyn PatchSink, h: Handle, ts: i64) -> Result<()> {
        let Some(id) = self.ids.app(h) else {
            if h == ROOT {
                self.set_target(h, Target::Root);
                self.update_props(sink, h, ts)?;
                for kid in self.node(h)?.kids.clone() {
                    self.reconcile(sink, kid, ts)?;
                }
                return Ok(());
            }
            return self.materialize(sink, h, false, ts);
        };
        let node = self.node(h)?;
        let parent = node.parent.ok_or(TranslateError::Malformed("a bound node without a parent"))?;
        let (_, parent_kind) = self.app_parent_of(parent)?;
        let target = match map::rule(&node.tag, &self.node(parent)?.tag, parent_kind) {
            Rule::Element(element) if parent_kind.is_some_and(|k| element.kind().spec().parents.contains(&k)) => {
                Target::Node(id, Some(element))
            }
            Rule::Assistant => Target::Assistant,
            _ => Target::Node(id, None),
        };
        let call_id = if matches!(target, Target::Node(_, Some(Element::Call))) {
            node.prop("id").and_then(WValue::as_str).map(str::to_owned)
        } else {
            None
        };
        let seeded: Vec<String> = if let Target::Node(_, Some(element)) = target {
            node.props.iter()
                .filter(|(p, _)| map::dest(element, p.as_str()) == Dest::Text)
                .filter_map(|(p, _)| self.seed.get(&(h, p.clone())).cloned())
                .collect()
        } else {
            Vec::new()
        };
        self.set_target(h, target);
        if let Target::Node(_, Some(element)) = target {
            if element == Element::Turn {
                self.turn = Some(id);
            }
            if let Some(call_id) = call_id {
                self.calls.insert(call_id, id);
            }
            for text in seeded {
                sink.commit_at(Draft::set_text(id, text), ts)?;
            }
        }
        self.update_props(sink, h, ts)?;
        for kid in self.node(h)?.kids.clone() {
            self.reconcile(sink, kid, ts)?;
        }
        Ok(())
    }

    /// Writes the props of `h` that differ from the pre-snapshot mirror, or
    /// all of them when `h` wasn't mirrored before (a resumed session).
    fn update_props(&mut self, sink: &dyn PatchSink, h: Handle, ts: i64) -> Result<()> {
        let node = self.node(h)?;
        let old = self.old.get(&h);
        let changed: Vec<(PropName, WValue)> = node
            .props
            .iter()
            .filter(|(prop, value)| old.is_none_or(|o| o.prop(prop.as_str()) != Some(value)))
            .cloned()
            .collect();
        for (prop, value) in changed {
            self.on_set(sink, h, prop.as_str(), &value, ts)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{MemorySink, Op as AppOp};

    fn setup() -> (MemorySink, Translator) {
        let sink = MemorySink::new();
        let root = sink.commit(Draft::insert(
            NodeId::DOCUMENT,
            ElementKind::Session,
            [
                (attr::ID, Value::from("test-session")),
                (attr::HARNESS, Value::from("omp2")),
                (attr::NODE, Value::from("test-node")),
                (attr::CREATED, Value::Int(100)),
            ],
            "",
        )).unwrap().node;
        (sink, Translator::new(root))
    }

    fn event(line: &str) -> DomEvent {
        match wire::parse_line(line).unwrap() {
            wire::Line::Dom(event) => event,
            wire::Line::Frame(_) => panic!("expected a DOM event"),
        }
    }

    #[test]
    fn maps_patch_and_stream_text_with_stable_bidirectional_ids() {
        let (sink, mut translator) = setup();
        translator.apply(&sink, event(r#"{"kind":"patch@1","data":{"cause":"a","prior":null,"label":null,"ops":[["ins",3,null,{"tag":"turn","props":[["id","turn-a"]]}],["ins",5,null,{"tag":"assistant","props":[["model","test-model"]]}],["ins",6,null,{"tag":"tool:assistant-content","props":[["kind","text"],["index",0]],"content":""}]]}}"#), 101).unwrap();
        let turn = translator.ids().app(Handle(5)).unwrap();
        let message = translator.ids().app(Handle(7)).unwrap();
        assert_eq!(translator.ids().omp2(message), Some(Handle(7)));
        assert_eq!(translator.ids().app(Handle(6)), None);
        translator.apply(&sink, event(r#"{"kind":"stream@1","sid":10,"op":"open","node":7,"prop":"text"}"#), 102).unwrap();
        translator.apply(&sink, event(r#"{"kind":"stream@1","sid":10,"op":"append","text":"hello "}"#), 103).unwrap();
        translator.apply(&sink, event(r#"{"kind":"stream@1","sid":10,"op":"append","text":"world"}"#), 104).unwrap();
        translator.apply(&sink, event(r#"{"kind":"stream@1","sid":10,"op":"close"}"#), 105).unwrap();
        let dom = sink.dom();
        let msg = dom.get(message).unwrap();
        assert_eq!(msg.text, "hello world");
        assert_eq!(msg.str_attr(attr::STATUS), Some("done"));
        assert_eq!(msg.str_attr(attr::MODEL), Some("test-model"));
        assert_eq!(msg.parent, turn);
        assert!(sink.patches().iter().any(|p| p.node == message && matches!(&p.op, AppOp::Append { text } if text == "hello ")));
    }

    #[test]
    fn unknown_tags_and_props_survive_in_extensions() {
        let (sink, mut translator) = setup();
        translator.apply(&sink, event(r#"{"kind":"patch@1","data":{"cause":"a","ops":[["ins",3,null,{"tag":"turn"}],["ins",5,null,{"tag":"notice","props":[["custom:opaque",{"a":1}]],"content":"raw"}]]}}"#), 101).unwrap();
        let id = translator.ids().app(Handle(6)).unwrap();
        let node = sink.dom().get(id).unwrap().clone();
        assert_eq!(node.kind, ElementKind::Ext);
        assert_eq!(node.str_attr(attr::TYPE), Some("notice"));
        assert_eq!(node.str_attr("x-opaque"), Some(r#"{"a":1}"#));
        translator.apply(&sink, event(r#"{"kind":"patch@1","data":{"cause":"b","ops":[["rm",6]]}}"#), 102).unwrap();
        assert!(sink.dom().get(id).is_none());
        assert_eq!(translator.ids().omp2(id), None);
    }

    #[test]
    fn move_reparents_with_fresh_app_id_and_keeps_native_handle() {
        let (sink, mut translator) = setup();
        translator.apply(&sink, event(r#"{"kind":"patch@1","data":{"ops":[["ins",3,null,{"tag":"turn","props":[["id","first"]]}],["ins",3,5,{"tag":"turn","props":[["id","second"]]}],["ins",5,null,{"tag":"user","props":[["text","hello"]]}]]}}"#), 101).unwrap();
        let original = translator.ids().app(Handle(7)).unwrap();
        let destination = translator.ids().app(Handle(6)).unwrap();
        translator.apply(&sink, event(r#"{"kind":"patch@1","data":{"ops":[["mv",7,6,null]]}}"#), 102).unwrap();
        let moved = translator.ids().app(Handle(7)).unwrap();
        assert_ne!(original, moved);
        assert!(sink.dom().get(original).is_none());
        assert_eq!(sink.dom().get(moved).unwrap().parent, destination);
        assert_eq!(sink.dom().get(moved).unwrap().text, "hello");
        assert_eq!(translator.ids().omp2(moved), Some(Handle(7)));
    }

    #[test]
    fn snapshot_reconciles_removed_nodes_and_resumes_streams() {
        let (sink, mut translator) = setup();
        translator.apply(&sink, event(r#"{"kind":"patch@1","data":{"ops":[["ins",3,null,{"tag":"turn"}],["ins",5,null,{"tag":"user","props":[["text","obsolete"]]}]]}}"#), 101).unwrap();
        let removed = translator.ids().app(Handle(6)).unwrap();
        translator.apply(&sink, event(r#"{"kind":"snapshot@1","data":{"high_water":8,"nodes":[{"handle":1,"parent":null,"node":{"tag":"session","kids":[2,3,4]}},{"handle":2,"parent":1,"node":{"tag":"meta"}},{"handle":3,"parent":1,"node":{"tag":"body","kids":[5]}},{"handle":4,"parent":1,"node":{"tag":"queues"}},{"handle":5,"parent":3,"node":{"tag":"turn","kids":[7]}},{"handle":7,"parent":5,"node":{"tag":"assistant","kids":[8]}},{"handle":8,"parent":7,"node":{"tag":"tool:assistant-content","props":[["kind","text"],["text","partial"]]}}],"streams":[{"sid":4,"node":8,"prop":"text","text":"partial"}]}}"#), 102).unwrap();
        assert!(sink.dom().get(removed).is_none());
        let message = translator.ids().app(Handle(8)).unwrap();
        translator.apply(&sink, event(r#"{"kind":"stream@1","sid":4,"op":"append","text":" result"}"#), 103).unwrap();
        translator.apply(&sink, event(r#"{"kind":"stream@1","sid":4,"op":"close"}"#), 104).unwrap();
        assert_eq!(sink.dom().get(message).unwrap().text, "partial result");
        assert_eq!(sink.dom().get(message).unwrap().str_attr(attr::STATUS), Some("done"));
    }
}
