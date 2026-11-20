//! Message composer and its controls.
//!
//! An iced `text_editor` with what the chat input needs on top: Enter to send
//! and Shift+Enter for a newline, undo and redo, growing from one line up to a
//! maximum height, `/` command and `@` file popups, harness, model, effort and mode
//! pickers, and a stop button with a queue for messages written mid turn.
//!
//! The app feeds it plain data ([`Command`], file paths, [`ModelOption`]s,
//! turn state) and acts on the [`Event`]s [`State::update`] returns.

mod fuzzy;
mod history;
mod keys;
mod mention;
mod pickers;
mod popover;
mod popup;
mod queue;
mod style;

use std::sync::Arc;

use iced::Alignment::Center;
use iced::advanced::input_method;
use iced::keyboard::{self, Key, key::Named};
use iced::widget::text_editor::{Action, Content, Cursor, Edit, Position};
use iced::widget::{button, column, container, row, space, text, text_input};
use iced::{Element, Length, Subscription, Task};
use loa_adapter::Prompt;
use loa_core::SessionMode;
use loa_ui_icons::{Icon, icon, muted};

use history::{EditKind, History, Snapshot};
use keys::KeyContext;
pub use pickers::{HarnessOption, ModelOption, Picker, PickerAction};
use pickers::Pickers;
pub use popup::PopupKind;
use popup::{MAX_ITEMS, Popup, Trigger};
pub use queue::QueuedMessage;
use queue::Queue;

/// Widget id of the editor, for focus operations.
pub const EDITOR_ID: &str = "loa-composer-editor";
/// Height at which the editor stops growing and scrolls (iced adds part of
/// the padding on top).
pub const MAX_EDITOR_HEIGHT: f32 = 240.0;
/// Undo steps kept.
pub const UNDO_DEPTH: usize = 200;
/// Popup rows shown at once; the list scrolls with the selection.
const VISIBLE_POPUP_ROWS: usize = 8;
/// Diameter of the round send and stop button.
const SEND_SIZE: f32 = 28.0;
/// Text size in the editor.
const EDITOR_TEXT_SIZE: f32 = 15.0;
/// Size of the ghost controls' icons, labels and chevrons.
const CONTROL_ICON: f32 = 15.0;
const CONTROL_TEXT: f32 = 13.0;
const CHEVRON: f32 = 12.0;
/// Width of a picker's menu.
const MENU_WIDTH: f32 = 260.0;
/// Labels of the icon-only buttons, shown as tooltips (and their accessible
/// names once iced exposes them).
pub const SEND_LABEL: &str = "Send (Enter)";
pub const STOP_LABEL: &str = "Stop (Esc)";
pub const REMOVE_QUEUED_LABEL: &str = "Remove queued message";
pub const MENTION_LABEL: &str = "Mention a file (@)";

/// A `/` command offered in the popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// Name without the slash.
    pub name: String,
    pub description: String,
}

/// What the app must act on.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Send this prompt as a new turn: typed now while idle, or the next
    /// queued message released when the previous turn ended (which also
    /// shortens the queue). `@` mentions of known files are attachments.
    Submit(Prompt),
    /// Interrupt the running turn. Queued messages went back into the editor.
    Stop,
    /// The queue was added to, edited or shortened. Holds the queued texts.
    QueueChanged(Vec<String>),
    /// The `@` popup opened: refresh the file list and pass it to
    /// [`State::set_files`]. `query` is what follows the `@`.
    FilesRequested { query: String },
    ModelChanged(ModelOption),
    EffortChanged(String),
    ModeChanged(SessionMode),
    /// The user picked another harness. Holds its id.
    HarnessChanged(String),
    /// The user pressed one of the [`State::set_model_actions`] rows. Holds
    /// its id.
    ModelAction(String),
    /// "Show in Files" in the folder menu.
    OpenWorkspace,
    /// A folder was picked from the folder menu. Holds its full path.
    FolderPicked(String),
    /// "Choose another folder…" in the folder menu: open a folder dialog.
    BrowseFolders,
}

/// The folder a session works in, as the composer's folder chip shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// The folder, shortened for display, e.g. `~/Projects/omp-gui`.
    pub path: String,
    /// The full path, shown on hover.
    pub full: String,
    /// The git branch checked out there, if it is a repository.
    pub branch: Option<String>,
}

/// Internal messages of the composer.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Edit(Action),
    Submit,
    Send,
    StopPressed,
    Interrupt,
    Undo,
    Redo,
    PopupNext,
    PopupPrevious,
    PopupAccept,
    PopupPick(usize),
    PopupClose,
    QueueEdit(u64, String),
    QueueRemove(u64),
    ModelPicked(ModelOption),
    ModelActionPicked(String),
    EffortPicked(String),
    ModePicked(SessionMode),
    HarnessPicked(String),
    /// Opens or closes a picker's menu.
    PickerToggled(Picker),
    /// Closes the open picker menu: a press outside it, or Escape.
    PickerClosed,
    /// Whether an input method is composing text.
    Composing(bool),
    /// "Show in Files" in the folder menu.
    WorkspacePressed,
    /// A folder in the folder menu.
    FolderPicked(String),
    /// "Choose another folder…" in the folder menu.
    BrowseFolders,
    /// The `+` button: starts an `@` file mention at the cursor.
    MentionPressed,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    /// A prompt was handed to the app; its turn has not started yet.
    Starting,
    Running,
}

/// The composer.
pub struct State {
    content: Content,
    history: History,
    composing: bool,
    phase: Phase,
    queue: Queue,
    commands: Vec<Command>,
    files: Vec<String>,
    /// Paths picked from the `@` popup since the last send, resolved even if
    /// a later file list no longer has them.
    mentions: Vec<String>,
    popup: Option<Popup>,
    /// A popup closed with Esc stays closed while its word is edited.
    dismissed: Option<Trigger>,
    pickers: Pickers,
    author: String,
    workspace: Option<Workspace>,
    /// Folders the folder menu offers, most recent first.
    folders: Vec<Workspace>,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        Self {
            content: Content::new(),
            history: History::new(UNDO_DEPTH),
            composing: false,
            phase: Phase::Idle,
            queue: Queue::default(),
            commands: Vec::new(),
            files: Vec::new(),
            mentions: Vec::new(),
            popup: None,
            dismissed: None,
            pickers: Pickers::default(),
            author: String::new(),
            workspace: None,
            folders: Vec::new(),
        }
    }

    /// Sets the folders the folder menu offers besides the current one.
    pub fn set_folders(&mut self, folders: Vec<Workspace>) {
        self.folders = folders;
    }

    /// Sets the folder the session works in, shown left of the pickers.
    pub fn set_workspace(&mut self, workspace: Option<Workspace>) {
        self.workspace = workspace;
    }

    /// The folder chip's contents.
    pub fn workspace(&self) -> Option<&Workspace> {
        self.workspace.as_ref()
    }

    /// Node key or user written to [`Prompt::by`].
    pub fn set_author(&mut self, author: impl Into<String>) {
        self.author = author.into();
    }

    /// Commands for the `/` popup.
    pub fn set_commands(&mut self, commands: Vec<Command>) {
        self.commands = commands;
        self.refilter();
    }

    /// The commands the `/` popup offers.
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// Files for the `@` popup, relative to the project, `/` separated.
    pub fn set_files(&mut self, files: Vec<String>) {
        self.files = files;
        self.refilter();
    }

    /// Models to pick from and the session's current one, by id.
    pub fn set_models(&mut self, models: Vec<ModelOption>, current: Option<&str>) {
        self.pickers.model = current.and_then(|id| models.iter().find(|m| m.id == id).cloned());
        self.pickers.models = models;
    }

    /// Rows listed under the models in the model picker, e.g. "All models…".
    pub fn set_model_actions(&mut self, actions: Vec<PickerAction>) {
        self.pickers.model_actions = actions;
    }

    /// Effort levels of the current model and the current level. An empty
    /// list hides the effort picker.
    pub fn set_efforts(&mut self, efforts: Vec<String>, current: Option<String>) {
        self.pickers.efforts = efforts;
        self.pickers.effort = current;
    }

    /// The models offered by [`State::set_models`] and the current one.
    pub fn models(&self) -> (&[ModelOption], Option<&ModelOption>) {
        (&self.pickers.models, self.pickers.model.as_ref())
    }

    /// The effort levels offered by [`State::set_efforts`] and the current
    /// one.
    pub fn efforts(&self) -> (&[String], Option<&str>) {
        (&self.pickers.efforts, self.pickers.effort.as_deref())
    }

    pub fn set_mode(&mut self, mode: SessionMode) {
        self.pickers.mode = mode;
    }

    /// Harnesses prompts can be routed to and the current one, by id. The
    /// harness picker shows when there are at least two options.
    pub fn set_harnesses(&mut self, harnesses: Vec<HarnessOption>, current: Option<&str>) {
        self.pickers.harness = current.filter(|id| harnesses.iter().any(|h| h.id == *id)).map(str::to_owned);
        self.pickers.harnesses = harnesses;
    }

    /// The current harness, if it is one of the options.
    pub fn harness(&self) -> Option<&HarnessOption> {
        let id = self.pickers.harness.as_deref()?;
        self.pickers.harnesses.iter().find(|h| h.id == id)
    }

    /// The session's turn started.
    pub fn turn_started(&mut self) {
        self.phase = Phase::Running;
    }

    /// The session's turn ended, or the app could not start the turn of the
    /// last [`Event::Submit`]. Releases the next queued message.
    pub fn turn_ended(&mut self) -> Option<Event> {
        self.phase = Phase::Idle;
        let next = self.queue.pop_sendable()?;
        Some(self.send(next.text.trim()))
    }

    /// Whether a turn runs or is starting.
    pub fn is_busy(&self) -> bool {
        self.phase != Phase::Idle
    }

    /// Replaces the editor's text, cursor at the end, and focuses it. Undo
    /// brings back what was there.
    pub fn set_text(&mut self, text: &str) -> Task<Message> {
        self.history.begin(history::EditKind::Other, self.snapshot());
        self.history.break_group();
        self.content = Content::with_text(text);
        let line = self.content.line_count().saturating_sub(1);
        let column = self.content.line(line).map_or(0, |l| l.text.len());
        self.content.move_to(Cursor { position: Position { line, column }, selection: None });
        iced::widget::operation::focus(EDITOR_ID)
    }

    /// Text in the editor.
    pub fn text(&self) -> String {
        self.content.text()
    }

    /// Editor cursor.
    pub fn cursor(&self) -> Cursor {
        self.content.cursor()
    }

    pub fn queue(&self) -> impl Iterator<Item = &QueuedMessage> {
        self.queue.iter()
    }

    /// Kind of the popup showing items, if any.
    pub fn popup_kind(&self) -> Option<PopupKind> {
        self.visible_popup().map(|p| p.trigger.kind)
    }

    /// Labels of the popup rows in order, with the highlighted row's index.
    pub fn popup_items(&self) -> (Vec<&str>, usize) {
        match self.visible_popup() {
            Some(p) => (p.matches.iter().map(|&i| self.candidate(p.trigger.kind, i)).collect(), p.selected),
            None => (Vec::new(), 0),
        }
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let mut task = Task::none();
        let event = match message {
            Message::Edit(action) => self.edit(action),
            Message::Submit => self.submit(),
            Message::Send => {
                task = iced::widget::operation::focus(EDITOR_ID);
                self.submit()
            }
            Message::StopPressed | Message::Interrupt => self.stop(),
            Message::Undo => self.step(History::undo),
            Message::Redo => self.step(History::redo),
            Message::PopupNext => {
                if let Some(p) = &mut self.popup {
                    p.next();
                }
                None
            }
            Message::PopupPrevious => {
                if let Some(p) = &mut self.popup {
                    p.previous();
                }
                None
            }
            Message::PopupAccept => self.accept(None),
            Message::PopupPick(row) => {
                task = iced::widget::operation::focus(EDITOR_ID);
                self.accept(Some(row))
            }
            Message::PopupClose => {
                self.dismissed = self.popup.take().map(|p| p.trigger);
                None
            }
            Message::QueueEdit(id, text) => self.queue.edit(id, text).then(|| self.queue_changed()),
            Message::QueueRemove(id) => self.queue.remove(id).then(|| self.queue_changed()),
            Message::HarnessPicked(id) => {
                self.pickers.open = None;
                let available = self.pickers.harnesses.iter().any(|h| h.id == id && h.available);
                (available && self.pickers.harness.as_ref() != Some(&id)).then(|| {
                    self.pickers.harness = Some(id.clone());
                    Event::HarnessChanged(id)
                })
            }
            Message::PickerToggled(picker) => {
                self.pickers.open = (self.pickers.open != Some(picker)).then_some(picker);
                None
            }
            Message::PickerClosed => {
                self.pickers.open = None;
                None
            }
            Message::WorkspacePressed => {
                self.pickers.open = None;
                Some(Event::OpenWorkspace)
            }
            Message::FolderPicked(path) => {
                self.pickers.open = None;
                Some(Event::FolderPicked(path))
            }
            Message::BrowseFolders => {
                self.pickers.open = None;
                Some(Event::BrowseFolders)
            }
            Message::MentionPressed => {
                task = iced::widget::operation::focus(EDITOR_ID);
                self.start_mention()
            }
            Message::ModelPicked(model) => {
                self.pickers.open = None;
                (self.pickers.model.as_ref() != Some(&model)).then(|| {
                    self.pickers.model = Some(model.clone());
                    Event::ModelChanged(model)
                })
            }
            Message::ModelActionPicked(id) => {
                self.pickers.open = None;
                Some(Event::ModelAction(id))
            }
            Message::EffortPicked(effort) => {
                self.pickers.open = None;
                (self.pickers.effort.as_ref() != Some(&effort)).then(|| {
                    self.pickers.effort = Some(effort.clone());
                    Event::EffortChanged(effort)
                })
            }
            Message::ModePicked(mode) => {
                self.pickers.open = None;
                (self.pickers.mode != mode).then(|| {
                    self.pickers.mode = mode;
                    Event::ModeChanged(mode)
                })
            }
            Message::Composing(composing) => {
                self.composing = composing;
                None
            }
        };
        (task, event)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let ctx = KeyContext {
            popup_open: self.visible_popup().is_some(),
            composing: self.composing,
            busy: self.is_busy(),
        };
        let placeholder = match self.harness() {
            _ if self.is_busy() => "Queue a message…".to_owned(),
            Some(harness) => format!("Ask {} anything. Type / for commands", harness.label),
            None => "Ask anything. Type / for commands".to_owned(),
        };
        // At least two lines of text (iced adds the padding) before the controls.
        let line = EDITOR_TEXT_SIZE * 1.3;
        let editor = iced::widget::text_editor(&self.content)
            .id(EDITOR_ID)
            .placeholder(placeholder)
            .on_action(Message::Edit)
            .height(Length::Shrink)
            .min_height(2.0 * line)
            .max_height(MAX_EDITOR_HEIGHT)
            .padding([6, 8])
            .size(EDITOR_TEXT_SIZE)
            .style(style::editor)
            .key_binding(move |press| keys::bind(press, ctx));

        let (glyph, size, on_press, label) = if self.is_busy() {
            (Icon::Square, 12.0, Some(Message::StopPressed), STOP_LABEL)
        } else {
            let can_send = !self.content.text().trim().is_empty();
            (Icon::ArrowUp, 16.0, can_send.then_some(Message::Send), SEND_LABEL)
        };
        let tint = if on_press.is_some() { style::on_accent } else { style::send_idle };
        let action = labelled(
            button(icon(glyph, size).style(tint))
                .width(SEND_SIZE)
                .height(SEND_SIZE)
                .padding((SEND_SIZE - size) / 2.0)
                .style(style::send)
                .on_press_maybe(on_press),
            label,
        );

        let pickers = &self.pickers;
        let mention = labelled(
            button(icon(Icon::Plus, 18.0).style(style::ghost_icon))
                .padding(5)
                .style(style::ghost(false))
                .on_press(Message::MentionPressed),
            MENTION_LABEL,
        );
        let mut controls = row![mention].spacing(2).align_y(Center);
        if pickers.harnesses.len() > 1 {
            let label = self.harness().map_or("Harness", |h| h.label.as_str());
            controls = controls.push(self.picker(Picker::Harness, Icon::Bot, label.to_owned()));
        }
        if !pickers.models.is_empty() || pickers.model.is_some() || !pickers.model_actions.is_empty() {
            let label = pickers.model.as_ref().map_or_else(|| "Model".to_owned(), |m| short_model_name(&m.name));
            controls = controls.push(self.picker(Picker::Model, Icon::Sparkles, label));
        }
        if !pickers.efforts.is_empty() {
            let label = capitalized(pickers.effort.as_deref().unwrap_or("Effort"));
            controls = controls.push(self.picker(Picker::Effort, Icon::Brain, label));
        }
        controls = controls
            .push(self.picker(Picker::Mode, Icon::Shield, capitalized(pickers.mode.as_str())))
            .push(space::horizontal())
            .push(action);

        let controls = container(controls).padding(iced::Padding { top: 4.0, right: 12.0, bottom: 12.0, left: 12.0 });
        // The editor's own padding plus this gives the text ~20px of room at
        // the top and left.
        let editor = container(editor).padding(iced::Padding { top: 12.0, right: 12.0, bottom: 0.0, left: 12.0 });
        let frame = container(column![editor, controls]).width(Length::Fill).style(style::frame);

        // Fixed slots, each an empty space when unused, so the editor keeps
        // its place in the widget tree (and its state) whatever appears
        // above it.
        let queue: Element<'_, Message> = if self.queue.is_empty() { space().into() } else { self.queue_view() };
        // Menus float over the page above the folder bar instead of taking
        // room in the layout, so opening one never moves anything.
        let menu: Option<Element<'_, Message>> = match pickers.open {
            Some(Picker::Folder) => Some(self.folder_menu()),
            Some(picker) => Some(self.menu_view(picker)),
            None => self.visible_popup().map(|popup| self.popup_view(popup)),
        };
        let bar: Element<'_, Message> = match &self.workspace {
            Some(workspace) => self.folder_bar(workspace),
            None => space().into(),
        };
        popover::popover(column![queue, bar, frame].spacing(6), menu).into()
    }

    /// A ghost bottom-row picker: a muted icon, its current choice and a
    /// small chevron, with no fill until hovered or open.
    fn picker<'a>(&self, picker: Picker, glyph: Icon, label: String) -> Element<'a, Message> {
        let open = self.pickers.open == Some(picker);
        let chevron = if open { Icon::ChevronUp } else { Icon::ChevronDown };
        let content = row![
            icon(glyph, CONTROL_ICON).style(style::ghost_icon),
            text(label).size(CONTROL_TEXT),
            icon(chevron, CHEVRON).style(style::chevron),
        ]
        .spacing(6)
        .align_y(Center);
        button(content).padding([5, 8]).style(style::ghost(open)).on_press(Message::PickerToggled(picker)).into()
    }

    /// The open picker's choices. The current one carries a check mark;
    /// unavailable harnesses are listed but cannot be pressed.
    fn menu_view(&self, picker: Picker) -> Element<'_, Message> {
        let pickers = &self.pickers;
        let rows: Vec<Element<'_, Message>> = match picker {
            Picker::Harness => pickers
                .harnesses
                .iter()
                .map(|h| {
                    let current = pickers.harness.as_deref() == Some(h.id.as_str());
                    let pick = h.available.then(|| Message::HarnessPicked(h.id.clone()));
                    menu_row(&h.label, h.detail.clone(), current, pick)
                })
                .collect(),
            Picker::Model => pickers
                .models
                .iter()
                .map(|m| {
                    let detail = match &m.role {
                        Some(role) => format!("{} · {role}", m.provider),
                        None => m.provider.clone(),
                    };
                    let current = pickers.model.as_ref() == Some(m);
                    menu_row(&m.name, Some(detail), current, Some(Message::ModelPicked(m.clone())))
                })
                .chain(
                    pickers
                        .model_actions
                        .iter()
                        .map(|a| menu_row(&a.label, None, false, Some(Message::ModelActionPicked(a.id.clone())))),
                )
                .collect(),
            Picker::Effort => pickers
                .efforts
                .iter()
                .map(|e| {
                    let current = pickers.effort.as_ref() == Some(e);
                    menu_row(e, None, current, Some(Message::EffortPicked(e.clone())))
                })
                .collect(),
            Picker::Mode => SessionMode::ALL
                .iter()
                .map(|&mode| menu_row(mode.as_str(), None, pickers.mode == mode, Some(Message::ModePicked(mode))))
                .collect(),
            Picker::Folder => Vec::new(),
        };
        container(column(rows).spacing(2)).padding(4).width(MENU_WIDTH).style(style::menu).into()
    }

    /// The bar above the editor: the session's folder and git branch. A
    /// click opens the folder menu.
    fn folder_bar<'a>(&'a self, workspace: &'a Workspace) -> Element<'a, Message> {
        let open = self.pickers.open == Some(Picker::Folder);
        let mut chip = row![icon(Icon::Folder, CONTROL_ICON).style(style::ghost_icon), text(workspace.path.as_str()).size(CONTROL_TEXT)]
            .spacing(6)
            .align_y(Center);
        if let Some(branch) = &workspace.branch {
            chip = chip
                .push(icon(Icon::GitFork, 13.0).style(style::chevron))
                .push(text(branch.as_str()).size(CONTROL_TEXT).style(style::faint));
        }
        chip = chip.push(icon(if open { Icon::ChevronUp } else { Icon::ChevronDown }, CHEVRON).style(style::chevron));
        let chip = button(chip).padding([5, 8]).style(style::ghost(open)).on_press(Message::PickerToggled(Picker::Folder));
        // The full path on hover, except while the menu is open: the tip
        // would cover the menu's last row.
        let chip: Element<'_, Message> = if open {
            chip.into()
        } else {
            let tip = container(text(workspace.full.as_str()).size(12)).padding([3, 8]).style(style::menu);
            iced::widget::tooltip(chip, tip, iced::widget::tooltip::Position::Top).into()
        };
        container(chip).id(WORKSPACE_CHIP_ID).into()
    }

    /// Recent folders, a folder dialog and Show in Files.
    fn folder_menu(&self) -> Element<'_, Message> {
        let current = self.workspace.as_ref().map(|w| w.full.as_str());
        let mut rows: Vec<Element<'_, Message>> = Vec::new();
        let mut listed: Vec<&str> = Vec::new();
        for folder in self.workspace.iter().chain(self.folders.iter()) {
            if listed.contains(&folder.full.as_str()) {
                continue;
            }
            listed.push(folder.full.as_str());
            let is_current = current == Some(folder.full.as_str());
            rows.push(menu_row(&folder.path, folder.branch.clone(), is_current, Some(Message::FolderPicked(folder.full.clone()))));
        }
        rows.push(menu_row("Choose another folder…", None, false, Some(Message::BrowseFolders)));
        rows.push(menu_row("Show in Files", None, false, Some(Message::WorkspacePressed)));
        container(column(rows).spacing(2)).padding(4).width(MENU_WIDTH.max(360.0)).style(style::menu).into()
    }

    /// Tracks input method composition so Enter and Esc reach the IME, and
    /// closes an open picker menu on Esc.
    pub fn subscription(&self) -> Subscription<Message> {
        let ime = iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::InputMethod(input_method::Event::Preedit(content, _)) => {
                Some(Message::Composing(!content.is_empty()))
            }
            iced::Event::InputMethod(
                input_method::Event::Commit(_) | input_method::Event::Closed | input_method::Event::Opened,
            ) => Some(Message::Composing(false)),
            _ => None,
        });
        if self.pickers.open.is_none() {
            return ime;
        }
        let escape = iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::Keyboard(keyboard::Event::KeyPressed { key: Key::Named(Named::Escape), .. }) => {
                Some(Message::PickerClosed)
            }
            _ => None,
        });
        Subscription::batch([ime, escape])
    }

    fn queue_view(&self) -> Element<'_, Message> {
        let rows = self.queue.iter().map(|item| {
            let id = item.id;
            row![
                text("Queued").size(12).style(style::muted),
                text_input("Empty message is skipped", &item.text)
                    .on_input(move |text| Message::QueueEdit(id, text))
                    .size(13)
                    .width(Length::Fill),
                labelled(
                    button(muted(Icon::X, 14.0)).padding(4).style(style::row(false)).on_press(Message::QueueRemove(id)),
                    REMOVE_QUEUED_LABEL,
                ),
            ]
            .spacing(8)
            .align_y(Center)
            .into()
        });
        container(column(rows).spacing(4)).padding(6).style(style::menu).width(Length::Fill).into()
    }

    fn popup_view<'a>(&'a self, popup: &'a Popup) -> Element<'a, Message> {
        let first = popup.selected.saturating_sub(VISIBLE_POPUP_ROWS - 1);
        let rows = popup.matches.iter().enumerate().skip(first).take(VISIBLE_POPUP_ROWS).map(|(row, &index)| {
            let label: Element<'a, Message> = match popup.trigger.kind {
                PopupKind::Command => {
                    let command = &self.commands[index];
                    row![
                        text(format!("/{}", command.name)).size(14),
                        text(&command.description).size(12).style(style::muted),
                    ]
                    .spacing(12)
                    .align_y(Center)
                    .into()
                }
                PopupKind::File => row![muted(Icon::FileText, 14.0), text(&self.files[index]).size(14)]
                    .spacing(8)
                    .align_y(Center)
                    .into(),
            };
            button(label)
                .padding([7, 8])
                .style(style::row(row == popup.selected))
                .width(Length::Fill)
                .on_press(Message::PopupPick(row))
                .into()
        });
        let title = match popup.trigger.kind {
            PopupKind::Command => "Commands",
            PopupKind::File => "Files",
        };
        let header = container(text(title).size(12).style(style::muted)).padding([4, 8]);
        container(column![header, column(rows).spacing(2)].spacing(2))
            .padding(4)
            .style(style::menu)
            .width(Length::Fill)
            .into()
    }

    fn visible_popup(&self) -> Option<&Popup> {
        self.popup.as_ref().filter(|p| p.is_visible())
    }

    fn candidate(&self, kind: PopupKind, index: usize) -> &str {
        match kind {
            PopupKind::Command => &self.commands[index].name,
            PopupKind::File => &self.files[index],
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { text: self.content.text(), cursor: self.content.cursor() }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.content = Content::with_text(&snapshot.text);
        self.content.move_to(snapshot.cursor);
    }

    fn edit(&mut self, action: Action) -> Option<Event> {
        if !matches!(action, Action::Scroll { .. }) {
            self.pickers.open = None;
        }
        match &action {
            Action::Edit(edit) => {
                let kind = edit_kind(edit);
                if self.history.continues(kind) {
                    self.content.perform(action);
                    self.history.extend(kind);
                } else {
                    let before = self.snapshot();
                    self.content.perform(action);
                    if self.content.text() == before.text {
                        self.history.break_group();
                    } else {
                        self.history.begin(kind, before);
                    }
                }
            }
            Action::Scroll { .. } => self.content.perform(action),
            _ => {
                self.history.break_group();
                self.content.perform(action);
            }
        }
        self.refresh_popup()
    }

    fn step(&mut self, step: fn(&mut History, Snapshot) -> Option<Snapshot>) -> Option<Event> {
        let current = self.snapshot();
        let target = step(&mut self.history, current)?;
        self.restore(target);
        self.refresh_popup()
    }

    /// Types an `@` at the cursor, after a space when it follows a word, so
    /// the file mention popup opens.
    fn start_mention(&mut self) -> Option<Event> {
        let cursor = self.content.cursor();
        let after_word = cursor.selection.is_none()
            && self.content.line(cursor.position.line).is_some_and(|line| {
                line.text.get(..cursor.position.column).and_then(|b| b.chars().next_back()).is_some_and(|c| !c.is_whitespace())
            });
        let mention = if after_word { " @" } else { "@" };
        self.edit(Action::Edit(Edit::Paste(Arc::new(mention.to_owned()))))
    }

    fn submit(&mut self) -> Option<Event> {
        let text = self.content.text();
        let message = text.trim();
        if message.is_empty() {
            return None;
        }
        let message = message.to_owned();
        self.content = Content::new();
        self.history.clear();
        self.popup = None;
        self.dismissed = None;
        if self.is_busy() {
            self.queue.push(message);
            return Some(self.queue_changed());
        }
        Some(self.send(&message))
    }

    /// Builds the prompt for `message` and marks the turn as starting.
    fn send(&mut self, message: &str) -> Event {
        let attachments =
            mention::resolve(message, |path| self.mentions.iter().any(|m| m == path) || self.files.iter().any(|f| f == path));
        if self.queue.is_empty() && self.content.is_empty() {
            self.mentions.clear();
        }
        self.phase = Phase::Starting;
        Event::Submit(Prompt { text: message.to_owned(), attachments, by: self.author.clone() })
    }

    /// Interrupts the turn and moves queued messages back into the editor,
    /// ahead of what is typed there, so nothing is sent after a stop.
    fn stop(&mut self) -> Option<Event> {
        if !self.is_busy() {
            return None;
        }
        let queued = self.queue.take_all();
        if !queued.is_empty() {
            let before = self.snapshot();
            let mut text = queued.into_iter().map(|m| m.text).collect::<Vec<_>>().join("\n\n");
            if !before.text.trim().is_empty() {
                text.push_str("\n\n");
                text.push_str(&before.text);
            }
            self.history.break_group();
            self.history.begin(EditKind::Other, before);
            self.content = Content::with_text(&text);
            let line = self.content.line_count().saturating_sub(1);
            let column = self.content.line(line).map_or(0, |l| l.text.len());
            self.content.move_to(Cursor { position: Position { line, column }, selection: None });
            self.popup = None;
        }
        Some(Event::Stop)
    }

    fn queue_changed(&self) -> Event {
        Event::QueueChanged(self.queue.iter().map(|m| m.text.clone()).collect())
    }

    /// Replaces the popup word with the chosen item.
    fn accept(&mut self, row: Option<usize>) -> Option<Event> {
        let popup = self.visible_popup()?;
        let index = match row {
            Some(row) => *popup.matches.get(row)?,
            None => popup.selected_candidate()?,
        };
        let trigger = popup.trigger.clone();
        let replacement = match trigger.kind {
            PopupKind::Command => format!("/{} ", self.commands[index].name),
            PopupKind::File => {
                let path = self.files[index].clone();
                let token = mention::token(&path);
                if !self.mentions.contains(&path) {
                    self.mentions.push(path);
                }
                format!("{token} ")
            }
        };

        let before = self.snapshot();
        self.history.break_group();
        self.content.move_to(Cursor {
            position: Position { line: trigger.line, column: trigger.end },
            selection: Some(Position { line: trigger.line, column: trigger.start }),
        });
        self.content.perform(Action::Edit(Edit::Paste(Arc::new(replacement))));
        self.history.begin(EditKind::Other, before);
        self.popup = None;
        self.dismissed = None;
        None
    }

    /// Opens, updates or closes the popup for the word under the cursor.
    fn refresh_popup(&mut self) -> Option<Event> {
        let cursor = self.content.cursor();
        let trigger = match cursor.selection {
            Some(_) => None,
            None => self
                .content
                .line(cursor.position.line)
                .and_then(|line| popup::detect(cursor.position.line, &line.text, cursor.position.column)),
        };
        let Some(trigger) = trigger else {
            self.popup = None;
            self.dismissed = None;
            return None;
        };
        if self.dismissed.as_ref().is_some_and(|d| d.same_anchor(&trigger)) {
            self.popup = None;
            return None;
        }
        self.dismissed = None;

        let previous = self.popup.take().filter(|p| p.trigger.same_anchor(&trigger));
        let opened = previous.is_none();
        let selected = previous.filter(|p| p.trigger.query == trigger.query).map_or(0, |p| p.selected);
        let matches = self.matches(&trigger);
        let event = (opened && trigger.kind == PopupKind::File)
            .then(|| Event::FilesRequested { query: trigger.query.clone() });
        let selected = selected.min(matches.len().saturating_sub(1));
        self.popup = Some(Popup { trigger, matches, selected });
        event
    }

    /// Recomputes the matches of the open popup after its candidates changed.
    fn refilter(&mut self) {
        if let Some(trigger) = self.popup.as_ref().map(|p| p.trigger.clone()) {
            let matches = self.matches(&trigger);
            if let Some(p) = &mut self.popup {
                p.selected = p.selected.min(matches.len().saturating_sub(1));
                p.matches = matches;
            }
        }
    }

    fn matches(&self, trigger: &Trigger) -> Vec<usize> {
        match trigger.kind {
            PopupKind::Command => {
                fuzzy::filter(&trigger.query, self.commands.iter().map(|c| c.name.as_str()), MAX_ITEMS)
            }
            PopupKind::File => fuzzy::filter(&trigger.query, self.files.iter().map(String::as_str), MAX_ITEMS),
        }
    }
}

/// A picker menu row: the choice, a muted note, and a check on the current
/// one. Without `pick` the row is listed but cannot be pressed.
fn menu_row(label: &str, detail: Option<String>, current: bool, pick: Option<Message>) -> Element<'_, Message> {
    let mut content = row![text(label).size(14)].spacing(8).align_y(Center);
    if let Some(detail) = detail {
        content = content.push(text(detail).size(12).style(style::muted));
    }
    content = content.push(space::horizontal());
    if current {
        content = content.push(icon(Icon::Check, 14.0));
    }
    button(content).padding([6, 8]).width(Length::Fill).style(style::row(false)).on_press_maybe(pick).into()
}

/// A model name as the picker shows it: `Claude Opus 5.5` reads `Opus 5.5`.
fn short_model_name(name: &str) -> String {
    name.strip_prefix("Claude ").filter(|rest| !rest.trim().is_empty()).unwrap_or(name).to_owned()
}

/// `medium` reads `Medium`.
fn capitalized(label: &str) -> String {
    let mut chars = label.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

fn edit_kind(edit: &Edit) -> EditKind {
    match edit {
        Edit::Insert(c) => EditKind::Insert { whitespace: c.is_whitespace() },
        Edit::Backspace | Edit::Delete => EditKind::Delete,
        Edit::Paste(_) | Edit::Enter | Edit::Indent | Edit::Unindent => EditKind::Other,
    }
}

/// Container id of the folder chip.
pub const WORKSPACE_CHIP_ID: &str = "loa-composer-workspace";

/// An icon-only button with its label as a tooltip.
fn labelled<'a>(button: iced::widget::Button<'a, Message>, label: &'static str) -> Element<'a, Message> {
    let tip = container(text(label).size(12)).padding([3, 8]).style(style::menu);
    iced::widget::tooltip(button, tip, iced::widget::tooltip::Position::Top).into()
}

#[cfg(test)]
mod tests;
