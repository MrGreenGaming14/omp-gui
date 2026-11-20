//! The `/` command and `@` file mention popups: when they open, what they
//! show and which item is selected.

/// Most items a popup lists at once.
pub const MAX_ITEMS: usize = 50;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PopupKind {
    /// `/` typed at the start of the message.
    Command,
    /// `@` typed at the start of a word.
    File,
}

/// The word under the cursor that opened a popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub kind: PopupKind,
    pub line: usize,
    /// Byte column of the `/` or `@`.
    pub start: usize,
    /// Byte column where the word ends. Accepting replaces `start..end`.
    pub end: usize,
    /// Text between the trigger character and the cursor.
    pub query: String,
}

impl Trigger {
    /// Whether `other` is the same popup word, possibly with a different query.
    pub fn same_anchor(&self, other: &Trigger) -> bool {
        self.kind == other.kind && self.line == other.line && self.start == other.start
    }
}

/// Finds the popup trigger for the cursor at byte `column` of `line`, the
/// text of line number `line_index`.
pub fn detect(line_index: usize, line: &str, column: usize) -> Option<Trigger> {
    let column = column.min(line.len());
    let before = line.get(..column)?;
    let start = before.rfind(char::is_whitespace).map_or(0, |i| i + whitespace_len(before, i));
    let word_start = line[start..].chars().next()?;
    let kind = match word_start {
        '/' if line_index == 0 && start == 0 => PopupKind::Command,
        '@' => PopupKind::File,
        _ => return None,
    };
    let end = line[column..].find(char::is_whitespace).map_or(line.len(), |i| column + i);
    // The cursor right before the `@` or `/` is not inside the word yet.
    let query = before.get(start + 1..)?.to_owned();
    Some(Trigger { kind, line: line_index, start, end, query })
}

fn whitespace_len(text: &str, at: usize) -> usize {
    text[at..].chars().next().map_or(1, char::len_utf8)
}

/// An open popup: its trigger, the matching candidate indices and the
/// highlighted row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Popup {
    pub trigger: Trigger,
    pub matches: Vec<usize>,
    pub selected: usize,
}

impl Popup {
    /// Whether the popup has anything to show and takes the navigation keys.
    pub fn is_visible(&self) -> bool {
        !self.matches.is_empty()
    }

    /// Candidate index of the highlighted row.
    pub fn selected_candidate(&self) -> Option<usize> {
        self.matches.get(self.selected).copied()
    }

    pub fn next(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + 1) % self.matches.len();
        }
    }

    pub fn previous(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + self.matches.len() - 1) % self.matches.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_opens_only_at_the_start_of_the_message() {
        let t = detect(0, "/rev", 4).unwrap();
        assert_eq!((t.kind, t.start, t.end, t.query.as_str()), (PopupKind::Command, 0, 4, "rev"));
        assert_eq!(detect(0, "see /rev", 8), None);
        assert_eq!(detect(1, "/rev", 4), None);
        assert_eq!(detect(0, "/review now", 11), None);
    }

    #[test]
    fn at_opens_on_the_word_under_the_cursor() {
        let t = detect(2, "look at @src/ma please", 15).unwrap();
        assert_eq!((t.kind, t.line, t.start, t.end, t.query.as_str()), (PopupKind::File, 2, 8, 15, "src/ma"));
        let t = detect(0, "x @src/main.rs", 6).unwrap();
        assert_eq!((t.start, t.end, t.query.as_str()), (2, 14, "src"));
        assert_eq!(detect(0, "mail@host", 9), None);
    }
}
