//! Fuzzy matching: the query's characters appear in the title in order, ignoring case. Matches
//! fall into [`Class`]es that always rank in order, and within a class by [`Match::score`].

use std::ops::Range;

/// How well a query lines up with a title. Lower ranks first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// The title starts with the query (`tog` in `Toggle panel`).
    Prefix,
    /// A word of the title starts with the query (`pan`).
    WordPrefix,
    /// Every run of matched characters starts a word (`tp`).
    WordBoundary,
    /// Any in-order subsequence (`gl`).
    Fuzzy,
}

/// A successful match; `indices` are ascending char (not byte) indices into the title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub class: Class,
    pub score: i32,
    pub indices: Vec<usize>,
}

const BASE: i32 = 16;
const BOUNDARY_BONUS: i32 = 8;
const CONSECUTIVE_BONUS: i32 = 12;
const MAX_LEADING_PENALTY: i32 = 15;
const NONE: i32 = i32::MIN / 2;

fn int(n: usize) -> i32 { i32::try_from(n).unwrap_or(i32::MAX / 4) }

/// Reusable matcher; keeps scratch buffers so filtering on each keystroke does not allocate.
#[derive(Debug, Default)]
pub struct Matcher {
    query: Vec<char>,
    title: Vec<char>,
    boundary: Vec<bool>,
    score: Vec<i32>,
    parent: Vec<usize>,
}

impl Matcher {
    pub fn new() -> Self { Self::default() }

    /// Sets the query; whitespace is ignored, so `tog pan` and `togpan` behave alike.
    pub fn set_query(&mut self, query: &str) {
        self.query.clear();
        self.query.extend(query.chars().filter(|c| !c.is_whitespace()).map(fold));
    }

    /// Whether the query is empty (every title matches).
    pub fn is_empty(&self) -> bool { self.query.is_empty() }

    pub fn matches(&mut self, title: &str) -> Option<Match> {
        if self.query.is_empty() { return Some(Match { class: Class::Prefix, score: 0, indices: vec![] }) }
        self.title.clear();
        self.boundary.clear();
        let mut prev = None;
        for c in title.chars() {
            self.boundary.push(is_boundary(prev, c));
            self.title.push(fold(c));
            prev = Some(c);
        }
        let m = self.query.len();
        if m > self.title.len() { return None }
        if let Some(start) = (0..=self.title.len() - m).find(|&j| self.boundary[j] && self.title[j..j + m] == self.query[..]) {
            let indices: Vec<usize> = (start..start + m).collect();
            let class = if start == 0 { Class::Prefix } else { Class::WordPrefix };
            return Some(Match { class, score: self.score_of(&indices), indices });
        }
        if let Some((score, indices)) = self.align(true) { return Some(Match { class: Class::WordBoundary, score, indices }) }
        self.align(false).map(|(score, indices)| Match { class: Class::Fuzzy, score, indices })
    }

    fn base(&self, j: usize) -> i32 { BASE + if self.boundary[j] { BOUNDARY_BONUS } else { 0 } }

    fn leading(j: usize) -> i32 { int(j).min(MAX_LEADING_PENALTY) }

    /// Score of an alignment, with the same rules as [`Matcher::align`].
    fn score_of(&self, indices: &[usize]) -> i32 {
        indices.iter().enumerate().fold(0, |total, (i, &j)| {
            total + self.base(j) + match i {
                0 => -Self::leading(j),
                _ if j == indices[i - 1] + 1 => CONSECUTIVE_BONUS,
                _ => -int(j - indices[i - 1] - 1),
            }
        })
    }

    /// Best alignment of the query in the title. With `word_runs`, a matched character must start
    /// a word or directly follow the previous match.
    fn align(&mut self, word_runs: bool) -> Option<(i32, Vec<usize>)> {
        let (m, n) = (self.query.len(), self.title.len());
        self.score.clear();
        self.score.resize(m * n, NONE);
        self.parent.clear();
        self.parent.resize(m * n, 0);
        for j in 0..n {
            if self.title[j] == self.query[0] && (!word_runs || self.boundary[j]) { self.score[j] = self.base(j) - Self::leading(j) }
        }
        for i in 1..m {
            let (prev_row, row) = ((i - 1) * n, i * n);
            // Best score[i-1][k] + k over k < j - 1 for gap transitions:
            // score[i-1][k] - (j - k - 1) == (score[i-1][k] + k) - (j - 1).
            let mut best_gap: Option<(i32, usize)> = None;
            for j in 1..n {
                if j >= 2 && self.score[prev_row + j - 2] > NONE {
                    let v = self.score[prev_row + j - 2] + int(j - 2);
                    if best_gap.is_none_or(|(b, _)| v > b) { best_gap = Some((v, j - 2)) }
                }
                if self.title[j] != self.query[i] { continue }
                let base = self.base(j);
                let (mut best, mut from) = (NONE, 0);
                if self.score[prev_row + j - 1] > NONE { (best, from) = (self.score[prev_row + j - 1] + base + CONSECUTIVE_BONUS, j - 1) }
                if let Some((v, k)) = best_gap.filter(|_| !word_runs || self.boundary[j])
                    && v - int(j - 1) + base > best
                {
                    (best, from) = (v - int(j - 1) + base, k);
                }
                (self.score[row + j], self.parent[row + j]) = (best, from);
            }
        }
        let last = (m - 1) * n;
        let (end, &score) = self.score[last..last + n].iter().enumerate().filter(|&(_, &s)| s > NONE).max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(&a.0)))?;
        let mut indices = vec![0; m];
        let mut j = end;
        for i in (0..m).rev() {
            indices[i] = j;
            j = self.parent[i * n + j];
        }
        Some((score, indices))
    }
}

fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

/// Whether `c` starts a word: first character, after a non-alphanumeric, or a camelCase hump.
fn is_boundary(prev: Option<char>, c: char) -> bool { prev.is_none_or(|p| !p.is_alphanumeric() || (p.is_lowercase() && c.is_uppercase())) }

/// Byte ranges of `title` flagged true where matched, for highlighting `indices`.
pub fn highlight_runs(title: &str, indices: &[usize]) -> Vec<(Range<usize>, bool)> {
    let mut runs: Vec<(Range<usize>, bool)> = Vec::new();
    let mut next = indices.iter().copied().peekable();
    for (i, (byte, c)) in title.char_indices().enumerate() {
        let matched = next.next_if_eq(&i).is_some();
        match runs.last_mut() {
            Some((range, flag)) if *flag == matched => range.end = byte + c.len_utf8(),
            _ => runs.push((byte..byte + c.len_utf8(), matched)),
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
    fn classes_indices_and_scores() {
        let classes = [("tog", Class::Prefix), ("PAN", Class::WordPrefix), ("tp", Class::WordBoundary), ("togpan", Class::WordBoundary), ("gl", Class::Fuzzy)];
        classes.iter().for_each(|&(q, c)| assert_eq!(m(q, "Toggle panel").unwrap().class, c, "{q}"));
        assert_eq!(m("sett", "openSettings").unwrap().class, Class::WordPrefix);
        assert_eq!((m("xyz", "Toggle panel"), m("lenap", "Toggle panel")), (None, None), "order matters");
        assert_eq!(m("tp", "Toggle panel").unwrap().indices, [0, 7]);
        assert_eq!(m("on", "Open New").unwrap().indices, [0, 5], "prefers the word start");
        assert_eq!(m("tog pan", "Toggle panel").unwrap().indices, [0, 1, 2, 7, 8, 9]);
        assert!(m("abc", "xabcx").unwrap().score > m("abc", "xaxbxcx").unwrap().score);
        let ü = m("üb", "Über Ansicht").unwrap();
        assert_eq!((ü.class, ü.indices), (Class::Prefix, vec![0, 1]));
    }

    #[test]
    fn highlight_runs_split_on_char_boundaries() {
        let title = "Über panel";
        let runs: Vec<_> = highlight_runs(title, &m("üpa", title).unwrap().indices).into_iter().map(|(r, hit)| (&title[r], hit)).collect();
        assert_eq!(runs, [("Ü", true), ("ber ", false), ("pa", true), ("nel", false)]);
        assert_eq!(highlight_runs("abc", &[]), [(0..3, false)]);
    }
}
