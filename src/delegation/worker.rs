//! The Rex process claims a service assignment, runs the admitted turns and
//! scratch tools until idle, then publishes on its claiming connection.
//!
//! It performs the inference itself through the in-process `HttpBackend`; it
//! does not tail another process or use the parent conversation. It records
//! its own transcript and prints terminal-safe answers and advisory status
//! in the PTY. The service publishes the structured result.

use std::path::Path;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use super::broker::Admission;
use super::{Failure, Observed, Outcome, Status, Usage, MAX_RESULT_TEXT_BYTES};
use crate::chat::backend::{
    Caller, ChatRequest, CompletionBackend, HttpBackend, ResponseEvent, Selection,
};
use crate::chat::thinking::ThinkingLevel;
use crate::chat::transcript::{ChatMessage, ToolPayload, TranscriptRole};
use crate::service::client::Client;
use serde_json::{json, Value};

enum Lease {
    Service(Client),
    #[cfg(test)]
    Fixture,
}

enum Origin {
    Service,
    #[cfg(test)]
    Fixture,
}

/// How often the worker re-reads the receipt for a cancel while streaming.
const CANCEL_POLL: Duration = Duration::from_millis(500);

/// Exit codes of `ahi internal-worker`. The result file, not the code, is the
/// record; the code is for the operator glancing at the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exit {
    /// A result with status `ok` was published.
    Ok = 0,
    /// A result with status `failed` was published.
    Failed = 1,
    /// The arguments or the stored records were refused before any claim.
    Refused = 2,
    /// Another worker holds the claim, or the attempt is stale or cancelled.
    NotClaimed = 3,
    /// A result with status `cancelled` or `timeout` was published.
    Stopped = 6,
    /// The turn ran but no result could be published.
    Unpublished = 8,
}

impl Exit {
    pub(crate) fn code(self) -> u8 {
        self as u8
    }
}

/// Run the worker for `dir` under `attempt`. Returns the exit code.
pub(crate) fn run(dir: &Path, attempt: &str) -> u8 {
    let cwd = match std::env::current_dir() {
        Ok(path) => path,
        Err(_) => return Exit::Refused.code(),
    };
    if !cfg!(unix) {
        return Exit::Refused.code();
    }
    report(reside(dir, attempt, &cwd))
}

fn reside(root: &Path, first: &str, cwd: &Path) -> Result<Exit, (Exit, String)> {
    use super::resident::{self, Transition};
    use crate::program_status::{Reporter, State};
    let fail = |e: super::Error| (Exit::Refused, e.message);
    let r = super::load_dir(root).map_err(fail)?;
    let dirs = r.directories.to_directories();
    let _lease = resident::lease(root).map_err(fail)?;
    // The child can be scheduled before `rex new` returns its full handle.
    let start = Instant::now();
    loop {
        let phase = super::load_dir(root).map_err(fail)?.phase;
        if !matches!(phase, super::Phase::Launching { .. }) {
            break;
        }
        if start.elapsed() > Duration::from_secs(20) {
            return Err((Exit::Refused, "unconfirmed Rex launch".into()));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let reporter = Reporter::for_stdout();
    resident::terminal_input(root.into(), r);
    let mut dir = root.to_owned();
    let mut attempt = first.to_owned();
    loop {
        reporter.state(State::Working, "ahi child", "");
        let exit = execute(&dir, &attempt, cwd, Observed::from_env(), Origin::Service)?;
        reporter.state(
            if exit == Exit::Ok {
                State::Done
            } else {
                State::Error
            },
            "ahi child",
            "",
        );
        if let Some(out) = super::read_outcome(&dir).map_err(fail)? {
            println!(
                "{}",
                out.text
                    .chars()
                    .filter(|c| !c.is_control() || *c == '\n')
                    .collect::<String>()
            );
        }
        println!("Idle. Enter a follow-up or /close.");
        reporter.state(State::Idle, "ahi child", "");
        loop {
            match resident::edge(&dir).map_err(fail)? {
                Some(Transition::Close) => {
                    reporter.clear();
                    let endpoint = resident::socket(root).map_err(fail)?;
                    let mut c = Client::listening(&dirs, &endpoint)
                        .map_err(|e| (Exit::Unpublished, e.message))?;
                    c.call(
                        "child.close",
                        json!(dir.file_name().and_then(|s| s.to_str())),
                    )
                    .map_err(|e| (Exit::Unpublished, e.message))?;
                    return Ok(Exit::Ok);
                }
                Some(Transition::Run(next)) => {
                    dir = super::assignment_dir(&dirs, &next).map_err(fail)?;
                    let r = super::load_dir(&dir).map_err(fail)?;
                    attempt = r
                        .phase
                        .attempt()
                        .ok_or((Exit::Refused, "missing run attempt".into()))?
                        .into();
                    break;
                }
                None => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }
}

#[cfg(test)]
pub(super) fn run_at(dir: &Path, attempt: &str, cwd: &Path, observed: Observed) -> u8 {
    report(execute(dir, attempt, cwd, observed, Origin::Fixture))
}
fn report(result: Result<Exit, (Exit, String)>) -> u8 {
    match result {
        Ok(exit) => exit.code(),
        Err((exit, message)) => {
            eprintln!("error: {message}");
            exit.code()
        }
    }
}

fn execute(
    dir: &Path,
    attempt: &str,
    cwd: &Path,
    observed: Observed,
    origin: Origin,
) -> Result<Exit, (Exit, String)> {
    if !dir.is_absolute() {
        return Err((
            Exit::Refused,
            "--assignment-dir must be absolute".to_string(),
        ));
    }
    let reserved = super::load_dir(dir).map_err(|err| (Exit::Refused, err.message))?;
    let expected = reserved
        .scratch
        .canonicalize()
        .map_err(|_| (Exit::Refused, "missing scratch directory".into()))?;
    if cwd.canonicalize().ok().as_ref() != Some(&expected)
        || !super::resident::session_matches(&reserved, &observed)
        || (super::resident::child_id(dir, &reserved).map_err(|e| (Exit::Refused, e.message))?
            == reserved.id
            && !std::fs::read_dir(&expected).is_ok_and(|mut files| files.next().is_none()))
    {
        return Err((
            Exit::Refused,
            "worker must run in the assigned Rex session and empty scratch directory".into(),
        ));
    }
    let (mut service, admission, claim): (Lease, Admission, super::Claim) = match origin {
        Origin::Service => {
            let token: Value = super::read_json(&dir.join("broker.json"))
                .map_err(|e| (Exit::Refused, e.message))?
                .ok_or((Exit::Refused, "no service credential".into()))?;
            let socket = token["socket"]
                .as_str()
                .ok_or((Exit::Refused, "missing service endpoint".into()))?;
            let mut c =
                Client::listening(&reserved.directories.to_directories(), Path::new(socket))
                    .map_err(|e| (Exit::Refused, e.message))?;
            let v=c.call("child.claim",json!({"id":reserved.id,"attempt":attempt,"nonce":token["nonce"],"cwd":expected,"pid":std::process::id(),"observed":observed})).map_err(|e|(Exit::NotClaimed,e.message))?;
            (
                Lease::Service(c),
                serde_json::from_value(v["admission"].clone())
                    .map_err(|e| (Exit::Refused, e.to_string()))?,
                serde_json::from_value(v["claim"].clone())
                    .map_err(|e| (Exit::Refused, e.to_string()))?,
            )
        }
        #[cfg(test)]
        Origin::Fixture => {
            let (_, claim) =
                super::claim(dir, attempt, &observed).map_err(|e| (Exit::NotClaimed, e.message))?;
            (
                Lease::Fixture,
                Admission {
                    mode: Default::default(),
                    policy: Default::default(),
                    follow_ups: vec![],
                },
                claim,
            )
        }
    };
    let receipt = reserved;
    eprintln!(
        "assignment {}: claimed attempt {attempt} (pid {})",
        receipt.id, claim.pid
    );
    let mut rec =
        super::resident::recorder(dir, &receipt).map_err(|e| (Exit::Unpublished, e.message))?;
    let completed = std::cell::Cell::new(0usize);
    let mut finish = |rec: &mut crate::session::recorder::Recorder,
                      status: Status,
                      text: String,
                      truncated: bool,
                      usage: Option<Usage>,
                      error: Option<Failure>|
     -> Result<Exit, (Exit, String)> {
        let outcome = Outcome {
            turns_completed: completed.get(),
            assignment: receipt.id.clone(),
            attempt: attempt.to_string(),
            status,
            gateway: receipt.gateway.clone(),
            model: receipt.model.clone(),
            text_bytes: text.len() as u64,
            text,
            truncated,
            usage,
            error,
            finished: crate::util::rfc3339::now(),
        };
        rec.finish_turn(
            match status {
                Status::Ok => crate::session::Status::Ok,
                Status::Cancelled => crate::session::Status::Cancelled,
                Status::Timeout => crate::session::Status::Timeout,
                Status::Failed => crate::session::Status::Failed,
            },
            json!({"status":status,"assignment":receipt.id}),
        );
        if let Some(reason) = rec.failure() {
            return Err((Exit::Unpublished, reason.into()));
        }
        let status = match &mut service {
            Lease::Service(service) => {
                let v = service
                    .call("child.finish", json!(outcome))
                    .map_err(|e| (Exit::Unpublished, e.message))?;
                serde_json::from_value(v["status"].clone())
                    .map_err(|e| (Exit::Unpublished, e.to_string()))?
            }
            #[cfg(test)]
            Lease::Fixture => {
                super::finish(dir, &outcome).map_err(|e| (Exit::Unpublished, e.message))?;
                status
            }
        };
        eprintln!(
            "assignment {}: {} ({} bytes of text{}; result published)",
            receipt.id,
            match status {
                Status::Ok => "ok",
                Status::Failed => "failed",
                Status::Cancelled => "cancelled",
                Status::Timeout => "timeout",
            },
            outcome.text_bytes,
            if truncated { ", truncated" } else { "" }
        );
        Ok(match status {
            Status::Ok => Exit::Ok,
            Status::Failed => Exit::Failed,
            Status::Cancelled | Status::Timeout => Exit::Stopped,
        })
    };
    // Once claimed, even a local validation failure is a structured result.
    let mut invalid = |message: String| {
        finish(
            &mut rec,
            Status::Failed,
            String::new(),
            false,
            None,
            Some(Failure {
                kind: "config".into(),
                message,
            }),
        )
    };
    let prompt = match super::read_prompt(dir, &receipt) {
        Ok(p) => p,
        Err(e) => return invalid(e.message),
    };
    let Some(thinking) = ThinkingLevel::parse(&receipt.thinking) else {
        return invalid("unknown thinking level".into());
    };
    let protocol = crate::harness::completion_connection(
        &receipt.directories.to_directories(),
        &receipt.gateway,
        &receipt.model,
    )
    .ok()
    .map(|c| c.protocol);
    let mut history = rec
        .session()
        .map(|s| s.active_messages(protocol))
        .unwrap_or_default();
    rec.begin_turn_as(receipt.id.clone(), &prompt);
    if rec.failure().is_some() || rec.over_budget() {
        return finish(
            &mut rec,
            Status::Failed,
            String::new(),
            false,
            None,
            Some(Failure {
                kind: "session".into(),
                message: "durable session unavailable or over budget".into(),
            }),
        );
    }
    history.push(ChatMessage::new(TranscriptRole::Operator, prompt));
    let remaining = super::remaining_secs(&receipt);
    if remaining <= 0 {
        return finish(&mut rec, Status::Timeout, String::new(), false, None, None);
    }
    let deadline = Instant::now() + Duration::from_secs(remaining as u64);
    // Last checkpoint before the provider sees anything.
    if super::cancel_requested(dir) {
        return finish(
            &mut rec,
            Status::Cancelled,
            String::new(),
            false,
            None,
            None,
        );
    }
    let backend = HttpBackend::new(receipt.directories.to_directories());
    let mut backend = if admission.policy.rules.is_empty() {
        backend.without_tools()
    } else {
        backend
            .in_workspace(expected)
            .with_frozen_policy(admission.policy)
    }
    .with_deadline(deadline)
    .with_caller(Caller {
        origin: "delegate",
        run_id: Some(receipt.id.clone()),
    });
    let mut follow_ups = admission.follow_ups.into_iter();
    let mut request = ChatRequest {
        // Even a Full Access parent never enables absolute paths in scratch.
        mode: if admission.mode.host_paths() {
            crate::tool_policy::ExecutionMode::ReadWrite
        } else {
            admission.mode
        },
        skills: Vec::new(),
        selection: Selection {
            gateway: receipt.gateway.clone(),
            model: receipt.model.clone(),
        },
        thinking,
        messages: history,
    };
    eprintln!(
        "assignment {}: inference started on {}/{} ({remaining}s remain)",
        receipt.id, receipt.gateway, receipt.model
    );
    let mut events = backend.submit(request.clone());
    let mut text = String::new();
    let mut truncated = false;
    let mut usage = None;
    // Set once the worker itself stopped the turn; the backend's eventual
    // `Failed` then means that stop, not a provider fault.
    let mut stopped: Option<Status> = None;
    let mut last_poll = Instant::now();
    loop {
        if stopped.is_none() {
            let now = Instant::now();
            if now >= deadline {
                stopped = Some(Status::Timeout);
                backend.cancel();
            } else if now.duration_since(last_poll) >= CANCEL_POLL {
                last_poll = now;
                if super::cancel_requested(dir) {
                    stopped = Some(Status::Cancelled);
                    backend.cancel();
                }
            }
        }
        let event = match events.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                let status = stopped.unwrap_or(Status::Failed);
                let error = (status == Status::Failed).then(|| Failure {
                    kind: "turn".into(),
                    message: "the turn ended without a completion".into(),
                });
                return finish(&mut rec, status, text, truncated, usage, error);
            }
        };
        match event {
            ResponseEvent::TextDelta(delta) => {
                rec.text(&delta);
                if !truncated {
                    truncated = super::push_bounded(&mut text, &delta, MAX_RESULT_TEXT_BYTES);
                }
            }
            ResponseEvent::Usage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                ..
            } => {
                usage = Some(Usage {
                    input_tokens,
                    output_tokens,
                    cached_input_tokens,
                });
            }
            ResponseEvent::ToolApprovalRequested(request) => request.reply.send(vec![
                    crate::chat::backend::ToolDecision::Unattended;
                    request.calls.len()
                ]),
            ResponseEvent::Completed => {
                if stopped.is_none() {
                    completed.set(completed.get() + 1);
                }
                if stopped.is_none() && !super::cancel_requested(dir) && Instant::now() < deadline {
                    if let Some(next) = follow_ups.next() {
                        rec.next_prompt(&next);
                        if rec.failure().is_some() || rec.over_budget() {
                            return finish(
                                &mut rec,
                                Status::Failed,
                                text,
                                truncated,
                                usage,
                                Some(Failure {
                                    kind: "session".into(),
                                    message: "child transcript unavailable or full".into(),
                                }),
                            );
                        }
                        request.messages.push(ChatMessage::new(
                            TranscriptRole::Assistant,
                            std::mem::take(&mut text),
                        ));
                        request
                            .messages
                            .push(ChatMessage::new(TranscriptRole::Operator, next));
                        truncated = false;
                        events = backend.submit(request.clone());
                        continue;
                    }
                }
                let status = stopped.unwrap_or_else(|| {
                    if super::cancel_requested(dir) {
                        Status::Cancelled
                    } else if Instant::now() >= deadline {
                        Status::Timeout
                    } else {
                        Status::Ok
                    }
                });
                return finish(&mut rec, status, text, truncated, usage, None);
            }
            ResponseEvent::Failed { message, kind } => {
                let status = stopped.unwrap_or(Status::Failed);
                let error = (status == Status::Failed).then(|| Failure {
                    kind: crate::service::error_kind_name(kind).to_string(),
                    message,
                });
                return finish(&mut rec, status, text, truncated, usage, error);
            }
            ResponseEvent::ToolCallsRequested { calls, native } => {
                rec.calls(&calls, native.as_ref());
                let mut m = ChatMessage::new(TranscriptRole::Assistant, std::mem::take(&mut text));
                m.tool = Some(ToolPayload::Calls { calls, native });
                request.messages.push(m);
                truncated = false;
            }
            ResponseEvent::ToolResult {
                call_id,
                tool,
                content,
                exit_code,
                detail,
                elapsed,
                output,
                ..
            } => {
                rec.tool_result(
                    &call_id,
                    &tool,
                    &output,
                    &content,
                    exit_code,
                    ahi_durable::ToolPresentation::new(detail, elapsed),
                );
                let mut m = ChatMessage::new(TranscriptRole::Tool { exit_code }, "");
                m.tool = Some(ToolPayload::Result {
                    call_id,
                    name: tool,
                    content,
                });
                request.messages.push(m);
            }
            ResponseEvent::CommandStdout { .. }
            | ResponseEvent::CommandStarted { .. }
            | ResponseEvent::ContextWindow(_)
            | ResponseEvent::ToolCallReturned
            | ResponseEvent::Reconnecting { .. } => {}
        }
    }
}
