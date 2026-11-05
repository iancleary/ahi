//! Anthropic Messages: content-block replay and `content_block_*` streaming.
//!
//! Authentication is the caller's concern (an `x-api-key` header resolved by
//! the harness). This module never adds identity headers or a system prompt.

use std::io::Read;
use std::sync::mpsc::Sender;

use serde::Deserialize;
use serde_json::{json, Value};

use super::replay::Exchanges;
use super::{emit, usage_event, RequestContext, StreamOutcome, DEFAULT_OUTPUT_BUDGET};
use crate::chat::backend::sse::{SseError, SseReader};
use crate::chat::backend::tool_call::ToolCallAssembler;
use crate::chat::backend::tools::ToolSchema;
use crate::chat::backend::ResponseEvent;
use crate::chat::thinking::ThinkingLevel;
use crate::chat::transcript::{ChatMessage, ToolPayload, TranscriptRole};
use crate::error::{Error, Result};
use crate::harness::ModelMetadata;

/// Content blocks one message may carry.
const MAX_BLOCKS: usize = 128;
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
/// Anthropic requires at least this much budget for extended thinking, and
/// the output budget must leave room for the answer after it.
const MIN_THINKING_BUDGET: u64 = 1024;

pub(super) fn request(context: &RequestContext<'_>) -> Result<Value> {
    let max_tokens = context.output_budget().unwrap_or(DEFAULT_OUTPUT_BUDGET);
    let mut body = json!({
        "model": context.model,
        "max_tokens": max_tokens,
        "stream": true,
        "messages": messages(context.messages),
    });
    if let Some(tools) = context.tools {
        body["tools"] = Value::Array(tools.iter().map(definition).collect());
    }
    if context.reasoning_effort().is_some() {
        let wanted = match context.thinking {
            ThinkingLevel::Off => 0,
            ThinkingLevel::Minimal => 1024,
            ThinkingLevel::Low => 2048,
            ThinkingLevel::Medium => 4096,
            ThinkingLevel::High => 8192,
        };
        let budget = wanted.min(max_tokens.saturating_sub(MIN_THINKING_BUDGET));
        if budget >= MIN_THINKING_BUDGET {
            body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
        }
    }
    Ok(body)
}

fn definition(schema: &ToolSchema) -> Value {
    json!({
        "name": schema.name,
        "description": schema.description,
        "input_schema": schema.parameters
    })
}

/// User text, assistant text plus `tool_use` blocks, and `tool_result` blocks
/// in a user message. Consecutive results are batched into one user message.
fn messages(messages: &[ChatMessage]) -> Vec<Value> {
    let exchanges = Exchanges::of(messages);
    let mut output: Vec<Value> = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        match message.role {
            TranscriptRole::Operator => output.push(json!({
                "role": "user",
                "content": [{"type": "text", "text": message.text}]
            })),
            TranscriptRole::Assistant => {
                let mut content = Vec::new();
                if !message.text.is_empty() {
                    content.push(json!({"type": "text", "text": message.text}));
                }
                if let Some(ToolPayload::Calls(calls)) = &message.tool {
                    for (position, call) in calls.iter().enumerate() {
                        if exchanges.is_answered(index, position) {
                            content.push(json!({
                                "type": "tool_use",
                                "id": call.id,
                                "name": call.name,
                                "input": Value::Object(call.arguments.clone())
                            }));
                        }
                    }
                }
                if !content.is_empty() {
                    output.push(json!({"role": "assistant", "content": content}));
                }
            }
            TranscriptRole::Tool { exit_code } if exchanges.is_paired_result(index) => {
                if let Some(ToolPayload::Result {
                    call_id, content, ..
                }) = &message.tool
                {
                    let block = json!({
                        "type": "tool_result",
                        "tool_use_id": call_id,
                        "content": content,
                        "is_error": exit_code != 0
                    });
                    match output.last_mut() {
                        Some(last)
                            if last["role"] == "user"
                                && last["content"][0]["type"] == "tool_result" =>
                        {
                            last["content"]
                                .as_array_mut()
                                .expect("content array")
                                .push(block);
                        }
                        _ => output.push(json!({"role": "user", "content": [block]})),
                    }
                }
            }
            TranscriptRole::Tool { .. } | TranscriptRole::Notice => {}
        }
    }
    output
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Event {
    #[serde(rename = "message_start")]
    MessageStart { message: MessageStart },
    #[serde(rename = "content_block_start")]
    BlockStart {
        index: usize,
        content_block: BlockStart,
    },
    #[serde(rename = "content_block_delta")]
    BlockDelta { index: usize, delta: Delta },
    #[serde(rename = "content_block_stop")]
    BlockStop { index: usize },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: MessageDelta,
        usage: Option<DeltaUsage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "error")]
    Error,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct MessageStart {
    id: String,
    role: String,
    usage: Option<StartUsage>,
}

#[derive(Deserialize)]
struct StartUsage {
    input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum BlockStart {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String },
    #[serde(rename = "thinking")]
    Thinking,
    #[serde(rename = "redacted_thinking")]
    RedactedThinking,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Delta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
    #[serde(rename = "thinking_delta")]
    Thinking,
    #[serde(rename = "signature_delta")]
    Signature,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct MessageDelta {
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct DeltaUsage {
    output_tokens: u64,
}

/// One content block while it streams. Tool input is kept as text until the
/// whole message completes; a prefix that happens to parse is never used.
enum Block {
    Text,
    ToolUse {
        id: String,
        name: String,
        input: String,
    },
    Thinking,
}

struct Slot {
    block: Block,
    open: bool,
}

fn invalid() -> Error {
    Error::protocol("invalid or conflicting Anthropic message event")
}

pub(super) fn read_stream(
    reader: impl Read,
    sender: &Sender<ResponseEvent>,
    metadata: &ModelMetadata,
) -> Result<StreamOutcome> {
    let mut sse = SseReader::new(reader, true);
    let mut slots: Vec<Option<Slot>> = Vec::new();
    let mut started = false;
    let mut input_tokens = None;
    let mut cached = None;
    let mut output_tokens = None;
    let mut stop_reason: Option<String> = None;
    let mut text_out = String::new();
    loop {
        let data = match sse.next_event() {
            Ok(data) => data,
            Err(SseError::Interrupted) => {
                return Err(Error::transport(
                    "Anthropic response stream interrupted; generation was not retried",
                ))
            }
            Err(SseError::TooLarge) => {
                return Err(Error::limit(
                    "Anthropic response exceeded stream size limit",
                ))
            }
            Err(SseError::NotSse(_)) => {
                return Err(Error::protocol(
                    "Anthropic returned a non-SSE body; streaming SSE required",
                ))
            }
            Err(SseError::Eof) => {
                return Err(Error::protocol(if sse.received_data() {
                    "Anthropic response ended before message_stop"
                } else {
                    "Anthropic response contained no SSE data events"
                }))
            }
        };
        let event: Event = serde_json::from_slice(data).map_err(|_| invalid())?;
        match event {
            Event::MessageStart { message } => {
                if started || message.role != "assistant" || message.id.is_empty() {
                    return Err(invalid());
                }
                started = true;
                if let Some(usage) = message.usage {
                    // Anthropic reports uncached, cache-read, and cache-write
                    // input separately; the meter wants the total with the
                    // cached part as a subset.
                    let read = usage.cache_read_input_tokens.unwrap_or(0);
                    let write = usage.cache_creation_input_tokens.unwrap_or(0);
                    input_tokens = Some(
                        usage
                            .input_tokens
                            .saturating_add(read)
                            .saturating_add(write),
                    );
                    cached = usage.cache_read_input_tokens;
                }
            }
            Event::BlockStart {
                index,
                content_block,
            } => {
                if !started || index >= MAX_BLOCKS {
                    return Err(invalid());
                }
                if slots.len() <= index {
                    slots.resize_with(index + 1, || None);
                }
                if slots[index].is_some() {
                    return Err(invalid());
                }
                let block = match content_block {
                    BlockStart::Text => Block::Text,
                    BlockStart::ToolUse { id, name } => Block::ToolUse {
                        id,
                        name,
                        input: String::new(),
                    },
                    BlockStart::Thinking | BlockStart::RedactedThinking => Block::Thinking,
                    BlockStart::Other => {
                        return Err(Error::protocol(
                            "Anthropic sent an unsupported content block type",
                        ))
                    }
                };
                slots[index] = Some(Slot { block, open: true });
            }
            Event::BlockDelta { index, delta } => {
                let Some(Some(Slot { block, open: true })) = slots.get_mut(index) else {
                    return Err(invalid());
                };
                match (block, delta) {
                    (Block::Text, Delta::Text { text }) => {
                        text_out.push_str(&text);
                        emit(sender, ResponseEvent::TextDelta(text))?;
                    }
                    (Block::ToolUse { input, .. }, Delta::InputJson { partial_json }) => {
                        if input.len() + partial_json.len() > MAX_ARGUMENT_BYTES {
                            return Err(Error::limit("model tool arguments exceed limit"));
                        }
                        input.push_str(&partial_json);
                    }
                    (Block::Thinking, Delta::Thinking | Delta::Signature) => {}
                    _ => return Err(invalid()),
                }
            }
            Event::BlockStop { index } => {
                let Some(Some(slot)) = slots.get_mut(index) else {
                    return Err(invalid());
                };
                if !slot.open {
                    return Err(invalid());
                }
                slot.open = false;
            }
            Event::MessageDelta { delta, usage } => {
                if !started || stop_reason.is_some() {
                    return Err(invalid());
                }
                stop_reason = delta.stop_reason;
                if let Some(usage) = usage {
                    output_tokens = Some(usage.output_tokens);
                }
            }
            Event::MessageStop => {
                if !started || slots.iter().flatten().any(|slot| slot.open) {
                    return Err(invalid());
                }
                if let (Some(input), Some(output)) = (input_tokens, output_tokens) {
                    emit(sender, usage_event(metadata, input, output, cached))?;
                }
                let mut calls = ToolCallAssembler::default();
                let mut call_index = 0;
                for slot in slots.iter_mut().flatten() {
                    if let Block::ToolUse { id, name, input } = &mut slot.block {
                        let input = if input.is_empty() {
                            // An argument-less call streams no input deltas.
                            "{}".to_string()
                        } else {
                            std::mem::take(input)
                        };
                        calls.push_function(
                            call_index,
                            std::mem::take(id),
                            std::mem::take(name),
                            input,
                        )?;
                        call_index += 1;
                    }
                }
                let outcome = match stop_reason.as_deref() {
                    Some("end_turn" | "stop_sequence" | "pause_turn") if calls.is_empty() => {
                        StreamOutcome::Stopped
                    }
                    Some("end_turn" | "stop_sequence" | "pause_turn") => {
                        return Err(Error::protocol(
                            "model tool calls require the tool_use stop reason",
                        ))
                    }
                    Some("tool_use") if !calls.is_empty() => StreamOutcome::ToolCalls {
                        text: text_out,
                        calls: calls.finish()?,
                    },
                    Some("tool_use") => {
                        return Err(Error::protocol(
                            "model stopped for tool use without any tool_use block",
                        ))
                    }
                    Some("max_tokens") => {
                        return Err(Error::limit(
                            "Anthropic response truncated by its output limit",
                        ))
                    }
                    Some("refusal" | "sensitive") => {
                        return Err(Error::protocol("Anthropic refused the response"))
                    }
                    Some(_) => {
                        return Err(Error::protocol(
                            "model response stopped for an unsupported stop reason",
                        ))
                    }
                    None => {
                        return Err(Error::protocol(
                            "Anthropic message completed without a stop reason",
                        ))
                    }
                };
                sse.drain();
                return Ok(outcome);
            }
            Event::Error => return Err(Error::protocol("Anthropic returned a streaming error")),
            Event::Other => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::backend::tools::call;
    use std::sync::mpsc;

    fn events(body: &[Value]) -> String {
        body.iter()
            .map(|event| {
                format!(
                    "event: {}\ndata: {event}\n\n",
                    event["type"].as_str().unwrap()
                )
            })
            .collect()
    }

    fn run(body: &str) -> (Result<StreamOutcome>, Vec<ResponseEvent>) {
        let (sender, receiver) = mpsc::channel();
        let result = read_stream(body.as_bytes(), &sender, &ModelMetadata::default());
        (result, receiver.try_iter().collect())
    }

    #[test]
    fn text_streams_and_usage_totals_cached_input() {
        let body = events(&[
            json!({"type":"message_start","message":{"id":"msg_1","role":"assistant","usage":{"input_tokens":10,"cache_read_input_tokens":30,"cache_creation_input_tokens":5}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"ping"}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}),
            json!({"type":"message_stop"}),
        ]);
        let (result, events) = run(&body);
        assert_eq!(result.unwrap(), StreamOutcome::Stopped);
        assert_eq!(
            events,
            vec![
                ResponseEvent::TextDelta("Hel".into()),
                ResponseEvent::TextDelta("lo".into()),
                ResponseEvent::Usage {
                    input_tokens: 45,
                    output_tokens: 4,
                    cached_input_tokens: Some(30),
                    cost_usd: None
                },
            ]
        );
    }

    #[test]
    fn tool_use_blocks_assemble_after_message_stop_only() {
        let body = events(&[
            json!({"type":"message_start","message":{"id":"msg_1","role":"assistant"}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Reading."}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01A","name":"read_file","input":{}}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\": \"src/"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"a.rs\"}"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_01B","name":"list_dir","input":{}}}),
            json!({"type":"content_block_stop","index":2}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}),
            json!({"type":"message_stop"}),
        ]);
        let (result, events) = run(&body);
        let StreamOutcome::ToolCalls { text, calls } = result.unwrap() else {
            panic!("expected tool calls");
        };
        assert_eq!(text, "Reading.");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "toolu_01A");
        assert_eq!(calls[0].arguments["path"], "src/a.rs");
        assert_eq!(calls[1].name, "list_dir");
        assert!(calls[1].arguments.is_empty());
        assert!(events
            .iter()
            .all(|event| matches!(event, ResponseEvent::TextDelta(_))));
    }

    #[test]
    fn malformed_streams_fail_closed() {
        let start = json!({"type":"message_start","message":{"id":"msg_1","role":"assistant"}});
        let text_block = json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}});
        for (events_, expected) in [
            (
                vec![start.clone(), text_block.clone()],
                "ended before message_stop",
            ),
            (
                vec![
                    start.clone(),
                    text_block.clone(),
                    json!({"type":"message_stop"}),
                ],
                "invalid or conflicting",
            ),
            (
                vec![
                    start.clone(),
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}),
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"[\"private\"]"}}),
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}),
                    json!({"type":"message_stop"}),
                ],
                "must be a JSON object",
            ),
            (
                vec![
                    start.clone(),
                    json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":1}}),
                    json!({"type":"message_stop"}),
                ],
                "truncated",
            ),
            (
                vec![
                    start.clone(),
                    json!({"type":"error","error":{"type":"overloaded_error","message":"private"}}),
                ],
                "streaming error",
            ),
        ] {
            let (result, _) = run(&events(&events_));
            let message = result.unwrap_err().to_string();
            assert!(message.contains(expected), "{message}");
            assert!(!message.contains("private"));
        }
        assert!(run("{\"error\":1}\n")
            .0
            .unwrap_err()
            .to_string()
            .contains("non-SSE"));
    }

    #[test]
    fn replay_batches_tool_results_into_one_user_message() {
        let mut assistant = ChatMessage::new(TranscriptRole::Assistant, "");
        assistant.tool = Some(ToolPayload::Calls(vec![
            call("toolu_a", "read_file", json!({"path": "a"})),
            call("toolu_b", "read_file", json!({"path": "b"})),
        ]));
        let result = |id: &str, code: i32| {
            let mut message = ChatMessage::new(TranscriptRole::Tool { exit_code: code }, "");
            message.tool = Some(ToolPayload::Result {
                call_id: id.into(),
                name: "read_file".into(),
                content: format!("content {id}"),
            });
            message
        };
        let replayed = messages(&[
            ChatMessage::new(TranscriptRole::Operator, "q"),
            assistant,
            result("toolu_a", 0),
            result("toolu_b", 126),
            ChatMessage::new(TranscriptRole::Assistant, "done"),
        ]);
        assert_eq!(
            replayed,
            vec![
                json!({"role": "user", "content": [{"type": "text", "text": "q"}]}),
                json!({"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_a", "name": "read_file", "input": {"path": "a"}},
                    {"type": "tool_use", "id": "toolu_b", "name": "read_file", "input": {"path": "b"}}
                ]}),
                json!({"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_a", "content": "content toolu_a", "is_error": false},
                    {"type": "tool_result", "tool_use_id": "toolu_b", "content": "content toolu_b", "is_error": true}
                ]}),
                json!({"role": "assistant", "content": [{"type": "text", "text": "done"}]}),
            ]
        );
    }

    #[test]
    fn request_sends_max_tokens_and_bounded_thinking() {
        let metadata = ModelMetadata {
            max_tokens: Some(std::num::NonZeroU64::new(3000).unwrap()),
            reasoning: Some(true),
            compat: crate::harness::CompletionCompatibility {
                supports_reasoning_effort: Some(true),
                ..Default::default()
            },
            ..Default::default()
        };
        let messages = [ChatMessage::new(TranscriptRole::Operator, "q")];
        let schemas = crate::chat::backend::tools::schemas(&["read_file".to_string()]);
        let context = RequestContext {
            model: "claude-x",
            thinking: ThinkingLevel::High,
            metadata: &metadata,
            messages: &messages,
            tools: Some(&schemas),
            browser_token: false,
        };
        let body = request(&context).unwrap();
        assert_eq!(body["max_tokens"], 3000);
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": 1976})
        );
        assert_eq!(body["tools"][0]["name"], "read_file");
        assert!(body["tools"][0]["input_schema"].is_object());
        assert!(body.get("system").is_none());
        let context = RequestContext {
            thinking: ThinkingLevel::Off,
            metadata: &ModelMetadata::default(),
            ..context
        };
        let body = request(&context).unwrap();
        assert_eq!(body["max_tokens"], 4096);
        assert!(body.get("thinking").is_none());
    }
}
