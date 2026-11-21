//! Message composer: an iced `text_editor` with Enter to send and Shift+Enter for a newline,
//! undo and redo, growth up to a maximum height, `/` command and `@` file popups, six kit
//! [`Picker`]s (harness, model, effort, permissions, mode, folder), and a stop button with a
//! queue for messages written mid turn. The app feeds it plain data and acts on [`Event`]s.

mod input;

pub use input::{PopupKind, QueuedMessage};

use iced::Alignment::Center;
use iced::advanced::input_method;
use iced::keyboard::{self, Key, key::Named};
use iced::widget::text_editor::{self, Action, Content, Cursor, Edit, Position};
use iced::widget::{button, column, container, row, space, text, text_input, tooltip};
use iced::{Background, Border, Color, Element, Length, Subscription, Task, Theme};
use input::{EditKind, History, KeyContext, Popup, Queue, Snapshot, Trigger};
use loa_contracts::harness::Prompt;
use loa_core::SessionMode;
use loa_ui_kit::icons::{Icon, icon, muted};
use loa_ui_kit::style::{self as kit, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole as Sp, TextRole as T, sp, text_size as ts, width};
use loa_ui_kit::widgets::{Picker, PickerItem, popover};
use std::borrow::Cow;
use std::sync::Arc;

/// Widget id of the editor, for focus operations.
pub const EDITOR_ID: &str = "loa-composer-editor";
/// Container id of the folder chip.
pub const WORKSPACE_CHIP_ID: &str = "loa-composer-workspace";
/// Height at which the editor stops growing and scrolls.
pub const MAX_EDITOR_HEIGHT: f32 = 240.0;
/// Undo steps kept.
pub const UNDO_DEPTH: usize = 200;
/// Most items a popup matches, and the rows shown at once.
const MAX_ITEMS: usize = 50;
const VISIBLE_POPUP_ROWS: usize = 8;
/// Tooltips of the icon-only buttons.
pub const SEND_LABEL: &str = "Send (Enter)";
pub const STOP_LABEL: &str = "Stop (Esc)";
pub const REMOVE_QUEUED_LABEL: &str = "Remove queued message";
pub const MENTION_LABEL: &str = "Mention a file (@)";
const FOLDER_ACTIONS: [&str; 2] = ["Choose another folder…", "Show in Files"];

/// A `/` command offered in the popup; `name` has no slash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    pub description: String,
}

/// A model the session can switch to; `id` is what the harness's `set_model` takes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelOption {
    pub id: String,
    pub name: String,
    pub provider: String,
    /// Model role this model fills (`default`, `smol`, …).
    pub role: Option<String>,
}

/// A row under the model choices that opens something instead of picking a model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerAction {
    pub id: String,
    pub label: String,
}

/// A harness prompts can be routed to; unavailable ones are listed, not picked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HarnessOption {
    pub id: String,
    pub label: String,
    pub available: bool,
    /// Muted note beside the label, e.g. `not installed`.
    pub detail: Option<String>,
}

/// A permission level: what the agent may do without asking.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessOption {
    pub id: String,
    pub label: String,
    pub detail: String,
}

/// The folder a session works in: `path` shortened for display, `full` on hover.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Workspace {
    pub path: String,
    pub full: String,
    pub branch: Option<String>,
}

/// A plain-text picker option (effort levels, modes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Label(pub String);

/// A picker option the composer lists: an id for [`State::pick_key`] and whether it can be picked.
trait Opt: PickerItem {
    fn key(&self) -> &str;
    fn enabled(&self) -> bool;
}

macro_rules! opt {
    ($t:ty, |$s:ident| $key:expr, $label:expr, $detail:expr, $enabled:expr) => {
        #[allow(unused_variables)]
        impl PickerItem for $t {
            fn label(&self) -> Cow<'_, str> { let $s = self; Cow::Borrowed($label) }
            fn detail(&self) -> Option<Cow<'_, str>> { let $s = self; $detail }
        }
        #[allow(unused_variables)]
        impl Opt for $t {
            fn key(&self) -> &str { let $s = self; $key }
            fn enabled(&self) -> bool { let $s = self; $enabled }
        }
    };
}
opt!(HarnessOption, |s| &s.id, &s.label, s.detail.as_deref().map(Cow::Borrowed), s.available);
opt!(ModelOption, |s| &s.id, &s.name, Some(s.role.as_ref().map_or(Cow::Borrowed(s.provider.as_str()), |r| Cow::Owned(format!("{} · {r}", s.provider)))), true);
opt!(AccessOption, |s| &s.id, &s.label, Some(Cow::Borrowed(s.detail.as_str())), true);
opt!(Workspace, |s| &s.full, &s.path, s.branch.as_deref().map(Cow::Borrowed), true);
opt!(Label, |s| &s.0, &s.0, None, true);

/// One of the composer's pickers.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PickerId { Harness, Model, Effort, Access, Mode, Folder }

/// What the app must act on.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Start this prompt as a turn: typed while idle, or the next queued message released at a
    /// turn end. `@` mentions of known files are attachments.
    Submit(Prompt),
    /// Interrupt the running turn; queued messages went back into the editor.
    Stop,
    /// The queue changed; holds the queued texts.
    QueueChanged(Vec<String>),
    /// The `@` popup opened: pass the project's files to [`State::set_files`].
    FilesRequested { query: String },
    ModelChanged(ModelOption),
    EffortChanged(String),
    ModeChanged(SessionMode),
    /// A permission level was picked, by id.
    AccessChanged(String),
    HarnessChanged(String),
    /// A [`State::set_model_actions`] row was pressed, by id.
    ModelAction(String),
    /// "Show in Files" in the folder menu.
    OpenWorkspace,
    /// A folder was picked from the folder menu, by full path.
    FolderPicked(String),
    /// "Choose another folder…": open a folder dialog.
    BrowseFolders,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Edit(Action),
    /// Enter in the editor.
    Submit,
    /// The send button.
    Send,
    Interrupt,
    Undo,
    Redo,
    PopupStep(isize),
    /// Accept the highlighted popup row, or row `n`.
    PopupAccept(Option<usize>),
    PopupClose,
    /// Edit a queued message's text, or remove it with `None`.
    Queue(u64, Option<String>),
    /// Open or close a picker's menu.
    Toggle(PickerId),
    /// Row `n` of a picker's menu: its options, then its action rows.
    Pick(PickerId, usize),
    /// A press outside the open menu, or Escape.
    PickerClosed,
    /// Whether an input method is composing text.
    Composing(bool),
    /// The `+` button: starts an `@` mention at the cursor.
    MentionPressed,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase { Idle, Starting, Running }

#[derive(Debug, Default)]
struct Pickers {
    harness: Picker<HarnessOption>,
    model: Picker<ModelOption>,
    model_actions: Vec<PickerAction>,
    effort: Picker<Label>,
    access: Picker<AccessOption>,
    mode: Picker<Label>,
    folder: Picker<Workspace>,
    folders: Vec<Workspace>,
    open: Option<PickerId>,
}

/// What the view needs of any picker.
trait Menu {
    fn len(&self) -> usize;
    fn row(&self, i: usize) -> (Cow<'_, str>, Option<Cow<'_, str>>, bool);
    fn key(&self, i: usize) -> &str;
    fn current(&self) -> Option<usize>;
}

impl<T: Opt> Menu for Picker<T> {
    fn len(&self) -> usize { self.options.len() }
    fn row(&self, i: usize) -> (Cow<'_, str>, Option<Cow<'_, str>>, bool) { (self.options[i].label(), self.options[i].detail(), self.options[i].enabled()) }
    fn key(&self, i: usize) -> &str { self.options[i].key() }
    fn current(&self) -> Option<usize> { self.selected }
}

/// Selects option `i` of `p` and returns it when that changes the selection.
fn select<T: Clone>(p: &mut Picker<T>, i: usize) -> Option<T> {
    let option = p.options.get(i).filter(|_| p.selected != Some(i))?.clone();
    p.selected = Some(i);
    Some(option)
}

fn select_by<T: Opt>(options: Vec<T>, current: Option<&str>) -> Picker<T> {
    let selected = current.and_then(|c| options.iter().position(|o| o.key() == c));
    Picker::new(options, selected)
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
    /// Paths picked from the `@` popup since the last send, resolved even if the file list moves on.
    mentions: Vec<String>,
    popup: Option<Popup>,
    /// A popup closed with Esc stays closed while its word is edited.
    dismissed: Option<Trigger>,
    pickers: Pickers,
    author: String,
}

impl Default for State {
    fn default() -> Self { Self::new() }
}

impl State {
    pub fn new() -> Self {
        let mut pickers = Pickers::default();
        pickers.mode = Picker::new(SessionMode::ALL.iter().map(|m| Label(m.as_str().to_owned())).collect(), Some(0));
        let (content, history, queue) = (Content::new(), History::new(UNDO_DEPTH), Queue::default());
        Self { content, history, composing: false, phase: Phase::Idle, queue, commands: Vec::new(), files: Vec::new(), mentions: Vec::new(), popup: None, dismissed: None, pickers, author: String::new() }
    }

    /// Folders the folder menu offers besides the current one.
    pub fn set_folders(&mut self, folders: Vec<Workspace>) {
        self.pickers.folders = folders;
        self.rebuild_folders();
    }

    /// The folder the session works in, shown above the editor.
    pub fn set_workspace(&mut self, workspace: Option<Workspace>) {
        let p = &mut self.pickers;
        p.folder.selected = workspace.is_some().then_some(0);
        p.folder.options = workspace.into_iter().collect();
        self.rebuild_folders();
    }

    fn rebuild_folders(&mut self) {
        let p = &mut self.pickers;
        p.folder.options.truncate(usize::from(p.folder.selected.is_some()));
        for folder in &p.folders {
            if !p.folder.options.iter().any(|f| f.full == folder.full) {
                p.folder.options.push(folder.clone());
            }
        }
    }

    pub fn workspace(&self) -> Option<&Workspace> { self.pickers.folder.selected() }

    /// Node key or user written to [`Prompt::by`].
    pub fn set_author(&mut self, author: impl Into<String>) { self.author = author.into() }

    pub fn set_commands(&mut self, commands: Vec<Command>) {
        self.commands = commands;
        self.refilter();
    }

    pub fn commands(&self) -> &[Command] { &self.commands }

    /// Files for the `@` popup, project relative and `/` separated.
    pub fn set_files(&mut self, files: Vec<String>) {
        self.files = files;
        self.refilter();
    }

    pub fn set_models(&mut self, models: Vec<ModelOption>, current: Option<&str>) { self.pickers.model = select_by(models, current) }

    /// Rows listed under the models, e.g. "All models…".
    pub fn set_model_actions(&mut self, actions: Vec<PickerAction>) { self.pickers.model_actions = actions }

    /// Effort levels of the current model; none hides the picker.
    pub fn set_efforts(&mut self, efforts: Vec<String>, current: Option<String>) { self.pickers.effort = select_by(efforts.into_iter().map(Label).collect(), current.as_deref()) }

    pub fn models(&self) -> (&[ModelOption], Option<&ModelOption>) { (&self.pickers.model.options, self.pickers.model.selected()) }

    pub fn efforts(&self) -> (Vec<&str>, Option<&str>) {
        let p = &self.pickers.effort;
        (p.options.iter().map(|e| e.0.as_str()).collect(), p.selected().map(|e| e.0.as_str()))
    }

    pub fn set_mode(&mut self, mode: SessionMode) { self.pickers.mode.selected = SessionMode::ALL.iter().position(|m| *m == mode) }

    /// Permission levels and the current one; none hides the picker.
    pub fn set_access(&mut self, options: Vec<AccessOption>, current: Option<&str>) { self.pickers.access = select_by(options, current) }

    /// Harnesses prompts can be routed to; the picker shows with at least two.
    pub fn set_harnesses(&mut self, harnesses: Vec<HarnessOption>, current: Option<&str>) { self.pickers.harness = select_by(harnesses, current) }

    pub fn harness(&self) -> Option<&HarnessOption> { self.pickers.harness.selected() }

    /// The message picking the option or action row with id `key`, if listed.
    pub fn pick_key(&self, picker: PickerId, key: &str) -> Option<Message> {
        let menu = self.menu(picker);
        let actions = if picker == PickerId::Model { self.pickers.model_actions.iter().map(|a| a.id.as_str()).collect() } else { Vec::new() };
        let index = (0..menu.len()).map(|i| menu.key(i)).chain(actions).position(|k| k == key)?;
        Some(Message::Pick(picker, index))
    }

    pub fn turn_started(&mut self) { self.phase = Phase::Running }

    /// The turn ended, or the last [`Event::Submit`] could not start one: releases the next queued message.
    pub fn turn_ended(&mut self) -> Option<Event> {
        self.phase = Phase::Idle;
        let next = self.queue.pop_sendable()?;
        Some(self.send(next.text.trim()))
    }

    /// Whether a turn runs or is starting.
    pub fn is_busy(&self) -> bool { self.phase != Phase::Idle }

    /// Replaces the text, cursor at the end, and focuses the editor; undo brings back the old text.
    pub fn set_text(&mut self, text: &str) -> Task<Message> {
        self.history.break_group();
        self.history.begin(EditKind::Other, self.snapshot());
        self.replace(text);
        iced::widget::operation::focus(EDITOR_ID)
    }

    fn replace(&mut self, text: &str) {
        self.content = Content::with_text(text);
        let line = self.content.line_count().saturating_sub(1);
        let column = self.content.line(line).map_or(0, |l| l.text.len());
        self.content.move_to(Cursor { position: Position { line, column }, selection: None });
    }

    pub fn text(&self) -> String { self.content.text() }

    pub fn cursor(&self) -> Cursor { self.content.cursor() }

    pub fn queue(&self) -> impl Iterator<Item = &QueuedMessage> { self.queue.items.iter() }

    pub fn popup_kind(&self) -> Option<PopupKind> { self.visible_popup().map(|p| p.trigger.kind) }

    /// Labels of the popup rows in order, with the highlighted row.
    pub fn popup_items(&self) -> (Vec<&str>, usize) {
        self.visible_popup().map_or((Vec::new(), 0), |p| (p.matches.iter().map(|&i| self.candidate(p.trigger.kind, i)).collect(), p.selected))
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let focus = matches!(message, Message::Send | Message::PopupAccept(Some(_)) | Message::MentionPressed);
        let event = match message {
            Message::Edit(action) => self.edit(action),
            Message::Submit | Message::Send => self.submit(),
            Message::Interrupt => self.stop(),
            Message::Undo => self.step(History::undo),
            Message::Redo => self.step(History::redo),
            Message::PopupStep(by) => {
                self.popup.iter_mut().for_each(|p| p.step(by));
                None
            }
            Message::PopupAccept(row) => self.accept(row),
            Message::PopupClose => {
                self.dismissed = self.popup.take().map(|p| p.trigger);
                None
            }
            Message::Queue(id, text) => self.queue.edit(id, text).then(|| self.queue_changed()),
            Message::Toggle(picker) => {
                self.pickers.open = (self.pickers.open != Some(picker)).then_some(picker);
                None
            }
            Message::Pick(picker, index) => {
                self.pickers.open = None;
                self.pick(picker, index)
            }
            Message::PickerClosed => {
                self.pickers.open = None;
                None
            }
            Message::Composing(composing) => {
                self.composing = composing;
                None
            }
            Message::MentionPressed => self.start_mention(),
        };
        (if focus { iced::widget::operation::focus(EDITOR_ID) } else { Task::none() }, event)
    }

    fn pick(&mut self, picker: PickerId, i: usize) -> Option<Event> {
        let p = &mut self.pickers;
        match picker {
            PickerId::Harness if !p.harness.options.get(i)?.available => None,
            PickerId::Harness => select(&mut p.harness, i).map(|h| Event::HarnessChanged(h.id)),
            PickerId::Model if i >= p.model.options.len() => Some(Event::ModelAction(p.model_actions.get(i - p.model.options.len())?.id.clone())),
            PickerId::Model => select(&mut p.model, i).map(Event::ModelChanged),
            PickerId::Effort => select(&mut p.effort, i).map(|e| Event::EffortChanged(e.0)),
            PickerId::Access => select(&mut p.access, i).map(|a| Event::AccessChanged(a.id)),
            PickerId::Mode => select(&mut p.mode, i).map(|_| Event::ModeChanged(SessionMode::ALL[i])),
            PickerId::Folder => match i.checked_sub(p.folder.options.len()) {
                None => Some(Event::FolderPicked(p.folder.options[i].full.clone())),
                Some(0) => Some(Event::BrowseFolders),
                Some(1) => Some(Event::OpenWorkspace),
                Some(_) => None,
            },
        }
    }

    fn menu(&self, picker: PickerId) -> &dyn Menu {
        let p = &self.pickers;
        match picker {
            PickerId::Harness => &p.harness,
            PickerId::Model => &p.model,
            PickerId::Effort => &p.effort,
            PickerId::Access => &p.access,
            PickerId::Mode => &p.mode,
            PickerId::Folder => &p.folder,
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let ctx = KeyContext { popup_open: self.visible_popup().is_some(), composing: self.composing, busy: self.is_busy() };
        let placeholder = match self.harness() {
            _ if self.is_busy() => "Queue a message…".to_owned(),
            Some(harness) => format!("Ask {} anything. Type / for commands", harness.label),
            None => "Ask anything. Type / for commands".to_owned(),
        };
        let editor = iced::widget::text_editor(&self.content)
            .id(EDITOR_ID)
            .placeholder(placeholder)
            .on_action(Message::Edit)
            .height(Length::Shrink)
            .min_height(2.6 * ts(T::Body))
            .max_height(MAX_EDITOR_HEIGHT)
            .padding([sp(Sp::S), sp(Sp::S)])
            .size(ts(T::Body))
            .style(editor_style)
            .key_binding(move |press| input::bind(press, ctx));

        let (glyph, size, on_press, label) = if self.is_busy() {
            (Icon::Square, ts(T::Caption), Some(Message::Interrupt), STOP_LABEL)
        } else {
            (Icon::ArrowUp, ts(T::Heading), (!self.content.text().trim().is_empty()).then_some(Message::Send), SEND_LABEL)
        };
        let tint = kit::tint(if on_press.is_some() { Tone::OnAccent } else { Tone::Faint });
        let action = button(icon(glyph, size).style(tint)).width(2.0 * ts(T::Body)).height(2.0 * ts(T::Body)).padding(ts(T::Body) - size / 2.0);
        let mention = button(icon(Icon::Plus, ts(T::Heading)).style(kit::tint(Tone::Muted))).padding(sp(Sp::Xs)).style(kit::button(ButtonKind::Ghost));
        let p = &self.pickers;
        let chips = [
            (PickerId::Harness, Icon::Bot, p.harness.len() > 1, self.harness().map_or("Harness".into(), |h| h.label.clone())),
            (PickerId::Model, Icon::Sparkles, p.model.len() + p.model_actions.len() > 0, p.model.selected().map_or("Model".into(), |m| short_model_name(&m.name))),
            (PickerId::Effort, Icon::Brain, p.effort.len() > 0, capitalized(p.effort.selected().map_or("Effort", |e| e.0.as_str()))),
            (PickerId::Access, Icon::Shield, p.access.len() > 0, p.access.selected().map_or("Permissions".into(), |a| a.label.clone())),
            (PickerId::Mode, Icon::FileText, true, capitalized(p.mode.selected().map_or("", |m| m.0.as_str()))),
        ];
        let controls = row![labelled(mention.on_press(Message::MentionPressed), MENTION_LABEL)].spacing(sp(Sp::Xs)).align_y(Center);
        let controls = controls.extend(chips.into_iter().filter(|c| c.2).map(|(id, glyph, _, label)| self.chip(id, glyph, label)));
        let controls = controls.push(space::horizontal()).push(labelled(action.style(kit::button(ButtonKind::Accent)).on_press_maybe(on_press), label));
        let controls = container(controls).padding(iced::Padding { top: sp(Sp::Xs), ..iced::Padding::new(sp(Sp::M)) });
        let editor = container(editor).padding(iced::Padding { bottom: 0.0, ..iced::Padding::new(sp(Sp::M)) });
        let frame = container(column![editor, controls]).width(Length::Fill).style(kit::container(Surface::Raised));
        // Fixed slots (empty spaces when unused) keep the editor's place and state in the tree;
        // menus float over the page above everything instead of taking room.
        let queue: Element<'_, Message> = if self.queue.items.is_empty() { space().into() } else { self.queue_view() };
        let menu = match p.open {
            Some(picker) => Some(self.menu_view(picker)),
            None => self.visible_popup().map(|popup| self.popup_view(popup)),
        };
        let bar: Element<'_, Message> = self.workspace().map_or_else(|| space().into(), |w| self.folder_bar(w));
        popover(column![queue, bar, frame].spacing(sp(Sp::S)), menu).into()
    }

    /// A ghost picker chip: a muted icon, the current choice and a chevron.
    fn chip<'a>(&self, picker: PickerId, glyph: Icon, label: String) -> Element<'a, Message> {
        let open = self.pickers.open == Some(picker);
        let chevron = icon(if open { Icon::ChevronUp } else { Icon::ChevronDown }, ts(T::Caption)).style(kit::tint(Tone::Faint));
        let content = row![icon(glyph, ts(T::Body)).style(kit::tint(Tone::Muted)), text(label).size(ts(T::Label)), chevron].spacing(sp(Sp::S)).align_y(Center);
        button(content).padding([sp(Sp::Xs), sp(Sp::S)]).style(kit::button(ButtonKind::Tab { active: open })).on_press(Message::Toggle(picker)).into()
    }

    /// The open picker's rows: its options (the current one checked, disabled ones inert), then
    /// its action rows.
    fn menu_view(&self, picker: PickerId) -> Element<'_, Message> {
        let menu = self.menu(picker);
        let two_line = picker == PickerId::Access;
        let actions: Vec<&str> = match picker {
            PickerId::Model => self.pickers.model_actions.iter().map(|a| a.label.as_str()).collect(),
            PickerId::Folder => FOLDER_ACTIONS.to_vec(),
            _ => Vec::new(),
        };
        let options = (0..menu.len()).map(|i| {
            let (label, detail, enabled) = menu.row(i);
            option_row(label, detail, menu.current() == Some(i), two_line, enabled.then_some(Message::Pick(picker, i)))
        });
        let actions = actions.into_iter().enumerate().map(|(n, a)| option_row(a.into(), None, false, false, Some(Message::Pick(picker, menu.len() + n))));
        let wide = matches!(picker, PickerId::Access | PickerId::Folder);
        let rows = column(options.chain(actions)).spacing(sp(Sp::Xs));
        container(rows).padding(sp(Sp::Xs)).width(width(if wide { LayoutRole::ListWidth } else { LayoutRole::NavWidth })).style(kit::container(Surface::Popup)).into()
    }

    /// The bar above the editor: the folder and git branch; opens the folder menu.
    fn folder_bar<'a>(&'a self, workspace: &'a Workspace) -> Element<'a, Message> {
        let open = self.pickers.open == Some(PickerId::Folder);
        let mut chip = row![icon(Icon::Folder, ts(T::Body)).style(kit::tint(Tone::Muted)), text(workspace.path.as_str()).size(ts(T::Label))].spacing(sp(Sp::S)).align_y(Center);
        if let Some(branch) = &workspace.branch {
            chip = chip.push(icon(Icon::GitFork, ts(T::Label)).style(kit::tint(Tone::Faint))).push(text(branch.as_str()).size(ts(T::Label)).style(kit::text(Tone::Faint)));
        }
        let chip = chip.push(icon(if open { Icon::ChevronUp } else { Icon::ChevronDown }, ts(T::Caption)).style(kit::tint(Tone::Faint)));
        let chip = button(chip).padding([sp(Sp::Xs), sp(Sp::S)]).style(kit::button(ButtonKind::Tab { active: open })).on_press(Message::Toggle(PickerId::Folder));
        // The full path on hover, except while the menu is open: the tip would cover its last row.
        let chip = if open { chip.into() } else { tip(chip, workspace.full.as_str()) };
        container(chip).id(WORKSPACE_CHIP_ID).into()
    }

    /// Tracks input method composition so Enter and Esc reach the IME; Esc closes a menu.
    pub fn subscription(&self) -> Subscription<Message> {
        let ime = iced::event::listen_with(|event, _, _| match event {
            iced::Event::InputMethod(input_method::Event::Preedit(content, _)) => Some(Message::Composing(!content.is_empty())),
            iced::Event::InputMethod(input_method::Event::Commit(_) | input_method::Event::Closed | input_method::Event::Opened) => Some(Message::Composing(false)),
            _ => None,
        });
        if self.pickers.open.is_none() {
            return ime;
        }
        let escape = iced::event::listen_with(|event, _, _| match event {
            iced::Event::Keyboard(keyboard::Event::KeyPressed { key: Key::Named(Named::Escape), .. }) => Some(Message::PickerClosed),
            _ => None,
        });
        Subscription::batch([ime, escape])
    }

    fn queue_view(&self) -> Element<'_, Message> {
        let rows = self.queue.items.iter().map(|item| {
            let id = item.id;
            let input = text_input("Empty message is skipped", &item.text).on_input(move |text| Message::Queue(id, Some(text))).size(ts(T::Label)).width(Length::Fill);
            let remove = button(muted(Icon::X, ts(T::Body))).padding(sp(Sp::Xs)).style(kit::button(ButtonKind::Row { selected: false })).on_press(Message::Queue(id, None));
            row![text("Queued").size(ts(T::Caption)).style(kit::text(Tone::Muted)), input, labelled(remove, REMOVE_QUEUED_LABEL)].spacing(sp(Sp::S)).align_y(Center).into()
        });
        container(column(rows).spacing(sp(Sp::Xs))).padding(sp(Sp::S)).style(kit::container(Surface::Popup)).width(Length::Fill).into()
    }

    fn popup_view<'a>(&'a self, popup: &'a Popup) -> Element<'a, Message> {
        let first = popup.selected.saturating_sub(VISIBLE_POPUP_ROWS - 1);
        let rows = popup.matches.iter().enumerate().skip(first).take(VISIBLE_POPUP_ROWS).map(|(n, &index)| {
            let label = match popup.trigger.kind {
                PopupKind::Command => {
                    let command = &self.commands[index];
                    row![text(format!("/{}", command.name)).size(ts(T::Body)), text(&command.description).size(ts(T::Caption)).style(kit::text(Tone::Muted))].spacing(sp(Sp::M))
                }
                PopupKind::File => row![muted(Icon::FileText, ts(T::Body)), text(&self.files[index]).size(ts(T::Body))].spacing(sp(Sp::S)),
            };
            let style = kit::button(ButtonKind::Row { selected: n == popup.selected });
            button(label.align_y(Center)).padding(sp(Sp::S)).style(style).width(Length::Fill).on_press(Message::PopupAccept(Some(n))).into()
        });
        let title = if popup.trigger.kind == PopupKind::Command { "Commands" } else { "Files" };
        let header = container(text(title).size(ts(T::Caption)).style(kit::text(Tone::Muted))).padding([sp(Sp::Xs), sp(Sp::S)]);
        let body = column![header, column(rows).spacing(sp(Sp::Xs))].spacing(sp(Sp::Xs));
        container(body).padding(sp(Sp::Xs)).style(kit::container(Surface::Popup)).width(Length::Fill).into()
    }

    fn visible_popup(&self) -> Option<&Popup> { self.popup.as_ref().filter(|p| !p.matches.is_empty()) }

    fn candidate(&self, kind: PopupKind, index: usize) -> &str {
        match kind {
            PopupKind::Command => &self.commands[index].name,
            PopupKind::File => &self.files[index],
        }
    }

    fn snapshot(&self) -> Snapshot { Snapshot { text: self.content.text(), cursor: self.content.cursor() } }

    fn edit(&mut self, action: Action) -> Option<Event> {
        match &action {
            Action::Scroll { .. } => self.content.perform(action),
            Action::Edit(edit) => {
                self.pickers.open = None;
                let kind = match edit {
                    Edit::Insert(c) => EditKind::Insert { whitespace: c.is_whitespace() },
                    Edit::Backspace | Edit::Delete => EditKind::Delete,
                    _ => EditKind::Other,
                };
                if self.history.continues(kind) {
                    self.content.perform(action);
                    self.history.extend(kind);
                } else {
                    let before = self.snapshot();
                    self.content.perform(action);
                    if self.content.text() == before.text { self.history.break_group() } else { self.history.begin(kind, before) }
                }
            }
            _ => {
                self.pickers.open = None;
                self.history.break_group();
                self.content.perform(action);
            }
        }
        self.refresh_popup()
    }

    fn step(&mut self, step: fn(&mut History, Snapshot) -> Option<Snapshot>) -> Option<Event> {
        let current = self.snapshot();
        let target = step(&mut self.history, current)?;
        self.content = Content::with_text(&target.text);
        self.content.move_to(target.cursor);
        self.refresh_popup()
    }

    /// Types an `@` at the cursor, after a space when it follows a word.
    fn start_mention(&mut self) -> Option<Event> {
        let cursor = self.content.cursor();
        let line = self.content.line(cursor.position.line);
        let after_word = cursor.selection.is_none() && line.and_then(|l| l.text.get(..cursor.position.column)?.chars().next_back()).is_some_and(|c| !c.is_whitespace());
        self.edit(Action::Edit(Edit::Paste(Arc::new(if after_word { " @" } else { "@" }.to_owned()))))
    }

    fn submit(&mut self) -> Option<Event> {
        let message = self.content.text().trim().to_owned();
        if message.is_empty() {
            return None;
        }
        self.content = Content::new();
        self.history.clear();
        (self.popup, self.dismissed) = (None, None);
        if self.is_busy() {
            self.queue.push(message);
            return Some(self.queue_changed());
        }
        Some(self.send(&message))
    }

    /// Builds the prompt for `message` and marks the turn as starting.
    fn send(&mut self, message: &str) -> Event {
        let attachments = input::resolve(message, |path| self.mentions.iter().chain(&self.files).any(|m| m == path));
        if self.queue.items.is_empty() && self.content.is_empty() {
            self.mentions.clear();
        }
        self.phase = Phase::Starting;
        Event::Submit(Prompt { text: message.to_owned(), attachments, by: self.author.clone() })
    }

    /// Interrupts the turn and moves queued messages back into the editor ahead of what is typed.
    fn stop(&mut self) -> Option<Event> {
        if !self.is_busy() {
            return None;
        }
        let queued: Vec<String> = self.queue.items.drain(..).map(|m| m.text).collect();
        if !queued.is_empty() {
            let before = self.snapshot();
            let text = queued.into_iter().chain((!before.text.trim().is_empty()).then(|| before.text.clone())).collect::<Vec<_>>().join("\n\n");
            self.history.break_group();
            self.history.begin(EditKind::Other, before);
            self.replace(&text);
            self.popup = None;
        }
        Some(Event::Stop)
    }

    fn queue_changed(&self) -> Event { Event::QueueChanged(self.queue.items.iter().map(|m| m.text.clone()).collect()) }

    /// Replaces the popup word with the chosen item.
    fn accept(&mut self, row: Option<usize>) -> Option<Event> {
        let popup = self.visible_popup()?;
        let index = *popup.matches.get(row.unwrap_or(popup.selected))?;
        let trigger = popup.trigger.clone();
        let replacement = match trigger.kind {
            PopupKind::Command => format!("/{} ", self.commands[index].name),
            PopupKind::File => {
                let path = self.files[index].clone();
                let token = input::token(&path);
                if !self.mentions.contains(&path) {
                    self.mentions.push(path);
                }
                format!("{token} ")
            }
        };
        let before = self.snapshot();
        self.history.break_group();
        let at = |column| Position { line: trigger.line, column };
        self.content.move_to(Cursor { position: at(trigger.end), selection: Some(at(trigger.start)) });
        self.content.perform(Action::Edit(Edit::Paste(Arc::new(replacement))));
        self.history.begin(EditKind::Other, before);
        (self.popup, self.dismissed) = (None, None);
        None
    }

    /// Opens, updates or closes the popup for the word under the cursor.
    fn refresh_popup(&mut self) -> Option<Event> {
        let cursor = self.content.cursor();
        let line = self.content.line(cursor.position.line).filter(|_| cursor.selection.is_none());
        let Some(trigger) = line.and_then(|l| input::detect(cursor.position.line, &l.text, cursor.position.column)) else {
            (self.popup, self.dismissed) = (None, None);
            return None;
        };
        if self.dismissed.as_ref().is_some_and(|d| d.same_anchor(&trigger)) {
            self.popup = None;
            return None;
        }
        self.dismissed = None;
        let previous = self.popup.take().filter(|p| p.trigger.same_anchor(&trigger));
        let event = (previous.is_none() && trigger.kind == PopupKind::File).then(|| Event::FilesRequested { query: trigger.query.clone() });
        let selected = previous.filter(|p| p.trigger.query == trigger.query).map_or(0, |p| p.selected);
        let matches = self.matches(&trigger);
        self.popup = Some(Popup { selected: selected.min(matches.len().saturating_sub(1)), trigger, matches });
        event
    }

    /// Recomputes the open popup's matches after its candidates changed.
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
            PopupKind::Command => loa_ui_kit::text::filter(&trigger.query, self.commands.iter().map(|c| c.name.as_str()), MAX_ITEMS),
            PopupKind::File => loa_ui_kit::text::filter(&trigger.query, self.files.iter().map(String::as_str), MAX_ITEMS),
        }
    }
}

/// A menu row: the choice, a muted note (beside it, or under it when `two_line`), and a check
/// on the current one. Without `pick` the row is listed but inert.
fn option_row<'a>(label: Cow<'a, str>, detail: Option<Cow<'a, str>>, current: bool, two_line: bool, pick: Option<Message>) -> Element<'a, Message> {
    let detail = detail.map(|d| text(d).size(ts(T::Caption)).style(kit::text(Tone::Muted)));
    let label = text(label).size(ts(T::Body));
    let content = if two_line { row![column![label].push(detail).spacing(sp(Sp::Xs)).width(Length::Fill)] } else { row![label].push(detail).push(space::horizontal()) };
    let content = content.push(current.then(|| icon(Icon::Check, ts(T::Body)))).spacing(sp(Sp::S)).align_y(Center);
    button(content).padding(sp(Sp::S)).width(Length::Fill).style(kit::button(ButtonKind::Row { selected: false })).on_press_maybe(pick).into()
}

/// `Claude Opus 5.5` reads `Opus 5.5`.
fn short_model_name(name: &str) -> String { name.strip_prefix("Claude ").filter(|rest| !rest.trim().is_empty()).unwrap_or(name).to_owned() }

/// `medium` reads `Medium`.
fn capitalized(label: &str) -> String {
    let mut chars = label.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

fn tip<'a>(content: impl Into<Element<'a, Message>>, label: &'a str) -> Element<'a, Message> {
    let tip = container(text(label).size(ts(T::Caption))).padding([sp(Sp::Xs), sp(Sp::S)]).style(kit::container(Surface::Popup));
    tooltip(content, tip, tooltip::Position::Top).into()
}

/// An icon-only button with its label as a tooltip.
fn labelled<'a>(button: iced::widget::Button<'a, Message>, label: &'static str) -> Element<'a, Message> { tip(button, label) }

/// The editor inside the composer box: no fill and no edge of its own.
fn editor_style(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    text_editor::Style { background: Background::Color(Color::TRANSPARENT), border: Border::default(), ..kit::editor(theme, status) }
}
