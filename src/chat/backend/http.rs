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
use std::time::Duration;

use super::protocol::{self, RequestContext, StreamOutcome};
use super::tool_loop::{SessionGrants, ToolRuntime};
use super::{ChatRequest, CompletionBackend, ResponseEvent};
use crate::chat::transcript::{ChatMessage, ToolPayload, TranscriptRole};
use crate::error::{Error, Result};
use crate::harness::{CompletionConnection, CompletionTransport, Protocol};

#[cfg(test)]
mod tests;

pub(crate) struct HttpBackend {
    state: PathBuf,
    /// Tool root and `ahi.actions.yaml` location; `None` is the process's
    /// working directory at each submission.
    workspace: Option<PathBuf>,
    grants: SessionGrants,
    /// One client for the life of the chat: connections are reused across
    /// tool rounds and turns instead of a TLS handshake per request.
    agent: ureq::Agent,
    /// The active turn's cancel flag; `submit` replaces it, `cancel` sets it.
    cancel: Arc<AtomicBool>,
}

impl HttpBackend {
    pub(crate) fn new(state: PathBuf) -> Self {
        Self {
            state,
            workspace: None,
            grants: SessionGrants::default(),
            agent: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout(Duration::from_secs(120))
                .build(),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    #[cfg(test)]
    fn with_workspace(state: PathBuf, workspace: PathBuf) -> Self {
        Self {
            workspace: Some(workspace),
            ..Self::new(state)
        }
    }
}

impl CompletionBackend for HttpBackend {
    fn submit(&mut self, request: ChatRequest) -> Receiver<ResponseEvent> {
        let (sender, receiver) = mpsc::channel();
        let state = self.state.clone();
        let workspace = self.workspace.clone();
        let grants = self.grants.clone();
        let agent = self.agent.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Arc::clone(&cancel);
        std::thread::spawn(move || {
            let workspace = workspace.or_else(|| std::env::current_dir().ok());
            let result = complete(
                &state,
                workspace.as_deref(),
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
                Err(_) if cancel.load(Ordering::Relaxed) => {
                    ResponseEvent::Failed("response cancelled".to_string())
                }
                Err(error) => ResponseEvent::Failed(error.to_string()),
            };
            let _ = sender.send(event);
        });
        receiver
    }

    fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
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

fn complete(
    state: &Path,
    workspace: Option<&Path>,
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
    let mut tools = match workspace {
        Some(directory) => ToolRuntime::load(
            directory,
            state,
            &request.selection.gateway,
            connection.destination(),
            grants,
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
        let response = send(agent, state, &connection, &body)?;
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
                    return Err(Error::policy(format!(
                        "tool execution is disabled: no {} rule approves a builtin tool in this working directory",
                        crate::actions::FILE_NAME
                    )));
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
}

/// Send one request. The browser credential path pins its own destination and
/// handles one bounded refresh; a gateway gets its resolved headers.
fn send(
    agent: &ureq::Agent,
    state: &Path,
    connection: &CompletionConnection,
    body: &[u8],
) -> Result<ureq::Response> {
    match &connection.transport {
        CompletionTransport::OpenAiResponses => {
            crate::harness::inference_response(state, body, agent)
        }
        CompletionTransport::Gateway { url, headers } => {
            let mut call = agent
                .post(url)
                .set("Content-Type", "application/json")
                .set("Accept", "text/event-stream");
            for (name, value) in headers {
                call = call.set(name, value);
            }
            match call.send_bytes(body) {
                Ok(response) if response.status() == 200 => Ok(response),
                Ok(response) => Err(http_error(response.status())),
                Err(ureq::Error::Status(status, _)) => Err(http_error(status)),
                // Transport errors can contain URLs, credentials, or upstream text.
                Err(_) => Err(Error::transport(
                    "model request network, TLS, or timeout error",
                )),
            }
        }
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
    response: ureq::Response,
    sender: &Sender<ResponseEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<StreamOutcome> {
    let status = response.status();
    let media_type = media_type_category(response.header("Content-Type"));
    if status != 200 {
        return Err(Error::transport(format!(
            "model inference failed (HTTP {status}; Content-Type: {media_type})"
        )));
    }
    if protocol::requires_event_stream_type(protocol) && media_type != "text/event-stream" {
        return Err(Error::protocol("model response must be text/event-stream"));
    }
    let reader = CancelReader {
        inner: response.into_reader(),
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
