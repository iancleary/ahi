//! Durable sessions: the record of what a conversation said and did.
//!
//! A session is a tree of entries under one header. Each entry names its
//! parent, so a turn that failed stays on disk as a branch while the next
//! attempt hangs off the last turn that succeeded. Nothing is rewritten:
//! every line that was written stays, and a reader follows parents from a
//! `turn` entry to the root to rebuild the conversation that produced it.
//!
//! The shape follows Pi's session files and the storage model of
//! pi-durable: one logical store with more than one backend. This module
//! holds the types, their JSON form, and the `SessionStore` trait; the
//! `jsonl` backend is the interchange format a coordinator ferries between
//! machines, and the local database backend arrives in a later slice.
//! `docs/HEADLESS_EXEC.md` has the design.

pub(crate) mod jsonl;
pub(crate) mod sqlite;

use std::fmt;

use serde_json::{json, Map, Value};

use super::backend::ToolCall;
use super::transcript::{ChatMessage, NativeReplay, ToolPayload, TranscriptRole};
use crate::harness::Protocol;

/// Version of the entry format. A store whose header names a newer schema
/// is refused, never read with guesses.
pub const SCHEMA: u32 = 1;

/// The first line of a session: where, when, and with what it started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub id: String,
    pub started: String,
    pub host: String,
    pub cwd: String,
    pub gateway: String,
    pub model: String,
    pub ahi_version: String,
    pub schema: u32,
}

impl Header {
    /// A header for a session starting now, with a fresh id.
    pub fn new(cwd: &str, gateway: &str, model: &str) -> Self {
        Self {
            id: uuid(),
            started: crate::util::rfc3339::now(),
            host: host(),
            cwd: cwd.to_string(),
            gateway: gateway.to_string(),
            model: model.to_string(),
            ahi_version: crate::VERSION.to_string(),
            schema: SCHEMA,
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "type": "session",
            "schema": self.schema,
            "id": self.id,
            "started": self.started,
            "host": self.host,
            "cwd": self.cwd,
            "gateway": self.gateway,
            "model": self.model,
            "ahi_version": self.ahi_version,
        })
    }

    pub(crate) fn from_json(value: &Value) -> Result<Self, StoreError> {
        if value["type"] != "session" {
            return Err(StoreError::Corrupt(
                "the first line is not a session header".into(),
            ));
        }
        let schema = value["schema"]
            .as_u64()
            .ok_or_else(|| StoreError::Corrupt("header has no schema".into()))?;
        if schema > u64::from(SCHEMA) {
            return Err(StoreError::Schema(format!(
                "session schema {schema} is newer than this ahi reads ({SCHEMA})"
            )));
        }
        let text = |key: &str| -> Result<String, StoreError> {
            value[key]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| StoreError::Corrupt(format!("header lacks `{key}`")))
        };
        Ok(Self {
            id: text("id")?,
            started: text("started")?,
            host: text("host").unwrap_or_default(),
            cwd: text("cwd").unwrap_or_default(),
            gateway: text("gateway")?,
            model: text("model")?,
            ahi_version: text("ahi_version").unwrap_or_default(),
            schema: schema as u32,
        })
    }
}

/// One line after the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Eight hex digits, unique within the session.
    pub id: String,
    /// `None` for an entry hanging off the header.
    pub parent: Option<String>,
    pub timestamp: String,
    pub kind: Kind,
}

/// What an entry records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Message(StoredMessage),
    Turn(Turn),
    ModelChange { gateway: String, model: String },
    ThinkingChange { level: String },
}

/// A transcript message as stored: role, text, and the tool exchange the
/// model must see again. Screen-only detail is not kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub role: Role,
    pub text: String,
    pub(crate) tool: Option<ToolPayload>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Operator,
    Assistant,
    Tool { exit_code: i32 },
}

/// One invocation's outcome, written last in its branch. `result` is the
/// result object without `text` (the text is the assistant message before
/// this line), so a resubmission with the same `run_id` can be answered
/// from the store without a model call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub run_id: String,
    /// `ok`, `failed`, `timeout`, `cancelled`.
    pub status: String,
    pub exit_code: u8,
    /// Hex SHA-256 of the prompt that started the turn.
    pub prompt_sha256: String,
    pub result: Value,
}

impl StoredMessage {
    /// Back to a transcript message. Native replay items are kept only for
    /// `protocol`; the TUI drops them on a protocol change for the same
    /// reason, since another provider cannot read them.
    pub(crate) fn to_chat(&self, protocol: Option<Protocol>) -> ChatMessage {
        let role = match self.role {
            Role::Operator => TranscriptRole::Operator,
            Role::Assistant => TranscriptRole::Assistant,
            Role::Tool { exit_code } => TranscriptRole::Tool { exit_code },
        };
        let mut message = ChatMessage::new(role, self.text.clone());
        message.tool = match &self.tool {
            Some(ToolPayload::Calls { calls, native }) => Some(ToolPayload::Calls {
                calls: calls.clone(),
                native: native
                    .clone()
                    .filter(|native| protocol == Some(native.protocol)),
            }),
            other => other.clone(),
        };
        message
    }
}

impl Entry {
    pub(crate) fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("id".into(), json!(self.id));
        object.insert("parentId".into(), json!(self.parent));
        object.insert("timestamp".into(), json!(self.timestamp));
        match &self.kind {
            Kind::Message(message) => {
                object.insert("type".into(), json!("message"));
                let (role, exit_code) = match message.role {
                    Role::Operator => ("operator", None),
                    Role::Assistant => ("assistant", None),
                    Role::Tool { exit_code } => ("tool", Some(exit_code)),
                };
                object.insert("role".into(), json!(role));
                if let Some(code) = exit_code {
                    object.insert("exit_code".into(), json!(code));
                }
                object.insert("text".into(), json!(message.text));
                match &message.tool {
                    Some(ToolPayload::Calls { calls, native }) => {
                        object.insert(
                            "calls".into(),
                            Value::Array(
                                calls
                                    .iter()
                                    .map(|call| {
                                        json!({
                                            "id": call.id,
                                            "name": call.name,
                                            "arguments": Value::Object(call.arguments.clone()),
                                        })
                                    })
                                    .collect(),
                            ),
                        );
                        if let Some(native) = native {
                            object.insert(
                                "native".into(),
                                json!({
                                    "protocol": serde_json::to_value(native.protocol)
                                        .unwrap_or(Value::Null),
                                    "items": native.items,
                                }),
                            );
                        }
                    }
                    Some(ToolPayload::Result {
                        call_id,
                        name,
                        content,
                    }) => {
                        object.insert(
                            "result".into(),
                            json!({"call_id": call_id, "name": name, "content": content}),
                        );
                    }
                    None => {}
                }
            }
            Kind::Turn(turn) => {
                object.insert("type".into(), json!("turn"));
                object.insert("run_id".into(), json!(turn.run_id));
                object.insert("status".into(), json!(turn.status));
                object.insert("exit_code".into(), json!(turn.exit_code));
                object.insert("prompt_sha256".into(), json!(turn.prompt_sha256));
                object.insert("result".into(), turn.result.clone());
            }
            Kind::ModelChange { gateway, model } => {
                object.insert("type".into(), json!("model_change"));
                object.insert("gateway".into(), json!(gateway));
                object.insert("model".into(), json!(model));
            }
            Kind::ThinkingChange { level } => {
                object.insert("type".into(), json!("thinking_change"));
                object.insert("level".into(), json!(level));
            }
        }
        Value::Object(object)
    }

    pub(crate) fn from_json(value: &Value) -> Result<Self, StoreError> {
        let corrupt = |what: &str| StoreError::Corrupt(format!("entry lacks `{what}`"));
        let text = |key: &str| value[key].as_str().map(str::to_string);
        let id = text("id").ok_or_else(|| corrupt("id"))?;
        let parent = text("parentId");
        let timestamp = text("timestamp").unwrap_or_default();
        let kind = match value["type"].as_str() {
            Some("message") => {
                let role = match (value["role"].as_str(), value["exit_code"].as_i64()) {
                    (Some("operator"), _) => Role::Operator,
                    (Some("assistant"), _) => Role::Assistant,
                    (Some("tool"), code) => Role::Tool {
                        exit_code: code.unwrap_or(1) as i32,
                    },
                    _ => return Err(corrupt("role")),
                };
                let tool = if let Some(calls) = value["calls"].as_array() {
                    let calls = calls
                        .iter()
                        .map(|call| {
                            Ok(ToolCall {
                                id: call["id"]
                                    .as_str()
                                    .ok_or_else(|| corrupt("call id"))?
                                    .into(),
                                name: call["name"]
                                    .as_str()
                                    .ok_or_else(|| corrupt("call name"))?
                                    .into(),
                                arguments: call["arguments"]
                                    .as_object()
                                    .cloned()
                                    .unwrap_or_default(),
                            })
                        })
                        .collect::<Result<Vec<_>, StoreError>>()?;
                    let native = value.get("native").and_then(|native| {
                        Some(NativeReplay {
                            protocol: serde_json::from_value(native["protocol"].clone()).ok()?,
                            items: native["items"].as_array()?.clone(),
                        })
                    });
                    Some(ToolPayload::Calls { calls, native })
                } else {
                    value.get("result").map(|result| ToolPayload::Result {
                        call_id: result["call_id"].as_str().unwrap_or_default().into(),
                        name: result["name"].as_str().unwrap_or_default().into(),
                        content: result["content"].as_str().unwrap_or_default().into(),
                    })
                };
                Kind::Message(StoredMessage {
                    role,
                    text: text("text").unwrap_or_default(),
                    tool,
                })
            }
            Some("turn") => Kind::Turn(Turn {
                run_id: text("run_id").ok_or_else(|| corrupt("run_id"))?,
                status: text("status").unwrap_or_else(|| "unknown".into()),
                exit_code: value["exit_code"].as_u64().unwrap_or(1) as u8,
                prompt_sha256: text("prompt_sha256").unwrap_or_default(),
                result: value.get("result").cloned().unwrap_or(Value::Null),
            }),
            Some("model_change") => Kind::ModelChange {
                gateway: text("gateway").unwrap_or_default(),
                model: text("model").unwrap_or_default(),
            },
            Some("thinking_change") => Kind::ThinkingChange {
                level: text("level").unwrap_or_default(),
            },
            Some(other) => {
                return Err(StoreError::Corrupt(format!("unknown entry type `{other}`")))
            }
            None => return Err(corrupt("type")),
        };
        Ok(Self {
            id,
            parent,
            timestamp,
            kind,
        })
    }
}

/// Why a store could not be opened or written. Every variant is exit 7 for
/// `ahi exec`; the message says which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The file or database could not be read or written.
    Io(String),
    /// The header names a schema this ahi does not read.
    Schema(String),
    /// A line or row is not what the schema says.
    Corrupt(String),
    /// Another writer appended since this store was opened.
    Conflict(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message)
            | Self::Schema(message)
            | Self::Corrupt(message)
            | Self::Conflict(message) => f.write_str(message),
        }
    }
}

/// One session, open for reading and appending. Backends differ in where
/// the lines live; callers see the same tree.
pub(crate) trait SessionStore {
    fn header(&self) -> &Header;
    /// Every entry, in write order.
    fn entries(&self) -> &[Entry];
    /// The last entry written, which the next append must still find last.
    fn tip(&self) -> Option<&str>;
    /// Append `kinds` as one step, each under the previous and the first
    /// under `parent`. Returns the new ids. Fails without writing when
    /// another writer moved the tip.
    fn append(&mut self, parent: Option<&str>, kinds: Vec<Kind>)
        -> Result<Vec<String>, StoreError>;
    /// `jsonl` or `sqlite`, for the result object.
    fn backend(&self) -> &'static str;
    /// Where the session lives, for messages.
    fn location(&self) -> String;

    /// The entry a new turn hangs off: the last `turn` that succeeded, or
    /// the root when none has. A failed turn's branch is left where it is.
    fn active_parent(&self) -> Option<String> {
        self.entries()
            .iter()
            .rev()
            .find(|entry| matches!(&entry.kind, Kind::Turn(turn) if turn.status == "ok"))
            .map(|entry| entry.id.clone())
    }

    /// Entries from `from` back to the root, in conversation order.
    fn path_to_root(&self, from: Option<&str>) -> Vec<&Entry> {
        let mut path = Vec::new();
        let mut next = from.map(str::to_string);
        while let Some(id) = next {
            let Some(entry) = self.entries().iter().find(|entry| entry.id == id) else {
                break;
            };
            path.push(entry);
            next = entry.parent.clone();
        }
        path.reverse();
        path
    }

    /// The conversation a new turn continues: every message on the active
    /// path, as transcript messages for `protocol`.
    fn active_messages(&self, protocol: Option<Protocol>) -> Vec<ChatMessage> {
        self.path_to_root(self.active_parent().as_deref())
            .into_iter()
            .filter_map(|entry| match &entry.kind {
                Kind::Message(message) => Some(message.to_chat(protocol)),
                _ => None,
            })
            .collect()
    }

    /// The `turn` entry written for `run_id`, if this session has one.
    fn find_turn(&self, run_id: &str) -> Option<&Entry> {
        self.entries()
            .iter()
            .find(|entry| matches!(&entry.kind, Kind::Turn(turn) if turn.run_id == run_id))
    }

    /// The last assistant text before `turn_id` on its branch: the answer
    /// that turn produced.
    fn answer_of(&self, turn_id: &str) -> Option<String> {
        let turn = self.entries().iter().find(|entry| entry.id == turn_id)?;
        self.path_to_root(turn.parent.as_deref())
            .into_iter()
            .rev()
            .find_map(|entry| match &entry.kind {
                Kind::Message(StoredMessage {
                    role: Role::Assistant,
                    text,
                    ..
                }) => Some(text.clone()),
                _ => None,
            })
    }
}

/// Eight hex digits from the OS.
pub(crate) fn entry_id() -> String {
    let mut bytes = [0u8; 4];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A version 4 UUID from the OS.
pub(crate) fn uuid() -> String {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: Vec<String> = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        hex[0..4].concat(),
        hex[4..6].concat(),
        hex[6..8].concat(),
        hex[8..10].concat(),
        hex[10..16].concat()
    )
}

/// The machine's name, for the header: the environment first, then the
/// `hostname` program, then `unknown`. Never an error.
fn host() -> String {
    for key in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return value.trim().to_string();
            }
        }
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_round_trip_through_json() {
        let mut arguments = Map::new();
        arguments.insert("path".into(), json!("src/a.rs"));
        let entries = vec![
            Entry {
                id: "aaaaaaaa".into(),
                parent: None,
                timestamp: "2026-11-11T00:00:00Z".into(),
                kind: Kind::Message(StoredMessage {
                    role: Role::Operator,
                    text: "hello".into(),
                    tool: None,
                }),
            },
            Entry {
                id: "bbbbbbbb".into(),
                parent: Some("aaaaaaaa".into()),
                timestamp: "2026-11-11T00:00:01Z".into(),
                kind: Kind::Message(StoredMessage {
                    role: Role::Assistant,
                    text: "reading".into(),
                    tool: Some(ToolPayload::Calls {
                        calls: vec![ToolCall {
                            id: "c1".into(),
                            name: "read_file".into(),
                            arguments,
                        }],
                        native: Some(NativeReplay {
                            protocol: Protocol::OpenAiResponses,
                            items: vec![json!({"type": "reasoning"})],
                        }),
                    }),
                }),
            },
            Entry {
                id: "cccccccc".into(),
                parent: Some("bbbbbbbb".into()),
                timestamp: String::new(),
                kind: Kind::Message(StoredMessage {
                    role: Role::Tool { exit_code: 126 },
                    text: "read_file src/a.rs: denied".into(),
                    tool: Some(ToolPayload::Result {
                        call_id: "c1".into(),
                        name: "read_file".into(),
                        content: "denied: no rule".into(),
                    }),
                }),
            },
            Entry {
                id: "dddddddd".into(),
                parent: Some("cccccccc".into()),
                timestamp: String::new(),
                kind: Kind::Turn(Turn {
                    run_id: "r1".into(),
                    status: "ok".into(),
                    exit_code: 0,
                    prompt_sha256: "ab".into(),
                    result: json!({"effects": "none"}),
                }),
            },
            Entry {
                id: "eeeeeeee".into(),
                parent: Some("dddddddd".into()),
                timestamp: String::new(),
                kind: Kind::ModelChange {
                    gateway: "g".into(),
                    model: "m".into(),
                },
            },
            Entry {
                id: "ffffffff".into(),
                parent: Some("eeeeeeee".into()),
                timestamp: String::new(),
                kind: Kind::ThinkingChange {
                    level: "low".into(),
                },
            },
        ];
        for entry in &entries {
            let line = entry.to_json();
            assert_eq!(&Entry::from_json(&line).unwrap(), entry, "{line}");
        }
        assert_eq!(
            entries[1].to_json()["native"]["protocol"],
            "openai-responses"
        );
        let header = Header::new("/w", "local", "m");
        let parsed = Header::from_json(&header.to_json()).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(parsed.id.len(), 36);
    }

    #[test]
    fn native_replay_crosses_only_to_its_own_protocol() {
        let stored = StoredMessage {
            role: Role::Assistant,
            text: String::new(),
            tool: Some(ToolPayload::Calls {
                calls: Vec::new(),
                native: Some(NativeReplay {
                    protocol: Protocol::OpenAiResponses,
                    items: vec![json!(1)],
                }),
            }),
        };
        let same = stored.to_chat(Some(Protocol::OpenAiResponses));
        assert!(matches!(
            same.tool,
            Some(ToolPayload::Calls {
                native: Some(_),
                ..
            })
        ));
        let other = stored.to_chat(Some(Protocol::OpenAiCompletions));
        assert!(matches!(
            other.tool,
            Some(ToolPayload::Calls { native: None, .. })
        ));
        let none = stored.to_chat(None);
        assert!(matches!(
            none.tool,
            Some(ToolPayload::Calls { native: None, .. })
        ));
    }

    #[test]
    fn newer_schemas_and_wrong_first_lines_are_refused() {
        let mut header = Header::new("/w", "g", "m").to_json();
        header["schema"] = json!(SCHEMA + 1);
        assert!(matches!(
            Header::from_json(&header),
            Err(StoreError::Schema(_))
        ));
        assert!(matches!(
            Header::from_json(&json!({"type": "message"})),
            Err(StoreError::Corrupt(_))
        ));
        assert!(matches!(
            Entry::from_json(&json!({"id": "x", "type": "teleport"})),
            Err(StoreError::Corrupt(_))
        ));
    }

    #[test]
    fn ids_are_file_safe() {
        assert_eq!(entry_id().len(), 8);
        let id = uuid();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_ne!(uuid(), id);
    }
}
