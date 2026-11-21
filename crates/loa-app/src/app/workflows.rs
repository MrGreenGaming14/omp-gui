//! The composer's workflow picker: omp's model roles by their workflow
//! names. The workflows area lists them; this keeps the picker in step with
//! the session and routes a pick: to the running session's role (what `/role`
//! does), to the next start through the models area's pending model, or,
//! mid-turn, to the end of the turn.

use loa_core::attr;

use super::*;
use crate::areas::workflows::{OMP_ONLY, State as Workflows};

impl App {
    /// Shows the workflows in the composer: every workflow with the active
    /// one marked, dimmed unless the composer's harness is omp. Runs after
    /// every update, so a harness change or a refreshed list shows at once.
    pub(super) fn sync_workflows(&mut self) {
        let active = self.ctx.sessions.active().map(|session| session.id().to_string());
        // A pick waiting for a turn lapses when its session isn't the shown one.
        if self.areas.workflows.pending().is_some_and(|(session, _)| Some(session) != active.as_ref()) {
            self.areas.workflows.set_pending(None);
        }
        let enabled = Workflows::omp_active(&self.ctx);
        let role = self.workflow_role();
        let current = self.ctx.composer.workflow() == Some(role.as_str());
        // The options are rebuilt only when their inputs changed: the list of
        // workflows, the active role or the harness.
        if !self.areas.workflows.picker_changed(&role, enabled) && current {
            return;
        }
        let mut options = self.areas.workflows.options();
        // A role no workflow names (`/role smol`) is shown as itself.
        if !options.iter().any(|option| option.role == role) {
            options.push(composer::WorkflowOption { label: role.clone(), role: role.clone() });
        }
        self.ctx.composer.set_workflows(options, Some(role), enabled);
    }

    /// The role of the workflow in effect: the one picked mid-turn, else the
    /// one picked for the next start, else the shown session's model role,
    /// else the default.
    fn workflow_role(&self) -> String {
        let session = self.ctx.sessions.active();
        let picked = session
            .and_then(|session| self.areas.workflows.pending().filter(|(id, _)| id == session.id().as_str()).map(|(_, role)| role.clone()))
            .or_else(|| self.areas.models.pending_role(&self.ctx))
            .or_else(|| session.and_then(|session| session.dom().root()?.str_attr(attr::MODEL_ROLE).map(str::to_owned)));
        let default = self.areas.workflows.default_role();
        match picked {
            Some(role) if !role.eq_ignore_ascii_case(default) => role,
            _ => default.to_owned(),
        }
    }

    /// A workflow was picked in the composer.
    pub(super) fn workflow_picked(&mut self, role: String) -> Task<Message> {
        if !Workflows::omp_active(&self.ctx) {
            self.ctx.notify(OMP_ONLY);
            return Task::none();
        }
        let name = self.areas.workflows.name_of(&role);
        let running = self.ctx.sessions.active().filter(|session| session.harness().is_some());
        if let Some(session) = running {
            if session.is_busy() {
                let id = session.id().to_string();
                self.areas.workflows.set_pending(Some((id, role)));
                self.ctx.notify(format!("Workflow {name}: applies once this turn ends"));
                return Task::none();
            }
            self.areas.workflows.set_pending(None);
            return self.update(Message::Sessions(areas::sessions::Message::Command(format!("role.{role}"))));
        }
        let Some(model) = self.areas.workflows.model_for(&role).map(str::to_owned) else {
            self.ctx.notify(format!("omp has no model role {role}"));
            return Task::none();
        };
        self.areas.models.pick_role(model, role, &mut self.ctx);
        let session = if self.ctx.sessions.active().is_some() { "This session continues" } else { "The next session starts" };
        self.ctx.notify(format!("{session} in workflow {name}"));
        Task::none()
    }

    /// The switch a workflow picked mid-turn asks for, now that the turn of
    /// session `id` ended.
    pub(super) fn take_workflow_switch(&mut self, id: &SessionId) -> Option<Task<Message>> {
        if self.areas.workflows.pending().is_none_or(|(session, _)| session != id.as_str()) {
            return None;
        }
        let (_, role) = self.areas.workflows.take_pending()?;
        Some(self.update(Message::Sessions(areas::sessions::Message::Command(format!("role.{role}")))))
    }
}
