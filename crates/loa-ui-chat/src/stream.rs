//! Incrementally parsed markdown for one streaming assistant message.

use iced::widget::markdown;

/// Parsed markdown of one message, fed only the text that arrives.
#[derive(Debug, Default)]
pub(crate) struct Streamed {
    content: markdown::Content,
    /// Bytes of message text handed to the parser so far.
    fed: usize,
    /// Newlines in the text so far, for height estimates.
    lines: usize,
}

impl Streamed {
    /// Parses the whole text of a message once.
    pub(crate) fn new(text: &str) -> Self {
        let mut streamed = Self::default();
        streamed.push(text);
        streamed
    }

    /// Parses `chunk`, which follows the text pushed so far.
    pub(crate) fn push(&mut self, chunk: &str) {
        self.content.push_str(&hard_breaks(chunk));
        self.fed += chunk.len();
        self.lines += newlines(chunk);
    }

    /// Brings the parse in line with `text` after `chunk` was appended to it:
    /// pushes the chunk when the parse holds exactly the text before it, and
    /// re-parses otherwise (a patch was missed or the text was replaced).
    pub(crate) fn append(&mut self, text: &str, chunk: &str) {
        if self.fed + chunk.len() == text.len() && text.ends_with(chunk) {
            self.push(chunk);
        } else {
            *self = Self::new(text);
        }
    }

    pub(crate) fn items(&self) -> &[markdown::Item] {
        self.content.items()
    }

    pub(crate) fn fed(&self) -> usize {
        self.fed
    }

    pub(crate) fn lines(&self) -> usize {
        self.lines
    }
}

/// `text` with every newline made a markdown hard break: replies keep the
/// line breaks the model wrote ("one number per line") instead of markdown
/// joining the lines of a paragraph. Blank lines still end paragraphs, and
/// the trailing spaces this adds inside code blocks don't show.
pub(crate) fn hard_breaks(text: &str) -> String {
    text.replace('\n', "  \n")
}

fn newlines(text: &str) -> usize {
    text.bytes().filter(|b| *b == b'\n').count()
}
