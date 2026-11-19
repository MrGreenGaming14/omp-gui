//! Single writer per session (#15).

mod common;

use common::{OWNER, PEER, STRANGER, Journal, Scoped, allow_none, build, config};
use loa_bus::{Denied, Policy, ReplicateError, RequestError, SessionWriter};
use loa_core::{ApplyError, CommitError, Draft, NodeId, PatchSink};
use std::sync::{Arc, Mutex};
use std::thread;

#[test]
fn commits_are_persisted_in_gap_free_order_and_journal_replays_to_the_live_dom() {
    let journal = Journal::default();
    let writer = SessionWriter::new(config("s1", OWNER, &journal, allow_none()));
    let tree = build(&writer, "s1");
    let writer = Arc::new(writer);
    let handles: Vec<_> = (0..4)
        .map(|t| {
            let writer = Arc::clone(&writer);
            thread::spawn(move || {
                for i in 0..50 {
                    writer.commit(Draft::append(tree.message, format!("{t}:{i} "))).unwrap();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(journal.seqs(), (1..=203).collect::<Vec<_>>());
    assert_eq!(writer.seq(), 203);
    writer.with_dom(|dom| assert_eq!(*dom, journal.replay()));
}

#[test]
fn failed_persist_fails_the_commit_and_changes_nothing() {
    let journal = Journal::default();
    let writer = SessionWriter::new(config("s1", OWNER, &journal, allow_none()));
    let tree = build(&writer, "s1");
    let (_, mut sub) = writer.subscribe(Policy::every_patch(16));
    let before = writer.with_dom(Clone::clone);

    *journal.fail_at.lock().unwrap() = Some(4);
    let err = writer.commit(Draft::append(tree.message, "lost")).unwrap_err();
    assert!(matches!(err, CommitError::Journal(_)), "{err:?}");
    writer.with_dom(|dom| assert_eq!(*dom, before));
    assert!(sub.try_recv().is_err(), "a failed commit was published");

    *journal.fail_at.lock().unwrap() = None;
    let patch = writer.commit(Draft::append(tree.message, "kept")).unwrap();
    assert_eq!(patch.seq, 4);
    writer.with_dom(|dom| assert_eq!(dom.get(tree.message).unwrap().text, "kept"));
    assert_eq!(journal.seqs(), vec![1, 2, 3, 4]);
}

#[test]
fn invalid_draft_is_never_persisted() {
    let journal = Journal::default();
    let writer = SessionWriter::new(config("s1", OWNER, &journal, allow_none()));
    let tree = build(&writer, "s1");

    let missing = writer.commit(Draft::append(NodeId(99), "x")).unwrap_err();
    assert_eq!(missing, CommitError::Apply(ApplyError::MissingNode(NodeId(99))));
    let bad_attr = writer.commit(Draft::set(tree.turn, "status", "exploded")).unwrap_err();
    assert!(matches!(bad_attr, CommitError::Apply(ApplyError::Schema(_))), "{bad_attr:?}");
    let second_root = writer
        .commit(Draft::insert(NodeId::DOCUMENT, loa_core::ElementKind::Session, Vec::<(String, _)>::new(), ""))
        .unwrap_err();
    assert!(matches!(second_root, CommitError::Apply(_)), "{second_root:?}");

    assert_eq!(journal.seqs(), vec![1, 2, 3]);
    assert_eq!(writer.commit(Draft::set(tree.turn, "status", "done")).unwrap().seq, 4);
}

#[test]
fn a_node_that_does_not_own_the_session_cannot_write_it() {
    let journal = Journal::default();
    let mirror = SessionWriter::new(config("s1", PEER, &journal, Arc::new(Scoped { scope: Mutex::new(Some(NodeId(2))) })));
    assert!(!mirror.is_writer());

    let draft = Draft::insert(NodeId::DOCUMENT, loa_core::ElementKind::Session, Vec::<(String, _)>::new(), "");
    assert_eq!(mirror.commit(draft.clone()).unwrap_err(), CommitError::NotWriter);
    assert_eq!(mirror.request(&PEER, draft, 0).unwrap_err(), RequestError::Commit(CommitError::NotWriter));
    assert!(journal.seqs().is_empty());
}

#[test]
fn mirror_applies_the_owners_patches_in_order_only() {
    let owner_journal = Journal::default();
    let owner = SessionWriter::new(config("s1", OWNER, &owner_journal, allow_none()));
    build(&owner, "s1");
    let patches = owner_journal.patches.lock().unwrap().clone();

    let mirror_journal = Journal::default();
    let mirror = SessionWriter::new(config("s1", PEER, &mirror_journal, allow_none()));
    let (_, sub) = mirror.subscribe(Policy::every_patch(16));

    let gap = mirror.replicate(patches[1].clone()).unwrap_err();
    assert_eq!(gap, ReplicateError::Commit(CommitError::Apply(ApplyError::Sequence { expected: 1, got: 2 })));
    for patch in &patches {
        mirror.replicate(patch.clone()).unwrap();
    }
    assert_eq!(mirror.with_dom(Clone::clone), owner.with_dom(Clone::clone));
    assert_eq!(mirror_journal.seqs(), vec![1, 2, 3]);
    assert_eq!(sub.pending(), 3);

    assert_eq!(owner.replicate(patches[0].clone()).unwrap_err(), ReplicateError::Owner);
}

#[test]
fn peer_requests_pass_authorization_against_the_current_dom() {
    let journal = Journal::default();
    let auth = Arc::new(Scoped { scope: Mutex::new(None) });
    let writer = SessionWriter::new(config("s1", OWNER, &journal, auth.clone()));
    let tree = build(&writer, "s1");
    *auth.scope.lock().unwrap() = Some(tree.turn);

    let patch = writer.request(&PEER, Draft::append(tree.message, "from peer"), 7).unwrap();
    assert_eq!((patch.seq, patch.ts), (4, 7));

    let outside = writer.request(&PEER, Draft::set(tree.session, "title", "renamed"), 8).unwrap_err();
    assert_eq!(outside, RequestError::Denied(Denied("outside the granted turn".into())));
    let stranger = writer.request(&STRANGER, Draft::append(tree.message, "x"), 9).unwrap_err();
    assert!(matches!(stranger, RequestError::Denied(_)));

    assert_eq!(journal.seqs(), vec![1, 2, 3, 4]);
    writer.with_dom(|dom| {
        assert_eq!(dom.get(tree.message).unwrap().text, "from peer");
        assert!(dom.get(tree.session).unwrap().attr("title").is_none());
    });
}

#[test]
fn peer_request_at_stale_revision_cannot_overwrite_a_local_commit() {
    let journal = Journal::default();
    let auth = Arc::new(Scoped { scope: Mutex::new(None) });
    let writer = SessionWriter::new(config("s1", OWNER, &journal, auth.clone()));
    let tree = build(&writer, "s1");
    *auth.scope.lock().unwrap() = Some(tree.turn);
    let observed = writer.seq();

    writer.commit(Draft::append(tree.message, "local")).unwrap();
    assert_eq!(
        writer.request_if_seq(&PEER, observed, Draft::append(tree.message, "stale"), 7),
        Err(RequestError::Conflict { expected: observed, actual: observed + 1 })
    );
    assert_eq!(writer.seq(), observed + 1);
    writer.with_dom(|dom| assert_eq!(dom.get(tree.message).unwrap().text, "local"));
    assert_eq!(journal.seqs(), (1..=observed + 1).collect::<Vec<_>>());

    let patch = writer.request_if_seq(&PEER, writer.seq(), Draft::append(tree.message, " peer"), 8).unwrap();
    assert_eq!(patch.seq, observed + 2);
    writer.with_dom(|dom| assert_eq!(dom.get(tree.message).unwrap().text, "local peer"));
}

#[test]
fn local_commits_and_peer_requests_share_one_order_and_keep_each_callers_order() {
    let journal = Journal::default();
    let auth = Arc::new(Scoped { scope: Mutex::new(None) });
    let writer = SessionWriter::new(config("s1", OWNER, &journal, auth.clone()));
    let tree = build(&writer, "s1");
    *auth.scope.lock().unwrap() = Some(tree.turn);
    let writer = Arc::new(writer);
    let (_, mut sub) = writer.subscribe(Policy::every_patch(1000));

    let local = {
        let writer = Arc::clone(&writer);
        thread::spawn(move || {
            for i in 0..100 {
                writer.commit(Draft::append(tree.message, format!("L{i};"))).unwrap();
            }
        })
    };
    let peer = {
        let writer = Arc::clone(&writer);
        thread::spawn(move || {
            for i in 0..100 {
                writer.request(&PEER, Draft::append(tree.message, format!("P{i};")), 0).unwrap();
            }
        })
    };
    local.join().unwrap();
    peer.join().unwrap();

    let text = writer.with_dom(|dom| dom.get(tree.message).unwrap().text.clone());
    let pieces: Vec<&str> = text.split_terminator(';').collect();
    let locals: Vec<String> = pieces.iter().filter(|p| p.starts_with('L')).map(|p| p.to_string()).collect();
    let peers: Vec<String> = pieces.iter().filter(|p| p.starts_with('P')).map(|p| p.to_string()).collect();
    assert_eq!(locals, (0..100).map(|i| format!("L{i}")).collect::<Vec<_>>());
    assert_eq!(peers, (0..100).map(|i| format!("P{i}")).collect::<Vec<_>>());

    let mut published = String::new();
    while let Ok(event) = sub.try_recv() {
        let loa_bus::Event::Patch(patch) = event else { panic!("unexpected {event:?}") };
        let loa_core::Op::Append { text } = &patch.op else { panic!("unexpected {patch:?}") };
        published.push_str(text);
    }
    assert_eq!(published, text, "subscribers saw a different order than the DOM");
    assert_eq!(journal.seqs(), (1..=203).collect::<Vec<_>>());
}

#[test]
fn closed_writer_refuses_commits_and_ends_subscriptions_after_draining() {
    let journal = Journal::default();
    let writer = SessionWriter::new(config("s1", OWNER, &journal, allow_none()));
    let (_, mut sub) = writer.subscribe(Policy::every_patch(16));
    let tree = build(&writer, "s1");
    writer.close();

    assert_eq!(writer.commit(Draft::append(tree.message, "late")).unwrap_err(), CommitError::Closed);
    for seq in 1..=3 {
        match sub.try_recv() {
            Ok(loa_bus::Event::Patch(p)) => assert_eq!(p.seq, seq),
            other => panic!("expected patch {seq}, got {other:?}"),
        }
    }
    assert_eq!(sub.try_recv().unwrap_err(), loa_bus::TryRecvError::Closed);
    let (_, mut late) = writer.subscribe(Policy::every_patch(16));
    assert_eq!(late.try_recv().unwrap_err(), loa_bus::TryRecvError::Closed);
}
