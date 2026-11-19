//! DOM diff and rewind plans.
//!
//! A rewind compares two states of one session: the DOM now and the DOM at an
//! earlier patch position. [`diff`] lists what differs by node id, and
//! [`rewind_plan`] turns that into what the app must do: which live work to
//! cancel, which to resume, and how to bring the session to the target state
//! (patches when they can express it, a fork otherwise).
//!
//! Both DOMs are expected to come from the same session, so a node id means
//! the same element in both. Patches never move a node or change its kind, so
//! an id whose kind or parent differs between the two is treated as two
//! different elements: removed from the old DOM and added in the new one.

use loa_core::schema::TEXT;
use loa_core::{
    CallStatus, DecisionStatus, Dom, Draft, ElementKind, JobStatus, MessageStatus, Node, NodeId, TurnStatus, Value,
    attr,
};
use std::collections::HashSet;

/// What differs between two DOMs, borrowing from both.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Diff<'a> {
    /// Nodes only in the old DOM, in its document order. Includes every node
    /// of a removed subtree.
    pub removed: Vec<NodeId>,
    /// Nodes only in the new DOM, in its document order.
    pub added: Vec<NodeId>,
    /// Nodes in both whose attributes or text differ, in the old DOM's
    /// document order.
    pub changed: Vec<Change<'a>>,
    /// The removed nodes whose parent stays: removing these removes the rest.
    removed_tops: Vec<NodeId>,
}

/// How one node present in both DOMs differs.
#[derive(Clone, Debug, PartialEq)]
pub struct Change<'a> {
    pub node: NodeId,
    /// Attributes that were added, cleared or changed, sorted by name.
    pub attrs: Vec<AttrChange<'a>>,
    pub text: Option<TextChange<'a>>,
}

/// One attribute that differs. `None` means absent on that side.
#[derive(Clone, Debug, PartialEq)]
pub struct AttrChange<'a> {
    pub name: &'a str,
    pub old: Option<&'a Value>,
    pub new: Option<&'a Value>,
}

/// Node text that differs.
#[derive(Clone, Debug, PartialEq)]
pub struct TextChange<'a> {
    pub old: &'a str,
    pub new: &'a str,
}

/// Why a [`Diff`] can't be expressed as drafts.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum Inexpressible {
    /// Ids are never reused, so a node only in the new DOM can't be inserted
    /// again under its id.
    #[error("node {0} exists only in the target state and ids are never reused")]
    Added(NodeId),
    /// An `<ext>` attribute named like the reserved text name: a `set` on it
    /// would replace the node text instead.
    #[error("node {0} has an attribute named `{TEXT}`, which a set patch can't write")]
    TextAttr(NodeId),
}

pub type Result<T, E = Inexpressible> = std::result::Result<T, E>;

impl Diff<'_> {
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.added.is_empty() && self.changed.is_empty()
    }

    /// Drafts that turn the old DOM into the new one: removes of the removed
    /// subtrees, then sets and appends on changed nodes. Committing them in
    /// order to the old DOM yields a DOM whose nodes equal the new DOM's.
    pub fn drafts(&self) -> Result<Vec<Draft>> {
        if let Some(&id) = self.added.first() {
            return Err(Inexpressible::Added(id));
        }
        let sets: usize = self.changed.iter().map(|c| c.attrs.len() + usize::from(c.text.is_some())).sum();
        let mut drafts = Vec::with_capacity(self.removed_tops.len() + sets);
        drafts.extend(self.removed_tops.iter().map(|&node| Draft::Remove { node }));
        for change in &self.changed {
            for a in &change.attrs {
                if a.name == TEXT {
                    return Err(Inexpressible::TextAttr(change.node));
                }
                drafts.push(Draft::Set { node: change.node, name: a.name.to_owned(), value: a.new.cloned() });
            }
            if let Some(text) = &change.text {
                drafts.push(match text.new.strip_prefix(text.old) {
                    Some(tail) if !text.old.is_empty() => Draft::append(change.node, tail),
                    _ => Draft::set_text(change.node, text.new),
                });
            }
        }
        Ok(drafts)
    }
}

/// Lists the nodes removed, added and changed going from `old` to `new`.
pub fn diff<'a>(old: &'a Dom, new: &'a Dom) -> Diff<'a> {
    let mut out = Diff::default();
    let same = |a: &Node, b: &Node| a.kind == b.kind && a.parent == b.parent;
    for o in old.iter() {
        match new.get(o.id) {
            Some(n) if same(o, n) => {
                if let Some(change) = compare(o, n) {
                    out.changed.push(change);
                }
            }
            _ => out.removed.push(o.id),
        }
    }
    for n in new.iter() {
        if !old.get(n.id).is_some_and(|o| same(o, n)) {
            out.added.push(n.id);
        }
    }
    let removed: HashSet<NodeId> = out.removed.iter().copied().collect();
    out.removed_tops = out
        .removed
        .iter()
        .copied()
        .filter(|id| old.get(*id).is_some_and(|n| !removed.contains(&n.parent)))
        .collect();
    out
}

fn compare<'a>(old: &'a Node, new: &'a Node) -> Option<Change<'a>> {
    let mut attrs = Vec::new();
    let mut o = old.attrs.iter().peekable();
    let mut n = new.attrs.iter().peekable();
    // Both maps iterate sorted by name: merge them.
    loop {
        let change = match (o.peek().copied(), n.peek().copied()) {
            (None, None) => break,
            (Some((ok, ov)), Some((nk, nv))) if ok == nk => {
                let change = (!same_value(ov, nv)).then(|| AttrChange { name: ok, old: Some(ov), new: Some(nv) });
                o.next();
                n.next();
                change
            }
            (Some((ok, ov)), Some((nk, _))) if ok < nk => {
                o.next();
                Some(AttrChange { name: ok, old: Some(ov), new: None })
            }
            (Some((ok, ov)), None) => {
                o.next();
                Some(AttrChange { name: ok, old: Some(ov), new: None })
            }
            (_, Some((nk, nv))) => {
                n.next();
                Some(AttrChange { name: nk, old: None, new: Some(nv) })
            }
        };
        attrs.extend(change);
    }
    let text = (old.text != new.text).then(|| TextChange { old: &old.text, new: &new.text });
    (!attrs.is_empty() || text.is_some()).then_some(Change { node: old.id, attrs, text })
}

/// Value equality where a float equals itself bit for bit, so a `NaN`
/// attribute doesn't show up as a change of a node against itself.
fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::List(x), Value::List(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same_value(x, y)),
        _ => a == b,
    }
}

/// True for work in progress that a rewind has to stop or restart: running or
/// waiting turns, streaming messages, pending or running calls, running jobs
/// and waiting decisions.
pub fn is_live(node: &Node) -> bool {
    let Some(status) = node.str_attr(attr::STATUS) else { return false };
    match node.kind {
        ElementKind::Turn => matches!(TurnStatus::parse(status), Some(TurnStatus::Running | TurnStatus::Waiting)),
        ElementKind::Message => MessageStatus::parse(status) == Some(MessageStatus::Streaming),
        ElementKind::Call => matches!(CallStatus::parse(status), Some(CallStatus::Pending | CallStatus::Running)),
        ElementKind::Job => JobStatus::parse(status) == Some(JobStatus::Running),
        ElementKind::Decision => DecisionStatus::parse(status) == Some(DecisionStatus::Waiting),
        _ => false,
    }
}

/// What the app does to rewind a session from its current DOM to a target DOM.
#[derive(Clone, Debug, PartialEq)]
pub struct RewindPlan {
    /// Live in the current DOM and gone or no longer live in the target: stop
    /// them in the harness (abort the call, kill the job, withdraw the
    /// decision). Deepest first, so children stop before their turn.
    pub cancel: Vec<NodeId>,
    /// Live in the target DOM and gone or no longer live in the current one:
    /// restart them once the session is at the target. Parents first.
    pub resume: Vec<NodeId>,
    /// How the session reaches the target state.
    pub apply: Apply,
}

/// How a rewind moves the session DOM to the target state.
#[derive(Clone, Debug, PartialEq)]
pub enum Apply {
    /// Commit these drafts, in order, to the current session.
    Drafts(Vec<Draft>),
    /// Patches can't express the change (see `why`): fork the session's
    /// journal at patch `seq`, the target DOM's sequence number, and continue
    /// in the fork.
    Fork { seq: u64, why: Inexpressible },
}

/// Plans a rewind from `current` to `target`, an earlier state of the same
/// session.
pub fn rewind_plan(current: &Dom, target: &Dom) -> RewindPlan {
    let live_only_in = |dom: &Dom, other: &Dom| -> Vec<NodeId> {
        dom.iter().filter(|n| is_live(n) && !other.get(n.id).is_some_and(is_live)).map(|n| n.id).collect()
    };
    let mut cancel = live_only_in(current, target);
    cancel.reverse();
    let resume = live_only_in(target, current);
    let apply = match diff(current, target).drafts() {
        Ok(drafts) => Apply::Drafts(drafts),
        Err(why) => Apply::Fork { seq: target.seq(), why },
    };
    RewindPlan { cancel, resume, apply }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{DecisionKind, JobKind, Role};

    fn no_attrs() -> Vec<(&'static str, Value)> {
        Vec::new()
    }

    fn session() -> (Dom, NodeId) {
        let mut dom = Dom::new();
        let attrs = [
            (attr::ID, Value::from("s1")),
            (attr::HARNESS, "omp".into()),
            (attr::NODE, "k".into()),
            (attr::CREATED, Value::Int(1)),
        ];
        let s = dom.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""), 1).unwrap().node;
        (dom, s)
    }

    fn insert(dom: &mut Dom, parent: NodeId, kind: ElementKind, attrs: Vec<(&'static str, Value)>) -> NodeId {
        dom.commit(Draft::insert(parent, kind, attrs, ""), 0).unwrap().node
    }

    fn set(dom: &mut Dom, node: NodeId, name: &str, value: impl Into<Value>) {
        dom.commit(Draft::set(node, name, value), 0).unwrap();
    }

    /// Node-for-node equality, ignoring sequence numbers and the next id.
    fn same_tree(a: &Dom, b: &Dom) -> bool {
        a.len() == b.len() && a.root().map(|n| n.id) == b.root().map(|n| n.id) && b.iter().all(|n| a.get(n.id) == Some(n))
    }

    fn apply(dom: &Dom, drafts: &[Draft]) -> Dom {
        let mut dom = dom.clone();
        for draft in drafts {
            dom.commit(draft.clone(), 0).unwrap();
        }
        dom
    }

    #[test]
    fn rewinding_past_a_running_turn_cancels_its_live_work_and_removes_it() {
        let (mut dom, s) = session();
        let t1 = insert(&mut dom, s, ElementKind::Turn, vec![(attr::STATUS, "running".into())]);
        set(&mut dom, t1, attr::STATUS, TurnStatus::Done);
        let target = dom.clone();

        let t2 = insert(&mut dom, s, ElementKind::Turn, vec![(attr::STATUS, "running".into())]);
        let user = insert(&mut dom, t2, ElementKind::Message, vec![(attr::ROLE, Role::User.into()), (attr::STATUS, "done".into())]);
        let reply = insert(
            &mut dom,
            t2,
            ElementKind::Message,
            vec![(attr::ROLE, Role::Assistant.into()), (attr::STATUS, "streaming".into())],
        );
        let call = insert(&mut dom, t2, ElementKind::Call, vec![(attr::TOOL, "bash".into()), (attr::STATUS, "running".into())]);
        let ask = insert(
            &mut dom,
            call,
            ElementKind::Decision,
            vec![(attr::KIND, DecisionKind::Approval.into()), (attr::STATUS, "waiting".into())],
        );
        let job = insert(&mut dom, s, ElementKind::Job, vec![(attr::KIND, JobKind::Shell.into()), (attr::STATUS, "running".into())]);
        set(&mut dom, s, attr::MODEL, "m2");

        let d = diff(&dom, &target);
        assert_eq!(d.removed, vec![t2, user, reply, call, ask, job]);
        assert!(d.added.is_empty());

        let plan = rewind_plan(&dom, &target);
        assert_eq!(plan.cancel, vec![job, ask, call, reply, t2]);
        assert!(plan.resume.is_empty());
        let Apply::Drafts(drafts) = &plan.apply else { panic!("expected drafts, got {:?}", plan.apply) };
        assert_eq!(drafts, &vec![Draft::remove(t2), Draft::remove(job), Draft::clear(s, attr::MODEL)]);
        assert!(same_tree(&apply(&dom, drafts), &target));
    }

    #[test]
    fn rewinding_into_an_unfinished_turn_resumes_it() {
        let (mut dom, s) = session();
        let turn = insert(&mut dom, s, ElementKind::Turn, vec![(attr::STATUS, "running".into())]);
        let reply = insert(
            &mut dom,
            turn,
            ElementKind::Message,
            vec![(attr::ROLE, Role::Assistant.into()), (attr::STATUS, "streaming".into())],
        );
        dom.commit(Draft::append(reply, "Hel"), 0).unwrap();
        let target = dom.clone();

        dom.commit(Draft::append(reply, "lo"), 0).unwrap();
        set(&mut dom, reply, attr::STATUS, MessageStatus::Done);
        set(&mut dom, turn, attr::STATUS, TurnStatus::Done);

        let plan = rewind_plan(&dom, &target);
        assert!(plan.cancel.is_empty());
        assert_eq!(plan.resume, vec![turn, reply]);
        let Apply::Drafts(drafts) = &plan.apply else { panic!("expected drafts, got {:?}", plan.apply) };
        let rewound = apply(&dom, drafts);
        assert!(same_tree(&rewound, &target));
        assert_eq!(rewound.get(reply).unwrap().text, "Hel");
    }

    #[test]
    fn restoring_removed_nodes_needs_a_fork_at_the_target_position() {
        let (mut dom, s) = session();
        let turn = insert(&mut dom, s, ElementKind::Turn, vec![(attr::STATUS, "running".into())]);
        let call = insert(&mut dom, turn, ElementKind::Call, vec![(attr::TOOL, "read".into()), (attr::STATUS, "running".into())]);
        let target = dom.clone();
        dom.commit(Draft::remove(turn), 0).unwrap();

        let d = diff(&dom, &target);
        assert_eq!(d.added, vec![turn, call]);
        assert_eq!(d.drafts(), Err(Inexpressible::Added(turn)));

        let plan = rewind_plan(&dom, &target);
        assert_eq!(plan.resume, vec![turn, call]);
        assert_eq!(plan.apply, Apply::Fork { seq: target.seq(), why: Inexpressible::Added(turn) });
    }

    #[test]
    fn diff_reports_attribute_and_text_changes() {
        let (mut dom, s) = session();
        let todo = insert(&mut dom, s, ElementKind::Todo, vec![(attr::STATE, "pending".into()), (attr::PHASE, "a".into())]);
        dom.commit(Draft::set_text(todo, "write tests"), 0).unwrap();
        let old = dom.clone();
        set(&mut dom, todo, attr::STATE, "done");
        dom.commit(Draft::clear(todo, attr::PHASE), 0).unwrap();
        set(&mut dom, todo, attr::ID, "t1");
        dom.commit(Draft::append(todo, " and docs"), 0).unwrap();

        let d = diff(&old, &dom);
        assert!(d.removed.is_empty() && d.added.is_empty());
        let pending = Value::from("pending");
        let done = Value::from("done");
        let phase = Value::from("a");
        let id = Value::from("t1");
        assert_eq!(
            d.changed,
            vec![Change {
                node: todo,
                attrs: vec![
                    AttrChange { name: attr::ID, old: None, new: Some(&id) },
                    AttrChange { name: attr::PHASE, old: Some(&phase), new: None },
                    AttrChange { name: attr::STATE, old: Some(&pending), new: Some(&done) },
                ],
                text: Some(TextChange { old: "write tests", new: "write tests and docs" }),
            }]
        );
        // Text that only grew is replayed as an append, like streaming.
        assert_eq!(d.drafts().unwrap().last(), Some(&Draft::append(todo, " and docs")));
        assert!(same_tree(&apply(&old, &d.drafts().unwrap()), &dom));
    }

    #[test]
    fn nan_attribute_is_not_a_change() {
        let (mut dom, s) = session();
        set(&mut dom, s, "x-f", Value::List(vec![Value::Float(f64::NAN)]));
        assert!(diff(&dom, &dom.clone()).is_empty());
    }

    #[test]
    fn ext_attribute_named_like_text_forces_a_fork() {
        // `<ext>` accepts any attribute name on insert, but a set of `#text`
        // replaces the node text, so patches can't add this attribute.
        let (mut target, s) = session();
        let ext = insert(&mut target, s, ElementKind::Ext, vec![(attr::HARNESS, "omp".into()), (TEXT, "raw".into())]);
        let (mut current, _) = session();
        insert(&mut current, s, ElementKind::Ext, vec![(attr::HARNESS, "omp".into())]);
        assert_eq!(diff(&current, &target).drafts(), Err(Inexpressible::TextAttr(ext)));
        assert_eq!(
            rewind_plan(&current, &target).apply,
            Apply::Fork { seq: target.seq(), why: Inexpressible::TextAttr(ext) }
        );
    }

    #[test]
    fn same_id_with_another_kind_is_a_different_element() {
        let (mut a, s) = session();
        let turn = insert(&mut a, s, ElementKind::Turn, no_attrs());
        let (mut b, _) = session();
        let todo = insert(&mut b, s, ElementKind::Todo, no_attrs());
        assert_eq!(turn, todo);
        let d = diff(&a, &b);
        assert_eq!((d.removed, d.added), (vec![turn], vec![todo]));
    }

    /// Small deterministic generator (xorshift64*) for the property test.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
            &items[self.below(items.len())]
        }
    }

    /// One random valid draft for `dom`, or `None` when the pick doesn't fit.
    fn random_draft(rng: &mut Rng, dom: &Dom) -> Option<Draft> {
        let nodes: Vec<&Node> = dom.iter().collect();
        let node = *rng.pick(&nodes);
        let status = |rng: &mut Rng, kind: ElementKind| -> Option<&'static str> {
            let values: &[&str] = match kind {
                ElementKind::Turn => TurnStatus::VALUES,
                ElementKind::Message => MessageStatus::VALUES,
                ElementKind::Call => CallStatus::VALUES,
                ElementKind::Job => JobStatus::VALUES,
                ElementKind::Decision => DecisionStatus::VALUES,
                _ => return None,
            };
            Some(rng.pick(values))
        };
        match rng.below(10) {
            0..=3 => {
                let (kind, mut attrs): (ElementKind, Vec<(&str, Value)>) = match node.kind {
                    ElementKind::Session => match rng.below(3) {
                        0 => (ElementKind::Job, vec![(attr::KIND, JobKind::Subagent.into())]),
                        1 => (ElementKind::Todo, vec![]),
                        _ => (ElementKind::Turn, vec![]),
                    },
                    ElementKind::Turn => match rng.below(3) {
                        0 => (ElementKind::Message, vec![(attr::ROLE, Role::Assistant.into())]),
                        1 => (ElementKind::Call, vec![(attr::TOOL, "bash".into())]),
                        _ => (ElementKind::Decision, vec![(attr::KIND, DecisionKind::Question.into())]),
                    },
                    ElementKind::Call => (ElementKind::Input, vec![]),
                    _ => (ElementKind::Ext, vec![(attr::HARNESS, "omp".into()), ("x-n", Value::Int(rng.below(5) as i64))]),
                };
                if let Some(s) = status(rng, kind) {
                    attrs.push((attr::STATUS, s.into()));
                }
                let index = rng.below(node.children.len() + 1) as u32;
                let text = if rng.below(2) == 0 { "" } else { "seed" };
                Some(Draft::insert_at(node.id, index, kind, attrs, text))
            }
            4 | 5 => status(rng, node.kind).map(|s| Draft::set(node.id, attr::STATUS, s)),
            6 => Some(match rng.below(3) {
                0 => Draft::set(node.id, "x-v", Value::Int(rng.below(3) as i64)),
                1 => Draft::set(node.id, "x-w", Value::List(vec![Value::Float(0.5), Value::Bytes(vec![1])])),
                _ => Draft::clear(node.id, if rng.below(2) == 0 { "x-v" } else { "x-w" }),
            }),
            7 => Some(Draft::append(node.id, *rng.pick(&["a", "bc", ""]))),
            8 => Some(Draft::set_text(node.id, *rng.pick(&["", "x", "seed"]))),
            _ => (node.kind != ElementKind::Session).then(|| Draft::remove(node.id)),
        }
    }

    #[test]
    fn drafts_of_a_diff_to_an_earlier_state_rebuild_it() {
        let mut checked_drafts = 0;
        let mut checked_forks = 0;
        for seed in 1..=40u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let (mut dom, _) = session();
            let mut states = vec![dom.clone()];
            while states.len() < 60 {
                if let Some(draft) = random_draft(&mut rng, &dom) {
                    dom.commit(draft, 0).unwrap();
                    states.push(dom.clone());
                }
            }
            for j in 0..states.len() {
                for i in 0..=j {
                    let (current, target) = (&states[j], &states[i]);
                    let d = diff(current, target);
                    let expressible = target.iter().all(|n| current.contains(n.id));
                    let plan = rewind_plan(current, target);
                    match (d.drafts(), &plan.apply) {
                        (Ok(drafts), Apply::Drafts(planned)) => {
                            assert!(expressible, "seed {seed}: {i} <- {j}");
                            assert_eq!(&drafts, planned);
                            let rewound = apply(current, &drafts);
                            assert!(same_tree(&rewound, target), "seed {seed}: {i} <- {j}");
                            assert!(diff(&rewound, target).is_empty());
                            // Nothing live survives the rewind unless the
                            // target had it live, and resumed work is live.
                            for n in rewound.iter().filter(|n| is_live(n)) {
                                assert!(target.get(n.id).is_some_and(is_live));
                            }
                            for id in &plan.resume {
                                assert!(rewound.get(*id).is_some_and(is_live));
                            }
                            checked_drafts += 1;
                        }
                        (Err(why), Apply::Fork { seq, why: planned }) => {
                            assert!(!expressible, "seed {seed}: {i} <- {j}");
                            assert_eq!((&why, *seq), (planned, target.seq()));
                            checked_forks += 1;
                        }
                        (drafts, apply) => panic!("diff says {drafts:?}, plan says {apply:?}"),
                    }
                    for id in &plan.cancel {
                        assert!(current.get(*id).is_some_and(is_live));
                        assert!(!target.get(*id).is_some_and(is_live));
                    }
                }
            }
        }
        assert!(checked_drafts > 1000 && checked_forks > 1000, "{checked_drafts} drafts, {checked_forks} forks");
    }
}
