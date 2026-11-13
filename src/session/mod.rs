//! Durable sessions: the record of what a conversation said and did.
//!
//! A session is one file of JSON lines under the state directory: a header,
//! then one entry per line, never rewritten. Each entry names its parent,
//! so a turn that failed or was cancelled stays on disk as a branch while
//! the next turn hangs off the last turn that succeeded. A reader follows
//! parents from a `turn` entry to the root to rebuild the conversation that
//! produced it, and the line's position (`seq`) is both its id and the
//! cursor a follower resumes from.
//!
//! One process writes a session at a time: the chat that holds it today,
//! the server that owns the whole store later. Readers (`ahi session`)
//! tolerate a file that grows under them, since a torn last line is skipped.
//! The format is also the interchange format: `ahi session export` copies
//! the file, `import` brings one written elsewhere into the store.

pub mod commands;
pub(crate) mod recorder;
pub(crate) mod store;

use std::fmt;

use serde_json::{json, Map, Value};

pub(crate) use store::{Session, Store};

use crate::chat::backend::ToolCall;
pub(crate) use crate::chat::transcript::ToolPayload;
use crate::chat::transcript::{ChatMessage, NativeReplay, TranscriptRole};
use crate::harness::Protocol;

/// Version of the line format. A header naming a newer schema is refused,
/// never read with guesses.
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
    /// The thinking level at the start; `thinking_change` entries follow.
    pub thinking: String,
    pub ahi_version: String,
    pub schema: u32,
}

impl Header {
    /// A header for a session starting now, with a fresh id.
    pub fn new(cwd: &str, gateway: &str, model: &str, thinking: &str) -> Self {
        Self {
            id: uuid(),
            started: crate::util::rfc3339::now(),
            host: host(),
            cwd: cwd.to_string(),
            gateway: gateway.to_string(),
            model: model.to_string(),
            thinking: thinking.to_string(),
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
            "thinking": self.thinking,
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
        let id = text("id")?;
        if !valid_id(&id) {
            return Err(StoreError::Corrupt(format!(
                "header id `{id}` is not a session id"
            )));
        }
        Ok(Self {
            id,
            started: text("started")?,
            host: text("host").unwrap_or_default(),
            cwd: text("cwd").unwrap_or_default(),
            gateway: text("gateway")?,
            model: text("model")?,
            thinking: text("thinking").unwrap_or_else(|_| "off".into()),
            ahi_version: text("ahi_version").unwrap_or_default(),
            schema: schema as u32,
        })
    }
}

/// One line after the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Position in the file, from 1. Unique and increasing within a session.
    pub seq: u64,
    /// `None` for an entry hanging off the header.
    pub parent: Option<u64>,
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
/// model must see again. For a tool result `text` is the bounded preview
/// the screen shows and `tool` carries the content the model saw.
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

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Failed,
    Cancelled,
    Timeout,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "ok" => Some(Self::Ok),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "timeout" => Some(Self::Timeout),
            _ => None,
        }
    }
}

/// One turn's outcome, written last in its branch. `result` holds what a
/// caller may need to answer a repeated submission without a model call:
/// the origin, an error, usage. The answer text is the assistant message
/// before this line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub run_id: String,
    pub status: Status,
    /// Hex SHA-256 of the prompt that started the turn.
    pub prompt_sha256: String,
    pub result: Value,
}

impl StoredMessage {
    /// Back to a transcript message. Native replay items are kept only for
    /// `protocol`; the screen drops them on a protocol change for the same
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
        object.insert("seq".into(), json!(self.seq));
        object.insert("parent".into(), json!(self.parent));
        object.insert("time".into(), json!(self.timestamp));
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
                object.insert("status".into(), json!(turn.status.as_str()));
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
        let seq = value["seq"].as_u64().ok_or_else(|| corrupt("seq"))?;
        let parent = value["parent"].as_u64();
        let timestamp = text("time").unwrap_or_default();
        let kind = match value["type"].as_str() {
            Some("message") => {
                let role = match (value["role"].as_str(), value["exit_code"].as_i64()) {
                    (Some("operator"), _) => Role::Operator,
                    (Some("assistant"), _) => Role::Assistant,
                    (Some("tool"), code) => Role::Tool {
                        exit_code: code.ok_or_else(|| corrupt("exit_code"))? as i32,
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
                    match value.get("result") {
                        Some(result) => Some(ToolPayload::Result {
                            call_id: result["call_id"]
                                .as_str()
                                .ok_or_else(|| corrupt("result call_id"))?
                                .into(),
                            name: result["name"]
                                .as_str()
                                .ok_or_else(|| corrupt("result name"))?
                                .into(),
                            content: result["content"]
                                .as_str()
                                .ok_or_else(|| corrupt("result content"))?
                                .into(),
                        }),
                        None => None,
                    }
                };
                Kind::Message(StoredMessage {
                    role,
                    text: text("text").ok_or_else(|| corrupt("text"))?,
                    tool,
                })
            }
            Some("turn") => Kind::Turn(Turn {
                run_id: text("run_id").ok_or_else(|| corrupt("run_id"))?,
                status: value["status"]
                    .as_str()
                    .and_then(Status::parse)
                    .ok_or_else(|| corrupt("status"))?,
                prompt_sha256: text("prompt_sha256").unwrap_or_default(),
                result: value.get("result").cloned().unwrap_or(Value::Null),
            }),
            Some("model_change") => Kind::ModelChange {
                gateway: text("gateway").ok_or_else(|| corrupt("gateway"))?,
                model: text("model").ok_or_else(|| corrupt("model"))?,
            },
            Some("thinking_change") => Kind::ThinkingChange {
                level: text("level").ok_or_else(|| corrupt("level"))?,
            },
            Some(other) => {
                return Err(StoreError::Corrupt(format!("unknown entry type `{other}`")))
            }
            None => return Err(corrupt("type")),
        };
        Ok(Self {
            seq,
            parent,
            timestamp,
            kind,
        })
    }
}

/// Why a session could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The file could not be read or written.
    Io(String),
    /// The header names a schema this ahi does not read.
    Schema(String),
    /// A line is not what the schema says.
    Corrupt(String),
    /// No such session.
    NotFound(String),
    /// A session with this id exists and the two histories differ.
    Conflict(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message)
            | Self::Schema(message)
            | Self::Corrupt(message)
            | Self::NotFound(message)
            | Self::Conflict(message) => f.write_str(message),
        }
    }
}

/// The settings in force on a session's active path: the header's, then
/// whatever `model_change` and `thinking_change` entries say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub gateway: String,
    pub model: String,
    pub thinking: String,
}

/// A caller-visible run id: sixteen hex digits from the OS.
pub fn run_id() -> String {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Hex SHA-256 of `bytes`, for `prompt_sha256`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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

/// A session id is a lower-case hyphenated UUID and nothing else, so an id
/// from a command line or a file can name a path inside the store and
/// nowhere else.
pub(crate) fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase(),
        })
}

/// A caller-chosen run id: 1 to 64 letters, digits, `-`, or `_`. It names
/// a thread and an ask id and is written into the record, so nothing else
/// is accepted, from the command line or the wire.
pub(crate) fn valid_run_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
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
    fn entries_round_trip_through_json_and_bad_lines_are_refused() {
        let mut arguments = Map::new();
        arguments.insert("path".into(), json!("src/a.rs"));
        let entries = vec![
            Entry {
                seq: 1,
                parent: None,
                timestamp: "2026-11-11T00:00:00Z".into(),
                kind: Kind::Message(StoredMessage {
                    role: Role::Operator,
                    text: "hello".into(),
                    tool: None,
                }),
            },
            Entry {
                seq: 2,
                parent: Some(1),
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
                seq: 3,
                parent: Some(2),
                timestamp: String::new(),
                kind: Kind::Message(StoredMessage {
                    role: Role::Tool { exit_code: 126 },
                    text: "denied".into(),
                    tool: Some(ToolPayload::Result {
                        call_id: "c1".into(),
                        name: "read_file".into(),
                        content: "denied: rule".into(),
                    }),
                }),
            },
            Entry {
                seq: 4,
                parent: Some(3),
                timestamp: String::new(),
                kind: Kind::Turn(Turn {
                    run_id: "0123456789abcdef".into(),
                    status: Status::Cancelled,
                    prompt_sha256: "ab".into(),
                    result: json!({"origin": "tui"}),
                }),
            },
            Entry {
                seq: 5,
                parent: Some(4),
                timestamp: String::new(),
                kind: Kind::ModelChange {
                    gateway: "g".into(),
                    model: "m".into(),
                },
            },
            Entry {
                seq: 6,
                parent: Some(5),
                timestamp: String::new(),
                kind: Kind::ThinkingChange {
                    level: "high".into(),
                },
            },
        ];
        for entry in &entries {
            let line = entry.to_json().to_string();
            assert_eq!(
                &Entry::from_json(&serde_json::from_str(&line).unwrap()).unwrap(),
                entry
            );
        }
        // Replay items are dropped for another protocol and kept for the same.
        let Kind::Message(message) = &entries[1].kind else {
            panic!()
        };
        assert!(message
            .to_chat(Some(Protocol::AnthropicMessages))
            .tool
            .is_some_and(|tool| matches!(tool, ToolPayload::Calls { native: None, .. })));
        assert!(message
            .to_chat(Some(Protocol::OpenAiResponses))
            .tool
            .is_some_and(|tool| matches!(
                tool,
                ToolPayload::Calls {
                    native: Some(_),
                    ..
                }
            )));
        for bad in [
            json!({"seq": 1, "type": "message", "role": "tool", "text": "x"}),
            json!({"seq": 1, "type": "turn", "run_id": "r", "status": "won"}),
            json!({"seq": 1, "type": "note"}),
            json!({"type": "message", "role": "operator", "text": "x"}),
        ] {
            assert!(
                matches!(Entry::from_json(&bad), Err(StoreError::Corrupt(_))),
                "{bad}"
            );
        }
        let header = Header::new("/w", "g", "m", "low");
        let back = Header::from_json(&header.to_json()).unwrap();
        assert_eq!(back, header);
        assert!(valid_id(&header.id));
        assert!(!valid_id("../etc/passwd"));
        assert!(!valid_id(&header.id.to_uppercase()));
        let mut newer = header.to_json();
        newer["schema"] = json!(SCHEMA + 1);
        assert!(matches!(
            Header::from_json(&newer),
            Err(StoreError::Schema(_))
        ));
        assert_eq!(run_id().len(), 16);
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
