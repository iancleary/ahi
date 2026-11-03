//! Transcript messages, wrapping, and the scroll window.
//!
//! Wrapping returns plain strings. The screen places them in a paragraph.

/// One transcript entry sent to a backend, or a local notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatMessage {
    pub(crate) role: TranscriptRole,
    pub(crate) text: String,
}

/// Who produced a transcript line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptRole {
    Operator,
    Assistant,
    Notice,
}

pub(crate) const STAND_IN_NOTE: &str =
    "stand-in backend: replies and token counts are not from a model";

/// Attach the stand-in note to the only assistant message, when it is a stand-in reply.
pub(crate) fn attach_stand_in_note(messages: &mut [ChatMessage]) {
    let assistants = messages
        .iter()
        .filter(|message| message.role == TranscriptRole::Assistant)
        .count();
    if assistants != 1 {
        return;
    }
    let Some(message) = messages
        .iter_mut()
        .rev()
        .find(|message| message.role == TranscriptRole::Assistant)
    else {
        return;
    };
    if message.text.starts_with("[stand-in]") && !message.text.contains(STAND_IN_NOTE) {
        if !message.text.ends_with('\n') {
            message.text.push('\n');
        }
        message.text.push_str(STAND_IN_NOTE);
    }
}

pub(crate) fn wrapped_lines(messages: &[ChatMessage], width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for message in messages {
        match message.role {
            TranscriptRole::Operator => lines.extend(wrap_labeled("you ", &message.text, width)),
            TranscriptRole::Assistant => lines.extend(wrap_labeled("ahi ", &message.text, width)),
            TranscriptRole::Notice => lines.extend(wrap_plain(&message.text, width)),
        }
    }
    lines
}

pub(crate) fn visible_text(lines: &[String], height: usize, scroll_from_bottom: usize) -> String {
    if height == 0 || lines.is_empty() {
        return String::new();
    }
    let end = lines.len().saturating_sub(scroll_from_bottom);
    let start = end.saturating_sub(height);
    lines[start..end].join("\n")
}

pub(crate) fn clamp_scroll(scroll_from_bottom: &mut usize, line_count: usize, height: usize) {
    let max_scroll = line_count.saturating_sub(height);
    if *scroll_from_bottom > max_scroll {
        *scroll_from_bottom = max_scroll;
    }
}

fn wrap_labeled(label: &str, text: &str, width: usize) -> Vec<String> {
    if width <= label.chars().count() {
        return wrap_plain(&format!("{label}{text}"), width);
    }
    let indent = " ".repeat(label.chars().count());
    let mut lines = Vec::new();
    for (index, paragraph) in text.split('\n').enumerate() {
        let prefix = if index == 0 { label } else { &indent };
        let wrapped = wrap_paragraph(prefix, paragraph, width);
        if wrapped.is_empty() {
            lines.push(prefix.trim_end().to_string());
        } else {
            lines.extend(wrapped);
        }
    }
    lines
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
