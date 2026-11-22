//! Workflow list and editor. File access and validation belong to the app.

use iced::widget::{column, container, radio, row, scrollable, text_editor, tooltip};
use iced::{Alignment, Element, Fill};
use loa_ui_kit::style::{self, ButtonKind, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, sp, text_size, width};
use loa_ui_kit::widgets::{Msg, Screen, label, section_header, two_line_row};
use loa_view::settings::workflows::{AgentInfo, Draft, Error, FieldError, Intent, Rules, Workflow};
use crate::{action, errors, field};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access { Allow, Disallow, Unspecified }
impl Access {
    fn label(self) -> &'static str { match self { Self::Allow => "Allow", Self::Disallow => "Disallow", Self::Unspecified => "Not set" } }
}

#[derive(Clone, Debug)]
pub enum Local {
    Open(String), New, Role(String), Name(String), Models(String), Access(String, Access),
    Text(text_editor::Action), Save, Delete, ConfirmDelete, CancelDelete,
}
pub type Message = Msg<Local, Intent>;

#[derive(Debug)]
pub struct State {
    entries: Vec<Workflow>, agents: Vec<AgentInfo>, previous_role: Option<String>,
    role: String, models: String, rules: Rules, text: text_editor::Content, text_dirty: bool,
    errors: Vec<FieldError>, error: Option<Error>, confirming_delete: bool,
}
impl Default for State { fn default() -> Self { Self::new() } }
impl State {
    pub fn new() -> Self {
        Self { entries: Vec::new(), agents: Vec::new(), previous_role: None, role: String::new(), models: String::new(), rules: Rules::Managed { name: String::new(), allowed: Vec::new(), disallowed: Vec::new(), notes: String::new() }, text: text_editor::Content::new(), text_dirty: false, errors: Vec::new(), error: None, confirming_delete: false }
    }
    pub fn set_workflows(&mut self, entries: Vec<Workflow>) { self.entries = entries; }
    pub fn set_agents(&mut self, agents: Vec<AgentInfo>) { self.agents = agents; }
    pub fn set_errors(&mut self, errors: Vec<FieldError>) { self.errors = errors; }
    pub fn set_error(&mut self, error: Option<Error>) { self.error = error; }
    pub fn selected(&self) -> Option<&str> { self.previous_role.as_deref() }
    pub fn new_workflow(&mut self) {
        self.previous_role = None; self.role.clear(); self.models.clear();
        self.rules = Rules::Managed { name: String::new(), allowed: Vec::new(), disallowed: Vec::new(), notes: String::new() };
        self.text = text_editor::Content::new(); self.text_dirty = false; self.errors.clear(); self.error = None; self.confirming_delete = false;
    }
    pub fn edit(&mut self, role: &str) -> bool {
        let Some(entry) = self.entries.iter().find(|entry| entry.role == role).cloned() else { return false };
        self.previous_role = Some(entry.role.clone()); self.role = entry.role; self.models = entry.models.join(", ");
        self.text = text_editor::Content::with_text(match &entry.rules { Rules::Managed { notes, .. } => notes, Rules::HandWritten { text } => text, Rules::None => "" });
        self.rules = entry.rules; self.text_dirty = false; self.errors.clear(); self.error = None; self.confirming_delete = false; true
    }
    pub fn draft(&self) -> Draft {
        let mut rules = self.rules.clone();
        if self.text_dirty {
            match &mut rules { Rules::Managed { notes, .. } => *notes = self.text.text(), Rules::HandWritten { text } => *text = self.text.text(), Rules::None => {} }
        }
        Draft { workflow: Workflow { role: self.role.trim().to_owned(), models: self.models.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect(), rules } }
    }
    pub fn access(&self, agent: &str) -> Access {
        match &self.rules { Rules::Managed { allowed, .. } if allowed.iter().any(|s| s == agent) => Access::Allow, Rules::Managed { disallowed, .. } if disallowed.iter().any(|s| s == agent) => Access::Disallow, _ => Access::Unspecified }
    }
    pub fn view(&self) -> Element<'_, Message> {
        let close = action("Close", ButtonKind::Ghost, Some(Msg::Emit(Intent::Close))).into();
        column![section_header("Workflows", [close]), self.body()].spacing(sp(SpaceRole::M)).padding(sp(SpaceRole::M)).into()
    }
    pub fn body(&self) -> Element<'_, Message> {
        let mut list = column![action("New workflow", ButtonKind::Secondary, Some(Msg::Local(Local::New)))].spacing(sp(SpaceRole::Xs));
        for entry in &self.entries {
            // Sections run long: the row shows their first line.
            let first = |text: &str| text.lines().find(|line| !line.trim().is_empty()).unwrap_or_default().trim().to_owned();
            let preview = match &entry.rules { Rules::None => "No rules in RULES.md for this role".to_owned(), Rules::HandWritten { text } => first(text), Rules::Managed { name, allowed, disallowed, notes } => format!("{name} · Allow: {} · Disallow: {} · {}", allowed.join(", "), disallowed.join(", "), first(notes)) };
            let detail = format!("{} · {}", entry.models.join(", "), preview);
            list = list.push(two_line_row(entry.role.as_str(), detail, self.selected() == Some(entry.role.as_str()), Msg::Local(Local::Open(entry.role.clone()))));
        }
        let list = container(scrollable(list).height(Fill)).width(width(LayoutRole::ListWidth));
        let form = scrollable(container(self.form()).padding(sp(SpaceRole::M))).style(style::scrollbar(false)).width(Fill).height(Fill);
        row![list, form].spacing(sp(SpaceRole::L)).height(Fill).into()
    }
    fn form(&self) -> Element<'_, Message> {
        let mut form = column![].spacing(sp(SpaceRole::M)).max_width(width(LayoutRole::ContentWidth));
        if let Some(error) = &self.error {
            let message = match error { Error::ChangedOnDisk => "Changed outside Loa — reload before saving".to_owned(), Error::Message(message) => message.clone() };
            form = form.push(errors([message]));
            if matches!(error, Error::ChangedOnDisk) { form = form.push(action("Reload", ButtonKind::Secondary, Some(Msg::Emit(Intent::Reload)))); }
        }
        if !self.errors.is_empty() { form = form.push(errors(self.errors.iter().map(|e| format!("{}: {}", e.field, e.message)))); }
        form = form.push(label("Role", TextRole::Label, Tone::Muted)).push(field("ROLE", &self.role, |s| Msg::Local(Local::Role(s))));
        form = form.push(label("Models (comma separated)", TextRole::Label, Tone::Muted)).push(field("provider/model", &self.models, |s| Msg::Local(Local::Models(s))));
        if let Rules::Managed { name, allowed, disallowed, .. } = &self.rules {
            form = form.push(label("Name", TextRole::Label, Tone::Muted)).push(field("Research", name, |s| Msg::Local(Local::Name(s))));
            let mut agents = self.agents.clone();
            for name in allowed.iter().chain(disallowed) { if !agents.iter().any(|a| &a.name == name) { agents.push(AgentInfo { name: name.clone(), enabled: false }); } }
            form = form.push(label("Agents", TextRole::Label, Tone::Muted));
            for agent in agents {
                let caption = if agent.enabled { agent.name.clone() } else { format!("{} (disabled)", agent.name) };
                let mut choices = row![label(caption, TextRole::Body, Tone::Normal)].spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
                for access in [Access::Allow, Access::Disallow, Access::Unspecified] {
                    let name = agent.name.clone();
                    let control = radio(access.label(), access, Some(self.access(&name)), move |choice| Msg::Local(Local::Access(name.clone(), choice))).text_size(text_size(TextRole::Label));
                    let tip = container(label(format!("{} {}", access.label(), agent.name), TextRole::Caption, Tone::Normal)).padding(sp(SpaceRole::Xs)).style(style::container(loa_ui_kit::style::Surface::Popup));
                    choices = choices.push(tooltip(control, tip, tooltip::Position::Top));
                }
                form = form.push(choices);
            }
        }
        if !matches!(self.rules, Rules::None) {
            let title = if matches!(self.rules, Rules::HandWritten { .. }) { "Rules markdown" } else { "Notes" };
            let editor = text_editor(&self.text).on_action(|a| Msg::Local(Local::Text(a))).size(text_size(TextRole::Body)).padding(sp(SpaceRole::S)).height(width(LayoutRole::ListWidth)).style(style::editor);
            form = form.push(label(title, TextRole::Label, Tone::Muted)).push(editor);
        } else { form = form.push(label("No rules in RULES.md for this role", TextRole::Label, Tone::Muted)); }
        let mut actions = row![action("Save", ButtonKind::Primary, Some(Msg::Local(Local::Save)))].spacing(sp(SpaceRole::S));
        if self.previous_role.is_some() {
            if self.confirming_delete { actions = actions.push(action("Confirm delete", ButtonKind::Danger, Some(Msg::Local(Local::ConfirmDelete)))).push(action("Cancel", ButtonKind::Secondary, Some(Msg::Local(Local::CancelDelete)))); }
            else { actions = actions.push(action("Delete", ButtonKind::Danger, Some(Msg::Local(Local::Delete)))); }
        }
        form.push(actions).into()
    }
}
impl Screen for State {
    type Local = Local; type Event = Intent;
    fn reduce(&mut self, message: Local) -> Option<Intent> {
        match message {
            Local::Open(role) => { if self.edit(&role) { return Some(Intent::Open(role)); } }
            Local::New => { self.new_workflow(); return Some(Intent::New); }
            Local::Role(role) => self.role = role,
            Local::Models(models) => self.models = models,
            Local::Name(value) => if let Rules::Managed { name, .. } = &mut self.rules { *name = value; },
            Local::Access(agent, access) => if let Rules::Managed { allowed, disallowed, .. } = &mut self.rules {
                allowed.retain(|s| s != &agent); disallowed.retain(|s| s != &agent);
                match access { Access::Allow => allowed.push(agent), Access::Disallow => disallowed.push(agent), Access::Unspecified => {} }
            },
            Local::Text(action) => { self.text_dirty |= action.is_edit(); self.text.perform(action); }
            Local::Save => return Some(Intent::Save { draft: self.draft(), previous_role: self.previous_role.clone() }),
            Local::Delete => self.confirming_delete = self.previous_role.is_some(),
            Local::CancelDelete => self.confirming_delete = false,
            Local::ConfirmDelete => { if self.confirming_delete { self.confirming_delete = false; return self.previous_role.clone().map(Intent::Delete); } }
        }
        None
    }
    fn guard(&self, event: &Intent) -> bool { matches!(event, Intent::Close) || matches!(event, Intent::Reload) && matches!(self.error, Some(Error::ChangedOnDisk)) }
}
