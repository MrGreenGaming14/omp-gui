//! Rebuilds stale digests at turn end and compaction.
//!
//! [`decide`] is the policy: rebuild when the session's digest is missing or
//! stale, it has turns, and it isn't excluded from project context.
//! [`RebuildDriver`] applies it: each trigger pushes the session's deadline
//! out by the debounce window, so a burst of turns produces one rebuild once
//! the session goes quiet. A failed rebuild keeps the old digest and flags it
//! stale.

use crate::digest::{DigestState, digest_state, mark_stale_draft, write_drafts};
use crate::digester::Digester;
use crate::settings::is_excluded;
use crate::source::SessionSource;
use crate::{Error, Result};
use loa_core::{Dom, Draft, ElementKind, Patch};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// What happened in a session that may call for a new digest.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Trigger {
    TurnEnd,
    Compaction,
}

/// What to do about a session's digest.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Rebuild,
    /// The digest matches the DOM.
    UpToDate,
    /// No turns yet.
    Empty,
    /// The session is excluded from project context, so no digest is spent on it.
    Excluded,
}

/// The rebuild policy for a session in its current state.
pub fn decide(dom: &Dom) -> Decision {
    let Some(root) = dom.root() else { return Decision::Empty };
    if is_excluded(dom) {
        return Decision::Excluded;
    }
    if dom.children_of_kind(root.id, ElementKind::Turn).next().is_none() {
        return Decision::Empty;
    }
    match digest_state(dom) {
        DigestState::Fresh => Decision::UpToDate,
        DigestState::Missing | DigestState::Stale => Decision::Rebuild,
    }
}

/// Commits drafts to a session. `loa-app` backs it with the session's
/// `PatchSink` (through `loa-bus`).
pub trait SessionWriter: Send + Sync {
    fn commit(&self, session: &str, draft: Draft) -> Result<Patch>;
}

/// Reported once per handled session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RebuildOutcome {
    /// A new digest was written, built from `source_seq`.
    Rebuilt { session: String, source_seq: u64 },
    /// Nothing to do; carries the reason.
    Skipped { session: String, decision: Decision },
    /// The rebuild failed. The old digest, if any, is now flagged stale.
    Failed { session: String, error: String },
}

/// Sends triggers to a running [`RebuildDriver`].
#[derive(Clone, Debug)]
pub struct RebuildHandle {
    tx: mpsc::UnboundedSender<(String, Trigger)>,
}

impl RebuildHandle {
    /// Reports a turn end or compaction in `session`. `false` once the driver stopped.
    pub fn notify(&self, session: &str, trigger: Trigger) -> bool {
        self.tx.send((session.to_owned(), trigger)).is_ok()
    }
}

/// Runs the rebuild policy for many sessions, one rebuild at a time.
pub struct RebuildDriver {
    source: Arc<dyn SessionSource>,
    writer: Arc<dyn SessionWriter>,
    digester: Digester,
    debounce: Duration,
}

impl RebuildDriver {
    pub fn new(
        source: Arc<dyn SessionSource>,
        writer: Arc<dyn SessionWriter>,
        digester: Digester,
        debounce: Duration,
    ) -> Self {
        Self { source, writer, digester, debounce }
    }

    /// Starts the driver on the current tokio runtime. It stops when every
    /// handle is dropped, after handling the sessions still pending.
    pub fn spawn(self) -> (RebuildHandle, mpsc::UnboundedReceiver<RebuildOutcome>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        tokio::spawn(self.run(rx, out_tx));
        (RebuildHandle { tx }, out_rx)
    }

    async fn run(self, mut rx: mpsc::UnboundedReceiver<(String, Trigger)>, out: mpsc::UnboundedSender<RebuildOutcome>) {
        let mut due: HashMap<String, Instant> = HashMap::new();
        let mut open = true;
        while open || !due.is_empty() {
            // After the last handle is dropped, pending sessions run right away.
            let wake = if open { due.values().min().copied() } else { Some(Instant::now()) };
            tokio::select! {
                message = rx.recv(), if open => match message {
                    Some((session, _)) => {
                        due.insert(session, Instant::now() + self.debounce);
                    }
                    None => open = false,
                },
                () = tokio::time::sleep_until(wake.unwrap_or_else(Instant::now)), if wake.is_some() => {
                    let now = Instant::now();
                    let mut ready: Vec<String> =
                        due.iter().filter(|(_, at)| !open || **at <= now).map(|(s, _)| s.clone()).collect();
                    ready.sort();
                    for session in ready {
                        due.remove(&session);
                        // The receiver may be gone; the rebuild still counts.
                        let _ = out.send(self.handle(session).await);
                    }
                }
            }
        }
    }

    async fn handle(&self, session: String) -> RebuildOutcome {
        let dom = match self.source.load(&session).await {
            Ok(Some(dom)) => dom,
            Ok(None) => return RebuildOutcome::Failed { error: Error::SessionNotFound(session.clone()).to_string(), session },
            Err(e) => return RebuildOutcome::Failed { session, error: e.to_string() },
        };
        match decide(&dom) {
            Decision::Rebuild => {}
            decision => return RebuildOutcome::Skipped { session, decision },
        }
        match self.rebuild(&session, &dom).await {
            Ok(source_seq) => RebuildOutcome::Rebuilt { session, source_seq },
            Err(error) => {
                let mut error = error.to_string();
                if let Some(draft) = mark_stale_draft(&dom)
                    && let Err(e) = self.writer.commit(&session, draft)
                {
                    error = format!("{error}; flagging the old digest stale failed: {e}");
                }
                tracing::warn!(%session, %error, "digest rebuild failed");
                RebuildOutcome::Failed { session, error }
            }
        }
    }

    async fn rebuild(&self, session: &str, dom: &Dom) -> Result<u64> {
        let built = self.digester.digest(dom).await?;
        for draft in write_drafts(dom, &built.content, built.source_seq, built.model.as_deref(), loa_core::now_ms()) {
            self.writer.commit(session, draft)?;
        }
        Ok(built.source_seq)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::digest_of;
    use crate::digester::{PromptLimits, Summarizer, SummaryAnswer};
    use crate::source::SessionInfo;
    use crate::source::testing::{Builder, info_of};
    use async_trait::async_trait;
    use loa_core::{MemorySink, PatchSink, attr};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Sessions that can be read and written, like the app's live sessions.
    #[derive(Default)]
    struct Live {
        sinks: Mutex<HashMap<String, Arc<MemorySink>>>,
    }

    impl Live {
        fn add(&self, b: Builder) -> Arc<MemorySink> {
            let sink = Arc::new(b.sink);
            let id = info_of(&sink.dom()).id;
            self.sinks.lock().unwrap().insert(id, sink.clone());
            sink
        }
    }

    #[async_trait]
    impl SessionSource for Live {
        async fn list(&self, _: &str) -> Result<Vec<SessionInfo>> {
            Ok(self.sinks.lock().unwrap().values().map(|s| info_of(&s.dom())).collect())
        }
        async fn load(&self, session: &str) -> Result<Option<Arc<Dom>>> {
            Ok(self.sinks.lock().unwrap().get(session).map(|s| Arc::new(s.dom())))
        }
    }

    impl SessionWriter for Live {
        fn commit(&self, session: &str, draft: Draft) -> Result<Patch> {
            let sink = self.sinks.lock().unwrap().get(session).cloned().unwrap();
            Ok(sink.commit(draft)?)
        }
    }

    struct Fake {
        calls: AtomicUsize,
        fail: bool,
    }

    #[async_trait]
    impl Summarizer for Fake {
        async fn summarize(&self, _: &str) -> Result<SummaryAnswer> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail {
                return Err(Error::Summarizer("model unavailable".into()));
            }
            let text = format!(r#"{{"summary":"digest {n}","decisions":[],"files":[],"open_items":[],"pointers":["T1"]}}"#);
            Ok(SummaryAnswer { text, model: Some("smol".into()) })
        }
    }

    fn driver(live: &Arc<Live>, fake: &Arc<Fake>) -> (RebuildHandle, mpsc::UnboundedReceiver<RebuildOutcome>) {
        let digester = Digester::new(fake.clone(), PromptLimits::default());
        RebuildDriver::new(live.clone(), live.clone(), digester, Duration::from_secs(2)).spawn()
    }

    #[test]
    fn policy_rebuilds_missing_and_stale_only() {
        let b = Builder::new("s1", "p", 1);
        assert_eq!(decide(&b.dom()), Decision::Empty);
        b.turn("hi", "hello", 2);
        assert_eq!(decide(&b.dom()), Decision::Rebuild);
        let content = crate::DigestContent { summary: "s".into(), ..Default::default() };
        for d in write_drafts(&b.dom(), &content, b.dom().seq(), None, 0) {
            b.commit(d);
        }
        assert_eq!(decide(&b.dom()), Decision::UpToDate);
        b.turn("again", "ok", 3);
        assert_eq!(decide(&b.dom()), Decision::Rebuild);
        b.commit(Draft::set(b.root, attr::EXCLUDED, true));
        assert_eq!(decide(&b.dom()), Decision::Excluded);
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_of_turns_produces_one_rebuild() {
        let live = Arc::new(Live::default());
        let b = Builder::new("s1", "p", 1);
        b.turn("hi", "hello", 2);
        let sink = live.add(b);
        let fake = Arc::new(Fake { calls: AtomicUsize::new(0), fail: false });
        let (handle, mut outcomes) = driver(&live, &fake);
        for _ in 0..3 {
            handle.notify("s1", Trigger::TurnEnd);
            tokio::time::advance(Duration::from_millis(500)).await;
        }
        handle.notify("s1", Trigger::Compaction);
        let outcome = outcomes.recv().await.unwrap();
        assert_eq!(outcome, RebuildOutcome::Rebuilt { session: "s1".into(), source_seq: 4 });
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        let digest = digest_of(&sink.dom()).unwrap();
        assert_eq!(digest.content.summary, "digest 1");
        assert!(!digest.stale);

        // Nothing changed since: the next trigger doesn't call the model.
        handle.notify("s1", Trigger::TurnEnd);
        let outcome = outcomes.recv().await.unwrap();
        assert_eq!(outcome, RebuildOutcome::Skipped { session: "s1".into(), decision: Decision::UpToDate });
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_rebuild_keeps_the_old_digest_flagged_stale() {
        let live = Arc::new(Live::default());
        let b = Builder::new("s1", "p", 1);
        b.turn("hi", "hello", 2);
        let old = crate::DigestContent { summary: "old digest".into(), ..Default::default() };
        for d in write_drafts(&b.dom(), &old, b.dom().seq(), None, 0) {
            b.commit(d);
        }
        b.turn("more", "work", 3);
        let sink = live.add(b);
        let fake = Arc::new(Fake { calls: AtomicUsize::new(0), fail: true });
        let (handle, mut outcomes) = driver(&live, &fake);
        handle.notify("s1", Trigger::TurnEnd);
        assert!(matches!(outcomes.recv().await.unwrap(), RebuildOutcome::Failed { .. }));
        let digest = digest_of(&sink.dom()).unwrap();
        assert_eq!(digest.content.summary, "old digest");
        assert!(digest.stale);
        assert_eq!(sink.dom().get(digest.node).unwrap().bool_attr(attr::STALE), Some(true));
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_handle_flushes_pending_sessions() {
        let live = Arc::new(Live::default());
        let b = Builder::new("s1", "p", 1);
        b.turn("hi", "hello", 2);
        let sink = live.add(b);
        let fake = Arc::new(Fake { calls: AtomicUsize::new(0), fail: false });
        let (handle, mut outcomes) = driver(&live, &fake);
        handle.notify("s1", Trigger::TurnEnd);
        drop(handle);
        assert!(matches!(outcomes.recv().await.unwrap(), RebuildOutcome::Rebuilt { .. }));
        assert!(outcomes.recv().await.is_none());
        assert!(digest_of(&sink.dom()).is_some());
    }
}
