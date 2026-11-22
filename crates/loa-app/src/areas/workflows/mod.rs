//! Workflows: named modes that tell omp's manager model which agents to use.
//!
//! The screen comes from loa-ui-workflows. Opening it resolves omp's agent
//! directory (`omp config path`), lists the workflows written to `RULES.md`
//! and `config.yml` there and the agents they may allow or disallow
//! (disabled ones included). Save and delete go through
//! [`WorkflowFiles`], which validates the new `config.yml` with omp before
//! writing; a refusal shows at the top of the editor.
//!
//! The workflows also feed the composer's workflow picker (the app asks
//! [`State::options`] when the list, the active role or the harness changed,
//! see `app/workflows.rs`), so they are read at startup too. Everything here is for omp sessions: opening the
//! screen and the palette commands are refused for any other harness.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::{Element, Task};
use loa_models::agents;
use loa_models::config::OmpConfig;
use loa_models::omp::{OmpFiles, OmpLaunch};
use loa_models::process::TokioProcessRunner;
use loa_models::validate::{OmpSettingsValidator, SettingsValidator};
use loa_models::workflows::{Workflow, WorkflowFiles, role_for};
use loa_adapter::{Prompt, SessionConfig};
use loa_bus::SessionId;
use loa_core::attr;
use loa_ui_composer::{self as composer, WorkflowOption};
use loa_ui_palette as palette;
use loa_ui_workflows as screen;

use crate::area::{Area, AreaId, Ctx, Request, Target};

#[cfg(test)]
mod tests;

const OMP: &str = "omp";

/// Palette command name of "New workflow".
const NEW: &str = "new";
/// Prefix of the palette command names of "Edit workflow: <name>".
const EDIT: &str = "edit.";
/// Prefix of the palette command names of "Use workflow: <name>".
const USE: &str = "use.";
const SECTION: &str = "Workflows";

/// Shown when the harness isn't omp.
pub(crate) const OMP_ONLY: &str = "Available with OMP sessions";

/// The role of the workflow that is no workflow: omp's own `default`.
const DEFAULT_ROLE: &str = "default";
const DEFAULT_LABEL: &str = "Default";

#[derive(Clone, Debug)]
pub enum Message {
    /// Opens the screen on a blank editor.
    Open,
    /// Opens the screen on the workflow with this role.
    Edit(String),
    Screen(screen::Message),
    Loaded { intent: Intent, result: Result<Data, String> },
    /// A save or delete finished: the write's refusal, or the lists read
    /// again (`None` when they couldn't be).
    Written { outcome: Outcome, result: Result<Option<Data>, String> },
    /// The model role of `session` was switched (the role now in effect), or
    /// the switch failed.
    RoleSwitched { session: String, result: Result<String, String> },
}

/// What a read is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Only list the workflows, for the palette and the picker.
    Refresh,
    New,
    Edit(String),
}

/// What a write did.
#[derive(Clone, Debug)]
pub enum Outcome {
    Saved { name: String, role: String },
    Deleted { name: String },
}

/// What a read found.
#[derive(Clone, Debug)]
pub struct Data {
    agent_dir: PathBuf,
    workflows: Vec<Workflow>,
    agents: Vec<screen::AgentInfo>,
    /// The first candidate model of each role in `config.yml`.
    models: HashMap<String, String>,
}

/// What the area uses instead of asking omp; set by tests.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone)]
pub(crate) struct Fixed {
    pub agent_dir: PathBuf,
    pub validator: Arc<dyn SettingsValidator>,
}

pub struct State {
    screen: screen::State,
    /// omp's agent directory, once resolved.
    agent_dir: Option<PathBuf>,
    workflows: Vec<Workflow>,
    models: HashMap<String, String>,
    fixed: Option<Fixed>,
    /// A workflow picked while the session's turn runs: the session and the
    /// role to switch to once the turn ends.
    pending: Option<(String, String)>,
    /// Bumped whenever the list of workflows changes.
    revision: u64,
    /// What the composer's picker was last built from: the list revision, the
    /// active role and whether the harness is omp.
    picker: Option<(u64, String, bool)>,
    /// Sessions switched to another role mid-session: the role the model is
    /// told about with the session's next prompt.
    switched: HashMap<String, String>,
    /// The session whose queued prompt waits for its role switch to succeed.
    held: Option<String>,
}

impl State {
    pub fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let state = Self {
            screen: screen::State::new(),
            agent_dir: None,
            workflows: Vec::new(),
            models: HashMap::new(),
            fixed: None,
            pending: None,
            revision: 0,
            picker: None,
            switched: HashMap::new(),
            held: None,
        };
        // The palette and the picker list the workflows.
        let task = if ctx.services.harnesses.runnable(OMP).is_some() { state.read(ctx, Intent::Refresh) } else { Task::none() };
        (state, task)
    }

    /// Reads omp's directory from `fixed` instead of from omp, and validates
    /// with its validator.
    #[cfg(test)]
    pub(crate) fn use_fixed(&mut self, fixed: Fixed) {
        self.fixed = Some(fixed);
    }

    pub(crate) fn omp_active(ctx: &Ctx) -> bool {
        ctx.composer.harness().is_some_and(|harness| harness.id == OMP)
    }

    /// The role of the workflow that is no workflow: a hand-written default
    /// mode's role, else omp's `default`.
    pub(crate) fn default_role(&self) -> &str {
        self.workflows.iter().find(|workflow| workflow.role.eq_ignore_ascii_case(DEFAULT_ROLE)).map_or(DEFAULT_ROLE, |workflow| workflow.role.as_str())
    }

    /// The picker's rows: the default workflow first, then every other one.
    pub(crate) fn options(&self) -> Vec<WorkflowOption> {
        let default = self.default_role();
        let label = self.workflows.iter().find(|workflow| workflow.role == default).map_or(DEFAULT_LABEL, |workflow| workflow.name.as_str());
        let mut options = vec![WorkflowOption { role: default.to_owned(), label: label.to_owned() }];
        options.extend(
            self.workflows
                .iter()
                .filter(|workflow| workflow.role != default)
                .map(|workflow| WorkflowOption { role: workflow.role.clone(), label: workflow.name.clone() }),
        );
        options
    }

    /// The model `role` starts on, from omp's `config.yml`.
    pub(crate) fn model_for(&self, role: &str) -> Option<&str> {
        self.models.get(role).map(String::as_str)
    }

    /// The name `role` is shown under.
    pub(crate) fn name_of(&self, role: &str) -> String {
        self.workflows.iter().find(|workflow| workflow.role == role).map_or_else(|| role.to_owned(), |workflow| workflow.name.clone())
    }

    pub(crate) fn pending(&self) -> Option<&(String, String)> {
        self.pending.as_ref()
    }

    pub(crate) fn set_pending(&mut self, pending: Option<(String, String)>) {
        self.pending = pending;
    }

    pub(crate) fn take_pending(&mut self) -> Option<(String, String)> {
        self.pending.take()
    }

    /// Records what the composer's picker is built from now; true when that
    /// differs from the last time, so the picker needs new options.
    pub(crate) fn picker_changed(&mut self, active: &str, enabled: bool) -> bool {
        let same = self.picker.as_ref().is_some_and(|(revision, role, was)| *revision == self.revision && role == active && *was == enabled);
        if !same {
            self.picker = Some((self.revision, active.to_owned(), enabled));
        }
        !same
    }

    /// Holds the queued prompt of `session` back until its role switch
    /// succeeds.
    pub(crate) fn hold_queue(&mut self, session: &SessionId) {
        self.held = Some(session.to_string());
    }

    /// The role `config`'s session starts or resumes in: the one picked for
    /// it, else the role it already runs on, else the default.
    fn start_role(&self, config: &SessionConfig, target: &Target<'_>, ctx: &Ctx) -> String {
        let resumed = || {
            let session = ctx.sessions.get(&config.session_id).filter(|_| target.resume)?;
            session.dom().root()?.str_attr(attr::MODEL_ROLE).map(str::to_owned)
        };
        config.model_role.clone().or_else(resumed).unwrap_or_else(|| self.default_role().to_owned())
    }

    fn launch(ctx: &Ctx) -> OmpLaunch {
        let services = &ctx.services;
        let executable = services.harnesses.get(OMP).and_then(|harness| harness.executable.clone());
        OmpLaunch {
            executable: executable.unwrap_or_else(|| OmpLaunch::default().executable),
            cwd: Some(services.cwd.clone()),
            env: Vec::new(),
        }
    }

    /// Starts reading for `intent`.
    fn read(&self, ctx: &Ctx, intent: Intent) -> Task<Message> {
        let launch = Self::launch(ctx);
        let project = ctx.services.cwd.clone();
        let fixed = self.fixed.clone();
        Task::perform(read(launch, project, fixed), move |result| Message::Loaded { intent: intent.clone(), result })
    }

    /// Opens the screen on `intent` unless the harness isn't omp.
    fn open(&mut self, intent: Intent, ctx: &mut Ctx) -> Task<Message> {
        if !Self::omp_active(ctx) {
            ctx.notify(OMP_ONLY);
            return Task::none();
        }
        self.read(ctx, intent)
    }

    fn apply(&mut self, data: Data, ctx: &mut Ctx) {
        self.screen.set_data(entries(&data.workflows), data.agents);
        ctx.services.omp_agent_dir.set(data.agent_dir.clone());
        self.agent_dir = Some(data.agent_dir);
        if self.workflows != data.workflows {
            self.revision += 1;
        }
        self.workflows = data.workflows;
        self.models = data.models;
    }

    fn loaded(&mut self, intent: Intent, result: Result<Data, String>, ctx: &mut Ctx) {
        let data = match result {
            Ok(data) => data,
            Err(error) => {
                if intent != Intent::Refresh {
                    ctx.notify(error);
                }
                return;
            }
        };
        self.apply(data, ctx);
        match &intent {
            Intent::Refresh => return,
            Intent::Edit(role) => {
                if !self.workflows.iter().any(|workflow| workflow.role == *role) {
                    ctx.notify(format!("No workflow with role {role}"));
                    return;
                }
                self.screen.edit(Some(role));
            }
            Intent::New => self.screen.edit(None),
        }
        // The harness may have changed while omp's directory was read.
        if !Self::omp_active(ctx) {
            ctx.notify(OMP_ONLY);
            return;
        }
        ctx.show_screen(AreaId::Workflows);
    }

    fn validator(&self, ctx: &Ctx) -> Arc<dyn SettingsValidator> {
        match &self.fixed {
            Some(fixed) => Arc::clone(&fixed.validator),
            None => Arc::new(OmpSettingsValidator::new(Self::launch(ctx).executable, Arc::new(TokioProcessRunner))),
        }
    }

    fn screen_event(&mut self, event: screen::Event, ctx: &mut Ctx) -> Task<Message> {
        match event {
            screen::Event::Close => {
                ctx.close_screen();
                Task::none()
            }
            screen::Event::Save { previous_role, entry } => self.save(previous_role, entry, ctx),
            screen::Event::Delete { role } => self.delete(role, ctx),
        }
    }

    fn files(&self) -> Option<WorkflowFiles> {
        self.agent_dir.as_deref().map(WorkflowFiles::in_agent_dir)
    }

    /// Writes the workflow the editor validated. Its role comes from its
    /// name; renaming moves it to the new role.
    fn save(&mut self, previous_role: Option<String>, entry: screen::Entry, ctx: &mut Ctx) -> Task<Message> {
        if !Self::omp_active(ctx) {
            ctx.notify(OMP_ONLY);
            return Task::none();
        }
        let Some(files) = self.files() else {
            self.screen.set_error(Some("omp's agent directory isn't known yet".to_owned()));
            return Task::none();
        };
        let role = match role_for(&entry.name) {
            Ok(role) => role,
            Err(error) => {
                self.screen.set_error(Some(error.to_string()));
                return Task::none();
            }
        };
        let workflow = Workflow {
            role: role.clone(),
            name: entry.name.clone(),
            allowed: entry.allowed,
            disallowed: entry.disallowed,
            notes: entry.notes,
            managed: true,
        };
        let outcome = Outcome::Saved { name: entry.name, role };
        let validator = self.validator(ctx);
        let project = ctx.services.cwd.clone();
        Task::perform(
            async move {
                files.save(&workflow, previous_role.as_deref(), validator.as_ref()).await.map_err(|error| error.to_string())?;
                Ok(relist(files, &project))
            },
            move |result| Message::Written { outcome: outcome.clone(), result },
        )
    }

    fn delete(&mut self, role: String, ctx: &mut Ctx) -> Task<Message> {
        if !Self::omp_active(ctx) {
            ctx.notify(OMP_ONLY);
            return Task::none();
        }
        let Some(files) = self.files() else {
            self.screen.set_error(Some("omp's agent directory isn't known yet".to_owned()));
            return Task::none();
        };
        let outcome = Outcome::Deleted { name: self.name_of(&role) };
        let validator = self.validator(ctx);
        let project = ctx.services.cwd.clone();
        Task::perform(
            async move {
                files.delete(&role, validator.as_ref()).await.map_err(|error| error.to_string())?;
                Ok(relist(files, &project))
            },
            move |result| Message::Written { outcome: outcome.clone(), result },
        )
    }

    /// The role of session `session` was switched, or the switch failed. A
    /// switch is told to the model with the session's next prompt, and
    /// releases the queued prompt that waited for it; a failure keeps that
    /// prompt queued.
    fn role_switched(&mut self, session: String, result: Result<String, String>, ctx: &mut Ctx) {
        let held = self.held.as_deref() == Some(session.as_str());
        let role = match result {
            Ok(role) => role,
            Err(error) => {
                if held {
                    ctx.notify(format!("{error}. The queued message stays queued; Stop returns it to the editor"));
                }
                return;
            }
        };
        if ctx.sessions.get(&session).is_some_and(|open| open.harness_id() == Some(OMP)) {
            self.switched.insert(session.clone(), role);
        }
        if !held {
            return;
        }
        self.held = None;
        if ctx.sessions.is_active(&session) && ctx.composer.queue().next().is_some() {
            if let Some(event) = ctx.composer.turn_ended() {
                ctx.request(Request::Forward(Box::new(crate::app::Message::ComposerEvent(event))));
            }
        }
    }

    /// Shows what a write did: the refusal in the editor, or the new lists.
    fn written(&mut self, outcome: Outcome, result: Result<Option<Data>, String>, ctx: &mut Ctx) {
        let data = match result {
            Ok(data) => data,
            Err(error) => {
                self.screen.set_error(Some(error));
                return;
            }
        };
        if let Some(data) = data {
            self.apply(data, ctx);
        }
        match outcome {
            Outcome::Saved { name, role } => {
                self.screen.edit(Some(&role));
                ctx.notify(format!("Saved workflow {name}"));
            }
            Outcome::Deleted { name } => ctx.notify(format!("Deleted workflow {name}")),
        }
    }
}

/// The lists after a write; `None` when they can't be read again.
fn relist(files: WorkflowFiles, project: &Path) -> Option<Data> {
    let agent_dir = files.config.parent()?.to_path_buf();
    load(agent_dir, project).ok()
}

fn entries(workflows: &[Workflow]) -> Vec<screen::Entry> {
    workflows
        .iter()
        .map(|workflow| screen::Entry {
            role: workflow.role.clone(),
            name: workflow.name.clone(),
            allowed: workflow.allowed.clone(),
            disallowed: workflow.disallowed.clone(),
            notes: workflow.notes.clone(),
            editable: workflow.managed,
        })
        .collect()
}

/// Resolves omp's agent directory and reads the workflows and agents in it.
async fn read(launch: OmpLaunch, project: PathBuf, fixed: Option<Fixed>) -> Result<Data, String> {
    let agent_dir = match fixed {
        Some(fixed) => fixed.agent_dir,
        None => OmpFiles::resolve(&launch)
            .await
            .map_err(|error| format!("Can't find omp's agent directory: {error}"))?
            .agent_dir,
    };
    load(agent_dir, &project)
}

fn load(agent_dir: PathBuf, project: &Path) -> Result<Data, String> {
    let files = WorkflowFiles::in_agent_dir(&agent_dir);
    let workflows = files.list().map_err(|error| format!("Can't read {}: {error}", files.rules.display()))?;
    let roles = OmpConfig::load(&files.config)
        .and_then(|config| config.model_roles())
        .map_err(|error| format!("Can't read the model roles in {}: {error}", files.config.display()))?;
    let models = roles.into_iter().filter_map(|role| Some((role.role, role.candidates.first()?.to_string()))).collect();
    let (listed, _) = agents::list(&agent_dir, Some(project));
    let mut seen = HashSet::new();
    let agents = listed
        .iter()
        .filter(|file| seen.insert(file.definition.name().to_owned()))
        .map(|file| screen::AgentInfo { name: file.definition.name().to_owned(), enabled: file.enabled })
        .collect();
    Ok(Data { agent_dir, workflows, agents, models })
}

impl Area for State {
    type Message = Message;

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            Message::Open => self.open(Intent::New, ctx),
            Message::Edit(role) => self.open(Intent::Edit(role), ctx),
            Message::Screen(message) => match self.screen.update(message) {
                Some(event) => self.screen_event(event, ctx),
                None => Task::none(),
            },
            Message::Loaded { intent, result } => {
                self.loaded(intent, result, ctx);
                Task::none()
            }
            Message::Written { outcome, result } => {
                self.written(outcome, result, ctx);
                Task::none()
            }
            Message::RoleSwitched { session, result } => {
                self.role_switched(session, result, ctx);
                Task::none()
            }
        }
    }

    fn palette_commands(&self, ctx: &Ctx) -> Vec<palette::Command> {
        let enabled = Self::omp_active(ctx);
        let mut commands = vec![palette::Command::new(AreaId::Workflows.command(NEW), "New workflow", SECTION).enabled(enabled)];
        for option in self.options() {
            let id = AreaId::Workflows.command(&format!("{USE}{}", option.role));
            commands.push(palette::Command::new(id, format!("Use workflow: {}", option.label), SECTION).enabled(enabled));
        }
        for workflow in &self.workflows {
            let id = AreaId::Workflows.command(&format!("{EDIT}{}", workflow.role));
            commands.push(palette::Command::new(id, format!("Edit workflow: {}", workflow.name), SECTION).enabled(enabled));
        }
        commands
    }

    fn run_command(&mut self, id: &str, ctx: &mut Ctx) -> Option<Task<Message>> {
        let name = id.strip_prefix(AreaId::Workflows.key())?.strip_prefix('.')?;
        if name == NEW {
            return Some(self.open(Intent::New, ctx));
        }
        if let Some(role) = name.strip_prefix(USE) {
            // The composer's pick, so the keyboard and the mouse take one path.
            let event = composer::Event::WorkflowChanged(role.to_owned());
            ctx.request(Request::Forward(Box::new(crate::app::Message::ComposerEvent(event))));
            return Some(Task::none());
        }
        let role = name.strip_prefix(EDIT)?;
        Some(self.open(Intent::Edit(role.to_owned()), ctx))
    }

    fn contribute(&self, config: &mut SessionConfig, target: &Target<'_>, ctx: &Ctx) {
        // OMP never says which model role is active; RULES.md sections are
        // written against it, so the app does.
        if target.harness == OMP {
            let role = self.start_role(config, target, ctx);
            config.instructions = Some(format!("The active model role is `{role}`. Follow the RULES.md section for that role."));
        }
    }

    fn before_send(&mut self, session: &SessionId, prompt: &mut Prompt, _: &mut Ctx) {
        if let Some(role) = self.switched.remove(session.as_str()) {
            prompt.text = format!("The active model role is now `{role}`.\n\n{}", prompt.text);
        }
    }

    fn view_screen<'a>(&'a self, _: &'a Ctx) -> Option<Element<'a, Message>> {
        Some(self.screen.view().map(Message::Screen))
    }
}
