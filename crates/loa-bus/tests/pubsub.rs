//! Publish subscribe with per subscriber backpressure (#14).

mod common;

use common::{OWNER, Journal, allow_none, build, config};
use loa_bus::{Event, Policy, SessionWriter, TryRecvError};
use loa_core::{Draft, Op, PatchSink};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

fn writer() -> (SessionWriter, common::Tree) {
    let writer = SessionWriter::new(config("s1", OWNER, &Journal::default(), allow_none()));
    let tree = build(&writer, "s1");
    (writer, tree)
}

#[test]
fn subscribing_during_commits_leaves_no_gap_between_dom_and_patches() {
    let (writer, tree) = writer();
    let writer = Arc::new(writer);
    let stop = Arc::new(AtomicBool::new(false));
    let committer = {
        let (writer, stop) = (Arc::clone(&writer), Arc::clone(&stop));
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                writer.commit(Draft::append(tree.message, "t")).unwrap();
            }
        })
    };
    for _ in 0..200 {
        let (mut mirror, mut sub) = writer.subscribe(Policy::every_patch(1 << 20));
        let (seq, mut sub_seq) = writer.subscribe_seq(Policy::every_patch(1 << 20));
        // Wait until the subscription has seen something, then check the
        // first patch continues the snapshot exactly.
        let first = loop {
            if let Ok(event) = sub.try_recv() {
                break event;
            }
            thread::yield_now();
        };
        let Event::Patch(patch) = &first else { panic!("unexpected {first:?}") };
        assert_eq!(patch.seq, mirror.seq() + 1);
        first.apply_to(&mut mirror).unwrap();
        let first_seq = loop {
            if let Ok(Event::Patch(p)) = sub_seq.try_recv() {
                break p.seq;
            }
            thread::yield_now();
        };
        assert_eq!(first_seq, seq + 1);
    }
    stop.store(true, Ordering::Relaxed);
    committer.join().unwrap();
}

#[test]
fn a_slow_subscriber_lags_without_slowing_the_writer_or_other_subscribers() {
    let (writer, tree) = writer();
    // Never read until the end.
    let (_, mut stuck) = writer.subscribe(Policy::every_patch(8));
    // Room for the whole run, so its pace relative to the writer can't lag it.
    let (mut mirror, mut reader) = writer.subscribe(Policy::every_patch(8192));
    let writer = Arc::new(writer);

    let committer = {
        let writer = Arc::clone(&writer);
        thread::spawn(move || {
            for i in 0..5000 {
                writer.commit(Draft::append(tree.message, format!("{i},"))).unwrap();
            }
            writer.close();
        })
    };
    // The live reader keeps up and sees every patch, in order.
    let mut seen = 0;
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    runtime.block_on(async {
        while let Some(event) = reader.recv().await {
            assert!(event.apply_to(&mut mirror).unwrap(), "the live reader lagged");
            seen += 1;
        }
    });
    committer.join().unwrap();
    assert_eq!(seen, 5000);
    assert_eq!(mirror, writer.with_dom(Clone::clone));

    // The stuck one holds what fit, then a notice telling it where to resync.
    for seq in 4..12 {
        match stuck.try_recv() {
            Ok(Event::Patch(p)) => assert_eq!(p.seq, seq),
            other => panic!("expected patch {seq}, got {other:?}"),
        }
    }
    assert_eq!(stuck.try_recv(), Ok(Event::Lagged { resync_from: 12 }));
    assert_eq!(stuck.try_recv(), Err(TryRecvError::Closed));
}

#[test]
fn a_subscriber_that_never_reads_does_not_block_a_commit() {
    let (writer, tree) = writer();
    let (_, _stuck) = writer.subscribe(Policy::every_patch(1));
    // Commits on this thread would deadlock here if the writer waited for room.
    for _ in 0..10_000 {
        writer.commit(Draft::append(tree.message, "x")).unwrap();
    }
    assert_eq!(writer.seq(), 10_003);
}

#[test]
fn lagged_subscriber_resyncs_by_resubscribing() {
    let (writer, tree) = writer();
    let (mut mirror, mut sub) = writer.subscribe(Policy::every_patch(2));
    for i in 0..10 {
        writer.commit(Draft::append(tree.message, i.to_string())).unwrap();
    }
    let resync_from = loop {
        match sub.try_recv().unwrap() {
            Event::Lagged { resync_from } => break resync_from,
            event => {
                event.apply_to(&mut mirror).unwrap();
            }
        }
    };
    assert_eq!(resync_from, mirror.seq() + 1);
    let (fresh, mut sub) = writer.subscribe(Policy::every_patch(2));
    assert_eq!(fresh.get(tree.message).unwrap().text, "0123456789");
    writer.commit(Draft::append(tree.message, "!")).unwrap();
    assert!(matches!(sub.try_recv(), Ok(Event::Patch(p)) if p.seq == fresh.seq() + 1));
}

#[test]
fn ui_subscriber_gets_streamed_tokens_merged_and_rebuilds_the_same_dom() {
    let (writer, tree) = writer();
    let (mut ui_dom, mut ui) = writer.subscribe(Policy::coalescing(3));
    let (_, mut peer) = writer.subscribe(Policy::every_patch(10_000));

    for i in 0..1000 {
        writer.commit(Draft::append(tree.message, format!("{i} "))).unwrap();
    }
    writer.commit(Draft::set(tree.message, "status", "done")).unwrap();
    writer.commit(Draft::append(tree.message, "tail")).unwrap();

    // 1000 tokens merged into one event, so a queue of 3 never lagged.
    let mut events = Vec::new();
    while let Ok(event) = ui.try_recv() {
        events.push(event);
    }
    assert_eq!(events.len(), 3, "{events:?}");
    match &events[0] {
        Event::Appends(a) => assert_eq!((a.node, a.first_seq, a.last_seq), (tree.message, 4, 1003)),
        other => panic!("expected merged appends, got {other:?}"),
    }
    assert!(matches!(&events[1], Event::Patch(p) if matches!(p.op, Op::Set { .. })));
    assert!(matches!(&events[2], Event::Patch(p) if p.seq == 1005));
    for event in events {
        assert!(event.apply_to(&mut ui_dom).unwrap());
    }
    assert_eq!(ui_dom, writer.with_dom(Clone::clone));

    // Peers get every append on its own.
    let mut count = 0;
    while let Ok(Event::Patch(_)) = peer.try_recv() {
        count += 1;
    }
    assert_eq!(count, 1002);
}

#[test]
fn coalescing_never_merges_across_other_nodes_or_patches() {
    let (writer, tree) = writer();
    let other = writer
        .commit(Draft::insert(tree.turn, loa_core::ElementKind::Message, [("role", loa_core::Value::from("assistant"))], ""))
        .unwrap()
        .node;
    let (mut mirror, mut ui) = writer.subscribe(Policy::coalescing(100));
    writer.commit(Draft::append(tree.message, "a")).unwrap();
    writer.commit(Draft::append(tree.message, "b")).unwrap();
    writer.commit(Draft::append(other, "c")).unwrap();
    writer.commit(Draft::append(tree.message, "d")).unwrap();
    writer.commit(Draft::set(tree.message, "status", "done")).unwrap();
    writer.commit(Draft::append(tree.message, "e")).unwrap();
    writer.commit(Draft::append(tree.message, "f")).unwrap();

    let mut texts = Vec::new();
    while let Ok(event) = ui.try_recv() {
        texts.push(match &event {
            Event::Appends(a) => a.text.clone(),
            Event::Patch(p) => match &p.op {
                Op::Append { text } => text.clone(),
                _ => "<set>".into(),
            },
            Event::Lagged { .. } => panic!("lagged"),
        });
        event.apply_to(&mut mirror).unwrap();
    }
    assert_eq!(texts, ["ab", "c", "d", "<set>", "ef"]);
    assert_eq!(mirror, writer.with_dom(Clone::clone));
}

#[test]
fn async_receiver_wakes_for_commits_from_another_thread() {
    let (writer, tree) = writer();
    let writer = Arc::new(writer);
    let (mut mirror, mut sub) = writer.subscribe(Policy::every_patch(4));
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).build().unwrap();
    let got = runtime.block_on(async {
        let task = tokio::spawn(async move {
            let mut n = 0;
            while let Some(event) = sub.recv().await {
                event.apply_to(&mut mirror).unwrap();
                n += 1;
                if n == 3 {
                    break;
                }
            }
            mirror
        });
        let committer = Arc::clone(&writer);
        thread::spawn(move || {
            for token in ["a", "b", "c"] {
                committer.commit(Draft::append(tree.message, token)).unwrap();
            }
        });
        task.await.unwrap()
    });
    assert_eq!(got.get(tree.message).unwrap().text, "abc");
}
