//! Markdown subset parser for assistant replies.
//!
//! Blocks carry the byte range of the source they came from, so the screen can
//! copy raw text and a streamed reply can be re-parsed from `last_block_start`
//! on every append instead of from its first byte. The subset is what models
//! emit in practice: ATX headings, rules, fenced code, lists, one level of
//! quote, pipe tables, and paragraphs with the usual inline marks. Anything
//! else is literal text. No byte is lost: every byte of the source is either
//! in a run's text or a marker the parse consumed. Unterminated constructs
//! stay literal so a half-received stream never swallows text.

use std::ops::Range;

/// A run of inline text with one style, in on-screen form (markers removed).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Run {
    /// Rendered text. A hard line break is a `\n` inside it.
    pub(crate) text: String,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) code: bool,
    pub(crate) strike: bool,
    /// Destination for `[text](url)`; autolinks have `link == Some(text)`.
    pub(crate) link: Option<String>,
}

/// Column alignment from a table delimiter row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Align {
    Left,
    Center,
    Right,
}

/// One block-level element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Block {
    Heading {
        level: u8,
        runs: Vec<Run>,
    },
    Paragraph {
        runs: Vec<Run>,
    },
    /// `closed` is false while a stream has not yet sent the closing fence.
    Code {
        lang: Option<String>,
        lines: Vec<String>,
        closed: bool,
    },
    /// `marker` is what to draw: "•" for bullets, "3." for ordered, "[x]"/"[ ]" for tasks.
    ListItem {
        depth: u8,
        marker: String,
        runs: Vec<Run>,
    },
    Quote {
        runs: Vec<Run>,
    },
    Rule,
    Table {
        header: Vec<Vec<Run>>,
        align: Vec<Align>,
        rows: Vec<Vec<Vec<Run>>>,
    },
    Blank,
}

/// A block with the source it was parsed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub(crate) block: Block,
    /// Byte range of the source text this block came from (for raw copy).
    pub(crate) source: Range<usize>,
}

/// Parse `text` into blocks. Tolerant of unfinished input: an open fence is a
/// `Code` block with `closed == false`, a lone table header is a paragraph
/// until its delimiter row arrives, and unmatched inline markers are literal.
pub(crate) fn parse(text: &str) -> Vec<Parsed> {
    let lines = split_lines(text);
    let mut blocks = Vec::new();
    // Indents of the open list items, innermost last; cleared by any block
    // that is not a list item or a blank line.
    let mut list_indents: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let first = i;
        let (indent, body) = split_indent(lines[i].text);
        let block = if body.trim().is_empty() {
            i += 1;
            Block::Blank
        } else if let Some((fence, len, lang)) = fence_open(body) {
            i += 1;
            let mut code = Vec::new();
            let mut closed = false;
            while i < lines.len() {
                let (_, candidate) = split_indent(lines[i].text);
                i += 1;
                if fence_close(candidate, fence, len) {
                    closed = true;
                    break;
                }
                code.push(strip_spaces(lines[i - 1].text, indent).to_string());
            }
            Block::Code {
                lang,
                lines: code,
                closed,
            }
        } else if indent <= 3 && is_rule(body) {
            i += 1;
            Block::Rule
        } else if let Some((level, content)) = heading(body).filter(|_| indent <= 3) {
            i += 1;
            Block::Heading {
                level,
                runs: inline(content),
            }
        } else if let Some((marker, content)) = list_marker(body) {
            while list_indents.last().is_some_and(|&top| top > indent) {
                list_indents.pop();
            }
            if list_indents.last() != Some(&indent) {
                list_indents.push(indent);
            }
            let depth = u8::try_from(list_indents.len() - 1).unwrap_or(u8::MAX);
            i += 1;
            let parts = continuation(&lines, &mut i, content);
            Block::ListItem {
                depth,
                marker,
                runs: inline(&join_lines(&parts)),
            }
        } else if indent <= 3 && body.starts_with('>') {
            let mut parts = Vec::new();
            while i < lines.len() {
                let (quote_indent, quote_body) = split_indent(lines[i].text);
                if quote_indent > 3 || !quote_body.starts_with('>') {
                    break;
                }
                let content = &quote_body[1..];
                parts.push(content.strip_prefix(' ').unwrap_or(content));
                i += 1;
            }
            Block::Quote {
                runs: inline(&join_quote(&parts)),
            }
        } else if let Some(align) = table_start(&lines, i) {
            let header = split_cells(body).iter().map(|c| inline(c)).collect();
            i += 2;
            let mut rows = Vec::new();
            while i < lines.len() {
                let (row_indent, row_body) = split_indent(lines[i].text);
                if row_body.trim().is_empty() || starts_block(row_body, row_indent) {
                    break;
                }
                let mut cells: Vec<Vec<Run>> =
                    split_cells(row_body).iter().map(|c| inline(c)).collect();
                while cells.len() < align.len() {
                    cells.push(Vec::new());
                }
                rows.push(cells);
                i += 1;
            }
            Block::Table {
                header,
                align,
                rows,
            }
        } else {
            i += 1;
            let parts = continuation(&lines, &mut i, body);
            Block::Paragraph {
                runs: inline(&join_lines(&parts)),
            }
        };
        if !matches!(block, Block::ListItem { .. } | Block::Blank) {
            list_indents.clear();
        }
        blocks.push(Parsed {
            block,
            source: lines[first].start..lines[i - 1].next,
        });
    }
    blocks
}

// Kept for an incremental streaming layout that re-parses from the last
// block; the screen re-lays the whole last message today.
#[cfg_attr(not(test), allow(dead_code))]
/// Byte offset of the start of the last block that `parse(text)` would produce
/// after appending to `text`: callers re-parse from here on a stream append.
pub(crate) fn last_block_start(text: &str) -> usize {
    let blocks = parse(text);
    let Some(last) = blocks.last() else {
        return 0;
    };
    // A heading, rule, closed fence, or blank line that already has its line
    // terminator cannot grow: whatever comes next opens a new block.
    let settled = matches!(
        last.block,
        Block::Heading { .. } | Block::Rule | Block::Blank | Block::Code { closed: true, .. }
    );
    if settled && text.ends_with('\n') {
        text.len()
    } else {
        last.source.start
    }
}

/// One source line without its terminator, with where it starts and where the
/// next line starts (the end of the input for the last line).
struct Line<'a> {
    text: &'a str,
    start: usize,
    next: usize,
}

fn split_lines(text: &str) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let rest = &text[start..];
        let (len, next) = match rest.find('\n') {
            Some(n) => (n, start + n + 1),
            None => (rest.len(), text.len()),
        };
        let line = &rest[..len];
        let line = line.strip_suffix('\r').unwrap_or(line);
        out.push(Line {
            text: line,
            start,
            next,
        });
        start = next;
    }
    out
}

/// Leading blanks (spaces and tabs, one column each) and the rest.
fn split_indent(line: &str) -> (usize, &str) {
    let indent = line
        .bytes()
        .take_while(|&b| b == b' ' || b == b'\t')
        .count();
    (indent, &line[indent..])
}

/// `text` with up to `max` leading spaces removed.
fn strip_spaces(text: &str, max: usize) -> &str {
    let n = text.bytes().take_while(|&b| b == b' ').count().min(max);
    &text[n..]
}

/// Lines that continue a paragraph or list item: non-blank lines up to the
/// next block start. Lazy continuation is allowed, so indentation does not
/// matter. Advances `*i` past them.
fn continuation<'a>(lines: &[Line<'a>], i: &mut usize, first: &'a str) -> Vec<&'a str> {
    let mut parts = vec![first];
    while *i < lines.len() {
        let (indent, body) = split_indent(lines[*i].text);
        if body.trim().is_empty() || starts_block(body, indent) || table_start(lines, *i).is_some()
        {
            break;
        }
        parts.push(body);
        *i += 1;
    }
    parts
}

/// Whether a line opens a block that interrupts a paragraph, list item, or table.
fn starts_block(body: &str, indent: usize) -> bool {
    fence_open(body).is_some()
        || (indent <= 3 && (heading(body).is_some() || is_rule(body) || body.starts_with('>')))
        || list_marker(body).is_some()
}

/// Join paragraph lines. Every line break is kept as a `\n`: in a chat
/// transcript a model's newline is a line break, not CommonMark's soft break,
/// and a joined paragraph would reflow lists of short lines into prose. A
/// trailing backslash is consumed as the hard-break marker it is; trailing
/// blanks are trimmed on every line.
fn join_lines(parts: &[&str]) -> String {
    let mut out = String::new();
    let last = parts.len().saturating_sub(1);
    for (k, part) in parts.iter().enumerate() {
        let trimmed = part.trim_end();
        let trimmed = trimmed.strip_suffix('\\').unwrap_or(trimmed);
        out.push_str(trimmed);
        if k != last {
            out.push('\n');
        }
    }
    out
}

/// Join quote lines like a paragraph, with an empty `>` line as a line break.
fn join_quote(parts: &[&str]) -> String {
    let mut out = String::new();
    for (k, group) in parts.split(|p| p.trim().is_empty()).enumerate() {
        if k > 0 {
            out.push('\n');
        }
        if !group.is_empty() {
            out.push_str(&join_lines(group));
        }
    }
    out
}

/// Fence character, length, and language of an opening fence line. Any
/// indent is accepted so a fence inside a nested list item still counts.
fn fence_open(body: &str) -> Option<(u8, usize, Option<String>)> {
    let fence = *body.as_bytes().first()?;
    if fence != b'`' && fence != b'~' {
        return None;
    }
    let len = body.bytes().take_while(|&b| b == fence).count();
    if len < 3 {
        return None;
    }
    let info = body[len..].trim();
    if fence == b'`' && info.contains('`') {
        return None;
    }
    let lang = info.split_whitespace().next().map(str::to_string);
    Some((fence, len, lang))
}

fn fence_close(body: &str, fence: u8, len: usize) -> bool {
    let n = body.bytes().take_while(|&b| b == fence).count();
    n >= len && body[n..].trim().is_empty()
}

/// Level and content of an ATX heading, closing `#` sequence removed.
fn heading(body: &str) -> Option<(u8, &str)> {
    let level = body.bytes().take_while(|&b| b == b'#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &body[level..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let rest = rest.trim();
    let without_close = rest.trim_end_matches('#');
    let content = if without_close.len() == rest.len() {
        rest
    } else if without_close.is_empty() || without_close.ends_with([' ', '\t']) {
        without_close.trim_end()
    } else {
        rest
    };
    Some((level as u8, content))
}

/// Three or more of one of `*`, `-`, `_`, blanks allowed between.
fn is_rule(body: &str) -> bool {
    let Some(mark) = body.chars().next() else {
        return false;
    };
    if !matches!(mark, '*' | '-' | '_') {
        return false;
    }
    let mut count = 0;
    for c in body.chars() {
        if c == mark {
            count += 1;
        } else if c != ' ' && c != '\t' {
            return false;
        }
    }
    count >= 3
}

/// Marker to draw and item content for a list item line.
fn list_marker(body: &str) -> Option<(String, &str)> {
    let bytes = body.as_bytes();
    let (marker_len, marker) = match *bytes.first()? {
        b'-' | b'*' | b'+' => (1, "•".to_string()),
        b'0'..=b'9' => {
            let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 9 || digits >= bytes.len() {
                return None;
            }
            if bytes[digits] != b'.' && bytes[digits] != b')' {
                return None;
            }
            (digits + 1, body[..digits + 1].to_string())
        }
        _ => return None,
    };
    let rest = &body[marker_len..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let content = rest.trim_start();
    for (prefix, task) in [("[ ]", "[ ]"), ("[x]", "[x]"), ("[X]", "[x]")] {
        if let Some(after) = content.strip_prefix(prefix) {
            if after.is_empty() || after.starts_with([' ', '\t']) {
                return Some((task.to_string(), after.trim_start()));
            }
        }
    }
    Some((marker, content))
}

/// Alignments when `lines[i]` is a table header followed by a delimiter row
/// with the same cell count.
fn table_start(lines: &[Line<'_>], i: usize) -> Option<Vec<Align>> {
    let (_, header) = split_indent(lines.get(i)?.text);
    if !header.contains('|') {
        return None;
    }
    let (_, delimiter) = split_indent(lines.get(i + 1)?.text);
    let align = delimiter_row(delimiter)?;
    (split_cells(header).len() == align.len()).then_some(align)
}

fn delimiter_row(body: &str) -> Option<Vec<Align>> {
    if !body.contains('|') {
        return None;
    }
    let cells = split_cells(body);
    if cells.is_empty() {
        return None;
    }
    cells
        .iter()
        .map(|cell| {
            let left = cell.starts_with(':');
            let right = cell.ends_with(':');
            let dashes = cell.trim_start_matches(':').trim_end_matches(':');
            if dashes.is_empty() || !dashes.bytes().all(|b| b == b'-') {
                return None;
            }
            Some(match (left, right) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            })
        })
        .collect()
}

/// Cells of a table line split on unescaped pipes, trimmed, outer pipes dropped.
fn split_cells(body: &str) -> Vec<String> {
    let trimmed = body.trim();
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '|' => cells.push(std::mem::take(&mut cell).trim().to_string()),
            _ => cell.push(c),
        }
    }
    if !(inner.ends_with('|') && !inner.ends_with("\\|")) {
        cells.push(cell.trim().to_string());
    }
    cells
}

/// Style in effect while scanning inline text.
#[derive(Debug, Clone, Default)]
struct Style {
    bold: bool,
    italic: bool,
    code: bool,
    strike: bool,
    link: Option<String>,
}

impl Style {
    fn run(&self, text: String) -> Run {
        Run {
            text,
            bold: self.bold,
            italic: self.italic,
            code: self.code,
            strike: self.strike,
            link: self.link.clone(),
        }
    }

    fn matches(&self, run: &Run) -> bool {
        run.bold == self.bold
            && run.italic == self.italic
            && run.code == self.code
            && run.strike == self.strike
            && run.link == self.link
    }
}

/// Parse inline marks into styled runs. Adjacent runs with one style merge.
fn inline(text: &str) -> Vec<Run> {
    let mut runs = Vec::new();
    inline_into(text, &Style::default(), &mut runs);
    runs
}

fn push_text(runs: &mut Vec<Run>, style: &Style, text: &str) {
    if text.is_empty() {
        return;
    }
    match runs.last_mut() {
        Some(last) if style.matches(last) => last.text.push_str(text),
        _ => runs.push(style.run(text.to_string())),
    }
}

fn inline_into(s: &str, style: &Style, runs: &mut Vec<Run>) {
    let bytes = s.as_bytes();
    let mut literal = String::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if bytes.get(i + 1).is_some_and(u8::is_ascii_punctuation) => {
                literal.push(bytes[i + 1] as char);
                i += 2;
            }
            b'`' => {
                let n = run_len(bytes, i, b'`');
                match find_backticks(bytes, i + n, n) {
                    Some(close) => {
                        push_text(runs, style, &literal);
                        literal.clear();
                        let code = Style {
                            code: true,
                            ..style.clone()
                        };
                        push_text(runs, &code, code_span(&s[i + n..close]));
                        i = close + n;
                    }
                    None => {
                        literal.push_str(&s[i..i + n]);
                        i += n;
                    }
                }
            }
            mark @ (b'*' | b'_' | b'~') => match emphasis(s, i, mark) {
                Some((n, close)) => {
                    push_text(runs, style, &literal);
                    literal.clear();
                    let mut inner = style.clone();
                    match (mark, n) {
                        (b'~', _) => inner.strike = true,
                        (_, 1) => inner.italic = true,
                        (_, 2) => inner.bold = true,
                        _ => {
                            inner.bold = true;
                            inner.italic = true;
                        }
                    }
                    inline_into(&s[i + n..close], &inner, runs);
                    i = close + n;
                }
                None => {
                    let n = run_len(bytes, i, mark);
                    literal.push_str(&s[i..i + n]);
                    i += n;
                }
            },
            b'[' => match link(s, i) {
                Some((text_end, url_end)) => {
                    push_text(runs, style, &literal);
                    literal.clear();
                    let linked = Style {
                        link: Some(s[text_end + 2..url_end].to_string()),
                        ..style.clone()
                    };
                    inline_into(&s[i + 1..text_end], &linked, runs);
                    i = url_end + 1;
                }
                None => {
                    literal.push('[');
                    i += 1;
                }
            },
            b'<' => match angle_autolink(s, i) {
                Some(end) => {
                    push_text(runs, style, &literal);
                    literal.clear();
                    push_autolink(runs, style, &s[i + 1..end]);
                    i = end + 1;
                }
                None => {
                    literal.push('<');
                    i += 1;
                }
            },
            b'h' if style.link.is_none() && bare_autolink_start(s, i) => {
                let end = url_end(s, i);
                push_text(runs, style, &literal);
                literal.clear();
                push_autolink(runs, style, &s[i..end]);
                i = end;
            }
            _ => {
                let c = s[i..].chars().next().unwrap_or('\u{FFFD}');
                literal.push(c);
                i += c.len_utf8().max(1);
            }
        }
    }
    push_text(runs, style, &literal);
}

fn push_autolink(runs: &mut Vec<Run>, style: &Style, url: &str) {
    let linked = Style {
        link: Some(url.to_string()),
        ..style.clone()
    };
    push_text(runs, &linked, url);
}

fn run_len(bytes: &[u8], at: usize, mark: u8) -> usize {
    bytes[at..].iter().take_while(|&&b| b == mark).count()
}

/// Start of the next backtick run of exactly `n`, skipping other runs.
fn find_backticks(bytes: &[u8], from: usize, n: usize) -> Option<usize> {
    let mut j = from;
    while j < bytes.len() {
        if bytes[j] == b'`' {
            let r = run_len(bytes, j, b'`');
            if r == n {
                return Some(j);
            }
            j += r;
        } else {
            j += 1;
        }
    }
    None
}

/// Code span content: one space off each end when both are present and the
/// span is not all spaces.
fn code_span(inner: &str) -> &str {
    if inner.len() >= 2
        && inner.starts_with(' ')
        && inner.ends_with(' ')
        && !inner.bytes().all(|b| b == b' ')
    {
        &inner[1..inner.len() - 1]
    } else {
        inner
    }
}

fn char_before(s: &str, at: usize) -> Option<char> {
    s[..at].chars().next_back()
}

fn char_at(s: &str, at: usize) -> Option<char> {
    s[at..].chars().next()
}

fn is_blank(c: Option<char>) -> bool {
    c.is_none_or(char::is_whitespace)
}

fn is_word(c: Option<char>) -> bool {
    c.is_some_and(char::is_alphanumeric)
}

/// Opener lengths to try for a run of `r` marks, longest first.
fn opener_lengths(mark: u8, r: usize) -> &'static [usize] {
    match (mark, r) {
        (b'~', r) if r >= 2 => &[2],
        (b'~', _) => &[],
        (_, 1) => &[1],
        (_, 2) => &[2, 1],
        _ => &[3, 2, 1],
    }
}

/// Opener length and closer start for an emphasis run at `i`, when it opens
/// a span that closes. The whole opener run is skipped before scanning, so
/// its leftover marks belong to the inner text.
fn emphasis(s: &str, i: usize, mark: u8) -> Option<(usize, usize)> {
    let bytes = s.as_bytes();
    let r = run_len(bytes, i, mark);
    if is_blank(char_at(s, i + r)) {
        return None;
    }
    if mark == b'_' && is_word(char_before(s, i)) {
        return None;
    }
    opener_lengths(mark, r)
        .iter()
        .find_map(|&n| find_closer(s, i + r, mark, n).map(|close| (n, close)))
}

/// Start of the closing run of `n` marks for an opener whose text begins at
/// `from`. Escapes, code spans, and nested openers of the same mark are
/// skipped. A longer closing run matches on its last `n` marks.
fn find_closer(s: &str, from: usize, mark: u8, n: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut j = from;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'`' => {
                let r = run_len(bytes, j, b'`');
                j = match find_backticks(bytes, j + r, r) {
                    Some(close) => close + r,
                    None => j + r,
                };
            }
            b if b == mark => {
                let r = run_len(bytes, j, mark);
                let can_close = j > from
                    && !is_blank(char_before(s, j))
                    && (mark != b'_' || !is_word(char_at(s, j + r)));
                if can_close && r >= n {
                    return Some(j + r - n);
                }
                let can_open =
                    !is_blank(char_at(s, j + r)) && (mark != b'_' || !is_word(char_before(s, j)));
                let nested = if can_open {
                    opener_lengths(mark, r)
                        .iter()
                        .find_map(|&m| find_closer(s, j + r, mark, m).map(|close| close + m))
                } else {
                    None
                };
                j = nested.unwrap_or(j + r);
            }
            _ => j += 1,
        }
    }
    None
}

/// `]` and `)` positions of a `[text](url)` link opening at `i`. The
/// destination may not contain blanks; titles are not supported.
fn link(s: &str, i: usize) -> Option<(usize, usize)> {
    let bytes = s.as_bytes();
    let mut depth = 0;
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'`' => {
                let r = run_len(bytes, j, b'`');
                j = match find_backticks(bytes, j + r, r) {
                    Some(close) => close + r,
                    None => j + r,
                };
            }
            b'[' => {
                depth += 1;
                j += 1;
            }
            b']' if depth == 0 => break,
            b']' => {
                depth -= 1;
                j += 1;
            }
            _ => j += 1,
        }
    }
    if bytes.get(j) != Some(&b']') || bytes.get(j + 1) != Some(&b'(') {
        return None;
    }
    let mut parens = 0;
    let mut k = j + 2;
    while k < bytes.len() {
        match bytes[k] {
            b'\\' => k += 2,
            b'(' => {
                parens += 1;
                k += 1;
            }
            b')' if parens == 0 => return Some((j, k)),
            b')' => {
                parens -= 1;
                k += 1;
            }
            b if b.is_ascii_whitespace() || b == b'<' => return None,
            _ => k += 1,
        }
    }
    None
}

fn scheme_len(s: &str, i: usize) -> Option<usize> {
    ["https://", "http://"]
        .iter()
        .find(|scheme| s[i..].starts_with(*scheme))
        .map(|scheme| scheme.len())
}

/// Position of the closing `>` of an `<http…>` autolink opening at `i`.
fn angle_autolink(s: &str, i: usize) -> Option<usize> {
    let scheme = scheme_len(s, i + 1)?;
    let bytes = s.as_bytes();
    let mut j = i + 1 + scheme;
    while j < bytes.len() {
        match bytes[j] {
            b'>' => return (j > i + 1 + scheme).then_some(j),
            b'<' => return None,
            b if b.is_ascii_whitespace() => return None,
            _ => j += 1,
        }
    }
    None
}

fn bare_autolink_start(s: &str, i: usize) -> bool {
    scheme_len(s, i)
        .is_some_and(|scheme| !is_word(char_before(s, i)) && !is_blank(char_at(s, i + scheme)))
}

/// End of a bare URL starting at `i`: up to the first blank or `<`, with
/// trailing punctuation and unbalanced closing brackets left out.
fn url_end(s: &str, i: usize) -> usize {
    let bytes = s.as_bytes();
    let mut end = i;
    while end < bytes.len() && !bytes[end].is_ascii_whitespace() && bytes[end] != b'<' {
        end += 1;
    }
    loop {
        let url = &s[i..end];
        let Some(last) = url.chars().next_back() else {
            break;
        };
        let unbalanced = |open: char, close: char| {
            last == close && url.matches(open).count() < url.matches(close).count()
        };
        if matches!(
            last,
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '*' | '_' | '~'
        ) || unbalanced('(', ')')
            || unbalanced('[', ']')
            || unbalanced('{', '}')
        {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Run {
        Run {
            text: text.to_string(),
            ..Run::default()
        }
    }

    fn bold(text: &str) -> Run {
        Run {
            bold: true,
            ..plain(text)
        }
    }

    fn italic(text: &str) -> Run {
        Run {
            italic: true,
            ..plain(text)
        }
    }

    fn code(text: &str) -> Run {
        Run {
            code: true,
            ..plain(text)
        }
    }

    fn blocks(text: &str) -> Vec<Block> {
        parse(text).into_iter().map(|p| p.block).collect()
    }

    fn paragraph(runs: Vec<Run>) -> Block {
        Block::Paragraph { runs }
    }

    #[test]
    fn empty_input_has_no_blocks() {
        assert!(parse("").is_empty());
        assert_eq!(last_block_start(""), 0);
    }

    #[test]
    fn paragraph_keeps_line_breaks() {
        assert_eq!(
            blocks("one\ntwo\nthree"),
            vec![paragraph(vec![plain("one\ntwo\nthree")])]
        );
    }

    #[test]
    fn hard_breaks_from_trailing_spaces_or_backslash() {
        assert_eq!(
            blocks("one  \ntwo\\\nthree"),
            vec![paragraph(vec![plain("one\ntwo\nthree")])]
        );
    }

    #[test]
    fn headings_with_levels_and_closing_hashes() {
        assert_eq!(
            blocks("# Title\n### Sub ##\n####### not"),
            vec![
                Block::Heading {
                    level: 1,
                    runs: vec![plain("Title")]
                },
                Block::Heading {
                    level: 3,
                    runs: vec![plain("Sub")]
                },
                paragraph(vec![plain("####### not")]),
            ]
        );
        assert_eq!(blocks("#hashtag"), vec![paragraph(vec![plain("#hashtag")])]);
    }

    #[test]
    fn rules_of_three_marks() {
        assert_eq!(
            blocks("---\n* * *\n___\n--"),
            vec![
                Block::Rule,
                Block::Rule,
                Block::Rule,
                paragraph(vec![plain("--")])
            ]
        );
    }

    #[test]
    fn fenced_code_keeps_lines_verbatim() {
        let text = "```rust\nfn main() {}\n    indented\n```\nafter";
        assert_eq!(
            blocks(text),
            vec![
                Block::Code {
                    lang: Some("rust".to_string()),
                    lines: vec!["fn main() {}".to_string(), "    indented".to_string()],
                    closed: true,
                },
                paragraph(vec![plain("after")]),
            ]
        );
    }

    #[test]
    fn unclosed_fence_is_open_code_block() {
        assert_eq!(
            blocks("~~~\n**not bold**\n"),
            vec![Block::Code {
                lang: None,
                lines: vec!["**not bold**".to_string()],
                closed: false,
            }]
        );
        assert_eq!(last_block_start("~~~\n**not bold**\n"), 0);
    }

    #[test]
    fn fence_inside_nested_list_item_strips_its_indent() {
        let text = "- item\n    ```sh\n    echo hi\n    ```\n";
        assert_eq!(
            blocks(text),
            vec![
                Block::ListItem {
                    depth: 0,
                    marker: "•".to_string(),
                    runs: vec![plain("item")],
                },
                Block::Code {
                    lang: Some("sh".to_string()),
                    lines: vec!["echo hi".to_string()],
                    closed: true,
                },
            ]
        );
    }

    #[test]
    fn tilde_fence_ignores_backtick_fence_and_vice_versa() {
        assert_eq!(
            blocks("```\n~~~\n```"),
            vec![Block::Code {
                lang: None,
                lines: vec!["~~~".to_string()],
                closed: true,
            }]
        );
    }

    #[test]
    fn bullet_and_ordered_lists_with_nesting() {
        let text = "- a\n  - b\n    1. c\n- d\n3) e";
        assert_eq!(
            blocks(text),
            vec![
                Block::ListItem {
                    depth: 0,
                    marker: "•".to_string(),
                    runs: vec![plain("a")]
                },
                Block::ListItem {
                    depth: 1,
                    marker: "•".to_string(),
                    runs: vec![plain("b")]
                },
                Block::ListItem {
                    depth: 2,
                    marker: "1.".to_string(),
                    runs: vec![plain("c")]
                },
                Block::ListItem {
                    depth: 0,
                    marker: "•".to_string(),
                    runs: vec![plain("d")]
                },
                Block::ListItem {
                    depth: 0,
                    marker: "3)".to_string(),
                    runs: vec![plain("e")]
                },
            ]
        );
    }

    #[test]
    fn task_items_and_continuation_lines() {
        let text = "- [ ] todo\n  more\n* [X] done";
        assert_eq!(
            blocks(text),
            vec![
                Block::ListItem {
                    depth: 0,
                    marker: "[ ]".to_string(),
                    runs: vec![plain("todo\nmore")]
                },
                Block::ListItem {
                    depth: 0,
                    marker: "[x]".to_string(),
                    runs: vec![plain("done")]
                },
            ]
        );
    }

    #[test]
    fn list_depth_resets_after_other_blocks() {
        let text = "  - deep\n\ntext\n- top";
        let depths: Vec<u8> = blocks(text)
            .into_iter()
            .filter_map(|b| match b {
                Block::ListItem { depth, .. } => Some(depth),
                _ => None,
            })
            .collect();
        assert_eq!(depths, vec![0, 0]);
    }

    #[test]
    fn quotes_merge_lines_and_parse_inline() {
        assert_eq!(
            blocks("> **a**\n> b\n>\n> c"),
            vec![Block::Quote {
                runs: vec![bold("a"), plain("\nb\nc")]
            }]
        );
    }

    #[test]
    fn pipe_table_with_alignment_and_escaped_pipe() {
        let text = "| h1 | h2 | h3 |\n|:---|:--:|---:|\n| a | `b` | c \\| d |\nshort";
        assert_eq!(
            blocks(text),
            vec![Block::Table {
                header: vec![vec![plain("h1")], vec![plain("h2")], vec![plain("h3")]],
                align: vec![Align::Left, Align::Center, Align::Right],
                rows: vec![
                    vec![vec![plain("a")], vec![code("b")], vec![plain("c | d")]],
                    vec![vec![plain("short")], vec![], vec![]],
                ],
            }]
        );
    }

    #[test]
    fn header_without_delimiter_row_is_a_paragraph() {
        assert_eq!(
            blocks("| a | b |\n| c | d |"),
            vec![paragraph(vec![plain("| a | b |\n| c | d |")])]
        );
        assert_eq!(
            blocks("| a | b |\n|---|"),
            vec![paragraph(vec![plain("| a | b |\n|---|")])]
        );
    }

    #[test]
    fn table_interrupts_a_paragraph() {
        let parsed = parse("intro\n| a |\n|---|\n| 1 |\n\nend");
        assert_eq!(parsed[0].block, paragraph(vec![plain("intro")]));
        assert!(matches!(parsed[1].block, Block::Table { .. }));
        assert_eq!(parsed[1].source, 6..24);
        assert_eq!(parsed[2].block, Block::Blank);
        assert_eq!(parsed[3].block, paragraph(vec![plain("end")]));
    }

    #[test]
    fn inline_emphasis_kinds() {
        assert_eq!(
            inline("**b** __b__ *i* _i_ ~~s~~ ***bi***"),
            vec![
                bold("b"),
                plain(" "),
                bold("b"),
                plain(" "),
                italic("i"),
                plain(" "),
                italic("i"),
                plain(" "),
                Run {
                    strike: true,
                    ..plain("s")
                },
                plain(" "),
                Run {
                    bold: true,
                    italic: true,
                    ..plain("bi")
                },
            ]
        );
    }

    #[test]
    fn nested_emphasis() {
        assert_eq!(
            inline("*a **b** c*"),
            vec![
                italic("a "),
                Run {
                    bold: true,
                    ..italic("b")
                },
                italic(" c"),
            ]
        );
        assert_eq!(
            inline("**a *b* c**"),
            vec![
                bold("a "),
                Run {
                    italic: true,
                    ..bold("b")
                },
                bold(" c"),
            ]
        );
    }

    #[test]
    fn unterminated_or_misplaced_markers_stay_literal() {
        assert_eq!(inline("**foo"), vec![plain("**foo")]);
        assert_eq!(inline("a * b * c"), vec![plain("a * b * c")]);
        assert_eq!(inline("snake_case_name"), vec![plain("snake_case_name")]);
        assert_eq!(
            inline("2 * 3 = 6 and 4 * 5"),
            vec![plain("2 * 3 = 6 and 4 * 5")]
        );
        assert_eq!(inline("~tilde~"), vec![plain("~tilde~")]);
    }

    #[test]
    fn code_spans_of_any_backtick_length_protect_markers() {
        assert_eq!(
            inline("use `**x**` and `` a`b `` and ```c```"),
            vec![
                plain("use "),
                code("**x**"),
                plain(" and "),
                code("a`b"),
                plain(" and "),
                code("c"),
            ]
        );
        assert_eq!(inline("`unclosed"), vec![plain("`unclosed")]);
        assert_eq!(
            inline("*a `*` b*"),
            vec![
                italic("a "),
                Run {
                    code: true,
                    ..italic("*")
                },
                italic(" b")
            ]
        );
    }

    #[test]
    fn links_and_autolinks() {
        assert_eq!(
            inline("[**docs**](https://x.io/a_(b)) <https://y.io> https://z.io/p). plain"),
            vec![
                Run {
                    bold: true,
                    link: Some("https://x.io/a_(b)".to_string()),
                    ..plain("docs")
                },
                plain(" "),
                Run {
                    link: Some("https://y.io".to_string()),
                    ..plain("https://y.io")
                },
                plain(" "),
                Run {
                    link: Some("https://z.io/p".to_string()),
                    ..plain("https://z.io/p")
                },
                plain("). plain"),
            ]
        );
        assert_eq!(inline("[x](a b)"), vec![plain("[x](a b)")]);
        assert_eq!(inline("[x] (y)"), vec![plain("[x] (y)")]);
        assert_eq!(inline("<notalink>"), vec![plain("<notalink>")]);
    }

    #[test]
    fn backslash_escapes_and_non_ascii() {
        assert_eq!(
            inline("\\*not\\* 2\\3 ü→*é*"),
            vec![plain("*not* 2\\3 ü→"), italic("é")]
        );
    }

    #[test]
    fn source_ranges_cover_every_byte() {
        let text = "# t\r\n\r\npara\n- a\n\n```\nx\n```\n> q\n| a |\n|-|\n";
        let parsed = parse(text);
        let mut cursor = 0;
        for p in &parsed {
            assert_eq!(p.source.start, cursor, "gap before {:?}", p.block);
            cursor = p.source.end;
        }
        assert_eq!(cursor, text.len());
        assert_eq!(
            blocks("# t\r\npara\r\n")[0],
            Block::Heading {
                level: 1,
                runs: vec![plain("t")]
            }
        );
    }

    #[test]
    fn last_block_start_points_at_growable_block() {
        assert_eq!(last_block_start("one\ntwo"), 0);
        assert_eq!(last_block_start("one\n\ntwo"), 5);
        assert_eq!(last_block_start("# h\n"), 4);
        assert_eq!(last_block_start("# h"), 0);
        assert_eq!(last_block_start("a\n\n"), 3);
        assert_eq!(last_block_start("a\n\n  "), 3);
        assert_eq!(last_block_start("x\n- a\n- b\n"), 6);
        assert_eq!(last_block_start("```\n```\n"), 8);
        assert_eq!(last_block_start("```\n```"), 0);
    }

    #[test]
    fn streaming_prefixes_agree_with_the_full_parse() {
        let full = "# Title\n\nSome *text* with `code` and a [link](https://a.b/c).\nContinued line  \nhard break\n\n\
                    - item one\n  - nested **bold**\n    ```rust\n    let x = 1;\n    ```\n  - [x] done\n2. ordered\n\n\
                    > quoted\n> lines\n\n| h | k |\n|---|:-:|\n| 1 | 2 |\n| 3 | 4 |\n\n---\n\n~~~\nopen fence\nstill open\n~~~\n\
                    intro\n| a |\n|---|\nfinal paragraph ***bi*** ~~s~~ ü→é\n";
        let whole = parse(full);
        for end in 0..=full.len() {
            if !full.is_char_boundary(end) {
                continue;
            }
            let prefix = &full[..end];
            let parsed = parse(prefix);
            let stable = last_block_start(prefix);
            let settled: Vec<&Parsed> = parsed.iter().filter(|p| p.source.start < stable).collect();
            for (k, block) in settled.iter().enumerate() {
                assert_eq!(
                    Some(*block),
                    whole.get(k),
                    "prefix of {end} bytes: block {k} differs"
                );
            }
            assert!(stable <= end);
        }
    }

    #[test]
    fn streaming_partial_fence_and_markers_are_literal() {
        assert_eq!(blocks("``"), vec![paragraph(vec![plain("``")])]);
        assert_eq!(blocks("**bo"), vec![paragraph(vec![plain("**bo")])]);
        assert_eq!(
            blocks("[link](https://"),
            vec![paragraph(vec![plain("[link](https://")])]
        );
        assert_eq!(
            blocks("- "),
            vec![Block::ListItem {
                depth: 0,
                marker: "•".to_string(),
                runs: vec![]
            }]
        );
        assert_eq!(blocks("| a |"), vec![paragraph(vec![plain("| a |")])]);
    }

    #[test]
    fn list_item_does_not_swallow_following_block_starts() {
        assert_eq!(
            blocks("- a\n# h\n- b\n> q"),
            vec![
                Block::ListItem {
                    depth: 0,
                    marker: "•".to_string(),
                    runs: vec![plain("a")]
                },
                Block::Heading {
                    level: 1,
                    runs: vec![plain("h")]
                },
                Block::ListItem {
                    depth: 0,
                    marker: "•".to_string(),
                    runs: vec![plain("b")]
                },
                Block::Quote {
                    runs: vec![plain("q")]
                },
            ]
        );
    }

    #[test]
    fn indented_code_is_a_paragraph() {
        assert_eq!(
            blocks("    not code"),
            vec![paragraph(vec![plain("not code")])]
        );
    }
}
