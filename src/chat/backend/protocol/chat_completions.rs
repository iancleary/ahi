//! OpenAI Chat Completions: `messages` replay and `choices[0].delta` streaming.

use std::io::Read;
use std::sync::mpsc::Sender;

use serde::Deserialize;
use serde_json::{json, Value};

use super::replay::Exchanges;
use super::{emit, usage_event, RequestContext, StreamOutcome};
use crate::chat::backend::sse::{SseError, SseReader};
use crate::chat::backend::tool_call::{ToolCallAssembler, ToolCallDelta};
use crate::chat::backend::tools::ToolSchema;
use crate::chat::transcript::{ChatMessage, ToolPayload, TranscriptRole};
use crate::error::{Error, Result};
use crate::harness::{CompletionCompatibility, ModelMetadata};

pub(super) fn request(context: &RequestContext<'_>) -> Value {
    let metadata = context.metadata;
    let mut body = json!({
        "model": context.model,
        "messages": messages(context.messages, &metadata.compat),
        "stream": true,
        "stream_options": {"include_usage": true}
    });
    if let Some(tools) = context.tools {
        body["tools"] = Value::Array(tools.iter().map(definition).collect());
    }
    if metadata.compat.supports_usage_in_streaming == Some(false) {
        body.as_object_mut()
            .expect("request object")
            .remove("stream_options");
    }
    if let Some(budget) = context.output_budget() {
        let field = metadata
            .compat
            .max_tokens_field
            .map(|field| field.field())
            .unwrap_or("max_tokens");
        body[field] = json!(budget);
    }
    if let Some(effort) = context.reasoning_effort() {
        body["reasoning_effort"] = json!(effort);
    }
    if metadata.compat.supports_store == Some(true) {
        body["store"] = json!(false);
    }
    body
}

/// The `tools[]` entry for one builtin.
pub(crate) fn definition(schema: &ToolSchema) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": schema.name,
            "description": schema.description,
            "parameters": schema.parameters
        }
    })
}

pub(super) fn messages(messages: &[ChatMessage], compat: &CompletionCompatibility) -> Vec<Value> {
    let exchanges = Exchanges::of(messages);
    let mut output = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        match message.role {
            TranscriptRole::Operator => {
                // Some servers reject a user message directly after tool
                // results; the bridging text matches Pi's compatibility shim.
                if compat.requires_assistant_after_tool_result == Some(true)
                    && output
                        .last()
                        .is_some_and(|last: &Value| last["role"] == "tool")
                {
                    output.push(json!({
                        "role": "assistant",
                        "content": "I have processed the tool results."
                    }));
                }
                output.push(json!({"role": "user", "content": message.text}));
            }
            TranscriptRole::Assistant => {
                let calls: Vec<Value> = match &message.tool {
                    Some(ToolPayload::Calls { calls, .. }) => calls
                        .iter()
                        .enumerate()
                        .filter(|(position, _)| exchanges.is_answered(index, *position))
                        .map(|(_, call)| {
                            json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": Value::Object(call.arguments.clone()).to_string()
                                }
                            })
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                if message.text.is_empty() && calls.is_empty() {
                    continue;
                }
                let mut value = json!({"role": "assistant", "content": message.text});
                if !calls.is_empty() {
                    if message.text.is_empty() {
                        value["content"] = Value::Null;
                    }
                    value["tool_calls"] = Value::Array(calls);
                }
                if compat.requires_reasoning_content_on_assistant_messages == Some(true) {
                    value["reasoning_content"] = json!("");
                }
                output.push(value);
            }
            TranscriptRole::Tool { .. } if exchanges.is_paired_result(index) => {
                if let Some(ToolPayload::Result {
                    call_id,
                    name,
                    content,
                }) = &message.tool
                {
                    let mut value =
                        json!({"role": "tool", "tool_call_id": call_id, "content": content});
                    if compat.requires_tool_result_name == Some(true) {
                        value["name"] = json!(name);
                    }
                    output.push(value);
                }
            }
            TranscriptRole::Tool { .. } | TranscriptRole::Notice => {}
        }
    }
    output
}

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    usage: Option<Usage>,
    error: Option<Value>,
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
    function_call: Option<Value>,
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

#[derive(Clone, Copy)]
enum FinishReason {
    Stop,
    ToolCalls,
}

pub(super) fn read_stream(
    reader: impl Read + Send + 'static,
    sender: &Sender<crate::chat::backend::ResponseEvent>,
    metadata: &ModelMetadata,
) -> Result<StreamOutcome> {
    let mut sse = SseReader::new(reader, false);
    let mut finished = None;
    let mut tool_calls = ToolCallAssembler::default();
    let mut text = String::new();
    loop {
        let data = match sse.next_event() {
            Ok(data) => data,
            Err(SseError::Interrupted) => {
                return Err(Error::transport("model response stream interrupted"))
            }
            Err(SseError::TooLarge) => {
                return Err(Error::limit("model response exceeded stream size limit"))
            }
            Err(SseError::Eof | SseError::NotSse(_)) => {
                return Err(Error::protocol("model response ended before [DONE]"))
            }
        };
        if data == b"[DONE]" {
            if finished.is_none() && metadata.compat.supports_finish_reason == Some(false) {
                finished = Some(if tool_calls.is_empty() {
                    FinishReason::Stop
                } else {
                    FinishReason::ToolCalls
                });
            }
            let outcome = match finished {
                Some(FinishReason::Stop) if tool_calls.is_empty() => Ok(StreamOutcome::Stopped),
                Some(FinishReason::Stop) => Err(Error::protocol(
                    "model tool calls require the tool_calls finish reason",
                )),
                Some(FinishReason::ToolCalls) => {
                    tool_calls.finish().map(|calls| StreamOutcome::ToolCalls {
                        text,
                        calls,
                        native: None,
                    })
                }
                None => Err(Error::protocol(
                    "model stream completed without a finish reason",
                )),
            };
            if outcome.is_ok() {
                sse.drain();
            }
            return outcome;
        }
        let chunk: Chunk = serde_json::from_slice(data)
            .map_err(|_| Error::protocol("invalid model response event"))?;
        if chunk.error.is_some() {
            return Err(Error::protocol("model returned a streaming error"));
        }
        for choice in chunk.choices {
            if choice.index != 0 {
                return Err(Error::protocol(
                    "multiple model response choices are not supported",
                ));
            }
            if finished.is_some() {
                return Err(Error::protocol("model sent a choice after finishing"));
            }
            if choice.delta.function_call.is_some() {
                return Err(Error::protocol(
                    "legacy model function_call is not supported; use tool_calls",
                ));
            }
            if let Some(deltas) = choice.delta.tool_calls {
                tool_calls.extend(deltas)?;
            }
            if let Some(content) = choice.delta.content {
                text.push_str(&content);
                emit(
                    sender,
                    crate::chat::backend::ResponseEvent::TextDelta(content),
                )?;
            }
            if let Some(reason) = choice.finish_reason {
                match reason.as_str() {
                    "stop" => finished = Some(FinishReason::Stop),
                    "tool_calls" => finished = Some(FinishReason::ToolCalls),
                    "length" => {
                        return Err(Error::limit("model response truncated by its output limit"))
                    }
                    _ => {
                        return Err(Error::protocol(
                            "model response stopped for an unsupported finish reason",
                        ))
                    }
                }
            }
        }
        if let Some(usage) = chunk.usage {
            emit(
                sender,
                usage_event(
                    metadata,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    usage
                        .prompt_tokens_details
                        .as_ref()
                        .and_then(|details| details.cached_tokens),
                ),
            )?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::backend::tools::call;

    fn assistant(text: &str, ids: &[&str]) -> ChatMessage {
        let mut message = ChatMessage::new(TranscriptRole::Assistant, text);
        if !ids.is_empty() {
            message.tool = Some(ToolPayload::Calls {
                calls: ids
                    .iter()
                    .map(|id| call(id, "read_file", json!({"path": "a"})))
                    .collect(),
                native: None,
            });
        }
        message
    }

    fn result(id: &str) -> ChatMessage {
        let mut message = ChatMessage::new(TranscriptRole::Tool { exit_code: 0 }, "preview");
        message.tool = Some(ToolPayload::Result {
            call_id: id.into(),
            name: "read_file".into(),
            content: format!("content {id}"),
        });
        message
    }

    #[test]
    fn a_bridge_follows_tool_results_only_when_required() {
        let transcript = vec![
            ChatMessage::new(TranscriptRole::Operator, "q"),
            assistant("", &["a"]),
            result("a"),
            ChatMessage::new(TranscriptRole::Operator, "next"),
        ];
        let roles = |compat: &CompletionCompatibility| {
            messages(&transcript, compat)
                .into_iter()
                .map(|value| value["role"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            roles(&CompletionCompatibility::default()),
            ["user", "assistant", "tool", "user"]
        );
        let compat = CompletionCompatibility {
            requires_assistant_after_tool_result: Some(true),
            ..Default::default()
        };
        assert_eq!(
            roles(&compat),
            ["user", "assistant", "tool", "assistant", "user"]
        );
    }

    #[test]
    fn complete_exchanges_replay_and_orphans_are_dropped() {
        let transcript = vec![
            ChatMessage::new(TranscriptRole::Operator, "q"),
            assistant("", &["a", "b"]),
            result("a"),
            ChatMessage::new(TranscriptRole::Notice, "local"),
            result("unknown"),
            assistant("answer", &[]),
            result("b"), // after another assistant message: unpaired
            assistant("partial", &["c"]),
            ChatMessage::new(TranscriptRole::Tool { exit_code: 0 }, "legacy"),
        ];
        let compat = CompletionCompatibility {
            requires_tool_result_name: Some(true),
            ..Default::default()
        };
        assert_eq!(
            messages(&transcript, &compat),
            vec![
                json!({"role": "user", "content": "q"}),
                json!({"role": "assistant", "content": null, "tool_calls": [
                    {"id": "a", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"a\"}"}}
                ]}),
                json!({"role": "tool", "tool_call_id": "a", "content": "content a", "name": "read_file"}),
                json!({"role": "assistant", "content": "answer"}),
                json!({"role": "assistant", "content": "partial"}),
            ]
        );
    }
}
