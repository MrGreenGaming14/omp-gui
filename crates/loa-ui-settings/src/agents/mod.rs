//! The agents screen: the omp agents found on disk on the left, the form that builds one on the
//! right.
//!
//! The form has one field per section of the definition: name, description, model, thinking
//! level, one checkbox per tool, one checkbox per other agent the new agent may spawn, and the
//! prompt body. A template fills it: blank, or a copy of a listed agent. Save hands the
//! [`Draft`] to the app, which checks it, writes it when it has no errors and answers with
//! [`State::set_errors`]; the problems show next to their fields and, from then on, every edit
//! asks the app to check again ([`Intent::Check`]).
//!
//! The app lists the agents ([`State::set_agents`]) and what the form offers
//! ([`State::set_choices`]). Replace and delete ask first; enable and disable act on the agent
//! being edited.

use std::fmt;
use std::path::{Path, PathBuf};

use iced::widget::{Space, column, container, row, scrollable, text_editor};
use iced::{Alignment, Element, Fill};
use loa_ui_kit::style::{self, ButtonKind, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, sp, text_size, width};
use loa_ui_kit::widgets::{Msg, Screen, label, section_header, two_line_row};
use loa_view::settings::agents::{AgentEntry, AgentFields, AgentSource, Choices, Draft, FieldError, Intent, ModelSpec};

use crate::{action, check, errors, field, pick};

#[cfg(test)]
mod tests;

/// The model choice that leaves `model` out: omp picks its default.
pub const MODEL_DEFAULT: &str = "(omp default)";
/// The thinking level choice that leaves `thinkingLevel` out.
pub const THINKING_DEFAULT: &str = "(default)";

#[derive(Clone, Debug)]
pub enum Local {
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
}

pub type Message = Msg<Local, Intent>;

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

/// A question waiting for the user's answer.
#[derive(Clone, Debug)]
enum Pending {
    Replace { draft: Draft, path: PathBuf },
    Delete { name: String, path: PathBuf },
}

#[derive(Debug)]
pub struct State {
    entries: Vec<AgentEntry>,
    choices: Choices,
    /// The listed file the form was filled from; keys the form has no field for stay in it.
    base: Option<PathBuf>,
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
    /// Set by the first Save press; from then on the errors follow the edits.
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
            choices: Choices::default(),
            base: None,
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
    pub fn set_agents(&mut self, entries: Vec<AgentEntry>) {
        self.entries = entries;
        match self.template.index {
            Some(index) if index < self.entries.len() => self.template.label = copy_label(&self.entries[index].fields.name),
            Some(_) => self.template = Template::blank(),
            None => {}
        }
    }

    /// The models, thinking levels and tools the form offers.
    pub fn set_choices(&mut self, choices: Choices) {
        self.choices = choices;
    }

    /// The question waiting for an answer, if any.
    pub fn pending_question(&self) -> Option<String> {
        self.pending.as_ref().map(|pending| match pending {
            Pending::Replace { draft, .. } => format!("Replace {}?", draft.fields.name),
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

    /// Asks whether to replace the agent file at `path` with `draft`.
    pub fn ask_replace(&mut self, draft: Draft, path: PathBuf) {
        self.pending = Some(Pending::Replace { draft, path });
    }

    /// The app's check of the form: the problems by field, none when it passed.
    pub fn set_errors(&mut self, errors: Vec<FieldError>) {
        self.errors = errors;
    }

    /// Shows `message` next to `field` (`"form"` for the top of the form), keeping the form open.
    pub fn set_error(&mut self, field: &'static str, message: String) {
        self.pending = None;
        self.errors.retain(|error| error.field != field);
        self.errors.push(FieldError { field, message });
    }

    /// Starts a blank form.
    pub fn new_agent(&mut self) {
        self.fill(Template::blank(), None, None);
    }

    /// Fills the form from the agent at `index`, unchanged.
    pub fn edit(&mut self, index: usize) {
        if index < self.entries.len() {
            self.fill(Template::blank(), Some(index), None);
            self.editing = Some(self.entries[index].path.clone());
        }
    }

    /// Fills the form with a copy of the agent at `index`: every field and the body, with `-copy`
    /// after the name.
    pub fn copy(&mut self, index: usize) {
        if let Some(entry) = self.entries.get(index) {
            let template = Template { index: Some(index), label: copy_label(&entry.fields.name) };
            let name = format!("{}-copy", entry.fields.name);
            self.fill(template, Some(index), Some(name));
        }
    }

    /// The agents' names in list order.
    pub fn agent_names(&self) -> Vec<&str> {
        self.entries.iter().map(|entry| entry.fields.name.as_str()).collect()
    }

    /// The templates the picker offers.
    pub fn templates(&self) -> Vec<Template> {
        std::iter::once(Template::blank())
            .chain(self.entries.iter().enumerate().map(|(index, entry)| Template { index: Some(index), label: copy_label(&entry.fields.name) }))
            .collect()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The problems the app's last check found, by field.
    pub fn errors(&self) -> &[FieldError] {
        &self.errors
    }

    /// The fields the form holds now.
    pub fn fields(&self) -> AgentFields {
        AgentFields {
            name: self.name.trim().to_owned(),
            description: self.description.trim().to_owned(),
            model: self.model.clone(),
            thinking_level: self.thinking.clone(),
            tools: ordered(&self.tools_on, &self.choices.tools),
            spawns: ordered(&self.spawns_on, &self.other_agents()),
            body: self.body.text(),
        }
    }

    /// The form as the app writes it.
    pub fn draft(&self) -> Draft {
        Draft { fields: self.fields(), base: self.base.clone(), editing: self.editing.clone() }
    }

    fn fill(&mut self, template: Template, base: Option<usize>, name: Option<String>) {
        let entry = base.and_then(|index| self.entries.get(index));
        let fields = entry.map(|entry| entry.fields.clone()).unwrap_or_default();
        self.base = entry.map(|entry| entry.path.clone());
        self.template = template;
        self.name = name.unwrap_or(fields.name);
        self.description = fields.description;
        self.model = fields.model;
        self.thinking = fields.thinking_level;
        self.tools_on = fields.tools.unwrap_or_default();
        self.spawns_on = fields.spawns.unwrap_or_default();
        self.body = text_editor::Content::with_text(&fields.body);
        self.errors.clear();
        self.show_errors = false;
        self.editing = None;
        self.pending = None;
    }

    /// The other known agents, deduplicated, in list order.
    fn other_agents(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for entry in &self.entries {
            let name = &entry.fields.name;
            if name != self.name.trim() && !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
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
        options.extend(self.choices.models.iter().cloned());
        let current = model_label(self.model.as_ref());
        if !options.contains(&current) {
            options.push(current);
        }
        options
    }

    fn errors_for(&self, field: &str) -> impl Iterator<Item = Element<'_, Message>> {
        self.errors.iter().filter(move |error| error.field == field).map(|error| label(error.message.as_str(), TextRole::Caption, Tone::Danger).into())
    }

    /// A labelled field with its errors below.
    fn field<'a>(&'a self, name: &'static str, field: &str, input: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
        column![label(name, TextRole::Label, Tone::Muted), input.into()].extend(self.errors_for(field)).spacing(sp(SpaceRole::Xs)).into()
    }

    fn checks<'a>(names: &[String], selected: &[String], message: fn(String, bool) -> Local) -> Element<'a, Message> {
        let extras = selected.iter().filter(|item| !names.contains(item));
        column(names.iter().chain(extras).map(|name| {
            let item = name.clone();
            check(selected.contains(name), name.clone(), move |on| Msg::Local(message(item.clone(), on)))
        }))
        .spacing(sp(SpaceRole::Xs))
        .into()
    }

    /// The screen with its own title and Close, for the full center.
    pub fn view(&self) -> Element<'_, Message> {
        let close = action("Close", ButtonKind::Ghost, Some(Msg::Emit(Intent::Close))).into();
        column![section_header("Agents", [close]), self.body()].spacing(sp(SpaceRole::M)).padding(sp(SpaceRole::M)).into()
    }

    /// The agent list and the form, for a frame that has its own title.
    pub fn body(&self) -> Element<'_, Message> {
        let mut list = column![action("New agent", ButtonKind::Secondary, Some(Msg::Local(Local::New)))].spacing(sp(SpaceRole::Xs));
        if self.entries.is_empty() {
            list = list.push(label("No agents yet", TextRole::Label, Tone::Muted));
        }
        for (index, entry) in self.entries.iter().enumerate() {
            let source = match entry.source {
                AgentSource::User => "User",
                AgentSource::Project => "Project",
            };
            let detail = if entry.enabled { source.to_owned() } else { format!("{source}  ·  disabled") };
            let selected = self.editing.as_ref() == Some(&entry.path);
            list = list.push(two_line_row(entry.fields.name.as_str(), detail, selected, Msg::Local(Local::Edit(index))));
        }
        let list = container(scrollable(list).height(Fill)).width(width(LayoutRole::NavWidth));
        let form = scrollable(container(self.form()).padding([0.0, sp(SpaceRole::M)])).style(style::scrollbar(false)).width(Fill).height(Fill);
        row![list, form].spacing(sp(SpaceRole::L)).height(Fill).into()
    }

    fn form(&self) -> Element<'_, Message> {
        let mut form = column![].spacing(sp(SpaceRole::M)).max_width(width(LayoutRole::ContentWidth));
        if let Some(pending) = &self.pending {
            let yes = match pending {
                Pending::Replace { .. } => "Replace",
                Pending::Delete { .. } => "Delete",
            };
            let question = label(self.pending_question().unwrap_or_default(), TextRole::Body, Tone::Normal);
            let yes = action(yes, ButtonKind::Danger, Some(Msg::Local(Local::Confirm)));
            let no = action("Cancel", ButtonKind::Secondary, Some(Msg::Local(Local::Cancel)));
            form = form.push(row![question, yes, no].spacing(sp(SpaceRole::S)).align_y(Alignment::Center));
        }
        if let Some(path) = &self.editing {
            let enabled = self.is_enabled(path).unwrap_or(true);
            let toggle = action(if enabled { "Disable" } else { "Enable" }, ButtonKind::Secondary, Some(Msg::Local(Local::Toggle)));
            let delete = action("Delete", ButtonKind::Danger, Some(Msg::Local(Local::Delete)));
            form = form.push(row![toggle, delete].spacing(sp(SpaceRole::S)));
        }
        let form_errors: Vec<String> = self.errors.iter().filter(|error| error.field == "form").map(|error| error.message.clone()).collect();
        if !form_errors.is_empty() {
            form = form.push(errors(form_errors));
        }
        form = form
            .push(self.field("Template", "template", pick(self.templates(), Some(self.template.clone()), "Blank", |t| Msg::Local(Local::Template(t)))))
            .push(self.field("Name", "name", field("my-agent", &self.name, |n| Msg::Local(Local::Name(n)))))
            .push(self.field("Description", "description", field("What the agent is for", &self.description, |d| Msg::Local(Local::Description(d)))))
            .push(self.field("Model", "model", pick(self.model_options(), Some(model_label(self.model.as_ref())), MODEL_DEFAULT, |m| Msg::Local(Local::Model(m)))));
        let mut levels = vec![THINKING_DEFAULT.to_owned()];
        levels.extend(self.choices.thinking_levels.iter().cloned());
        let thinking = self.thinking.clone().unwrap_or_else(|| THINKING_DEFAULT.to_owned());
        form = form.push(self.field("Thinking level", "thinkingLevel", pick(levels, Some(thinking), THINKING_DEFAULT, |t| Msg::Local(Local::Thinking(t)))));

        let mut tools = column![Self::checks(&self.choices.tools, &self.tools_on, Local::Tool)].spacing(sp(SpaceRole::Xs));
        if let Some(note) = &self.choices.tools_note {
            tools = tools.push(label(note.as_str(), TextRole::Caption, Tone::Muted));
        }
        if self.tools_on.is_empty() {
            tools = tools.push(label("None ticked: the agent gets omp's default tools", TextRole::Caption, Tone::Muted));
        }
        form = form.push(self.field("Tools", "tools", tools));

        let others = self.other_agents();
        let spawns = if others.is_empty() && self.spawns_on.is_empty() {
            label("No other agents to spawn", TextRole::Caption, Tone::Muted).into()
        } else {
            Self::checks(&others, &self.spawns_on, Local::Spawn)
        };
        form = form.push(self.field("May spawn", "spawns", spawns));

        let editor = text_editor(&self.body).placeholder("The agent's system prompt").on_action(|a| Msg::Local(Local::Body(a)));
        let editor = editor.size(text_size(TextRole::Body)).padding(sp(SpaceRole::S)).height(width(LayoutRole::ListWidth)).style(style::editor);
        form = form.push(self.field("Prompt", "body", editor));
        let save = action("Save", ButtonKind::Primary, Some(Msg::Local(Local::Save)));
        form.push(row![save, Space::new().width(Fill)].align_y(Alignment::Center)).into()
    }
}

impl Screen for State {
    type Local = Local;
    type Event = Intent;

    fn reduce(&mut self, m: Local) -> Option<Intent> {
        match m {
            Local::Save => {
                self.pending = None;
                self.show_errors = true;
                return Some(Intent::Save(self.draft()));
            }
            Local::Toggle => return self.editing.clone().map(Intent::Toggle),
            Local::Delete => {
                if let Some(path) = self.editing.clone() {
                    self.pending = Some(Pending::Delete { name: self.name.clone(), path });
                }
                return None;
            }
            Local::Cancel => {
                self.pending = None;
                return None;
            }
            Local::Confirm => {
                return match self.pending.take()? {
                    Pending::Replace { draft, path } => Some(Intent::Replace(draft, path)),
                    Pending::Delete { path, .. } => Some(Intent::Delete(path)),
                };
            }
            Local::Template(template) => match template.index {
                Some(index) => self.copy(index),
                None => self.new_agent(),
            },
            Local::New => self.new_agent(),
            Local::Edit(index) => self.edit(index),
            Local::Name(name) => self.name = name,
            Local::Description(description) => self.description = description,
            Local::Model(choice) => self.model = self.model_choice(&choice),
            Local::Thinking(choice) => self.thinking = (choice != THINKING_DEFAULT).then_some(choice),
            Local::Tool(tool, on) => toggle(&mut self.tools_on, tool, on),
            Local::Spawn(agent, on) => toggle(&mut self.spawns_on, agent, on),
            Local::Body(action) => self.body.perform(action),
        }
        self.show_errors.then(|| Intent::Check(self.draft()))
    }

    /// The view emits only Close directly; everything else goes through the reducer.
    fn guard(&self, e: &Intent) -> bool {
        matches!(e, Intent::Close)
    }
}

/// `selected` in the order of `known`, unknown entries last; `None` when nothing is selected (the
/// key is left out and omp's default applies).
fn ordered(selected: &[String], known: &[String]) -> Option<Vec<String>> {
    let mut out: Vec<String> = known.iter().filter(|item| selected.contains(item)).cloned().collect();
    out.extend(selected.iter().filter(|item| !known.contains(item)).cloned());
    (!out.is_empty()).then_some(out)
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
