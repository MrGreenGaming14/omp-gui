//! Session registry and the firehose across sessions.

mod common;

use common::{OWNER, Journal, allow_none, build, config};
use loa_bus::{Bus, BusError, Event, Policy, SessionId, TryRecvError};
use loa_core::{CommitError, Draft, PatchSink};
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

#[test]
fn sessions_open_once_and_close_for_good() {
    let bus = Bus::new();
    let writer = bus.open(config("a", OWNER, &Journal::default(), allow_none())).unwrap();
    assert_eq!(
        bus.open(config("a", OWNER, &Journal::default(), allow_none())).err(),
        Some(BusError::AlreadyOpen(SessionId::from("a")))
    );
    let tree = build(&*bus.get("a").unwrap(), "a");
    assert_eq!(writer.seq(), 3);

    let (_, mut sub) = writer.subscribe(Policy::every_patch(8));
    assert!(bus.close("a").is_some());
    assert!(bus.get("a").is_none());
    assert!(bus.ids().is_empty());
    assert_eq!(writer.commit(Draft::append(tree.message, "x")).unwrap_err(), CommitError::Closed);
    assert_eq!(sub.try_recv(), Err(TryRecvError::Closed));

    // The id is free again, e.g. to reopen the session from its journal.
    bus.open(config("a", OWNER, &Journal::default(), allow_none())).unwrap();
}

#[test]
fn firehose_tags_patches_with_their_session_and_keeps_each_sessions_order() {
    let bus = Bus::new();
    let mut firehose = bus.firehose(Policy::every_patch(10_000));
    let writers: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|id| bus.open(config(id, OWNER, &Journal::default(), allow_none())).unwrap())
        .collect();
    let handles: Vec<_> = writers
        .iter()
        .map(|writer| {
            let writer = Arc::clone(writer);
            thread::spawn(move || {
                let tree = build(&*writer, writer.session().as_str());
                for i in 0..200 {
                    writer.commit(Draft::append(tree.message, i.to_string())).unwrap();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }

    let mut last: HashMap<SessionId, u64> = HashMap::new();
    while let Ok((session, event)) = firehose.try_recv() {
        let Event::Patch(patch) = event else { panic!("unexpected {event:?}") };
        let prev = last.insert(session.clone(), patch.seq).unwrap_or(0);
        assert_eq!(patch.seq, prev + 1, "session {session} out of order");
    }
    assert_eq!(last.len(), 3);
    assert!(last.values().all(|seq| *seq == 203));
}

#[test]
fn slow_firehose_lags_without_holding_up_sessions() {
    let bus = Bus::new();
    let mut firehose = bus.firehose(Policy::every_patch(2));
    let a = bus.open(config("a", OWNER, &Journal::default(), allow_none())).unwrap();
    build(&*a, "a");
    let b = bus.open(config("b", OWNER, &Journal::default(), allow_none())).unwrap();
    build(&*b, "b");

    assert!(matches!(firehose.try_recv(), Ok((s, Event::Patch(p))) if s.as_str() == "a" && p.seq == 1));
    assert!(matches!(firehose.try_recv(), Ok((s, Event::Patch(p))) if s.as_str() == "a" && p.seq == 2));
    assert_eq!(firehose.try_recv(), Ok((SessionId::from("a"), Event::Lagged { resync_from: 3 })));
    assert_eq!(firehose.try_recv(), Err(TryRecvError::Closed));
    assert_eq!((a.seq(), b.seq()), (3, 3));
}

#[test]
fn coalescing_firehose_merges_tokens_per_session_only() {
    let bus = Bus::new();
    let mut firehose = bus.firehose(Policy::coalescing(10));
    let a = bus.open(config("a", OWNER, &Journal::default(), allow_none())).unwrap();
    let b = bus.open(config("b", OWNER, &Journal::default(), allow_none())).unwrap();
    let ta = build(&*a, "a");
    let tb = build(&*b, "b");
    while firehose.try_recv().is_ok() {}

    // Same node id and adjacent sequence numbers in both sessions.
    a.commit(Draft::append(ta.message, "a1")).unwrap();
    b.commit(Draft::append(tb.message, "b1")).unwrap();
    b.commit(Draft::append(tb.message, "b2")).unwrap();
    a.commit(Draft::append(ta.message, "a2")).unwrap();

    let mut got = Vec::new();
    while let Ok((session, event)) = firehose.try_recv() {
        let text = match event {
            Event::Appends(m) => m.text,
            Event::Patch(p) => match &p.op {
                loa_core::Op::Append { text } => text.clone(),
                other => panic!("unexpected {other:?}"),
            },
            Event::Lagged { .. } => panic!("lagged"),
        };
        got.push(format!("{session}:{text}"));
    }
    assert_eq!(got, ["a:a1", "b:b1b2", "a:a2"]);
}

#[test]
fn dropping_the_bus_ends_the_firehose() {
    let bus = Bus::new();
    let mut firehose = bus.firehose(Policy::every_patch(8));
    drop(bus);
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    assert_eq!(runtime.block_on(firehose.recv()), None);
}
