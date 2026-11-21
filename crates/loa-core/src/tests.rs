use crate::rewind::{self, Apply, Inexpressible};
use crate::schema::TEXT;
use crate::testkit::DomFixture;
use crate::*;

const TURN: NodeId = NodeId(2);
const MSG: NodeId = NodeId(3);

/// Session 1 > turn 2 > user message 3 "hi".
fn seeded() -> Dom {
    DomFixture::session().message(Role::User, "hi").build()
}

/// (name, the patch to apply, expected error variant prefix).
type Case = (&'static str, fn(&Dom) -> Patch, Option<&'static str>);

#[test]
fn apply_cases() {
    let insert = |d: &Dom, parent, kind, attrs: Attrs| d.prepare(Draft::insert(parent, kind, attrs, ""), 0);
    #[rustfmt::skip]
    let cases: &[Case] = &[
        ("append", |d| d.prepare(Draft::append(MSG, " there"), 0), None),
        ("set text", |d| d.prepare(Draft::set_text(MSG, "new"), 0), None),
        ("clear text", |d| d.prepare(Draft::clear(MSG, TEXT), 0), None),
        ("insert at 0", |d| d.prepare(Draft::insert_at(TURN, 0, ElementKind::Message, attrs! { ROLE: Role::System }, ""), 0), None),
        ("index past end", |d| d.prepare(Draft::insert_at(TURN, 9, ElementKind::Message, attrs! { ROLE: Role::User }, ""), 0), Some("Index")),
        ("x- attribute", |d| d.prepare(Draft::set(MSG, "x-uuid", "abc"), 0), None),
        ("ext keeps any attr", |d| d.prepare(Draft::insert(MSG, ElementKind::Ext, attrs! { HARNESS: "c", "any": vec![1u8] }, "{}"), 0), None),
        ("remove subtree", |d| d.prepare(Draft::remove(TURN), 0), None),
        ("seq gap", |d| Patch { seq: d.seq() + 2, node: TURN, ts: 0, op: Op::Remove }, Some("Sequence")),
        ("missing node", |d| d.prepare(Draft::append(NodeId(99), "x"), 0), Some("MissingNode")),
        ("second root", |d| d.prepare(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, Attrs::new(), ""), 0), Some("RootExists")),
        ("id reused", |d| Patch { node: MSG, ..d.prepare(Draft::insert(TURN, ElementKind::Todo, Attrs::new(), ""), 0) }, Some("IdReused")),
        ("bad parent", |d| d.prepare(Draft::insert(NodeId(1), ElementKind::Input, Attrs::new(), ""), 0), Some("Schema(Parent")),
        ("missing required", |d| d.prepare(Draft::insert(TURN, ElementKind::Message, Attrs::new(), ""), 0), Some("Schema(Required")),
        ("bad enum", |d| d.prepare(Draft::set(MSG, attr::ROLE, "robot"), 0), Some("Schema(EnumValue")),
        ("bad type", |d| d.prepare(Draft::set(TURN, attr::STARTED, "yesterday"), 0), Some("Schema(Type")),
        ("unknown attr", |d| d.prepare(Draft::set(TURN, "colour", "red"), 0), Some("Schema(UnknownAttr")),
        ("clear required", |d| d.prepare(Draft::clear(MSG, attr::ROLE), 0), Some("Schema(Required")),
        ("text takes a string", |d| d.prepare(Draft::set(MSG, TEXT, 1i64), 0), Some("Schema(TextType")),
    ];
    for (name, patch, expected) in cases {
        let mut dom = seeded();
        let before = dom.clone();
        let patch = patch(&dom);
        let checked = dom.check(&patch);
        let applied = dom.apply(&patch);
        assert_eq!(checked, applied, "{name}");
        match (expected, applied) {
            (None, Ok(())) => assert_eq!(dom.seq(), before.seq() + 1, "{name}"),
            (Some(want), Err(e)) => {
                assert!(format!("{e:?}").starts_with(want), "{name}: {e:?}");
                assert_eq!(dom, before, "{name}: a rejected patch changed the DOM");
            }
            (want, got) => panic!("{name}: expected {want:?}, got {got:?}"),
        }
    }
    // A removed id is never handed out again, and remove takes the subtree.
    let mut dom = seeded();
    dom.commit(Draft::remove(TURN), 0).unwrap();
    assert_eq!((dom.len(), dom.contains(MSG)), (1, false));
    assert!(insert(&dom, NodeId(1), ElementKind::Turn, Attrs::new()).node > MSG);
}

#[test]
fn from_parts_and_memory_sink_round_trip() {
    let dom = seeded();
    let nodes: Vec<Node> = dom.iter().cloned().collect();
    assert_eq!(Dom::from_parts(nodes.clone(), dom.next_id(), dom.seq()).unwrap(), dom);
    assert!(matches!(Dom::from_parts(nodes.clone(), MSG, 0), Err(PartsError::NextId { .. })));
    let mut detached = nodes.clone();
    detached[1].children.clear();
    assert_eq!(Dom::from_parts(detached, dom.next_id(), 0), Err(PartsError::Unreachable(MSG)));

    let sink = MemorySink::new();
    let w = DomWriter::new(&sink).at(7);
    let s = w.insert(NodeId::DOCUMENT, ElementKind::Session, attrs! { ID: "s", HARNESS: "h", NODE: "k", CREATED: 1i64 }, "");
    let turn = w.insert(s.unwrap(), ElementKind::Turn, attrs! { STATUS: TurnStatus::Running }, "").unwrap();
    w.close(turn, TurnStatus::Done).unwrap();
    assert!(w.append(NodeId(77), "x").is_err());
    let mut replayed = Dom::new();
    sink.patches().iter().for_each(|p| replayed.apply(p).unwrap());
    assert_eq!(replayed, sink.dom());
    assert_eq!(sink.dom().get(turn).unwrap().int_attr(attr::ENDED), Some(7));
}

#[test]
fn xml_golden() {
    let mut f = DomFixture::session();
    f.message(Role::User, "hi").call("bash", attrs! { "x-n": 1.0 }).decision(DecisionKind::Question);
    let d = f.last;
    f.commit(Draft::set(d, attr::OPTIONS, vec!["Yes".to_owned(), "It's \\ fine".to_owned()]));
    let raw = attrs! { HARNESS: "c", "raw": b"\x00\xffhi".to_vec(), "weird name": "a", "_xA": "b", "x-l": vec![Value::Int(1), Value::Bytes(vec![1, 2])] };
    f.add(f.session, ElementKind::Ext, raw, "a < b && \"d\"\r");
    let expected = r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- loa session dom: schema 1, seq 7, next nid 7 -->
<session nid="1" created.int="1" harness="omp" id="s1" node="k">
  <turn nid="2">
    <message nid="3" role="user">hi</message>
    <call nid="4" tool="bash" x-n.float="1.0">
      <decision nid="5" kind="question" options.list="['Yes', 'It\'s \\ fine']"/>
    </call>
  </turn>
  <ext nid="6" _x005F_xA="b" harness="c" raw.bytes="AP9oaQ==" weird_x0020_name="a" x-l.list="[1, bytes(AQI=)]">a &lt; b &amp;&amp; "d"&#x0D;</ext>
</session>
"#;
    assert_eq!(xml::to_string(&f.dom), expected);
}

#[test]
fn rewind_cases() {
    let running = |f: &mut DomFixture| {
        f.turn();
        let turn = f.last;
        f.commit(Draft::set(turn, attr::STATUS, TurnStatus::Running));
        f.call("bash", attrs! { STATUS: CallStatus::Running });
        (turn, f.last)
    };
    // Rewinding past running work cancels it (deepest first) and removes it.
    let mut f = DomFixture::session();
    let target = f.build();
    let (turn, call) = running(&mut f);
    f.commit(Draft::set(f.session, attr::MODEL, "m2"));
    let plan = rewind::plan(&f.dom, &target);
    assert_eq!((plan.cancel, plan.resume), (vec![call, turn], vec![]));
    assert_eq!(plan.apply, Apply::Drafts(vec![Draft::remove(turn), Draft::clear(f.session, attr::MODEL)]));

    // Rewinding into unfinished work resumes it; grown text replays as an append.
    let mut f = DomFixture::session();
    let (turn, call) = running(&mut f);
    f.commit(Draft::set_text(call, "Hel"));
    let target = f.build();
    f.commit(Draft::append(call, "lo")).commit(Draft::set(call, attr::STATUS, CallStatus::Done));
    f.commit(Draft::set(turn, attr::STATUS, TurnStatus::Done));
    let plan = rewind::plan(&f.dom, &target);
    assert_eq!((plan.cancel, plan.resume), (vec![], vec![turn, call]));
    let Apply::Drafts(drafts) = plan.apply else { panic!("expected drafts") };
    for draft in drafts {
        f.commit(draft);
    }
    assert_eq!(rewind::drafts(&f.dom, &target), Ok(vec![]));

    // Restoring removed nodes needs a fork at the target's position.
    let mut f = DomFixture::session();
    let (turn, _) = running(&mut f);
    let target = f.build();
    f.commit(Draft::remove(turn));
    let why = Inexpressible::Added(turn);
    assert_eq!(rewind::plan(&f.dom, &target).apply, Apply::Fork { seq: target.seq(), why });
}
