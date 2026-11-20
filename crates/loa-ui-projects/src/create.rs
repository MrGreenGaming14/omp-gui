//! The New project screen: a name, the project's folder and optional shared
//! instructions. Create hands them to the app as [`Event::Create`]; the app
//! answers a failure with [`State::failed`].

use crate::screen::{hint, section};
use crate::style;
use iced::widget::{button, column, container, row, scrollable, space, text, text_editor, text_input};
use iced::{Alignment, Element, Fill, Length};
use loa_projects::ProjectSettings;
use loa_ui_icons::Icon;
use std::path::PathBuf;

/// Width of the form.
const CONTENT_WIDTH: f32 = 640.0;

/// Id of the name input, focused when the screen opens.
pub const NAME_ID: &str = "loa-new-project-name";

#[derive(Debug)]
pub struct State {
    name: String,
    folder: String,
    instructions: text_editor::Content,
    creating: bool,
    error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Message {
    NameChanged(String),
    FolderChanged(String),
    /// The Choose… button beside the folder.
    ChooseFolder,
    InstructionsEdited(text_editor::Action),
    Create,
    Cancel,
}

/// What the app does for the screen.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Show a folder dialog starting here; answer with [`State::set_folder`].
    ChooseFolder(PathBuf),
    /// Create the project; answer a failure with [`State::failed`].
    Create { name: String, settings: ProjectSettings },
    Cancel,
}

impl State {
    /// An empty form whose folder starts at `folder`, the current one.
    pub fn new(folder: impl Into<String>) -> Self {
        Self {
            name: String::new(),
            folder: folder.into(),
            instructions: text_editor::Content::new(),
            creating: false,
            error: None,
        }
    }

    pub fn set_folder(&mut self, folder: PathBuf) {
        self.folder = folder.display().to_string();
        self.error = None;
    }

    /// The app couldn't create the project.
    pub fn failed(&mut self, error: impl Into<String>) {
        self.creating = false;
        self.error = Some(error.into());
    }

    fn can_create(&self) -> bool {
        !self.creating && !self.name.trim().is_empty() && !self.folder.trim().is_empty()
    }

    pub fn update(&mut self, message: Message) -> Option<Event> {
        match message {
            Message::NameChanged(name) => self.name = name,
            Message::FolderChanged(folder) => self.folder = folder,
            Message::ChooseFolder => return Some(Event::ChooseFolder(PathBuf::from(self.folder.trim()))),
            Message::InstructionsEdited(action) => self.instructions.perform(action),
            Message::Create => {
                if !self.can_create() {
                    return None;
                }
                let folder = PathBuf::from(self.folder.trim());
                if !folder.is_dir() {
                    self.error = Some(format!("{} isn't a folder", folder.display()));
                    return None;
                }
                self.creating = true;
                self.error = None;
                let settings = ProjectSettings {
                    instructions: self.instructions.text().trim_end().to_owned(),
                    folder: Some(folder),
                    ..ProjectSettings::default()
                };
                return Some(Event::Create { name: self.name.trim().to_owned(), settings });
            }
            Message::Cancel => return Some(Event::Cancel),
        }
        None
    }

    pub fn view(&self) -> Element<'_, Message> {
        let title = row![loa_ui_icons::icon(Icon::Folder, 20.0), text("New project").size(style::TITLE)]
            .spacing(10)
            .align_y(Alignment::Center);

        let can_create = self.can_create();
        let name = column![
            section("Name"),
            text_input("Launch plan", &self.name)
                .id(NAME_ID)
                .on_input(Message::NameChanged)
                .on_submit_maybe(can_create.then_some(Message::Create))
                .size(style::BODY)
                .padding([6.0, 8.0])
                .style(style::input),
        ]
        .spacing(8);

        let folder = column![
            section("Folder"),
            hint("New chats in this project start here. Its .omgui folder links to every folder the project's chats run in."),
            row![
                text_input("/path/to/folder", &self.folder)
                    .on_input(Message::FolderChanged)
                    .size(style::BODY)
                    .padding([6.0, 8.0])
                    .style(style::input),
                button(text("Choose…").size(style::SMALL)).padding([6.0, 12.0]).style(style::flat).on_press(Message::ChooseFolder),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(8);

        let instructions = column![
            section("Shared instructions"),
            hint("Optional. Given to every chat in this project, like a CLAUDE.md for the whole project."),
            text_editor(&self.instructions)
                .placeholder("Conventions, constraints, where things live…")
                .on_action(Message::InstructionsEdited)
                .size(style::BODY)
                .padding(10)
                .height(Length::Fixed(140.0))
                .style(style::editor),
        ]
        .spacing(8);

        let mut actions = row![].spacing(8).align_y(Alignment::Center);
        if let Some(error) = &self.error {
            actions = actions.push(text(error).size(style::SMALL).style(style::danger));
        }
        actions = actions.push(space::horizontal()).push(
            button(text("Cancel").size(style::SMALL)).padding([6.0, 10.0]).style(style::flat).on_press(Message::Cancel),
        );
        actions = actions.push(
            button(text(if self.creating { "Creating…" } else { "Create project" }).size(style::SMALL))
                .padding([6.0, 14.0])
                .style(button::primary)
                .on_press_maybe(can_create.then_some(Message::Create)),
        );

        let content = column![title, name, folder, instructions, actions].spacing(24).max_width(CONTENT_WIDTH);
        scrollable(container(content).padding([28.0, 32.0]).center_x(Fill))
            .direction(style::thin_scrollbar())
            .style(style::scrollbar)
            .height(Fill)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_needs_a_name_and_an_existing_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::new(dir.path().join("missing").display().to_string());
        assert_eq!(state.update(Message::Create), None, "no name yet");
        let _ = state.update(Message::NameChanged("  Launch  ".into()));
        assert_eq!(state.update(Message::Create), None);
        assert!(state.error.as_deref().is_some_and(|error| error.ends_with("isn't a folder")));

        state.set_folder(dir.path().to_path_buf());
        let _ = state.update(Message::InstructionsEdited(text_editor::Action::Edit(text_editor::Edit::Paste(
            std::sync::Arc::new("Use rustls.\n".into()),
        ))));
        let Some(Event::Create { name, settings }) = state.update(Message::Create) else { panic!("creates") };
        assert_eq!(name, "Launch");
        assert_eq!(settings.folder.as_deref(), Some(dir.path()));
        assert_eq!(settings.instructions, "Use rustls.");
        assert_eq!(state.update(Message::Create), None, "one create at a time");

        state.failed("a project named Launch already exists");
        assert!(state.update(Message::Create).is_some(), "a failure allows another try");
        let _ = state.view();
    }
}
