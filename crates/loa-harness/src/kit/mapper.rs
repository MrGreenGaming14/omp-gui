//! The generic session: an [`EventMapper`] turns decoded harness events into
//! DOM writes and replies, [`HarnessOps`] talks to the process, and
//! [`DrivenSession`] implements `HarnessSession` once over both.

use super::ledger::TurnLedger;
use async_trait::async_trait;
use loa_contracts::harness::{AdapterError, Answer, HarnessSession, ModelSelection, Prompt, Result, SessionState};
use loa_core::{DomWriter, NodeId, PatchSink};
use std::sync::Arc;
use tokio::sync::{Mutex, watch};

/// Pure protocol-to-DOM translation of one harness.
pub trait EventMapper: Send + 'static {
    type Event: Send;
    /// What the mapper needs sent back (auto-approvals, decision answers).
    type Reply: Send;

    fn apply(&mut self, ev: Self::Event, w: &DomWriter) -> Result<Vec<Self::Reply>>;
    /// The reply answering `decision`; `None` when the harness needs none.
    fn answer(&mut self, decision: NodeId, answer: &Answer, w: &DomWriter) -> Result<Option<Self::Reply>>;
    fn ledger(&self) -> &TurnLedger<String>;
    fn ledger_mut(&mut self) -> &mut TurnLedger<String>;
}

/// The process side of one session.
#[async_trait]
pub trait HarnessOps: Send + Sync + 'static {
    type Reply: Send;

    fn harness(&self) -> &'static str;
    async fn send(&self, prompt: &Prompt) -> Result<()>;
    async fn reply(&self, reply: Self::Reply) -> Result<()>;
    async fn interrupt(&self) -> Result<()>;
    async fn set_model(&self, _selection: &ModelSelection) -> Result<()> {
        Err(AdapterError::Unsupported { harness: self.harness(), what: "set_model" })
    }
    async fn shutdown(&self) -> Result<()>;
}

/// A [`HarnessSession`] driven by a mapper and the process operations.
pub struct DrivenSession<M: EventMapper, O: HarnessOps<Reply = M::Reply>> {
    mapper: Mutex<M>,
    ops: O,
    sink: Arc<dyn PatchSink>,
    state: watch::Sender<SessionState>,
}

impl<M: EventMapper, O: HarnessOps<Reply = M::Reply>> DrivenSession<M, O> {
    pub fn new(mapper: M, ops: O, sink: Arc<dyn PatchSink>) -> Self {
        let state = watch::Sender::new(mapper.ledger().state());
        Self { mapper: Mutex::new(mapper), ops, sink, state }
    }

    pub fn ops(&self) -> &O {
        &self.ops
    }

    /// Applies one decoded event from the harness and sends the mapper's replies.
    pub async fn feed(&self, ev: M::Event) -> Result<()> {
        let replies = {
            let mut mapper = self.mapper.lock().await;
            let replies = mapper.apply(ev, &DomWriter::new(&*self.sink));
            self.state.send_replace(mapper.ledger().state());
            replies?
        };
        for reply in replies {
            self.ops.reply(reply).await?;
        }
        Ok(())
    }

    /// The process is gone.
    pub async fn closed(&self) {
        self.mapper.lock().await.ledger_mut().close();
        self.state.send_replace(SessionState::Closed);
    }
}

#[async_trait]
impl<M: EventMapper, O: HarnessOps<Reply = M::Reply>> HarnessSession for DrivenSession<M, O> {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        {
            let mut mapper = self.mapper.lock().await;
            match mapper.ledger().state() {
                SessionState::Closed => return Err(AdapterError::Closed),
                SessionState::Idle => {}
                _ => return Err(AdapterError::Busy),
            }
            mapper.ledger_mut().begin(&DomWriter::new(&*self.sink), &prompt)?;
            self.state.send_replace(mapper.ledger().state());
        }
        self.ops.send(&prompt).await
    }

    async fn interrupt(&self) -> Result<()> {
        if self.state() == SessionState::Idle { Ok(()) } else { self.ops.interrupt().await }
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        let reply = {
            let mut mapper = self.mapper.lock().await;
            let reply = mapper.answer(decision, &answer, &DomWriter::new(&*self.sink));
            mapper.ledger_mut().answered(decision);
            self.state.send_replace(mapper.ledger().state());
            reply?
        };
        match reply {
            Some(reply) => self.ops.reply(reply).await,
            None => Ok(()),
        }
    }

    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        self.ops.set_model(&selection).await
    }

    fn harness(&self) -> &'static str {
        self.ops.harness()
    }

    fn state(&self) -> SessionState {
        *self.state.borrow()
    }

    async fn wait_idle(&self) -> Result<()> {
        let mut state = self.state.subscribe();
        let state = state.wait_for(|s| matches!(s, SessionState::Idle | SessionState::Closed)).await;
        match state.map(|s| *s) {
            Ok(SessionState::Idle) => Ok(()),
            _ => Err(AdapterError::Closed),
        }
    }

    async fn shutdown(&self) -> Result<()> {
        let result = self.ops.shutdown().await;
        self.closed().await;
        result
    }
}
