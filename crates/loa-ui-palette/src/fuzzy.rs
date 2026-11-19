//! Fuzzy matching of a query against command titles.
//!
//! A query matches a title when its characters appear in the title in order,
//! ignoring case. Matches fall into [`Class`]es that always rank in order
//! (prefix, then word start, then word boundaries, then anything), and within
//! a class by [`Match::score`]. The palette breaks remaining ties by recency.

use std::ops::Range;

/// How well a query lines up with a title. Lower ranks first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// The title starts with the query (`tog` in `Toggle panel`).
    Prefix,
    /// A word of the title starts with the query (`pan` in `Toggle panel`).
    WordPrefix,
    /// Every run of matched characters starts a word (`tp` in `Toggle panel`).
    WordBoundary,
    /// Any in-order subsequence (`gl` in `Toggle panel`).
    Fuzzy,
}

/// A successful match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Ranking class.
    pub class: Class,
    /// Score within the class; higher is better.
    pub score: i32,
    /// Char indices (not byte offsets) of the matched title characters, ascending.
    pub indices: Vec<usize>,
}

const BASE: i32 = 16;
const BOUNDARY_BONUS: i32 = 8;
const CONSECUTIVE_BONUS: i32 = 12;
const MAX_LEADING_PENALTY: i32 = 15;
const NONE: i32 = i32::MIN / 2;

/// Reusable matcher. Keeps its scratch buffers between calls so filtering a
/// command list on each keystroke does not allocate per title.
#[derive(Debug, Default)]
pub struct Matcher {
    query: Vec<char>,
    title: Vec<char>,
    boundary: Vec<bool>,
    score: Vec<i32>,
    parent: Vec<usize>,
}

impl Matcher {
    /// A matcher with empty buffers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the query the next [`Matcher::matches`] calls use.
    pub fn set_query(&mut self, query: &str) {
        self.query.clear();
        self.query
            .extend(query.chars().filter(|c| !c.is_whitespace()).map(fold));
    }

    /// Whether the query is empty (every title matches).
    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    /// Matches the current query against `title`. Whitespace in the query is
    /// ignored, so `tog pan` and `togpan` behave alike.
    pub fn matches(&mut self, title: &str) -> Option<Match> {
        if self.query.is_empty() {
            return Some(Match {
                class: Class::Prefix,
                score: 0,
                indices: Vec::new(),
            });
        }
        self.title.clear();
        self.boundary.clear();
        let mut prev: Option<char> = None;
        for c in title.chars() {
            self.boundary.push(is_boundary(prev, c));
            self.title.push(fold(c));
            prev = Some(c);
        }
        let (m, n) = (self.query.len(), self.title.len());
        if m > n {
            return None;
        }

        if let Some(start) = self.contiguous_word_start() {
            let indices: Vec<usize> = (start..start + m).collect();
            let class = if start == 0 {
                Class::Prefix
            } else {
                Class::WordPrefix
            };
            let score = self.score_of(&indices);
            return Some(Match {
                class,
                score,
                indices,
            });
        }
        if let Some((score, indices)) = self.align(true) {
            return Some(Match {
                class: Class::WordBoundary,
                score,
                indices,
            });
        }
        self.align(false).map(|(score, indices)| Match {
            class: Class::Fuzzy,
            score,
            indices,
        })
    }

    /// First word start where the whole query appears contiguously.
    fn contiguous_word_start(&self) -> Option<usize> {
        let m = self.query.len();
        (0..=self.title.len() - m)
            .find(|&j| self.boundary[j] && self.title[j..j + m] == self.query[..])
    }

    fn base(&self, j: usize) -> i32 {
        BASE + if self.boundary[j] { BOUNDARY_BONUS } else { 0 }
    }

    fn leading(j: usize) -> i32 {
        i32::try_from(j).map_or(MAX_LEADING_PENALTY, |j| j.min(MAX_LEADING_PENALTY))
    }

    fn gap(from: usize, to: usize) -> i32 {
        i32::try_from(to - from - 1).unwrap_or(i32::MAX / 4)
    }

    /// Score of a given alignment, with the same rules as [`Matcher::align`].
    fn score_of(&self, indices: &[usize]) -> i32 {
        let mut total = 0;
        for (i, &j) in indices.iter().enumerate() {
            total += self.base(j);
            if i == 0 {
                total -= Self::leading(j);
            } else if j == indices[i - 1] + 1 {
                total += CONSECUTIVE_BONUS;
            } else {
                total -= Self::gap(indices[i - 1], j);
            }
        }
        total
    }

    /// Best scoring alignment of the query in the title. With
    /// `word_runs`, a matched character must start a word or directly follow
    /// the previous match.
    fn align(&mut self, word_runs: bool) -> Option<(i32, Vec<usize>)> {
        let (m, n) = (self.query.len(), self.title.len());
        self.score.clear();
        self.score.resize(m * n, NONE);
        self.parent.clear();
        self.parent.resize(m * n, 0);

        for j in 0..n {
            if self.title[j] == self.query[0] && (!word_runs || self.boundary[j]) {
                self.score[j] = self.base(j) - Self::leading(j);
            }
        }
        for i in 1..m {
            let (prev_row, row) = ((i - 1) * n, i * n);
            // Best of score[i-1][k] + k over k < j - 1, for gap transitions:
            // score[i-1][k] - (j - k - 1) == (score[i-1][k] + k) - (j - 1).
            let mut best_gap: Option<(i32, usize)> = None;
            for j in 1..n {
                if j >= 2 {
                    let k = j - 2;
                    let s = self.score[prev_row + k];
                    if s > NONE {
                        let v = s + i32::try_from(k).unwrap_or(i32::MAX / 4);
                        if best_gap.is_none_or(|(b, _)| v > b) {
                            best_gap = Some((v, k));
                        }
                    }
                }
                if self.title[j] != self.query[i] {
                    continue;
                }
                let base = self.base(j);
                let mut best = NONE;
                let mut from = 0;
                let consecutive = self.score[prev_row + j - 1];
                if consecutive > NONE {
                    best = consecutive + base + CONSECUTIVE_BONUS;
                    from = j - 1;
                }
                if (!word_runs || self.boundary[j])
                    && let Some((v, k)) = best_gap
                {
                    let s = v - i32::try_from(j - 1).unwrap_or(i32::MAX / 4) + base;
                    if s > best {
                        best = s;
                        from = k;
                    }
                }
                self.score[row + j] = best;
                self.parent[row + j] = from;
            }
        }

        let last = (m - 1) * n;
        let (end, &score) = self.score[last..last + n]
            .iter()
            .enumerate()
            .filter(|&(_, &s)| s > NONE)
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(&a.0)))?;
        let mut indices = vec![0; m];
        let mut j = end;
        for i in (0..m).rev() {
            indices[i] = j;
            j = self.parent[i * n + j];
        }
        Some((score, indices))
    }
}

/// Case folding used on both sides of a match.
fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

/// Whether `c` starts a word: first character, after a non-alphanumeric, or a
/// camelCase hump.
fn is_boundary(prev: Option<char>, c: char) -> bool {
    match prev {
        None => true,
        Some(p) => !p.is_alphanumeric() || (p.is_lowercase() && c.is_uppercase()),
    }
}

/// Splits `title` into runs of matched and unmatched text for highlighting.
/// `indices` are ascending char indices as in [`Match::indices`]; the result
/// is byte ranges into `title` with a flag that is true for matched runs.
pub fn highlight_runs(title: &str, indices: &[usize]) -> Vec<(Range<usize>, bool)> {
    let mut runs: Vec<(Range<usize>, bool)> = Vec::new();
    let mut next = indices.iter().copied().peekable();
    for (char_index, (byte, c)) in title.char_indices().enumerate() {
        let matched = next.peek() == Some(&char_index);
        if matched {
            next.next();
        }
        let end = byte + c.len_utf8();
        match runs.last_mut() {
            Some((range, flag)) if *flag == matched => range.end = end,
            _ => runs.push((byte..end, matched)),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(query: &str, title: &str) -> Option<Match> {
        let mut matcher = Matcher::new();
        matcher.set_query(query);
        matcher.matches(title)
    }

    #[test]
    fn classes_reflect_where_the_query_lands() {
        assert_eq!(m("tog", "Toggle panel").unwrap().class, Class::Prefix);
        assert_eq!(m("PAN", "Toggle panel").unwrap().class, Class::WordPrefix);
        assert_eq!(m("tp", "Toggle panel").unwrap().class, Class::WordBoundary);
        assert_eq!(m("togpan", "Toggle panel").unwrap().class, Class::WordBoundary);
        assert_eq!(m("gl", "Toggle panel").unwrap().class, Class::Fuzzy);
        assert_eq!(m("sett", "openSettings").unwrap().class, Class::WordPrefix);
        assert_eq!(m("xyz", "Toggle panel"), None);
        assert_eq!(m("lenap", "Toggle panel"), None, "order matters");
    }

    #[test]
    fn indices_point_at_the_ranked_characters() {
        assert_eq!(m("pan", "Toggle panel").unwrap().indices, vec![7, 8, 9]);
        assert_eq!(m("tp", "Toggle panel").unwrap().indices, vec![0, 7]);
        // Prefers the word start "N" in "New" over the earlier "n" in "Open".
        assert_eq!(m("on", "Open New").unwrap().indices, vec![0, 5]);
    }

    #[test]
    fn consecutive_and_boundary_matches_score_higher() {
        let tight = m("ter", "Open terminal").unwrap();
        let loose = m("ter", "Toggle tree").unwrap();
        assert!(tight.class < loose.class || tight.score > loose.score);

        let consecutive = m("abc", "xabcx").unwrap();
        let spread = m("abc", "xaxbxcx").unwrap();
        assert_eq!(consecutive.class, Class::Fuzzy);
        assert_eq!(spread.class, Class::Fuzzy);
        assert!(consecutive.score > spread.score);
    }

    #[test]
    fn whitespace_in_query_is_ignored_and_non_ascii_folds() {
        assert_eq!(m("tog pan", "Toggle panel").unwrap().indices, vec![0, 1, 2, 7, 8, 9]);
        let ü = m("üb", "Über Ansicht").unwrap();
        assert_eq!(ü.class, Class::Prefix);
        assert_eq!(ü.indices, vec![0, 1]);
    }

    #[test]
    fn highlight_runs_split_on_char_boundaries() {
        let title = "Über panel";
        let found = m("üpa", title).unwrap();
        let runs: Vec<(&str, bool)> = highlight_runs(title, &found.indices)
            .into_iter()
            .map(|(r, hit)| (&title[r], hit))
            .collect();
        assert_eq!(
            runs,
            vec![("Ü", true), ("ber ", false), ("pa", true), ("nel", false)]
        );
        assert_eq!(highlight_runs("abc", &[]), vec![(0..3, false)]);
    }
}
