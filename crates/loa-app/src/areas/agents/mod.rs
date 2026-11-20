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
//! Save writes the validated definition: back to the file the form edits, or
//! to a new file in the user agents directory. An existing name is replaced
//! only after the screen asks. Enable, disable and delete act on the file
//! being edited (delete asks first). Each outcome refreshes the lists and
//! says that only omp sessions started later see the change.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use iced::{Element, Task};
use loa_models::agents::{self, AgentDefinition, AgentFile, Overwrite};
use loa_models::omp::{OmpFiles, OmpLaunch};
use loa_omp_bridge::OmpBridgeConfig;
use loa_ui_agents as form;
use loa_ui_models::backend::Backend;
use loa_ui_palette as palette;

use crate::area::{Area, AreaId, Ctx};

#[cfg(test)]
mod live;
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
            form::Event::Replace(definition, path) => self.replace(definition, &path, ctx),
            form::Event::Toggle(path) => self.toggle(&path, ctx),
            form::Event::Delete(path) => self.delete(&path, ctx),
        }
    }

    /// Writes `definition`: back to the file the form edits, or to a new file
    /// in the user agents directory. An agent of the same name elsewhere is
    /// replaced only after the screen asks.
    fn save(&mut self, definition: AgentDefinition, ctx: &mut Ctx) {
        if self.agent_dir.is_none() {
            self.form.set_error("form", "omp's agent directory isn't known yet".to_owned());
            return;
        }
        let editing = self.form.editing().map(Path::to_path_buf);
        let name = definition.name().to_owned();
        let clash = self
            .files
            .iter()
            .find(|file| file.definition.name() == name && Some(&file.path) != editing.as_ref())
            .map(|file| file.path.clone());
        match (clash, editing) {
            (Some(path), _) => self.form.ask_replace(definition, path),
            (None, Some(path)) => self.replace_file(&definition, &path, ctx),
            (None, None) => {
                let dir = self.user_agents();
                let result = agents::write(&dir, &definition, Overwrite::No);
                self.saved(&definition, result, ctx);
            }
        }
    }

    /// The user confirmed replacing the file at `path`.
    fn replace(&mut self, definition: AgentDefinition, path: &Path, ctx: &mut Ctx) {
        let editing = self.form.editing().map(Path::to_path_buf);
        self.replace_file(&definition, path, ctx);
        // Renaming an agent onto another one: the old file is gone with the rename.
        if let Some(old) = editing.filter(|old| old != path && old.exists())
            && self.form.editing() == Some(path)
            && let Err(error) = agents::delete(&old)
        {
            self.form.set_error("form", error.to_string());
        }
    }

    fn user_agents(&self) -> PathBuf {
        self.agent_dir.clone().unwrap_or_default().join("agents")
    }

    /// Writes `definition` over the file at `path`: into the same directory,
    /// and disabled again if it was disabled. A renamed agent leaves the old
    /// file behind only when it is the file written.
    fn replace_file(&mut self, definition: &AgentDefinition, path: &Path, ctx: &mut Ctx) {
        let result = self.overwrite(definition, path);
        self.saved(definition, result, ctx);
    }

    fn overwrite(&self, definition: &AgentDefinition, path: &Path) -> loa_models::Result<PathBuf> {
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.user_agents());
        let enabled = self.files.iter().find(|file| file.path == path).is_none_or(|file| file.enabled);
        let written = agents::write(&dir, definition, Overwrite::Yes)?;
        if enabled {
            if path != written && path.exists() {
                agents::delete(path)?;
            }
            return Ok(written);
        }
        if path.exists() {
            agents::delete(path)?;
        }
        agents::set_enabled(&written, false)
    }

    /// Shows the outcome of a write and lists the agents again.
    fn saved(&mut self, definition: &AgentDefinition, result: loa_models::Result<PathBuf>, ctx: &mut Ctx) {
        let name = definition.name();
        match result {
            Ok(path) => {
                ctx.notify(format!("Saved {name}. omp sessions started from now on can use it; sessions already running won't."));
                self.relist(ctx);
                self.form.edit_path(&path);
            }
            Err(error) => self.show_error(error),
        }
    }

    /// Keeps the form open with the reason next to the field it concerns.
    fn show_error(&mut self, error: loa_models::Error) {
        let field = match &error {
            loa_models::Error::Invalid { field, .. } if field == "name" => "name",
            loa_models::Error::AlreadyExists { .. } => "name",
            _ => "form",
        };
        self.form.set_error(field, error.to_string());
    }

    fn toggle(&mut self, path: &Path, ctx: &mut Ctx) {
        let Some(enabled) = self.form.is_enabled(path) else { return };
        let name = self.form.name().to_owned();
        match agents::set_enabled(path, !enabled) {
            Ok(path) => {
                let verb = if enabled { "Disabled" } else { "Enabled" };
                ctx.notify(format!("{verb} {name}. omp sessions started from now on pick up the change; sessions already running won't."));
                self.relist(ctx);
                self.form.edit_path(&path);
            }
            Err(error) => self.show_error(error),
        }
    }

    fn delete(&mut self, path: &Path, ctx: &mut Ctx) {
        let name = self.form.name().to_owned();
        match agents::delete(path) {
            Ok(()) => {
                ctx.notify(format!("Deleted {name}. omp sessions started from now on won't have it; sessions already running keep it."));
                self.relist(ctx);
                self.form.new_agent();
            }
            Err(error) => self.show_error(error),
        }
    }

    /// Lists the agents again, for the screen and the palette.
    fn relist(&mut self, ctx: &Ctx) {
        let Some(agent_dir) = &self.agent_dir else { return };
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
