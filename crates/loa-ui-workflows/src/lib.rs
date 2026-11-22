//! The workflows screen: the workflows on the left, the editor for one on
//! the right.
//!
//! The editor has the workflow's name, one three-way choice per known agent
//! (allow, disallow, not set) and optional notes. Workflows written by hand
//! in `RULES.md` are listed too but their fields are read-only.
//! [`State::update`] returns [`Event::Save`] only when the draft is valid:
//! the name is not empty and no agent is both allowed and disallowed.
//!
//! The app hands the workflows and agents over ([`State::set_data`]) and
//! acts on the events. The crate knows nothing of how they are stored.

use iced::widget::{button, column, container, radio, row, scrollable, space, text, text_editor, text_input, tooltip};
use iced::{Element, Fill, Length};
use loa_ui_icons::{Icon, icon, muted as muted_icon};

#[cfg(test)]
mod tests;

const BODY: f32 = 14.0;
const SMALL: f32 = 12.0;
const ICON: f32 = 14.0;
const LIST_WIDTH: f32 = 260.0;
const NOTES_HEIGHT: f32 = 160.0;

/// One workflow, as the app stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The workflow's role. In [`Event::Save`] it is the role being edited,
    /// empty for a new workflow; the app derives the final role from the
    /// name.
    pub role: String,
    pub name: String,
    pub allowed: Vec<String>,
    pub disallowed: Vec<String>,
    pub notes: String,
    /// False for workflows written by hand in `RULES.md`.
    pub editable: bool,
}

/// An agent the workflow can allow or disallow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentInfo {
    pub name: String,
    pub enabled: bool,
}

/// What a workflow says about one agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Allow,
    Disallow,
    Unspecified,
}

impl Access {
    const ALL: [Access; 3] = [Access::Allow, Access::Disallow, Access::Unspecified];

    fn label(self) -> &'static str {
        match self {
            Access::Allow => "Allow",
            Access::Disallow => "Disallow",
            Access::Unspecified => "Not set",
        }
    }

    /// The radio's contextual label, e.g. "Allow a": what it does to which
    /// agent, for screen readers and the tooltip.
    pub fn describe(self, agent: &str) -> String {
        format!("{} {agent}", self.label())
    }
}

/// What the screen asks the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The draft validated: write it. `previous_role` is `None` for a new
    /// workflow.
    Save { previous_role: Option<String>, entry: Entry },
    /// The user confirmed deleting the workflow.
    Delete { role: String },
    Close,
}

#[derive(Clone, Debug)]
pub enum Message {
    /// A workflow of the list was picked, by role.
    Select(String),
    New,
    Name(String),
    Access(String, Access),
    Notes(text_editor::Action),
    Save,
    /// Asks first; pressed again it deletes.
    Delete,
    CancelDelete,
    Close,
}

pub struct State {
    workflows: Vec<Entry>,
    agents: Vec<AgentInfo>,
    /// The role the editor edits; `None` for a new workflow.
    selected: Option<String>,
    name: String,
    allowed: Vec<String>,
    disallowed: Vec<String>,
    notes: text_editor::Content,
    editable: bool,
    confirming_delete: bool,
    error: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        Self {
            workflows: Vec::new(),
            agents: Vec::new(),
            selected: None,
            name: String::new(),
            allowed: Vec::new(),
            disallowed: Vec::new(),
            notes: text_editor::Content::new(),
            editable: true,
            confirming_delete: false,
            error: None,
        }
    }

    /// The workflows and agents to show. The selection and the draft stay
    /// when the selected role is still listed; otherwise the editor starts
    /// a new workflow.
    pub fn set_data(&mut self, workflows: Vec<Entry>, agents: Vec<AgentInfo>) {
        self.workflows = workflows;
        self.agents = agents;
        if let Some(role) = self.selected.clone() {
            match self.workflows.iter().find(|entry| entry.role == role) {
                Some(entry) => self.editable = entry.editable,
                None => self.edit(None),
            }
        }
    }

    /// Fills the editor from the workflow with `role`, or starts a new one
    /// when `role` is `None` or unknown.
    pub fn edit(&mut self, role: Option<&str>) {
        self.error = None;
        self.confirming_delete = false;
        match role.and_then(|role| self.workflows.iter().find(|entry| entry.role == role)) {
            Some(entry) => {
                self.selected = Some(entry.role.clone());
                self.name = entry.name.clone();
                self.allowed = entry.allowed.clone();
                self.disallowed = entry.disallowed.clone();
                self.notes = text_editor::Content::with_text(&entry.notes);
                self.editable = entry.editable;
            }
            None => {
                self.selected = None;
                self.name.clear();
                self.allowed.clear();
                self.disallowed.clear();
                self.notes = text_editor::Content::new();
                self.editable = true;
            }
        }
    }

    /// Shows `error` at the top of the editor, or clears it.
    pub fn set_error(&mut self, error: Option<String>) {
        self.error = error;
    }

    /// The role being edited; `None` for a new workflow.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// What the draft says about `agent`.
    pub fn access(&self, agent: &str) -> Access {
        if self.allowed.iter().any(|name| name == agent) {
            Access::Allow
        } else if self.disallowed.iter().any(|name| name == agent) {
            Access::Disallow
        } else {
            Access::Unspecified
        }
    }

    /// The first problem with the draft.
    fn problem(&self) -> Option<String> {
        if self.name.trim().is_empty() {
            return Some("Give the workflow a name.".to_owned());
        }
        self.allowed.iter().find(|name| self.disallowed.contains(name)).map(|name| format!("{name} cannot be both allowed and disallowed."))
    }

    pub fn update(&mut self, message: Message) -> Option<Event> {
        match message {
            Message::Close => return Some(Event::Close),
            Message::Select(role) => self.edit(Some(&role)),
            Message::New => self.edit(None),
            Message::Delete => {
                if !self.editable {
                    return None;
                }
                let role = self.selected.clone()?;
                if self.confirming_delete {
                    self.confirming_delete = false;
                    return Some(Event::Delete { role });
                }
                self.confirming_delete = true;
            }
            Message::CancelDelete => self.confirming_delete = false,
            Message::Save => {
                if !self.editable {
                    return None;
                }
                if let Some(problem) = self.problem() {
                    self.error = Some(problem);
                    return None;
                }
                self.error = None;
                self.confirming_delete = false;
                return Some(Event::Save {
                    previous_role: self.selected.clone(),
                    entry: Entry {
                        role: self.selected.clone().unwrap_or_default(),
                        name: self.name.trim().to_owned(),
                        allowed: self.allowed.clone(),
                        disallowed: self.disallowed.clone(),
                        notes: self.notes_text(),
                        editable: true,
                    },
                });
            }
            Message::Name(name) if self.editable => self.name = name,
            Message::Access(agent, access) if self.editable => {
                self.allowed.retain(|name| *name != agent);
                self.disallowed.retain(|name| *name != agent);
                match access {
                    Access::Allow => self.allowed.push(agent),
                    Access::Disallow => self.disallowed.push(agent),
                    Access::Unspecified => {}
                }
            }
            Message::Notes(action) if self.editable || !action.is_edit() => self.notes.perform(action),
            Message::Name(_) | Message::Access(..) | Message::Notes(_) => {}
        }
        None
    }

    /// The notes without the trailing newline the editor adds.
    fn notes_text(&self) -> String {
        self.notes.text().trim_end_matches('\n').to_owned()
    }

    /// The agents to list: the known ones, then any the workflow names that
    /// are not known (removed since).
    fn rows(&self) -> Vec<(String, bool)> {
        let mut rows: Vec<(String, bool)> = self.agents.iter().map(|agent| (agent.name.clone(), agent.enabled)).collect();
        for name in self.allowed.iter().chain(&self.disallowed) {
            if !rows.iter().any(|(known, _)| known == name) {
                rows.push((name.clone(), true));
            }
        }
        rows
    }

    pub fn view(&self) -> Element<'_, Message> {
        let header = row![text("Workflows").size(BODY), space::horizontal(), button(row![icon(Icon::X, ICON), text("Close").size(SMALL)].spacing(4).align_y(iced::Center)).on_press(Message::Close)]
            .align_y(iced::Center);

        let mut list = column![button(row![icon(Icon::Plus, ICON), text("New workflow").size(SMALL)].spacing(4).align_y(iced::Center)).on_press(Message::New)].spacing(4);
        if self.workflows.is_empty() {
            list = list.push(text("No workflows yet").size(SMALL).style(muted));
        }
        for entry in &self.workflows {
            let mut line = column![text(entry.name.as_str()).size(BODY)].spacing(2);
            if !entry.editable {
                line = line.push(text("read-only, from RULES.md").size(SMALL).style(muted));
            }
            let style = if self.selected.as_deref() == Some(entry.role.as_str()) { button::secondary } else { button::text };
            list = list.push(button(line).width(Fill).style(style).on_press(Message::Select(entry.role.clone())));
        }
        let list = container(scrollable(list).height(Fill)).width(LIST_WIDTH).padding(8);

        let mut form = column![].spacing(12).max_width(720.0);
        if let Some(error) = &self.error {
            form = form.push(row![muted_icon(Icon::TriangleAlert, ICON), text(error.as_str()).size(BODY).style(text::danger)].spacing(6).align_y(iced::Center));
        }
        if !self.editable {
            form = form.push(text("This workflow is written by hand in RULES.md. Edit it there.").size(SMALL).style(muted));
        }
        let name = text_input("Research", &self.name).size(BODY);
        let name = if self.editable { name.on_input(Message::Name) } else { name };
        form = form.push(column![text("Name").size(SMALL).style(muted), name].spacing(4));

        let mut agents = column![].spacing(6);
        let rows = self.rows();
        if rows.is_empty() {
            agents = agents.push(text("No agents known").size(SMALL).style(muted));
        }
        for (agent, enabled) in rows {
            let label = if enabled { agent.clone() } else { format!("{agent} (disabled)") };
            let current = self.access(&agent);
            let choice: Element<'_, Message> = if self.editable {
                row(Access::ALL.map(|access| {
                    let name = agent.clone();
                    let radio = radio(access.label(), access, Some(current), move |access| Message::Access(name.clone(), access)).text_size(SMALL).size(16);
                    // The visible text stays short; the tooltip (and the accessible
                    // name once iced exposes one) says which agent it is for.
                    let tip = container(text(access.describe(&agent)).size(SMALL)).padding(4).style(container::rounded_box);
                    tooltip(radio, tip, tooltip::Position::Top).into()
                }))
                .spacing(12)
                .into()
            } else {
                text(current.label()).size(SMALL).into()
            };
            agents = agents.push(row![text(label).size(BODY).width(Length::FillPortion(2)), container(choice).width(Length::FillPortion(3))].spacing(8).align_y(iced::Center));
        }
        form = form.push(column![text("Agents").size(SMALL).style(muted), agents].spacing(4));

        let notes = text_editor(&self.notes).placeholder("Notes added to the workflow's section").height(Length::Fixed(NOTES_HEIGHT));
        let notes = if self.editable { notes.on_action(Message::Notes) } else { notes };
        form = form.push(column![text("Notes").size(SMALL).style(muted), notes].spacing(4));

        if self.editable {
            let mut actions = row![button(row![icon(Icon::Check, ICON), text("Save").size(SMALL)].spacing(4).align_y(iced::Center)).on_press(Message::Save)].spacing(8).align_y(iced::Center);
            if self.selected.is_some() {
                if self.confirming_delete {
                    actions = actions
                        .push(button(text("Confirm delete").size(SMALL)).style(button::danger).on_press(Message::Delete))
                        .push(button(text("Cancel").size(SMALL)).style(button::secondary).on_press(Message::CancelDelete));
                } else {
                    actions = actions.push(button(text("Delete").size(SMALL)).style(button::danger).on_press(Message::Delete));
                }
            }
            form = form.push(actions);
        }

        let form = scrollable(container(form).padding(16)).width(Fill).height(Fill);
        column![container(header).padding([8.0, 12.0]), row![list, form].height(Fill)].into()
    }
}

fn muted(theme: &iced::Theme) -> text::Style {
    text::Style { color: Some(theme.extended_palette().background.base.text.scale_alpha(0.6)) }
}
