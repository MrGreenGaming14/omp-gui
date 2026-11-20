//! The agents screen: the omp agents found on disk on the left, the form
//! that builds one on the right.
//!
//! The form has one field per section of the definition: name, description,
//! model, thinking level, one checkbox per tool, one checkbox per other
//! agent the new agent may spawn, and the prompt body. A template fills it:
//! blank, or a copy of a listed agent. [`State::update`] returns
//! [`Event::Save`] only when [`loa_models::agents::validate`] finds nothing;
//! otherwise the problems show next to their fields.
//!
//! The app lists the agents ([`State::set_agents`]), the models
//! ([`State::set_models`]) and the tools ([`State::set_tools`]) and writes
//! what [`Event::Save`] carries.

use std::fmt;
use std::path::{Path, PathBuf};

use iced::widget::{button, checkbox, column, container, pick_list, row, scrollable, space, text, text_editor, text_input};
use iced::{Element, Fill, Length};
use loa_models::agents::{AgentDefinition, AgentFile, AgentSource, FieldError, ModelSpec, validate};
use loa_models::config::THINKING_LEVELS;

#[cfg(test)]
mod tests;

/// The model choice that leaves `model` out: omp picks its default.
pub const MODEL_DEFAULT: &str = "(omp default)";
/// The thinking level choice that leaves `thinkingLevel` out.
pub const THINKING_DEFAULT: &str = "(default)";

const BODY: f32 = 14.0;
const SMALL: f32 = 12.0;
const LIST_WIDTH: f32 = 240.0;
const EDITOR_HEIGHT: f32 = 240.0;

/// What the screen asks the app to do.
#[derive(Clone, Debug)]
pub enum Event {
    /// The form validated: write this definition.
    Save(AgentDefinition),
    /// The user confirmed replacing the agent file at the path.
    Replace(AgentDefinition, PathBuf),
    /// Enable or disable the agent file at the path (the opposite of its
    /// state now).
    Toggle(PathBuf),
    /// The user confirmed deleting the agent file at the path.
    Delete(PathBuf),
    Close,
}

/// A question waiting for the user's answer.
#[derive(Clone, Debug)]
enum Pending {
    Replace { definition: AgentDefinition, path: PathBuf },
    Delete { name: String, path: PathBuf },
}

#[derive(Clone, Debug)]
pub enum Message {
    /// A template was picked.
    Template(Template),
    /// An agent of the list was picked: its fields fill the form.
    Edit(usize),
    New,
    Name(String),
    Description(String),
    Model(String),
    Thinking(String),
    Tool(String, bool),
    Spawn(String, bool),
    Body(text_editor::Action),
    Save,
    /// Enables or disables the agent being edited.
    Toggle,
    /// Asks before deleting the agent being edited.
    Delete,
    /// Answers the pending question with yes.
    Confirm,
    /// Answers the pending question with no.
    Cancel,
    Close,
}

/// A way to start the form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    /// The agent copied, by position in the list; `None` is blank.
    index: Option<usize>,
    label: String,
}

impl Template {
    fn blank() -> Self {
        Self { index: None, label: "Blank".to_owned() }
    }
}

impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

struct Entry {
    path: PathBuf,
    name: String,
    source: AgentSource,
    enabled: bool,
    definition: AgentDefinition,
}

pub struct State {
    entries: Vec<Entry>,
    models: Vec<String>,
    tools: Vec<String>,
    tools_note: Option<String>,
    /// The definition the form started from; keys the form has no field for
    /// stay in it.
    base: AgentDefinition,
    template: Template,
    name: String,
    description: String,
    model: Option<ModelSpec>,
    thinking: Option<String>,
    tools_on: Vec<String>,
    spawns_on: Vec<String>,
    body: text_editor::Content,
    errors: Vec<FieldError>,
    /// The file the form edits; `None` for a new agent or a copy.
    editing: Option<PathBuf>,
    pending: Option<Pending>,
    /// Set by the first Save press; from then on the errors follow the
    /// edits.
    show_errors: bool,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            models: Vec::new(),
            tools: Vec::new(),
            tools_note: None,
            base: blank_definition(),
            template: Template::blank(),
            name: String::new(),
            description: String::new(),
            model: None,
            thinking: None,
            tools_on: Vec::new(),
            spawns_on: Vec::new(),
            body: text_editor::Content::new(),
            errors: Vec::new(),
            editing: None,
            pending: None,
            show_errors: false,
        }
    }

    /// The agents found on disk, in list order.
    pub fn set_agents(&mut self, files: &[AgentFile]) {
        self.entries = files
            .iter()
            .map(|file| Entry {
                path: file.path.clone(),
                name: file.definition.name().to_owned(),
                source: file.source,
                enabled: file.enabled,
                definition: file.definition.clone(),
            })
            .collect();
        match self.template.index {
            Some(index) if index < self.entries.len() => {
                self.template.label = copy_label(&self.entries[index].name);
            }
            Some(_) => self.template = Template::blank(),
            None => {}
        }
    }

    /// The question waiting for an answer, if any.
    pub fn pending_question(&self) -> Option<String> {
        self.pending.as_ref().map(|pending| match pending {
            Pending::Replace { definition, .. } => format!("Replace {}?", definition.name()),
            Pending::Delete { name, .. } => format!("Delete {name}?"),
        })
    }

    /// The file the form edits, when it edits one.
    pub fn editing(&self) -> Option<&Path> {
        self.editing.as_deref()
    }

    /// Whether the agent file at `path` is listed enabled.
    pub fn is_enabled(&self, path: &Path) -> Option<bool> {
        self.entries.iter().find(|entry| entry.path == path).map(|entry| entry.enabled)
    }

    /// Fills the form from the listed agent file at `path`, unchanged.
    pub fn edit_path(&mut self, path: &Path) -> bool {
        match self.entries.iter().position(|entry| entry.path == path) {
            Some(index) => {
                self.edit(index);
                true
            }
            None => false,
        }
    }

    /// Asks whether to replace the agent file at `path` with `definition`.
    pub fn ask_replace(&mut self, definition: AgentDefinition, path: PathBuf) {
        self.pending = Some(Pending::Replace { definition, path });
    }

    /// Shows `message` next to `field` (`"form"` for the top of the form),
    /// keeping the form open.
    pub fn set_error(&mut self, field: &'static str, message: String) {
        self.pending = None;
        self.errors.retain(|error| error.field != field);
        self.errors.push(FieldError { field, message });
    }

    /// Model choices: `provider/model` ids and role aliases such as `@slow`.
    pub fn set_models(&mut self, models: Vec<String>) {
        self.models = models;
    }

    /// The tools to tick, and a line saying where the list comes from when
    /// it isn't omp's own.
    pub fn set_tools(&mut self, tools: Vec<String>, note: Option<String>) {
        self.tools = tools;
        self.tools_note = note;
    }

    /// Starts a blank form.
    pub fn new_agent(&mut self) {
        self.fill(Template::blank(), blank_definition(), None);
    }

    /// Fills the form from the agent at `index`, unchanged.
    pub fn edit(&mut self, index: usize) {
        if let Some(entry) = self.entries.get(index) {
            let definition = entry.definition.clone();
            let path = entry.path.clone();
            self.fill(Template::blank(), definition, None);
            self.editing = Some(path);
        }
    }

    /// Fills the form with a copy of the agent at `index`: every field and
    /// the body, with `-copy` after the name.
    pub fn copy(&mut self, index: usize) {
        if let Some(entry) = self.entries.get(index) {
            let template = Template { index: Some(index), label: copy_label(&entry.name) };
            let definition = entry.definition.clone();
            let name = format!("{}-copy", entry.name);
            self.fill(template, definition, Some(name));
        }
    }

    /// The agents' names in list order.
    pub fn agent_names(&self) -> Vec<&str> {
        self.entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    /// The templates the picker offers.
    pub fn templates(&self) -> Vec<Template> {
        std::iter::once(Template::blank())
            .chain(self.entries.iter().enumerate().map(|(index, entry)| Template { index: Some(index), label: copy_label(&entry.name) }))
            .collect()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The problems the last check found, by field.
    pub fn errors(&self) -> &[FieldError] {
        &self.errors
    }

    /// The definition the form holds now.
    pub fn definition(&self) -> Result<AgentDefinition, loa_models::Error> {
        let mut def = self.base.clone();
        def.set_name(self.name.trim())?;
        def.set_description(self.description.trim())?;
        def.set_model(self.model.clone())?;
        def.set_thinking_level(self.thinking.as_deref())?;
        def.set_tools(self.ordered(&self.tools_on, &self.tools))?;
        let others = self.other_agents();
        def.set_spawns(self.ordered(&self.spawns_on, &others))?;
        def.set_body(self.body.text());
        Ok(def)
    }

    /// `selected` in the order of `known`, unknown entries last; `None` when
    /// nothing is selected (the key is left out and omp's default applies).
    fn ordered(&self, selected: &[String], known: &[String]) -> Option<Vec<String>> {
        let mut out: Vec<String> = known.iter().filter(|item| selected.contains(item)).cloned().collect();
        out.extend(selected.iter().filter(|item| !known.contains(item)).cloned());
        (!out.is_empty()).then_some(out)
    }

    fn fill(&mut self, template: Template, base: AgentDefinition, name: Option<String>) {
        self.template = template;
        self.name = name.unwrap_or_else(|| base.name().to_owned());
        self.description = base.description().to_owned();
        self.model = base.model();
        self.thinking = base.thinking_level().map(str::to_owned);
        self.tools_on = base.tools().unwrap_or_default();
        self.spawns_on = base.spawns().unwrap_or_default();
        self.body = text_editor::Content::with_text(base.body());
        self.base = base;
        self.errors.clear();
        self.show_errors = false;
        self.editing = None;
        self.pending = None;
    }

    /// The other known agents, deduplicated, in list order.
    fn other_agents(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for entry in &self.entries {
            if entry.name != self.name.trim() && !names.contains(&entry.name) {
                names.push(entry.name.clone());
            }
        }
        names
    }

    /// Runs the app's checks over the form.
    fn check(&mut self) {
        let known_agents: Vec<String> = self.entries.iter().map(|entry| entry.name.clone()).collect();
        self.errors = match self.definition() {
            Ok(def) => validate(&def, &self.tools, &known_agents),
            Err(error) => vec![FieldError { field: "form", message: error.to_string() }],
        };
    }

    pub fn update(&mut self, message: Message) -> Option<Event> {
        match message {
            Message::Close => return Some(Event::Close),
            Message::Save => {
                self.pending = None;
                self.show_errors = true;
                self.check();
                if !self.errors.is_empty() {
                    return None;
                }
                return match self.definition() {
                    Ok(def) => Some(Event::Save(def)),
                    Err(error) => {
                        self.errors.push(FieldError { field: "form", message: error.to_string() });
                        None
                    }
                };
            }
            Message::Toggle => return self.editing.clone().map(Event::Toggle),
            Message::Delete => {
                if let Some(path) = self.editing.clone() {
                    self.pending = Some(Pending::Delete { name: self.name.clone(), path });
                }
                return None;
            }
            Message::Cancel => {
                self.pending = None;
                return None;
            }
            Message::Confirm => {
                return match self.pending.take() {
                    Some(Pending::Replace { definition, path }) => Some(Event::Replace(definition, path)),
                    Some(Pending::Delete { path, .. }) => Some(Event::Delete(path)),
                    None => None,
                };
            }
            Message::Template(template) => match template.index {
                Some(index) => self.copy(index),
                None => self.new_agent(),
            },
            Message::New => self.new_agent(),
            Message::Edit(index) => self.edit(index),
            Message::Name(name) => self.name = name,
            Message::Description(description) => self.description = description,
            Message::Model(choice) => self.model = self.model_choice(&choice),
            Message::Thinking(choice) => self.thinking = (choice != THINKING_DEFAULT).then_some(choice),
            Message::Tool(tool, on) => toggle(&mut self.tools_on, tool, on),
            Message::Spawn(agent, on) => toggle(&mut self.spawns_on, agent, on),
            Message::Body(action) => self.body.perform(action),
        }
        if self.show_errors {
            self.check();
        }
        None
    }

    /// The model a picked label stands for.
    fn model_choice(&self, label: &str) -> Option<ModelSpec> {
        if label == MODEL_DEFAULT {
            return None;
        }
        match &self.model {
            Some(current) if model_label(Some(current)) == label => Some(current.clone()),
            _ => Some(ModelSpec::One(label.to_owned())),
        }
    }

    fn model_options(&self) -> Vec<String> {
        let mut options = vec![MODEL_DEFAULT.to_owned()];
        options.extend(self.models.iter().cloned());
        let current = model_label(self.model.as_ref());
        if !options.contains(&current) {
            options.push(current);
        }
        options
    }

    fn errors_for(&self, field: &str) -> Vec<Element<'_, Message>> {
        self.errors.iter().filter(|error| error.field == field).map(|error| text(error.message.as_str()).size(SMALL).style(text::danger).into()).collect()
    }

    /// A labelled field with its errors below.
    fn field<'a>(&'a self, label: &'static str, field: &str, input: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
        let mut block = column![text(label).size(SMALL).style(muted), input.into()].spacing(4);
        for error in self.errors_for(field) {
            block = block.push(error);
        }
        block.into()
    }

    fn checks<'a>(names: &[String], selected: &[String], message: fn(String, bool) -> Message) -> Element<'a, Message> {
        let extras = selected.iter().filter(|item| !names.contains(item));
        column(names.iter().chain(extras).map(|name| {
            let tool = name.clone();
            checkbox(selected.contains(name)).label(name.clone()).text_size(BODY).on_toggle(move |on| message(tool.clone(), on)).into()
        }))
        .spacing(4)
        .into()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let header = row![text("Agents").size(BODY), space::horizontal(), button(text("Close").size(SMALL)).on_press(Message::Close)]
            .align_y(iced::Center);

        let mut list = column![button(text("New agent").size(SMALL)).on_press(Message::New)].spacing(4);
        if self.entries.is_empty() {
            list = list.push(text("No agents yet").size(SMALL).style(muted));
        }
        for (index, entry) in self.entries.iter().enumerate() {
            let tag = match entry.source {
                AgentSource::User => "User",
                AgentSource::Project => "Project",
            };
            let mut line = row![text(entry.name.as_str()).size(BODY), text(tag).size(SMALL).style(muted)].spacing(6).align_y(iced::Center);
            if !entry.enabled {
                line = line.push(text("disabled").size(SMALL).style(muted));
            }
            list = list.push(button(line).width(Fill).style(button::text).on_press(Message::Edit(index)));
        }
        let list = container(scrollable(list).height(Fill)).width(LIST_WIDTH).padding(8);

        let mut form = column![].spacing(12).max_width(720.0);
        if let Some(pending) = &self.pending {
            let yes = match pending {
                Pending::Replace { .. } => "Replace",
                Pending::Delete { .. } => "Delete",
            };
            let question = self.pending_question().unwrap_or_default();
            form = form.push(
                row![
                    text(question).size(BODY),
                    button(text(yes).size(SMALL)).on_press(Message::Confirm),
                    button(text("Cancel").size(SMALL)).style(button::secondary).on_press(Message::Cancel),
                ]
                .spacing(8)
                .align_y(iced::Center),
            );
        }
        if let Some(path) = &self.editing {
            let enabled = self.entries.iter().find(|entry| entry.path == *path).is_none_or(|entry| entry.enabled);
            form = form.push(
                row![
                    button(text(if enabled { "Disable" } else { "Enable" }).size(SMALL)).style(button::secondary).on_press(Message::Toggle),
                    button(text("Delete").size(SMALL)).style(button::danger).on_press(Message::Delete),
                ]
                .spacing(8),
            );
        }
        form = form
            .push(self.field("Template", "template", pick_list(self.templates(), Some(self.template.clone()), Message::Template)))
            .push(self.field("Name", "name", text_input("my-agent", &self.name).on_input(Message::Name).size(BODY)))
            .push(self.field(
                "Description",
                "description",
                text_input("What the agent is for", &self.description).on_input(Message::Description).size(BODY),
            ))
            .push(self.field("Model", "model", pick_list(self.model_options(), Some(model_label(self.model.as_ref())), Message::Model)));
        let mut levels = vec![THINKING_DEFAULT.to_owned()];
        levels.extend(THINKING_LEVELS.iter().map(|level| (*level).to_owned()));
        let thinking = self.thinking.clone().unwrap_or_else(|| THINKING_DEFAULT.to_owned());
        form = form.push(self.field("Thinking level", "thinkingLevel", pick_list(levels, Some(thinking), Message::Thinking)));

        let mut tools = column![Self::checks(&self.tools, &self.tools_on, Message::Tool)].spacing(4);
        if let Some(note) = &self.tools_note {
            tools = tools.push(text(note.as_str()).size(SMALL).style(muted));
        }
        if self.tools_on.is_empty() {
            tools = tools.push(text("None ticked: the agent gets omp's default tools").size(SMALL).style(muted));
        }
        form = form.push(self.field("Tools", "tools", tools));

        let others = self.other_agents();
        let spawns = if others.is_empty() && self.spawns_on.is_empty() {
            text("No other agents to spawn").size(SMALL).style(muted).into()
        } else {
            Self::checks(&others, &self.spawns_on, Message::Spawn)
        };
        form = form.push(self.field("May spawn", "spawns", spawns));

        let editor = text_editor(&self.body).placeholder("The agent's system prompt").on_action(Message::Body).height(Length::Fixed(EDITOR_HEIGHT));
        form = form.push(self.field("Prompt", "body", editor));
        let mut save = row![button(text("Save").size(SMALL)).on_press(Message::Save)].spacing(8);
        for error in self.errors_for("form") {
            save = save.push(error);
        }
        form = form.push(save);

        let form = scrollable(container(form).padding(16)).width(Fill).height(Fill);
        column![container(header).padding([8.0, 12.0]), row![list, form].height(Fill)].into()
    }
}

fn blank_definition() -> AgentDefinition {
    AgentDefinition::new("", "").expect("an empty name and description are valid YAML strings")
}

fn copy_label(name: &str) -> String {
    format!("Copy of {name}")
}

fn model_label(model: Option<&ModelSpec>) -> String {
    match model {
        None => MODEL_DEFAULT.to_owned(),
        Some(spec) => spec.entries().join(", "),
    }
}

fn toggle(list: &mut Vec<String>, item: String, on: bool) {
    if on {
        if !list.contains(&item) {
            list.push(item);
        }
    } else {
        list.retain(|entry| *entry != item);
    }
}

fn muted(theme: &iced::Theme) -> text::Style {
    text::Style { color: Some(theme.extended_palette().background.base.text.scale_alpha(0.6)) }
}
