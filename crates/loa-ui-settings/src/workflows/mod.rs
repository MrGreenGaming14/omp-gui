//! Workflow list and editor. File access and validation belong to the app.

use iced::widget::{column, container, radio, row, scrollable, text_editor, tooltip};
use iced::{Alignment, Element, Fill};
use loa_ui_kit::style::{self, ButtonKind, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, sp, text_size, width};
use loa_ui_kit::widgets::{Msg, Screen, label, section_header, two_line_row};
use loa_view::settings::workflows::{AgentInfo, Draft, Error, FieldError, Intent, ManagedFields, RoutingNote, RuleBlock, Rules, Workflow};
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
    Description(String), Routing(String, String), AddStep, Step(usize, String), RemoveStep(usize), MoveStepUp(usize), MoveStepDown(usize),
    AddBlock, BlockHeading(usize, String), BlockText(usize, text_editor::Action), RemoveBlock(usize), CopyFrom(String),
    Text(text_editor::Action), Save, Delete, ConfirmDelete, CancelDelete,
}
pub type Message = Msg<Local, Intent>;

#[derive(Debug, Default)]
struct BlockEditor {
    content: text_editor::Content,
    dirty: bool,
}

#[derive(Debug)]
pub struct State {
    entries: Vec<Workflow>, agents: Vec<AgentInfo>, previous_role: Option<String>,
    role: String, models: String, rules: Rules, text: text_editor::Content, text_dirty: bool,
    block_text: Vec<BlockEditor>,
    errors: Vec<FieldError>, error: Option<Error>, confirming_delete: bool,
}
impl Default for State { fn default() -> Self { Self::new() } }
impl State {
    pub fn new() -> Self {
        Self { entries: Vec::new(), agents: Vec::new(), previous_role: None, role: String::new(), models: String::new(), rules: Rules::Managed { name: String::new(), allowed: Vec::new(), disallowed: Vec::new(), notes: String::new(), fields: ManagedFields::default() }, text: text_editor::Content::new(), text_dirty: false, block_text: Vec::new(), errors: Vec::new(), error: None, confirming_delete: false }
    }
    pub fn set_workflows(&mut self, entries: Vec<Workflow>) { self.entries = entries; }
    pub fn set_agents(&mut self, agents: Vec<AgentInfo>) { self.agents = agents; }
    pub fn set_errors(&mut self, errors: Vec<FieldError>) { self.errors = errors; }
    pub fn set_error(&mut self, error: Option<Error>) { self.error = error; }
    pub fn selected(&self) -> Option<&str> { self.previous_role.as_deref() }
    pub fn new_workflow(&mut self) {
        self.previous_role = None; self.role.clear(); self.models.clear();
        self.rules = Rules::Managed { name: String::new(), allowed: Vec::new(), disallowed: Vec::new(), notes: String::new(), fields: ManagedFields::default() };
        self.text = text_editor::Content::new(); self.text_dirty = false; self.block_text.clear(); self.errors.clear(); self.error = None; self.confirming_delete = false;
    }
    pub fn edit(&mut self, role: &str) -> bool {
        let Some(entry) = self.entries.iter().find(|entry| entry.role == role).cloned() else { return false };
        self.previous_role = Some(entry.role.clone()); self.role = entry.role; self.models = entry.models.join(", ");
        self.set_rules(entry.rules);
        self.errors.clear(); self.error = None; self.confirming_delete = false; true
    }
    fn set_rules(&mut self, rules: Rules) {
        self.text = text_editor::Content::with_text(match &rules {
            Rules::Managed { notes, .. } => notes,
            Rules::HandWritten { text } | Rules::External { text } => text,
            Rules::None => "",
        });
        self.block_text = match &rules {
            Rules::Managed { fields, .. } => fields.blocks.iter().map(|block| BlockEditor { content: text_editor::Content::with_text(&block.text), dirty: false }).collect(),
            _ => Vec::new(),
        };
        self.rules = rules;
        self.text_dirty = false;
    }
    pub fn draft(&self) -> Draft {
        let mut rules = self.rules.clone();
        if self.text_dirty {
            match &mut rules { Rules::Managed { notes, .. } => *notes = self.text.text(), Rules::HandWritten { text } | Rules::External { text } => *text = self.text.text(), Rules::None => {} }
        }
        if let Rules::Managed { fields, .. } = &mut rules {
            for (block, editor) in fields.blocks.iter_mut().zip(&self.block_text) {
                if editor.dirty { block.text = editor.content.text(); }
            }
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
            let preview = match &entry.rules { Rules::None => "No rules in RULES.md for this role".to_owned(), Rules::HandWritten { text } | Rules::External { text } => first(text), Rules::Managed { name, allowed, disallowed, notes, .. } => format!("{name} · Allow: {} · Disallow: {} · {}", allowed.join(", "), disallowed.join(", "), first(notes)) };
            let detail = format!("{} · {}", entry.models.join(", "), preview);
            list = list.push(two_line_row(entry.role.as_str(), detail, self.selected() == Some(entry.role.as_str()), Msg::Local(Local::Open(entry.role.clone()))));
            if !matches!(entry.rules, Rules::None) {
                list = list.push(action(format!("Copy from {}…", entry.role), ButtonKind::Ghost, Some(Msg::Local(Local::CopyFrom(entry.role.clone())))));
            }
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
        form = form.push(label("Role", TextRole::Label, Tone::Muted));
        if matches!(self.rules, Rules::External { .. }) {
            form = form.push(label(self.role.as_str(), TextRole::Body, Tone::Normal)).push(label("Edited outside Loa — edit the raw Markdown below; this workflow cannot be renamed here.", TextRole::Label, Tone::Muted));
        } else {
            form = form.push(field("ROLE", &self.role, |s| Msg::Local(Local::Role(s))));
        }
        form = form.push(label("Models (comma separated)", TextRole::Label, Tone::Muted)).push(field("provider/model", &self.models, |s| Msg::Local(Local::Models(s))));
        if let Rules::Managed { name, allowed, disallowed, fields, .. } = &self.rules {
            form = form.push(label("Name", TextRole::Label, Tone::Muted)).push(field("Research", name, |s| Msg::Local(Local::Name(s))));
            form = form.push(label("Description", TextRole::Label, Tone::Muted)).push(field("Short description", &fields.description, |s| Msg::Local(Local::Description(s))));
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
                if allowed.contains(&agent.name) {
                    let note = fields.routing.iter().find(|note| note.agent == agent.name).map(|note| note.note.as_str()).unwrap_or("");
                    let name = agent.name.clone();
                    form = form.push(label("Routing note", TextRole::Label, Tone::Muted)).push(field("When to use this agent", note, move |s| Msg::Local(Local::Routing(name.clone(), s))));
                }
            }
            form = form.push(label("Steps", TextRole::Label, Tone::Muted));
            for (index, step) in fields.steps.iter().enumerate() {
                let controls = row![
                    field("Step", step, move |s| Msg::Local(Local::Step(index, s))),
                    action("Up", ButtonKind::Secondary, (index > 0).then_some(Msg::Local(Local::MoveStepUp(index)))),
                    action("Down", ButtonKind::Secondary, (index + 1 < fields.steps.len()).then_some(Msg::Local(Local::MoveStepDown(index)))),
                    action("Remove", ButtonKind::Danger, Some(Msg::Local(Local::RemoveStep(index))))
                ].spacing(sp(SpaceRole::S));
                form = form.push(label(format!("Step {}", index + 1), TextRole::Label, Tone::Muted)).push(controls);
            }
            form = form.push(action("Add step", ButtonKind::Secondary, Some(Msg::Local(Local::AddStep))));
            form = form.push(label("Rule blocks", TextRole::Label, Tone::Muted));
            for (index, block) in fields.blocks.iter().enumerate() {
                let editor = text_editor(&self.block_text[index].content).on_action(move |a| Msg::Local(Local::BlockText(index, a))).size(text_size(TextRole::Body)).padding(sp(SpaceRole::S)).height(width(LayoutRole::ListWidth)).style(style::editor);
                form = form.push(label("Heading", TextRole::Label, Tone::Muted))
                    .push(field("Rule block heading", &block.heading, move |s| Msg::Local(Local::BlockHeading(index, s))))
                    .push(label("Markdown", TextRole::Label, Tone::Muted)).push(editor)
                    .push(action("Remove block", ButtonKind::Danger, Some(Msg::Local(Local::RemoveBlock(index)))));
            }
            form = form.push(action("Add block", ButtonKind::Secondary, Some(Msg::Local(Local::AddBlock))));
        }
        if !matches!(self.rules, Rules::None) {
            let title = if matches!(self.rules, Rules::HandWritten { .. } | Rules::External { .. }) { "Rules markdown" } else { "Notes" };
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
            Local::Role(role) => if !matches!(self.rules, Rules::External { .. }) { self.role = role; },
            Local::Models(models) => self.models = models,
            Local::Name(value) => if let Rules::Managed { name, .. } = &mut self.rules { *name = value; },
            Local::Access(agent, access) => if let Rules::Managed { allowed, disallowed, fields, .. } = &mut self.rules {
                allowed.retain(|s| s != &agent); disallowed.retain(|s| s != &agent);
                if access != Access::Allow { fields.routing.retain(|note| note.agent != agent); }
                match access { Access::Allow => allowed.push(agent), Access::Disallow => disallowed.push(agent), Access::Unspecified => {} }
            },
            Local::Description(value) => if let Rules::Managed { fields, .. } = &mut self.rules { fields.description = value; },
            Local::Routing(agent, value) => if let Rules::Managed { allowed, fields, .. } = &mut self.rules {
                if allowed.contains(&agent) {
                    if value.trim().is_empty() { fields.routing.retain(|note| note.agent != agent); }
                    else if let Some(note) = fields.routing.iter_mut().find(|note| note.agent == agent) { note.note = value; }
                    else { fields.routing.push(RoutingNote { agent, note: value }); }
                }
            },
            Local::AddStep => if let Rules::Managed { fields, .. } = &mut self.rules { fields.steps.push(String::new()); },
            Local::Step(index, value) => if let Rules::Managed { fields, .. } = &mut self.rules { if let Some(step) = fields.steps.get_mut(index) { *step = value; } },
            Local::RemoveStep(index) => if let Rules::Managed { fields, .. } = &mut self.rules { if index < fields.steps.len() { fields.steps.remove(index); } },
            Local::MoveStepUp(index) => if let Rules::Managed { fields, .. } = &mut self.rules { if index > 0 && index < fields.steps.len() { fields.steps.swap(index, index - 1); } },
            Local::MoveStepDown(index) => if let Rules::Managed { fields, .. } = &mut self.rules { if index < fields.steps.len().saturating_sub(1) { fields.steps.swap(index, index + 1); } },
            Local::AddBlock => if let Rules::Managed { fields, .. } = &mut self.rules { fields.blocks.push(RuleBlock::default()); self.block_text.push(BlockEditor::default()); },
            Local::BlockHeading(index, value) => if let Rules::Managed { fields, .. } = &mut self.rules { if let Some(block) = fields.blocks.get_mut(index) { block.heading = value; } },
            Local::BlockText(index, action) => {
                if let Some(editor) = self.block_text.get_mut(index) {
                    editor.dirty |= action.is_edit();
                    editor.content.perform(action);
                }
            },
            Local::RemoveBlock(index) => if let Rules::Managed { fields, .. } = &mut self.rules { if index < fields.blocks.len() { fields.blocks.remove(index); self.block_text.remove(index); } },
            Local::CopyFrom(role) => {
                if let Some(entry) = self.entries.iter().find(|entry| entry.role == role && !matches!(entry.rules, Rules::None)).cloned() {
                    let rules = match entry.rules {
                        Rules::Managed { name, allowed, disallowed, notes, fields } => {
                            Rules::Managed { name: format!("{name} copy"), allowed, disallowed, notes, fields }
                        }
                        Rules::External { text } | Rules::HandWritten { text } => Rules::Managed {
                            name: format!("{role} copy"), allowed: Vec::new(), disallowed: Vec::new(), notes: text, fields: ManagedFields::default(),
                        },
                        Rules::None => return None,
                    };
                    self.new_workflow();
                    self.models = entry.models.join(", ");
                    self.set_rules(rules);
                }
            }
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
