//! Terminal styling, cell-width wrapping, and scroll windows for transcript blocks.

use super::{ChatMessage, TranscriptRole};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Text};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(crate) struct TranscriptRow {
    /// Original content, without terminal padding.
    pub(crate) text: String,
    /// False for a soft wrap, true for an original newline/message boundary.
    pub(crate) break_after: bool,
    style: Style,
}

pub(crate) fn rows(messages: &[ChatMessage], width: usize) -> Vec<TranscriptRow> {
    let mut rows = Vec::new();
    for message in messages {
        if !rows.is_empty() {
            rows.push(TranscriptRow {
                text: String::new(),
                break_after: true,
                style: Style::default(),
            });
        }
        let background = match message.role {
            TranscriptRole::Operator => Color::Rgb(30, 45, 60),
            TranscriptRole::Tool { exit_code: 0 } => Color::Rgb(30, 60, 45),
            TranscriptRole::Tool { .. } => Color::Rgb(60, 30, 30),
            TranscriptRole::Assistant | TranscriptRole::Notice => Color::Reset,
        };
        let style = Style::default().bg(background);
        for paragraph in message.text.split('\n') {
            let mut text = String::new();
            let mut cells = 0;
            for grapheme in paragraph.graphemes(true) {
                let size = grapheme.width();
                if cells + size > width.max(1) && !text.is_empty() {
                    rows.push(TranscriptRow {
                        text,
                        break_after: false,
                        style,
                    });
                    text = String::new();
                    cells = 0;
                }
                text.push_str(grapheme);
                cells += size;
            }
            rows.push(TranscriptRow {
                text,
                break_after: true,
                style,
            });
        }
    }
    rows
}

pub(crate) fn wrapped_lines(messages: &[ChatMessage], width: usize) -> Vec<Line<'static>> {
    rows(messages, width)
        .into_iter()
        .map(|row| {
            let padding = width.saturating_sub(row.text.width());
            let mut text = row.text;
            text.extend(std::iter::repeat_n(' ', padding));
            Line::styled(text, row.style)
        })
        .collect()
}

pub(crate) fn visible_text(
    lines: &[Line<'static>],
    height: usize,
    scroll_from_bottom: usize,
) -> Text<'static> {
    let end = lines.len().saturating_sub(scroll_from_bottom);
    let start = end.saturating_sub(height);
    Text::from(lines[start..end].to_vec())
}

pub(crate) fn clamp_scroll(scroll_from_bottom: &mut usize, line_count: usize, height: usize) {
    let max_scroll = line_count.saturating_sub(height);
    if *scroll_from_bottom > max_scroll {
        *scroll_from_bottom = max_scroll;
    }
}
