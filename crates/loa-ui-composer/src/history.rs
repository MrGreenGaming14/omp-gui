//! Undo and redo for the composer editor, which iced's `text_editor` lacks.
//!
//! The history stores the editor state from before each group of edits.
//! Consecutive typing of one word (plus the whitespace after it) forms one
//! group, as does a run of deletions; every other edit is its own group.

use std::collections::VecDeque;

use iced::widget::text_editor::Cursor;

/// Editor text and cursor at one point in time.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub text: String,
    pub cursor: Cursor,
}

/// How an edit groups with the edits around it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EditKind {
    /// Typing one character.
    Insert { whitespace: bool },
    /// Backspace or Delete.
    Delete,
    /// Paste, newline, completion, or any other edit that stands alone.
    Other,
}

/// Bounded undo and redo stacks of [`Snapshot`]s.
#[derive(Debug)]
pub struct History {
    undo: VecDeque<Snapshot>,
    redo: Vec<Snapshot>,
    open: Option<EditKind>,
    depth: usize,
}

impl History {
    /// A history keeping at most `depth` undo steps.
    pub fn new(depth: usize) -> Self {
        Self { undo: VecDeque::new(), redo: Vec::new(), open: None, depth: depth.max(1) }
    }

    /// Whether an edit of `kind` joins the group that is still open, so it
    /// needs no snapshot of its own.
    pub fn continues(&self, kind: EditKind) -> bool {
        match (self.open, kind) {
            // A word starting after whitespace opens a new group.
            (Some(EditKind::Insert { whitespace: previous }), EditKind::Insert { whitespace }) => {
                !(previous && !whitespace)
            }
            (Some(EditKind::Delete), EditKind::Delete) => true,
            _ => false,
        }
    }

    /// Records `before`, the state preceding a new group of edits of `kind`.
    /// Clears the redo stack.
    pub fn begin(&mut self, kind: EditKind, before: Snapshot) {
        self.redo.clear();
        self.undo.push_back(before);
        if self.undo.len() > self.depth {
            self.undo.pop_front();
        }
        self.open = (kind != EditKind::Other).then_some(kind);
    }

    /// Notes that an edit of `kind` joined the open group.
    pub fn extend(&mut self, kind: EditKind) {
        self.open = Some(kind);
    }

    /// Ends the open group, so the next edit starts a new one. Called on
    /// cursor moves and clicks.
    pub fn break_group(&mut self) {
        self.open = None;
    }

    /// Steps back. `current` is the state now; returns the state to restore.
    pub fn undo(&mut self, current: Snapshot) -> Option<Snapshot> {
        self.open = None;
        let previous = self.undo.pop_back()?;
        self.redo.push(current);
        Some(previous)
    }

    /// Steps forward again after [`undo`](Self::undo).
    pub fn redo(&mut self, current: Snapshot) -> Option<Snapshot> {
        self.open = None;
        let next = self.redo.pop()?;
        self.undo.push_back(current);
        if self.undo.len() > self.depth {
            self.undo.pop_front();
        }
        Some(next)
    }

    /// Forgets every step, as after the message was sent.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.open = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::text_editor::Position;

    fn snap(text: &str) -> Snapshot {
        Snapshot {
            text: text.to_owned(),
            cursor: Cursor { position: Position { line: 0, column: text.len() }, selection: None },
        }
    }

    #[test]
    fn depth_drops_oldest_steps() {
        let mut history = History::new(2);
        history.begin(EditKind::Other, snap("a"));
        history.begin(EditKind::Other, snap("ab"));
        history.begin(EditKind::Other, snap("abc"));
        assert_eq!(history.undo(snap("abcd")).map(|s| s.text), Some("abc".into()));
        assert_eq!(history.undo(snap("abc")).map(|s| s.text), Some("ab".into()));
        assert_eq!(history.undo(snap("ab")), None);
    }

    #[test]
    fn word_after_space_starts_a_group() {
        let mut history = History::new(10);
        history.begin(EditKind::Insert { whitespace: false }, snap(""));
        assert!(history.continues(EditKind::Insert { whitespace: false }));
        assert!(history.continues(EditKind::Insert { whitespace: true }));
        history.extend(EditKind::Insert { whitespace: true });
        assert!(!history.continues(EditKind::Insert { whitespace: false }));
        assert!(!history.continues(EditKind::Delete));
    }
}
