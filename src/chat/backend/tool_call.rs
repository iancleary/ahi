//! Assemble untrusted streamed function calls before exposing them to policy.
//! Parsing a call is not authorization to execute it.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::{Error, Result};

const MAX_CALLS: usize = 32;
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: Map<String, Value>,
}

#[derive(Deserialize)]
pub(super) struct ToolCallDelta {
    index: usize,
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    function: Option<FunctionDelta>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Default)]
struct PendingCall {
    id: Option<String>,
    kind: Option<String>,
    name: String,
    arguments: String,
}

/// Calls indexed by the model's `index`: a flat array of at most `MAX_CALLS`
/// entries, where a hole at finish means the indexes were not contiguous.
#[derive(Default)]
pub(super) struct ToolCallAssembler {
    calls: Vec<Option<PendingCall>>,
}

impl ToolCallAssembler {
    pub(super) fn is_empty(&self) -> bool {
        self.calls.iter().all(Option::is_none)
    }

    pub(super) fn push_function(
        &mut self,
        index: usize,
        id: String,
        name: String,
        arguments: String,
    ) -> Result<()> {
        self.extend(vec![ToolCallDelta {
            index,
            id: Some(id),
            kind: Some("function".to_string()),
            function: Some(FunctionDelta {
                name: Some(name),
                arguments: Some(arguments),
            }),
        }])
    }

    pub(super) fn extend(&mut self, deltas: Vec<ToolCallDelta>) -> Result<()> {
        for delta in deltas {
            if delta.index >= MAX_CALLS {
                return Err(Error::limit("model tool call index exceeds limit"));
            }
            if self.calls.len() <= delta.index {
                self.calls.resize_with(delta.index + 1, || None);
            }
            let call = self.calls[delta.index].get_or_insert_with(PendingCall::default);
            if let Some(id) = delta.id {
                if !valid_identifier(&id, 128) || call.id.as_ref().is_some_and(|old| old != &id) {
                    return Err(Error::protocol("invalid or changing model tool call id"));
                }
                call.id = Some(id);
            }
            if let Some(kind) = delta.kind {
                if kind != "function" {
                    return Err(Error::protocol("unsupported model tool call type"));
                }
                call.kind = Some(kind);
            }
            if let Some(function) = delta.function {
                if let Some(name) = function.name {
                    if call.name.len() + name.len() > 64 {
                        return Err(Error::limit("model tool name exceeds limit"));
                    }
                    call.name.push_str(&name);
                }
                if let Some(arguments) = function.arguments {
                    if call.arguments.len() + arguments.len() > MAX_ARGUMENT_BYTES {
                        return Err(Error::limit("model tool arguments exceed limit"));
                    }
                    call.arguments.push_str(&arguments);
                }
            }
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<Vec<ToolCall>> {
        if self.is_empty() {
            return Err(Error::protocol(
                "model finished tool calls without any calls",
            ));
        }
        let mut result: Vec<ToolCall> = Vec::with_capacity(self.calls.len());
        for call in self.calls {
            let Some(call) = call else {
                return Err(Error::protocol(
                    "model tool call indices are not contiguous",
                ));
            };
            let id = call
                .id
                .ok_or_else(|| Error::protocol("model tool call has no id"))?;
            if result.iter().any(|existing| existing.id == id) {
                return Err(Error::protocol("model tool call ids are not unique"));
            }
            if call.kind.as_deref() != Some("function") || !valid_identifier(&call.name, 64) {
                return Err(Error::protocol("model tool call has an invalid function"));
            }
            // Never include parser errors or raw arguments in diagnostics: either
            // may contain prompt text or sensitive values from the model.
            let arguments: Map<String, Value> = serde_json::from_str(&call.arguments)
                .map_err(|_| Error::protocol("model tool arguments must be a JSON object"))?;
            result.push(ToolCall {
                id,
                name: call.name,
                arguments,
            });
        }
        Ok(result)
    }
}

fn valid_identifier(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
