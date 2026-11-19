//! Ranks a project's digests and packs them into a token budget.
//!
//! Score = `relevance_weight * relevance + (1 - relevance_weight) * recency`,
//! where relevance is BM25 of the query (the new chat's first prompt or
//! title) over the digest's title, summary, decisions, files and open items,
//! divided by the best BM25 among the candidates, and recency halves every
//! `half_life_ms` since the session was last active. Ties go to the most
//! recently active session, then the lower session id.
//!
//! Token cost is estimated as one token per four bytes of the rendered
//! section, rounded up ([`estimate_tokens`]). Packing walks the ranked list
//! and keeps every section that still fits.

use crate::context::render_digest_section;
use crate::digest::{Digest, digest_of};
use crate::settings::is_excluded;
use crate::text::words;
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
    /// The candidate for a session DOM, `None` without a digest.
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
}

/// Unix milliseconds of the latest turn start or end, else the session's creation.
pub fn last_active(dom: &Dom) -> i64 {
    let Some(root) = dom.root() else { return 0 };
    let created = root.int_attr(attr::CREATED).unwrap_or(0);
    dom.children_of_kind(root.id, ElementKind::Turn)
        .flat_map(|t| [t.int_attr(attr::STARTED), t.int_attr(attr::ENDED)])
        .flatten()
        .fold(created, i64::max)
}

#[derive(Clone, Debug, PartialEq)]
pub struct RankParams {
    /// Unix milliseconds "now" for recency.
    pub now: i64,
    pub half_life_ms: i64,
    /// Share of relevance in the score, 0 to 1.
    pub relevance_weight: f64,
    pub budget_tokens: usize,
    /// The chat the context is for; its own digest is left out.
    pub exclude_session: Option<String>,
}

impl RankParams {
    /// A week half life and 70 % relevance.
    pub fn new(now: i64, budget_tokens: usize) -> Self {
        Self { now, half_life_ms: 7 * 24 * 3600 * 1000, relevance_weight: 0.7, budget_tokens, exclude_session: None }
    }
}

/// A packed digest and why it ranked where it did.
#[derive(Clone, Debug, PartialEq)]
pub struct Ranked {
    pub candidate: Candidate,
    pub score: f64,
    pub relevance: f64,
    pub recency: f64,
    /// The rendered markdown section and its estimated tokens.
    pub section: String,
    pub tokens: usize,
}

/// Estimated tokens of `text`: bytes divided by four, rounded up.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

const K1: f64 = 1.2;
const B: f64 = 0.75;

/// Ranks `candidates` against `query` and keeps those that fit the budget,
/// best first. Excluded sessions and `params.exclude_session` are dropped
/// before scoring, so they don't shift other sessions' scores either.
pub fn rank_and_pack(candidates: Vec<Candidate>, query: &str, params: &RankParams) -> Vec<Ranked> {
    let candidates: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| !c.excluded && params.exclude_session.as_deref() != Some(c.session.as_str()))
        .collect();
    let docs: Vec<HashMap<String, usize>> = candidates.iter().map(term_counts).collect();
    let lengths: Vec<usize> = docs.iter().map(|d| d.values().sum()).collect();
    let avg_len = lengths.iter().sum::<usize>() as f64 / lengths.len().max(1) as f64;
    let mut query_terms: Vec<String> = words(query).collect();
    query_terms.sort();
    query_terms.dedup();

    let n = docs.len() as f64;
    let idf: Vec<f64> = query_terms
        .iter()
        .map(|t| {
            let df = docs.iter().filter(|d| d.contains_key(t)).count() as f64;
            (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
        })
        .collect();
    let bm25: Vec<f64> = docs
        .iter()
        .zip(&lengths)
        .map(|(doc, len)| {
            query_terms
                .iter()
                .zip(&idf)
                .map(|(t, idf)| {
                    let tf = *doc.get(t).unwrap_or(&0) as f64;
                    idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * *len as f64 / avg_len.max(1.0)))
                })
                .sum()
        })
        .collect();
    let best = bm25.iter().copied().fold(0.0, f64::max);

    let mut ranked: Vec<Ranked> = candidates
        .into_iter()
        .zip(bm25)
        .map(|(candidate, bm25)| {
            let relevance = if best > 0.0 { bm25 / best } else { 0.0 };
            let age = params.now.saturating_sub(candidate.last_active).max(0) as f64;
            let recency = 0.5f64.powf(age / params.half_life_ms.max(1) as f64);
            let score = params.relevance_weight * relevance + (1.0 - params.relevance_weight) * recency;
            let section = render_digest_section(&candidate);
            let tokens = estimate_tokens(&section);
            Ranked { candidate, score, relevance, recency, section, tokens }
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.candidate.last_active.cmp(&a.candidate.last_active))
            .then_with(|| a.candidate.session.cmp(&b.candidate.session))
    });
    let mut left = params.budget_tokens;
    ranked.retain(|r| {
        let fits = r.tokens <= left;
        if fits {
            left -= r.tokens;
        }
        fits
    });
    ranked
}

fn term_counts(c: &Candidate) -> HashMap<String, usize> {
    let d = &c.digest.content;
    let mut counts = HashMap::new();
    let fields = c
        .title
        .iter()
        .map(String::as_str)
        .chain([d.summary.as_str()])
        .chain(d.decisions.iter().map(String::as_str))
        .chain(d.files.iter().flat_map(|f| [f.path.as_str(), f.note.as_str()]))
        .chain(d.open_items.iter().map(String::as_str));
    for field in fields {
        for word in words(field) {
            *counts.entry(word).or_insert(0) += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::{DigestContent, FileNote};
    use loa_core::NodeId;

    const DAY: i64 = 24 * 3600 * 1000;

    fn candidate(session: &str, summary: &str, files: &[&str], last_active: i64) -> Candidate {
        Candidate {
            session: session.into(),
            title: None,
            digest: Digest {
                node: NodeId(9),
                content: DigestContent {
                    summary: summary.into(),
                    files: files.iter().map(|f| FileNote { path: (*f).into(), note: String::new() }).collect(),
                    ..Default::default()
                },
                source_seq: 1,
                stale: false,
                model: None,
                created: None,
            },
            last_active,
            excluded: false,
        }
    }

    fn order(ranked: &[Ranked]) -> Vec<&str> {
        ranked.iter().map(|r| r.candidate.session.as_str()).collect()
    }

    #[test]
    fn relevant_digest_beats_a_newer_unrelated_one() {
        let now = 100 * DAY;
        let candidates = vec![
            candidate("new", "Tuned the CI cache for faster builds.", &[".github/ci.yml"], now),
            candidate("old", "Built the terminal pane with alacritty.", &["crates/loa-ui-terminal/src/lib.rs"], now - 10 * DAY),
        ];
        let ranked = rank_and_pack(candidates, "fix scrolling in the terminal pane", &RankParams::new(now, 10_000));
        assert_eq!(order(&ranked), ["old", "new"]);
        assert!(ranked[0].relevance > ranked[1].relevance);
    }

    #[test]
    fn without_overlap_recency_decides_and_ties_are_stable() {
        let now = 100 * DAY;
        let candidates = vec![
            candidate("b", "alpha", &[], now - DAY),
            candidate("a", "beta", &[], now - DAY),
            candidate("c", "gamma", &[], now),
        ];
        let ranked = rank_and_pack(candidates, "unrelated words", &RankParams::new(now, 10_000));
        assert_eq!(order(&ranked), ["c", "a", "b"]);
    }

    #[test]
    fn packing_respects_the_budget_and_skips_what_does_not_fit() {
        let now = DAY;
        let big = "word ".repeat(400);
        let candidates =
            vec![candidate("big", &format!("terminal {big}"), &[], now), candidate("small", "terminal note", &[], now - DAY)];
        let all = rank_and_pack(candidates.clone(), "terminal", &RankParams::new(now, 10_000));
        let small_cost = all.iter().find(|r| r.candidate.session == "small").unwrap().tokens;
        let ranked = rank_and_pack(candidates, "terminal", &RankParams::new(now, small_cost + 10));
        assert_eq!(order(&ranked), ["small"]);
        assert!(ranked.iter().map(|r| r.tokens).sum::<usize>() <= small_cost + 10);
        assert_eq!(ranked[0].tokens, estimate_tokens(&ranked[0].section));
    }

    #[test]
    fn excluded_sessions_and_the_chat_itself_are_left_out() {
        let now = DAY;
        let mut excluded = candidate("x", "terminal work", &[], now);
        excluded.excluded = true;
        let candidates =
            vec![excluded, candidate("self", "terminal work", &[], now), candidate("other", "terminal work", &[], now)];
        let params = RankParams { exclude_session: Some("self".into()), ..RankParams::new(now, 10_000) };
        assert_eq!(order(&rank_and_pack(candidates, "terminal", &params)), ["other"]);
    }
}
