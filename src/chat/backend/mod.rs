//! Backend contract for one completion turn.
//!
//! The screen drains a `std::sync::mpsc::Receiver` on each frame. No async
//! runtime is involved.

mod http;
mod protocol;
mod sse;
mod tool_call;
mod tool_loop;
pub(crate) mod tools;

pub(crate) use tool_call::ToolCall;

use std::sync::mpsc::{Receiver, Sender};

pub(crate) use http::HttpBackend;

use super::thinking::ThinkingLevel;
use super::transcript::ChatMessage;

/// Gateway and model, already split on the first slash of a selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selection {
    pub(crate) gateway: String,
    pub(crate) model: String,
}

pub(crate) fn parse_selection(text: &str) -> Selection {
    let text = text.trim();
    match text.split_once('/') {
        Some((gateway, model)) => Selection {
            gateway: gateway.to_string(),
            model: model.to_string(),
        },
        None => Selection {
            gateway: text.to_string(),
            model: String::new(),
        },
    }
}

/// What the screen asks a backend to complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatRequest {
    pub(crate) selection: Selection,
    pub(crate) thinking: ThinkingLevel,
    pub(crate) messages: Vec<ChatMessage>,
}

/// One streaming result from a backend. Not `Clone`: an approval's reply
/// channel must have exactly one owner, so dropping it denies the batch.
#[derive(Debug, PartialEq)]
pub(crate) enum ResponseEvent {
    TextDelta(String),
    /// Validated requests only. This event never grants execution authority.
    ToolCallsRequested(Vec<ToolCall>),
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cached_input_tokens: Option<u64>,
        cost_usd: Option<f64>,
    },
    ContextWindow(u64),
    /// Calls that rules alone did not decide. The screen answers through
    /// `reply`, once, with one decision per call in order. Dropping the reply
    /// denies the whole batch and ends the turn.
    ToolApprovalRequested(ApprovalRequest),
    /// Emitted after EVERY tool call returns, including errors and denials,
    /// before the assistant response continues. The UI rechecks Git HEAD
    /// against its stored location snapshot; this is not a turn-completion event.
    ToolCallReturned,
    /// The outcome of one requested call. `exit_code` is 0 for success, 1 for a
    /// tool error, and 126 for a call that policy or the operator denied.
    ToolResult {
        call_id: String,
        tool: String,
        /// Bounded preview for the transcript.
        output: String,
        /// The `role: "tool"` content sent back to the model.
        content: String,
        exit_code: i32,
    },
    Completed,
    /// A completion that could not be produced.
    Failed(String),
}

/// Completes one turn without blocking the screen.
pub(crate) trait CompletionBackend {
    fn submit(&mut self, request: ChatRequest) -> Receiver<ResponseEvent>;
}

/// One call awaiting the operator, described by its prepared (normalized)
/// action, never by the model's raw arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProposedAction {
    pub(crate) call_id: String,
    pub(crate) tool: String,
    pub(crate) target: String,
    /// Workspace-relative prefix a session grant would cover; `""` is the root.
    pub(crate) grant_prefix: String,
}

/// The operator's answer for one proposed action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolDecision {
    Approve,
    /// Approve, and grant this tool under `grant_prefix` for the session.
    ApproveForSession,
    Deny,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ApprovalRequest {
    pub(crate) batch: u64,
    pub(crate) calls: Vec<ProposedAction>,
    pub(crate) reply: ApprovalReply,
}

/// The only path by which an operator decision reaches the backend. Model
/// output and tool results travel on the response channel, never this one.
pub(crate) struct ApprovalReply {
    batch: u64,
    sender: Sender<Vec<ToolDecision>>,
}

impl ApprovalReply {
    pub(crate) fn new(batch: u64, sender: Sender<Vec<ToolDecision>>) -> Self {
        Self { batch, sender }
    }

    pub(crate) fn send(self, decisions: Vec<ToolDecision>) {
        // A closed channel means the turn already ended; nothing runs.
        let _ = self.sender.send(decisions);
    }
}

impl std::fmt::Debug for ApprovalReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ApprovalReply({})", self.batch)
    }
}

impl PartialEq for ApprovalReply {
    fn eq(&self, other: &Self) -> bool {
        self.batch == other.batch
    }
}

impl Eq for ApprovalReply {}
