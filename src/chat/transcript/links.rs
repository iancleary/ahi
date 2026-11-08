//! Scanners for things in transcript text that a screen can open: `http(s)`
//! URLs and workspace-relative file references.
//!
//! Both scanners are pure: they report byte ranges into the input and never
//! touch the filesystem. A file reference is only a candidate; the caller
//! decides whether it exists. The path rules mirror the tool-call path
//! policy (relative, no `..`, no backslash, no control characters) so a
//! candidate that passes here is one the tools would also accept.

use std::ops::Range;

/// Longest URL reported. Longer runs are cut at a char boundary at or below
/// this many bytes, after trimming.
const MAX_URL_BYTES: usize = 2048;

/// Longest file reference token considered.
const MAX_REF_BYTES: usize = 1024;

/// File extensions that make a bare token (no `/`) a candidate.
const KNOWN_EXTENSIONS: &[&str] = &[
    "rs",
    "toml",
    "lock",
    "md",
    "txt",
    "json",
    "yaml",
    "yml",
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "py",
    "sh",
    "bash",
    "zsh",
    "ps1",
    "typ",
    "html",
    "css",
    "scss",
    "c",
    "h",
    "cpp",
    "hpp",
    "go",
    "java",
    "kt",
    "rb",
    "sql",
    "xml",
    "csv",
    "ini",
    "cfg",
    "conf",
    "env",
    "svg",
    "png",
    "jpg",
    "pdf",
    "proto",
    "dockerfile",
    "mk",
];

/// A file reference candidate found in text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileRef {
    /// Byte range of the whole token, `:line:col` suffix included.
    pub(crate) range: Range<usize>,
    /// The path part, as written.
    pub(crate) path: String,
    pub(crate) line: Option<u32>,
    pub(crate) column: Option<u32>,
}

/// `http://`/`https://` runs in `text`: byte ranges. Trailing `.,;:!?` and
/// unbalanced `)`/`]`/`}` and quotes are trimmed. Runs with control or
/// whitespace chars end there. Max 2048 bytes.
pub(crate) fn urls(text: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    let mut at = 0;
    while let Some(offset) = find_scheme(&text[at..]) {
        let start = at + offset;
        if start > 0 && is_token_byte(bytes[start - 1]) {
            // Scheme glued to a preceding word (`xhttp://`) is not a URL.
            at = start + 1;
            continue;
        }
        let mut end = start;
        for (index, c) in text[start..].char_indices() {
            if c.is_whitespace() || c.is_control() || c == '<' || c == '>' {
                break;
            }
            end = start + index + c.len_utf8();
        }
        let end = trim_url_end(text, start, end);
        let end = cap(text, start, end, MAX_URL_BYTES);
        // A bare scheme with no authority is not worth reporting.
        if end > start + scheme_len(&text[start..]) {
            found.push(start..end);
        }
        at = end.max(start + 1);
    }
    found
}

/// Candidate file references: a token with a `/` or a known extension,
/// optional `:line` and `:line:col`, no whitespace, no `..` component, not
/// starting with `/`, `~`, or a drive letter, no backslash. Trailing
/// punctuation trimmed. The caller checks existence. Tokens inside URLs
/// found by `urls` are not candidates.
pub(crate) fn file_refs(text: &str) -> Vec<FileRef> {
    let skip = urls(text);
    let mut found = Vec::new();
    let mut at = 0;
    let bytes = text.as_bytes();
    while at < text.len() {
        // Tokens are maximal runs without whitespace or token-ending bytes.
        let Some(c) = text[at..].chars().next() else {
            break;
        };
        if !is_token_start(c) {
            at += c.len_utf8();
            continue;
        }
        let mut end = at;
        for (index, c) in text[at..].char_indices() {
            if !is_token_char(c) {
                break;
            }
            end = at + index + c.len_utf8();
        }
        let next = end.max(at + 1);
        if skip.iter().any(|url| url.start < end && at < url.end) {
            at = next;
            continue;
        }
        // Strip delimiters on both sides: `(src/a.rs)`, `"a/b.rs",`, `` `x.rs` ``.
        let mut start = at;
        while start < end && is_opening(bytes[start]) {
            start += 1;
        }
        let end = trim_ref_end(text, start, end);
        if end > start && end - start <= MAX_REF_BYTES {
            if let Some(file_ref) = parse_ref(text, start, end) {
                found.push(file_ref);
            }
        }
        at = next;
    }
    found
}

/// Byte offset of the first `http://` or `https://` in `text`.
fn find_scheme(text: &str) -> Option<usize> {
    let a = text.find("http://");
    let b = text.find("https://");
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn scheme_len(text: &str) -> usize {
    if text.starts_with("https://") {
        8
    } else {
        7
    }
}

/// Word bytes that must not directly precede a scheme.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' || b == b'/'
}

/// Drop trailing punctuation and closers that have no opener within the run.
/// A Wikipedia-style `Foo_(bar)` keeps its closing paren; `(see http://x)`
/// loses it.
fn trim_url_end(text: &str, start: usize, mut end: usize) -> usize {
    loop {
        let run = &text[start..end];
        let Some(last) = run.chars().next_back() else {
            return end;
        };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '*' | '`' => true,
            ')' => count(run, '(') < count(run, ')'),
            ']' => count(run, '[') < count(run, ']'),
            '}' => count(run, '{') < count(run, '}'),
            _ => false,
        };
        if !drop {
            return end;
        }
        end -= last.len_utf8();
    }
}

fn count(text: &str, needle: char) -> usize {
    text.chars().filter(|&c| c == needle).count()
}

/// Cut `start..end` to at most `limit` bytes on a char boundary.
fn cap(text: &str, start: usize, end: usize, limit: usize) -> usize {
    if end - start <= limit {
        return end;
    }
    let mut cut = start + limit;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

fn is_token_start(c: char) -> bool {
    is_token_char(c)
}

/// Characters that may appear inside a file reference token, delimiters
/// included so that `(a/b.rs)` is one token to trim afterwards.
fn is_token_char(c: char) -> bool {
    !(c.is_whitespace() || c.is_control() || c == '<' || c == '>' || c == '|')
}

fn is_opening(b: u8) -> bool {
    matches!(b, b'(' | b'[' | b'{' | b'"' | b'\'' | b'`')
}

/// Trailing punctuation, quotes, and closers that are not balanced inside the
/// token are not part of the path.
fn trim_ref_end(text: &str, start: usize, mut end: usize) -> usize {
    loop {
        let run = &text[start..end];
        let Some(last) = run.chars().next_back() else {
            return end;
        };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '`' | '*' => true,
            ')' => count(run, '(') < count(run, ')'),
            ']' => count(run, '[') < count(run, ']'),
            '}' => count(run, '{') < count(run, '}'),
            _ => false,
        };
        if !drop {
            return end;
        }
        end -= last.len_utf8();
    }
}

/// Split `path[:line[:col]]`, then apply the path rules.
fn parse_ref(text: &str, start: usize, end: usize) -> Option<FileRef> {
    let token = &text[start..end];
    let (path, line, column) = split_position(token);
    if !is_candidate_path(path) {
        return None;
    }
    Some(FileRef {
        range: start..end,
        path: path.to_owned(),
        line,
        column,
    })
}

/// Peel up to two numeric `:n` suffixes. A non-numeric suffix leaves the
/// token whole, so `a:b/c.rs` is judged as a path (and rejected for `:`).
fn split_position(token: &str) -> (&str, Option<u32>, Option<u32>) {
    let (head, last) = match token.rsplit_once(':') {
        Some((head, last)) if !head.is_empty() => match last.parse::<u32>() {
            Ok(n) => (head, n),
            Err(_) => return (token, None, None),
        },
        _ => return (token, None, None),
    };
    match head.rsplit_once(':') {
        Some((path, line)) if !path.is_empty() => match line.parse::<u32>() {
            Ok(line) => (path, Some(line), Some(last)),
            Err(_) => (head, Some(last), None),
        },
        _ => (head, Some(last), None),
    }
}

/// The same rules the tool-call path policy applies, plus the requirement
/// that the token looks like a file at all: a `/` or a known extension.
fn is_candidate_path(path: &str) -> bool {
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('~')
        || has_drive_letter(path)
        || path.contains('\\')
        || path.contains(':')
        || path.contains("//")
        || path.ends_with('/')
    {
        return false;
    }
    if path
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || is_invisible_format(c))
    {
        return false;
    }
    if path.split('/').any(|part| part == "..") {
        return false;
    }
    let looks_like_file = path.contains('/') || has_known_extension(path);
    if !looks_like_file {
        return false;
    }
    // Every component needs a word character; `./`, `-`, and `*.rs` globs
    // are prose, not references.
    path.split('/')
        .all(|part| part.chars().any(|c| c.is_alphanumeric()) && !part.contains('*'))
}

fn has_drive_letter(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn has_known_extension(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            let ext = ext.to_ascii_lowercase();
            KNOWN_EXTENSIONS.contains(&ext.as_str())
        }
        _ => name.eq_ignore_ascii_case("dockerfile") || name.eq_ignore_ascii_case("justfile"),
    }
}

/// Bidirectional controls, zero-width characters, and similar `Cf` code points.
fn is_invisible_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2069}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url_strs(text: &str) -> Vec<&str> {
        urls(text).into_iter().map(|r| &text[r]).collect()
    }

    fn refs(text: &str) -> Vec<(String, Option<u32>, Option<u32>)> {
        file_refs(text)
            .into_iter()
            .map(|f| (f.path, f.line, f.column))
            .collect()
    }

    #[test]
    fn plain_url() {
        assert_eq!(
            url_strs("see https://example.com/a"),
            ["https://example.com/a"]
        );
    }

    #[test]
    fn http_and_https() {
        assert_eq!(
            url_strs("http://a.b and https://c.d"),
            ["http://a.b", "https://c.d"]
        );
    }

    #[test]
    fn trailing_punctuation_trimmed() {
        assert_eq!(url_strs("go to https://x.y/z."), ["https://x.y/z"]);
        assert_eq!(url_strs("go to https://x.y/z?!,"), ["https://x.y/z"]);
        assert_eq!(url_strs("https://x.y/z:"), ["https://x.y/z"]);
    }

    #[test]
    fn query_keeps_internal_punctuation() {
        assert_eq!(
            url_strs("https://x.y/z?a=1&b=2;c"),
            ["https://x.y/z?a=1&b=2;c"]
        );
    }

    #[test]
    fn unbalanced_paren_trimmed() {
        assert_eq!(url_strs("(see https://x.y/z)"), ["https://x.y/z"]);
        assert_eq!(url_strs("(see https://x.y/z)."), ["https://x.y/z"]);
    }

    #[test]
    fn wikipedia_style_paren_kept() {
        let text = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
        assert_eq!(url_strs(text), [text]);
        assert_eq!(
            url_strs("(https://en.wikipedia.org/wiki/Rust_(programming_language))"),
            ["https://en.wikipedia.org/wiki/Rust_(programming_language)"]
        );
    }

    #[test]
    fn brackets_and_quotes_trimmed() {
        assert_eq!(url_strs("[https://x.y/z]"), ["https://x.y/z"]);
        assert_eq!(url_strs("\"https://x.y/z\""), ["https://x.y/z"]);
        assert_eq!(url_strs("'https://x.y/z'"), ["https://x.y/z"]);
        assert_eq!(url_strs("<https://x.y/z>"), ["https://x.y/z"]);
    }

    #[test]
    fn markdown_link_syntax() {
        assert_eq!(url_strs("[docs](https://x.y/z)"), ["https://x.y/z"]);
        assert_eq!(url_strs("[docs](https://x.y/z \"t\")"), ["https://x.y/z"]);
    }

    #[test]
    fn url_ends_at_whitespace_and_control() {
        assert_eq!(
            url_strs("https://x.y/a\thttps://x.y/b"),
            ["https://x.y/a", "https://x.y/b"]
        );
        assert_eq!(url_strs("https://x.y/a\u{1}b"), ["https://x.y/a"]);
        assert_eq!(url_strs("https://x.y/a\nb"), ["https://x.y/a"]);
    }

    #[test]
    fn bare_scheme_not_reported() {
        assert!(url_strs("https:// is a scheme").is_empty());
        assert!(url_strs("xhttps://a.b").is_empty());
    }

    #[test]
    fn url_ranges_are_byte_offsets() {
        let text = "héllo https://x.y/é.";
        let ranges = urls(text);
        assert_eq!(ranges.len(), 1);
        assert_eq!(&text[ranges[0].clone()], "https://x.y/é");
    }

    #[test]
    fn url_capped_at_2048_bytes_on_char_boundary() {
        let mut text = String::from("https://x.y/");
        while text.len() < 2047 {
            text.push('a');
        }
        text.push('é'); // straddles the 2048 boundary
        text.push_str("aaaa");
        let ranges = urls(&text);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].end, 2047);
        assert!(text.is_char_boundary(ranges[0].end));

        let ascii = format!("https://x.y/{}", "b".repeat(3000));
        assert_eq!(urls(&ascii)[0].len(), 2048);
    }

    #[test]
    fn ref_with_line_and_column() {
        assert_eq!(
            refs("error at src/a.rs:12:5 here"),
            [("src/a.rs".to_owned(), Some(12), Some(5))]
        );
        let found = file_refs("error at src/a.rs:12:5 here");
        assert_eq!(found[0].range, 9..22);
    }

    #[test]
    fn ref_with_line_only() {
        assert_eq!(
            refs("src/a.rs:12"),
            [("src/a.rs".to_owned(), Some(12), None)]
        );
    }

    #[test]
    fn bare_known_extension() {
        assert_eq!(
            refs("edit Cargo.toml now"),
            [("Cargo.toml".to_owned(), None, None)]
        );
        assert_eq!(refs("Justfile"), [("Justfile".to_owned(), None, None)]);
    }

    #[test]
    fn unknown_extension_without_slash_rejected() {
        assert!(refs("file.unknownext").is_empty());
        assert!(refs("v1.2.3").is_empty());
        assert!(refs("e.g. this").is_empty());
    }

    #[test]
    fn trailing_colon_trimmed() {
        assert_eq!(refs("a/b.rs:"), [("a/b.rs".to_owned(), None, None)]);
        assert_eq!(refs("in a/b.rs: it"), [("a/b.rs".to_owned(), None, None)]);
    }

    #[test]
    fn trailing_punctuation_trimmed_from_refs() {
        assert_eq!(
            refs("see src/lib.rs."),
            [("src/lib.rs".to_owned(), None, None)]
        );
        assert_eq!(
            refs("see src/lib.rs:3,"),
            [("src/lib.rs".to_owned(), Some(3), None)]
        );
    }

    #[test]
    fn delimiters_stripped_around_ref() {
        assert_eq!(refs("(src/a.rs)"), [("src/a.rs".to_owned(), None, None)]);
        assert_eq!(refs("`src/a.rs`"), [("src/a.rs".to_owned(), None, None)]);
        assert_eq!(refs("\"src/a.rs\","), [("src/a.rs".to_owned(), None, None)]);
        assert_eq!(
            refs("[src/a.rs:4]"),
            [("src/a.rs".to_owned(), Some(4), None)]
        );
    }

    #[test]
    fn windows_paths_rejected() {
        assert!(refs("C:\\Users\\x\\a.rs").is_empty());
        assert!(refs("C:/Users/x/a.rs").is_empty());
        assert!(refs("src\\a.rs").is_empty());
    }

    #[test]
    fn parent_component_rejected() {
        assert!(refs("../x").is_empty());
        assert!(refs("a/../b.rs").is_empty());
        assert!(refs("../x.rs:3").is_empty());
    }

    #[test]
    fn absolute_and_home_rejected() {
        assert!(refs("/abs").is_empty());
        assert!(refs("/abs/a.rs:1").is_empty());
        assert!(refs("~/a.rs").is_empty());
    }

    #[test]
    fn tokens_inside_urls_not_reported() {
        assert!(refs("https://example.com/src/a.rs").is_empty());
        assert_eq!(
            refs("see https://example.com/x/y.md and src/a.rs"),
            [("src/a.rs".to_owned(), None, None)]
        );
    }

    #[test]
    fn whitespace_breaks_refs() {
        assert_eq!(
            refs("src/a.rs src/b.rs"),
            [
                ("src/a.rs".to_owned(), None, None),
                ("src/b.rs".to_owned(), None, None)
            ]
        );
        assert_eq!(refs("src/ a.rs"), [("a.rs".to_owned(), None, None)]);
    }

    #[test]
    fn control_and_invisible_chars_rejected() {
        assert!(refs("src/a\u{200B}.rs").is_empty());
        assert!(refs("src/a\u{202E}b.rs").is_empty());
    }

    #[test]
    fn prose_slashes_rejected() {
        assert!(refs("a/").is_empty());
        assert!(refs("./").is_empty());
        assert!(refs("src/*.rs").is_empty());
        assert!(refs("a//b").is_empty());
    }

    #[test]
    fn non_numeric_colon_suffix_rejected() {
        assert!(refs("a:b/c.rs").is_empty());
        assert!(refs("src/a.rs:abc").is_empty());
    }

    #[test]
    fn ref_ranges_are_byte_offsets() {
        let text = "é src/ü.rs:7 x";
        let found = file_refs(text);
        assert_eq!(found.len(), 1);
        assert_eq!(&text[found[0].range.clone()], "src/ü.rs:7");
        assert_eq!(found[0].path, "src/ü.rs");
        assert_eq!(found[0].line, Some(7));
    }

    #[test]
    fn oversized_ref_token_ignored() {
        let long = format!("{}/a.rs", "x".repeat(MAX_REF_BYTES));
        assert!(refs(&long).is_empty());
    }
}
