//! Agents: the form that builds an omp agent definition.
//!
//! The screen comes from `loa_ui_settings::agents`; Configs › Agents draws it, and the palette's
//! New agent and Edit agent commands open it in the center. Opening it resolves omp's agent
//! directory through the model backend, lists the user agents there and the project agents
//! under the active folder, and reads what the form offers: omp's models and role aliases, and
//! omp's tools (the built in defaults when omp can't list them, which the screen says).
//! Everything here is for omp sessions: the palette lists the commands only while the harness
//! is omp.
//!
//! Save checks the form with [`agents::validate`] and writes the definition: back to the file
//! the form edits, or to a new file in the user agents directory. An existing name is replaced
//! only after the screen asks. Enable, disable and delete act on the file being edited (delete
//! asks first). Each outcome refreshes the lists and says that only omp sessions started later
//! see the change.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use iced::{Element, Task};
use loa_mcp::bridge::{self, OmpBridgeConfig};
use loa_models::service::Backend;
use loa_settings::models::agents::{self, AgentDefinition, AgentFile, Overwrite};
use loa_settings::models::error::{Error, Result as SettingsResult};
use loa_settings::models::selector::THINKING_LEVELS;
use loa_ui_kit::widgets::Screen;
use loa_ui_settings::agents as screen;
use loa_view::settings::agents::{self as view, Draft, Intent};

use crate::area::{Area, AreaCommand, AreaMessage, Ctx, Listed};
use crate::view::settings::agents::{entry_to_view, field_error_to_view, fields_from_view};

#[cfg(test)]
mod live;
#[cfg(test)]
mod tests;

const OMP: &str = "omp";
const SECTION: &str = "Agents";
/// Palette title of "New agent".
const NEW_TITLE: &str = "New agent";

/// Shown when the harness isn't omp.
const OMP_ONLY: &str = "Available with OMP sessions";

#[derive(Clone, Debug)]
pub enum Message {
    /// Opens the screen on a blank form.
    Open,
    /// Opens the screen on the agent with this name.
    Edit(String),
    Screen(screen::Message),
    Loaded { load: Load, result: Result<Data, String> },
}

impl AreaMessage for Message {
    type Area = State;
}

/// What a read is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load {
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
    /// Models and tools; not read for [`Load::Refresh`].
    choices: Option<view::Choices>,
}

pub struct State {
    screen: screen::State,
    /// omp's agent directory, once resolved.
    agent_dir: Option<PathBuf>,
    files: Vec<AgentFile>,
    /// The tools the form offers, checked by Save.
    tools: Vec<String>,
    /// A full read finished: the form has its choices.
    ready: bool,
    /// The tools used instead of asking omp; set by tests.
    fixed_tools: Option<Vec<String>>,
}

impl State {
    /// Lists omp's tools as `tools` instead of asking omp.
    #[cfg(test)]
    pub(crate) fn use_tools(&mut self, tools: Vec<String>) {
        self.fixed_tools = Some(tools);
    }

    #[cfg(test)]
    pub(crate) fn form(&self) -> &screen::State {
        &self.screen
    }

    fn omp_active(ctx: &Ctx) -> bool {
        ctx.composer.harness().is_some_and(|harness| harness.id == OMP)
    }

    /// The form, for Configs › Agents: only with omp, once it has loaded.
    pub fn configs_body<'a>(&'a self, ctx: &'a Ctx) -> Option<Element<'a, Message>> {
        (Self::omp_active(ctx) && self.ready).then(|| self.screen.body().map(Message::Screen))
    }

    /// Starts reading for `load`.
    fn read(&self, ctx: &Ctx, load: Load) -> Task<Message> {
        let full = load != Load::Refresh;
        let tools = match &self.fixed_tools {
            Some(tools) => Tools::Fixed(tools.clone()),
            None => {
                let program = ctx.runtime.harnesses().get(OMP).and_then(|harness| harness.executable.clone()).unwrap_or_else(|| PathBuf::from(OMP));
                let mut config = OmpBridgeConfig::new(ctx.runtime.cwd().to_path_buf(), ctx.services.storage.dirs.data.join("omp-bridge").join("tools"));
                config.program = program;
                Tools::Omp(config)
            }
        };
        let reading = read(ctx.backends.models.clone(), ctx.runtime.cwd().to_path_buf(), tools, full);
        Task::perform(reading, move |result| Message::Loaded { load: load.clone(), result })
    }

    /// Shows the screen and reads for `load`, unless the harness isn't omp.
    fn open(&mut self, load: Load, ctx: &mut Ctx) -> Task<Message> {
        if !Self::omp_active(ctx) {
            ctx.notify(OMP_ONLY);
            return Task::none();
        }
        ctx.show_screen(Self::ID);
        self.read(ctx, load)
    }

    fn loaded(&mut self, load: Load, result: Result<Data, String>, ctx: &mut Ctx) {
        let data = match result {
            Ok(data) => data,
            Err(error) => {
                if load != Load::Refresh {
                    ctx.notify(error);
                }
                return;
            }
        };
        self.agent_dir = Some(data.agent_dir);
        self.files = data.files;
        self.screen.set_agents(self.files.iter().map(entry_to_view).collect());
        if let Some(choices) = data.choices {
            self.tools.clone_from(&choices.tools);
            self.screen.set_choices(choices);
            self.ready = true;
        }
        match &load {
            Load::Refresh => return,
            Load::Edit(name) => match self.files.iter().position(|file| file.definition.name() == name.as_str()) {
                Some(index) => self.screen.edit(index),
                None => {
                    ctx.notify(format!("No agent named {name}"));
                    if ctx.screen() == Some(Self::ID) {
                        ctx.close_screen();
                    }
                    return;
                }
            },
            Load::New => self.screen.new_agent(),
        }
        if !data.broken.is_empty() {
            ctx.notify(format!("Some agent files can't be read: {}", data.broken.join("; ")));
        }
    }

    fn screen_intent(&mut self, intent: Intent, ctx: &mut Ctx) {
        match intent {
            Intent::Close => ctx.close_screen(),
            Intent::Check(draft) => {
                let errors = self.check(&draft);
                self.screen.set_errors(errors);
            }
            Intent::Save(draft) => {
                let errors = self.check(&draft);
                let valid = errors.is_empty();
                self.screen.set_errors(errors);
                if valid {
                    self.save(draft, ctx);
                }
            }
            Intent::Replace(draft, path) => self.replace(draft, &path, ctx),
            Intent::Toggle(path) => self.toggle(&path, ctx),
            Intent::Delete(path) => self.delete(&path, ctx),
        }
    }

    /// The definition `draft` describes: its fields over the listed file it started from.
    fn definition(&self, draft: &Draft) -> SettingsResult<AgentDefinition> {
        let base = draft.base.as_ref().and_then(|base| self.files.iter().find(|file| file.path == *base));
        let base = match base {
            Some(file) => file.definition.clone(),
            None => AgentDefinition::new("", "")?,
        };
        fields_from_view(&base, draft.fields.clone())
    }

    /// The app's checks over the form.
    fn check(&self, draft: &Draft) -> Vec<view::FieldError> {
        let known_agents: Vec<String> = self.files.iter().map(|file| file.definition.name().to_owned()).collect();
        match self.definition(draft) {
            Ok(def) => agents::validate(&def, &self.tools, &known_agents).into_iter().map(field_error_to_view).collect(),
            Err(error) => vec![view::FieldError { field: "form", message: error.to_string() }],
        }
    }

    /// Writes `draft`: back to the file the form edits, or to a new file in the user agents
    /// directory. An agent of the same name elsewhere is replaced only after the screen asks.
    fn save(&mut self, draft: Draft, ctx: &mut Ctx) {
        if self.agent_dir.is_none() {
            self.screen.set_error("form", "omp's agent directory isn't known yet".to_owned());
            return;
        }
        let definition = match self.definition(&draft) {
            Ok(definition) => definition,
            Err(error) => return self.show_error(error),
        };
        let name = definition.name();
        let clash = self.files.iter().find(|file| file.definition.name() == name && Some(&file.path) != draft.editing.as_ref()).map(|file| file.path.clone());
        match (clash, &draft.editing) {
            (Some(path), _) => self.screen.ask_replace(draft, path),
            (None, Some(path)) => self.replace_file(&definition, path, ctx),
            (None, None) => {
                let result = agents::write(&self.user_agents(), &definition, Overwrite::No);
                self.saved(&definition, result, ctx);
            }
        }
    }

    /// The user confirmed replacing the file at `path`.
    fn replace(&mut self, draft: Draft, path: &Path, ctx: &mut Ctx) {
        let definition = match self.definition(&draft) {
            Ok(definition) => definition,
            Err(error) => return self.show_error(error),
        };
        self.replace_file(&definition, path, ctx);
        // Renaming an agent onto another one: the old file is gone with the rename.
        if let Some(old) = draft.editing.filter(|old| old != path && old.exists())
            && self.screen.editing() == Some(path)
            && let Err(error) = agents::delete(&old)
        {
            self.screen.set_error("form", error.to_string());
        }
    }

    fn user_agents(&self) -> PathBuf {
        self.agent_dir.clone().unwrap_or_default().join("agents")
    }

    /// Writes `definition` over the file at `path`: into the same directory, and disabled again if
    /// it was disabled. A renamed agent leaves the old file behind only when it is the file written.
    fn replace_file(&mut self, definition: &AgentDefinition, path: &Path, ctx: &mut Ctx) {
        let result = self.overwrite(definition, path);
        self.saved(definition, result, ctx);
    }

    fn overwrite(&self, definition: &AgentDefinition, path: &Path) -> SettingsResult<PathBuf> {
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
    fn saved(&mut self, definition: &AgentDefinition, result: SettingsResult<PathBuf>, ctx: &mut Ctx) {
        match result {
            Ok(path) => {
                ctx.notify(format!("Saved {}. omp sessions started from now on can use it; sessions already running won't.", definition.name()));
                self.relist(ctx);
                self.screen.edit_path(&path);
            }
            Err(error) => self.show_error(error),
        }
    }

    /// Keeps the form open with the reason next to the field it concerns.
    fn show_error(&mut self, error: Error) {
        let field = match &error {
            Error::Invalid { field, .. } if field == "name" => "name",
            Error::AlreadyExists { .. } => "name",
            _ => "form",
        };
        self.screen.set_error(field, error.to_string());
    }

    fn toggle(&mut self, path: &Path, ctx: &mut Ctx) {
        let Some(enabled) = self.screen.is_enabled(path) else { return };
        let name = self.screen.name().to_owned();
        match agents::set_enabled(path, !enabled) {
            Ok(path) => {
                let verb = if enabled { "Disabled" } else { "Enabled" };
                ctx.notify(format!("{verb} {name}. omp sessions started from now on pick up the change; sessions already running won't."));
                self.relist(ctx);
                self.screen.edit_path(&path);
            }
            Err(error) => self.show_error(error),
        }
    }

    fn delete(&mut self, path: &Path, ctx: &mut Ctx) {
        let name = self.screen.name().to_owned();
        match agents::delete(path) {
            Ok(()) => {
                ctx.notify(format!("Deleted {name}. omp sessions started from now on won't have it; sessions already running keep it."));
                self.relist(ctx);
                self.screen.new_agent();
            }
            Err(error) => self.show_error(error),
        }
    }

    /// Lists the agents again, for the screen and the palette.
    fn relist(&mut self, ctx: &Ctx) {
        let Some(agent_dir) = &self.agent_dir else { return };
        let (files, _) = agents::list(agent_dir, Some(ctx.runtime.cwd()));
        self.files = files;
        self.screen.set_agents(self.files.iter().map(entry_to_view).collect());
    }

    /// Each agent's name once, in list order.
    fn names(&self) -> Vec<&str> {
        let mut seen = HashSet::new();
        self.files.iter().map(|file| file.definition.name()).filter(|name| seen.insert(*name)).collect()
    }
}

/// Where the form's tools come from.
enum Tools {
    /// omp's own, listed through the bridge.
    Omp(OmpBridgeConfig),
    /// A fixed list; set by tests.
    Fixed(Vec<String>),
}

/// Resolves omp's agent directory, lists the agents and, when `full`, reads the models and tools
/// the form offers.
async fn read(backend: Backend, project: PathBuf, tools: Tools, full: bool) -> Result<Data, String> {
    let agent_dir = backend.files().await.map_err(|error| format!("Can't find omp's agent directory: {error}"))?.agent_dir;
    let (files, broken) = agents::list(&agent_dir, Some(&project));
    let broken = broken.into_iter().map(|(path, error)| format!("{}: {error}", path.display())).collect();
    if !full {
        return Ok(Data { agent_dir, files, broken, choices: None });
    }
    let (tools, tools_note) = match tools {
        Tools::Fixed(tools) => (tools, None),
        Tools::Omp(bridge) => match bridge::list_omp_tools(&bridge).await {
            Ok(specs) if !specs.is_empty() => (specs.into_iter().map(|spec| spec.name).collect(), None),
            Ok(_) => (default_tools(), Some(fallback_note("omp reported no tools"))),
            Err(error) => (default_tools(), Some(fallback_note(&error.to_string()))),
        },
    };
    let thinking_levels = THINKING_LEVELS.iter().map(|level| (*level).to_owned()).collect();
    let choices = view::Choices { models: models(&backend).await, thinking_levels, tools, tools_note };
    Ok(Data { agent_dir, files, broken, choices: Some(choices) })
}

fn default_tools() -> Vec<String> {
    bridge::DEFAULT_TOOLS.iter().map(|tool| (*tool).to_owned()).collect()
}

fn fallback_note(reason: &str) -> String {
    format!("Showing omp's default tools: {reason}")
}

/// omp's `provider/model` ids, then its role aliases (`@default`, `@slow`). Empty when the
/// catalog can't be read; the form then offers omp's default.
async fn models(backend: &Backend) -> Vec<String> {
    let Ok(catalog) = backend.catalog().await else { return Vec::new() };
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
    const ID: &'static str = "agents";
    const COMMANDS: &'static [AreaCommand<Self>] = &[
        AreaCommand::<Self>::new("agents.new", NEW_TITLE, SECTION, |state, _, ctx| state.open(Load::New, ctx))
            .listed(Listed::When(|_, ctx| State::omp_active(ctx).then_some(NEW_TITLE))),
        AreaCommand::<Self>::new("agents.edit.", "", SECTION, |state, name, ctx| state.open(Load::Edit(name.to_owned()), ctx)).listed(Listed::Each(|state, ctx| {
            if !State::omp_active(ctx) {
                return Vec::new();
            }
            state.names().into_iter().map(|name| (name.to_owned(), format!("Edit agent: {name}"))).collect()
        })),
    ];

    type Message = Message;

    fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let state = Self { screen: screen::State::new(), agent_dir: None, files: Vec::new(), tools: Vec::new(), ready: false, fixed_tools: None };
        // The palette lists the agents it can edit.
        let task = if ctx.runtime.harnesses().runnable(OMP).is_some() { state.read(ctx, Load::Refresh) } else { Task::none() };
        (state, task)
    }

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            Message::Open => self.open(Load::New, ctx),
            Message::Edit(name) => self.open(Load::Edit(name), ctx),
            Message::Screen(message) => {
                if let Some(intent) = self.screen.update(message) {
                    self.screen_intent(intent, ctx);
                }
                Task::none()
            }
            Message::Loaded { load, result } => {
                self.loaded(load, result, ctx);
                Task::none()
            }
        }
    }

    fn view_screen<'a>(&'a self, _: &'a Ctx) -> Option<Element<'a, Message>> {
        Some(self.screen.view().map(Message::Screen))
    }
}
