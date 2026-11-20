//! Fuzzy matching for the `/` and `@` popups.

/// Scores `candidate` against `query`, or `None` when the query characters do
/// not appear in order. Case is ignored. Higher is better: matches at word
/// starts and runs of consecutive matches score more, skipped characters less.
#[cfg(test)]
fn score(query: &str, candidate: &str) -> Option<i32> {
    score_folded(&folded(query), candidate)
}

/// Indices of `candidates` matching `query`, best first, at most `limit`.
/// Ties go to the shorter candidate, then to the earlier one. An empty query
/// keeps every candidate in its original order.
pub fn filter<'a>(query: &str, candidates: impl IntoIterator<Item = &'a str>, limit: usize) -> Vec<usize> {
    if query.is_empty() {
        return candidates.into_iter().take(limit).enumerate().map(|(i, _)| i).collect();
    }
    let query = folded(query);
    let mut scored: Vec<(i32, usize, usize)> = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(i, c)| score_folded(&query, c).map(|s| (s, c.len(), i)))
        .collect();
    scored.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    scored.truncate(limit);
    scored.into_iter().map(|(_, _, i)| i).collect()
}

/// The query's characters folded once, so each candidate is compared without
/// lowercasing the query again.
fn folded(query: &str) -> Vec<char> {
    query.chars().map(fold_case).collect()
}

/// The rule of `score`, for a query already passed through `folded`.
fn score_folded(query: &[char], candidate: &str) -> Option<i32> {
    let mut wanted = query.iter().copied().peekable();
    let mut total = 0i32;
    let mut previous: Option<char> = None;
    let mut last_match: Option<usize> = None;
    let mut gap = 0i32;

    for (index, c) in candidate.chars().enumerate() {
        let Some(q) = wanted.peek().copied() else { break };
        if fold_case(c) == q {
            wanted.next();
            total += 1;
            if is_word_start(previous, c) {
                total += 6;
            }
            if last_match.is_some_and(|last| last + 1 == index) {
                total += 4;
            }
            if last_match.is_some() {
                total -= gap.min(4);
            }
            gap = 0;
            last_match = Some(index);
        } else if last_match.is_some() {
            gap += 1;
        }
        previous = Some(c);
    }

    wanted.peek().is_none().then_some(total)
}

/// Case-folded form of `c`: its lowercase when that is one character. Two
/// characters match exactly when their folds are equal.
fn fold_case(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

fn is_word_start(previous: Option<char>, c: char) -> bool {
    match previous {
        None => true,
        Some(p) => matches!(p, '/' | '\\' | '_' | '-' | '.' | ' ' | ':') || (p.is_lowercase() && c.is_uppercase()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_out_of_order_and_ignores_case() {
        assert!(score("mr", "src/main.rs").is_some());
        assert!(score("MAIN", "src/main.rs").is_some());
        assert!(score("rm", "src/xy.rs").is_none());
    }

    #[test]
    fn word_starts_beat_scattered_letters() {
        let files = ["crates/composer/src/model.rs", "src/main.rs"];
        assert_eq!(filter("main", files, 10), vec![1]);
        let files = ["xmxoxdxexl", "src/model.rs"];
        assert_eq!(filter("model", files, 10), vec![1, 0]);
    }
}
