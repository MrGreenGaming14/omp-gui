//! `@path` file mentions: how they are written into the message and how a
//! sent message turns them into attachments.

use std::path::PathBuf;

use loa_adapter::Attachment;

/// The text inserted for a mention of `path`. Paths with whitespace are quoted
/// so the mention stays one word.
pub fn token(path: &str) -> String {
    if path.contains(char::is_whitespace) { format!("@\"{path}\"") } else { format!("@{path}") }
}

/// Paths written as `@path` or `@"path"` in `text`, in order. An unquoted
/// mention yields both the whole word and, when it ends in punctuation, the
/// word without it, so `see @a.rs,` can resolve to `a.rs`.
pub fn mentioned(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    let mut at_word_start = true;
    while let Some(c) = rest.chars().next() {
        if c == '@' && at_word_start {
            let after = &rest[1..];
            if let Some(quoted) = after.strip_prefix('"')
                && let Some(close) = quoted.find('"')
            {
                found.push(&quoted[..close]);
                rest = &quoted[close + 1..];
                at_word_start = false;
                continue;
            }
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            let word = &after[..end];
            if !word.is_empty() {
                found.push(word);
                let trimmed = word.trim_end_matches([',', '.', ';', ':', '!', '?', ')', ']', '"', '\'']);
                if !trimmed.is_empty() && trimmed.len() != word.len() {
                    found.push(trimmed);
                }
            }
            rest = &after[end..];
            at_word_start = false;
            continue;
        }
        at_word_start = c.is_whitespace();
        rest = &rest[c.len_utf8()..];
    }
    found
}

/// Attachments for the mentions in `text` whose path `is_known` accepts,
/// without duplicates, in order of first mention.
pub fn resolve(text: &str, is_known: impl Fn(&str) -> bool) -> Vec<Attachment> {
    let mut attachments: Vec<Attachment> = Vec::new();
    for path in mentioned(text) {
        if is_known(path) && !attachments.iter().any(|a| a.path.as_os_str() == path) {
            attachments.push(Attachment { path: PathBuf::from(path), mime: mime(path).map(str::to_owned) });
        }
    }
    attachments
}

/// MIME type for the extensions a harness treats differently from text.
/// Everything else is left to the adapter.
pub(crate) fn mime(path: &str) -> Option<&'static str> {
    let (_, ext) = path.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_mentions_once_with_mime() {
        let known = ["src/main.rs", "docs/a b.png"];
        let text = "fix @src/main.rs, compare @\"docs/a b.png\" and @src/main.rs and @nope or me@src/main.rs";
        let got = resolve(text, |p| known.contains(&p));
        assert_eq!(
            got,
            vec![
                Attachment { path: "src/main.rs".into(), mime: None },
                Attachment { path: "docs/a b.png".into(), mime: Some("image/png".into()) },
            ]
        );
    }
}
