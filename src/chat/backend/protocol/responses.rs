//! OpenAI Responses: `input` item replay and output-item streaming.
//!
//! Used by the browser-login OpenAI credential and by gateways that declare
//! `api: openai-responses`. The browser-token request omits the fields that
//! endpoint rejects (Pi's `isChatGPTSignIn` boundary); gateways get them.

use std::io::Read;
use std::sync::mpsc::Sender;

use serde::Deserialize;
use serde_json::{json, Value};

use super::replay::Exchanges;
use super::{emit, usage_event, RequestContext, StreamOutcome};
use crate::chat::backend::sse::{SseError, SseReader};
use crate::chat::backend::tool_call::ToolCallAssembler;
use crate::chat::backend::tools::ToolSchema;
use crate::chat::backend::ResponseEvent;
use crate::chat::transcript::{ChatMessage, ToolPayload, TranscriptRole};
use crate::error::{Error, Result};
use crate::harness::ModelMetadata;

/// Output items one response may carry.
const MAX_SLOTS: usize = 128;
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;

pub(super) fn request(context: &RequestContext<'_>) -> Value {
    let mut body = json!({
        "model": context.model,
        "input": input(context.messages),
        "stream": true,
        "store": false
    });
    if let Some(tools) = context.tools {
        body["tools"] = Value::Array(tools.iter().map(definition).collect());
    }
    if let Some(effort) = context.reasoning_effort() {
        body["reasoning"] = json!({"effort": effort});
    }
    if !context.browser_token {
        if let Some(budget) = context.output_budget() {
            body["max_output_tokens"] = json!(budget);
        }
    }
    body
}

fn definition(schema: &ToolSchema) -> Value {
    json!({
        "type": "function",
        "name": schema.name,
        "description": schema.description,
        "parameters": schema.parameters
    })
}

/// Text history plus complete tool exchanges as `function_call` and
/// `function_call_output` items. Native reasoning items are not replayed.
fn input(messages: &[ChatMessage]) -> Vec<Value> {
    let exchanges = Exchanges::of(messages);
    let mut items = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        match message.role {
            TranscriptRole::Operator => items.push(json!({
                "role": "user",
                "content": [{"type": "input_text", "text": message.text}]
            })),
            TranscriptRole::Assistant => {
                if !message.text.is_empty() {
                    items.push(json!({"role": "assistant", "content": message.text}));
                }
                if let Some(ToolPayload::Calls(calls)) = &message.tool {
                    for (position, call) in calls.iter().enumerate() {
                        if exchanges.is_answered(index, position) {
                            items.push(json!({
                                "type": "function_call",
                                "call_id": call.id,
                                "name": call.name,
                                "arguments": Value::Object(call.arguments.clone()).to_string()
                            }));
                        }
                    }
                }
            }
            TranscriptRole::Tool { .. } if exchanges.is_paired_result(index) => {
                if let Some(ToolPayload::Result {
                    call_id, content, ..
                }) = &message.tool
                {
                    items.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": content
                    }));
                }
            }
            TranscriptRole::Tool { .. } | TranscriptRole::Notice => {}
        }
    }
    items
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Event {
    #[serde(rename = "response.created")]
    Created { response: ResponseIdentity },
    #[serde(rename = "response.output_item.added")]
    Added { output_index: usize, item: Item },
    #[serde(rename = "response.output_text.delta")]
    Text {
        output_index: usize,
        item_id: String,
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "response.function_call_arguments.delta")]
    Arguments {
        output_index: usize,
        item_id: String,
        delta: String,
    },
    #[serde(rename = "response.function_call_arguments.done")]
    ArgumentsDone {
        output_index: usize,
        item_id: String,
        arguments: String,
    },
    #[serde(rename = "response.output_item.done")]
    ItemDone { output_index: usize, item: Item },
    #[serde(rename = "response.completed")]
    Completed { response: TerminalResponse },
    #[serde(rename = "response.incomplete")]
    Incomplete { response: IncompleteResponse },
    #[serde(rename = "response.failed")]
    Failed,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "response.refusal.delta", alias = "response.refusal.done")]
    Refusal,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct ResponseIdentity {
    id: String,
}

#[derive(Deserialize)]
struct TerminalResponse {
    id: String,
    status: String,
    output: Vec<Item>,
    usage: Option<Usage>,
    error: Option<Value>,
    incomplete_details: Option<Value>,
}

#[derive(Deserialize)]
struct IncompleteResponse {
    incomplete_details: Option<IncompleteDetails>,
}
#[derive(Deserialize)]
struct IncompleteDetails {
    reason: String,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    input_tokens_details: Option<InputDetails>,
}
#[derive(Deserialize)]
struct InputDetails {
    cached_tokens: Option<u64>,
}

#[derive(Deserialize, PartialEq)]
#[serde(tag = "type")]
enum Item {
    #[serde(rename = "message")]
    Message {
        id: String,
        role: String,
        status: String,
        content: Vec<Content>,
    },
    #[serde(rename = "function_call")]
    Function {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
        status: Option<String>,
    },
    #[serde(rename = "reasoning")]
    Reasoning { id: String },
}

impl Item {
    fn id(&self) -> &str {
        match self {
            Self::Message { id, .. } | Self::Function { id, .. } | Self::Reasoning { id } => id,
        }
    }
}

#[derive(Deserialize, PartialEq)]
#[serde(tag = "type")]
enum Content {
    #[serde(rename = "output_text")]
    Text { text: String },
    #[serde(rename = "refusal")]
    Refusal,
}

enum Slot {
    Pending {
        item: Item,
        /// Streamed text per content index; allocated only for message items.
        text: Vec<String>,
        arguments: String,
        arguments_done: bool,
    },
    Finished(Item),
}

fn invalid() -> Error {
    Error::protocol("invalid or conflicting OpenAI response event")
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// Output slots indexed by `output_index`: a flat array with a hole check,
/// not a map. `MAX_SLOTS` bounds it.
#[derive(Default)]
struct Slots(Vec<Option<Slot>>);

impl Slots {
    fn insert(&mut self, index: usize, slot: Slot) -> Result<()> {
        if index >= MAX_SLOTS {
            return Err(invalid());
        }
        if self.0.len() <= index {
            self.0.resize_with(index + 1, || None);
        }
        if self.0[index].is_some() {
            return Err(invalid());
        }
        self.0[index] = Some(slot);
        Ok(())
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut Slot> {
        self.0.get_mut(index).and_then(Option::as_mut)
    }

    fn take(&mut self, index: usize) -> Option<Slot> {
        self.0.get_mut(index).and_then(Option::take)
    }

    fn put(&mut self, index: usize, slot: Slot) {
        self.0[index] = Some(slot);
    }

    /// Occupied slots, which the terminal output must match one for one.
    fn occupied(&self) -> usize {
        self.0.iter().filter(|slot| slot.is_some()).count()
    }

    fn finished(&self, index: usize) -> Option<&Item> {
        match self.0.get(index) {
            Some(Some(Slot::Finished(item))) => Some(item),
            _ => None,
        }
    }
}

pub(super) fn read_stream(
    reader: impl Read,
    sender: &Sender<ResponseEvent>,
    metadata: &ModelMetadata,
) -> Result<StreamOutcome> {
    let mut sse = SseReader::new(reader, true);
    let mut slots = Slots::default();
    // Item ids seen; at most MAX_SLOTS of them, so a linear scan is fine.
    let mut ids: Vec<String> = Vec::new();
    let mut response_id = None;
    let mut text_out = String::new();
    loop {
        let data = match sse.next_event() {
            Ok(data) => data,
            Err(SseError::Interrupted) => {
                return Err(Error::transport(
                    "OpenAI response stream interrupted; generation was not retried",
                ))
            }
            Err(SseError::TooLarge) => {
                return Err(Error::limit("OpenAI response exceeded stream size limit"))
            }
            Err(SseError::NotSse(b'<')) => {
                return Err(Error::protocol(
                    "OpenAI returned a non-SSE HTML/markup body; streaming SSE required",
                ))
            }
            Err(SseError::NotSse(_)) => {
                return Err(Error::protocol(
                    "OpenAI returned a non-SSE JSON-like body; streaming SSE required",
                ))
            }
            Err(SseError::Eof) => {
                return Err(Error::protocol(if sse.received_data() {
                    "OpenAI response ended before a terminal response event"
                } else {
                    "OpenAI response contained no SSE data events"
                }))
            }
        };
        let event: Event = serde_json::from_slice(data).map_err(|_| invalid())?;
        match event {
            Event::Created { response } => {
                if response_id.is_some() || !identifier(&response.id) {
                    return Err(invalid());
                }
                response_id = Some(response.id);
            }
            Event::Added { output_index, item } => {
                if !identifier(item.id()) || ids.iter().any(|id| id == item.id()) {
                    return Err(invalid());
                }
                let arguments = match &item {
                    Item::Function { arguments, .. } => arguments.clone(),
                    _ => String::new(),
                };
                if arguments.len() > MAX_ARGUMENT_BYTES {
                    return Err(Error::limit("model tool arguments exceed limit"));
                }
                ids.push(item.id().to_owned());
                slots.insert(
                    output_index,
                    Slot::Pending {
                        item,
                        text: Vec::new(),
                        arguments,
                        arguments_done: false,
                    },
                )?;
            }
            Event::Text {
                output_index,
                item_id,
                content_index,
                delta,
            } => {
                let Some(Slot::Pending {
                    item: Item::Message { id, .. },
                    text,
                    ..
                }) = slots.get_mut(output_index)
                else {
                    return Err(invalid());
                };
                if id != &item_id || content_index >= MAX_SLOTS {
                    return Err(invalid());
                }
                if text.len() <= content_index {
                    text.resize_with(content_index + 1, String::new);
                }
                text[content_index].push_str(&delta);
                text_out.push_str(&delta);
                emit(sender, ResponseEvent::TextDelta(delta))?;
            }
            Event::Arguments {
                output_index,
                item_id,
                delta,
            } => {
                let Some(Slot::Pending {
                    item: Item::Function { id, .. },
                    arguments,
                    arguments_done: false,
                    ..
                }) = slots.get_mut(output_index)
                else {
                    return Err(invalid());
                };
                if id != &item_id {
                    return Err(invalid());
                }
                if arguments.len() + delta.len() > MAX_ARGUMENT_BYTES {
                    return Err(Error::limit("model tool arguments exceed limit"));
                }
                arguments.push_str(&delta);
            }
            Event::ArgumentsDone {
                output_index,
                item_id,
                arguments: final_arguments,
            } => {
                let Some(Slot::Pending {
                    item: Item::Function { id, .. },
                    arguments,
                    arguments_done,
                    ..
                }) = slots.get_mut(output_index)
                else {
                    return Err(invalid());
                };
                if id != &item_id
                    || *arguments_done
                    || !final_arguments.starts_with(arguments.as_str())
                    || final_arguments.len() > MAX_ARGUMENT_BYTES
                {
                    return Err(invalid());
                }
                *arguments = final_arguments;
                *arguments_done = true;
            }
            Event::ItemDone {
                output_index,
                item: final_item,
            } => {
                let Some(Slot::Pending {
                    item,
                    text,
                    arguments,
                    arguments_done,
                }) = slots.take(output_index)
                else {
                    return Err(invalid());
                };
                match (&item, &final_item) {
                    (
                        Item::Message { id, role, .. },
                        Item::Message {
                            id: final_id,
                            role: final_role,
                            status,
                            content,
                        },
                    ) if id == final_id
                        && role == "assistant"
                        && final_role == role
                        && status == "completed" =>
                    {
                        if text.len() > content.len() {
                            return Err(invalid());
                        }
                        for (index, part) in content.iter().enumerate() {
                            match part {
                                Content::Refusal => {
                                    return Err(Error::protocol("OpenAI refused the response"))
                                }
                                Content::Text { text: final_text } => {
                                    match text.get(index).filter(|streamed| !streamed.is_empty()) {
                                        Some(streamed) if streamed != final_text => {
                                            return Err(invalid())
                                        }
                                        Some(_) => {}
                                        None => {
                                            text_out.push_str(final_text);
                                            emit(
                                                sender,
                                                ResponseEvent::TextDelta(final_text.clone()),
                                            )?;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    (
                        Item::Function {
                            id, call_id, name, ..
                        },
                        Item::Function {
                            id: final_id,
                            call_id: final_call_id,
                            name: final_name,
                            arguments: final_arguments,
                            status,
                        },
                    ) if id == final_id && call_id == final_call_id && name == final_name => {
                        if status
                            .as_deref()
                            .is_some_and(|status| status != "completed")
                            || final_arguments.len() > MAX_ARGUMENT_BYTES
                            || !final_arguments.starts_with(&arguments)
                            || (arguments_done && final_arguments != &arguments)
                        {
                            return Err(invalid());
                        }
                    }
                    (Item::Reasoning { id }, Item::Reasoning { id: final_id })
                        if id == final_id => {}
                    _ => return Err(invalid()),
                }
                slots.put(output_index, Slot::Finished(final_item));
            }
            Event::Completed { response } => {
                if response.status != "completed"
                    || response.error.is_some()
                    || response.incomplete_details.is_some()
                    || !identifier(&response.id)
                    || response_id.as_ref().is_some_and(|id| id != &response.id)
                    || response.output.len() != slots.occupied()
                {
                    return Err(invalid());
                }
                let mut calls = ToolCallAssembler::default();
                let mut call_index = 0;
                for (index, item) in response.output.into_iter().enumerate() {
                    if slots.finished(index) != Some(&item) {
                        return Err(Error::protocol(
                            "OpenAI completed with unfinished or conflicting output items",
                        ));
                    }
                    if let Item::Function {
                        call_id,
                        name,
                        arguments,
                        ..
                    } = item
                    {
                        calls.push_function(call_index, call_id, name, arguments)?;
                        call_index += 1;
                    }
                }
                if let Some(usage) = response.usage {
                    let cached = usage
                        .input_tokens_details
                        .and_then(|details| details.cached_tokens);
                    if cached.is_some_and(|count| count > usage.input_tokens) {
                        return Err(invalid());
                    }
                    emit(
                        sender,
                        usage_event(metadata, usage.input_tokens, usage.output_tokens, cached),
                    )?;
                }
                let outcome = if calls.is_empty() {
                    StreamOutcome::Stopped
                } else {
                    StreamOutcome::ToolCalls {
                        text: text_out,
                        calls: calls.finish()?,
                    }
                };
                sse.drain();
                return Ok(outcome);
            }
            Event::Incomplete { response } => {
                return Err(
                    if response
                        .incomplete_details
                        .is_some_and(|details| details.reason == "max_output_tokens")
                    {
                        Error::limit("OpenAI response truncated by its output limit")
                    } else {
                        Error::protocol("OpenAI response incomplete")
                    },
                )
            }
            Event::Failed | Event::Error => {
                return Err(Error::protocol("OpenAI returned a streaming failure"))
            }
            Event::Refusal => return Err(Error::protocol("OpenAI refused the response")),
            Event::Other => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::backend::tools::call;

    #[test]
    fn complete_exchanges_replay_as_function_call_items() {
        let mut assistant = ChatMessage::new(TranscriptRole::Assistant, "checking");
        assistant.tool = Some(ToolPayload::Calls(vec![
            call("call_a", "read_file", json!({"path": "a"})),
            call("call_b", "read_file", json!({"path": "b"})),
        ]));
        let mut result = ChatMessage::new(TranscriptRole::Tool { exit_code: 0 }, "preview");
        result.tool = Some(ToolPayload::Result {
            call_id: "call_a".into(),
            name: "read_file".into(),
            content: "fn a() {}".into(),
        });
        let items = input(&[
            ChatMessage::new(TranscriptRole::Operator, "q"),
            assistant,
            result,
            ChatMessage::new(TranscriptRole::Notice, "local"),
            ChatMessage::new(TranscriptRole::Operator, "next"),
        ]);
        assert_eq!(
            items,
            vec![
                json!({"role": "user", "content": [{"type": "input_text", "text": "q"}]}),
                json!({"role": "assistant", "content": "checking"}),
                json!({"type": "function_call", "call_id": "call_a", "name": "read_file", "arguments": "{\"path\":\"a\"}"}),
                json!({"type": "function_call_output", "call_id": "call_a", "output": "fn a() {}"}),
                json!({"role": "user", "content": [{"type": "input_text", "text": "next"}]}),
            ]
        );
    }
}
