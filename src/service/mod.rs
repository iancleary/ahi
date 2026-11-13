//! The session service: one object that owns the session store and runs
//! turns through the chat engine, for every front end.
//!
//! `ahi serve` wraps a `Service` in a listener on a Unix socket; `ahi exec`
//! with no server on the host constructs one in its own process and calls
//! the same methods. A session is the resource: a turn is submitted to it,
//! its entries and the streamed text are events a subscriber follows, an
//! approval the policy could not decide is an `ask` event a client
//! answers, and a cancel is a request. One session has at most one open
//! turn. Every turn runs on its own thread through the same `HttpBackend`,
//! `ToolRuntime`, and `Recorder` the chat screen uses.
//!
//! `docs/SERVE.md` has the design.

pub(crate) mod client;
pub(crate) mod ledger;
pub(crate) mod wire;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::chat::backend::{
    ApprovalReply, Caller, ChatRequest, CompletionBackend, HttpBackend, ResponseEvent, Selection,
    ToolDecision,
};
use crate::chat::thinking::ThinkingLevel;
use crate::chat::transcript::{ChatMessage, TranscriptRole};
use crate::directories::Directories;
use crate::error::ErrorKind;
use crate::session::recorder::Recorder;
use crate::session::{Header, Settings, Status, Store, StoreError};
use ledger::Ledger;

/// Events a subscriber keeps for a session.
const SUBSCRIBER_CAPACITY: usize = 256;
/// How long `shutdown` waits for cancelled turns to close.
pub(crate) const GRACE: Duration = Duration::from_secs(5);
/// After a cancel, how long a turn waits for the engine's last word before
/// closing on its own.
const CANCEL_GRACE: Duration = Duration::from_secs(3);
/// The answer text kept in a result document.
const TEXT_CAP: usize = 64 * 1024;

/// What a client asks of a session.
#[derive(Debug, Clone)]
pub(crate) struct TurnRequest {
    pub(crate) prompt: String,
    /// Caller-chosen; generated when absent. Idempotent per session.
    pub(crate) run_id: Option<String>,
    /// Hex SHA-256 the caller computed; the turn is refused when the prompt
    /// does not match it.
    pub(crate) prompt_sha256: Option<String>,
    /// Tool root. Must be a directory.
    pub(crate) workdir: PathBuf,
    /// An explicit policy file in place of the workdir's `ahi.tools.yaml`.
    pub(crate) tools: Option<PathBuf>,
    pub(crate) timeout: Option<Duration>,
    /// Overrides the session's thinking level for this turn.
    pub(crate) thinking: Option<String>,
    /// Answer every `ask` as denied at once: no human is attached.
    pub(crate) unattended: bool,
    /// The front end, for the result and the audit log: `exec` or `serve`.
    pub(crate) origin: &'static str,
    /// A client-supplied name, for telling a space from a shell.
    pub(crate) client: String,
}

/// The answer to `submit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Submitted {
    Running {
        run_id: String,
    },
    /// The session already holds a turn with this `run_id` and prompt: its
    /// result document, no model call made.
    Replayed(Value),
}

/// What a subscriber receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    /// A line written to the session.
    Entry {
        session: String,
        entry: Value,
    },
    /// Streamed assistant text that is not yet an entry.
    Text {
        session: String,
        run_id: String,
        delta: String,
    },
    /// Calls waiting on a human; answered with `answer`.
    Ask {
        session: String,
        run_id: String,
        ask_id: String,
        calls: Vec<Value>,
    },
    /// The turn closed; `result` is its document.
    Turn {
        session: String,
        run_id: String,
        status: Status,
        result: Value,
    },
    Keepalive,
}

/// Why a request was refused. `kind` is one of the result document's
/// error kinds plus `not_found` and `conflict`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceError {
    pub(crate) kind: &'static str,
    pub(crate) message: String,
}

impl ServiceError {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl From<StoreError> for ServiceError {
    fn from(err: StoreError) -> Self {
        let kind = match err {
            StoreError::NotFound(_) => "not_found",
            StoreError::Conflict(_) => "conflict",
            _ => "session",
        };
        Self::new(kind, err.to_string())
    }
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub(crate) type ServiceResult<T> = Result<T, ServiceError>;

#[derive(Clone)]
pub(crate) struct Service {
    inner: Arc<Inner>,
}

struct Inner {
    state: Directories,
    store: Store,
    lives: Mutex<HashMap<String, Arc<Mutex<Live>>>>,
}

/// One session this process has open.
struct Live {
    recorder: Recorder,
    open: Option<OpenTurn>,
    subscribers: Vec<SyncSender<Event>>,
    /// Approvals waiting on a client, by ask id.
    asks: HashMap<String, ApprovalReply>,
    /// Result documents of turns this process closed, by run id.
    results: HashMap<String, Value>,
}

struct OpenTurn {
    run_id: String,
    backend: Arc<Mutex<HttpBackend>>,
    cancelled: bool,
}

impl Service {
    pub(crate) fn open(state: &Directories) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: state.clone(),
                store: Store::new(state),
                lives: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Where sessions live, for `health`.
    pub(crate) fn store_dir(&self) -> &Path {
        self.inner.store.dir()
    }

    /// Sessions with a turn running.
    pub(crate) fn open_turns(&self) -> usize {
        lock(&self.inner.lives)
            .values()
            .filter(|live| lock(live).open.is_some())
            .count()
    }

    pub(crate) fn create_session(
        &self,
        cwd: &Path,
        gateway: &str,
        model: &str,
        thinking: &str,
    ) -> ServiceResult<Header> {
        if ThinkingLevel::parse(thinking).is_none() {
            return Err(ServiceError::new(
                "usage",
                format!("unknown thinking level `{thinking}`"),
            ));
        }
        let header = Header::new(&cwd.display().to_string(), gateway, model, thinking);
        let session = self.inner.store.create(header.clone())?;
        lock(&self.inner.lives).insert(
            header.id.clone(),
            Arc::new(Mutex::new(Live::new(Recorder::resumed(session, "serve")))),
        );
        Ok(header)
    }

    pub(crate) fn list_sessions(&self) -> ServiceResult<Vec<Value>> {
        let (rows, _) = self.inner.store.list()?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let mut object = row.header.to_json();
                object["entries"] = json!(row.entries);
                object["turns"] = json!(row.turns);
                if let Some((run_id, status)) = row.last_turn {
                    object["last_status"] = json!(status.as_str());
                    object["last_run_id"] = json!(run_id);
                }
                object
            })
            .collect())
    }

    /// Header, tip, active parent, settings, and the open turn's run id.
    pub(crate) fn session_info(&self, id: &str) -> ServiceResult<Value> {
        let live = self.live(id)?;
        let live = lock(&live);
        let session = live.session();
        let settings = session.settings();
        Ok(json!({
            "header": session.header().to_json(),
            "tip": session.tip(),
            "active_parent": session.active_parent(),
            "settings": {"gateway": settings.gateway, "model": settings.model, "thinking": settings.thinking},
            "open_run_id": live.open.as_ref().map(|open| open.run_id.clone()),
        }))
    }

    /// Entries after `after` in write order, or the branch ending at
    /// `branch` back to the root.
    pub(crate) fn entries(
        &self,
        id: &str,
        after: u64,
        branch: Option<u64>,
    ) -> ServiceResult<Vec<Value>> {
        let live = self.live(id)?;
        let live = lock(&live);
        let session = live.session();
        Ok(match branch {
            Some(seq) => session
                .path_to_root(Some(seq))
                .into_iter()
                .map(|entry| entry.to_json())
                .collect(),
            None => session
                .entries_after(after)
                .iter()
                .map(|entry| entry.to_json())
                .collect(),
        })
    }

    /// Follow a session: every entry after `after` is delivered first, then
    /// everything as it happens. A subscriber that falls `SUBSCRIBER_CAPACITY`
    /// events behind is dropped and resumes with the last `seq` it saw.
    pub(crate) fn subscribe(&self, id: &str, after: u64) -> ServiceResult<Receiver<Event>> {
        let live = self.live(id)?;
        let mut live = lock(&live);
        let (sender, receiver) = mpsc::sync_channel(SUBSCRIBER_CAPACITY);
        for entry in live.session().entries_after(after) {
            // The channel is empty and bounded; past its capacity the caller
            // should page with `entries` first.
            if sender
                .try_send(Event::Entry {
                    session: id.to_string(),
                    entry: entry.to_json(),
                })
                .is_err()
            {
                break;
            }
        }
        live.subscribers.push(sender);
        Ok(receiver)
    }

    /// Change the session's model or thinking level between turns.
    pub(crate) fn settings(
        &self,
        id: &str,
        gateway_model: Option<(&str, &str)>,
        thinking: Option<&str>,
    ) -> ServiceResult<Settings> {
        let live = self.live(id)?;
        let mut live = lock(&live);
        if live.open.is_some() {
            return Err(ServiceError::new(
                "conflict",
                "a turn is running; settings change between turns",
            ));
        }
        if let Some((gateway, model)) = gateway_model {
            live.recorder.model_changed(&Selection {
                gateway: gateway.to_string(),
                model: model.to_string(),
            });
        }
        if let Some(level) = thinking {
            if ThinkingLevel::parse(level).is_none() {
                return Err(ServiceError::new(
                    "usage",
                    format!("unknown thinking level `{level}`"),
                ));
            }
            live.recorder.thinking_changed(level);
        }
        live.fan_out_fresh(id);
        Ok(live.session().settings())
    }

    /// Run a turn on the session, or answer a repeated `run_id` from the
    /// record. The turn runs on its own thread; follow it with `subscribe`
    /// or poll `turn`.
    pub(crate) fn submit(&self, id: &str, request: TurnRequest) -> ServiceResult<Submitted> {
        let live_arc = self.live(id)?;
        let mut live = lock(&live_arc);
        let digest = crate::session::sha256_hex(request.prompt.as_bytes());
        if request
            .prompt_sha256
            .as_deref()
            .is_some_and(|given| !given.eq_ignore_ascii_case(&digest))
        {
            return Err(ServiceError::new(
                "usage",
                "the prompt does not match prompt_sha256; it may have arrived changed",
            ));
        }
        if let Some(run_id) = request.run_id.as_deref() {
            if let Some(found) = live.result_of(run_id) {
                let recorded = found["prompt_sha256"].as_str().unwrap_or_default();
                if !recorded.is_empty() && recorded != digest {
                    return Err(ServiceError::new(
                        "conflict",
                        format!("run_id {run_id} was already used for a different prompt"),
                    ));
                }
                return Ok(Submitted::Replayed(found));
            }
            if live.open.as_ref().is_some_and(|open| open.run_id == run_id) {
                return Err(ServiceError::new(
                    "conflict",
                    format!("run_id {run_id} is running"),
                ));
            }
        }
        if live.open.is_some() {
            return Err(ServiceError::new(
                "conflict",
                "a turn is running on this session",
            ));
        }
        let workdir = request
            .workdir
            .canonicalize()
            .ok()
            .filter(|path| path.is_dir())
            .ok_or_else(|| {
                ServiceError::new(
                    "usage",
                    format!("workdir {} is not a directory", request.workdir.display()),
                )
            })?;
        if let Some(tools) = &request.tools {
            crate::chat::check_actions_file(tools)
                .map_err(|err| ServiceError::new("policy", err.to_string()))?;
            if tools
                .canonicalize()
                .is_ok_and(|path| path.starts_with(&workdir))
            {
                return Err(ServiceError::new(
                    "usage",
                    "the policy file must lie outside the working directory",
                ));
            }
        }
        let settings = live.session().settings();
        let thinking = match request.thinking.as_deref() {
            Some(level) => ThinkingLevel::parse(level).ok_or_else(|| {
                ServiceError::new("usage", format!("unknown thinking level `{level}`"))
            })?,
            None => ThinkingLevel::parse(&settings.thinking).unwrap_or(ThinkingLevel::Off),
        };
        // The connection is resolved again inside the engine; resolving it
        // here first turns a bad model or credential into a refusal instead
        // of a failed turn, so nothing is written for it.
        crate::harness::completion_connection(
            &self.inner.state,
            &settings.gateway,
            &settings.model,
        )
        .map_err(|err| {
            ServiceError::new(
                if err.kind() == ErrorKind::Auth {
                    "auth"
                } else {
                    "config"
                },
                err.to_string(),
            )
        })?;
        let run_id = request
            .run_id
            .clone()
            .unwrap_or_else(crate::session::run_id);
        let backend = HttpBackend::new(self.inner.state.clone())
            .in_workspace(workdir)
            .with_actions(request.tools.clone())
            .with_caller(Caller {
                origin: request.origin,
                run_id: Some(run_id.clone()),
            });
        let deadline = request.timeout.map(|timeout| Instant::now() + timeout);
        let backend = match deadline {
            Some(deadline) => backend.with_deadline(deadline),
            None => backend,
        };
        let backend = Arc::new(Mutex::new(backend));
        live.open = Some(OpenTurn {
            run_id: run_id.clone(),
            backend: Arc::clone(&backend),
            cancelled: false,
        });
        let turn = Turn {
            service: self.inner.clone(),
            session: id.to_string(),
            live: Arc::clone(&live_arc),
            run_id: run_id.clone(),
            request,
            selection: Selection {
                gateway: settings.gateway,
                model: settings.model,
            },
            thinking,
            backend,
            deadline,
            digest,
        };
        drop(live);
        std::thread::Builder::new()
            .name(format!("turn-{run_id}"))
            .spawn(move || turn.run())
            .map_err(|err| ServiceError::new("session", format!("cannot start the turn: {err}")))?;
        Ok(Submitted::Running { run_id })
    }

    /// The result document of a closed turn, or `None` while it runs.
    pub(crate) fn turn(&self, id: &str, run_id: &str) -> ServiceResult<Option<Value>> {
        let live = self.live(id)?;
        let live = lock(&live);
        if live.open.as_ref().is_some_and(|open| open.run_id == run_id) {
            return Ok(None);
        }
        live.result_of(run_id).map(Some).ok_or_else(|| {
            ServiceError::new("not_found", format!("no turn {run_id} on session {id}"))
        })
    }

    /// Stop the running turn. It closes as `cancelled`.
    pub(crate) fn cancel(&self, id: &str, run_id: &str) -> ServiceResult<()> {
        let live = self.live(id)?;
        let mut live = lock(&live);
        let Some(open) = live.open.as_mut().filter(|open| open.run_id == run_id) else {
            return Err(ServiceError::new(
                "not_found",
                format!("no turn {run_id} is running on session {id}"),
            ));
        };
        open.cancelled = true;
        lock(&open.backend).cancel();
        Ok(())
    }

    /// Answer an `ask` event: one decision per call, in order.
    pub(crate) fn answer(&self, id: &str, ask_id: &str, allow: &[bool]) -> ServiceResult<()> {
        let live = self.live(id)?;
        let mut live = lock(&live);
        let Some(reply) = live.asks.remove(ask_id) else {
            return Err(ServiceError::new(
                "not_found",
                format!("no ask {ask_id} is waiting on session {id}"),
            ));
        };
        reply.send(
            allow
                .iter()
                .map(|&yes| {
                    if yes {
                        ToolDecision::Approve
                    } else {
                        ToolDecision::Deny
                    }
                })
                .collect(),
        );
        Ok(())
    }

    /// Cancel every open turn and wait up to `grace` for them to close.
    pub(crate) fn shutdown(&self, grace: Duration) {
        let lives: Vec<Arc<Mutex<Live>>> = lock(&self.inner.lives).values().cloned().collect();
        for live in &lives {
            let mut live = lock(live);
            if let Some(open) = live.open.as_mut() {
                open.cancelled = true;
                lock(&open.backend).cancel();
            }
        }
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline && lives.iter().any(|live| lock(live).open.is_some()) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn live(&self, id: &str) -> ServiceResult<Arc<Mutex<Live>>> {
        let mut lives = lock(&self.inner.lives);
        if let Some(live) = lives.get(id) {
            return Ok(Arc::clone(live));
        }
        let session = self.inner.store.open(id)?;
        let live = Arc::new(Mutex::new(Live::new(Recorder::resumed(session, "serve"))));
        lives.insert(id.to_string(), Arc::clone(&live));
        Ok(live)
    }
}

impl Live {
    fn new(recorder: Recorder) -> Self {
        Self {
            recorder,
            open: None,
            subscribers: Vec::new(),
            asks: HashMap::new(),
            results: HashMap::new(),
        }
    }

    fn session(&self) -> &crate::session::Session {
        self.recorder
            .session()
            .expect("a live session always holds its store")
    }

    /// A closed turn's document: from this process, or rebuilt from the
    /// record (the `turn` entry's result plus the answer before it).
    fn result_of(&self, run_id: &str) -> Option<Value> {
        if let Some(found) = self.results.get(run_id) {
            return Some(found.clone());
        }
        let session = self.session();
        let entry = session.find_turn(run_id)?;
        let crate::session::Kind::Turn(turn) = &entry.kind else {
            return None;
        };
        let mut doc = turn.result.clone();
        if turn.status == Status::Ok {
            doc["text"] = json!(session.answer_of(entry.seq).unwrap_or_default());
        }
        doc["prompt_sha256"] = json!(turn.prompt_sha256);
        Some(doc)
    }

    fn emit(&mut self, event: Event) {
        self.subscribers
            .retain(|subscriber| subscriber.try_send(event.clone()).is_ok());
    }

    fn fan_out_fresh(&mut self, session: &str) {
        for entry in self.recorder.take_fresh() {
            self.emit(Event::Entry {
                session: session.to_string(),
                entry: entry.to_json(),
            });
        }
    }
}

/// One turn, on its own thread.
struct Turn {
    service: Arc<Inner>,
    session: String,
    live: Arc<Mutex<Live>>,
    run_id: String,
    request: TurnRequest,
    selection: Selection,
    thinking: ThinkingLevel,
    backend: Arc<Mutex<HttpBackend>>,
    deadline: Option<Instant>,
    digest: String,
}

impl Turn {
    fn run(self) {
        let _ = &self.service;
        let receiver = {
            let mut live = lock(&self.live);
            let mut messages = live.session().active_messages(None);
            messages.push(ChatMessage::new(
                TranscriptRole::Operator,
                self.request.prompt.clone(),
            ));
            live.recorder
                .begin_turn_as(self.run_id.clone(), &self.request.prompt);
            live.fan_out_fresh(&self.session);
            lock(&self.backend).submit(ChatRequest {
                selection: self.selection.clone(),
                thinking: self.thinking,
                messages,
            })
        };
        let mut ledger = Ledger::default();
        let mut text = String::new();
        let mut usage = None;
        let mut timed_out = false;
        // Set when the turn was cancelled or timed out: the engine gets
        // `CANCEL_GRACE` to report, then the turn closes without it, since a
        // request blocked on a silent server ends only at its own timeout.
        let mut closing_at: Option<Instant> = None;
        let mut outcome: Option<Result<(), (ErrorKind, String)>> = None;
        while outcome.is_none() {
            let now = Instant::now();
            if closing_at.is_some_and(|at| now >= at) {
                outcome = Some(Err((ErrorKind::Other, "cancelled".into())));
                break;
            }
            let wait = self
                .deadline
                .map(|deadline| deadline.saturating_duration_since(now))
                .unwrap_or(Duration::from_secs(1))
                .min(Duration::from_millis(250));
            match receiver.recv_timeout(wait) {
                Ok(event) => self.handle(event, &mut ledger, &mut text, &mut usage, &mut outcome),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    outcome = Some(Err((
                        ErrorKind::Other,
                        "the turn ended without a result".into(),
                    )))
                }
            }
            if closing_at.is_none() {
                let cancelled = lock(&self.live)
                    .open
                    .as_ref()
                    .is_some_and(|open| open.cancelled);
                let past_deadline = self
                    .deadline
                    .is_some_and(|deadline| Instant::now() >= deadline);
                if cancelled || past_deadline {
                    timed_out = past_deadline && !cancelled;
                    lock(&self.backend).cancel();
                    closing_at = Some(Instant::now() + CANCEL_GRACE);
                }
            }
        }
        // Nothing the engine says after this point is recorded.
        drop(receiver);
        ledger.close();
        let mut live = lock(&self.live);
        let cancelled = live.open.as_ref().is_some_and(|open| open.cancelled);
        let (status, error) = match outcome.expect("the loop ends with an outcome") {
            Ok(()) => (Status::Ok, None),
            Err(_) if cancelled => (Status::Cancelled, None),
            Err(_) if timed_out => (Status::Timeout, None),
            Err((kind, message)) => (Status::Failed, Some((kind, message))),
        };
        let mut doc = json!({
            "schema": 1,
            "ahi_version": crate::VERSION,
            "run_id": self.run_id,
            "session": self.session,
            "status": status.as_str(),
            "origin": self.request.origin,
            "client": self.request.client,
            "gateway": self.selection.gateway,
            "model": self.selection.model,
        });
        if let Some((kind, message)) = &error {
            doc["error"] = json!({
                "kind": error_kind_name(*kind),
                "message": message,
            });
        }
        if let Some(usage) = &usage {
            doc["usage"] = usage.clone();
        }
        for (key, value) in ledger.to_json().as_object().into_iter().flatten() {
            doc[key] = value.clone();
        }
        // The record holds the document without the text, which is the
        // assistant message before the turn line.
        live.recorder.finish_turn(status, doc.clone());
        live.fan_out_fresh(&self.session);
        if status == Status::Ok {
            let mut answer = text;
            let truncated = cap(&mut answer);
            doc["text"] = json!(answer);
            doc["text_truncated"] = json!(truncated);
        }
        doc["prompt_sha256"] = json!(self.digest);
        live.results.insert(self.run_id.clone(), doc.clone());
        live.asks.clear();
        live.open = None;
        live.emit(Event::Turn {
            session: self.session.clone(),
            run_id: self.run_id.clone(),
            status,
            result: doc,
        });
    }

    fn handle(
        &self,
        event: ResponseEvent,
        ledger: &mut Ledger,
        text: &mut String,
        usage: &mut Option<Value>,
        outcome: &mut Option<Result<(), (ErrorKind, String)>>,
    ) {
        let mut live = lock(&self.live);
        match event {
            ResponseEvent::TextDelta(delta) => {
                live.recorder.text(&delta);
                if text.len() < TEXT_CAP {
                    text.push_str(&delta);
                }
                live.emit(Event::Text {
                    session: self.session.clone(),
                    run_id: self.run_id.clone(),
                    delta,
                });
            }
            ResponseEvent::ToolCallsRequested { calls, native } => {
                ledger.requested(calls.len());
                live.recorder.calls(&calls, native.as_ref());
                live.fan_out_fresh(&self.session);
            }
            ResponseEvent::ToolApprovalRequested(request) => {
                if self.request.unattended || live.subscribers.is_empty() {
                    ledger.unattended(request.calls.len());
                    request
                        .reply
                        .send(vec![ToolDecision::Unattended; request.calls.len()]);
                } else {
                    let ask_id = format!("{}-{}", self.run_id, request.batch);
                    let calls = request
                        .calls
                        .iter()
                        .map(|call| {
                            json!({
                                "call_id": call.call_id,
                                "tool": call.tool,
                                "target": call.target,
                                "detail": call.detail,
                            })
                        })
                        .collect();
                    live.asks.insert(ask_id.clone(), request.reply);
                    live.emit(Event::Ask {
                        session: self.session.clone(),
                        run_id: self.run_id.clone(),
                        ask_id,
                        calls,
                    });
                }
            }
            ResponseEvent::ToolResult {
                call_id,
                tool,
                output,
                content,
                exit_code,
                record,
                ..
            } => {
                ledger.returned(exit_code, record.as_deref());
                live.recorder
                    .tool_result(&call_id, &tool, &output, &content, exit_code);
                live.fan_out_fresh(&self.session);
            }
            ResponseEvent::Usage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                cost_usd,
            } => {
                *usage = Some(json!({
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens,
                    "cached_input_tokens": cached_input_tokens,
                    "cost_usd": cost_usd,
                }));
            }
            ResponseEvent::ToolCallReturned
            | ResponseEvent::ContextWindow(_)
            | ResponseEvent::Reconnecting { .. } => {}
            ResponseEvent::Completed => *outcome = Some(Ok(())),
            ResponseEvent::Failed { message, kind } => *outcome = Some(Err((kind, message))),
        }
    }
}

/// The result document's name for an engine failure.
pub(crate) fn error_kind_name(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Auth => "auth",
        ErrorKind::Policy => "policy",
        ErrorKind::Transport => "transport",
        ErrorKind::Protocol => "protocol",
        ErrorKind::Limit => "limit",
        ErrorKind::Other => "turn",
    }
}

/// Cut `text` at `TEXT_CAP` on a character boundary. Returns whether it was cut.
fn cap(text: &mut String) -> bool {
    if text.len() <= TEXT_CAP {
        return false;
    }
    let mut cut = TEXT_CAP;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    true
}

/// A poisoned lock means a turn thread panicked while holding it; the data
/// is still the data, and a panic is a bug to see, not a reason to hang.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
