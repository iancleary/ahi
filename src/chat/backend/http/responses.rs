use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::sync::mpsc::Sender;

use serde::Deserialize;
use serde_json::json;

use super::{emit, ChatRequest, ResponseEvent, FRAME_CAP, STREAM_CAP};
use crate::chat::backend::tool_call::ToolCallAssembler;
use crate::chat::transcript::TranscriptRole;
use crate::error::{Error, Result};
use crate::harness::ModelMetadata;

const REQUEST_CAP: u64 = 16 * 1024 * 1024;

pub(super) fn request_body(request: &ChatRequest, metadata: &ModelMetadata) -> Result<Vec<u8>> {
    let input: Vec<_> = request
        .messages
        .iter()
        .filter_map(|message| match message.role {
            TranscriptRole::Operator => Some(
                json!({"role": "user", "content": [{"type": "input_text", "text": message.text}]}),
            ),
            TranscriptRole::Assistant => {
                Some(json!({"role": "assistant", "content": message.text}))
            }
            TranscriptRole::Notice | TranscriptRole::Tool { .. } => None,
        })
        .collect();
    let mut body =
        json!({"model": request.selection.model, "input": input, "stream": true, "store": false});
    if metadata.reasoning == Some(true)
        && metadata.compat.supports_reasoning_effort == Some(true)
        && request.thinking != crate::chat::thinking::ThinkingLevel::Off
    {
        body["reasoning"] = json!({"effort": request.thinking.as_str()});
    }
    let body =
        serde_json::to_vec(&body).map_err(|_| Error::new("cannot serialize OpenAI request"))?;
    if body.len() as u64 > REQUEST_CAP {
        return Err(Error::new(
            "OpenAI request body exceeds 16 MiB; shorten the conversation",
        ));
    }
    if metadata
        .input_limits
        .max_request_bytes
        .is_some_and(|limit| body.len() as u64 > limit.get())
    {
        return Err(Error::new("model request exceeds inputLimits.maxRequestBytes; shorten the conversation or select another model"));
    }
    Ok(body)
}

pub(super) fn complete(
    state: &Path,
    request: &ChatRequest,
    sender: &Sender<ResponseEvent>,
    metadata: &ModelMetadata,
) -> Result<()> {
    let body = request_body(request, metadata)?;
    let response = crate::harness::inference_response(state, &body)?;
    consume(response, sender, metadata)
}

pub(super) fn consume(
    response: ureq::Response,
    sender: &Sender<ResponseEvent>,
    metadata: &ModelMetadata,
) -> Result<()> {
    let status = response.status();
    let media_type = media_type_category(response.header("Content-Type"));
    if status != 200 {
        return Err(Error::new(format!(
            "OpenAI inference failed (HTTP {status}; Content-Type: {media_type})"
        )));
    }
    if let Some(window) = metadata.context_window {
        emit(sender, ResponseEvent::ContextWindow(window.get()))?;
    }
    read_stream(response.into_reader(), sender, metadata).map_err(|error| {
        Error::new(format!(
            "{error} (HTTP {status}; Content-Type: {media_type})"
        ))
    })
}

fn media_type_category(header: Option<&str>) -> &'static str {
    let Some(value) = header else {
        return "missing";
    };
    let value = value.split(';').next().unwrap_or_default().trim();
    [
        "text/event-stream",
        "application/json",
        "text/html",
        "text/plain",
        "application/octet-stream",
    ]
    .into_iter()
    .find(|category| value.eq_ignore_ascii_case(category))
    .unwrap_or("other")
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
    error: Option<serde_json::Value>,
    incomplete_details: Option<serde_json::Value>,
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
        text: BTreeMap<usize, String>,
        arguments: String,
        arguments_done: bool,
    },
    Finished(Item),
}

fn invalid() -> Error {
    Error::new("invalid or conflicting OpenAI response event")
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(super) fn read_stream(
    reader: impl Read,
    sender: &Sender<ResponseEvent>,
    metadata: &ModelMetadata,
) -> Result<()> {
    let mut reader = BufReader::new(reader);
    let mut data = Vec::new();
    let mut total = 0u64;
    let mut slots = BTreeMap::<usize, Slot>::new();
    let mut ids = BTreeSet::new();
    let mut response_id = None;
    let mut received_data = false;
    loop {
        let mut line = Vec::new();
        let size = reader
            .by_ref()
            .take(FRAME_CAP + 1)
            .read_until(b'\n', &mut line)
            .map_err(|_| {
                Error::new("OpenAI response stream interrupted; generation was not retried")
            })? as u64;
        total += size;
        if size > FRAME_CAP || total > STREAM_CAP {
            return Err(Error::new("OpenAI response exceeded stream size limit"));
        }
        if size == 0 {
            return Err(Error::new(if received_data {
                "OpenAI response ended before a terminal response event"
            } else {
                "OpenAI response contained no SSE data events"
            }));
        }
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        if !line.is_empty() {
            if !received_data {
                match line.iter().find(|byte| !byte.is_ascii_whitespace()) {
                    Some(b'{' | b'[') => {
                        return Err(Error::new(
                            "OpenAI returned a non-SSE JSON-like body; streaming SSE required",
                        ));
                    }
                    Some(b'<') => {
                        return Err(Error::new(
                            "OpenAI returned a non-SSE HTML/markup body; streaming SSE required",
                        ));
                    }
                    _ => {}
                }
            }
            if let Some(value) = line.strip_prefix(b"data:") {
                received_data = true;
                let value = value.strip_prefix(b" ").unwrap_or(value);
                if data.len() + value.len() + 1 > FRAME_CAP as usize {
                    return Err(Error::new("OpenAI response event exceeded size limit"));
                }
                data.extend_from_slice(value);
                data.push(b'\n');
            }
            continue;
        }
        if data.is_empty() {
            continue;
        }
        let event: Event = serde_json::from_slice(&data).map_err(|_| invalid())?;
        data.clear();
        match event {
            Event::Created { response } => {
                if response_id.is_some() || !identifier(&response.id) {
                    return Err(invalid());
                }
                response_id = Some(response.id);
            }
            Event::Added { output_index, item } => {
                if output_index >= 128
                    || !identifier(item.id())
                    || !ids.insert(item.id().to_owned())
                    || slots.contains_key(&output_index)
                {
                    return Err(invalid());
                }
                let arguments = match &item {
                    Item::Function { arguments, .. } => arguments.clone(),
                    _ => String::new(),
                };
                if arguments.len() > 65536 {
                    return Err(Error::new("model tool arguments exceed limit"));
                }
                slots.insert(
                    output_index,
                    Slot::Pending {
                        item,
                        text: BTreeMap::new(),
                        arguments,
                        arguments_done: false,
                    },
                );
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
                }) = slots.get_mut(&output_index)
                else {
                    return Err(invalid());
                };
                if id != &item_id || content_index >= 128 {
                    return Err(invalid());
                }
                text.entry(content_index).or_default().push_str(&delta);
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
                }) = slots.get_mut(&output_index)
                else {
                    return Err(invalid());
                };
                if id != &item_id {
                    return Err(invalid());
                }
                if arguments.len() + delta.len() > 65536 {
                    return Err(Error::new("model tool arguments exceed limit"));
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
                }) = slots.get_mut(&output_index)
                else {
                    return Err(invalid());
                };
                if id != &item_id
                    || *arguments_done
                    || !final_arguments.starts_with(arguments.as_str())
                    || final_arguments.len() > 65536
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
                }) = slots.remove(&output_index)
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
                        if text.keys().any(|index| *index >= content.len()) {
                            return Err(invalid());
                        }
                        for (index, part) in content.iter().enumerate() {
                            match part {
                                Content::Refusal => {
                                    return Err(Error::new("OpenAI refused the response"))
                                }
                                Content::Text { text: final_text } => {
                                    if let Some(streamed) = text.get(&index) {
                                        if streamed != final_text {
                                            return Err(invalid());
                                        }
                                    } else {
                                        emit(sender, ResponseEvent::TextDelta(final_text.clone()))?;
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
                            || final_arguments.len() > 65536
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
                slots.insert(output_index, Slot::Finished(final_item));
            }
            Event::Completed { response } => {
                if response.status != "completed"
                    || response.error.is_some()
                    || response.incomplete_details.is_some()
                    || !identifier(&response.id)
                    || response_id.as_ref().is_some_and(|id| id != &response.id)
                    || response.output.len() != slots.len()
                {
                    return Err(invalid());
                }
                let mut calls = ToolCallAssembler::default();
                let mut call_index = 0;
                for (index, item) in response.output.into_iter().enumerate() {
                    if !matches!(slots.get(&index), Some(Slot::Finished(finished)) if *finished == item)
                    {
                        return Err(Error::new(
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
                        ResponseEvent::Usage {
                            input_tokens: usage.input_tokens,
                            output_tokens: usage.output_tokens,
                            cached_input_tokens: cached,
                            cost_usd: metadata.usage_cost(
                                usage.input_tokens,
                                usage.output_tokens,
                                cached,
                            ),
                        },
                    )?;
                }
                if !calls.is_empty() {
                    emit(sender, ResponseEvent::ToolCallsRequested(calls.finish()?))?;
                    return Err(Error::new("tool execution is disabled until an authorization policy and sandbox are implemented"));
                }
                return Ok(());
            }
            Event::Incomplete { response } => {
                return Err(Error::new(
                    if response
                        .incomplete_details
                        .is_some_and(|details| details.reason == "max_output_tokens")
                    {
                        "OpenAI response truncated by its output limit"
                    } else {
                        "OpenAI response incomplete"
                    },
                ))
            }
            Event::Failed | Event::Error => {
                return Err(Error::new("OpenAI returned a streaming failure"))
            }
            Event::Refusal => return Err(Error::new("OpenAI refused the response")),
            Event::Other => {}
        }
    }
}
