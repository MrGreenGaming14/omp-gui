//! The New project screen: a name, the project's folder and optional shared instructions. Create hands them to
//! the app as [`CreateIntent::Create`]; the app answers a failure with [`State::failed`].

use crate::{action, field, scroll, section};
use iced::widget::{Space, column, container, row, text_editor};
use iced::{Alignment, Element, Fill};
use loa_ui_kit::icons::tinted;
use loa_ui_kit::style::{self, ButtonKind, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, sp, text_size, width};
use loa_ui_kit::widgets::{Msg, Screen, label};
use loa_view::settings::projects::{CreateIntent, ProjectSettings};
use loa_view::theme::Icon;

/// Id of the name input, focused when the screen opens.
pub const NAME_ID: &str = "loa-new-project-name";

#[derive(Clone, Debug)]
pub enum Local {
    Name(String),
    Folder(String),
    /// The Choose… button beside the folder.
    ChooseFolder,
    Instructions(text_editor::Action),
    Create,
}

pub type Message = Msg<Local, CreateIntent>;

#[derive(Debug)]
pub struct State {
    name: String,
    folder: String,
    instructions: text_editor::Content,
    creating: bool,
    error: Option<String>,
}

impl State {
    /// An empty form whose folder starts at `folder`, the current one.
    pub fn new(folder: impl Into<String>) -> Self {
        Self { name: String::new(), folder: folder.into(), instructions: text_editor::Content::new(), creating: false, error: None }
    }

    /// The folder the app's dialog picked.
    pub fn set_folder(&mut self, folder: impl Into<String>) {
        (self.folder, self.error) = (folder.into(), None);
    }

    /// The app couldn't create the project.
    pub fn failed(&mut self, error: impl Into<String>) {
        (self.creating, self.error) = (false, Some(error.into()));
    }

    pub fn error(&self) -> Option<&str> { self.error.as_deref() }

    fn can_create(&self) -> bool {
        !self.creating && !self.name.trim().is_empty() && !self.folder.trim().is_empty()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let title = row![tinted(Icon::Folder, TextRole::Title, Tone::Normal), label("New project", TextRole::Title, Tone::Normal)].spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
        let create = self.can_create().then_some(Msg::Local(Local::Create));
        let hint = |s| label(s, TextRole::Label, Tone::Muted);
        let name = field("Launch plan", &self.name, |n| Msg::Local(Local::Name(n))).id(NAME_ID).on_submit_maybe(create.clone());
        let folder = row![
            field("/path/to/folder", &self.folder, |f| Msg::Local(Local::Folder(f))),
            action("Choose…", ButtonKind::Secondary, Some(Msg::Local(Local::ChooseFolder))),
        ]
        .spacing(sp(SpaceRole::S))
        .align_y(Alignment::Center);
        let folder = column![hint("New chats in this project start here. Its .omgui folder links to every folder the project's chats run in."), folder];
        let editor = text_editor(&self.instructions).placeholder("Conventions, constraints, where things live…").on_action(|a| Msg::Local(Local::Instructions(a)));
        let editor = editor.size(text_size(TextRole::Body)).padding(sp(SpaceRole::S)).height(sp(SpaceRole::Xl) * 6.0).style(style::editor);
        let instructions = column![hint("Optional. Given to every chat in this project, like a CLAUDE.md for the whole project."), editor];
        let error = self.error.as_deref().map(|e| label(e, TextRole::Caption, Tone::Danger));
        let actions = row![
            error,
            Space::new().width(Fill),
            action("Cancel", ButtonKind::Ghost, Some(Msg::Emit(CreateIntent::Cancel))),
            action(if self.creating { "Creating…" } else { "Create project" }, ButtonKind::Primary, create),
        ]
        .spacing(sp(SpaceRole::S))
        .align_y(Alignment::Center);
        let gap = sp(SpaceRole::S);
        let content = column![title, section("Name", name), section("Folder", folder.spacing(gap)), section("Shared instructions", instructions.spacing(gap)), actions];
        scroll(container(content.spacing(sp(SpaceRole::L)).max_width(width(LayoutRole::ContentWidth))).center_x(Fill))
    }
}

impl Screen for State {
    type Local = Local;
    type Event = CreateIntent;

    fn reduce(&mut self, m: Local) -> Option<CreateIntent> {
        match m {
            Local::Name(name) => self.name = name,
            Local::Folder(folder) => self.folder = folder,
            Local::ChooseFolder => return Some(CreateIntent::ChooseFolder(self.folder.trim().to_owned())),
            Local::Instructions(action) => self.instructions.perform(action),
            Local::Create if self.can_create() => {
                (self.creating, self.error) = (true, None);
                let settings = ProjectSettings {
                    instructions: self.instructions.text().trim_end().to_owned(),
                    folder: Some(self.folder.trim().to_owned()),
                    ..ProjectSettings::default()
                };
                return Some(CreateIntent::Create { name: self.name.trim().to_owned(), settings });
            }
            Local::Create => {}
        }
        None
    }

    fn guard(&self, e: &CreateIntent) -> bool {
        matches!(e, CreateIntent::Cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn create_needs_a_name_and_a_folder_and_one_try_at_a_time() {
        let mut s = State::new("");
        assert_eq!(s.update(Msg::Local(Local::Create)), None, "no name yet");
        s.update(Msg::Local(Local::Name("  Launch  ".into())));
        assert_eq!(s.update(Msg::Local(Local::Create)), None, "no folder yet");
        assert_eq!(s.update(Msg::Local(Local::ChooseFolder)), Some(CreateIntent::ChooseFolder(String::new())));

        s.set_folder("/work/launch");
        s.update(Msg::Local(Local::Instructions(text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new("Use rustls.\n".into()))))));
        let Some(CreateIntent::Create { name, settings }) = s.update(Msg::Local(Local::Create)) else { panic!("creates") };
        assert_eq!(name, "Launch");
        assert_eq!(settings.folder.as_deref(), Some("/work/launch"));
        assert_eq!(settings.instructions, "Use rustls.");
        assert_eq!(s.update(Msg::Local(Local::Create)), None, "one create at a time");

        s.failed("a project named Launch already exists");
        assert_eq!(s.error(), Some("a project named Launch already exists"));
        assert!(s.update(Msg::Local(Local::Create)).is_some(), "a failure allows another try");
        assert_eq!(s.update(Msg::Emit(CreateIntent::Cancel)), Some(CreateIntent::Cancel));
        let _ = s.view();
    }
}
