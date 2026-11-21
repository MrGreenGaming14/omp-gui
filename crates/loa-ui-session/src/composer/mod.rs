//! Message composer: an iced `text_editor` with Enter to send and Shift+Enter for a newline,
//! undo and redo, word deletes, growth up to a maximum height, `/` command and `@` file popups,
//! token highlighting (commands, mentions, `[Image #n]` and omp's shimmering magic keywords),
//! pasted or dropped images shown as cards and sent as attachments, kit [`Picker`]s (harness,
//! model, effort slider, permissions, mode, project, folder) anchored to their buttons, and a
//! stop button with a queue for messages written mid turn. The app feeds it plain
//! [`loa_view::session::composer`] rows and acts on its [`Intent`]s.

mod highlight;
mod input;

pub use input::{PopupKind, QueuedMessage};

use iced::Alignment::Center;
use iced::advanced::input_method;
use iced::keyboard::{self, Key, key::Named};
use iced::widget::text_editor::{self, Action, Content, Cursor, Edit, Position};
use iced::widget::{button, column, container, row, slider, space, stack, text, text_input, tooltip};
use iced::{Background, Border, Color, ContentFit, Element, Length, Padding, Subscription, Task, Theme};
use input::{EditKind, History, KeyContext, Popup, Queue, Snapshot, Trigger};
use loa_ui_kit::icons::{colored, icon, muted};
use loa_ui_kit::style::{self as kit, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{ColorRole, LayoutRole, SpaceRole as Sp, TextRole as T, for_theme, sp, text_size as ts, width};
use loa_ui_kit::widgets::{Picker, popover};
use loa_view::session::composer::{AccessOption, Command, HarnessOption, Intent, Label, ModelOption, PastedImage, PickerAction, Workspace};
use loa_view::session::{Attachment, Prompt, SessionMode};
use loa_view::theme::Icon;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Widget ids of the editor and the folder chip.
pub const EDITOR_ID: &str = "loa-composer-editor";
pub const WORKSPACE_CHIP_ID: &str = "loa-composer-workspace";
/// Height at which the editor stops growing and scrolls.
pub const MAX_EDITOR_HEIGHT: f32 = 240.0;
pub const UNDO_DEPTH: usize = 200;
/// Most items a popup matches, and the rows it shows at once.
const MAX_ITEMS: usize = 50;
const VISIBLE_POPUP_ROWS: usize = 8;
/// Tooltips of the icon-only buttons.
pub const SEND_LABEL: &str = "Send (Enter)";
pub const STOP_LABEL: &str = "Stop (Esc)";
pub const REMOVE_QUEUED_LABEL: &str = "Remove queued message";
pub const MENTION_LABEL: &str = "Mention a file (@)";
/// The project chip's label and menu row for a session in no project.
pub const NO_PROJECT: &str = "No project";
const FOLDER_ACTIONS: [&str; 2] = ["Choose another folder…", "Show in Files"];

/// A listed picker option: its label and note, an id for [`State::pick_key`] and whether it can
/// be picked.
trait Opt {
    fn label(&self) -> Cow<'_, str>;
    fn detail(&self) -> Option<Cow<'_, str>>;
    fn key(&self) -> &str;
    fn enabled(&self) -> bool;
}

macro_rules! opt {
    ($t:ty, |$s:ident| $key:expr, $label:expr, $detail:expr, $enabled:expr) => {
        #[allow(unused_variables)]
        impl Opt for $t {
            fn label(&self) -> Cow<'_, str> { let $s = self; Cow::Borrowed($label) }
            fn detail(&self) -> Option<Cow<'_, str>> { let $s = self; $detail }
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
pub enum PickerId { Harness, Model, Effort, Access, Mode, Folder, Project }

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Edit(Action),
    /// Enter in the editor, and the send button.
    Submit,
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
    Toggle(PickerId),
    /// Row `n` of a picker's menu: its options, then its action rows.
    Pick(PickerId, usize),
    /// A press outside the open menu, or Escape.
    PickerClosed,
    /// Whether an input method is composing text.
    Composing(bool),
    /// The `+` button: starts an `@` mention at the cursor.
    MentionPressed,
    /// Ctrl+V was pressed; the editor pastes any text itself.
    PasteImage,
    /// The × on a pasted image's card.
    RemoveImage(u32),
    /// A shimmer step of the magic keyword gradient.
    Shimmer,
    /// The window gained or lost focus; the shimmer runs only while focused.
    WindowFocused(bool),
    /// The effort slider moved to this level index.
    EffortSlid(u8),
    /// The effort slider was let go: its level applies.
    EffortReleased,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase { Idle, Starting, Running }

#[derive(Debug, Default)]
pub(crate) struct Pickers {
    harness: Picker<HarnessOption>,
    model: Picker<ModelOption>,
    model_actions: Vec<PickerAction>,
    effort: Picker<Label>,
    access: Picker<AccessOption>,
    mode: Picker<Label>,
    folder: Picker<Workspace>,
    folders: Vec<Workspace>,
    /// [`NO_PROJECT`], then the project names; the chip shows even with none.
    project: Picker<Label>,
    pub(crate) open: Option<PickerId>,
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
    Picker { options, selected, open: false }
}

/// The selected option of `p`.
fn chosen<T>(p: &Picker<T>) -> Option<&T> { p.options.get(p.selected?) }

/// An image pasted or dropped since the last send, shown as `[Image #n]` in the text and as a
/// card above it.
#[derive(Clone, Debug)]
pub(crate) struct Image {
    n: u32,
    image: PastedImage,
    /// Its token is in the text, so its card shows and it is sent.
    pub(crate) shown: bool,
}

/// The composer.
pub struct State {
    content: Content,
    history: History,
    composing: bool,
    /// The editor holds text the send button can send; kept in step with the editor, so `view`
    /// does not read the text.
    can_send: bool,
    phase: Phase,
    queue: Queue,
    commands: Vec<Command>,
    files: Vec<String>,
    /// Paths picked from the `@` popup or dropped since the last send, resolved even if the file list moves on.
    mentions: Vec<String>,
    popup: Option<Popup>,
    /// A popup closed with Esc stays closed while its word is edited.
    dismissed: Option<Trigger>,
    pub(crate) pickers: Pickers,
    author: String,
    /// The text holds a magic keyword, so its gradient shimmers.
    pub(crate) keyword: bool,
    /// Shimmer step, see [`highlight::Settings::phase`].
    pub(crate) shimmer: u8,
    /// The window has focus.
    pub(crate) focused: bool,
    pub(crate) images: Vec<Image>,
    /// Number of the last image added since the last send.
    next_image: u32,
    /// The effort level index under the slider while it is dragged.
    effort_drag: Option<u8>,
}

impl Default for State {
    fn default() -> Self { Self::new() }
}

impl State {
    pub fn new() -> Self {
        let mode = SessionMode::ALL.iter().map(|m| Label(m.as_str().to_owned())).collect();
        let project = Picker { options: vec![Label(NO_PROJECT.to_owned())], selected: Some(0), open: false };
        let pickers = Pickers { mode: Picker { options: mode, selected: Some(0), open: false }, project, ..Pickers::default() };
        let (content, history, queue) = (Content::new(), History::new(UNDO_DEPTH), Queue::default());
        Self {
            content,
            history,
            composing: false,
            can_send: false,
            phase: Phase::Idle,
            queue,
            commands: Vec::new(),
            files: Vec::new(),
            mentions: Vec::new(),
            popup: None,
            dismissed: None,
            pickers,
            author: String::new(),
            keyword: false,
            shimmer: 0,
            focused: true,
            images: Vec::new(),
            next_image: 0,
            effort_drag: None,
        }
    }

    /// Folders the folder menu offers besides the current one.
    pub fn set_folders(&mut self, folders: Vec<Workspace>) {
        self.pickers.folders = folders;
        self.rebuild_folders();
    }

    /// The folder the session works in, shown above the editor.
    pub fn set_workspace(&mut self, workspace: Option<Workspace>) {
        self.pickers.folder = Picker { options: workspace.into_iter().collect(), selected: None, open: false };
        self.pickers.folder.selected = (!self.pickers.folder.options.is_empty()).then_some(0);
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

    pub fn workspace(&self) -> Option<&Workspace> { chosen(&self.pickers.folder) }
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
    pub fn models(&self) -> (&[ModelOption], Option<&ModelOption>) { (&self.pickers.model.options, chosen(&self.pickers.model)) }

    pub fn efforts(&self) -> (Vec<&str>, Option<&str>) {
        let p = &self.pickers.effort;
        (p.options.iter().map(|e| e.0.as_str()).collect(), chosen(p).map(|e| e.0.as_str()))
    }

    pub fn set_mode(&mut self, mode: SessionMode) { self.pickers.mode.selected = SessionMode::ALL.iter().position(|m| *m == mode) }
    /// Permission levels and the current one; none hides the picker.
    pub fn set_access(&mut self, options: Vec<AccessOption>, current: Option<&str>) { self.pickers.access = select_by(options, current) }
    /// Harnesses prompts can be routed to; the picker shows with at least two.
    pub fn set_harnesses(&mut self, harnesses: Vec<HarnessOption>, current: Option<&str>) { self.pickers.harness = select_by(harnesses, current) }
    pub fn harness(&self) -> Option<&HarnessOption> { chosen(&self.pickers.harness) }

    /// The projects the project chip offers, and the current one (`None`: no project).
    pub fn set_projects(&mut self, projects: Vec<String>, current: Option<String>) {
        let options: Vec<Label> = std::iter::once(NO_PROJECT.to_owned()).chain(projects).map(Label).collect();
        let selected = current.map_or(Some(0), |current| options.iter().skip(1).position(|o| o.0 == current).map(|i| i + 1));
        self.pickers.project = Picker { options, selected, open: false };
    }

    /// The message picking the option or action row with id `key`, if listed.
    pub fn pick_key(&self, picker: PickerId, key: &str) -> Option<Message> {
        let menu = self.menu(picker);
        let actions = self.pickers.model_actions.iter().map(|a| a.id.as_str()).filter(|_| picker == PickerId::Model);
        Some(Message::Pick(picker, (0..menu.len()).map(|i| menu.key(i)).chain(actions).position(|k| k == key)?))
    }

    pub fn turn_started(&mut self) { self.phase = Phase::Running }

    /// The turn ended, or the last [`Intent::Submit`] could not start one: releases the next queued message.
    pub fn turn_ended(&mut self) -> Option<Intent> {
        self.phase = Phase::Idle;
        let next = self.queue.pop_sendable()?;
        Some(self.send(next.text.trim()))
    }

    /// Whether a turn runs or is starting.
    pub fn is_busy(&self) -> bool { self.phase != Phase::Idle }

    /// Follows the session now shown: busy while it runs a turn. Leaving a running session for
    /// an idle one puts the messages queued for it back into the editor, so none goes to the
    /// wrong session. A send on its way ([`Phase::Starting`]) is left alone.
    pub fn follow_session(&mut self, busy: bool) {
        match (busy, self.phase) {
            (true, _) => self.phase = Phase::Running,
            (false, Phase::Running) => {
                self.phase = Phase::Idle;
                self.unqueue();
            }
            (false, _) => {}
        }
    }

    /// The app could not start a turn for `text`, the last [`Intent::Submit`]: it goes back
    /// into the editor, ahead of anything typed since, and the composer is idle again.
    pub fn give_back(&mut self, text: &str) -> Task<Message> {
        self.phase = Phase::Idle;
        let typed = self.content.text();
        let text = if typed.trim().is_empty() { text.to_owned() } else { format!("{text}\n\n{}", typed.trim_end()) };
        self.set_text(&text)
    }

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
        self.text_changed();
    }

    /// Adds a pasted or dropped image: `[Image #n]` goes in at the cursor and its card above
    /// the text. It is sent with the message while its token is in it.
    pub fn attach_image(&mut self, image: PastedImage) -> Task<Message> {
        self.next_image += 1;
        let n = self.next_image;
        self.images.push(Image { n, image, shown: false });
        self.insert_word(&highlight::image_token(n));
        iced::widget::operation::focus(EDITOR_ID)
    }

    /// Mentions a dropped file by its path, as the `@` popup would.
    pub fn mention_path(&mut self, path: &str) -> Task<Message> {
        if !self.mentions.iter().any(|known| known == path) {
            self.mentions.push(path.to_owned());
        }
        self.insert_word(&input::token(path));
        iced::widget::operation::focus(EDITOR_ID)
    }

    pub fn text(&self) -> String { self.content.text() }
    pub fn cursor(&self) -> Cursor { self.content.cursor() }
    pub fn queue(&self) -> impl Iterator<Item = &QueuedMessage> { self.queue.items.iter() }
    pub fn popup_kind(&self) -> Option<PopupKind> { self.visible_popup().map(|p| p.trigger.kind) }

    /// Labels of the popup rows in order, with the highlighted row.
    pub fn popup_items(&self) -> (Vec<&str>, usize) {
        self.visible_popup().map_or((Vec::new(), 0), |p| (p.matches.iter().map(|&i| self.candidate(p.trigger.kind, i)).collect(), p.selected))
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Intent>) {
        let focus = matches!(message, Message::Send | Message::PopupAccept(Some(_)) | Message::MentionPressed);
        if matches!(message, Message::Pick(..) | Message::PickerClosed) {
            self.pickers.open = None;
        }
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
            Message::Pick(picker, index) => self.pick(picker, index),
            Message::PickerClosed => None,
            Message::Composing(composing) => {
                self.composing = composing;
                None
            }
            Message::MentionPressed => self.start_mention(),
            Message::PasteImage => Some(Intent::PasteImage),
            Message::RemoveImage(n) => self.remove_image(n),
            Message::Shimmer => {
                self.shimmer = shimmer_phase();
                None
            }
            Message::WindowFocused(focused) => {
                self.focused = focused;
                None
            }
            Message::EffortSlid(level) => {
                self.effort_drag = Some(level);
                None
            }
            Message::EffortReleased => {
                // The card stays open, so the level can be moved again.
                let level = self.effort_drag.take().map(usize::from);
                level.and_then(|i| select(&mut self.pickers.effort, i)).map(|e| Intent::EffortChanged(e.0))
            }
        };
        (if focus { iced::widget::operation::focus(EDITOR_ID) } else { Task::none() }, event)
    }

    fn pick(&mut self, picker: PickerId, i: usize) -> Option<Intent> {
        let p = &mut self.pickers;
        match picker {
            PickerId::Harness if !p.harness.options.get(i)?.available => None,
            PickerId::Harness => select(&mut p.harness, i).map(|h| Intent::HarnessChanged(h.id)),
            PickerId::Model if i >= p.model.options.len() => Some(Intent::ModelAction(p.model_actions.get(i - p.model.options.len())?.id.clone())),
            PickerId::Model => select(&mut p.model, i).map(Intent::ModelChanged),
            PickerId::Effort => select(&mut p.effort, i).map(|e| Intent::EffortChanged(e.0)),
            PickerId::Access => select(&mut p.access, i).map(|a| Intent::AccessChanged(a.id)),
            PickerId::Mode => select(&mut p.mode, i).map(|_| Intent::ModeChanged(SessionMode::ALL[i])),
            PickerId::Project => select(&mut p.project, i).map(|l| Intent::ProjectPicked((i > 0).then_some(l.0))),
            PickerId::Folder => match i.checked_sub(p.folder.options.len()) {
                None => Some(Intent::FolderPicked(p.folder.options[i].full.clone())),
                Some(0) => Some(Intent::BrowseFolders),
                Some(1) => Some(Intent::OpenWorkspace),
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
            PickerId::Project => &p.project,
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let selection = self.content.cursor().selection.is_some();
        let ctx = KeyContext { popup_open: self.visible_popup().is_some(), composing: self.composing, busy: self.is_busy(), selection };
        let placeholder = match self.harness() {
            _ if self.is_busy() => "Queue a message…".to_owned(),
            Some(harness) => format!("Ask {} anything. Type / for commands", harness.label),
            None => "Ask anything. Type / for commands".to_owned(),
        };
        let editor = iced::widget::text_editor(&self.content).id(EDITOR_ID).placeholder(placeholder).on_action(Message::Edit).height(Length::Shrink);
        let editor = editor.min_height(2.6 * ts(T::Body)).max_height(MAX_EDITOR_HEIGHT).padding(sp(Sp::S)).size(ts(T::Body)).style(editor_style).key_binding(move |press| input::bind(press, ctx));
        let editor = editor.highlight_with::<highlight::Highlighter>(highlight::Settings { phase: self.shimmer }, highlight::format);
        let (glyph, size, on_press, label) = match self.is_busy() {
            true => (Icon::Square, ts(T::Caption), Some(Message::Interrupt), STOP_LABEL),
            false => (Icon::ArrowUp, ts(T::Heading), self.can_send.then_some(Message::Send), SEND_LABEL),
        };
        let tint = kit::tint(if on_press.is_some() { Tone::OnAccent } else { Tone::Faint });
        let action = button(icon(glyph, size).style(tint)).width(2.0 * ts(T::Body)).height(2.0 * ts(T::Body)).padding(ts(T::Body) - size / 2.0);
        let mention = button(icon(Icon::Plus, ts(T::Heading)).style(kit::tint(Tone::Muted))).padding(sp(Sp::Xs)).style(kit::button(ButtonKind::Ghost));
        let p = &self.pickers;
        let chips = [
            (PickerId::Harness, Icon::Bot, p.harness.len() > 1, self.harness().map_or("Harness".into(), |h| h.label.clone())),
            (PickerId::Model, Icon::Sparkles, p.model.len() + p.model_actions.len() > 0, chosen(&p.model).map_or("Model".into(), |m| short_model_name(&m.name))),
            (PickerId::Effort, Icon::Brain, p.effort.len() > 0, capitalized(chosen(&p.effort).map_or("Effort", |e| e.0.as_str()))),
            (PickerId::Access, Icon::Shield, p.access.len() > 0, chosen(&p.access).map_or("Permissions".into(), |a| a.label.clone())),
        ];
        let controls = row![tip(mention.on_press(Message::MentionPressed), MENTION_LABEL)].spacing(sp(Sp::Xs)).align_y(Center);
        let controls = controls.extend(chips.into_iter().filter(|c| c.2).map(|(id, glyph, _, label)| self.chip(id, glyph, label, false)));
        let controls = controls.push(space::horizontal()).push(tip(action.style(kit::button(ButtonKind::Accent)).on_press_maybe(on_press), label));
        let controls = container(controls).padding(Padding { top: sp(Sp::Xs), ..Padding::new(sp(Sp::M)) });
        let editor = container(editor).padding(Padding { bottom: 0.0, ..Padding::new(sp(Sp::M)) });
        // Fixed slots (empty spaces when unused) keep the editor's place and state in the tree;
        // menus float over the page instead of taking room.
        let images = self.images.iter().any(|image| image.shown).then(|| self.image_cards());
        let frame = container(column![slot(images), editor, controls]).width(Length::Fill).style(kit::container(Surface::Raised));
        let queue = (!self.queue.items.is_empty()).then(|| self.queue_view());
        // Picker menus open from their own button; the `/` and `@` popups span the composer.
        let popup = self.visible_popup().filter(|_| p.open.is_none()).map(|popup| self.popup_view(popup));
        // The project and session mode sit left of the folder, small: they are set rarely.
        let project = self.chip(PickerId::Project, Icon::Files, chosen(&p.project).map_or(NO_PROJECT.to_owned(), |l| l.0.clone()), true);
        let mode = self.chip(PickerId::Mode, Icon::FileText, capitalized(chosen(&p.mode).map_or("", |m| m.0.as_str())), true);
        let bar = row![project, mode].push(self.workspace().map(|w| self.folder_bar(w))).spacing(sp(Sp::Xs)).align_y(Center);
        popover(column![slot(queue), bar, frame].spacing(sp(Sp::S)), popup).into()
    }

    /// A ghost picker chip: a muted icon, the current choice and a chevron, smaller when
    /// `compact`; its menu opens from it.
    fn chip<'a>(&'a self, picker: PickerId, glyph: Icon, label: String, compact: bool) -> Element<'a, Message> {
        let size = if compact { ts(T::Caption) } else { ts(T::Body) };
        let content = row![icon(glyph, size).style(kit::tint(Tone::Muted)), text(label).size(if compact { ts(T::Caption) } else { ts(T::Label) })].spacing(sp(Sp::S)).align_y(Center);
        self.toggle(picker, content.into(), compact)
    }

    /// A toggle for `picker`'s menu around `content`, with an open/closed chevron; the open menu
    /// floats from it and closes on a press outside.
    fn toggle<'a>(&'a self, picker: PickerId, content: Element<'a, Message>, compact: bool) -> Element<'a, Message> {
        let open = self.pickers.open == Some(picker);
        let chevron = icon(if open { Icon::ChevronUp } else { Icon::ChevronDown }, ts(T::Caption)).style(kit::tint(Tone::Faint));
        let content = row![content, chevron].spacing(if compact { sp(Sp::Xs) } else { sp(Sp::S) }).align_y(Center);
        let padding = if compact { [sp(Sp::Xs) / 2.0, sp(Sp::Xs)] } else { [sp(Sp::Xs), sp(Sp::S)] };
        let button = button(content).padding(padding).style(kit::button(ButtonKind::Tab { active: open })).on_press(Message::Toggle(picker));
        popover(button, open.then(|| self.menu_view(picker))).own_width().on_dismiss(Message::PickerClosed).into()
    }

    /// The open picker's rows: its options (the current one checked, disabled ones inert), then
    /// its action rows. Effort is a slider card instead.
    fn menu_view(&self, picker: PickerId) -> Element<'_, Message> {
        if picker == PickerId::Effort {
            return self.effort_card();
        }
        let menu = self.menu(picker);
        let actions: Vec<&str> = match picker {
            PickerId::Model => self.pickers.model_actions.iter().map(|a| a.label.as_str()).collect(),
            PickerId::Folder => FOLDER_ACTIONS.to_vec(),
            _ => Vec::new(),
        };
        let options = (0..menu.len()).map(|i| {
            let (label, detail, enabled) = menu.row(i);
            option_row(label, detail, menu.current() == Some(i), picker == PickerId::Access, enabled.then_some(Message::Pick(picker, i)))
        });
        let actions = actions.into_iter().enumerate().map(|(n, a)| option_row(a.into(), None, false, false, Some(Message::Pick(picker, menu.len() + n))));
        let menu_width = match picker {
            PickerId::Access | PickerId::Folder => width(LayoutRole::ListWidth),
            PickerId::Mode => width(LayoutRole::NavWidth) / 1.5,
            _ => width(LayoutRole::NavWidth),
        };
        let rows = container(column(options.chain(actions)).spacing(sp(Sp::Xs))).padding(sp(Sp::Xs));
        rows.width(menu_width).style(kit::container(Surface::Popup)).into()
    }

    /// The effort picker: the level in large type over the model, and a slider with a mark per
    /// level. Dragging previews; letting go applies.
    fn effort_card(&self) -> Element<'_, Message> {
        const TRACK: f32 = 30.0;
        const KNOB: f32 = 17.0;
        const DOT: f32 = 6.0;
        let p = &self.pickers;
        let levels = p.effort.options.len();
        let shown = self.effort_drag.map(usize::from).or(p.effort.selected);
        let title = shown.and_then(|i| p.effort.options.get(i)).map_or_else(|| "Effort".to_owned(), |level| capitalized(&level.0));
        let mut heading = column![text(title).size(ts(T::Heading)).style(kit::text(Tone::Accent))].spacing(sp(Sp::Xs) / 2.0).align_x(Center).width(Length::Fill);
        if let Some(model) = chosen(&p.model) {
            let name = row![text(short_model_name(&model.name)).size(ts(T::Caption)).style(kit::text(Tone::Muted)), icon(Icon::ChevronRight, ts(T::Caption)).style(kit::tint(Tone::Faint))].spacing(sp(Sp::Xs) / 2.0).align_y(Center);
            heading = heading.push(button(name).padding([1.0, sp(Sp::Xs)]).style(kit::button(ButtonKind::Ghost)).on_press(Message::Toggle(PickerId::Model)));
        }
        let header = row![icon(Icon::Brain, ts(T::Body)).style(kit::tint(Tone::Muted)), heading, space().width(ts(T::Body))].align_y(Center);
        let value = u8::try_from(shown.unwrap_or(0)).unwrap_or(u8::MAX);
        let last = u8::try_from(levels.saturating_sub(1)).unwrap_or(u8::MAX);
        let slider = slider(0..=last, value, Message::EffortSlid).step(1u8).on_release(Message::EffortReleased).height(TRACK + 6.0).style(effort_slider(TRACK, KNOB));
        // A mark at each level, centered where the knob stops; the knob's own level has none.
        let mut dots = row![].align_y(Center);
        for index in 0..levels {
            if index > 0 {
                dots = dots.push(space::horizontal());
            }
            let dot: Element<'_, Message> = if index == usize::from(value) { space().width(DOT).into() } else { container(space()).width(DOT).height(DOT).style(effort_dot(index < usize::from(value))).into() };
            dots = dots.push(dot);
        }
        let dots = container(dots).padding([0.0, KNOB - DOT / 2.0]).height(TRACK + 6.0).center_y(TRACK + 6.0).width(Length::Fill);
        let card = column![header, stack![slider, dots]].spacing(sp(Sp::M));
        container(card).padding(sp(Sp::M)).width(width(LayoutRole::NavWidth) * 1.2).style(kit::container(Surface::Popup)).into()
    }

    /// The folder chip above the editor: the folder and git branch; opens the folder menu.
    fn folder_bar<'a>(&'a self, workspace: &'a Workspace) -> Element<'a, Message> {
        let mut chip = row![icon(Icon::Folder, ts(T::Body)).style(kit::tint(Tone::Muted)), text(workspace.path.as_str()).size(ts(T::Label))].spacing(sp(Sp::S)).align_y(Center);
        if let Some(branch) = &workspace.branch {
            chip = chip.push(icon(Icon::GitFork, ts(T::Label)).style(kit::tint(Tone::Faint))).push(text(branch.as_str()).size(ts(T::Label)).style(kit::text(Tone::Faint)));
        }
        let chip = self.toggle(PickerId::Folder, chip.into(), false);
        // The full path on hover, except while the menu is open: the tip would cover its last row.
        let chip = if self.pickers.open == Some(PickerId::Folder) { chip } else { tip(chip, workspace.full.as_str()) };
        container(chip).id(WORKSPACE_CHIP_ID).into()
    }

    /// The cards of the images whose token is in the text.
    fn image_cards(&self) -> Element<'_, Message> {
        let bold = iced::Font { weight: iced::font::Weight::Bold, ..iced::Font::DEFAULT };
        let cards = self.images.iter().filter(|image| image.shown).map(|image| {
            let color = highlight::image_color(image.n);
            let remove = button(muted(Icon::X, ts(T::Caption))).padding(2).style(kit::button(ButtonKind::Ghost)).on_press(Message::RemoveImage(image.n));
            let header = row![colored(Icon::Image, ts(T::Body), color), text(format!("#{}", image.n)).size(ts(T::Caption)).font(bold).color(color), space::horizontal(), remove].spacing(sp(Sp::Xs)).align_y(Center);
            let thumbnail = iced::widget::image(iced::widget::image::Handle::from_path(&image.image.path)).width(Length::Fill).height(56).content_fit(ContentFit::Contain);
            let size = image.image.size.map_or_else(String::new, |(w, h)| format!("{w}x{h}"));
            let card = column![header, thumbnail, text(size).size(ts(T::Caption)).color(color)].spacing(sp(Sp::Xs)).align_x(Center);
            container(card).width(116).padding(sp(Sp::S)).style(move |_: &Theme| container::Style { border: Border { color, width: 1.0, radius: 8.0.into() }, ..container::Style::default() }).into()
        });
        container(row(cards).spacing(sp(Sp::S))).padding(Padding { bottom: 0.0, ..Padding::new(sp(Sp::M)) }).into()
    }

    /// Tracks input method composition so Enter and Esc reach the IME, closes an open menu on
    /// Esc, and runs the keyword shimmer while the window has focus.
    pub fn subscription(&self) -> Subscription<Message> {
        let events = iced::event::listen_with(|event, _, _| match event {
            iced::Event::InputMethod(input_method::Event::Preedit(content, _)) => Some(Message::Composing(!content.is_empty())),
            // winit sends an empty preedit right before a commit, so commits need no arm.
            iced::Event::InputMethod(input_method::Event::Closed) => Some(Message::Composing(false)),
            iced::Event::Window(iced::window::Event::Focused) => Some(Message::WindowFocused(true)),
            iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::WindowFocused(false)),
            _ => None,
        });
        let mut subscriptions = vec![events];
        if self.pickers.open.is_some() {
            subscriptions.push(iced::event::listen_with(|event, _, _| match event {
                iced::Event::Keyboard(keyboard::Event::KeyPressed { key: Key::Named(Named::Escape), .. }) => Some(Message::PickerClosed),
                _ => None,
            }));
        }
        if self.keyword && self.focused {
            let step = std::time::Duration::from_millis(highlight::SHIMMER_MS / u64::from(highlight::PHASES));
            subscriptions.push(iced::time::every(step).map(|_| Message::Shimmer));
        }
        Subscription::batch(subscriptions)
    }

    fn queue_view(&self) -> Element<'_, Message> {
        let rows = self.queue.items.iter().map(|item| {
            let id = item.id;
            let input = text_input("Empty message is skipped", &item.text).on_input(move |text| Message::Queue(id, Some(text))).size(ts(T::Label)).width(Length::Fill);
            let remove = button(muted(Icon::X, ts(T::Body))).padding(sp(Sp::Xs)).style(kit::button(ButtonKind::Row { selected: false })).on_press(Message::Queue(id, None));
            row![text("Queued").size(ts(T::Caption)).style(kit::text(Tone::Muted)), input, tip(remove, REMOVE_QUEUED_LABEL)].spacing(sp(Sp::S)).align_y(Center).into()
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
    fn candidate(&self, kind: PopupKind, index: usize) -> &str { if kind == PopupKind::Command { &self.commands[index].name } else { &self.files[index] } }
    fn snapshot(&self) -> Snapshot { Snapshot { text: self.content.text(), cursor: self.content.cursor() } }

    fn edit(&mut self, action: Action) -> Option<Intent> {
        if !matches!(action, Action::Scroll { .. }) {
            self.pickers.open = None;
        }
        let kind = match &action {
            Action::Scroll { .. } => return (self.content.perform(action), self.refresh_popup()).1,
            Action::Edit(edit) => edit_kind(edit),
            _ => {
                self.history.break_group();
                self.content.perform(action);
                return self.refresh_popup();
            }
        };
        if self.history.continues(kind) {
            self.content.perform(action);
            self.history.extend(kind);
        } else {
            let before = self.snapshot();
            let line_count = self.content.line_count();
            self.content.perform(action);
            // The text is compared only when the cursor and line count stay put.
            let moved = self.content.cursor() != before.cursor || self.content.line_count() != line_count;
            if !moved && self.content.text() == before.text { self.history.break_group() } else { self.history.begin(kind, before) }
        }
        self.text_changed();
        self.refresh_popup()
    }

    fn step(&mut self, step: fn(&mut History, Snapshot) -> Option<Snapshot>) -> Option<Intent> {
        let current = self.snapshot();
        let target = step(&mut self.history, current)?;
        self.content = Content::with_text(&target.text);
        self.content.move_to(target.cursor);
        self.text_changed();
        self.refresh_popup()
    }

    /// Types an `@` at the cursor, after a space when it follows a word.
    fn start_mention(&mut self) -> Option<Intent> {
        let mention = if self.after_word() { " @" } else { "@" };
        self.edit(Action::Edit(Edit::Paste(Arc::new(mention.to_owned()))))
    }

    /// Whether the cursor sits right after a word, with nothing selected.
    fn after_word(&self) -> bool {
        let cursor = self.content.cursor();
        let line = self.content.line(cursor.position.line);
        cursor.selection.is_none() && line.and_then(|l| l.text.get(..cursor.position.column)?.chars().next_back()).is_some_and(|c| !c.is_whitespace())
    }

    /// Inserts `word` at the cursor as a word of its own, as one undo step.
    fn insert_word(&mut self, word: &str) {
        let text = if self.after_word() { format!(" {word} ") } else { format!("{word} ") };
        self.history.break_group();
        let _ = self.edit(Action::Edit(Edit::Paste(Arc::new(text))));
        self.history.break_group();
    }

    /// Drops image `n`'s token from the text, so its card hides. The image itself stays known,
    /// so undo brings back token, card and attachment.
    fn remove_image(&mut self, n: u32) -> Option<Intent> {
        let token = highlight::image_token(n);
        let before = self.snapshot();
        if before.text.contains(&token) {
            let text = before.text.replace(&format!("{token} "), "").replace(&token, "");
            self.history.break_group();
            self.history.begin(EditKind::Other, before);
            self.replace(&text);
        }
        // The text moved under any open popup's word.
        self.refresh_popup()
    }

    /// Brings what depends on the text in step with it: the send button, the keyword shimmer
    /// and which image cards show.
    fn text_changed(&mut self) {
        self.can_send = !self.content.is_empty() && self.content.lines().any(|line| !line.text.trim().is_empty());
        self.keyword = self.content.lines().any(|line| highlight::has_keyword(&line.text));
        if !self.keyword {
            self.shimmer = 0;
        }
        for image in &mut self.images {
            let token = highlight::image_token(image.n);
            image.shown = self.content.lines().any(|line| line.text.contains(&token));
        }
    }

    fn submit(&mut self) -> Option<Intent> {
        let message = Some(self.content.text().trim().to_owned()).filter(|m| !m.is_empty())?;
        self.content = Content::new();
        self.text_changed();
        self.history.clear();
        (self.popup, self.dismissed) = (None, None);
        if self.is_busy() {
            self.queue.push(message);
            return Some(self.queue_changed());
        }
        Some(self.send(&message))
    }

    /// Builds the prompt for `message` and marks the turn as starting. Mentioned files and the
    /// images whose token it holds are attached.
    fn send(&mut self, message: &str) -> Intent {
        let mut attachments = input::resolve(message, |path| self.mentions.iter().chain(&self.files).any(|m| m == path));
        for image in &self.images {
            if message.contains(&highlight::image_token(image.n)) && !attachments.iter().any(|a| a.path == image.image.path) {
                let mime = loa_util::text::mime_of(&image.image.path).map(str::to_owned);
                attachments.push(Attachment { path: image.image.path.clone(), mime });
            }
        }
        if self.queue.items.is_empty() && self.content.is_empty() {
            self.mentions.clear();
            self.images.clear();
            self.next_image = 0;
        }
        self.phase = Phase::Starting;
        Intent::Submit(Prompt { text: message.to_owned(), attachments, by: self.author.clone() })
    }

    /// Interrupts the turn and moves queued messages back into the editor ahead of what is typed.
    fn stop(&mut self) -> Option<Intent> {
        if !self.is_busy() {
            return None;
        }
        self.unqueue();
        Some(Intent::Stop)
    }

    /// Moves queued messages back into the editor, ahead of what is typed.
    fn unqueue(&mut self) {
        let queued: Vec<String> = self.queue.items.drain(..).map(|m| m.text).collect();
        if queued.is_empty() {
            return;
        }
        let before = self.snapshot();
        let text = queued.into_iter().chain((!before.text.trim().is_empty()).then(|| before.text.clone())).collect::<Vec<_>>().join("\n\n");
        self.history.break_group();
        self.history.begin(EditKind::Other, before);
        self.replace(&text);
        self.popup = None;
    }

    fn queue_changed(&self) -> Intent { Intent::QueueChanged(self.queue.items.iter().map(|m| m.text.clone()).collect()) }

    /// Replaces the popup word with the chosen item.
    fn accept(&mut self, row: Option<usize>) -> Option<Intent> {
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
        self.text_changed();
        self.history.begin(EditKind::Other, before);
        (self.popup, self.dismissed) = (None, None);
        None
    }

    /// Opens, updates or closes the popup for the word under the cursor.
    fn refresh_popup(&mut self) -> Option<Intent> {
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
        let event = (previous.is_none() && trigger.kind == PopupKind::File).then(|| Intent::FilesRequested { query: trigger.query.clone() });
        let selected = previous.filter(|p| p.trigger.query == trigger.query).map_or(0, |p| p.selected);
        let matches = self.matches(&trigger);
        self.popup = Some(Popup { selected: selected.min(matches.len().saturating_sub(1)), trigger, matches });
        event
    }

    /// Recomputes the open popup's matches after its candidates changed.
    fn refilter(&mut self) {
        let Some(trigger) = self.popup.as_ref().map(|p| p.trigger.clone()) else { return };
        let matches = self.matches(&trigger);
        if let Some(p) = &mut self.popup {
            (p.selected, p.matches) = (p.selected.min(matches.len().saturating_sub(1)), matches);
        }
    }

    fn matches(&self, trigger: &Trigger) -> Vec<usize> {
        match trigger.kind {
            PopupKind::Command => loa_ui_kit::text::filter(&trigger.query, self.commands.iter().map(|c| c.name.as_str()), MAX_ITEMS),
            PopupKind::File => loa_ui_kit::text::filter(&trigger.query, self.files.iter().map(String::as_str), MAX_ITEMS),
        }
    }
}

/// Whether a dropped file is an image the composer attaches as a card.
pub fn is_image(path: &Path) -> bool { loa_util::text::mime_of(path).is_some_and(|mime| mime.starts_with("image/") && mime != "image/svg+xml") }

/// The pasted image at `path`, `size` pixels when known.
pub fn pasted(path: PathBuf, size: Option<(u32, u32)>) -> PastedImage { PastedImage { path, size } }

/// How an edit groups with its neighbours. An IME commit arrives as a paste; a single typed
/// character is an insert.
fn edit_kind(edit: &Edit) -> EditKind {
    match edit {
        Edit::Insert(c) => EditKind::Insert { whitespace: c.is_whitespace() },
        Edit::Paste(text) => {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c != '\n' && c != '\r' => EditKind::Insert { whitespace: c.is_whitespace() },
                _ => EditKind::Other,
            }
        }
        Edit::Backspace | Edit::Delete => EditKind::Delete,
        _ => EditKind::Other,
    }
}

/// The shimmer step for the current time, so every keyword moves in step.
fn shimmer_phase() -> u8 {
    let ms = u64::try_from(loa_util::time::now_ms()).unwrap_or_default() % highlight::SHIMMER_MS;
    u8::try_from(ms * u64::from(highlight::PHASES) / highlight::SHIMMER_MS).unwrap_or_default()
}

/// `content`, or an empty space holding its place in the widget tree.
fn slot(content: Option<Element<'_, Message>>) -> Element<'_, Message> { content.unwrap_or_else(|| space().into()) }

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

/// `content` with `label` as a tooltip above it.
fn tip<'a>(content: impl Into<Element<'a, Message>>, label: &'a str) -> Element<'a, Message> {
    let tip = container(text(label).size(ts(T::Caption))).padding([sp(Sp::Xs), sp(Sp::S)]).style(kit::container(Surface::Popup));
    tooltip(content, tip, tooltip::Position::Top).into()
}

/// The editor inside the composer box: no fill and no edge of its own.
fn editor_style(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    text_editor::Style { background: Background::Color(Color::TRANSPARENT), border: Border::default(), ..kit::editor(theme, status) }
}

/// The effort slider: a thick rounded track, filled with the accent up to a large white knob.
fn effort_slider(rail: f32, knob: f32) -> impl Fn(&Theme, slider::Status) -> slider::Style {
    move |theme, _| {
        let t = for_theme(theme);
        slider::Style {
            rail: slider::Rail {
                backgrounds: (Background::Color(t.color(ColorRole::Accent)), Background::Color(t.color(ColorRole::Raised))),
                width: rail,
                border: Border { color: t.color(ColorRole::Line), width: 1.0, radius: (rail / 2.0).into() },
            },
            handle: slider::Handle { shape: slider::HandleShape::Circle { radius: knob }, background: Background::Color(Color::WHITE), border_width: 0.0, border_color: Color::TRANSPARENT },
        }
    }
}

/// A level mark on the effort track: light on the filled part, faint on the rest.
fn effort_dot(filled: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let color = if filled { Color::WHITE.scale_alpha(0.6) } else { for_theme(theme).color(ColorRole::Text3) };
        container::Style { background: Some(Background::Color(color)), border: Border { radius: 99.0.into(), ..Border::default() }, ..container::Style::default() }
    }
}
