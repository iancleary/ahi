//! The wire form of the service: one JSON object per line, in either
//! direction, over any byte stream.
//!
//! A request is `{"id": n, "method": "...", "params": {...}}` and gets one
//! answer, `{"id": n, "result": ...}` or `{"id": n, "error": {"kind",
//! "message"}}`. Once a connection has subscribed to a session, the
//! session's events arrive on the same stream as objects with an `"event"`
//! key and no `id`. This is the shape herdr-core uses for its own cores, and
//! it survives a stdio bridge (`ahi connect`, `ssh host ahi connect`) because
//! every message is a line.

// Without a Unix socket nothing on this platform speaks the wire form yet;
// the in-process client still goes through `dispatch`.
#![cfg_attr(not(unix), allow(dead_code))]

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use serde_json::{json, Value};

use super::{Event, Service, ServiceError, ServiceResult, Submitted, TurnRequest};
use crate::session::Status;

/// A client's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) id: Value,
    pub(crate) method: String,
    pub(crate) params: Value,
}

impl Request {
    pub(crate) fn parse(line: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(line).map_err(|err| format!("not JSON: {err}"))?;
        let method = value["method"].as_str().ok_or("no method")?.to_string();
        Ok(Self {
            id: value.get("id").cloned().unwrap_or(Value::Null),
            method,
            params: value.get("params").cloned().unwrap_or(json!({})),
        })
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({"id": self.id, "method": self.method, "params": self.params})
    }
}

/// The server's answer to one request.
pub(crate) fn response(id: &Value, result: ServiceResult<Value>) -> Value {
    match result {
        Ok(result) => json!({"id": id, "result": result}),
        Err(err) => json!({"id": id, "error": {"kind": err.kind, "message": err.message}}),
    }
}

impl Event {
    pub(crate) fn to_json(&self) -> Value {
        match self {
            Self::Entry { session, entry } => {
                json!({"event": "entry", "session": session, "entry": entry})
            }
            Self::Text {
                session,
                run_id,
                delta,
            } => json!({"event": "text", "session": session, "run_id": run_id, "delta": delta}),
            Self::Ask {
                session,
                run_id,
                ask_id,
                calls,
            } => json!({
                "event": "ask", "session": session, "run_id": run_id, "ask_id": ask_id, "calls": calls
            }),
            Self::Turn {
                session,
                run_id,
                status,
                result,
            } => json!({
                "event": "turn", "session": session, "run_id": run_id,
                "status": status.as_str(), "result": result
            }),
            Self::Keepalive => json!({"event": "keepalive"}),
        }
    }

    pub(crate) fn from_json(value: &Value) -> Option<Self> {
        let text = |key: &str| value[key].as_str().map(str::to_string);
        Some(match value["event"].as_str()? {
            "entry" => Self::Entry {
                session: text("session")?,
                entry: value.get("entry")?.clone(),
            },
            "text" => Self::Text {
                session: text("session")?,
                run_id: text("run_id")?,
                delta: text("delta")?,
            },
            "ask" => Self::Ask {
                session: text("session")?,
                run_id: text("run_id")?,
                ask_id: text("ask_id")?,
                calls: value["calls"].as_array()?.clone(),
            },
            "turn" => Self::Turn {
                session: text("session")?,
                run_id: text("run_id")?,
                status: match value["status"].as_str()? {
                    "ok" => Status::Ok,
                    "failed" => Status::Failed,
                    "cancelled" => Status::Cancelled,
                    "timeout" => Status::Timeout,
                    _ => return None,
                },
                result: value.get("result")?.clone(),
            },
            "keepalive" => Self::Keepalive,
            _ => return None,
        })
    }
}

/// Decode `turn.submit` params. `origin` names the front end that asked
/// (`exec`, `tui`, or `serve` for anything else), for the record and the
/// audit log.
pub(crate) fn turn_request(params: &Value) -> ServiceResult<TurnRequest> {
    let text = |key: &str| params[key].as_str().map(str::to_string);
    let origin = match params["origin"].as_str() {
        Some("exec") => "exec",
        Some("tui") => "tui",
        _ => "serve",
    };
    let prompt = text("prompt").ok_or_else(|| usage("turn.submit needs a prompt"))?;
    if prompt.trim().is_empty() {
        return Err(usage("the prompt is empty"));
    }
    Ok(TurnRequest {
        prompt,
        run_id: text("run_id"),
        prompt_sha256: text("prompt_sha256"),
        workdir: text("workdir").map(PathBuf::from),
        tools: text("tools").map(PathBuf::from),
        timeout: params["timeout"].as_u64().map(Duration::from_secs),
        thinking: text("thinking"),
        unattended: params["unattended"].as_bool().unwrap_or(false),
        origin,
        client: text("client").unwrap_or_default(),
    })
}

/// Encode a `TurnRequest` as `turn.submit` params.
pub(crate) fn turn_params(session: &str, request: &TurnRequest) -> Value {
    json!({
        "session": session,
        "prompt": request.prompt,
        "run_id": request.run_id,
        "prompt_sha256": request.prompt_sha256,
        "workdir": request.workdir.as_ref().map(|path| path.display().to_string()),
        "tools": request.tools.as_ref().map(|path| path.display().to_string()),
        "timeout": request.timeout.map(|timeout| timeout.as_secs()),
        "thinking": request.thinking,
        "unattended": request.unattended,
        "origin": request.origin,
        "client": request.client,
    })
}

fn usage(message: &str) -> ServiceError {
    ServiceError {
        kind: "usage",
        message: message.to_string(),
    }
}

fn session_param(params: &Value) -> ServiceResult<&str> {
    params["session"]
        .as_str()
        .ok_or_else(|| usage("this method needs a session id"))
}

/// Answer one request. `subscribe` is called when the connection asks to
/// follow a session: the connection owns the receiver from then on.
pub(crate) fn dispatch(
    service: &Service,
    request: &Request,
    subscribe: &mut dyn FnMut(Receiver<Event>),
) -> ServiceResult<Value> {
    let params = &request.params;
    match request.method.as_str() {
        "health" => Ok(json!({
            "version": crate::VERSION,
            "store": service.store_dir().display().to_string(),
            "open_turns": service.open_turns(),
        })),
        "session.list" => Ok(Value::Array(service.list_sessions()?)),
        "session.create" => {
            let text = |key: &str| params[key].as_str();
            let header = service.create_session(
                text("cwd").map(std::path::Path::new),
                text("gateway"),
                text("model"),
                text("thinking").unwrap_or("off"),
            )?;
            Ok(header.to_json())
        }
        "session.get" => service.session_info(session_param(params)?),
        "session.entries" => Ok(Value::Array(service.entries(
            session_param(params)?,
            params["after"].as_u64().unwrap_or(0),
            params["branch"].as_u64(),
        )?)),
        "session.subscribe" => {
            let receiver = service.subscribe(
                session_param(params)?,
                params["after"].as_u64().unwrap_or(0),
            )?;
            subscribe(receiver);
            Ok(json!({}))
        }
        "session.settings" => {
            let gateway_model = match (params["gateway"].as_str(), params["model"].as_str()) {
                (Some(gateway), Some(model)) => Some((gateway, model)),
                (None, None) => None,
                _ => return Err(usage("gateway and model go together")),
            };
            let settings = service.settings(
                session_param(params)?,
                gateway_model,
                params["thinking"].as_str(),
            )?;
            Ok(json!({
                "gateway": settings.gateway, "model": settings.model, "thinking": settings.thinking
            }))
        }
        "turn.submit" => {
            let session = session_param(params)?;
            let turn = turn_request(params)?;
            Ok(match service.submit(session, turn)? {
                Submitted::Running { run_id } => json!({"run_id": run_id, "status": "running"}),
                Submitted::Replayed(doc) => doc,
            })
        }
        "turn.get" => {
            let run_id = params["run_id"]
                .as_str()
                .ok_or_else(|| usage("turn.get needs a run_id"))?;
            Ok(service
                .turn(session_param(params)?, run_id)?
                .unwrap_or_else(|| json!({"run_id": run_id, "status": "running"})))
        }
        "turn.cancel" => {
            let run_id = params["run_id"]
                .as_str()
                .ok_or_else(|| usage("turn.cancel needs a run_id"))?;
            service.cancel(session_param(params)?, run_id)?;
            Ok(json!({}))
        }
        "ask.answer" => {
            let ask_id = params["ask_id"]
                .as_str()
                .ok_or_else(|| usage("ask.answer needs an ask_id"))?;
            let allow: Vec<bool> = params["allow"]
                .as_array()
                .ok_or_else(|| usage("ask.answer needs `allow`, one boolean per call"))?
                .iter()
                .map(|value| value.as_bool().unwrap_or(false))
                .collect();
            service.answer(session_param(params)?, ask_id, &allow)?;
            Ok(json!({}))
        }
        other => Err(ServiceError {
            kind: "not_found",
            message: format!("unknown method `{other}`"),
        }),
    }
}
