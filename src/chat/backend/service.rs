//! The chat screen as a client of the session service: a
//! `CompletionBackend` whose turns run in `ahi serve` (or in an in-process
//! `Service`) and come back as the same `ResponseEvent`s the screen already
//! draws. The server records the session, so the screen's own recorder is
//! off in this mode.
//!
//! One screen, one session: the first turn creates it in the screen's
//! working directory, `/clear` starts another at the next turn, `/model` and
//! `/thinking` become `session.settings` before the next submission, and
//! `--resume` names an existing one. Approvals the policy could not decide
//! arrive as `ask` events and go back as `ask.answer` when the operator
//! decides in the dialog the screen already has.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use super::{
    ApprovalReply, ApprovalRequest, ChatRequest, CompletionBackend, ProposedAction, ResponseEvent,
    ToolDecision,
};
use crate::chat::transcript::TranscriptRole;
use crate::error::ErrorKind;
use crate::service::client::Client;
use crate::service::{Event, TurnRequest};
use crate::session::{Kind, Status};

pub(crate) struct ServiceBackend {
    client: Arc<Mutex<Client>>,
    /// The session turns go to; created at the first turn.
    session: Option<String>,
    workdir: PathBuf,
    actions: Option<PathBuf>,
    /// What the session's settings were last set to, so a changed
    /// selection or level is sent before the next turn.
    settings: Option<(String, String, String)>,
    /// The running turn, for `cancel`.
    running: Option<String>,
    /// Bumps per approval batch, like the engine's own counter.
    batches: u64,
    /// How to reach the service again after the connection died: for the
    /// local socket, start a server if none answers and open a new client.
    /// `None` for a bridge, whose command the operator restarts.
    reconnect: Option<Reconnect>,
}

/// A way to a fresh client. The session id is kept, so the server reopens
/// the same session from disk and the chat continues where it was.
pub(crate) type Reconnect = Box<dyn FnMut() -> Result<Client, String> + Send>;

impl ServiceBackend {
    pub(crate) fn new(
        client: Client,
        session: Option<String>,
        workdir: PathBuf,
        actions: Option<PathBuf>,
    ) -> Self {
        Self {
            client: Arc::new(Mutex::new(client)),
            session,
            workdir,
            actions,
            settings: None,
            running: None,
            batches: 0,
            reconnect: None,
        }
    }

    /// Reconnect through the local socket: `ensure_running`, then a new
    /// client, then the version check, on the next turn after a dead one.
    pub(crate) fn with_local_reconnect(
        mut self,
        state: crate::directories::Directories,
        socket: PathBuf,
    ) -> Self {
        self.reconnect = Some(Box::new(move || {
            let path = crate::serve::ensure_running(&state, Some(&socket))
                .map_err(|err| err.to_string())?;
            let mut client = Client::open(&state, Some(&path), false).map_err(|err| err.message)?;
            let health = client
                .call("health", json!({}))
                .map_err(|err| err.message)?;
            crate::serve::check_version(health["version"].as_str().unwrap_or("?"), &path)
                .map_err(|err| err.to_string())?;
            Ok(client)
        }));
        self
    }

    #[cfg(test)]
    fn with_reconnect(mut self, reconnect: Reconnect) -> Self {
        self.reconnect = Some(reconnect);
        self
    }

    /// Replace the dead client with a fresh one, when there is a way to one.
    /// The settings cache is dropped so the next turn reads them from the
    /// server, which reopened the session from disk.
    fn reconnect(&mut self) -> Result<(), String> {
        let Some(reconnect) = self.reconnect.as_mut() else {
            return Err("the connection to the service closed; restart the chat".to_string());
        };
        let client = reconnect()?;
        if let Ok(mut slot) = self.client.lock() {
            *slot = client;
        }
        self.settings = None;
        Ok(())
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let mut client = self
            .client
            .lock()
            .map_err(|_| "client poisoned".to_string())?;
        client.call(method, params).map_err(|err| err.message)
    }

    /// Make sure a session exists and its settings match the request.
    fn prepare(&mut self, request: &ChatRequest) -> Result<String, String> {
        let wanted = (
            request.selection.gateway.clone(),
            request.selection.model.clone(),
            request.thinking.as_str().to_string(),
        );
        let session = match &self.session {
            Some(id) => id.clone(),
            None => {
                let header = self.call(
                    "session.create",
                    json!({
                        "cwd": self.workdir.display().to_string(),
                        "gateway": wanted.0,
                        "model": wanted.1,
                        "thinking": wanted.2,
                    }),
                )?;
                let id = header["id"]
                    .as_str()
                    .ok_or("the service returned no session id")?
                    .to_string();
                self.session = Some(id.clone());
                self.settings = Some(wanted.clone());
                id
            }
        };
        if self.settings.is_none() {
            let info = self.call("session.get", json!({"session": session}))?;
            let settings = &info["settings"];
            self.settings = Some((
                settings["gateway"].as_str().unwrap_or_default().to_string(),
                settings["model"].as_str().unwrap_or_default().to_string(),
                settings["thinking"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            ));
        }
        if self.settings.as_ref() != Some(&wanted) {
            let current = self.settings.clone().unwrap_or_default();
            let mut params = json!({"session": session});
            if (current.0, current.1) != (wanted.0.clone(), wanted.1.clone()) {
                params["gateway"] = json!(wanted.0);
                params["model"] = json!(wanted.1);
            }
            if current.2 != wanted.2 {
                params["thinking"] = json!(wanted.2);
            }
            self.call("session.settings", params)?;
            self.settings = Some(wanted);
        }
        Ok(session)
    }
    /// Settings, then a subscription from the tip, then the turn: the
    /// session id and the run id, or the first refusal.
    fn start_turn(
        &mut self,
        request: &ChatRequest,
        prompt: &str,
    ) -> Result<(String, String), String> {
        let session = self.prepare(request)?;
        let turn = TurnRequest {
            prompt: prompt.to_string(),
            run_id: None,
            prompt_sha256: None,
            workdir: Some(self.workdir.clone()),
            tools: self.actions.clone(),
            timeout: None,
            thinking: Some(request.thinking.as_str().to_string()),
            unattended: false,
            origin: "tui",
            client: "ahi tui".to_string(),
        };
        let info = self.call("session.get", json!({"session": session}))?;
        let tip = info["tip"].as_u64().unwrap_or(0);
        self.call(
            "session.subscribe",
            json!({"session": session, "after": tip}),
        )?;
        let answer = self.call(
            "turn.submit",
            crate::service::wire::turn_params(&session, &turn),
        )?;
        let run_id = answer["run_id"].as_str().unwrap_or_default().to_string();
        Ok((session, run_id))
    }
}

impl CompletionBackend for ServiceBackend {
    fn submit(&mut self, request: ChatRequest) -> Receiver<ResponseEvent> {
        let (sender, receiver) = mpsc::channel();
        let prompt = request
            .messages
            .iter()
            .rev()
            .find(|message| !message.role.is_notice())
            .filter(|message| message.role == TranscriptRole::Operator)
            .map(|message| message.text.clone());
        let Some(prompt) = prompt else {
            let _ = sender.send(ResponseEvent::Failed {
                message: "nothing to send: the last message is not yours".to_string(),
                kind: ErrorKind::Other,
            });
            return receiver;
        };
        // Prepare, subscribe from the tip, submit. A dead server fails the
        // first call of that; one reconnect and one retry, then the failure
        // is the operator's to read.
        let (session, run_id) = match self.start_turn(&request, &prompt) {
            Ok(started) => started,
            Err(first) => match self
                .reconnect()
                .and_then(|()| self.start_turn(&request, &prompt))
            {
                Ok(started) => started,
                Err(message) => {
                    let _ = sender.send(ResponseEvent::Failed {
                        message: if message == first {
                            message
                        } else {
                            format!("{first}; {message}")
                        },
                        kind: ErrorKind::Transport,
                    });
                    return receiver;
                }
            },
        };
        self.running = Some(run_id.clone());
        let client = Arc::clone(&self.client);
        let events = self
            .client
            .lock()
            .map(|client| client.events())
            .unwrap_or_default();
        let batch_base = self.batches;
        self.batches += 64;
        thread::spawn(move || follow(client, events, session, run_id, batch_base, sender));
        receiver
    }

    fn cancel(&mut self) {
        if let (Some(session), Some(run_id)) = (&self.session, self.running.take()) {
            let _ = self.call("turn.cancel", json!({"session": session, "run_id": run_id}));
        }
    }

    fn reset(&mut self) {
        self.session = None;
        self.settings = None;
        self.running = None;
    }
}

/// Turn the session's events into the screen's events until the turn closes.
fn follow(
    client: Arc<Mutex<Client>>,
    events: crate::service::client::Events,
    session: String,
    run_id: String,
    mut batch: u64,
    sender: Sender<ResponseEvent>,
) {
    let mut last_seq = 0u64;
    loop {
        // Wait on the events alone, never holding the client: the approval
        // thread needs it to answer, and so does `cancel`.
        let event = crate::service::client::next_event(&events, Duration::from_millis(100));
        let event = match event {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                let _ = sender.send(ResponseEvent::Failed {
                    message: "the connection to the service closed".to_string(),
                    kind: ErrorKind::Transport,
                });
                return;
            }
        };
        let done = match event {
            Event::Text {
                run_id: ref id,
                delta,
                ..
            } if *id == run_id => sender.send(ResponseEvent::TextDelta(delta)).is_err(),
            Event::Entry { entry, .. } => {
                if let Some(seq) = entry["seq"].as_u64() {
                    last_seq = last_seq.max(seq);
                }
                match entry_event(&entry) {
                    Some(events) => events.into_iter().any(|event| sender.send(event).is_err()),
                    None => false,
                }
            }
            // Fell behind: pick the stream up again from the last entry seen,
            // or tell the screen the turn is lost.
            Event::Lagged {
                session: ref lagged,
            } if *lagged == session => {
                let resubscribed = client.lock().ok().and_then(|mut client| {
                    client
                        .call(
                            "session.subscribe",
                            json!({"session": session, "after": last_seq}),
                        )
                        .ok()
                });
                if resubscribed.is_some() {
                    false
                } else {
                    let _ = sender.send(ResponseEvent::Failed {
                        message: "fell behind the service's events and could not resubscribe"
                            .to_string(),
                        kind: ErrorKind::Transport,
                    });
                    true
                }
            }
            Event::Ask {
                run_id: ref id,
                ask_id,
                calls,
                ..
            } if *id == run_id => {
                batch += 1;
                let (reply, decisions) = mpsc::channel::<Vec<ToolDecision>>();
                let proposed = calls
                    .iter()
                    .map(|call| ProposedAction {
                        call_id: call["call_id"].as_str().unwrap_or_default().to_string(),
                        tool: call["tool"].as_str().unwrap_or_default().to_string(),
                        target: call["target"].as_str().unwrap_or(".").to_string(),
                        grant_prefix: String::new(),
                        detail: call["detail"].as_str().unwrap_or_default().to_string(),
                    })
                    .collect();
                let count = calls.len();
                let answer_to = Arc::clone(&client);
                let answer_session = session.clone();
                thread::spawn(move || {
                    // A dropped reply denies the batch, as the engine does.
                    let allow: Vec<bool> = match decisions.recv() {
                        Ok(decisions) if decisions.len() == count => decisions
                            .into_iter()
                            .map(|decision| {
                                matches!(
                                    decision,
                                    ToolDecision::Approve | ToolDecision::ApproveForSession
                                )
                            })
                            .collect(),
                        _ => vec![false; count],
                    };
                    if let Ok(mut client) = answer_to.lock() {
                        let _ = client.call(
                            "ask.answer",
                            json!({"session": answer_session, "ask_id": ask_id, "allow": allow}),
                        );
                    }
                });
                sender
                    .send(ResponseEvent::ToolApprovalRequested(ApprovalRequest {
                        batch,
                        calls: proposed,
                        reply: ApprovalReply::new(batch, reply),
                    }))
                    .is_err()
            }
            Event::Turn {
                run_id: ref id,
                status,
                result,
                ..
            } if *id == run_id => {
                if let Some(usage) = result.get("usage").filter(|usage| !usage.is_null()) {
                    let _ = sender.send(ResponseEvent::Usage {
                        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
                        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
                        cached_input_tokens: usage["cached_input_tokens"].as_u64(),
                        cost_usd: usage["cost_usd"].as_f64(),
                    });
                }
                let final_event = match status {
                    Status::Ok => ResponseEvent::Completed,
                    Status::Cancelled => ResponseEvent::Failed {
                        message: "response cancelled".to_string(),
                        kind: ErrorKind::Other,
                    },
                    Status::Timeout => ResponseEvent::Failed {
                        message: "the turn timed out".to_string(),
                        kind: ErrorKind::Other,
                    },
                    Status::Failed => ResponseEvent::Failed {
                        message: result["error"]["message"]
                            .as_str()
                            .unwrap_or("the turn failed")
                            .to_string(),
                        kind: match result["error"]["kind"].as_str() {
                            Some("auth") => ErrorKind::Auth,
                            Some("policy") => ErrorKind::Policy,
                            Some("transport") => ErrorKind::Transport,
                            Some("protocol") => ErrorKind::Protocol,
                            Some("limit") => ErrorKind::Limit,
                            _ => ErrorKind::Other,
                        },
                    },
                };
                let _ = sender.send(final_event);
                return;
            }
            _ => false,
        };
        if done {
            return;
        }
    }
}

/// The screen events an entry stands for: a tool round's request, or one
/// result. Operator and plain assistant lines are already on screen.
fn entry_event(entry: &Value) -> Option<Vec<ResponseEvent>> {
    let entry = crate::session::Entry::from_json(entry).ok()?;
    let Kind::Message(message) = entry.kind else {
        return None;
    };
    match (message.role, message.tool) {
        (
            crate::session::Role::Assistant,
            Some(crate::chat::transcript::ToolPayload::Calls { calls, native }),
        ) => Some(vec![ResponseEvent::ToolCallsRequested { calls, native }]),
        (
            crate::session::Role::Tool { exit_code },
            Some(crate::chat::transcript::ToolPayload::Result {
                call_id,
                name,
                content,
            }),
        ) => Some(vec![
            ResponseEvent::ToolResult {
                call_id,
                tool: name,
                output: message.text,
                content,
                exit_code,
                detail: None,
                elapsed: Duration::ZERO,
                record: None,
            },
            ResponseEvent::ToolCallReturned,
        ]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::transcript::ChatMessage;
    use crate::directories::Directories;
    use std::fs;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    /// One SSE response per connection, in order.
    fn gateway(bodies: Vec<String>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            for body in bodies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut discard = vec![0; length];
                reader.read_exact(&mut discard).unwrap();
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .unwrap();
            }
        });
        (url, handle)
    }

    fn text_stream(text: &str) -> String {
        format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices": [{"index": 0, "delta": {"content": text}, "finish_reason": "stop"}]})
        )
    }

    fn calls_stream(name: &str, arguments: Value) -> String {
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "type": "function",
                 "function": {"name": name, "arguments": arguments.to_string()}}]}}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})
        )
    }

    fn request(prompt: &str) -> ChatRequest {
        ChatRequest {
            selection: super::super::parse_selection("local/vendor/model"),
            thinking: crate::chat::thinking::ThinkingLevel::Off,
            messages: vec![ChatMessage::new(TranscriptRole::Operator, prompt)],
        }
    }

    fn collect(
        receiver: &Receiver<ResponseEvent>,
        mut on_ask: impl FnMut(ApprovalRequest),
    ) -> Vec<String> {
        let mut seen = Vec::new();
        loop {
            let event = receiver
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|err| panic!("{err}; seen so far: {seen:?}"));
            let done = matches!(
                event,
                ResponseEvent::Completed | ResponseEvent::Failed { .. }
            );
            match event {
                ResponseEvent::ToolApprovalRequested(request) => {
                    seen.push(format!("ask {}", request.calls[0].target));
                    on_ask(request);
                }
                ResponseEvent::TextDelta(text) => seen.push(format!("text {text}")),
                ResponseEvent::ToolCallsRequested { calls, .. } => {
                    seen.push(format!("calls {}", calls[0].name))
                }
                ResponseEvent::ToolResult { exit_code, .. } => {
                    seen.push(format!("result {exit_code}"))
                }
                ResponseEvent::Failed { message, .. } => seen.push(format!("failed {message}")),
                other => seen.push(
                    format!("{other:?}")
                        .split_whitespace()
                        .next()
                        .unwrap()
                        .to_string(),
                ),
            }
            if done {
                return seen;
            }
        }
    }

    #[test]
    fn the_screen_backend_runs_turns_through_the_service_and_relays_asks() {
        let (url, gateway) = gateway(vec![
            calls_stream("read_file", json!({"path": "src/a.rs"})),
            text_stream("read it"),
            text_stream("fresh"),
            text_stream("back"),
        ]);
        let dir = crate::fixture_directory::create_unique_fixture_directory("service-backend");
        fs::create_dir_all(dir.join("work/src")).unwrap();
        fs::create_dir_all(dir.join("state")).unwrap();
        fs::write(dir.join("work/src/a.rs"), "fn a() {}\n").unwrap();
        fs::write(
            dir.join("work/ahi.tools.yaml"),
            "version: 1\nrules:\n- {id: reads, action: tool_call, match: {tool: read_file}, effect: approve, ask: true}\n",
        )
        .unwrap();
        fs::write(
            dir.join("state/models.json"),
            format!(
                r#"{{"providers":{{"local":{{"baseUrl":"{url}","api":"openai-completions","apiKey":"gateway","models":[{{"id":"vendor/model"}}]}}}}}}"#
            ),
        )
        .unwrap();
        let state = Directories::single_root(dir.join("state"));
        let client = Client::open(&state, None, true).unwrap();
        let mut backend = ServiceBackend::new(client, None, dir.join("work"), None);

        let receiver = backend.submit(request("read a"));
        let seen = collect(&receiver, |ask| {
            ask.reply.send(vec![ToolDecision::Approve]);
        });
        assert_eq!(
            seen,
            [
                "calls read_file",
                "ask src/a.rs",
                "result 0",
                "ToolCallReturned",
                "text read it",
                "Completed"
            ]
        );
        let session = backend.session.clone().unwrap();
        let store = crate::session::Store::new(&state);
        let stored = store.open(&session).unwrap();
        assert_eq!(
            stored.entries().len(),
            5,
            "operator, calls, result, answer, turn"
        );
        assert_eq!(stored.summary().last_turn.unwrap().1, Status::Ok);
        assert_eq!(
            fs::read_to_string(dir.join("work/src/a.rs")).unwrap(),
            "fn a() {}\n"
        );

        // /clear: the next turn is a new session; a changed model is a setting.
        backend.reset();
        let mut changed = request("new");
        changed.thinking = crate::chat::thinking::ThinkingLevel::High;
        let receiver = backend.submit(changed);
        assert_eq!(collect(&receiver, |_| {}), ["text fresh", "Completed"]);
        let second = backend.session.clone().unwrap();
        assert_ne!(second, session);
        assert_eq!(store.open(&second).unwrap().settings().thinking, "high");
        assert_eq!(store.list().unwrap().0.len(), 2);

        // The server dies: the next turn reconnects and continues the same
        // session, which the fresh service reopens from disk.
        let dead = Client::via(&["false".to_string()]).unwrap();
        *backend.client.lock().unwrap() = dead;
        let reconnect_state = state.clone();
        let mut backend = backend.with_reconnect(Box::new(move || {
            Client::open(&reconnect_state, None, true).map_err(|err| err.message)
        }));
        let mut again = request("again");
        again.thinking = crate::chat::thinking::ThinkingLevel::High;
        let receiver = backend.submit(again);
        assert_eq!(collect(&receiver, |_| {}), ["text back", "Completed"]);
        assert_eq!(backend.session.as_deref(), Some(second.as_str()));
        let continued = store.open(&second).unwrap();
        assert_eq!(
            continued.summary().turns,
            2,
            "the same session, one turn more"
        );
        assert_eq!(
            continued.settings().thinking,
            "high",
            "settings read back from the server"
        );
        // Without a way back, the failure names the remedy.
        let dead = Client::via(&["false".to_string()]).unwrap();
        *backend.client.lock().unwrap() = dead;
        backend.reconnect = None;
        let receiver = backend.submit(request("lost"));
        let seen = collect(&receiver, |_| {});
        assert_eq!(seen.len(), 1);
        assert!(seen[0].contains("restart the chat"), "{}", seen[0]);
        gateway.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
