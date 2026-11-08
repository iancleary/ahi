//! Terminal styling, cell-width wrapping, and scroll windows for transcript blocks.
//!
//! Rows are the source of truth for scrolling and selection. `TranscriptLayout`
//! keeps them between frames and re-lays only the messages that changed; a
//! frame then pads and styles the visible window alone.

use super::{ChatMessage, TranscriptRole};
use ratatui_core::style::{Color, Style};
use ratatui_core::text::Line;
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
    /// Bytes of the last message that the rows cover, for append-only resumes.
    last_len: usize,
}

/// What changed since the last `sync`, as the screen tracked it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dirty {
    /// Messages from this index on may have changed in any way.
    From(usize),
    /// Only the last message changed, and only by appending text.
    AppendedLast,
}

impl TranscriptLayout {
    pub(crate) fn new() -> Self {
        Self {
            width: 0,
            rows: Vec::new(),
            starts: Vec::new(),
            last_len: 0,
        }
    }

    /// Bring rows up to date. A width change re-lays everything; otherwise
    /// only messages from the dirty index on are re-laid, and messages appended
    /// without a dirty mark are picked up too. An append to the last message
    /// resumes from that message's last row instead of its first byte, so a
    /// streamed reply costs the new text, not the whole message, per frame.
    /// Leaves `*dirty` clean.
    pub(crate) fn sync(&mut self, messages: &[ChatMessage], width: usize, dirty: &mut Dirty) {
        let width = width.max(1);
        let from = match *dirty {
            _ if width != self.width => 0,
            Dirty::AppendedLast if self.resume_last(messages, width) => messages.len(),
            Dirty::AppendedLast => messages.len().saturating_sub(1),
            Dirty::From(index) => index.min(self.starts.len()).min(messages.len()),
        };
        self.width = width;
        let keep_rows = self.starts.get(from).copied().unwrap_or(self.rows.len());
        self.rows.truncate(keep_rows);
        self.starts.truncate(from);
        for message in &messages[from..] {
            self.starts.push(self.rows.len());
            layout_message(&mut self.rows, message, width);
        }
        if let Some(last) = messages.last() {
            self.last_len = last.text.len();
        }
        *dirty = Dirty::From(messages.len());
    }

    /// Re-wrap only the last row of the last message plus its new text.
    /// False when the rows do not cover a prefix of that message, in which
    /// case the caller re-lays it in full.
    fn resume_last(&mut self, messages: &[ChatMessage], width: usize) -> bool {
        let Some(message) = messages.last() else {
            return false;
        };
        if self.starts.len() != messages.len() || message.text.len() < self.last_len {
            return false;
        }
        let first_row = self.starts[messages.len() - 1];
        // Messages with no rows (tool-only turns) and the separator do not
        // carry text to resume from.
        if self.rows.len() <= first_row + 1 && (first_row > 0 || self.rows.len() <= first_row) {
            return false;
        }
        if !message.text.is_char_boundary(self.last_len) {
            return false;
        }
        let Some(tail) = self.rows.pop() else {
            return false;
        };
        let mut text = tail.text;
        text.push_str(&message.text[self.last_len..]);
        layout_paragraphs(&mut self.rows, &text, tail.style, width);
        true
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
        TranscriptRole::Alert => Color::Rgb(70, 50, 20),
        TranscriptRole::Assistant | TranscriptRole::Notice => Color::Reset,
    };
    let style = Style::default().bg(background);
    layout_paragraphs(rows, &message.text, style, width);
}

/// Wrap `text` by grapheme cell width, one row per soft wrap or newline.
fn layout_paragraphs(rows: &mut Vec<TranscriptRow>, text: &str, style: Style, width: usize) {
    for paragraph in text.split('\n') {
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
        let mut dirty = Dirty::From(0);
        layout.sync(&messages, 6, &mut dirty);
        let texts =
            |rows: &[TranscriptRow]| rows.iter().map(|r| r.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(layout.rows()), texts(&rows(&messages, 6)));
        assert_eq!(dirty, Dirty::From(2));

        // Streaming appends to the last message: only it is re-laid.
        messages[1].text.push_str(" and more");
        dirty = Dirty::From(1);
        layout.sync(&messages, 6, &mut dirty);
        assert_eq!(texts(layout.rows()), texts(&rows(&messages, 6)));

        // A message appended without a dirty mark is still picked up.
        messages.push(message(TranscriptRole::Notice, "note"));
        layout.sync(&messages, 6, &mut dirty);
        assert_eq!(texts(layout.rows()), texts(&rows(&messages, 6)));

        // An earlier message edited in place re-lays from there.
        messages[0].text = "edited".into();
        dirty = Dirty::From(0);
        layout.sync(&messages, 6, &mut dirty);
        assert_eq!(texts(layout.rows()), texts(&rows(&messages, 6)));

        // A width change re-lays everything; clearing truncates.
        layout.sync(&messages, 40, &mut dirty);
        assert_eq!(texts(layout.rows()), texts(&rows(&messages, 40)));
        messages.clear();
        dirty = Dirty::From(0);
        layout.sync(&messages, 40, &mut dirty);
        assert!(layout.rows().is_empty());
    }

    #[test]
    fn appending_to_the_last_message_resumes_from_its_last_row() {
        let texts =
            |rows: &[TranscriptRow]| rows.iter().map(|r| r.text.clone()).collect::<Vec<_>>();
        let breaks =
            |rows: &[TranscriptRow]| rows.iter().map(|r| r.break_after).collect::<Vec<_>>();
        let check = |layout: &TranscriptLayout, messages: &[ChatMessage], width| {
            let expected = rows(messages, width);
            assert_eq!(texts(layout.rows()), texts(&expected));
            assert_eq!(breaks(layout.rows()), breaks(&expected));
        };
        let width = 7;
        // Deltas that cross wrap boundaries, carry newlines (including a
        // trailing one), wide graphemes, and a combining mark joining the
        // previous grapheme.
        let deltas = [
            "stream",
            "ing wor",
            "ds\n",
            "next p",
            "aragraph that wraps",
            "\n\n",
            "日本語テキスト",
            "e",
            "\u{301}",
            "",
            "tail",
        ];

        // First message of the transcript: no separator row precedes it.
        let mut messages = vec![message(TranscriptRole::Assistant, "")];
        let mut layout = TranscriptLayout::new();
        let mut dirty = Dirty::From(0);
        layout.sync(&messages, width, &mut dirty);
        for delta in deltas {
            messages[0].text.push_str(delta);
            dirty = Dirty::AppendedLast;
            layout.sync(&messages, width, &mut dirty);
            check(&layout, &messages, width);
            assert_eq!(dirty, Dirty::From(1));
        }

        // Later message: a separator precedes it and the resume must keep it.
        messages.push(message(TranscriptRole::Operator, "question"));
        messages.push(message(TranscriptRole::Assistant, ""));
        layout.sync(&messages, width, &mut dirty);
        for delta in deltas {
            messages[2].text.push_str(delta);
            dirty = Dirty::AppendedLast;
            layout.sync(&messages, width, &mut dirty);
            check(&layout, &messages, width);
        }

        // An append flag on a message whose rows were never laid out (a
        // tool-only turn with no text) falls back to a full layout of it.
        let mut tool_turn = message(TranscriptRole::Assistant, "");
        tool_turn.tool = Some(super::super::ToolPayload::Calls {
            calls: Vec::new(),
            native: None,
        });
        messages.push(tool_turn);
        layout.sync(&messages, width, &mut dirty);
        check(&layout, &messages, width);
        messages[3].text.push_str("late text");
        dirty = Dirty::AppendedLast;
        layout.sync(&messages, width, &mut dirty);
        check(&layout, &messages, width);

        // A width change with the append flag re-lays everything.
        messages[3].text.push_str(" more");
        dirty = Dirty::AppendedLast;
        layout.sync(&messages, 11, &mut dirty);
        check(&layout, &messages, 11);

        // Text that shrank is not an append: full layout of the last message.
        messages[3].text = "short".into();
        dirty = Dirty::AppendedLast;
        layout.sync(&messages, 11, &mut dirty);
        check(&layout, &messages, 11);
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
