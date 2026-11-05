//! Terminal styling, cell-width wrapping, and scroll windows for transcript blocks.
//!
//! Rows are the source of truth for scrolling and selection. `TranscriptLayout`
//! keeps them between frames and re-lays only the messages that changed; a
//! frame then pads and styles the visible window alone.

use super::{ChatMessage, TranscriptRole};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(crate) struct TranscriptRow {
    /// Original content, without terminal padding.
    pub(crate) text: String,
    /// False for a soft wrap, true for an original newline/message boundary.
    pub(crate) break_after: bool,
    style: Style,
}

/// Wrapped rows for a message list, kept up to date incrementally.
pub(crate) struct TranscriptLayout {
    width: usize,
    rows: Vec<TranscriptRow>,
    /// Row index where each laid-out message begins, separator included.
    starts: Vec<usize>,
}

impl TranscriptLayout {
    pub(crate) fn new() -> Self {
        Self {
            width: 0,
            rows: Vec::new(),
            starts: Vec::new(),
        }
    }

    /// Bring rows up to date. A width change re-lays everything; otherwise
    /// only messages from `*dirty_from` on are re-laid, and messages appended
    /// without a dirty mark are picked up too. Leaves `*dirty_from` clean.
    pub(crate) fn sync(&mut self, messages: &[ChatMessage], width: usize, dirty_from: &mut usize) {
        let width = width.max(1);
        let from = if width == self.width {
            (*dirty_from).min(self.starts.len()).min(messages.len())
        } else {
            0
        };
        self.width = width;
        let keep_rows = self.starts.get(from).copied().unwrap_or(self.rows.len());
        self.rows.truncate(keep_rows);
        self.starts.truncate(from);
        for message in &messages[from..] {
            self.starts.push(self.rows.len());
            layout_message(&mut self.rows, message, width);
        }
        *dirty_from = messages.len();
    }

    pub(crate) fn rows(&self) -> &[TranscriptRow] {
        &self.rows
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }
}

/// Append one message's rows: a blank separator when rows precede it, then
/// each paragraph wrapped by grapheme cell width. Tool-only assistant turns
/// have nothing to show; their results do.
fn layout_message(rows: &mut Vec<TranscriptRow>, message: &ChatMessage, width: usize) {
    if message.text.is_empty() && message.tool.is_some() {
        return;
    }
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

/// All rows of a message list. One-shot; the screen uses `TranscriptLayout`.
#[cfg(test)]
pub(crate) fn rows(messages: &[ChatMessage], width: usize) -> Vec<TranscriptRow> {
    let mut rows = Vec::new();
    for message in messages {
        layout_message(&mut rows, message, width);
    }
    rows
}

fn padded_line(row: &TranscriptRow, width: usize) -> Line<'static> {
    let padding = width.saturating_sub(row.text.width());
    let mut text = String::with_capacity(row.text.len() + padding);
    text.push_str(&row.text);
    text.extend(std::iter::repeat_n(' ', padding));
    Line::styled(text, row.style)
}

/// Every row as a padded line. For tests and one-shot callers.
#[cfg(test)]
pub(crate) fn wrapped_lines(messages: &[ChatMessage], width: usize) -> Vec<Line<'static>> {
    rows(messages, width)
        .iter()
        .map(|row| padded_line(row, width))
        .collect()
}

/// The visible window as padded lines, and the index of its first row.
pub(crate) fn visible_lines(
    rows: &[TranscriptRow],
    width: usize,
    height: usize,
    scroll_from_bottom: usize,
) -> (Vec<Line<'static>>, usize) {
    let end = rows.len().saturating_sub(scroll_from_bottom);
    let start = end.saturating_sub(height);
    (
        rows[start..end]
            .iter()
            .map(|row| padded_line(row, width))
            .collect(),
        start,
    )
}

pub(crate) fn clamp_scroll(scroll_from_bottom: &mut usize, line_count: usize, height: usize) {
    let max_scroll = line_count.saturating_sub(height);
    if *scroll_from_bottom > max_scroll {
        *scroll_from_bottom = max_scroll;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: TranscriptRole, text: &str) -> ChatMessage {
        ChatMessage::new(role, text)
    }

    #[test]
    fn incremental_layout_matches_the_one_shot_layout() {
        let mut messages = vec![
            message(TranscriptRole::Operator, "hello world"),
            message(TranscriptRole::Assistant, "a\nlonger reply here"),
        ];
        let mut layout = TranscriptLayout::new();
        let mut dirty = 0;
        layout.sync(&messages, 6, &mut dirty);
        let texts =
            |rows: &[TranscriptRow]| rows.iter().map(|r| r.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(layout.rows()), texts(&rows(&messages, 6)));
        assert_eq!(dirty, 2);

        // Streaming appends to the last message: only it is re-laid.
        messages[1].text.push_str(" and more");
        dirty = 1;
        layout.sync(&messages, 6, &mut dirty);
        assert_eq!(texts(layout.rows()), texts(&rows(&messages, 6)));

        // A message appended without a dirty mark is still picked up.
        messages.push(message(TranscriptRole::Notice, "note"));
        layout.sync(&messages, 6, &mut dirty);
        assert_eq!(texts(layout.rows()), texts(&rows(&messages, 6)));

        // An earlier message edited in place re-lays from there.
        messages[0].text = "edited".into();
        dirty = 0;
        layout.sync(&messages, 6, &mut dirty);
        assert_eq!(texts(layout.rows()), texts(&rows(&messages, 6)));

        // A width change re-lays everything; clearing truncates.
        layout.sync(&messages, 40, &mut dirty);
        assert_eq!(texts(layout.rows()), texts(&rows(&messages, 40)));
        messages.clear();
        dirty = 0;
        layout.sync(&messages, 40, &mut dirty);
        assert!(layout.rows().is_empty());
    }

    #[test]
    fn visible_window_pads_only_its_rows() {
        let messages = vec![message(TranscriptRole::Assistant, "1\n2\n3\n4\n5")];
        let rows = rows(&messages, 4);
        let (lines, start) = visible_lines(&rows, 4, 2, 1);
        assert_eq!(start, 2);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].to_string(), "3   ");
        assert_eq!(lines[1].to_string(), "4   ");
    }
}
