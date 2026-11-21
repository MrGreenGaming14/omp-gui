//! UTF-8-safe cutting and paging, URL escaping, random tokens and MIME lookup.

use std::path::Path;

/// The longest prefix of `s` that is at most `max` bytes and ends on a char boundary.
pub fn clip(s: &str, max: usize) -> &str { &s[..floor_boundary(s, max)] }

/// `s` clipped to `max` bytes with a marker saying how much was cut.
pub fn clip_marked(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    let kept = clip(s, max);
    if kept.len() == s.len() {
        return s.into();
    }
    format!("{kept}… [{} more bytes]", s.len() - kept.len()).into()
}

/// Page sizes: a request of 0 gets `default`, any other is capped at `max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paginator {
    pub default: usize,
    pub max: usize,
}

impl Paginator {
    pub fn size(&self, requested: usize) -> usize { if requested == 0 { self.default } else { requested.min(self.max) } }

    /// The byte page of `text` at `offset` and the next page's offset. Pages end on char
    /// boundaries, on a line break when one is in the page's second half, and always advance.
    pub fn bytes(&self, text: &str, offset: usize, requested: usize) -> (std::ops::Range<usize>, Option<usize>) {
        let max = self.size(requested);
        let start = floor_boundary(text, offset);
        let mut end = floor_boundary(text, start.saturating_add(max));
        if end < text.len() {
            if let Some(nl) = text[start..end].rfind('\n').filter(|nl| *nl > max / 2) {
                end = start + nl + 1;
            }
            if end == start {
                end = ceil_boundary(text, start + 1);
            }
        }
        (start..end, (end < text.len()).then_some(end))
    }
}

/// The first non-blank line of `s`, trimmed and cut to `max` chars with a trailing `…`.
pub fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
    line.char_indices().nth(max).map_or_else(|| line.to_owned(), |(cut, _)| format!("{}…", &line[..cut]))
}

/// The largest char boundary of `s` at or below `i`.
pub fn floor_boundary(s: &str, i: usize) -> usize { (0..=i.min(s.len())).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0) }

/// The smallest char boundary of `s` at or above `i`.
pub fn ceil_boundary(s: &str, i: usize) -> usize { (i.min(s.len())..=s.len()).find(|&i| s.is_char_boundary(i)).unwrap_or(s.len()) }

/// `s` with every byte outside RFC 3986 unreserved characters written as `%XX`.
pub fn percent_encode(s: &str) -> String {
    let keep = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~');
    s.bytes().map(|b| if keep(b) { char::from(b).to_string() } else { format!("%{b:02X}") }).collect()
}

/// `bytes` bytes from the OS random source as lowercase hex.
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("OS random source");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// The MIME type of the media and document extensions harnesses treat specially.
pub fn mime_of(p: &Path) -> Option<&'static str> {
    Some(match p.extension()?.to_str()?.to_ascii_lowercase().as_str() {
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
    fn cuts_escapes_and_looks_up() {
        let s = "aé€b"; // byte offsets: a=0, é=1..3, €=3..6, b=6
        [(0, 0, 0), (2, 1, 3), (4, 3, 6), (9, 7, 7)].iter().for_each(|&(i, lo, hi)| assert_eq!((floor_boundary(s, i), ceil_boundary(s, i)), (lo, hi), "{i}"));
        assert_eq!((clip(s, 5), clip(s, 99)), ("aé", s));
        assert_eq!((first_line("\n  hello world \nx", 5), first_line("hi", 5)), ("hello…".to_owned(), "hi".to_owned()));
        assert_eq!((clip_marked("abcdef", 2), clip_marked("ab", 2)), ("ab… [4 more bytes]".into(), "ab".into()));
        assert_eq!(percent_encode("a b/é~"), "a%20b%2F%C3%A9~");
        assert!(random_hex(8).len() == 16 && random_hex(8) != random_hex(8));
        let mimes = [("x.PNG", Some("image/png")), ("doc.pdf", Some("application/pdf")), ("a.rs", None), ("noext", None)];
        mimes.iter().for_each(|&(p, m)| assert_eq!(mime_of(Path::new(p)), m));
    }

    #[test]
    fn byte_pages_cover_the_text_exactly_once() {
        let pages = Paginator { default: 8, max: 16 };
        assert_eq!((pages.size(0), pages.size(5), pages.size(99)), (8, 5, 16));
        for text in ["", "short", "line one\nline two\nline three\n", &"ü€x\n".repeat(40)] {
            let (mut joined, mut offset) = (String::new(), 0);
            loop {
                let (range, next) = pages.bytes(text, offset, 7);
                assert!(range.start == offset && range.len() <= 7 && (!range.is_empty() || text.is_empty()));
                joined.push_str(&text[range]);
                match next {
                    Some(n) => offset = n,
                    None => break,
                }
            }
            assert_eq!(joined, text);
        }
    }
}
