//! Ranks a project's digests by a [`Weighted`] mix of [`Bm25`] relevance and [`Recency`],
//! and packs them into a token budget. Ties go to the most recently active, then the
//! lower session id. A token is estimated as four bytes of the rendered section.

use crate::context::render_digest_section;
use crate::digest::{Digest, digest_of};
use crate::settings::is_excluded;
use loa_core::{Dom, ElementKind, attr};
use std::collections::HashMap;

/// One session's digest offered for project context.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub session: String,
    pub title: Option<String>,
    pub digest: Digest,
    /// Unix milliseconds of the session's last activity.
    pub last_active: i64,
    pub excluded: bool,
}

impl Candidate {
    /// `None` without a digest.
    pub fn from_dom(dom: &Dom) -> Option<Self> {
        let root = dom.root()?;
        Some(Self {
            session: root.str_attr(attr::ID)?.to_owned(),
            title: root.str_attr(attr::TITLE).map(str::to_owned),
            digest: digest_of(dom)?,
            last_active: last_active(dom),
            excluded: is_excluded(dom),
        })
    }

    /// The digest's searchable fields.
    fn fields(&self) -> impl Iterator<Item = &str> {
        let d = &self.digest.content;
        let files = d.files.iter().flat_map(|f| [f.path.as_str(), f.note.as_str()]);
        let lists = d.decisions.iter().chain(&d.open_items).map(String::as_str);
        self.title.as_deref().into_iter().chain([d.summary.as_str()]).chain(lists).chain(files)
    }
}

/// Unix milliseconds of the latest turn start or end, else the session's creation.
pub fn last_active(dom: &Dom) -> i64 {
    let Some(root) = dom.root() else { return 0 };
    let turns = dom.children_of_kind(root.id, ElementKind::Turn);
    turns.flat_map(|t| [t.int_attr(attr::STARTED), t.int_attr(attr::ENDED)]).flatten().fold(root.int_attr(attr::CREATED).unwrap_or(0), i64::max)
}

/// Lowercase alphanumeric runs of two or more chars, minus English stop words.
pub(crate) fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    const STOP: &[&str] = &[
        "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "into", "is", "it", "of", "on", "or", "that",
        "the", "this", "to", "was", "were", "with",
    ];
    let words = text.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().nth(1).is_some());
    words.map(str::to_lowercase).filter(|w| !STOP.contains(&w.as_str()))
}

/// A ranking strategy: a score in 0..=1 for a candidate against the query words.
pub trait Scorer: Send + Sync {
    fn score(&self, query: &[String], candidate: &Candidate) -> f64;
}

/// BM25 fitted on the candidates, divided by the best candidate's score.
pub struct Bm25 {
    docs: HashMap<String, HashMap<String, usize>>,
    avg_len: f64,
    best: f64,
}

impl Bm25 {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;

    pub fn fit(candidates: &[Candidate], query: &[String]) -> Self {
        let mut docs = HashMap::new();
        for c in candidates {
            let mut counts = HashMap::new();
            c.fields().flat_map(words).for_each(|w| *counts.entry(w).or_insert(0) += 1);
            docs.insert(c.session.clone(), counts);
        }
        let total: usize = docs.values().map(|d: &HashMap<String, usize>| d.values().sum::<usize>()).sum();
        let mut bm25 = Self { avg_len: total as f64 / docs.len().max(1) as f64, docs, best: 1.0 };
        bm25.best = candidates.iter().map(|c| bm25.raw(query, &c.session)).fold(0.0, f64::max);
        bm25
    }

    fn raw(&self, query: &[String], session: &str) -> f64 {
        let Some(doc) = self.docs.get(session) else { return 0.0 };
        let (n, len) = (self.docs.len() as f64, doc.values().sum::<usize>() as f64);
        let term = |t: &String| {
            let df = self.docs.values().filter(|d| d.contains_key(t)).count() as f64;
            let tf = *doc.get(t).unwrap_or(&0) as f64;
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            idf * tf * (Self::K1 + 1.0) / (tf + Self::K1 * (1.0 - Self::B + Self::B * len / self.avg_len.max(1.0)))
        };
        query.iter().map(term).sum()
    }
}

impl Scorer for Bm25 {
    fn score(&self, query: &[String], c: &Candidate) -> f64 {
        if self.best > 0.0 { self.raw(query, &c.session) / self.best } else { 0.0 }
    }
}

/// Halves every `half_life_ms` since the session was last active.
pub struct Recency {
    pub now: i64,
    pub half_life_ms: i64,
}

impl Scorer for Recency {
    fn score(&self, _: &[String], c: &Candidate) -> f64 {
        let age = self.now.saturating_sub(c.last_active).max(0) as f64;
        0.5f64.powf(age / self.half_life_ms.max(1) as f64)
    }
}

/// The weighted sum of other scorers.
pub struct Weighted(pub Vec<(f64, Box<dyn Scorer>)>);

impl Scorer for Weighted {
    fn score(&self, query: &[String], c: &Candidate) -> f64 {
        self.0.iter().map(|(w, s)| w * s.score(query, c)).sum()
    }
}

/// `now` in Unix ms; `relevance_weight` is relevance's share (0 to 1), recency gets the rest.
#[derive(Clone, Debug, PartialEq)]
pub struct RankParams {
    pub now: i64,
    pub half_life_ms: i64,
    pub relevance_weight: f64,
    pub budget_tokens: usize,
}

impl RankParams {
    /// A week's half life and 70 % relevance.
    pub fn new(now: i64, budget_tokens: usize) -> Self {
        Self { now, half_life_ms: 7 * 24 * 3600 * 1000, relevance_weight: 0.7, budget_tokens }
    }
}

/// A packed digest, its score and its rendered section.
#[derive(Clone, Debug, PartialEq)]
pub struct Ranked {
    pub candidate: Candidate,
    pub score: f64,
    pub section: String,
}

/// Four bytes per token.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Ranks `candidates` against `query` and keeps, best first, those that fit the budget.
/// Excluded sessions are dropped before fitting, so they don't shift other scores.
pub fn rank_and_pack(mut candidates: Vec<Candidate>, query: &str, params: &RankParams) -> Vec<Ranked> {
    candidates.retain(|c| !c.excluded);
    let mut query: Vec<String> = words(query).collect();
    query.sort();
    query.dedup();
    let (w, recency) = (params.relevance_weight, Recency { now: params.now, half_life_ms: params.half_life_ms });
    let scorer = Weighted(vec![(w, Box::new(Bm25::fit(&candidates, &query))), (1.0 - w, Box::new(recency))]);
    let mut ranked: Vec<Ranked> = candidates.into_iter().map(|c| Ranked { score: scorer.score(&query, &c), section: render_digest_section(&c), candidate: c }).collect();
    ranked.sort_by(|a, b| {
        let (ca, cb) = (&a.candidate, &b.candidate);
        b.score.total_cmp(&a.score).then(cb.last_active.cmp(&ca.last_active)).then_with(|| ca.session.cmp(&cb.session))
    });
    let mut left = params.budget_tokens;
    ranked.retain(|r| estimate_tokens(&r.section) <= left && {
        left -= estimate_tokens(&r.section);
        true
    });
    ranked
}
