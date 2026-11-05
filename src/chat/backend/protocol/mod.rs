//! Wire protocols: request serialization, stream decoding, and tool schemas.
//!
//! The turn loop in `http.rs` is protocol-neutral. Each protocol supplies the
//! three things it needs: a request body for a transcript, a stream reader
//! that yields exactly one `StreamOutcome`, and its tool-definition shape.
//! Policy, approval, execution, and result accounting live outside this module.

pub(super) mod anthropic;
pub(super) mod chat_completions;
mod replay;
pub(super) mod responses;

use std::io::Read;
use std::sync::mpsc::Sender;

use crate::chat::thinking::ThinkingLevel;
use crate::chat::transcript::ChatMessage;
use crate::error::{Error, Result};
use crate::harness::{ModelMetadata, Protocol};

use super::tools::ToolSchema;
use super::{ChatRequest, ResponseEvent, ToolCall};

/// Conservative output budget sent when a model declares `maxTokens`.
pub(super) const DEFAULT_OUTPUT_BUDGET: u64 = 4096;
/// Hard cap on any serialized request, before the model's own declared limit.
pub(super) const REQUEST_CAP: u64 = 16 * 1024 * 1024;

/// How one model response ended.
#[derive(Debug, PartialEq)]
pub(super) enum StreamOutcome {
    Stopped,
    /// `text` is the assistant content streamed before the calls.
    ToolCalls {
        text: String,
        calls: Vec<ToolCall>,
    },
}

/// Everything a protocol needs to serialize one request.
pub(super) struct RequestContext<'a> {
    pub(super) model: &'a str,
    pub(super) thinking: ThinkingLevel,
    pub(super) metadata: &'a ModelMetadata,
    pub(super) messages: &'a [ChatMessage],
    /// Advertised builtin tools; `None` sends no tool field at all.
    pub(super) tools: Option<&'a [ToolSchema]>,
    /// A browser-login OpenAI credential: omit fields that endpoint rejects.
    pub(super) browser_token: bool,
}

impl<'a> RequestContext<'a> {
    pub(super) fn new(
        request: &'a ChatRequest,
        metadata: &'a ModelMetadata,
        messages: &'a [ChatMessage],
        tools: Option<&'a [ToolSchema]>,
        browser_token: bool,
    ) -> Self {
        Self {
            model: &request.selection.model,
            thinking: request.thinking,
            metadata,
            messages,
            tools,
            browser_token,
        }
    }

    /// `min(4096, maxTokens)` when the model declares a ceiling.
    pub(super) fn output_budget(&self) -> Option<u64> {
        self.metadata
            .max_tokens
            .map(|limit| DEFAULT_OUTPUT_BUDGET.min(limit.get()))
    }

    /// The reasoning effort to send, only when metadata explicitly permits it.
    pub(super) fn reasoning_effort(&self) -> Option<&'static str> {
        (self.metadata.reasoning == Some(true)
            && self.metadata.compat.supports_reasoning_effort == Some(true)
            && self.thinking != ThinkingLevel::Off)
            .then(|| self.thinking.as_str())
    }
}

/// Serialize a request and enforce the byte limits before any network use.
pub(super) fn request_body(protocol: Protocol, context: &RequestContext<'_>) -> Result<Vec<u8>> {
    let value = match protocol {
        Protocol::OpenAiCompletions => chat_completions::request(context),
        Protocol::OpenAiResponses => responses::request(context),
        Protocol::AnthropicMessages => anthropic::request(context)?,
    };
    let body =
        serde_json::to_vec(&value).map_err(|_| Error::new("cannot serialize model request"))?;
    if body.len() as u64 > REQUEST_CAP {
        return Err(Error::limit(
            "model request body exceeds 16 MiB; shorten the conversation",
        ));
    }
    if context
        .metadata
        .input_limits
        .max_request_bytes
        .is_some_and(|limit| body.len() as u64 > limit.get())
    {
        return Err(Error::limit("model request exceeds inputLimits.maxRequestBytes; shorten the conversation or select another model"));
    }
    Ok(body)
}

/// Decode one streamed response into events and exactly one outcome.
pub(super) fn read_stream(
    protocol: Protocol,
    reader: impl Read,
    sender: &Sender<ResponseEvent>,
    metadata: &ModelMetadata,
) -> Result<StreamOutcome> {
    match protocol {
        Protocol::OpenAiCompletions => chat_completions::read_stream(reader, sender, metadata),
        Protocol::OpenAiResponses => responses::read_stream(reader, sender, metadata),
        Protocol::AnthropicMessages => anthropic::read_stream(reader, sender, metadata),
    }
}

/// Whether a `200` response must declare `text/event-stream` to be read.
/// Chat Completions gateways are held to it; the others validate framing.
pub(super) fn requires_event_stream_type(protocol: Protocol) -> bool {
    protocol == Protocol::OpenAiCompletions
}

pub(super) fn emit(sender: &Sender<ResponseEvent>, event: ResponseEvent) -> Result<()> {
    sender
        .send(event)
        .map_err(|_| Error::new("model response receiver closed"))
}

/// Reported usage, normalized: `input` includes cached tokens.
pub(super) fn usage_event(
    metadata: &ModelMetadata,
    input: u64,
    output: u64,
    cached: Option<u64>,
) -> ResponseEvent {
    ResponseEvent::Usage {
        input_tokens: input,
        output_tokens: output,
        cached_input_tokens: cached,
        cost_usd: metadata.usage_cost(input, output, cached),
    }
}
