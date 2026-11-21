//! The composer's editing machinery: undo history (iced's editor has none), `/` and `@` popup
//! triggers, `@path` mentions, the mid-turn queue and the editor key bindings.

use super::Message;
use iced::keyboard::Key;
use iced::keyboard::key::Named;
use iced::widget::text_editor::{Binding, Cursor, KeyPress, Status};
use loa_view::session::Attachment;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// Editor text and cursor at one point in time.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Snapshot {
    pub text: String,
    pub cursor: Cursor,
}

/// How an edit groups with its neighbours: typing one word (and the whitespace after it) is one
/// group, a run of deletions another; any other edit stands alone.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum EditKind {
    Insert { whitespace: bool },
    Delete,
    Other,
}

/// Bounded undo and redo stacks holding the state before each group of edits.
#[derive(Debug)]
pub(crate) struct History {
    undo: VecDeque<Snapshot>,
    redo: Vec<Snapshot>,
    open: Option<EditKind>,
    depth: usize,
}

impl History {
    pub fn new(depth: usize) -> Self { Self { undo: VecDeque::new(), redo: Vec::new(), open: None, depth: depth.max(1) } }

    /// Whether an edit of `kind` joins the open group; a word after whitespace starts a new one.
    pub fn continues(&self, kind: EditKind) -> bool {
        match (self.open, kind) {
            (Some(EditKind::Insert { whitespace: before }), EditKind::Insert { whitespace }) => !before || whitespace,
            (Some(EditKind::Delete), EditKind::Delete) => true,
            _ => false,
        }
    }

    /// Records `before`, the state preceding a new group of `kind`; clears redo.
    pub fn begin(&mut self, kind: EditKind, before: Snapshot) {
        self.redo.clear();
        self.push_undo(before);
        self.open = (kind != EditKind::Other).then_some(kind);
    }

    fn push_undo(&mut self, s: Snapshot) {
        self.undo.push_back(s);
        if self.undo.len() > self.depth {
            self.undo.pop_front();
        }
    }

    pub fn extend(&mut self, kind: EditKind) { self.open = Some(kind) }
    pub fn break_group(&mut self) { self.open = None }

    pub fn undo(&mut self, current: Snapshot) -> Option<Snapshot> {
        self.open = None;
        let previous = self.undo.pop_back()?;
        self.redo.push(current);
        Some(previous)
    }

    pub fn redo(&mut self, current: Snapshot) -> Option<Snapshot> {
        self.open = None;
        let next = self.redo.pop()?;
        self.push_undo(current);
        Some(next)
    }

    pub fn clear(&mut self) { *self = Self::new(self.depth) }
}

/// Which popup a trigger opens.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PopupKind {
    /// `/` at the start of the message.
    Command,
    /// `@` at the start of a word.
    File,
}

/// The word under the cursor that opened a popup: byte columns `start..end` on `line`, and the
/// text between the trigger character and the cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Trigger {
    pub kind: PopupKind,
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub query: String,
}

impl Trigger {
    /// The same popup word, possibly with another query.
    pub fn same_anchor(&self, other: &Trigger) -> bool { (self.kind, self.line, self.start) == (other.kind, other.line, other.start) }
}

/// The popup trigger for the cursor at byte `column` of `line`, line number `index`.
pub(crate) fn detect(index: usize, line: &str, column: usize) -> Option<Trigger> {
    let column = column.min(line.len());
    let before = line.get(..column)?;
    let start = before.char_indices().rev().find(|(_, c)| c.is_whitespace()).map_or(0, |(i, c)| i + c.len_utf8());
    let kind = match line[start..].chars().next()? {
        '/' if index == 0 && start == 0 => PopupKind::Command,
        '@' => PopupKind::File,
        _ => return None,
    };
    let end = line[column..].find(char::is_whitespace).map_or(line.len(), |i| column + i);
    Some(Trigger { kind, line: index, start, end, query: before[start + 1..].to_owned() })
}

/// An open popup: its trigger, the matching candidate indices and the highlighted row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Popup {
    pub trigger: Trigger,
    pub matches: Vec<usize>,
    pub selected: usize,
}

impl Popup {
    /// Moves the highlight by `by` rows, wrapping.
    pub fn step(&mut self, by: isize) {
        let n = self.matches.len() as isize;
        if n > 0 {
            self.selected = (self.selected as isize + by).rem_euclid(n) as usize;
        }
    }
}

/// The text inserted for a mention of `path`, quoted when it holds whitespace.
pub(crate) fn token(path: &str) -> String { if path.contains(char::is_whitespace) { format!("@\"{path}\"") } else { format!("@{path}") } }

/// Paths written as `@path` or `@"path"` at word starts in `text`. An unquoted mention ending
/// in punctuation also yields the word without it, so `see @a.rs,` resolves `a.rs`.
pub(crate) fn mentioned(text: &str) -> Vec<&str> {
    let (mut found, mut rest, mut at_word_start) = (Vec::new(), text, true);
    while let Some(c) = rest.chars().next() {
        if c != '@' || !at_word_start {
            at_word_start = c.is_whitespace();
            rest = &rest[c.len_utf8()..];
            continue;
        }
        let after = &rest[1..];
        at_word_start = false;
        if let Some((quoted, tail)) = after.strip_prefix('"').and_then(|q| q.split_once('"')) {
            found.push(quoted);
            rest = tail;
            continue;
        }
        let end = after.find(char::is_whitespace).unwrap_or(after.len());
        let word = &after[..end];
        let trimmed = word.trim_end_matches([',', '.', ';', ':', '!', '?', ')', ']', '"', '\'']);
        found.extend([word, trimmed].into_iter().filter(|w| !w.is_empty()).take(if trimmed.len() == word.len() { 1 } else { 2 }));
        rest = &after[end..];
    }
    found
}

/// Attachments for the mentions in `text` that `is_known` accepts, once each, in order.
pub(crate) fn resolve(text: &str, is_known: impl Fn(&str) -> bool) -> Vec<Attachment> {
    let mut out: Vec<Attachment> = Vec::new();
    for path in mentioned(text) {
        if is_known(path) && !out.iter().any(|a| a.path.as_os_str() == path) {
            out.push(Attachment { path: PathBuf::from(path), mime: loa_util::text::mime_of(Path::new(path)).map(str::to_owned) });
        }
    }
    out
}

/// One message queued while a turn runs; `id` survives edits of its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedMessage {
    pub id: u64,
    pub text: String,
}

/// Messages waiting for the running turn, first in first out.
#[derive(Debug, Default)]
pub(crate) struct Queue {
    pub items: VecDeque<QueuedMessage>,
    next_id: u64,
}

impl Queue {
    pub fn push(&mut self, text: String) {
        self.items.push_back(QueuedMessage { id: self.next_id, text });
        self.next_id += 1;
    }

    /// Replaces the text of `id`, or removes it when `text` is `None`; false when it is gone.
    pub fn edit(&mut self, id: u64, text: Option<String>) -> bool {
        let Some(at) = self.items.iter().position(|m| m.id == id) else { return false };
        match text {
            Some(text) => self.items[at].text = text,
            None => drop(self.items.remove(at)),
        }
        true
    }

    /// The oldest message with non-blank text; blank ones (edited down to nothing) are dropped.
    pub fn pop_sendable(&mut self) -> Option<QueuedMessage> { std::iter::from_fn(|| self.items.pop_front()).find(|m| !m.text.trim().is_empty()) }
}

/// Composer state the key bindings depend on, copied into the editor's binding closure.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct KeyContext {
    pub popup_open: bool,
    /// An input method is composing; its keys must reach the IME.
    pub composing: bool,
    pub busy: bool,
}

/// Enter sends and Shift+Enter breaks the line, undo and redo, popup navigation, Esc interrupts.
pub(crate) fn bind(press: KeyPress, ctx: KeyContext) -> Option<Binding<Message>> {
    if !matches!(press.status, Status::Focused { .. }) {
        return None;
    }
    let named = match press.key.as_ref() {
        Key::Named(named) => Some(named),
        _ => None,
    };
    if ctx.composing && matches!(named, Some(Named::Enter | Named::Escape | Named::Tab | Named::ArrowUp | Named::ArrowDown)) {
        return None;
    }
    let m = press.modifiers;
    if m.command() {
        match press.key.to_latin(press.physical_key) {
            Some('z') if m.shift() => return Some(Binding::Custom(Message::Redo)),
            Some('z') => return Some(Binding::Custom(Message::Undo)),
            Some('y') => return Some(Binding::Custom(Message::Redo)),
            Some('c' | 'x' | 'v' | 'a') => {}
            // Other Ctrl/Cmd chords are app shortcuts. On Windows AltGr arrives as Ctrl+Alt and types.
            Some(_) if !(cfg!(windows) && m.alt()) => return None,
            _ => {}
        }
    }
    let message = match named {
        Some(Named::Enter) if m.shift() => return Some(Binding::Enter),
        Some(Named::Enter) if ctx.popup_open => Message::PopupAccept(None),
        Some(Named::Enter) => Message::Submit,
        Some(Named::Tab) if ctx.popup_open && !m.shift() => Message::PopupAccept(None),
        Some(Named::ArrowUp) if ctx.popup_open => Message::PopupStep(-1),
        Some(Named::ArrowDown) if ctx.popup_open => Message::PopupStep(1),
        Some(Named::Escape) if ctx.popup_open => Message::PopupClose,
        Some(Named::Escape) if ctx.busy => Message::Interrupt,
        _ => return Binding::from_key_press(press),
    };
    Some(Binding::Custom(message))
}
