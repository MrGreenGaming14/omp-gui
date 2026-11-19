//! Message composer and its controls.
//!
//! An iced `text_editor` with what the chat input needs on top: Enter to send
//! and Shift+Enter for a newline, undo and redo, growing from one line up to a
//! maximum height, `/` command and `@` file popups, model, effort and mode
//! pickers, and a stop button with a queue for messages written mid turn.
//!
//! The app feeds it plain data ([`Command`], file paths, [`ModelOption`]s,
//! turn state) and acts on the [`Event`]s [`State::update`] returns.

mod fuzzy;
mod history;
mod keys;
mod mention;
mod pickers;
mod popup;
mod queue;

use std::sync::Arc;

use iced::advanced::input_method;
use iced::widget::text_editor::{Action, Content, Cursor, Edit, Position};
use iced::widget::{button, column, container, pick_list, row, space, text, text_input};
use iced::{Element, Length, Subscription, Task};
use loa_adapter::Prompt;
use loa_core::SessionMode;

use history::{EditKind, History, Snapshot};
use keys::KeyContext;
pub use pickers::ModelOption;
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
    EffortPicked(String),
    ModePicked(SessionMode),
    /// Whether an input method is composing text.
    Composing(bool),
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
        }
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

    /// Effort levels of the current model and the current level. An empty
    /// list hides the effort picker.
    pub fn set_efforts(&mut self, efforts: Vec<String>, current: Option<String>) {
        self.pickers.efforts = efforts;
        self.pickers.effort = current;
    }

    pub fn set_mode(&mut self, mode: SessionMode) {
        self.pickers.mode = mode;
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
            Message::ModelPicked(model) => {
                (self.pickers.model.as_ref() != Some(&model)).then(|| {
                    self.pickers.model = Some(model.clone());
                    Event::ModelChanged(model)
                })
            }
            Message::EffortPicked(effort) => {
                (self.pickers.effort.as_ref() != Some(&effort)).then(|| {
                    self.pickers.effort = Some(effort.clone());
                    Event::EffortChanged(effort)
                })
            }
            Message::ModePicked(mode) => (self.pickers.mode != mode).then(|| {
                self.pickers.mode = mode;
                Event::ModeChanged(mode)
            }),
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
        let editor = iced::widget::text_editor(&self.content)
            .id(EDITOR_ID)
            .placeholder(if self.is_busy() { "Queue a message…" } else { "Message, / for commands, @ for files" })
            .on_action(Message::Edit)
            .height(Length::Shrink)
            .max_height(MAX_EDITOR_HEIGHT)
            .padding(8)
            .key_binding(move |press| keys::bind(press, ctx));

        let action: Element<'_, Message> = if self.is_busy() {
            button(text("Stop")).style(button::danger).on_press(Message::StopPressed).into()
        } else {
            let can_send = !self.content.text().trim().is_empty();
            button(text("Send")).style(button::primary).on_press_maybe(can_send.then_some(Message::Send)).into()
        };

        let mut controls = row![
            pick_list(&self.pickers.models[..], self.pickers.model.as_ref(), Message::ModelPicked)
                .placeholder("Model"),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
        if !self.pickers.efforts.is_empty() {
            controls = controls.push(
                pick_list(&self.pickers.efforts[..], self.pickers.effort.as_ref(), Message::EffortPicked)
                    .placeholder("Effort"),
            );
        }
        controls = controls
            .push(pick_list(SessionMode::ALL, Some(self.pickers.mode), Message::ModePicked))
            .push(space::horizontal())
            .push(action);

        let mut layout = column![].spacing(6);
        if !self.queue.is_empty() {
            layout = layout.push(self.queue_view());
        }
        if let Some(popup) = self.visible_popup() {
            layout = layout.push(self.popup_view(popup));
        }
        layout.push(editor).push(controls).into()
    }

    /// Tracks input method composition so Enter and Esc reach the IME.
    pub fn subscription(&self) -> Subscription<Message> {
        iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::InputMethod(input_method::Event::Preedit(content, _)) => {
                Some(Message::Composing(!content.is_empty()))
            }
            iced::Event::InputMethod(
                input_method::Event::Commit(_) | input_method::Event::Closed | input_method::Event::Opened,
            ) => Some(Message::Composing(false)),
            _ => None,
        })
    }

    fn queue_view(&self) -> Element<'_, Message> {
        let rows = self.queue.iter().map(|item| {
            let id = item.id;
            row![
                text("Queued").size(12),
                text_input("Empty message is skipped", &item.text)
                    .on_input(move |text| Message::QueueEdit(id, text))
                    .width(Length::Fill),
                button(text("Remove")).style(button::text).on_press(Message::QueueRemove(id)),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into()
        });
        container(column(rows).spacing(4)).padding(6).style(container::rounded_box).width(Length::Fill).into()
    }

    fn popup_view<'a>(&'a self, popup: &'a Popup) -> Element<'a, Message> {
        let first = popup.selected.saturating_sub(VISIBLE_POPUP_ROWS - 1);
        let rows = popup.matches.iter().enumerate().skip(first).take(VISIBLE_POPUP_ROWS).map(|(row, &index)| {
            let label: Element<'a, Message> = match popup.trigger.kind {
                PopupKind::Command => {
                    let command = &self.commands[index];
                    row![text(format!("/{}", command.name)), text(&command.description).size(12)]
                        .spacing(12)
                        .into()
                }
                PopupKind::File => text(&self.files[index]).into(),
            };
            let style = if row == popup.selected { button::primary } else { button::text };
            button(label).style(style).width(Length::Fill).on_press(Message::PopupPick(row)).into()
        });
        container(column(rows)).padding(4).style(container::rounded_box).width(Length::Fill).into()
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

fn edit_kind(edit: &Edit) -> EditKind {
    match edit {
        Edit::Insert(c) => EditKind::Insert { whitespace: c.is_whitespace() },
        Edit::Backspace | Edit::Delete => EditKind::Delete,
        Edit::Paste(_) | Edit::Enter | Edit::Indent | Edit::Unindent => EditKind::Other,
    }
}

#[cfg(test)]
mod tests;
