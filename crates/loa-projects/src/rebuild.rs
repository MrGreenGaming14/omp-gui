//! Rebuilds missing or stale digests, debounced per session so a burst of turns costs one
//! rebuild. A failed rebuild keeps the old digest, flagged stale.

use crate::digest::{digest_state, mark_stale_draft, write_drafts};
use loa_settings::projects::DigestState;
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
pub enum Decision {
    Rebuild,
    UpToDate,
    /// No turns yet.
    Empty,
    /// Excluded sessions get no digest.
    Excluded,
}

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

/// Runs the rebuild policy for many sessions, one rebuild at a time.
pub struct RebuildDriver {
    pub source: Arc<dyn SessionSource>,
    pub writer: Arc<dyn SessionWriter>,
    pub digester: Digester,
    pub debounce: Duration,
    /// Supplies each project's `digest_model`.
    pub settings: Option<Arc<dyn SettingsStore>>,
}

impl RebuildDriver {
    /// Runs on the current runtime until every sender is dropped and pending sessions are handled.
    /// Send a session id to queue its rebuild.
    pub fn spawn(self) -> (mpsc::UnboundedSender<String>, mpsc::UnboundedReceiver<RebuildOutcome>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        tokio::spawn(self.run(rx, out_tx));
        (tx, out_rx)
    }

    async fn run(self, mut rx: mpsc::UnboundedReceiver<String>, out: mpsc::UnboundedSender<RebuildOutcome>) {
        let mut due = KeyedDebouncer::new(self.debounce);
        let mut open = true;
        while open || !due.is_empty() {
            // Once the last sender is gone, pending sessions run right away.
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
