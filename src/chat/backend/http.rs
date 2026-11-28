//! The turn loop: resolve the connection, send a request, decode the stream,
//! run approved builtin tools, and continue the model until it stops.
//!
//! The loop is protocol-neutral. `protocol` serializes and decodes; `tool_loop`
//! decides and executes; `harness` resolves destinations and credentials. No
//! automatic retry of generation, and no redirect following.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::protocol::{self, RequestContext, StreamOutcome};
use super::tool_loop::{Caller, ToolRuntime, TurnContext};
use super::{ChatRequest, CompletionBackend, ResponseEvent};
use crate::chat::transcript::{ChatMessage, ToolPayload, TranscriptRole};
use crate::directories::Directories;
use crate::error::{Error, ErrorKind, Result};
use crate::harness::{CompletionConnection, CompletionTransport, Credential, Protocol};
use crate::tool_policy::grants::SessionGrants;

/// Connection attempts per request when a gateway cannot be reached, and the
/// pauses between them. Only failures to connect are retried: a request the
/// server may have received is never sent twice.
const CONNECT_ATTEMPTS: u8 = 3;
const CONNECT_BACKOFF: [Duration; 2] = [Duration::from_millis(250), Duration::from_millis(750)];
/// Bounded connection setup: DNS, proxy negotiation, and the TLS handshake.
/// Interactive generation has no total deadline once headers arrive.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long one transport read may sit silent before the reader rechecks
/// cancellation. A poll expiry is not a failure: interactive requests have
/// no total budget, so legitimate silent prefill or reasoning waits as long
/// as the provider needs. The poll is what lets a cancel close a silent
/// connection instead of parking on it until the peer speaks.
const READ_POLL: Duration = Duration::from_millis(250);
/// Bounds only the wait for response headers, not the streamed generation.
const HEADER_TIMEOUT: Duration = Duration::from_secs(300);
mod polling;

#[cfg(test)]
mod tests;

pub(crate) struct HttpBackend {
    state: Directories,
    /// Tool root and `ahi.tools.yaml` location; `None` is the process's
    /// working directory at each submission.
    workspace: Option<PathBuf>,
    /// An explicit policy file (`ahi tui --tools`) used instead of the
    /// workspace's `ahi.tools.yaml`.
    actions: Option<PathBuf>,
    /// Named in every audit line a tool round appends.
    caller: Caller,
    /// Bounds every tool call of a turn; `None` is the chat's unbounded turn.
    deadline: Option<Instant>,
    /// `false` is a hard ceiling: no tool root is resolved, no policy file is
    /// read, and the model is offered no tool schema. Used by the delegation
    /// worker, whose scratch directory must not be able to raise its powers.
    tools_enabled: bool,
    local: Option<crate::delegation::local::Profile>,
    frozen: Option<crate::tool_policy::ActionPolicy>,
    grants: SessionGrants,
    scratch: crate::tools::Scratch,
    #[cfg(test)]
    agent_timeout: Option<Duration>,
    /// Per-read poll of the interactive agent; tests shrink it to exercise
    /// silent-stream timing at CI speed.
    read_poll: Duration,
    /// The active turn's cancel flag; `submit` replaces it, `cancel` sets it.
    cancel: Arc<AtomicBool>,
    /// The active turn's worker, unparked by `cancel` so a backoff wait ends at once.
    worker: Option<std::thread::Thread>,
}

impl HttpBackend {
    pub(crate) fn new(state: Directories) -> Self {
        Self {
            state,
            workspace: None,
            actions: None,
            caller: Caller::default(),
            deadline: None,
            tools_enabled: true,
            local: None,
            frozen: None,
            grants: SessionGrants::default(),
            scratch: Default::default(),
            read_poll: READ_POLL,
            #[cfg(test)]
            agent_timeout: None,
            cancel: Arc::new(AtomicBool::new(false)),
            worker: None,
        }
    }

    /// Shrink the per-read poll for controlled-timing tests.
    #[cfg(test)]
    pub(crate) fn with_read_poll(mut self, read_poll: Duration) -> Self {
        self.read_poll = read_poll;
        self
    }

    /// Read tool policy from `path` instead of the workspace's
    /// `ahi.tools.yaml`. `None` keeps the working-directory lookup.
    pub(crate) fn with_actions(mut self, path: Option<PathBuf>) -> Self {
        self.actions = path;
        self
    }

    /// Confine tools to `directory` and read its `ahi.tools.yaml` (unless
    /// `with_actions` named a file) instead of the process's working
    /// directory.
    pub(crate) fn in_workspace(mut self, directory: PathBuf) -> Self {
        self.workspace = Some(directory);
        self
    }

    pub(crate) fn with_scratch(mut self, scratch: crate::tools::Scratch) -> Self {
        self.scratch = scratch;
        self
    }

    pub(crate) fn with_local_profile(
        mut self,
        local: Option<crate::delegation::local::Profile>,
    ) -> Self {
        self.local = local;
        self
    }

    pub(crate) fn with_frozen_policy(mut self, policy: crate::tool_policy::ActionPolicy) -> Self {
        self.frozen = Some(policy);
        self
    }

    /// Run every turn without tools, whatever the working directory holds.
    /// Unlike an empty workspace this never consults a policy file, so a
    /// planted `ahi.tools.yaml` cannot enable anything.
    pub(crate) fn without_tools(mut self) -> Self {
        self.tools_enabled = false;
        self
    }

    /// Who the audit log says ran each tool.
    pub(crate) fn with_caller(mut self, caller: Caller) -> Self {
        self.caller = caller;
        self
    }

    /// Bound the turn: no tool call starts past `deadline`, a command's own
    /// timeout is clamped to what remains, and a request that has not
    /// answered by then fails soon after instead of waiting out the turn.
    /// The caller still cancels at the deadline; this keeps a call, or a
    /// blocked request, from outliving it by much. The whole remaining time
    /// is available to one request: a long assignment is not silently cut
    /// down to an arbitrary request cap.
    pub(crate) fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }
}

/// The shared client for model requests. An unbounded turn has no total
/// request budget: generation may take as long as the provider needs, with
/// only connection setup bounded and a per-read poll keeping cancellation
/// responsive while the stream is silent. A bounded turn keeps one total
/// budget covering connection, request, and response, so it still fails
/// soon after its deadline rather than hanging on a silent peer.
fn model_agent(
    deadline: Option<Instant>,
    read_poll: Duration,
    cancel: Arc<AtomicBool>,
) -> ureq::Agent {
    let mut config = ureq::Agent::config_builder().max_redirects(0);
    config = match deadline {
        Some(deadline) => config.timeout_global(Some(
            deadline.saturating_duration_since(Instant::now()) + Duration::from_secs(1),
        )),
        None => config
            .timeout_resolve(Some(CONNECT_TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(HEADER_TIMEOUT)),
    };
    polling::PollingConnector {
        poll: read_poll,
        cancel,
    }
    .agent(config.build())
}

impl CompletionBackend for HttpBackend {
    fn submit(&mut self, request: ChatRequest) -> Receiver<ResponseEvent> {
        let (sender, receiver) = mpsc::channel();
        let state = self.state.clone();
        let workspace = self.workspace.clone();
        let actions = self.actions.clone();
        let grants = self.grants.clone();
        let scratch = self.scratch.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        // Pool across tool rounds; never attach a new turn to an old cancel flag.
        let agent = model_agent(self.deadline, self.read_poll, cancel.clone());
        #[cfg(test)]
        let agent = self.agent_timeout.map_or(agent, |short| {
            ureq::Agent::config_builder()
                .timeout_global(Some(short))
                .build()
                .new_agent()
        });
        self.cancel = Arc::clone(&cancel);
        let caller = self.caller.clone();
        let deadline = self.deadline;
        let tools_enabled = self.tools_enabled;
        let frozen = self.frozen.clone();
        let local = self.local.clone();
        let handle = std::thread::spawn(move || {
            let workspace = workspace.or_else(|| std::env::current_dir().ok());
            let result = complete(
                &state,
                ToolSource {
                    local,
                    frozen,
                    workspace: workspace.as_deref(),
                    tools_enabled,
                    actions: actions.as_deref(),
                    caller,
                    scratch,
                    deadline,
                },
                grants,
                &agent,
                request,
                &sender,
                &cancel,
            );
            let event = match result {
                Ok(()) => ResponseEvent::Completed,
                // Any failure after a cancel is the cancel: the dropped
                // connection or the checkpoint, never a provider fault.
                Err(_) if cancel.load(Ordering::Relaxed) => ResponseEvent::Failed {
                    message: "response cancelled".to_string(),
                    kind: ErrorKind::Other,
                },
                Err(error) => ResponseEvent::Failed {
                    message: error.to_string(),
                    kind: error.kind(),
                },
            };
            let _ = sender.send(event);
        });
        self.worker = Some(handle.thread().clone());
        receiver
    }

    fn reset(&mut self) {
        self.scratch = Default::default();
        self.grants = Default::default();
    }

    fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(worker) = &self.worker {
            worker.unpark();
        }
    }
}

/// The response body, read until the turn is cancelled. Each transport read
/// is polled: a silent stretch outlasts the per-read poll without failing,
/// because an interactive request has no total budget and a silent prefill
/// is legitimate. The poll is what makes a cancel, or a turn deadline, land
/// while the upstream is silent; the stream then errors and dropping the
/// reader closes the connection, which is the only cancellation signal
/// these APIs accept.
struct CancelReader<R> {
    inner: R,
    cancel: Arc<AtomicBool>,
}

impl<R: Read> Read for CancelReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let aborted =
            || std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "response cancelled");
        if self.cancel.load(Ordering::Relaxed) {
            return Err(aborted());
        }
        let read = self.inner.read(buf)?;
        if self.cancel.load(Ordering::Relaxed) {
            return Err(aborted());
        }
        Ok(read)
    }
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::new("response cancelled"))
    } else {
        Ok(())
    }
}

/// Where a turn's tool policy comes from: the workspace's `ahi.tools.yaml`
/// unless an explicit `actions` file replaces it.
struct ToolSource<'a> {
    local: Option<crate::delegation::local::Profile>,
    frozen: Option<crate::tool_policy::ActionPolicy>,
    scratch: crate::tools::Scratch,
    /// Tool root; `None` disables tools for the turn.
    workspace: Option<&'a Path>,
    tools_enabled: bool,
    actions: Option<&'a Path>,
    caller: Caller,
    deadline: Option<Instant>,
}

fn complete(
    state: &Directories,
    source: ToolSource<'_>,
    grants: SessionGrants,
    agent: &ureq::Agent,
    request: ChatRequest,
    sender: &Sender<ResponseEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    let connection = crate::harness::completion_connection(
        state,
        &request.selection.gateway,
        &request.selection.model,
    )?;
    let metadata = &connection.metadata;
    if let Some(window) = metadata.context_window {
        protocol::emit(sender, ResponseEvent::ContextWindow(window.get()))?;
    }
    let ToolSource {
        local,
        frozen,
        scratch,
        workspace,
        tools_enabled,
        actions,
        caller,
        deadline,
    } = source;
    let directory = workspace
        .ok_or_else(|| Error::policy("cannot resolve the instruction working directory"))?
        .canonicalize()
        .map_err(|_| Error::policy("cannot resolve the instruction working directory"))?;
    let instruction_files = crate::system_prompt::Instructions::load(&directory)?;
    let child = caller.origin == "delegate";
    let mut rounds = 0usize;
    let mode = request.mode.clone();
    let mut tools = match workspace.filter(|_| tools_enabled) {
        Some(directory) => ToolRuntime::load(
            directory,
            state,
            &request.selection.gateway,
            connection.destination(),
            grants,
            actions,
            TurnContext {
                local,
                frozen,
                scratch,
                mode: request.mode,
                caller,
                deadline,
                cancel: Arc::clone(cancel),
            },
        )?,
        None => None,
    };
    // The request's history is the turn's working transcript; tool exchanges
    // are appended to it, never copied.
    let ChatRequest {
        selection,
        thinking,
        mut messages,
        skills: explicit,
        mode: _,
    } = request;
    if !explicit.is_empty() {
        cancelled(cancel)?;
        let runtime = tools.as_mut().ok_or_else(|| Error::policy(
            "explicit skill loading is disabled: no installed skill is approved by the active tool policy"
        ))?;
        runtime.load_explicit(explicit, &mut messages, sender)?;
    }
    let schemas = tools.as_ref().map(ToolRuntime::schemas);
    let capabilities = match &tools {
        Some(runtime) => runtime.capability_summary(schemas.as_deref().unwrap_or_default()),
        None => {
            crate::tool_policy::ActionPolicy::default().capability_summary(&mode, &directory, &[])
        }
    };
    let instructions = instruction_files.with_capabilities(&capabilities);
    let mut turn = || -> Result<()> {
        loop {
            if child && deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(Error::new("assignment deadline reached"));
            }
            let context = RequestContext::new(
                &selection.model,
                thinking,
                metadata,
                &messages,
                schemas.as_deref(),
                connection.browser_token(),
            )
            .with_instructions(&instructions);
            let body = protocol::request_body(connection.protocol, &context)?;
            cancelled(cancel)?;
            let response = send(agent, state, &connection, &body, sender, cancel, deadline)?;
            let outcome = decode(connection.protocol, &context, response, sender, cancel)?;
            match outcome {
                StreamOutcome::Stopped => return Ok(()),
                StreamOutcome::ToolCalls {
                    text,
                    calls,
                    native,
                } => {
                    protocol::emit(
                        sender,
                        ResponseEvent::ToolCallsRequested {
                            calls: calls.clone(),
                            native: native.clone(),
                        },
                    )?;
                    let Some(tools) = tools.as_mut() else {
                        return Err(Error::policy(match actions {
                            Some(path) => format!(
                                "tool execution is disabled: no rule in {} approves a builtin tool",
                                path.display()
                            ),
                            None => format!(
                                "tool execution is disabled: no {} rule approves a builtin tool in this working directory",
                                crate::tool_policy::FILE_NAME
                            ),
                        }));
                    };
                    cancelled(cancel)?;
                    rounds += 1;
                    if child && rounds > 64 {
                        return Err(Error::policy(
                            "scratch child exceeded 64 tool rounds in one turn",
                        ));
                    }
                    let results = tools.run_round(&calls, sender)?;
                    let mut assistant = ChatMessage::new(TranscriptRole::Assistant, text);
                    assistant.tool = Some(ToolPayload::Calls { calls, native });
                    messages.push(assistant);
                    messages.extend(results);
                }
            }
        }
    };
    turn().map_err(|error| with_credential_hint(error, &connection))
}

/// Name where a gateway credential is configured on an authentication
/// failure. Browser-login messages already say `/login`; gateway hints name a
/// variable or the file, never a value.
fn with_credential_hint(error: Error, connection: &CompletionConnection) -> Error {
    if error.kind() != ErrorKind::Auth
        || matches!(connection.transport, CompletionTransport::OpenAiResponses)
    {
        return error;
    }
    let hint = match &connection.credential {
        Credential::BrowserLogin => return error,
        Credential::Environment(name) => {
            format!("check the {name} environment variable this gateway's credential is read from")
        }
        Credential::Configured => {
            "check the provider's apiKey or headers in models.json".to_string()
        }
        Credential::None => {
            "this gateway is configured without a credential; add apiKey or headers in models.json"
                .to_string()
        }
    };
    Error::auth(format!("{error}; {hint}"))
}

/// Send one request. The browser credential path pins its own destination and
/// handles one bounded refresh; a gateway gets its resolved headers.
fn send(
    agent: &ureq::Agent,
    state: &Directories,
    connection: &CompletionConnection,
    body: &[u8],
    sender: &Sender<ResponseEvent>,
    cancel: &Arc<AtomicBool>,
    deadline: Option<Instant>,
) -> Result<ureq::http::Response<ureq::Body>> {
    let mut attempt = 1;
    let mut reconnect = |exhausted: &str| {
        if attempt >= CONNECT_ATTEMPTS {
            return Err(Error::transport(exhausted));
        }
        let backoff = CONNECT_BACKOFF[usize::from(attempt - 1)];
        attempt += 1;
        protocol::emit(
            sender,
            ResponseEvent::Reconnecting {
                attempt,
                attempts: CONNECT_ATTEMPTS,
            },
        )?;
        wait(backoff, cancel)
    };
    match &connection.transport {
        CompletionTransport::OpenAiResponses => {
            crate::harness::inference_response(&state.data, body, agent, || {
                reconnect("cannot reach OpenAI after bounded connection attempts (DNS or connection failure); check network access")
            })
        }
        CompletionTransport::Gateway { url, headers } => {
            loop {
                let mut call = agent
                    .post(url)
                    .header("Content-Type", "application/json")
                    .header("Accept", "text/event-stream");
                if let Some(deadline) = deadline {
                    // A bounded gateway turn can spend most of its budget awaiting
                    // the first prefill. Share the remaining time across rounds and
                    // connection attempts; retain the existing settlement grace.
                    call = call
                        .config()
                        .timeout_global(Some(
                            deadline.saturating_duration_since(Instant::now())
                                + Duration::from_secs(1),
                        ))
                        .build();
                }
                for (name, value) in headers {
                    call = call.header(name, value);
                }
                match call.send(body) {
                    Ok(response) if response.status().as_u16() == 200 => return Ok(response),
                    Ok(response) => return Err(http_error(response.status().as_u16())),
                    Err(ureq::Error::StatusCode(status)) => return Err(http_error(status)),
                    // DNS and connection failures never reached the server, so
                    // another attempt cannot bill twice. Anything else might
                    // have, and is not retried.
                    Err(error) if crate::harness::connection_not_established(&error) => {
                        reconnect(&format!("cannot reach the gateway after {CONNECT_ATTEMPTS} connection attempts (DNS or connection failure); check its baseUrl and that it is running"))?;
                    }
                    // A silent gateway through connection setup or the wait
                    // for response headers. The request may already have been
                    // processed, so it is not retried.
                    Err(ureq::Error::Timeout(timeout)) => {
                        return Err(Error::transport(match timeout {
                            ureq::Timeout::RecvResponse => {
                                "the gateway did not start responding in time; the request may still have been processed"
                            }
                            _ => "model request network, TLS, or timeout error",
                        }))
                    }
                    // Transport errors can contain URLs, credentials, or upstream text.
                    Err(_) => {
                        return Err(Error::transport(
                            "model request network, TLS, or timeout error",
                        ))
                    }
                }
            }
        }
    }
}

/// Park through a backoff. `cancel` sets the flag and unparks this thread,
/// so the wait ends as soon as the cancel lands rather than at the next poll.
/// Parking can wake spuriously, so the deadline is re-checked each time.
fn wait(backoff: Duration, cancel: &Arc<AtomicBool>) -> Result<()> {
    let deadline = Instant::now() + backoff;
    loop {
        cancelled(cancel)?;
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        std::thread::park_timeout(deadline - now);
    }
}

fn http_error(status: u16) -> Error {
    let message = format!("model request failed (HTTP {status}); no automatic retry was attempted");
    match status {
        401 | 403 => Error::auth(message),
        _ => Error::transport(message),
    }
}

/// Validate the response envelope the protocol cares about, then decode.
fn decode(
    protocol: Protocol,
    context: &RequestContext<'_>,
    response: ureq::http::Response<ureq::Body>,
    sender: &Sender<ResponseEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<StreamOutcome> {
    let status = response.status().as_u16();
    let media_type = media_type_category(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
    );
    if status != 200 {
        return Err(Error::transport(format!(
            "model inference failed (HTTP {status}; Content-Type: {media_type})"
        )));
    }
    if protocol::requires_event_stream_type(protocol) && media_type != "text/event-stream" {
        return Err(Error::protocol("model response must be text/event-stream"));
    }
    let reader = CancelReader {
        inner: response.into_body().into_reader(),
        cancel: Arc::clone(cancel),
    };
    protocol::read_stream(protocol, reader, sender, context).map_err(|error| {
        // Content-Type is not an acceptance gate for framing-validated
        // protocols, but it is the most useful diagnostic when framing fails.
        if protocol == Protocol::OpenAiCompletions {
            error
        } else {
            Error::with_kind(
                format!("{error} (HTTP {status}; Content-Type: {media_type})"),
                error.kind(),
            )
        }
    })
}

/// An allowlisted category, never the raw header value.
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
