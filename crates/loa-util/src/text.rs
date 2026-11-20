//! UTF-8-safe cutting, URL escaping, random tokens and MIME lookup.

use std::path::Path;

/// The longest prefix of `s` that is at most `max` bytes and ends on a char boundary.
pub fn clip(s: &str, max: usize) -> &str { &s[..floor_boundary(s, max)] }

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
        assert_eq!(percent_encode("a b/é~"), "a%20b%2F%C3%A9~");
        assert!(random_hex(8).len() == 16 && random_hex(8) != random_hex(8));
        let mimes = [("x.PNG", Some("image/png")), ("doc.pdf", Some("application/pdf")), ("a.rs", None), ("noext", None)];
        mimes.iter().for_each(|&(p, m)| assert_eq!(mime_of(Path::new(p)), m));
    }
}
