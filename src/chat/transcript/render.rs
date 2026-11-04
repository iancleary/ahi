//! Terminal styling, wrapping, and scroll windows for transcript blocks.

use super::{ChatMessage, TranscriptRole};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Text};
use unicode_width::UnicodeWidthStr;

pub(crate) fn wrapped_lines(messages: &[ChatMessage], width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for message in messages {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        // One shared intensity for each tint; terminals cannot alpha-blend.
        let background = match message.role {
            TranscriptRole::Operator => Color::Rgb(30, 45, 60),
            TranscriptRole::Tool { exit_code: 0 } => Color::Rgb(30, 60, 45),
            TranscriptRole::Tool { .. } => Color::Rgb(60, 30, 30),
            TranscriptRole::Assistant | TranscriptRole::Notice => Color::Reset,
        };
        let style = Style::default().bg(background);
        lines.extend(
            wrap_plain(&message.text, width)
                .into_iter()
                .map(|mut text| {
                    // Paint the row through its right edge without indenting content.
                    let padding = width.saturating_sub(text.width());
                    text.extend(std::iter::repeat_n(' ', padding));
                    Line::styled(text, style)
                }),
        );
    }
    lines
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

fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        lines.extend(wrap_paragraph("", paragraph, width));
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn wrap_paragraph(prefix: &str, paragraph: &str, width: usize) -> Vec<String> {
    let prefix_len = prefix.chars().count();
    if width <= prefix_len {
        return vec![format!("{prefix}{paragraph}")];
    }
    let room = width - prefix_len;
    if paragraph.chars().count() <= room {
        return vec![format!("{prefix}{paragraph}")];
    }
    let mut lines = Vec::new();
    let chars: Vec<char> = paragraph.chars().collect();
    let mut offset = 0;
    let mut first = true;
    while offset < chars.len() {
        let end = (offset + room).min(chars.len());
        let piece: String = chars[offset..end].iter().collect();
        if first {
            lines.push(format!("{prefix}{piece}"));
            first = false;
        } else {
            lines.push(format!("{}{piece}", " ".repeat(prefix_len)));
        }
        offset = end;
    }
    lines
}
