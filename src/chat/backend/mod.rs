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
pub(crate) use tool_loop::{Caller, EXIT_DENIED, EXIT_OK};

use super::thinking::ThinkingLevel;
use super::transcript::ChatMessage;
use crate::error::ErrorKind;

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
    /// `native` is the provider's own output the screen stores for replay.
    ToolCallsRequested {
        calls: Vec<ToolCall>,
        native: Option<super::transcript::NativeReplay>,
    },
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
        /// Screen-only structure: diff texts, command streams, query.
        detail: Option<tools::ToolDetail>,
        /// Wall time of the call itself, approval excluded.
        elapsed: std::time::Duration,
        /// What the call did to the world, for a caller's ledger. `None` for
        /// reads, denials, and calls that never reached a tool. Boxed to keep
        /// the common text events small.
        record: Option<Box<ToolRecord>>,
    },
    Completed,
    /// A completion that could not be produced. `kind` is what the screen
    /// acts on: a `Transport` failure before any reply offers a resend, the
    /// others only explain themselves.
    Failed {
        message: String,
        kind: ErrorKind,
    },
    /// A gateway could not be reached and another connection attempt follows
    /// after a short backoff. Only connection failures are retried, never a
    /// request the server may have received.
    Reconnecting {
        attempt: u8,
        attempts: u8,
    },
}

/// Completes one turn without blocking the screen.
pub(crate) trait CompletionBackend {
    fn submit(&mut self, request: ChatRequest) -> Receiver<ResponseEvent>;
    /// Stop the active turn: no further request, tool round, or stream read
    /// proceeds, and the connection is dropped at the next read. The screen
    /// drops the receiver as well, so the turn's final event goes nowhere.
    fn cancel(&mut self) {}
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
    /// What the call would do, from its arguments alone: line counts for a
    /// write, empty for a read. Never file contents or raw arguments.
    pub(crate) detail: String,
}

/// The operator's answer for one proposed action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolDecision {
    Approve,
    /// Approve, and grant this tool under `grant_prefix` for the session.
    ApproveForSession,
    Deny,
    /// Denied because nobody can answer: `ahi exec --headless`, or a run
    /// whose stdin is not a terminal. The model is told so, which is more
    /// useful than "denied by the operator" when there is none.
    Unattended,
}

/// The audit-log facts of one effectful call, as the caller's ledger
/// records them: paths and hashes, argv and exit codes. Never contents or
/// output. Mirrors `hooks::audit::{WriteEvent, CommandEvent}` so a result
/// file and `audit.log` agree line for line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolRecord {
    Write {
        tool: String,
        /// Workspace-relative path.
        path: String,
        /// `ok`, or the tool's error message.
        outcome: String,
        /// SHA-256 hex of the file before and after; `None` when absent.
        before: Option<String>,
        after: Option<String>,
        lines_added: usize,
        lines_removed: usize,
    },
    Command {
        argv: Vec<String>,
        /// Workspace-relative working directory; empty is the root.
        cwd: String,
        outcome: String,
        /// Whether a process ran: `Some(true)` it did (even if killed),
        /// `Some(false)` it never started, `None` unknown.
        ran: Option<bool>,
        exit_code: Option<i32>,
        duration_ms: u128,
    },
}

impl ToolRecord {
    /// Whether this call changed the working directory or ran a program.
    /// `None` when that cannot be known from the outcome.
    pub(crate) fn effect(&self) -> Option<bool> {
        match self {
            Self::Write { outcome, .. } => Some(outcome == "ok"),
            Self::Command { ran, .. } => *ran,
        }
    }
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
