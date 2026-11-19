use loa_core::schema::TEXT;
use loa_core::{
    ApplyError, CallStatus, Dom, Draft, ElementKind, MemorySink, Node, NodeId, Op, PartsError, Patch, PatchSink, Role,
    SchemaError, Value, attr,
};

fn session_attrs() -> Vec<(&'static str, Value)> {
    vec![
        (attr::ID, "s1".into()),
        (attr::HARNESS, "omp".into()),
        (attr::NODE, "nodekey".into()),
        (attr::CREATED, Value::Int(1)),
    ]
}

/// Session with one turn holding a user message. Returns (dom, session, turn, message).
fn seeded() -> (Dom, NodeId, NodeId, NodeId) {
    let mut dom = Dom::new();
    let session = dom.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, session_attrs(), ""), 1).unwrap().node;
    let turn = dom.commit(Draft::insert(session, ElementKind::Turn, Vec::<(&str, Value)>::new(), ""), 2).unwrap().node;
    let message = dom
        .commit(Draft::insert(turn, ElementKind::Message, [(attr::ROLE, Value::from(Role::User))], "hi"), 3)
        .unwrap()
        .node;
    (dom, session, turn, message)
}

#[test]
fn streaming_appends_build_the_message_text() {
    let (mut dom, _, turn, _) = seeded();
    let reply = dom
        .commit(Draft::insert(turn, ElementKind::Message, [(attr::ROLE, Value::from(Role::Assistant))], ""), 4)
        .unwrap()
        .node;
    for token in ["Hel", "lo ", "there"] {
        dom.commit(Draft::append(reply, token), 5).unwrap();
    }
    assert_eq!(dom.get(reply).unwrap().text, "Hello there");
    dom.commit(Draft::set_text(reply, "replaced"), 6).unwrap();
    assert_eq!(dom.get(reply).unwrap().text, "replaced");
    dom.commit(Draft::clear(reply, TEXT), 7).unwrap();
    assert_eq!(dom.get(reply).unwrap().text, "");
}

#[test]
fn removed_ids_are_never_handed_out_again() {
    let (mut dom, _, turn, message) = seeded();
    dom.commit(Draft::remove(message), 4).unwrap();
    assert!(!dom.contains(message));

    let reuse = Patch {
        seq: dom.seq() + 1,
        node: message,
        ts: 5,
        op: Op::Insert {
            parent: turn,
            index: None,
            kind: ElementKind::Message,
            attrs: [(attr::ROLE.to_owned(), Value::from(Role::User))].into(),
            text: String::new(),
        },
    };
    assert!(matches!(dom.apply(&reuse), Err(ApplyError::IdReused { .. })));

    let fresh = dom.commit(Draft::insert(turn, ElementKind::Message, [(attr::ROLE, Value::from(Role::User))], ""), 5);
    assert!(fresh.unwrap().node > message);
}

#[test]
fn remove_takes_the_whole_subtree() {
    let (mut dom, session, turn, message) = seeded();
    let call = dom.commit(Draft::insert(turn, ElementKind::Call, [(attr::TOOL, Value::from("bash"))], ""), 4).unwrap().node;
    let input = dom.commit(Draft::insert(call, ElementKind::Input, Vec::<(&str, Value)>::new(), "{}"), 5).unwrap().node;
    dom.commit(Draft::remove(turn), 6).unwrap();
    for gone in [turn, message, call, input] {
        assert!(!dom.contains(gone));
    }
    assert_eq!(dom.get(session).unwrap().children, vec![]);
    assert_eq!(dom.len(), 1);
}

#[test]
fn insert_at_index_orders_children_and_iter_is_document_order() {
    let (mut dom, session, turn, message) = seeded();
    let first = dom
        .commit(Draft::insert_at(turn, 0, ElementKind::Message, [(attr::ROLE, Value::from(Role::System))], ""), 4)
        .unwrap()
        .node;
    assert_eq!(dom.get(turn).unwrap().children, vec![first, message]);
    let order: Vec<NodeId> = dom.iter().map(|n| n.id).collect();
    assert_eq!(order, vec![session, turn, first, message]);
    let up: Vec<NodeId> = dom.ancestors(message).map(|n| n.id).collect();
    assert_eq!(up, vec![turn, session]);

    let past = dom.commit(Draft::insert_at(turn, 9, ElementKind::Message, [(attr::ROLE, Value::from(Role::User))], ""), 5);
    assert!(matches!(past, Err(ApplyError::Index { index: 9, len: 2, .. })));
}

#[test]
fn a_rejected_patch_leaves_the_dom_untouched() {
    let (mut dom, _, turn, _) = seeded();
    let before = dom.clone();

    let gap = Patch { seq: dom.seq() + 2, node: turn, ts: 9, op: Op::Remove };
    assert_eq!(dom.apply(&gap), Err(ApplyError::Sequence { expected: 4, got: 5 }));

    let missing = dom.commit(Draft::append(NodeId(99), "x"), 9);
    assert_eq!(missing, Err(ApplyError::MissingNode(NodeId(99))));

    let second_root = dom.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, session_attrs(), ""), 9);
    assert_eq!(second_root, Err(ApplyError::RootExists));

    assert_eq!(dom, before);
}

#[test]
fn schema_rejects_bad_structure_and_attributes() {
    let (mut dom, session, turn, message) = seeded();
    let schema_err = |r: Result<Patch, ApplyError>| match r {
        Err(ApplyError::Schema(e)) => e,
        other => panic!("expected a schema error, got {other:?}"),
    };

    let misplaced = dom.commit(Draft::insert(session, ElementKind::Input, Vec::<(&str, Value)>::new(), ""), 4);
    assert!(matches!(schema_err(misplaced), SchemaError::Parent { child: ElementKind::Input, .. }));

    let no_role = dom.commit(Draft::insert(turn, ElementKind::Message, Vec::<(&str, Value)>::new(), ""), 4);
    assert!(matches!(schema_err(no_role), SchemaError::Required { name: "role", .. }));

    let bad_enum = dom.commit(Draft::set(message, attr::ROLE, "robot"), 4);
    assert!(matches!(schema_err(bad_enum), SchemaError::EnumValue { .. }));

    let bad_type = dom.commit(Draft::set(turn, attr::STARTED, "yesterday"), 4);
    assert!(matches!(schema_err(bad_type), SchemaError::Type { .. }));

    let unknown = dom.commit(Draft::set(turn, "colour", "red"), 4);
    assert!(matches!(schema_err(unknown), SchemaError::UnknownAttr { .. }));

    let cleared = dom.commit(Draft::clear(message, attr::ROLE), 4);
    assert!(matches!(schema_err(cleared), SchemaError::Required { .. }));
}

#[test]
fn extension_attributes_and_ext_elements_keep_harness_data() {
    let (mut dom, _, turn, message) = seeded();
    dom.commit(Draft::set(message, "x-claude-uuid", "abc"), 4).unwrap();
    let ext = dom
        .commit(
            Draft::insert(
                message,
                ElementKind::Ext,
                [(attr::HARNESS, Value::from("claude-code")), ("anything", Value::Bytes(vec![1, 2]))],
                "{\"raw\":true}",
            ),
            5,
        )
        .unwrap()
        .node;
    assert_eq!(dom.get(ext).unwrap().attr("anything"), Some(&Value::Bytes(vec![1, 2])));
    let call = dom
        .commit(
            Draft::insert(
                turn,
                ElementKind::Call,
                [(attr::TOOL, Value::from("read")), (attr::STATUS, CallStatus::Running.into())],
                "",
            ),
            6,
        )
        .unwrap()
        .node;
    assert_eq!(dom.get(call).unwrap().str_attr(attr::STATUS), Some("running"));
}

#[test]
fn from_parts_round_trips_and_rejects_broken_trees() {
    let (dom, _, turn, message) = seeded();
    let nodes: Vec<Node> = dom.iter().cloned().collect();
    let rebuilt = Dom::from_parts(nodes.clone(), dom.next_id(), dom.seq()).unwrap();
    assert_eq!(rebuilt, dom);

    assert!(matches!(
        Dom::from_parts(nodes.clone(), message, dom.seq()),
        Err(PartsError::NextId { .. })
    ));

    let mut detached = nodes.clone();
    detached.iter_mut().find(|n| n.id == turn).unwrap().children.clear();
    assert_eq!(Dom::from_parts(detached, dom.next_id(), dom.seq()), Err(PartsError::Unreachable(message)));

    let mut dangling = nodes;
    dangling.retain(|n| n.id != message);
    assert_eq!(
        Dom::from_parts(dangling, dom.next_id(), dom.seq()),
        Err(PartsError::Link { child: message, parent: turn })
    );
}

#[test]
fn memory_sink_orders_drafts_and_its_patches_replay_to_the_same_dom() {
    let sink = MemorySink::new();
    let session = sink.commit_at(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, session_attrs(), ""), 10).unwrap();
    assert_eq!((session.seq, session.node, session.ts), (1, NodeId(1), 10));
    let turn = sink.commit(Draft::insert(session.node, ElementKind::Turn, Vec::<(&str, Value)>::new(), "")).unwrap();
    assert_eq!((turn.seq, turn.node), (2, NodeId(2)));
    assert!(sink.commit(Draft::append(NodeId(77), "x")).is_err());

    let mut replayed = Dom::new();
    for patch in sink.patches() {
        replayed.apply(&patch).unwrap();
    }
    assert_eq!(replayed, sink.dom());
    assert_eq!(sink.with_dom(Dom::seq), 2);
}
