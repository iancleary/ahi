//! Backend contract for one completion turn.
//!
//! The screen drains a [`std::sync::mpsc::Receiver`] on each frame. No async
//! runtime is involved.

mod echo;

use std::sync::mpsc::Receiver;

pub(crate) use echo::EchoBackend;

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

/// One streaming result from a backend.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ResponseEvent {
    TextDelta(String),
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cached_input_tokens: Option<u64>,
        cost_usd: Option<f64>,
    },
    ContextWindow(u64),
    Completed,
    /// A completion that could not be produced. The stand-in backend does not emit this.
    #[allow(dead_code)]
    Failed(String),
}

/// Completes one turn without blocking the screen.
pub(crate) trait CompletionBackend {
    fn submit(&mut self, request: ChatRequest) -> Receiver<ResponseEvent>;
}
