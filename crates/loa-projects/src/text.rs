//! Small text helpers shared by the digester, search, read and ranking.

pub(crate) use loa_util::text::{ceil_boundary, clip, floor_boundary};

/// `text` clipped to `max` bytes with a marker saying how much was cut.
pub(crate) fn clip_marked(text: &str, max: usize) -> std::borrow::Cow<'_, str> {
    let kept = clip(text, max);
    if kept.len() == text.len() {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(format!("{kept}… [{} more bytes]", text.len() - kept.len()))
}

/// Lowercased text that remembers, for every byte, the byte offset in the
/// original it came from, so a match found case insensitively maps back to
/// the original text.
pub(crate) struct Folded {
    pub lower: String,
    origin: Vec<usize>,
}

impl Folded {
    pub fn new(text: &str) -> Self {
        let mut lower = String::with_capacity(text.len());
        let mut origin = Vec::with_capacity(text.len() + 1);
        for (at, ch) in text.char_indices() {
            for low in ch.to_lowercase() {
                lower.push(low);
                origin.resize(lower.len(), at);
            }
        }
        origin.push(text.len());
        Self { lower, origin }
    }

    /// Original byte offset of byte `at` of the lowercased text.
    pub fn origin(&self, at: usize) -> usize {
        self.origin[at.min(self.origin.len() - 1)]
    }
}

/// Words of `text` for lexical ranking: lowercase alphanumeric runs of two or
/// more chars, minus a short list of English stop words.
pub(crate) fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().nth(1).is_some())
        .map(str::to_lowercase)
        .filter(|w| !STOP_WORDS.contains(&w.as_str()))
}

const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "into", "is", "it", "of", "on", "or", "that",
    "the", "this", "to", "was", "were", "with",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folded_maps_matches_back_to_the_original() {
        // 'İ' lowercases to two chars, so lowercase offsets drift from the original.
        let folded = Folded::new("İx Needle");
        let at = folded.lower.find("needle").unwrap();
        assert_eq!(&"İx Needle"[folded.origin(at)..], "Needle");
    }
}
