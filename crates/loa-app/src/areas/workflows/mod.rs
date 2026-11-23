//! Workflow settings and the composer's workflow choices.

use iced::{Element, Task};
use loa_models::service::Backend;
use loa_runtime::session::SessionEvent;
use loa_settings::models::{agents, error::Error, workflows::{self, Workflow, WorkflowSnapshot}};
use loa_ui_kit::widgets::Screen;
use loa_ui_settings::{models::{self, Page}, workflows as screen};
use loa_view::{session::composer::{WorkflowKind, WorkflowOption}, settings::workflows::{self as view, Intent}};
use crate::{area::{Area, AreaCommand, AreaMessage, Ctx, Listed, Request}, view::settings::workflows::{from_view, to_view}};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub enum Message {
    Open,
    New,
    Edit(String),
    Pick(String),
    Page(Page),
    Screen(screen::Message),
    Loaded { load: Load, result: Result<Data, String> },
    Saved { role: Option<String>, result: Result<(), view::Error> },
}
impl AreaMessage for Message { type Area = State; }

#[derive(Clone, Debug)]
pub enum Load { Refresh, Open, New, Edit(String) }
#[derive(Clone, Debug)]
pub struct Data { workflows: Vec<Workflow>, snapshot: WorkflowSnapshot, agents: Vec<view::AgentInfo> }

pub struct State {
    backend: Backend,
    screen: screen::State,
    workflows: Vec<Workflow>,
    snapshot: Option<WorkflowSnapshot>,
    busy: Option<&'static str>,
}

impl State {
    fn read(&self, ctx: &Ctx, load: Load) -> Task<Message> {
        let backend = self.backend.clone();
        let project = ctx.runtime.cwd().to_path_buf();
        Task::perform(async move {
            let files = backend.files().await?;
            let (workflows, snapshot) = backend.load_workflows().await.map_err(|e| e.to_string())?;
            let (agents, _) = agents::list(&files.agent_dir, Some(&project));
            let agents = agents.into_iter().map(|file| view::AgentInfo { name: file.definition.name().to_owned(), enabled: file.enabled }).collect();
            Ok(Data { workflows, snapshot, agents })
        }, move |result| Message::Loaded { load: load.clone(), result })
    }
    fn open(&mut self, load: Load, ctx: &mut Ctx) -> Task<Message> {
        ctx.show_screen(Self::ID);
        if self.busy.is_some() { return Task::none(); }
        self.reload(ctx, load)
    }
    fn reload(&mut self, ctx: &Ctx, load: Load) -> Task<Message> {
        let load = match load {
            Load::Open => self.screen.selected().map_or(Load::Open, |role| Load::Edit(role.to_owned())),
            load => load,
        };
        self.busy = Some("Reading workflows…");
        self.read(ctx, load)
    }
    pub(crate) fn sync(&self, ctx: &mut Ctx) {
        let harness = ctx.runtime.sessions().active().and_then(|s| s.harness_id()).or(ctx.runtime.sessions().harness.as_deref());
        let current = match ctx.runtime.sessions().active() {
            Some(session) => session.dom().root().and_then(|root| root.str_attr(loa_core::attr::MODEL_ROLE)),
            None => ctx.composer.models().1.and_then(|model| model.role.as_deref()),
        }.map(str::to_owned);
        let options = self.workflows.iter().map(|workflow| {
            let (label, kind) = match &workflow.rules {
                workflows::Rules::Managed { name, .. } => (name.clone(), WorkflowKind::Managed),
                workflows::Rules::HandWritten { .. } | workflows::Rules::External { .. } => (workflow.role.clone(), WorkflowKind::HandWritten),
                workflows::Rules::None => (workflow.role.clone(), WorkflowKind::None),
            };
            WorkflowOption { role: workflow.role.clone(), label, kind, current: current.as_deref() == Some(workflow.role.as_str()) }
        }).collect();
        ctx.composer.set_workflows(options, harness == Some("omp"));
    }
    fn pick(&self, role: &str, ctx: &mut Ctx) {
        if ctx.runtime.sessions().active().and_then(|s| s.harness_id()).or(ctx.runtime.sessions().harness.as_deref()) != Some("omp") { ctx.notify("Available with OMP sessions"); return; }
        ctx.request(Request::Forward(Box::new(crate::app::Message::area(crate::areas::models::Message::PickRole(role.to_owned())))));
    }
    fn intent(&mut self, intent: Intent, ctx: &mut Ctx) -> Task<Message> {
        match intent {
            Intent::Close => { ctx.close_screen(); Task::none() }
            Intent::Open(_) | Intent::New => Task::none(),
            // The form goes back to what is on disk: a Save then can't overwrite the change it missed.
            Intent::Reload => self.reload(ctx, Load::Open),
            Intent::Save { draft, previous_role } => {
                let Some(snapshot) = self.snapshot.clone() else { return Task::none() };
                let mut workflow = draft.workflow;
                if previous_role.is_none() && workflow.role.is_empty() {
                    if let view::Rules::Managed { name, .. } = &workflow.rules {
                        match workflows::role_for(name) { Ok(role) => workflow.role = role, Err(error) => { self.screen.set_error(Some(view::Error::Message(error.to_string()))); return Task::none() } }
                    }
                }
                let workflow = match from_view(workflow) { Ok(workflow) => workflow, Err(error) => { self.screen.set_error(Some(view::Error::Message(error.to_string()))); return Task::none() } };
                let backend = self.backend.clone();
                let role = workflow.role.clone();
                self.busy = Some("Saving workflow…");
                Task::perform(async move { backend.save_workflow(&workflow, previous_role.as_deref(), &snapshot).await.map_err(error_to_view) }, move |result| Message::Saved { role: Some(role.clone()), result })
            }
            Intent::Delete(role) => {
                let Some(snapshot) = self.snapshot.clone() else { return Task::none() };
                let backend = self.backend.clone();
                self.busy = Some("Deleting workflow…");
                Task::perform(async move { backend.delete_workflow(&role, &snapshot).await.map_err(error_to_view) }, |result| Message::Saved { role: None, result })
            }
        }
    }
}
fn error_to_view(error: Error) -> view::Error { match error { Error::ChangedOnDisk { .. } => view::Error::ChangedOnDisk, error => view::Error::Message(error.to_string()) } }

// Runs after the models area's pending selection has contributed.
impl Area for State {
    const ID: &'static str = "workflows";
    fn contribute(&self, config: &mut loa_contracts::harness::SessionConfig, target: &crate::area::Target<'_>, ctx: &Ctx) {
        if target.harness != "omp" { return; }
        // A resumed session keeps the role its `<session>` records.
        let recorded = || ctx.runtime.sessions().get(&config.session_id)?.dom().root()?.str_attr(loa_core::attr::MODEL_ROLE).map(str::to_owned);
        let role = config.model_role.clone().or_else(|| if target.resume { recorded() } else { None });
        if let Some(role) = role {
            config.instructions = Some(format!("The active model role is `{role}`. Follow the RULES.md section for that role."));
        }
    }
    const COMMANDS: &'static [AreaCommand<Self>] = &[
        AreaCommand::<Self>::new("workflows.new", "New workflow", "Workflows", |s, _, ctx| s.open(Load::New, ctx)),
        AreaCommand::<Self>::new("workflows.edit.", "", "Workflows", |s, role, ctx| s.open(Load::Edit(role.to_owned()), ctx)).listed(Listed::Each(|s, _| s.workflows.iter().map(|w| (w.role.clone(), format!("Edit workflow: {}", w.role))).collect())),
        AreaCommand::<Self>::new("workflows.use.", "", "Workflows", |s, role, ctx| { s.pick(role, ctx); Task::none() }).listed(Listed::Each(|s, _| s.workflows.iter().map(|w| (w.role.clone(), format!("Use workflow: {}", w.role))).collect())),
    ];
    type Message = Message;
    fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let state = Self { backend: ctx.backends.models.clone(), screen: screen::State::new(), workflows: Vec::new(), snapshot: None, busy: None };
        state.sync(ctx);
        let task = state.read(ctx, Load::Refresh);
        (state, task)
    }
    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            Message::Open => self.open(Load::Open, ctx),
            Message::New => self.open(Load::New, ctx),
            Message::Edit(role) => self.open(Load::Edit(role), ctx),
            Message::Pick(role) => { self.pick(&role, ctx); Task::none() }
            Message::Page(Page::Workflows) => Task::none(),
            Message::Page(page) => { ctx.request(Request::Forward(Box::new(crate::app::Message::area(crate::areas::models::Message::Show(page))))); Task::none() }
            Message::Screen(loa_ui_kit::widgets::Msg::Emit(Intent::Close)) => self.intent(Intent::Close, ctx),
            Message::Screen(_) if self.busy.is_some() => Task::none(),
            Message::Screen(message) => self.screen.update(message).map_or_else(Task::none, |intent| self.intent(intent, ctx)),
            Message::Loaded { load, result } => {
                if matches!(load, Load::Refresh) {
                    // The startup read must not replace a newer editor snapshot.
                    if self.snapshot.is_some() || self.busy.is_some() { return Task::none(); }
                } else {
                    self.busy = None;
                }
                match result {
                    Ok(data) => {
                        self.workflows = data.workflows; self.snapshot = Some(data.snapshot);
                        self.screen.set_workflows(self.workflows.iter().map(to_view).collect()); self.screen.set_agents(data.agents); self.screen.set_error(None);
                        match load {
                            Load::New => self.screen.new_workflow(),
                            Load::Edit(role) if !self.screen.edit(&role) => {
                                self.screen.new_workflow();
                                ctx.notify(format!("No workflow {role}"));
                            }
                            _ => {}
                        }
                        self.sync(ctx);
                    }
                    Err(error) => if !matches!(load, Load::Refresh) { self.screen.set_error(Some(view::Error::Message(error))); },
                }
                Task::none()
            }
            Message::Saved { role, result } => match result {
                Ok(()) => { ctx.notify(if role.is_some() { "Workflow saved" } else { "Workflow deleted" }); self.reload(ctx, role.map_or(Load::New, Load::Edit)) }
                Err(error) => { self.busy = None; self.screen.set_error(Some(error)); Task::none() }
            }
        }
    }
    fn view_screen<'a>(&'a self, _: &'a Ctx) -> Option<Element<'a, Message>> {
        let body = match self.busy {
            Some(status) => models::status(status),
            None => self.screen.body().map(Message::Screen),
        };
        Some(models::frame(Page::Workflows, body, Message::Page, Message::Screen(loa_ui_kit::widgets::Msg::Emit(Intent::Close))))
    }
    fn on_session_event(&mut self, _: &SessionEvent, ctx: &mut Ctx) -> Task<Message> { self.sync(ctx); Task::none() }
}
