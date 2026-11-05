//! Assemble untrusted streamed function calls before exposing them to policy.
//! Parsing a call is not authorization to execute it.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::error::{Error, Result};

const MAX_CALLS: usize = 32;
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq)]
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

#[derive(Default)]
pub(super) struct ToolCallAssembler {
    calls: BTreeMap<usize, PendingCall>,
}

impl ToolCallAssembler {
    pub(super) fn is_empty(&self) -> bool {
        self.calls.is_empty()
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
                return Err(Error::new("model tool call index exceeds limit"));
            }
            let call = self.calls.entry(delta.index).or_default();
            if let Some(id) = delta.id {
                if !valid_identifier(&id, 128) || call.id.as_ref().is_some_and(|old| old != &id) {
                    return Err(Error::new("invalid or changing model tool call id"));
                }
                call.id = Some(id);
            }
            if let Some(kind) = delta.kind {
                if kind != "function" {
                    return Err(Error::new("unsupported model tool call type"));
                }
                call.kind = Some(kind);
            }
            if let Some(function) = delta.function {
                if let Some(name) = function.name {
                    if call.name.len() + name.len() > 64 {
                        return Err(Error::new("model tool name exceeds limit"));
                    }
                    call.name.push_str(&name);
                }
                if let Some(arguments) = function.arguments {
                    if call.arguments.len() + arguments.len() > MAX_ARGUMENT_BYTES {
                        return Err(Error::new("model tool arguments exceed limit"));
                    }
                    call.arguments.push_str(&arguments);
                }
            }
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<Vec<ToolCall>> {
        if self.calls.is_empty() {
            return Err(Error::new("model finished tool calls without any calls"));
        }
        let mut ids = BTreeSet::new();
        let mut result = Vec::new();
        for (expected_index, (index, call)) in self.calls.into_iter().enumerate() {
            if index != expected_index {
                return Err(Error::new("model tool call indices are not contiguous"));
            }
            let id = call
                .id
                .ok_or_else(|| Error::new("model tool call has no id"))?;
            if !ids.insert(id.clone()) {
                return Err(Error::new("model tool call ids are not unique"));
            }
            if call.kind.as_deref() != Some("function") || !valid_identifier(&call.name, 64) {
                return Err(Error::new("model tool call has an invalid function"));
            }
            // Never include parser errors or raw arguments in diagnostics: either
            // may contain prompt text or sensitive values from the model.
            let arguments: Map<String, Value> = serde_json::from_str(&call.arguments)
                .map_err(|_| Error::new("model tool arguments must be a JSON object"))?;
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
