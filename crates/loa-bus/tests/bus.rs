//! The session registry.

mod common;

use common::{OWNER, Journal, allow_none, build, config};
use loa_bus::{Bus, BusError, Policy, SessionId, TryRecvError};
use loa_core::{CommitError, Draft, PatchSink};

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
