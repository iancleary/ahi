//! Streaming gateway Chat Completions and browser-login OpenAI Responses.
//! No redirect following, tool execution, or synthetic usage.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::tool_call::{ToolCallAssembler, ToolCallDelta};
use super::{ChatRequest, CompletionBackend, ResponseEvent, ToolCall};
use crate::chat::transcript::TranscriptRole;
use crate::error::{Error, Result};

#[cfg(test)]
mod tests;

mod responses;

const FRAME_CAP: u64 = 1024 * 1024;
const STREAM_CAP: u64 = 16 * 1024 * 1024;
const DEFAULT_OUTPUT_BUDGET: u64 = 4096;

pub(crate) struct HttpBackend {
    state: PathBuf,
}

impl HttpBackend {
    pub(crate) fn new(state: PathBuf) -> Self {
        Self { state }
    }
}

impl CompletionBackend for HttpBackend {
    fn submit(&mut self, request: ChatRequest) -> Receiver<ResponseEvent> {
        let (sender, receiver) = mpsc::channel();
        let state = self.state.clone();
        std::thread::spawn(move || {
            let result = complete(&state, &request, &sender);
            let event = match result {
                Ok(()) => ResponseEvent::Completed,
                Err(error) => ResponseEvent::Failed(error.to_string()),
            };
            let _ = sender.send(event);
        });
        receiver
    }
}

fn complete(
    state: &std::path::Path,
    request: &ChatRequest,
    sender: &Sender<ResponseEvent>,
) -> Result<()> {
    let connection = crate::harness::completion_connection(
        state,
        &request.selection.gateway,
        &request.selection.model,
    )?;
    let metadata = &connection.metadata;
    let (url, headers) = match &connection.transport {
        crate::harness::CompletionTransport::OpenAiResponses => {
            return responses::complete(state, request, sender, metadata);
        }
        crate::harness::CompletionTransport::Gateway { url, headers } => (url, headers),
    };
    if let Some(window) = metadata.context_window {
        emit(sender, ResponseEvent::ContextWindow(window.get()))?;
    }
    let messages = request
        .messages
        .iter()
        .filter_map(|message| {
            let role = match message.role {
                TranscriptRole::Operator => "user",
                TranscriptRole::Assistant => "assistant",
                TranscriptRole::Notice | TranscriptRole::Tool { .. } => return None,
            };
            let mut value = json!({"role": role, "content": message.text});
            if role == "assistant"
                && metadata
                    .compat
                    .requires_reasoning_content_on_assistant_messages
                    == Some(true)
            {
                value["reasoning_content"] = json!("");
            }
            Some(value)
        })
        .collect::<Vec<_>>();
    let mut body = json!({
        "model": request.selection.model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true}
    });
    if metadata.compat.supports_usage_in_streaming == Some(false) {
        body.as_object_mut()
            .expect("request object")
            .remove("stream_options");
    }
    if let Some(limit) = metadata.max_tokens {
        let field = metadata
            .compat
            .max_tokens_field
            .map(|field| field.field())
            .unwrap_or("max_tokens");
        body[field] = json!(DEFAULT_OUTPUT_BUDGET.min(limit.get()));
    }
    if metadata.reasoning == Some(true)
        && metadata.compat.supports_reasoning_effort == Some(true)
        && request.thinking != crate::chat::thinking::ThinkingLevel::Off
    {
        body["reasoning_effort"] = json!(request.thinking.as_str());
    }
    if metadata.compat.supports_store == Some(true) {
        body["store"] = json!(false);
    }
    let body =
        serde_json::to_vec(&body).map_err(|_| Error::new("cannot serialize model request"))?;
    if metadata
        .input_limits
        .max_request_bytes
        .is_some_and(|limit| body.len() as u64 > limit.get())
    {
        return Err(Error::new("model request exceeds inputLimits.maxRequestBytes; shorten the conversation or select another model"));
    }
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(120))
        .build();
    let mut call = agent
        .post(url)
        .set("Content-Type", "application/json")
        .set("Accept", "text/event-stream");
    for (name, value) in headers {
        call = call.set(name, value);
    }
    let response = match call.send_bytes(&body) {
        Ok(response) => response,
        Err(ureq::Error::Status(status, _)) => return Err(http_error(status)),
        // Transport errors can contain URLs, credentials, or upstream text.
        Err(_) => return Err(Error::new("model request network, TLS, or timeout error")),
    };
    if response.status() != 200 {
        return Err(http_error(response.status()));
    }
    if response
        .header("Content-Type")
        .and_then(|v| v.split(';').next())
        .is_none_or(|v| !v.trim().eq_ignore_ascii_case("text/event-stream"))
    {
        return Err(Error::new("model response must be text/event-stream"));
    }
    match read_stream_with_metadata(response.into_reader(), sender, metadata)? {
        StreamOutcome::Stopped => Ok(()),
        StreamOutcome::ToolCalls(calls) => {
            emit(sender, ResponseEvent::ToolCallsRequested(calls))?;
            Err(Error::new("tool execution is disabled until an authorization policy and sandbox are implemented"))
        }
    }
}

fn http_error(status: u16) -> Error {
    Error::new(format!(
        "model request failed (HTTP {status}); no automatic retry was attempted"
    ))
}

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    usage: Option<Usage>,
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Choice {
    index: u64,
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Delta {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCallDelta>>,
    function_call: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Usage {
    prompt_tokens: u64,
    completion_tokens: u64,
    prompt_tokens_details: Option<PromptDetails>,
}

#[derive(Deserialize)]
struct PromptDetails {
    cached_tokens: Option<u64>,
}

fn emit(sender: &Sender<ResponseEvent>, event: ResponseEvent) -> Result<()> {
    sender
        .send(event)
        .map_err(|_| Error::new("model response receiver closed"))
}

enum StreamOutcome {
    Stopped,
    ToolCalls(Vec<ToolCall>),
}

#[derive(Clone, Copy)]
enum FinishReason {
    Stop,
    ToolCalls,
}

#[cfg(test)]
fn read_stream(reader: impl Read, sender: &Sender<ResponseEvent>) -> Result<StreamOutcome> {
    read_stream_with_metadata(reader, sender, &crate::harness::ModelMetadata::default())
}

fn read_stream_with_metadata(
    reader: impl Read,
    sender: &Sender<ResponseEvent>,
    metadata: &crate::harness::ModelMetadata,
) -> Result<StreamOutcome> {
    let mut reader = BufReader::new(reader);
    let mut data = Vec::new();
    let mut total = 0u64;
    let mut finished = None;
    let mut tool_calls = ToolCallAssembler::default();
    loop {
        let mut line = Vec::new();
        let size = reader
            .by_ref()
            .take(FRAME_CAP + 1)
            .read_until(b'\n', &mut line)
            .map_err(|_| Error::new("model response stream interrupted"))?
            as u64;
        total += size;
        if size > FRAME_CAP || total > STREAM_CAP {
            return Err(Error::new("model response exceeded stream size limit"));
        }
        if size == 0 {
            return Err(Error::new("model response ended before [DONE]"));
        }
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        if line.is_empty() {
            if data.is_empty() {
                continue;
            }
            if data.last() == Some(&b'\n') {
                data.pop();
            }
            if data == b"[DONE]" {
                if finished.is_none() && metadata.compat.supports_finish_reason == Some(false) {
                    finished = Some(if tool_calls.is_empty() {
                        FinishReason::Stop
                    } else {
                        FinishReason::ToolCalls
                    });
                }
                return match finished {
                    Some(FinishReason::Stop) if tool_calls.is_empty() => Ok(StreamOutcome::Stopped),
                    Some(FinishReason::Stop) => Err(Error::new(
                        "model tool calls require the tool_calls finish reason",
                    )),
                    Some(FinishReason::ToolCalls) => {
                        tool_calls.finish().map(StreamOutcome::ToolCalls)
                    }
                    None => Err(Error::new("model stream completed without a finish reason")),
                };
            }
            let chunk: Chunk = serde_json::from_slice(&data)
                .map_err(|_| Error::new("invalid model response event"))?;
            data.clear();
            if chunk.error.is_some() {
                return Err(Error::new("model returned a streaming error"));
            }
            for choice in chunk.choices {
                if choice.index != 0 {
                    return Err(Error::new(
                        "multiple model response choices are not supported",
                    ));
                }
                if finished.is_some() {
                    return Err(Error::new("model sent a choice after finishing"));
                }
                if choice.delta.function_call.is_some() {
                    return Err(Error::new(
                        "legacy model function_call is not supported; use tool_calls",
                    ));
                }
                if let Some(deltas) = choice.delta.tool_calls {
                    tool_calls.extend(deltas)?;
                }
                if let Some(text) = choice.delta.content {
                    emit(sender, ResponseEvent::TextDelta(text))?;
                }
                if let Some(reason) = choice.finish_reason {
                    match reason.as_str() {
                        "stop" => finished = Some(FinishReason::Stop),
                        "tool_calls" => finished = Some(FinishReason::ToolCalls),
                        "length" => {
                            return Err(Error::new("model response truncated by its output limit"))
                        }
                        _ => {
                            return Err(Error::new(
                                "model response stopped for an unsupported finish reason",
                            ))
                        }
                    }
                }
            }
            if let Some(usage) = chunk.usage {
                emit(
                    sender,
                    ResponseEvent::Usage {
                        input_tokens: usage.prompt_tokens,
                        output_tokens: usage.completion_tokens,
                        cached_input_tokens: usage
                            .prompt_tokens_details
                            .as_ref()
                            .and_then(|v| v.cached_tokens),
                        cost_usd: metadata.usage_cost(
                            usage.prompt_tokens,
                            usage.completion_tokens,
                            usage
                                .prompt_tokens_details
                                .as_ref()
                                .and_then(|details| details.cached_tokens),
                        ),
                    },
                )?;
            }
        } else if let Some(value) = line.strip_prefix(b"data:") {
            let value = value.strip_prefix(b" ").unwrap_or(value);
            if data.len() + value.len() + 1 > FRAME_CAP as usize {
                return Err(Error::new("model response event exceeded size limit"));
            }
            data.extend_from_slice(value);
            data.push(b'\n');
        }
    }
}
