//! Transcript data shared by the screen and completion backends.

pub(crate) mod render;

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
    Tool { exit_code: i32 },
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
