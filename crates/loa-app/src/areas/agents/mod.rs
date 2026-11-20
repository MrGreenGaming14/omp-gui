//! Agents (M4): the form that builds an omp agent definition.
//!
//! The screen comes from loa-ui-agents. Opening it resolves omp's agent
//! directory (`omp config path`), lists the user agents there and the project
//! agents under the workspace, and reads what the form offers: omp's models
//! and role aliases, and omp's tools (the built in defaults when omp can't
//! list them, which the screen says). Everything here is for omp sessions:
//! the composer button and the palette commands are disabled for any other
//! harness.
//!
//! Save writes the validated definition to the user agents directory without
//! replacing an existing file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use iced::{Element, Task};
use loa_models::agents::{self, AgentFile, Overwrite};
use loa_models::omp::{OmpFiles, OmpLaunch};
use loa_omp_bridge::OmpBridgeConfig;
use loa_ui_agents as form;
use loa_ui_models::backend::Backend;
use loa_ui_palette as palette;

use crate::area::{Area, AreaId, Ctx};

#[cfg(test)]
mod tests;

const OMP: &str = "omp";

/// Palette command name of "New agent".
const NEW: &str = "new";
/// Prefix of the palette command names of "Edit agent: <name>".
const EDIT: &str = "edit.";
const SECTION: &str = "Agents";

/// Shown when the harness isn't omp.
const OMP_ONLY: &str = "Available with OMP sessions";

#[derive(Clone, Debug)]
pub enum Message {
    /// Opens the screen on a blank form.
    Open,
    /// Opens the screen on the agent with this name.
    Edit(String),
    Form(form::Message),
    Loaded { intent: Intent, result: Result<Data, String> },
}

/// What a read is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Only list the agents, for the palette.
    Refresh,
    New,
    Edit(String),
}

/// What a read found.
#[derive(Clone, Debug)]
pub struct Data {
    agent_dir: PathBuf,
    files: Vec<AgentFile>,
    /// Files that couldn't be read, with the reason.
    broken: Vec<String>,
    /// Models and tools; not read for [`Intent::Refresh`].
    choices: Option<Choices>,
}

#[derive(Clone, Debug)]
pub struct Choices {
    models: Vec<String>,
    tools: Vec<String>,
    /// Where the tool list comes from when it isn't omp's own.
    tools_note: Option<String>,
}

/// What the area uses instead of asking omp; set by tests.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug)]
pub(crate) struct Fixed {
    pub agent_dir: PathBuf,
    pub models: Vec<String>,
    pub tools: Vec<String>,
}

pub struct State {
    form: form::State,
    /// omp's agent directory, once resolved.
    agent_dir: Option<PathBuf>,
    files: Vec<AgentFile>,
    fixed: Option<Fixed>,
}

impl State {
    pub fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let state = Self { form: form::State::new(), agent_dir: None, files: Vec::new(), fixed: None };
        // The palette lists the agents it can edit.
        let task = if ctx.services.harnesses.runnable(OMP).is_some() { state.read(ctx, Intent::Refresh) } else { Task::none() };
        (state, task)
    }

    /// Reads omp's directory, agents, models and tools from `fixed` instead
    /// of from omp.
    #[cfg(test)]
    pub(crate) fn use_fixed(&mut self, fixed: Fixed) {
        self.fixed = Some(fixed);
    }

    fn omp_active(ctx: &Ctx) -> bool {
        ctx.composer.harness().is_some_and(|harness| harness.id == OMP)
    }

    /// Starts reading for `intent`.
    fn read(&self, ctx: &Ctx, intent: Intent) -> Task<Message> {
        let services = &ctx.services;
        let executable = services.harnesses.get(OMP).and_then(|harness| harness.executable.clone());
        let launch = OmpLaunch {
            executable: executable.unwrap_or_else(|| OmpLaunch::default().executable),
            cwd: Some(services.cwd.clone()),
            env: Vec::new(),
        };
        let mut bridge = OmpBridgeConfig::new(services.cwd.clone(), services.dirs.data.join("omp-bridge").join("tools"));
        bridge.program = launch.executable.clone();
        let project = services.cwd.clone();
        let fixed = self.fixed.clone();
        let full = intent != Intent::Refresh;
        Task::perform(read(launch, bridge, project, fixed, full), move |result| Message::Loaded { intent: intent.clone(), result })
    }

    /// Opens the screen on `intent` unless the harness isn't omp.
    fn open(&mut self, intent: Intent, ctx: &mut Ctx) -> Task<Message> {
        if !Self::omp_active(ctx) {
            ctx.notify(OMP_ONLY);
            return Task::none();
        }
        self.read(ctx, intent)
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
        self.agent_dir = Some(data.agent_dir);
        self.files = data.files;
        self.form.set_agents(&self.files);
        if let Some(choices) = data.choices {
            self.form.set_models(choices.models);
            self.form.set_tools(choices.tools, choices.tools_note);
        }
        if intent == Intent::Refresh {
            return;
        }
        match &intent {
            Intent::Edit(name) => match self.files.iter().position(|file| file.definition.name() == name.as_str()) {
                Some(index) => self.form.edit(index),
                None => {
                    ctx.notify(format!("No agent named {name}"));
                    return;
                }
            },
            _ => self.form.new_agent(),
        }
        if !data.broken.is_empty() {
            ctx.notify(format!("Some agent files can't be read: {}", data.broken.join("; ")));
        }
        ctx.show_screen(AreaId::Agents);
    }

    fn form_event(&mut self, event: form::Event, ctx: &mut Ctx) {
        match event {
            form::Event::Close => ctx.close_screen(),
            form::Event::Save(definition) => self.save(definition, ctx),
        }
    }

    /// Writes `definition` to the user agents directory, refusing to replace
    /// an agent of the same name, and lists the agents again.
    fn save(&mut self, definition: loa_models::agents::AgentDefinition, ctx: &mut Ctx) {
        let Some(agent_dir) = self.agent_dir.clone() else {
            ctx.notify("omp's agent directory isn't known yet");
            return;
        };
        let name = definition.name().to_owned();
        match agents::write(&agent_dir.join("agents"), &definition, Overwrite::No) {
            Ok(path) => {
                ctx.notify(format!("Saved agent {name}"));
                self.relist(&agent_dir, ctx);
                if let Some(index) = self.files.iter().position(|file| file.path == path) {
                    self.form.edit(index);
                }
            }
            Err(error) => ctx.notify(format!("Can't save agent {name}: {error}")),
        }
    }

    fn relist(&mut self, agent_dir: &Path, ctx: &Ctx) {
        let (files, _) = agents::list(agent_dir, Some(&ctx.services.cwd));
        self.files = files;
        self.form.set_agents(&self.files);
    }
}

/// Resolves omp's agent directory, lists the agents and, when `full`, reads
/// the models and tools the form offers.
async fn read(
    launch: OmpLaunch,
    bridge: OmpBridgeConfig,
    project: PathBuf,
    fixed: Option<Fixed>,
    full: bool,
) -> Result<Data, String> {
    let agent_dir = match &fixed {
        Some(fixed) => fixed.agent_dir.clone(),
        None => OmpFiles::resolve(&launch)
            .await
            .map_err(|error| format!("Can't find omp's agent directory: {error}"))?
            .agent_dir,
    };
    let (files, broken) = agents::list(&agent_dir, Some(&project));
    let broken = broken.into_iter().map(|(path, error)| format!("{}: {error}", path.display())).collect();
    if !full {
        return Ok(Data { agent_dir, files, broken, choices: None });
    }
    let choices = match fixed {
        Some(fixed) => Choices { models: fixed.models, tools: fixed.tools, tools_note: None },
        None => {
            let (tools, tools_note) = match loa_omp_bridge::list_omp_tools(&bridge).await {
                Ok(specs) if !specs.is_empty() => (specs.into_iter().map(|spec| spec.name).collect(), None),
                Ok(_) => (default_tools(), Some(fallback_note("omp reported no tools"))),
                Err(error) => (default_tools(), Some(fallback_note(&error.to_string()))),
            };
            Choices { models: models(launch).await, tools, tools_note }
        }
    };
    Ok(Data { agent_dir, files, broken, choices: Some(choices) })
}

fn default_tools() -> Vec<String> {
    loa_omp_bridge::DEFAULT_TOOLS.iter().map(|tool| (*tool).to_owned()).collect()
}

fn fallback_note(reason: &str) -> String {
    format!("Showing omp's default tools: {reason}")
}

/// omp's `provider/model` ids, then its role aliases (`@default`, `@slow`).
/// Empty when the catalog can't be read; the form then offers omp's default.
async fn models(launch: OmpLaunch) -> Vec<String> {
    let Ok(catalog) = Backend::system(launch).catalog().await else { return Vec::new() };
    let mut ids: Vec<String> = catalog.models.iter().map(|info| info.model.selector()).collect();
    ids.sort();
    ids.dedup();
    let mut roles: Vec<String> = catalog.roles.iter().map(|assignment| format!("@{}", assignment.role)).collect();
    roles.sort();
    roles.dedup();
    roles.extend(ids);
    roles
}

impl Area for State {
    type Message = Message;

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            Message::Open => self.open(Intent::New, ctx),
            Message::Edit(name) => self.open(Intent::Edit(name), ctx),
            Message::Form(message) => {
                if let Some(event) = self.form.update(message) {
                    self.form_event(event, ctx);
                }
                Task::none()
            }
            Message::Loaded { intent, result } => {
                self.loaded(intent, result, ctx);
                Task::none()
            }
        }
    }

    fn palette_commands(&self, ctx: &Ctx) -> Vec<palette::Command> {
        let enabled = Self::omp_active(ctx);
        let mut commands = vec![palette::Command::new(AreaId::Agents.command(NEW), "New agent", SECTION).enabled(enabled)];
        let mut seen = HashSet::new();
        for file in &self.files {
            let name = file.definition.name();
            if seen.insert(name) {
                let id = AreaId::Agents.command(&format!("{EDIT}{name}"));
                commands.push(palette::Command::new(id, format!("Edit agent: {name}"), SECTION).enabled(enabled));
            }
        }
        commands
    }

    fn run_command(&mut self, id: &str, ctx: &mut Ctx) -> Option<Task<Message>> {
        let name = id.strip_prefix(AreaId::Agents.key())?.strip_prefix('.')?;
        if name == NEW {
            return Some(self.open(Intent::New, ctx));
        }
        let agent = name.strip_prefix(EDIT)?;
        Some(self.open(Intent::Edit(agent.to_owned()), ctx))
    }

    fn view_screen<'a>(&'a self, _: &'a Ctx) -> Option<Element<'a, Message>> {
        Some(self.form.view().map(Message::Form))
    }
}
