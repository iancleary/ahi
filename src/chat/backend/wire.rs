//! Transcript to Chat Completions `messages`.
//!
//! Tool exchanges are replayed only when complete: an assistant call is sent
//! with the results that answer it, and a result is sent only after the
//! assistant message that requested it. A turn that failed between a request
//! and its results therefore never produces an invalid follow-up request.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::chat::transcript::{ChatMessage, ToolPayload, TranscriptRole};
use crate::harness::CompletionCompatibility;

pub(super) fn messages(messages: &[ChatMessage], compat: &CompletionCompatibility) -> Vec<Value> {
    // Pass 1: pair each result with the assistant message it answers.
    let mut answered: BTreeMap<usize, BTreeSet<&str>> = BTreeMap::new();
    let mut paired_results = BTreeSet::new();
    let mut open: Option<(usize, BTreeSet<&str>)> = None;
    for (index, message) in messages.iter().enumerate() {
        match (&message.role, &message.tool) {
            (TranscriptRole::Assistant, Some(ToolPayload::Calls(calls))) => {
                open = Some((index, calls.iter().map(|call| call.id.as_str()).collect()));
            }
            (TranscriptRole::Assistant | TranscriptRole::Operator, _) => open = None,
            (TranscriptRole::Tool { .. }, Some(ToolPayload::Result { call_id, .. })) => {
                if let Some((assistant, ids)) = &mut open {
                    if ids.remove(call_id.as_str()) {
                        answered.entry(*assistant).or_default().insert(call_id);
                        paired_results.insert(index);
                    }
                }
            }
            _ => {}
        }
    }
    // Pass 2: emit.
    let mut output = Vec::new();
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
                    Some(ToolPayload::Calls(calls)) => calls
                        .iter()
                        .filter(|call| {
                            answered
                                .get(&index)
                                .is_some_and(|ids| ids.contains(call.id.as_str()))
                        })
                        .map(|call| {
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
            TranscriptRole::Tool { .. } if paired_results.contains(&index) => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::backend::tools::call;

    fn assistant(text: &str, ids: &[&str]) -> ChatMessage {
        let mut message = ChatMessage::new(TranscriptRole::Assistant, text);
        if !ids.is_empty() {
            message.tool = Some(ToolPayload::Calls(
                ids.iter()
                    .map(|id| call(id, "read_file", json!({"path": "a"})))
                    .collect(),
            ));
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
