//! From a `ChatMessage` to styled paragraphs: markdown for assistant text,
//! a header row plus diff or stream blocks for tool results, plain text with
//! link targets for everything else. Pure: the only outside input is the
//! `exists` probe that decides whether a file reference is clickable.

use std::rc::Rc;

use ratatui_core::style::Style;
use unicode_width::UnicodeWidthStr;

use super::render::{RowSpan, Styled, Target};
use super::{diff, highlight, links, markdown, theme, ChatMessage, ToolPayload, TranscriptRole};
use crate::chat::backend::tools::{self, ToolDetail};

/// Diff and stream bodies stop here; the model-facing content is unaffected.
const MAX_BODY_ROWS: usize = 200;
/// Stream previews under a command header.
const MAX_STREAM_ROWS: usize = 40;
const MAX_LINE_CHARS: usize = 400;

pub(crate) struct Options<'a> {
    pub(crate) markdown: bool,
    /// Whether a relative path names a real file in the workspace.
    pub(crate) exists: &'a dyn Fn(&str) -> bool,
}

#[cfg(test)]
impl Options<'static> {
    /// No markdown, no file targets: what layout tests use.
    pub(crate) const PLAIN: Options<'static> = Options {
        markdown: false,
        exists: &|_| false,
    };
}

/// The paragraphs of one message, in order. Empty for tool-only turns.
pub(crate) fn paragraphs(message: &ChatMessage, options: &Options, width: usize) -> Vec<Styled> {
    if message.text.is_empty() && message.tool.is_some() && message.detail.is_none() {
        return Vec::new();
    }
    match message.role {
        TranscriptRole::Tool { exit_code } => tool_result(message, exit_code, options),
        TranscriptRole::Assistant if options.markdown => {
            let mut out = Vec::new();
            markdown_blocks(&message.text, &mut out, options, width);
            out
        }
        TranscriptRole::Notice if crate::chat::header::is_header(&message.text) => message
            .text
            .split('\n')
            .enumerate()
            .map(|(index, line)| {
                let mut styled = Styled::default();
                match crate::chat::header::WORDMARK.get(index) {
                    Some(mark) if line.starts_with(mark) => {
                        styled.push(mark, theme::heading());
                        styled.push(&line[mark.len()..], theme::dim());
                    }
                    _ => styled.push(line, theme::dim()),
                }
                annotate_links(&mut styled, options, false);
                styled
            })
            .collect(),
        TranscriptRole::Notice if message.elapsed.is_some() && message.tool.is_none() => {
            // A turn footer: dim, right-aligned.
            let pad = width.saturating_sub(message.text.chars().count());
            let mut styled = Styled::default();
            styled.push(&" ".repeat(pad), Style::default());
            styled.push(&message.text, theme::dim());
            vec![styled]
        }
        _ => message
            .text
            .split('\n')
            .map(|line| {
                let mut styled = Styled::plain(line);
                annotate_links(&mut styled, options, true);
                styled
            })
            .collect(),
    }
}

/// Add URL and file-reference targets to ranges of `styled.text` that are
/// not already inside a span (code spans and existing links are skipped).
fn annotate_links(styled: &mut Styled, options: &Options, files: bool) {
    let covered: Vec<(usize, usize)> = styled.spans.iter().map(|s| (s.start, s.end)).collect();
    let free = |start: usize, end: usize| !covered.iter().any(|(s, e)| start < *e && *s < end);
    let text = styled.text.clone();
    for range in links::urls(&text) {
        if free(range.start, range.end) {
            styled.spans.push(RowSpan {
                start: range.start,
                end: range.end,
                style: theme::link(),
            });
            styled.targets.push(super::render::RowTarget {
                start: range.start,
                end: range.end,
                target: Target::Url(text[range].to_string()),
            });
        }
    }
    if files {
        for file in links::file_refs(&text) {
            let (start, end) = (file.range.start, file.range.end);
            if free(start, end)
                && !styled
                    .targets
                    .iter()
                    .any(|t| start < t.end && t.start < end)
                && (options.exists)(&file.path)
            {
                styled.spans.push(RowSpan {
                    start,
                    end,
                    style: theme::file_ref(),
                });
                styled.targets.push(super::render::RowTarget {
                    start,
                    end,
                    target: Target::File {
                        path: file.path,
                        line: file.line,
                        column: file.column,
                    },
                });
            }
        }
    }
    styled.spans.sort_by_key(|s| s.start);
    styled.targets.sort_by_key(|t| t.start);
}

// --- markdown ---------------------------------------------------------------

fn markdown_blocks(text: &str, out: &mut Vec<Styled>, options: &Options, width: usize) {
    for parsed in markdown::parse(text) {
        let source: Rc<str> = Rc::from(&text[parsed.source.clone()]);
        // Code is shown as written: no link or file scanning inside it.
        let is_code = matches!(parsed.block, markdown::Block::Code { .. });
        let mut block_rows = Vec::new();
        match parsed.block {
            markdown::Block::Blank => block_rows.push(Styled::default()),
            markdown::Block::Heading { level, runs } => {
                let mut styled = Styled::default();
                let mut level_marks = String::new();
                if level >= 3 {
                    level_marks.push_str(&"#".repeat(level as usize));
                    level_marks.push(' ');
                }
                styled.push(&level_marks, theme::heading());
                push_runs(&mut styled, &runs, theme::heading());
                block_rows.push(styled);
            }
            markdown::Block::Paragraph { runs } => {
                let mut styled = Styled::default();
                push_runs(&mut styled, &runs, Style::default());
                block_rows.push(styled);
            }
            markdown::Block::Quote { runs } => {
                let mut styled = Styled::default();
                styled.push("▎ ", theme::quote_bar());
                styled.hang = 2;
                push_runs(&mut styled, &runs, theme::italic());
                block_rows.push(styled);
            }
            markdown::Block::ListItem {
                depth,
                marker,
                runs,
            } => {
                let mut styled = Styled::default();
                styled.push(&"  ".repeat(depth as usize), Style::default());
                styled.push(&marker, theme::list_marker());
                styled.push(" ", Style::default());
                // Wrapped text lines up under the item's text, not its marker.
                styled.hang = UnicodeWidthStr::width(styled.text.as_str());
                push_runs(&mut styled, &runs, Style::default());
                block_rows.push(styled);
            }
            markdown::Block::Rule => {
                let mut styled = Styled::default();
                styled.push(&"─".repeat(width.clamp(1, 200)), theme::rule());
                block_rows.push(styled);
            }
            markdown::Block::Code {
                lang,
                lines,
                closed: _,
            } => code_block(lang.as_deref(), &lines, &mut block_rows),
            markdown::Block::Table {
                header,
                align,
                rows,
            } => table(&header, &align, &rows, width, &mut block_rows),
        }
        for mut styled in block_rows {
            styled.source = Some(source.clone());
            if !is_code {
                annotate_links(&mut styled, options, true);
            }
            out.push(styled);
        }
    }
}

fn push_runs(styled: &mut Styled, runs: &[markdown::Run], base: Style) {
    for run in runs {
        let mut style = base;
        if run.bold {
            style = style.patch(theme::bold());
        }
        if run.italic {
            style = style.patch(theme::italic());
        }
        if run.strike {
            style = style.patch(theme::strike());
        }
        if run.code {
            style = style.patch(theme::inline_code());
        }
        match &run.link {
            Some(url) => styled.push_target(
                &run.text,
                style.patch(theme::link()),
                Target::Url(url.clone()),
            ),
            None => styled.push(&run.text, style),
        }
    }
}

fn code_block(lang: Option<&str>, lines: &[String], out: &mut Vec<Styled>) {
    let body: Rc<str> = Rc::from(lines.join("\n"));
    let mut header = Styled {
        style: Some(theme::code_block()),
        ..Styled::default()
    };
    header.push(lang.unwrap_or("code"), theme::code_header());
    header.push("  ", Style::default());
    header.push_target("copy", theme::code_header(), Target::Copy(body));
    out.push(header);
    let language = lang.and_then(highlight::language);
    for line in lines.iter().take(MAX_BODY_ROWS) {
        let mut styled = Styled {
            style: Some(theme::code_block()),
            ..Styled::default()
        };
        push_highlighted(&mut styled, language, &expand_tabs(line));
        out.push(styled);
    }
    if lines.len() > MAX_BODY_ROWS {
        let mut styled = Styled {
            style: Some(theme::code_block()),
            ..Styled::default()
        };
        styled.push(
            &format!("… {} more lines", lines.len() - MAX_BODY_ROWS),
            theme::dim(),
        );
        out.push(styled);
    }
}

fn push_highlighted(styled: &mut Styled, language: Option<highlight::Language>, line: &str) {
    match language {
        Some(language) => {
            for (range, token) in highlight::tokens(language, line) {
                styled.push(&line[range], theme::token(token));
            }
        }
        None => styled.push(line, Style::default()),
    }
}

fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return clip(line);
    }
    let mut out = String::new();
    let mut column = 0;
    for ch in line.chars() {
        if ch == '\t' {
            let spaces = 4 - column % 4;
            out.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
        } else {
            out.push(ch);
            column += 1;
        }
    }
    clip(&out)
}

fn clip(line: &str) -> String {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

fn table(
    header: &[Vec<markdown::Run>],
    align: &[markdown::Align],
    rows: &[Vec<Vec<markdown::Run>>],
    width: usize,
    out: &mut Vec<Styled>,
) {
    let cell_text =
        |runs: &[markdown::Run]| runs.iter().map(|r| r.text.as_str()).collect::<String>();
    let columns = header
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    let mut widths = vec![0; columns];
    for row in std::iter::once(header).chain(rows.iter().map(Vec::as_slice)) {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell_text(cell).width());
        }
    }
    let total: usize = widths.iter().sum::<usize>() + 3 * columns.saturating_sub(1);
    if total > width {
        // Too wide to align: pipe rows as text so nothing is lost.
        for row in std::iter::once(header).chain(rows.iter().map(Vec::as_slice)) {
            let mut styled = Styled::default();
            for (i, cell) in row.iter().enumerate() {
                if i > 0 {
                    styled.push(" | ", theme::dim());
                }
                push_runs(&mut styled, cell, Style::default());
            }
            out.push(styled);
        }
        return;
    }
    let emit = |row: &[Vec<markdown::Run>], base: Style| -> Styled {
        let mut styled = Styled::default();
        for (i, column_width) in widths.iter().enumerate() {
            if i > 0 {
                styled.push(" │ ", theme::dim());
            }
            let cell = row.get(i).map(Vec::as_slice).unwrap_or(&[]);
            let text_width = cell_text(cell).width();
            let pad = column_width.saturating_sub(text_width);
            let (left, right) = match align.get(i) {
                Some(markdown::Align::Right) => (pad, 0),
                Some(markdown::Align::Center) => (pad / 2, pad - pad / 2),
                _ => (0, pad),
            };
            styled.push(&" ".repeat(left), Style::default());
            push_runs(&mut styled, cell, base);
            styled.push(&" ".repeat(right), Style::default());
        }
        styled
    };
    out.push(emit(header, theme::bold()));
    let mut rule = Styled::default();
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            rule.push("─┼─", theme::dim());
        }
        rule.push(&"─".repeat(*w), theme::dim());
    }
    out.push(rule);
    for row in rows {
        out.push(emit(row, Style::default()));
    }
}

// --- tool results -----------------------------------------------------------

fn tool_result(message: &ChatMessage, exit_code: i32, options: &Options) -> Vec<Styled> {
    let name = match &message.tool {
        Some(ToolPayload::Result { name, .. }) => name.as_str(),
        _ => "",
    };
    let mut lines = message.text.split('\n');
    let first = lines.next().unwrap_or("");
    // The first line of `output` is `tool target` or `tool target: reason`.
    let (target, reason) = {
        let rest = first.strip_prefix(name).unwrap_or(first).trim_start();
        match rest.split_once(": ") {
            Some((target, reason)) => (target.to_string(), Some(reason.to_string())),
            None => (rest.to_string(), None),
        }
    };
    let mut out = Vec::new();
    let mut header = Styled::default();
    header.push(name, theme::tool_header());
    header.push(" ", Style::default());
    if (options.exists)(&target) && !target.is_empty() && target != "." {
        header.push_target(
            &target,
            theme::tool_header().patch(theme::file_ref()),
            Target::File {
                path: target.clone(),
                line: None,
                column: None,
            },
        );
    } else {
        header.push(&target, theme::tool_header());
    }
    let mut summary: Vec<String> = Vec::new();
    match &message.detail {
        Some(ToolDetail::Write { before, after }) => {
            let hunks = diff::unified(before.as_deref(), after, 3);
            let (added, removed) = hunks.iter().fold((0, 0), |(a, r), hunk| {
                hunk.lines.iter().fold((a, r), |(a, r), line| match line {
                    diff::HunkLine::Added { .. } => (a + 1, r),
                    diff::HunkLine::Removed { .. } => (a, r + 1),
                    diff::HunkLine::Context { .. } => (a, r),
                })
            });
            if before.is_none() {
                summary.push("created".into());
            }
            summary.push(format!("+{added} -{removed}"));
        }
        Some(ToolDetail::Command {
            argv, exit_code, ..
        }) => {
            header.push(" ", Style::default());
            header.push(&tools::render_argv(argv), Style::default());
            summary.push(match exit_code {
                Some(code) => format!("exit {code}"),
                None => "killed".into(),
            });
        }
        Some(ToolDetail::Search {
            pattern,
            literal,
            glob,
            matches,
        }) => {
            summary.push(if *literal {
                format!("\"{pattern}\"")
            } else {
                format!("/{pattern}/")
            });
            if let Some(glob) = glob {
                summary.push(format!("in {glob}"));
            }
            summary.push(format!(
                "{matches} match{}",
                if *matches == 1 { "" } else { "es" }
            ));
        }
        Some(ToolDetail::Find { pattern, matches }) => {
            summary.push(pattern.clone());
            summary.push(format!(
                "{matches} match{}",
                if *matches == 1 { "" } else { "es" }
            ));
        }
        Some(ToolDetail::Read {
            offset,
            limit,
            lines,
        }) => {
            let start = offset.unwrap_or(1);
            summary.push(match limit {
                Some(_) if *lines > 0 => format!("lines {start}–{}", start + lines - 1),
                _ => format!("{lines} lines"),
            });
        }
        None => {}
    }
    if let Some(reason) = &reason {
        summary.push(reason.clone());
    } else if exit_code == tools_exit_denied() {
        summary.push("denied".into());
    } else if exit_code != 0 && message.detail.is_none() {
        summary.push(format!("exit {exit_code}"));
    }
    if let Some(elapsed) = message.elapsed {
        summary.push(theme::duration(elapsed));
    }
    for part in summary {
        header.push(" · ", theme::dim());
        header.push(&part, theme::dim());
    }
    out.push(header);

    match &message.detail {
        Some(ToolDetail::Write { before, after }) => {
            diff_rows(before.as_deref(), after, &target, &mut out);
        }
        Some(ToolDetail::Command {
            stdout,
            stderr,
            stdout_cut,
            stderr_cut,
            exit_code,
            ..
        }) => {
            stream_rows("stdout", stdout, *stdout_cut, None, &mut out);
            let style = (exit_code != &Some(0)).then(theme::diff_removed);
            stream_rows("stderr", stderr, *stderr_cut, style, &mut out);
            if stdout.is_empty() && stderr.is_empty() {
                let mut styled = Styled::default();
                styled.push("(no output)", theme::dim());
                out.push(styled);
            }
        }
        Some(ToolDetail::Search { .. }) => {
            for line in lines {
                out.push(search_row(line, options));
            }
        }
        Some(ToolDetail::Read { offset, .. }) => {
            let language = target.rsplit('.').next().and_then(highlight::language);
            read_rows(lines, offset.unwrap_or(1), language, &mut out);
        }
        _ => {
            for line in lines {
                let mut styled = Styled::plain(line);
                annotate_links(&mut styled, options, true);
                out.push(styled);
            }
        }
    }
    out
}

/// A `read_file` body: every file line behind its line number, wrapped text
/// lined up under the code, and the tool's own bracketed or ellipsis notes
/// (`[lines 3-9 of 40]`, `… 5 more lines`) dim and unnumbered.
fn read_rows<'a>(
    lines: impl Iterator<Item = &'a str>,
    first: usize,
    language: Option<highlight::Language>,
    out: &mut Vec<Styled>,
) {
    let lines: Vec<&str> = lines.collect();
    let is_note = |index: usize, line: &str| {
        let bracketed = line.starts_with('[') && line.ends_with(']');
        (index == 0 && bracketed && line.starts_with("[lines "))
            || (index + 1 == lines.len()
                && (line.starts_with('…') || (bracketed && line.contains(" more lines;"))))
    };
    let body = lines
        .iter()
        .enumerate()
        .filter(|(index, line)| !is_note(*index, line))
        .count();
    let digits = (first + body.saturating_sub(1)).to_string().len();
    let mut number = first;
    for (index, line) in lines.iter().enumerate() {
        let mut styled = Styled::default();
        if is_note(index, line) {
            styled.push(&clip(line), theme::dim());
        } else {
            styled.push(&format!("{number:>digits$} "), theme::gutter());
            styled.hang = digits + 1;
            push_highlighted(&mut styled, language, &expand_tabs(line));
            number += 1;
        }
        out.push(styled);
    }
}

fn tools_exit_denied() -> i32 {
    126
}

/// `path:line:text` from `search_files`, with the prefix clickable.
fn search_row(line: &str, options: &Options) -> Styled {
    let mut styled = Styled::default();
    let mut parts = line.splitn(3, ':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(path), Some(number), Some(text))
            if number.chars().all(|c| c.is_ascii_digit())
                && !number.is_empty()
                && (options.exists)(path) =>
        {
            styled.push_target(
                &format!("{path}:{number}"),
                theme::file_ref(),
                Target::File {
                    path: path.to_string(),
                    line: number.parse().ok(),
                    column: None,
                },
            );
            styled.push(":", theme::dim());
            styled.push(&clip(text), Style::default());
        }
        _ => {
            styled.push(
                &clip(line),
                if line.starts_with('[') {
                    theme::dim()
                } else {
                    Style::default()
                },
            );
        }
    }
    styled
}

fn stream_rows(label: &str, text: &str, cut: bool, style: Option<Style>, out: &mut Vec<Styled>) {
    if text.is_empty() {
        return;
    }
    let mut head = Styled {
        style,
        ..Styled::default()
    };
    head.push(label, theme::stream_label());
    out.push(head);
    let lines: Vec<&str> = text.lines().collect();
    for line in lines.iter().take(MAX_STREAM_ROWS) {
        let mut styled = Styled {
            style,
            ..Styled::default()
        };
        styled.push("  ", Style::default());
        styled.push(&expand_tabs(line), Style::default());
        out.push(styled);
    }
    if lines.len() > MAX_STREAM_ROWS || cut {
        let mut styled = Styled {
            style,
            ..Styled::default()
        };
        let more = lines.len().saturating_sub(MAX_STREAM_ROWS);
        styled.push(
            &if cut {
                format!("  … {more} more lines shown to the model, output truncated")
            } else {
                format!("  … {more} more lines")
            },
            theme::dim(),
        );
        out.push(styled);
    }
}

fn diff_rows(before: Option<&str>, after: &str, path: &str, out: &mut Vec<Styled>) {
    let hunks = diff::unified(before, after, 3);
    let language = path.rsplit('.').next().and_then(highlight::language);
    let digits = hunks
        .iter()
        .map(|h| (h.old_start + h.old_count).max(h.new_start + h.new_count))
        .max()
        .unwrap_or(1)
        .to_string()
        .len();
    let gutter = |old: Option<usize>, new: Option<usize>| {
        let cell = |n: Option<usize>| match n {
            Some(n) => format!("{n:>digits$}"),
            None => " ".repeat(digits),
        };
        format!("{} {} ", cell(old), cell(new))
    };
    let mut emitted = 0;
    let mut total = 0;
    for hunk in &hunks {
        total += hunk.lines.len() + 1;
        if emitted >= MAX_BODY_ROWS {
            continue;
        }
        let mut head = Styled::default();
        head.push(
            &format!(
                "@@ -{},{} +{},{} @@",
                hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
            ),
            theme::diff_hunk(),
        );
        out.push(head);
        emitted += 1;
        for line in &hunk.lines {
            if emitted >= MAX_BODY_ROWS {
                break;
            }
            emitted += 1;
            let (row_style, marker, old, new, text, emphasis, strong) = match line {
                diff::HunkLine::Context { old, new, text } => (
                    None,
                    ' ',
                    Some(*old),
                    Some(*new),
                    text,
                    &Vec::new(),
                    Style::default(),
                ),
                diff::HunkLine::Removed {
                    old,
                    text,
                    emphasis,
                } => (
                    Some(theme::diff_removed()),
                    '-',
                    Some(*old),
                    None,
                    text,
                    emphasis,
                    theme::diff_removed_emphasis(),
                ),
                diff::HunkLine::Added {
                    new,
                    text,
                    emphasis,
                } => (
                    Some(theme::diff_added()),
                    '+',
                    None,
                    Some(*new),
                    text,
                    emphasis,
                    theme::diff_added_emphasis(),
                ),
            };
            let mut styled = Styled {
                style: row_style,
                ..Styled::default()
            };
            styled.push(&gutter(old, new), theme::gutter());
            styled.push(&marker.to_string(), Style::default());
            styled.hang = UnicodeWidthStr::width(styled.text.as_str());
            let content_start = styled.text.len();
            let source = text.trim_end_matches('\r');
            let text = expand_tabs(source);
            push_highlighted(&mut styled, language, &text);
            // Emphasis overrides the token color's background only; merge
            // by splitting spans is overkill: emphasised ranges are pushed
            // as extra spans that patch over, so they must come after.
            // The ranges index the line as written. Tab expansion and the
            // length clip rewrite it, so they apply only to the same bytes.
            for range in emphasis.iter().filter(|_| text == source) {
                if range.end <= text.len() {
                    styled.spans.push(RowSpan {
                        start: content_start + range.start,
                        end: content_start + range.end,
                        style: strong,
                    });
                }
            }
            out.push(styled);
        }
    }
    if total > emitted {
        let mut styled = Styled::default();
        styled.push(&format!("… {} more lines", total - emitted), theme::dim());
        out.push(styled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::backend::ToolCall;
    use crate::chat::transcript::render::RowTarget;
    use std::time::Duration;

    fn exists(path: &str) -> bool {
        matches!(path, "src/main.rs" | "Cargo.toml" | "docs/a.md")
    }

    fn options() -> Options<'static> {
        Options {
            markdown: true,
            exists: &exists,
        }
    }

    fn texts(paragraphs: &[Styled]) -> Vec<&str> {
        paragraphs.iter().map(|p| p.text.as_str()).collect()
    }

    fn targets(paragraph: &Styled) -> Vec<&Target> {
        paragraph.targets.iter().map(|t| &t.target).collect()
    }

    fn assistant(text: &str) -> ChatMessage {
        ChatMessage::new(TranscriptRole::Assistant, text)
    }

    #[test]
    fn markdown_renders_headings_lists_quotes_code_and_rules() {
        let text =
            "# Title\n\nSome **bold** and `code` text.\n\n- one\n  - two\n\n> quoted\n\n---\n";
        let out = paragraphs(&assistant(text), &options(), 40);
        assert_eq!(
            texts(&out),
            vec![
                "Title",
                "",
                "Some bold and code text.",
                "",
                "• one",
                "  • two",
                "",
                "▎ quoted",
                "",
                &"─".repeat(40),
            ]
        );
        // The heading is accent and bold; markers are gone from the text but
        // the raw markdown rides along for a raw copy.
        assert_eq!(out[0].spans[0].style, theme::heading());
        assert_eq!(out[0].source.as_deref(), Some("# Title\n"));
        let bold = out[2]
            .spans
            .iter()
            .find(|s| &out[2].text[s.start..s.end] == "bold")
            .unwrap();
        assert_eq!(bold.style, theme::bold());
        let code = out[2]
            .spans
            .iter()
            .find(|s| &out[2].text[s.start..s.end] == "code")
            .unwrap();
        assert_eq!(code.style, theme::inline_code());
        assert_eq!(out[4].source.as_deref(), Some("- one\n"));
    }

    #[test]
    fn plain_mode_shows_markdown_as_written() {
        let out = paragraphs(
            &assistant("# Title\n**bold**"),
            &Options {
                markdown: false,
                exists: &exists,
            },
            40,
        );
        assert_eq!(texts(&out), vec!["# Title", "**bold**"]);
        assert!(out.iter().all(|p| p.spans.is_empty() && p.source.is_none()));
    }

    #[test]
    fn fenced_code_has_a_header_with_a_copy_target_and_highlighted_rows() {
        let text = "```rust\nfn main() {\n\tlet x = 1; // hi\n}\n```\n";
        let out = paragraphs(&assistant(text), &options(), 40);
        assert_eq!(out[0].text, "rust  copy");
        assert_eq!(
            targets(&out[0]),
            vec![&Target::Copy(Rc::from(
                "fn main() {\n\tlet x = 1; // hi\n}"
            ))]
        );
        assert_eq!(out[1].text, "fn main() {");
        assert_eq!(out[2].text, "    let x = 1; // hi");
        assert!(out[1..4]
            .iter()
            .all(|p| p.style == Some(theme::code_block())));
        let keyword = out[1]
            .spans
            .iter()
            .find(|s| &out[1].text[s.start..s.end] == "fn")
            .unwrap();
        assert_eq!(keyword.style, theme::token(highlight::Token::Keyword));
        let comment = out[2]
            .spans
            .iter()
            .find(|s| &out[2].text[s.start..s.end] == "// hi")
            .unwrap();
        assert_eq!(comment.style, theme::token(highlight::Token::Comment));
        // Code is never scanned for file references or links.
        let out = paragraphs(&assistant("```\nsee src/main.rs\n```"), &options(), 40);
        assert!(out[1].targets.is_empty());
    }

    #[test]
    fn unterminated_fence_mid_stream_is_already_a_code_block() {
        let out = paragraphs(&assistant("```sh\necho hi"), &options(), 40);
        assert_eq!(texts(&out), vec!["sh  copy", "echo hi"]);
        assert_eq!(out[1].style, Some(theme::code_block()));
    }

    #[test]
    fn links_and_file_references_become_targets_in_prose_and_plain_rows() {
        let out = paragraphs(
            &assistant("See [docs](https://example.com/d) and https://a.b/c. Then src/main.rs:12 and nope.rs"),
            &options(),
            80,
        );
        assert_eq!(
            out[0].text,
            "See docs and https://a.b/c. Then src/main.rs:12 and nope.rs"
        );
        assert_eq!(
            targets(&out[0]),
            vec![
                &Target::Url("https://example.com/d".into()),
                &Target::Url("https://a.b/c".into()),
                &Target::File {
                    path: "src/main.rs".into(),
                    line: Some(12),
                    column: None,
                },
            ]
        );
        let operator = ChatMessage::new(TranscriptRole::Operator, "open Cargo.toml please");
        let out = paragraphs(&operator, &options(), 80);
        assert_eq!(
            targets(&out[0]),
            vec![&Target::File {
                path: "Cargo.toml".into(),
                line: None,
                column: None,
            }]
        );
    }

    #[test]
    fn tables_align_when_they_fit_and_fall_back_to_pipes() {
        let text = "| a | bb |\n|---|---:|\n| 1 | 22 |\n";
        let out = paragraphs(&assistant(text), &options(), 40);
        assert_eq!(texts(&out), vec!["a │ bb", "──┼───", "1 │ 22"]);
        let out = paragraphs(&assistant(text), &options(), 4);
        assert_eq!(texts(&out), vec!["a | bb", "1 | 22"]);
    }

    fn tool(name: &str, output: &str, exit_code: i32, detail: Option<ToolDetail>) -> ChatMessage {
        let mut message = ChatMessage::new(TranscriptRole::Tool { exit_code }, output);
        message.tool = Some(ToolPayload::Result {
            call_id: "c".into(),
            name: name.into(),
            content: String::new(),
        });
        message.detail = detail;
        message.elapsed = Some(Duration::from_millis(3800));
        message
    }

    #[test]
    fn tool_headers_follow_one_grammar() {
        let read = tool(
            "read_file",
            "read_file src/main.rs\nfn main() {}",
            0,
            Some(ToolDetail::Read {
                offset: Some(10),
                limit: Some(2),
                lines: 1,
            }),
        );
        let out = paragraphs(&read, &options(), 80);
        assert_eq!(
            texts(&out),
            vec![
                "read_file src/main.rs · lines 10–10 · 3.8s",
                "10 fn main() {}"
            ]
        );
        assert!(matches!(
            targets(&out[0])[..],
            [Target::File { ref path, line: None, .. }] if path == "src/main.rs"
        ));
        let keyword = out[1]
            .spans
            .iter()
            .find(|s| &out[1].text[s.start..s.end] == "fn");
        assert!(
            keyword.is_some(),
            "read bodies are highlighted by extension"
        );

        let denied = tool("read_file", "read_file secret: denied by rule x", 126, None);
        assert_eq!(
            texts(&paragraphs(&denied, &options(), 80)),
            vec!["read_file secret · denied by rule x · 3.8s"]
        );

        let run = tool(
            "run_command",
            "run_command .\nexit 1",
            0,
            Some(ToolDetail::Command {
                argv: vec!["cargo".into(), "test".into(), "a b".into()],
                exit_code: Some(1),
                stdout: "out line".into(),
                stderr: "bad\nworse".into(),
                stdout_cut: false,
                stderr_cut: true,
            }),
        );
        let out = paragraphs(&run, &options(), 80);
        assert_eq!(
            texts(&out),
            vec![
                "run_command . cargo test 'a b' · exit 1 · 3.8s",
                "stdout",
                "  out line",
                "stderr",
                "  bad",
                "  worse",
                "  … 0 more lines shown to the model, output truncated",
            ]
        );
        assert_eq!(out[3].style, Some(theme::diff_removed()));

        let search = tool(
            "search_files",
            "search_files src\nsrc/main.rs:3:fn main() {}\nother/x.rs:1:zzz\n[2 more matches not shown]",
            0,
            Some(ToolDetail::Search {
                pattern: "main".into(),
                literal: true,
                glob: Some("*.rs".into()),
                matches: 2,
            }),
        );
        let out = paragraphs(&search, &options(), 80);
        assert_eq!(
            out[0].text,
            "search_files src · \"main\" · in *.rs · 2 matches · 3.8s"
        );
        assert_eq!(
            targets(&out[1]),
            vec![&Target::File {
                path: "src/main.rs".into(),
                line: Some(3),
                column: None,
            }]
        );
        assert!(
            targets(&out[2]).is_empty(),
            "unknown files are not clickable"
        );
        assert_eq!(out[3].spans[0].style, theme::dim());
    }

    #[test]
    fn edits_render_a_unified_diff_with_gutter_markers_and_emphasis() {
        let edit = tool(
            "edit_file",
            "edit_file src/main.rs\nedited main.rs",
            0,
            Some(ToolDetail::Write {
                before: Some("fn a() {\n    let x = 1;\n    x\n}\n".into()),
                after: "fn a() {\n    let y = 1;\n    y\n}\n".into(),
            }),
        );
        let out = paragraphs(&edit, &options(), 80);
        assert_eq!(
            texts(&out),
            vec![
                "edit_file src/main.rs · +2 -2 · 3.8s",
                "@@ -1,4 +1,4 @@",
                "1 1  fn a() {",
                "2   -    let x = 1;",
                "3   -    x",
                "  2 +    let y = 1;",
                "  3 +    y",
                "4 4  }",
            ]
        );
        assert_eq!(out[3].style, Some(theme::diff_removed()));
        assert_eq!(out[5].style, Some(theme::diff_added()));
        assert_eq!(out[2].style, None);
        let strong = out[3]
            .spans
            .iter()
            .find(|s| s.style == theme::diff_removed_emphasis())
            .expect("the changed word is emphasised");
        assert_eq!(&out[3].text[strong.start..strong.end], "x");

        let created = tool(
            "write_file",
            "write_file docs/a.md\ncreated a.md",
            0,
            Some(ToolDetail::Write {
                before: None,
                after: "# A\n".into(),
            }),
        );
        assert_eq!(
            texts(&paragraphs(&created, &options(), 80)),
            vec![
                "write_file docs/a.md · created · +1 -0 · 3.8s",
                "@@ -0,0 +1,1 @@",
                "  1 +# A",
            ]
        );
    }

    #[test]
    fn tool_only_assistant_turns_render_nothing_and_footers_right_align() {
        let mut calls = assistant("");
        calls.tool = Some(ToolPayload::Calls {
            calls: vec![ToolCall {
                id: "a".into(),
                name: "read_file".into(),
                arguments: serde_json::Map::new(),
            }],
            native: None,
        });
        assert!(paragraphs(&calls, &options(), 80).is_empty());

        let mut footer = ChatMessage::new(TranscriptRole::Notice, "done in 12s · 14:02");
        footer.elapsed = Some(Duration::from_secs(12));
        let out = paragraphs(&footer, &options(), 30);
        assert_eq!(
            out[0].text,
            format!("{}done in 12s · 14:02", " ".repeat(11))
        );
        assert_eq!(out[0].spans.last().unwrap().style, theme::dim());
    }

    #[test]
    fn startup_header_paints_the_wordmark() {
        let text = crate::chat::header::text(100, "m", "g", "medium", "ahi", true);
        let out = paragraphs(
            &ChatMessage::new(TranscriptRole::Notice, text),
            &options(),
            100,
        );
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].spans[0].style, theme::heading());
        assert_eq!(
            &out[0].text[out[0].spans[0].start..out[0].spans[0].end],
            crate::chat::header::WORDMARK[0]
        );
        let _: Vec<&RowTarget> = out[3].targets.iter().collect();
    }
}
