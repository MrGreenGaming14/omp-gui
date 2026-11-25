//! Colors in the editor: a `/command` that starts the message, `@` file
//! mentions, `[Image #n]` tokens and omp's magic keywords. A keyword is drawn
//! in a gradient that shimmers while [`Settings::phase`] advances, as omp's
//! own editor does.

use std::ops::Range;

use iced::advanced::text::highlighter::{self, Format};
use iced::{Color, Font, Theme, font};
use loa_ui_kit::theme::{ColorRole, for_theme};

use super::shimmer;

/// Colors in one keyword gradient, as in omp.
const STOPS: usize = 14;

/// A magic keyword: omp acts on it when it stands alone in a prompt.
struct Keyword {
    word: &'static str,
    /// Hue at the start and end of the gradient, in degrees.
    hue: (f32, f32),
    /// Lightness at the start and end, in percent; 62 throughout when unset.
    lightness: Option<(f32, f32)>,
}

/// omp's magic keywords (`packages/coding-agent/src/modes/magic-keywords.ts`).
const KEYWORDS: [Keyword; 5] = [
    Keyword { word: "ultrathink", hue: (0.0, 330.0), lightness: None },
    Keyword { word: "chillax", hue: (180.0, 235.0), lightness: Some((62.0, 45.0)) },
    Keyword { word: "orchestrate", hue: (150.0, 280.0), lightness: None },
    Keyword { word: "workflowz", hue: (30.0, 150.0), lightness: None },
    Keyword { word: "jevify", hue: (300.0, 420.0), lightness: None },
];

/// Card and token colors of pasted images, by image number.
const IMAGE_COLORS: [Color; 5] = [
    Color::from_rgb8(0xff, 0xa6, 0x57),
    Color::from_rgb8(0x79, 0xc0, 0xff),
    Color::from_rgb8(0xc6, 0x9c, 0xff),
    Color::from_rgb8(0x7e, 0xe7, 0x87),
    Color::from_rgb8(0xff, 0x7b, 0xb0),
];

const MENTION: Color = Color::from_rgb8(0x79, 0xc0, 0xff);

/// The color of image `n`'s token and card.
pub(crate) fn image_color(n: u32) -> Color {
    IMAGE_COLORS[(n.saturating_sub(1) as usize) % IMAGE_COLORS.len()]
}

/// The text standing for pasted image `n` in the message.
pub(crate) fn image_token(n: u32) -> String {
    format!("[Image #{n}]")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Settings {
    /// Shimmer step, `0..shimmer::PHASES`.
    pub phase: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Highlight {
    Command,
    Mention,
    Image(u32),
    Keyword(Color),
}

pub struct Highlighter {
    phase: u8,
    line: usize,
}

impl highlighter::Highlighter for Highlighter {
    type Settings = Settings;
    type Highlight = Highlight;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Highlight)>;

    fn new(settings: &Settings) -> Self {
        Self { phase: settings.phase, line: 0 }
    }

    fn update(&mut self, settings: &Settings) {
        if settings.phase != self.phase {
            self.phase = settings.phase;
            // Recolor from the top.
            self.line = 0;
        }
    }

    fn change_line(&mut self, line: usize) {
        self.line = self.line.min(line);
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        let spans = spans(line, self.line == 0, self.phase);
        self.line += 1;
        spans.into_iter()
    }

    fn current_line(&self) -> usize {
        self.line
    }
}

/// How each highlight is drawn.
pub fn format(highlight: &Highlight, theme: &Theme) -> Format<Font> {
    let bold = Font { weight: font::Weight::Semibold, ..Font::DEFAULT };
    match *highlight {
        Highlight::Command => Format { color: Some(for_theme(theme).color(ColorRole::Accent)), font: Some(bold) },
        Highlight::Mention => Format { color: Some(MENTION), font: None },
        Highlight::Image(n) => Format { color: Some(image_color(n)), font: Some(bold) },
        Highlight::Keyword(color) => Format { color: Some(color), font: Some(bold) },
    }
}

/// Whether `line` holds a magic keyword, which makes the shimmer run.
pub(crate) fn has_keyword(line: &str) -> bool {
    KEYWORDS.iter().any(|keyword| keyword_ranges(line, keyword.word).next().is_some())
}

/// The highlights of one line, in order and without overlaps. `first` marks
/// the message's first line, where a `/command` may start it.
fn spans(line: &str, first: bool, phase: u8) -> Vec<(Range<usize>, Highlight)> {
    let mut spans = Vec::new();
    if first && let Some(rest) = line.strip_prefix('/') {
        let name = rest.find(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))).unwrap_or(rest.len());
        if name > 0 {
            spans.push((0..1 + name, Highlight::Command));
        }
    }
    spans.extend(mentions(line).map(|range| (range, Highlight::Mention)));
    spans.extend(images(line));
    for keyword in &KEYWORDS {
        for range in keyword_ranges(line, keyword.word) {
            if spans.iter().any(|(taken, _)| taken.start < range.end && range.start < taken.end) {
                continue;
            }
            let chars = keyword.word.chars().count();
            let mut start = range.start;
            for (index, c) in keyword.word.chars().enumerate() {
                let end = start + c.len_utf8();
                spans.push((start..end, Highlight::Keyword(gradient(keyword, index, chars, phase))));
                start = end;
            }
        }
    }
    spans.sort_by_key(|(range, _)| range.start);
    spans
}

/// `@path` and `@"path"` mentions at the start of a word, trailing
/// punctuation left out.
fn mentions(line: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut previous: Option<char> = None;
    line.char_indices().filter_map(move |(at, c)| {
        let starts = c == '@' && previous.is_none_or(char::is_whitespace);
        previous = Some(c);
        if !starts {
            return None;
        }
        let after = &line[at + 1..];
        if let Some(quoted) = after.strip_prefix('"') {
            return quoted.find('"').map(|close| at..at + close + 3);
        }
        let word = &after[..after.find(char::is_whitespace).unwrap_or(after.len())];
        let word = word.trim_end_matches([',', '.', ';', ':', '!', '?', ')', ']', '"', '\'']);
        (!word.is_empty()).then(|| at..at + 1 + word.len())
    })
}

/// `[Image #n]` tokens and their numbers.
fn images(line: &str) -> impl Iterator<Item = (Range<usize>, Highlight)> + '_ {
    line.match_indices("[Image #").filter_map(move |(at, prefix)| {
        let rest = &line[at + prefix.len()..];
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let n = rest[..digits].parse().ok()?;
        rest[digits..].starts_with(']').then(|| (at..at + prefix.len() + digits + 1, Highlight::Image(n)))
    })
}

/// Where `word` stands alone in `line`, by omp's rule: no letter, digit,
/// `_`, `.`, `/`, `\` or `-` against it, no `::` before it, and no `(` or
/// `.name` after it.
fn keyword_ranges<'a>(line: &'a str, word: &'a str) -> impl Iterator<Item = Range<usize>> + 'a {
    let joins = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(word).filter_map(move |(at, _)| {
        let before = &line[..at];
        let after = &line[at + word.len()..];
        let prev = before.chars().next_back();
        if prev.is_some_and(|c| joins(c) || matches!(c, '.' | '/' | '\\' | '-')) || before.ends_with("::") {
            return None;
        }
        let mut next = after.chars();
        match next.next() {
            Some(c) if joins(c) || matches!(c, '/' | '\\' | '-' | '(') => None,
            Some('.') if next.next().is_some_and(|c| joins(c) || c == '-') => None,
            _ => Some(at..at + word.len()),
        }
    })
}

/// The color of character `index` of `chars` in `keyword` at shimmer `phase`.
fn gradient(keyword: &Keyword, index: usize, chars: usize, phase: u8) -> Color {
    let shift = shimmer::progress(phase);
    let at = (index as f32 / chars as f32 + shift).fract();
    let stop = ((at * STOPS as f32) as usize % STOPS) as f32 / STOPS as f32;
    let hue = (keyword.hue.0 + stop * (keyword.hue.1 - keyword.hue.0)).rem_euclid(360.0);
    let lightness = keyword.lightness.map_or(62.0, |(from, to)| from + stop * (to - from));
    hsl(hue, 0.9, lightness / 100.0)
}

fn hsl(hue: f32, saturation: f32, lightness: f32) -> Color {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue / 60.0;
    let x = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match sector as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = lightness - chroma / 2.0;
    Color::from_rgb(r + m, g + m, b + m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str, first: bool) -> Vec<(&str, &'static str)> {
        spans(line, first, 0)
            .into_iter()
            .map(|(range, highlight)| {
                let kind = match highlight {
                    Highlight::Command => "command",
                    Highlight::Mention => "mention",
                    Highlight::Image(_) => "image",
                    Highlight::Keyword(_) => "keyword",
                };
                (&line[range], kind)
            })
            .collect()
    }

    #[test]
    fn a_leading_slash_command_mentions_and_images_are_marked() {
        assert_eq!(
            kinds("/plan fix @src/a.rs, see [Image #2] and @\"a b.png\"", true),
            [("/plan", "command"), ("@src/a.rs", "mention"), ("[Image #2]", "image"), ("@\"a b.png\"", "mention")]
        );
        assert!(kinds("/plan", false).is_empty(), "only the message's first line starts a command");
        assert!(kinds("a/b me@x.rs [Image #x]", true).is_empty());
    }

    #[test]
    fn keywords_count_only_when_they_stand_alone() {
        let alone = kinds("please workflowz, then ultrathink.", true);
        assert_eq!(alone.iter().filter(|(_, kind)| *kind == "keyword").count(), "workflowz".len() + "ultrathink".len());
        for joined in ["workflowzz", "my_workflowz", "a/workflowz", "workflowz()", "x::workflowz", "workflowz.rs", "workflowz-ish"] {
            assert!(!has_keyword(joined), "{joined}");
        }
        assert!(has_keyword("(workflowz)"));
    }

    #[test]
    fn the_gradient_spans_the_hue_range_and_moves_with_the_phase() {
        let keyword = &KEYWORDS[3];
        let at = |index, phase| gradient(keyword, index, 9, phase);
        assert_ne!(at(0, 0), at(8, 0), "the word is a gradient");
        assert_ne!(at(0, 0), at(0, shimmer::PHASES / 2), "the shimmer moves it");
        assert_eq!(at(0, 0), at(0, shimmer::PHASES), "and loops");
        let red = hsl(0.0, 1.0, 0.5);
        assert!((red.r - 1.0).abs() < 1e-4 && red.g.abs() < 1e-4 && red.b.abs() < 1e-4);
    }
}
