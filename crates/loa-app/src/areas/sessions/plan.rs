//! App plan mode for harnesses without their own: a read-only gate until the user approves a plan.

use loa_util::LockExt;
use std::fmt;
use std::sync::{Arc, Mutex};

use iced::Task;
use loa_bus::SessionId;
use loa_contracts::harness::{Answer, Feature, Prompt};
use loa_core::{Dom, Draft, NodeId, PatchSink, SessionMode, answer, attr};
use loa_session::PlanPermit;
use loa_session::plan::{self as plan_mode, Outcome, Verdict};

use super::{Message, State, asks_first, capabilities, mode};
use crate::area::{Ctx, Request, perform};

/// What follows a plan review.
#[derive(Clone, Debug)]
pub enum PlanNext {
    Execute(String),
    Revise { prompt: String, permit: Held, step: NodeId },
}

/// A plan permit carried by a message.
#[derive(Clone)]
pub struct Held(Arc<Mutex<Option<Box<dyn PlanPermit>>>>);

impl fmt::Debug for Held {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Held")
    }
}

/// A session in app plan mode.
pub(super) struct Plan {
    /// Keeps the read-only gate on.
    permit: Box<dyn PlanPermit>,
    /// The open plan step; `None` while a plan waits for review.
    step: Option<NodeId>,
}

impl State {
    /// Takes a reopened session that is in app plan mode back into it.
    pub(super) fn restore_plan(&mut self, ctx: &mut Ctx) {
        let Some(session) = ctx.runtime.sessions().active() else { return };
        let id = session.id().to_string();
        let harness = session.harness_id().unwrap_or_default();
        if asks_first(ctx, harness) {
            self.gate.enforce_through_approvals(&id);
        }
        if mode(session.dom()) != SessionMode::Plan || self.plans.contains_key(&id) || capabilities(ctx, harness).has(Feature::PlanMode) {
            return;
        }
        let step = plan_mode::open_step(session.dom());
        match self.gate.permit(&id) {
            Ok(permit) => {
                self.plans.insert(id, Plan { permit, step });
            }
            Err(error) => ctx.notify(format!("Plan mode can't be enforced here: {error}")),
        }
    }

    /// Switches the active session to `mode`, typed or picked; the composer's picker follows.
    pub(super) fn pick_mode(&mut self, mode: SessionMode, ctx: &mut Ctx) -> Task<Message> {
        ctx.composer.set_mode(mode);
        let Some(session) = ctx.runtime.sessions().active() else { return Task::none() };
        let id = session.id().to_string();
        let harness_id = session.harness_id().unwrap_or_default().to_owned();
        let writer = Arc::clone(session.writer());
        if capabilities(ctx, &harness_id).has(Feature::PlanMode) {
            // The harness has its own plan mode.
            let Some(harness) = session.harness().cloned() else {
                let current = self::mode(session.dom());
                ctx.notify("The mode changes once the session runs: send a message first");
                ctx.composer.set_mode(current);
                return Task::none();
            };
            return perform(async move { harness.set_mode(mode).await }, Message::Done);
        }
        match mode {
            SessionMode::Plan if !self.plans.contains_key(&id) => {
                if session.is_busy() {
                    ctx.notify("Plan mode starts between turns: wait for this one to end");
                    ctx.composer.set_mode(SessionMode::Normal);
                    return Task::none();
                }
                if asks_first(ctx, &harness_id) {
                    self.gate.enforce_through_approvals(&id);
                }
                // `begin` commits through the writer, so it reads a copy: committing under `with_dom` would deadlock.
                let entered = self.gate.permit(&id).and_then(|permit| Ok((permit, plan_mode::begin(&writer.with_dom(Dom::clone), writer.as_ref())?)));
                match entered {
                    Ok((permit, step)) => {
                        self.plans.insert(id, Plan { permit, step: Some(step) });
                        ctx.notify("Plan mode: tools that change files or run commands are blocked until you approve a plan");
                    }
                    Err(error) => {
                        ctx.notify(format!("Can't enter plan mode on {}: {error}", ctx.runtime.harnesses().label(&harness_id)));
                        ctx.composer.set_mode(SessionMode::Normal);
                    }
                }
            }
            SessionMode::Normal => {
                let root = session.dom().root().map(|root| root.id);
                if self.plans.remove(&id).is_some()
                    && let Some(root) = root
                    && let Err(error) = writer.commit(Draft::set(root, attr::MODE, SessionMode::Normal))
                {
                    ctx.notify(error.to_string());
                }
            }
            SessionMode::Plan => {}
        }
        Task::none()
    }

    /// Whether the active session is in app plan mode.
    pub(super) fn in_plan(&self, ctx: &Ctx) -> bool {
        ctx.runtime.sessions().active().is_some_and(|session| self.plans.contains_key(session.id().as_str()))
    }

    /// At the end of a turn in plan mode, the plan the agent wrote goes up for review.
    pub(super) fn submit_plan(&mut self, ctx: &mut Ctx, session: &SessionId, turn: NodeId) {
        let Some(plan) = self.plans.get_mut(session.as_str()) else { return };
        let (Some(step), Some(open)) = (plan.step, ctx.runtime.sessions().get(session.as_str())) else { return };
        let dom = open.dom();
        let (Some(root), Some(text)) = (dom.root().map(|root| root.id), plan_mode::plan_text(dom, turn)) else { return };
        let submitted = loa_session::submit_plan(open.writer().as_ref(), root, step, text);
        if ctx.report("Can't raise the plan review", submitted).is_some() {
            plan.step = None;
        }
    }

    pub(super) fn answer_plan(&mut self, ctx: &mut Ctx, session: SessionId, decision: NodeId, answer: Answer) -> Task<Message> {
        let Some(open) = ctx.runtime.sessions().get(session.as_str()) else { return Task::none() };
        let Some(harness) = open.harness().cloned() else {
            plan_failed(ctx, &session, decision, "The session isn't running: send a message to resume it, then answer the plan");
            return Task::none();
        };
        let Some(plan) = self.plans.remove(session.as_str()) else {
            plan_failed(ctx, &session, decision, "This session isn't in plan mode anymore");
            return Task::none();
        };
        let reason = answer.reason.clone().unwrap_or_default();
        let verdict = match answer.values.first().map(String::as_str) {
            Some(answer::APPROVE) => Verdict::Approve,
            Some(answer::EDIT) => Verdict::Edit(reason),
            _ => Verdict::Reject(reason),
        };
        let (dom, writer) = (open.dom().clone(), Arc::clone(open.writer()));
        let by = if answer.by.is_empty() { ctx.services.node.key.clone() } else { answer.by };
        let id = session.clone();
        Task::perform(
            async move {
                let outcome = plan_mode::answer_plan(harness.as_ref(), &dom, writer.as_ref(), decision, &by, verdict, plan.permit).await.map_err(|error| error.to_string())?;
                Ok(match outcome {
                    Outcome::Execute { prompt } => PlanNext::Execute(prompt),
                    Outcome::Revise { prompt, permit, step } => PlanNext::Revise { prompt, permit: Held(Arc::new(Mutex::new(Some(permit)))), step },
                })
            },
            move |result| Message::PlanAnswered { session: id.clone(), decision, result },
        )
    }

    /// The plan was answered: execute it, or revise it under a new permit.
    pub(super) fn plan_answered(&mut self, ctx: &mut Ctx, session: SessionId, decision: NodeId, result: Result<PlanNext, String>) {
        let prompt = match result {
            Ok(PlanNext::Execute(prompt)) => prompt,
            Ok(PlanNext::Revise { prompt, permit, step }) => {
                if let Some(permit) = permit.0.locked().take() {
                    self.plans.insert(session.to_string(), Plan { permit, step: Some(step) });
                }
                prompt
            }
            Err(error) => {
                plan_failed(ctx, &session, decision, &format!("Can't answer the plan: {error}"));
                return self.restore_plan(ctx);
            }
        };
        if ctx.runtime.sessions().is_active(session.as_str()) {
            ctx.request(Request::Prompt(Prompt { text: prompt, by: ctx.services.node.key.clone(), ..Prompt::default() }));
        }
    }
}

/// Reopens the plan review card `decision` after its answer failed; the decisions area shows `error`.
fn plan_failed(ctx: &mut Ctx, session: &SessionId, decision: NodeId, error: &str) {
    let reopen = crate::areas::decisions::Message::Answered { session: session.to_string(), decision, result: Err(error.to_owned()) };
    ctx.request(Request::Forward(Box::new(crate::app::Message::area(reopen))));
}
