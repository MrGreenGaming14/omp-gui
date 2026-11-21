//! The session writer, its subscriptions and the bus registry.

mod common;

use common::{Fixture, OWNER, PEER, fixture, fixture_with};
use loa_bus::{
    Authorize, Bus, BusError, Denied, Event, NodeKey, Policy, ReplicateError, RequestError, SessionId, TryRecvError,
};
use loa_core::testkit::{AllowAll, DenyAll, MemoryPersist};
use loa_core::{ApplyError, CommitError, Dom, Draft, NodeId, Op, PatchSink};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// Lets [`PEER`] change nodes under `scope` only.
struct Scoped(Mutex<Option<NodeId>>);

impl Authorize for Scoped {
    fn authorize(&self, _: &SessionId, who: &NodeKey, draft: &Draft, dom: &Dom) -> Result<(), Denied> {
        let target = match draft {
            Draft::Insert { parent, .. } => *parent,
            Draft::Set { node, .. } | Draft::Append { node, .. } | Draft::Remove { node } => *node,
        };
        let scope = self.0.lock().unwrap().filter(|_| *who == PEER).ok_or_else(|| Denied("no grant".into()))?;
        let inside = target == scope || dom.ancestors(target).any(|n| n.id == scope);
        if inside { Ok(()) } else { Err(Denied("outside the granted turn".into())) }
    }
}

fn scoped() -> (Fixture, Arc<Scoped>) {
    let auth = Arc::new(Scoped(Mutex::new(None)));
    let f = fixture_with(OWNER, MemoryPersist::default(), auth.clone());
    *auth.0.lock().unwrap() = Some(f.turn);
    (f, auth)
}

fn drain(sub: &mut loa_bus::Subscription) -> Vec<Event> {
    std::iter::from_fn(|| sub.try_recv().ok()).collect()
}

#[test]
fn local_commits_and_peer_requests_persist_in_one_gap_free_order() {
    let (f, _) = scoped();
    let (_, mut sub) = f.writer.subscribe(Policy::every_patch(100));
    thread::scope(|s| {
        s.spawn(|| (0..20).for_each(|i| drop(f.writer.commit(Draft::append(f.message, format!("L{i};"))).unwrap())));
        s.spawn(|| {
            for i in 0..20 {
                f.writer.request(&PEER, None, Draft::append(f.message, format!("P{i};")), 0).unwrap();
            }
        });
    });
    assert_eq!(f.seqs(), (4..=43).collect::<Vec<_>>());
    let text = f.text();
    for who in ["L", "P"] {
        let mine: Vec<_> = text.split_terminator(';').filter(|p| p.starts_with(who)).collect();
        assert_eq!(mine, (0..20).map(|i| format!("{who}{i}")).collect::<Vec<_>>());
    }
    let published: String = drain(&mut sub)
        .into_iter()
        .map(|e| match e {
            Event::Patch(p) => match &p.op {
                Op::Append { text } => text.clone(),
                op => panic!("{op:?}"),
            },
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(published, text, "subscribers saw a different order than the DOM");
}

#[test]
fn failed_persist_and_invalid_drafts_change_and_publish_nothing() {
    let failing = MemoryPersist { fail: Some("disk full".into()), ..Default::default() };
    let f = fixture_with(OWNER, failing, Arc::new(AllowAll));
    let (before, mut sub) = f.writer.subscribe(Policy::every_patch(16));
    let err = f.writer.commit(Draft::append(f.message, "lost")).unwrap_err();
    assert_eq!(err, CommitError::Journal("disk full".into()));
    assert_eq!(f.writer.with_dom(Clone::clone), before);
    assert_eq!(sub.try_recv(), Err(TryRecvError::Empty));

    let f = fixture();
    let missing = f.writer.commit(Draft::append(NodeId(99), "x")).unwrap_err();
    assert_eq!(missing, CommitError::Apply(ApplyError::MissingNode(NodeId(99))));
    let bad = f.writer.commit(Draft::set(f.turn, "status", "exploded")).unwrap_err();
    assert!(matches!(bad, CommitError::Apply(ApplyError::Schema(_))), "{bad:?}");
    assert!(f.seqs().is_empty());
    assert_eq!(f.writer.commit(Draft::set(f.turn, "status", "done")).unwrap().seq, 4);
}

#[test]
fn mirrors_replicate_the_owners_patches_in_order_only() {
    let owner = fixture();
    for text in ["a", "b", "c"] {
        owner.writer.commit(Draft::append(owner.message, text)).unwrap();
    }
    let patches = owner.log.patches.lock().unwrap().clone();
    let mirror = fixture_with(PEER, MemoryPersist::default(), Arc::new(AllowAll));
    let (_, sub) = mirror.writer.subscribe(Policy::every_patch(16));
    assert!(!mirror.writer.is_writer());
    assert_eq!(mirror.writer.commit(Draft::append(mirror.message, "x")).unwrap_err(), CommitError::NotWriter);
    let req = mirror.writer.request(&PEER, None, Draft::append(mirror.message, "x"), 0);
    assert_eq!(req.unwrap_err(), RequestError::Commit(CommitError::NotWriter));

    let gap = mirror.writer.replicate(patches[1].clone()).unwrap_err();
    assert_eq!(gap, ReplicateError::Commit(CommitError::Apply(ApplyError::Sequence { expected: 4, got: 5 })));
    patches.iter().for_each(|p| mirror.writer.replicate(p.clone()).unwrap());
    assert_eq!(mirror.writer.with_dom(Clone::clone), owner.writer.with_dom(Clone::clone));
    assert_eq!((mirror.seqs(), sub.pending()), (vec![4, 5, 6], 3));
    assert_eq!(owner.writer.replicate(patches[0].clone()).unwrap_err(), ReplicateError::Owner);
}

#[test]
fn peer_requests_are_scoped_by_grant_and_revision() {
    let (f, auth) = scoped();
    let w = &f.writer;
    let patch = w.request(&PEER, None, Draft::append(f.message, "peer"), 7).unwrap();
    assert_eq!((patch.seq, patch.ts), (4, 7));
    let outside = w.request(&PEER, None, Draft::set(f.session, "title", "renamed"), 8).unwrap_err();
    assert_eq!(outside, RequestError::Denied(Denied("outside the granted turn".into())));
    assert!(matches!(w.request(&OWNER, None, Draft::append(f.message, "x"), 9), Err(RequestError::Denied(_))));

    // A request made against a revision a local commit has since moved past.
    w.commit(Draft::append(f.message, " local")).unwrap();
    let stale = w.request(&PEER, Some(4), Draft::append(f.message, " stale"), 9);
    assert_eq!(stale, Err(RequestError::Conflict { expected: 4, actual: 5 }));
    w.request(&PEER, Some(5), Draft::append(f.message, " fresh"), 9).unwrap();
    assert_eq!(f.text(), "peer local fresh");

    // The grant is checked against the tree at the moment of ordering.
    *auth.0.lock().unwrap() = None;
    assert!(matches!(w.request(&PEER, Some(6), Draft::append(f.message, "x"), 9), Err(RequestError::Denied(_))));
    assert_eq!(f.seqs(), [4, 5, 6]);
    let denied = fixture_with(OWNER, MemoryPersist::default(), Arc::new(DenyAll));
    assert!(denied.writer.request(&PEER, None, Draft::append(denied.message, "x"), 0).is_err());
}

#[test]
fn closing_drains_subscriptions_and_frees_the_id() {
    let bus = Bus::new();
    let f = fixture();
    let config = |f: &Fixture| loa_bus::WriterConfig {
        session: "a".into(),
        owner: OWNER,
        local: OWNER,
        dom: f.writer.with_dom(Clone::clone),
        persist: Box::new(MemoryPersist::default()),
        authorize: Arc::new(AllowAll),
    };
    let writer = bus.open(config(&f)).unwrap();
    assert_eq!(bus.open(config(&f)).err(), Some(BusError::AlreadyOpen("a".into())));
    let (_, mut sub) = writer.subscribe(Policy::every_patch(16));
    let (_, mut lagging) = writer.subscribe(Policy::every_patch(1));
    for text in ["a", "b"] {
        bus.get("a").unwrap().commit(Draft::append(f.message, text)).unwrap();
    }
    assert!(bus.close("a").is_some());
    assert!(bus.get("a").is_none() && bus.ids().is_empty());
    assert_eq!(writer.commit(Draft::append(f.message, "late")).unwrap_err(), CommitError::Closed);

    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let seqs: Vec<_> = rt.block_on(async {
        let mut seqs = Vec::new();
        while let Some(Event::Patch(p)) = sub.recv().await {
            seqs.push(p.seq);
        }
        seqs
    });
    assert_eq!(seqs, [4, 5]);
    assert!(matches!(drain(&mut lagging)[..], [Event::Patch(_), Event::Lagged { resync_from: 5 }]));
    let (_, mut late) = writer.subscribe(Policy::every_patch(16));
    assert_eq!(late.try_recv(), Err(TryRecvError::Closed));
    bus.open(config(&f)).unwrap();
}

#[test]
fn coalescing_merges_adjacent_appends_to_one_node_only() {
    let f = fixture();
    let w = &f.writer;
    let user = loa_core::attrs! { ROLE: loa_core::Role::User };
    let other = w.commit(Draft::insert(f.turn, loa_core::ElementKind::Message, user, "")).unwrap().node;
    let (mut mirror, mut ui) = w.subscribe(Policy::coalescing(8));
    let (_, mut peer) = w.subscribe(Policy::every_patch(1000));
    (0..500).for_each(|i| drop(w.commit(Draft::append(f.message, format!("{i} "))).unwrap()));
    for (node, text) in [(other, "c"), (f.message, "d")] {
        w.commit(Draft::append(node, text)).unwrap();
    }
    w.commit(Draft::set(f.message, "status", "done")).unwrap();
    ["e", "f"].iter().for_each(|t| drop(w.commit(Draft::append(f.message, *t)).unwrap()));

    let events = drain(&mut ui);
    let texts: Vec<_> = events
        .iter()
        .map(|e| match e {
            Event::Appends(a) => a.text.len(),
            Event::Patch(p) => if let Op::Append { text } = &p.op { text.len() } else { 0 },
            Event::Lagged { .. } => panic!("lagged"),
        })
        .collect();
    assert_eq!(texts, [1890, 1, 1, 0, 2]);
    assert!(matches!(&events[0], Event::Appends(a) if (a.first_seq, a.last_seq) == (5, 504)));
    events.into_iter().for_each(|e| assert!(e.apply_to(&mut mirror).unwrap()));
    assert_eq!(mirror, w.with_dom(Clone::clone));
    assert_eq!(drain(&mut peer).len(), 505, "peers get every patch on its own");
}

#[test]
fn subscribing_while_another_thread_commits_leaves_no_gap() {
    let f = fixture();
    let stop = AtomicBool::new(false);
    thread::scope(|s| {
        s.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                f.writer.commit(Draft::append(f.message, "t")).unwrap();
            }
        });
        for _ in 0..20 {
            let (mirror, mut sub) = f.writer.subscribe(Policy::every_patch(1 << 20));
            let (seq, mut by_seq) = f.writer.subscribe_seq(Policy::every_patch(1 << 20));
            let first = |sub: &mut loa_bus::Subscription| loop {
                if let Ok(Event::Patch(p)) = sub.try_recv() {
                    break p.seq;
                }
                thread::yield_now();
            };
            assert_eq!(first(&mut sub), mirror.seq() + 1);
            assert_eq!(first(&mut by_seq), seq + 1);
        }
        stop.store(true, Ordering::Relaxed);
    });
    // A subscriber that never reads did not hold up the committer.
    let (_, _stuck) = f.writer.subscribe(Policy::every_patch(1));
    (0..5).for_each(|_| drop(f.writer.commit(Draft::append(f.message, "x")).unwrap()));
}
