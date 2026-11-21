//! Rebuilds missing or stale digests at turn end and compaction. Triggers are debounced
//! per session, so a burst of turns costs one rebuild once the session goes quiet. A
//! failed rebuild keeps the old digest, flagged stale.

use crate::digest::{DigestState, digest_state, mark_stale_draft, write_drafts};
use crate::digester::Digester;
use crate::settings::{SettingsStore, is_excluded};
use crate::source::{SessionSource, session_project};
use crate::{Error, Result};
use loa_core::{Dom, Draft, ElementKind, Patch};
use loa_util::async_util::KeyedDebouncer;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Trigger {
    TurnEnd,
    Compaction,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Rebuild,
    UpToDate,
    /// No turns yet.
    Empty,
    /// Excluded sessions get no digest.
    Excluded,
}

/// The rebuild policy for a session in its current state.
pub fn decide(dom: &Dom) -> Decision {
    let Some(root) = dom.root() else { return Decision::Empty };
    if is_excluded(dom) {
        Decision::Excluded
    } else if dom.children_of_kind(root.id, ElementKind::Turn).next().is_none() {
        Decision::Empty
    } else if digest_state(dom) == DigestState::Fresh {
        Decision::UpToDate
    } else {
        Decision::Rebuild
    }
}

/// Commits drafts to a session.
pub trait SessionWriter: Send + Sync {
    fn commit(&self, session: &str, draft: Draft) -> Result<Patch>;
}

/// Reported once per handled session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RebuildOutcome {
    Rebuilt { session: String, source_seq: u64 },
    Skipped { session: String, decision: Decision },
    /// The old digest, if any, is now flagged stale.
    Failed { session: String, error: String },
}

/// Sends triggers to a running [`RebuildDriver`].
#[derive(Clone, Debug)]
pub struct RebuildHandle {
    tx: mpsc::UnboundedSender<String>,
}

impl RebuildHandle {
    /// `false` once the driver stopped.
    pub fn notify(&self, session: &str, _: Trigger) -> bool {
        self.tx.send(session.to_owned()).is_ok()
    }
}

/// Runs the rebuild policy for many sessions, one rebuild at a time.
pub struct RebuildDriver {
    source: Arc<dyn SessionSource>,
    writer: Arc<dyn SessionWriter>,
    digester: Digester,
    debounce: Duration,
    settings: Option<Arc<dyn SettingsStore>>,
}

impl RebuildDriver {
    pub fn new(source: Arc<dyn SessionSource>, writer: Arc<dyn SessionWriter>, digester: Digester, debounce: Duration) -> Self {
        Self { source, writer, digester, debounce, settings: None }
    }

    /// Digests each session with its project's `digest_model`, when set.
    pub fn with_settings(mut self, settings: Arc<dyn SettingsStore>) -> Self {
        self.settings = Some(settings);
        self
    }

    /// Runs on the current tokio runtime until every handle is dropped and pending sessions are handled.
    pub fn spawn(self) -> (RebuildHandle, mpsc::UnboundedReceiver<RebuildOutcome>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        tokio::spawn(self.run(rx, out_tx));
        (RebuildHandle { tx }, out_rx)
    }

    async fn run(self, mut rx: mpsc::UnboundedReceiver<String>, out: mpsc::UnboundedSender<RebuildOutcome>) {
        let mut due = KeyedDebouncer::new(self.debounce);
        let mut open = true;
        while open || !due.is_empty() {
            // Once the last handle is gone, pending sessions run right away.
            let wake = if open { due.next() } else { Some(Instant::now()) };
            tokio::select! {
                message = rx.recv(), if open => match message {
                    Some(session) => due.touch(session),
                    None => open = false,
                },
                () = tokio::time::sleep_until(wake.unwrap_or_else(Instant::now)), if wake.is_some() => {
                    for session in due.take_due(Instant::now(), !open) {
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
                if let Some(Err(e)) = mark_stale_draft(&dom).map(|draft| self.writer.commit(&session, draft)) {
                    error = format!("{error}; flagging the old digest stale failed: {e}");
                }
                tracing::warn!(%session, %error, "digest rebuild failed");
                RebuildOutcome::Failed { session, error }
            }
        }
    }

    async fn rebuild(&self, session: &str, dom: &Dom) -> Result<u64> {
        let model = match (&self.settings, session_project(dom)) {
            (Some(settings), Some(project)) => settings.load(&project).await?.digest_model,
            _ => None,
        };
        let built = self.digester.digest(dom, model.as_deref()).await?;
        for draft in write_drafts(dom, &built.content, built.source_seq, built.model.as_deref(), loa_core::now_ms()) {
            self.writer.commit(session, draft)?;
        }
        Ok(built.source_seq)
    }
}
