//! Transcript data shared by the screen and completion backends.

pub(crate) mod render;

use super::backend::ToolCall;

/// One transcript entry sent to a backend, or a local notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatMessage {
    pub(crate) role: TranscriptRole,
    /// What the screen shows. For a tool result this is a bounded preview.
    pub(crate) text: String,
    /// Structured tool exchange replayed to the model on later requests.
    pub(crate) tool: Option<ToolPayload>,
}

/// The model-facing half of a tool exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolPayload {
    /// Calls the assistant requested at the end of this assistant message.
    Calls(Vec<ToolCall>),
    /// The `role: "tool"` reply to one call.
    Result {
        call_id: String,
        name: String,
        content: String,
    },
}

impl ChatMessage {
    pub(crate) fn new(role: TranscriptRole, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
            tool: None,
        }
    }
}

/// Who produced a transcript line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptRole {
    Operator,
    Assistant,
    Notice,
    Tool { exit_code: i32 },
}
