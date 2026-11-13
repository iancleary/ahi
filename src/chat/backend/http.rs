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
use super::tool_loop::{Caller, SessionGrants, ToolRuntime, TurnContext};
use super::{ChatRequest, CompletionBackend, ResponseEvent};
use crate::chat::transcript::{ChatMessage, ToolPayload, TranscriptRole};
use crate::directories::Directories;
use crate::error::{Error, ErrorKind, Result};
use crate::harness::{CompletionConnection, CompletionTransport, Credential, Protocol};

/// Connection attempts per request when a gateway cannot be reached, and the
/// pauses between them. Only failures to connect are retried: a request the
/// server may have received is never sent twice.
const CONNECT_ATTEMPTS: u8 = 3;
const CONNECT_BACKOFF: [Duration; 2] = [Duration::from_millis(250), Duration::from_millis(750)];

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
    grants: SessionGrants,
    /// One client for the life of the chat: connections are reused across
    /// tool rounds and turns instead of a TLS handshake per request.
    agent: ureq::Agent,
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
            grants: SessionGrants::default(),
            agent: ureq::Agent::config_builder()
                .max_redirects(0)
                .timeout_global(Some(Duration::from_secs(120)))
                .build()
                .new_agent(),
            cancel: Arc::new(AtomicBool::new(false)),
            worker: None,
        }
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
    #[allow(dead_code)] // `ahi exec --workdir`
    pub(crate) fn in_workspace(mut self, directory: PathBuf) -> Self {
        self.workspace = Some(directory);
        self
    }

    /// Who the audit log says ran each tool.
    #[allow(dead_code)] // `ahi exec` and `ahi serve`
    pub(crate) fn with_caller(mut self, caller: Caller) -> Self {
        self.caller = caller;
        self
    }

    /// Bound every tool call of the turn: none starts past `deadline` and a
    /// command's own timeout is clamped to what remains. The caller still
    /// cancels at the deadline; this keeps a call from outliving it.
    #[allow(dead_code)] // `ahi exec --timeout`
    pub(crate) fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }
}

impl CompletionBackend for HttpBackend {
    fn submit(&mut self, request: ChatRequest) -> Receiver<ResponseEvent> {
        let (sender, receiver) = mpsc::channel();
        let state = self.state.clone();
        let workspace = self.workspace.clone();
        let actions = self.actions.clone();
        let grants = self.grants.clone();
        let agent = self.agent.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Arc::clone(&cancel);
        let caller = self.caller.clone();
        let deadline = self.deadline;
        let handle = std::thread::spawn(move || {
            let workspace = workspace.or_else(|| std::env::current_dir().ok());
            let result = complete(
                &state,
                ToolSource {
                    workspace: workspace.as_deref(),
                    actions: actions.as_deref(),
                    caller,
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

    fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(worker) = &self.worker {
            worker.unpark();
        }
    }
}

/// The response body, read until the turn is cancelled. A read blocked on a
/// silent server returns when its next bytes arrive or the request deadline
/// passes; the error then ends the stream and dropping the reader closes the
/// connection, which is the only cancellation signal these APIs accept.
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
        // Bytes that arrive after the cancel are not parsed: nothing from the
        // abandoned turn reaches the transcript.
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
    /// Tool root; `None` disables tools for the turn.
    workspace: Option<&'a Path>,
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
        workspace,
        actions,
        caller,
        deadline,
    } = source;
    let mut tools = match workspace {
        Some(directory) => ToolRuntime::load(
            directory,
            state,
            &request.selection.gateway,
            connection.destination(),
            grants,
            actions,
            TurnContext {
                caller,
                deadline,
                cancel: Arc::clone(cancel),
            },
        )?,
        None => None,
    };
    // `request` is consumed below; the destructure moves `messages` out.
    let schemas = tools.as_ref().map(ToolRuntime::schemas);
    // The request's history is the turn's working transcript; tool exchanges
    // are appended to it, never copied.
    let ChatRequest {
        selection,
        thinking,
        mut messages,
    } = request;
    let mut turn = || -> Result<()> {
        loop {
            let context = RequestContext::new(
                &selection.model,
                thinking,
                metadata,
                &messages,
                schemas.as_deref(),
                connection.browser_token(),
            );
            let body = protocol::request_body(connection.protocol, &context)?;
            cancelled(cancel)?;
            let response = send(agent, state, &connection, &body, sender, cancel)?;
            let outcome = decode(connection.protocol, metadata, response, sender, cancel)?;
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
) -> Result<ureq::http::Response<ureq::Body>> {
    match &connection.transport {
        CompletionTransport::OpenAiResponses => {
            crate::harness::inference_response(&state.data, body, agent)
        }
        CompletionTransport::Gateway { url, headers } => {
            let mut attempt = 1;
            loop {
                let mut call = agent
                    .post(url)
                    .header("Content-Type", "application/json")
                    .header("Accept", "text/event-stream");
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
                    Err(error) if unreachable(&error) => {
                        if attempt >= CONNECT_ATTEMPTS {
                            return Err(Error::transport(format!(
                                "cannot reach the gateway after {CONNECT_ATTEMPTS} connection attempts (DNS or connection failure); check its baseUrl and that it is running"
                            )));
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
                        wait(backoff, cancel)?;
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

/// Failures that happen before any byte reaches the server, so a retry
/// cannot cause a second billed request.
fn unreachable(error: &ureq::Error) -> bool {
    use std::io::ErrorKind;
    match error {
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => true,
        ureq::Error::Timeout(timeout) => matches!(timeout, ureq::Timeout::Connect),
        ureq::Error::Io(io) => matches!(
            io.kind(),
            ErrorKind::ConnectionRefused
                | ErrorKind::ConnectionReset
                | ErrorKind::NotConnected
                | ErrorKind::AddrNotAvailable
                | ErrorKind::NetworkUnreachable
                | ErrorKind::HostUnreachable
        ),
        _ => false,
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
    metadata: &crate::harness::ModelMetadata,
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
    protocol::read_stream(protocol, reader, sender, metadata).map_err(|error| {
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
