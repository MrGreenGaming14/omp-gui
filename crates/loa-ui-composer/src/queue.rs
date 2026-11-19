//! Messages written while a turn runs, sent one per turn end.

use std::collections::VecDeque;

/// One queued message. `id` stays the same while its text is edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedMessage {
    pub id: u64,
    pub text: String,
}

/// First in, first out queue of messages waiting for the running turn.
#[derive(Debug, Default)]
pub struct Queue {
    items: VecDeque<QueuedMessage>,
    next_id: u64,
}

impl Queue {
    pub fn push(&mut self, text: String) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push_back(QueuedMessage { id, text });
        id
    }

    /// Replaces the text of message `id`. False when it is no longer queued.
    pub fn edit(&mut self, id: u64, text: String) -> bool {
        match self.items.iter_mut().find(|m| m.id == id) {
            Some(item) => {
                item.text = text;
                true
            }
            None => false,
        }
    }

    /// Removes message `id`. False when it is no longer queued.
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.items.len();
        self.items.retain(|m| m.id != id);
        self.items.len() != before
    }

    /// Takes the oldest message with non-blank text, dropping blank ones
    /// (a queued message edited down to nothing).
    pub fn pop_sendable(&mut self) -> Option<QueuedMessage> {
        while let Some(item) = self.items.pop_front() {
            if !item.text.trim().is_empty() {
                return Some(item);
            }
        }
        None
    }

    /// Empties the queue, oldest first.
    pub fn take_all(&mut self) -> Vec<QueuedMessage> {
        self.items.drain(..).collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &QueuedMessage> {
        self.items.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}
