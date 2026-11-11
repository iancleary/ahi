//! `ahi exec`: one model turn without the chat screen.
//!
//! A prompt in (`-p`, a file, or stdin), the final answer out (stdout, and
//! a result file when asked), one exit code a caller can act on without
//! parsing text. The turn runs through the same `HttpBackend` as `ahi tui`,
//! so every transport, credential, tool, and stream boundary is shared; what
//! differs is the input discipline and the absence of a terminal.
//!
//! `--headless` is the worker contract a coordinator drives: prompt file
//! only, result file required, tools only from an explicit `--tools` file,
//! `ask` rules answered as denials because nobody is there to answer. See
//! `docs/HEADLESS_EXEC.md` for the plan; this module implements its first
//! two slices. The result file carries a ledger of every effectful call and
//! an `effects` verdict so a coordinator knows what changed even when the
//! turn failed.

use std::fs;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::backend::{
    tools, ApprovalRequest, Caller, ChatRequest, CompletionBackend, HttpBackend, ResponseEvent,
    Selection, ToolDecision, ToolRecord, EXIT_DENIED, EXIT_OK,
};
use super::thinking::ThinkingLevel;
use super::transcript::{ChatMessage, TranscriptRole};
use crate::directories::Directories;
use crate::error::ErrorKind;
use crate::tool_policy;

/// Version of the result object. Bumped only for an incompatible change.
pub const RESULT_SCHEMA: u32 = 1;
/// Largest prompt accepted, in bytes. A prompt is one user message; provider
/// request limits sit well above this, so the cap guards the caller, not ahi.
pub const PROMPT_CAP: usize = 1024 * 1024;
/// Largest `text` stored in the result object. stdout gets the whole answer.
pub const TEXT_CAP: usize = 4 * 1024 * 1024;
/// `--timeout` when the caller does not set one.
pub const DEFAULT_TIMEOUT_SECS: u64 = 900;
/// How often the event loop looks at the clock and the signal flag.
const POLL: Duration = Duration::from_millis(100);
/// After a cancel, how long the worker gets to report its terminal event
/// before the run is reported without it. A blocked request ends by the
/// deadline on its own; a killed command returns within one poll.
const GRACE: Duration = Duration::from_secs(5);

/// Where the prompt comes from. Exactly one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptSource {
    Inline(String),
    File(PathBuf),
    /// Used only when stdin is not a terminal.
    Stdin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
}

impl Format {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

pub struct Options {
    /// `gateway/model-id`, as the picker saves it.
    pub model: String,
    pub prompt: PromptSource,
    /// Hex SHA-256 the prompt bytes must hash to, when the caller knows it.
    pub prompt_sha256: Option<String>,
    /// `off`, `minimal`, `low`, `medium`, or `high`.
    pub thinking: String,
    pub format: Format,
    pub result_file: Option<PathBuf>,
    pub headless: bool,
    pub run_id: Option<String>,
    /// Tool root and, without `tools`, where `ahi.tools.yaml` is looked up.
    /// The process's working directory when absent.
    pub workdir: Option<PathBuf>,
    /// An explicit policy file. Headless runs advertise no tool without one.
    pub tools: Option<PathBuf>,
    /// Validate everything up to the model call and stop.
    pub check: bool,
    /// Progress lines on stderr in interactive mode too.
    pub verbose: bool,
    /// Whole-run bound: the model, every tool call, every wait.
    pub timeout: Duration,
}

/// Process exit codes. A coordinator reacts to these, not to text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Exit {
    /// Validated successful completion.
    Ok = 0,
    /// Transport, malformed stream, refusal, truncation, tools disabled.
    Turn = 1,
    /// Bad arguments, unreadable or invalid prompt, result file in the way,
    /// bad working directory, operator file inside it, digest mismatch.
    Usage = 2,
    /// `models.json` missing or invalid, unknown gateway or model.
    Config = 3,
    /// Credential missing, rejected, or its refresh failed.
    Credential = 4,
    /// The policy file is missing or invalid. Nothing was sent.
    Policy = 5,
    /// `--timeout` passed, or SIGINT, SIGTERM, or SIGHUP arrived.
    Timeout = 6,
    /// The turn finished but the result file could not be written.
    ResultUnwritable = 8,
}

/// What `run` hands back to `main`: the code, and stdout if the turn earned it.
pub struct Finished {
    pub code: u8,
    /// Written once, only after a validated success. `None` otherwise.
    pub stdout: Option<String>,
}

/// One failure, classified for the exit code and the result object.
#[derive(Debug)]
struct Failure {
    exit: Exit,
    /// `usage`, `config`, `policy`, or an `ErrorKind` name.
    kind: &'static str,
    message: String,
}

impl Failure {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            exit: Exit::Usage,
            kind: "usage",
            message: message.into(),
        }
    }

    fn config(message: impl Into<String>) -> Self {
        Self {
            exit: Exit::Config,
            kind: "config",
            message: message.into(),
        }
    }

    fn policy(message: impl Into<String>) -> Self {
        Self {
            exit: Exit::Policy,
            kind: "policy",
            message: message.into(),
        }
    }

    fn timeout(limit: Duration) -> Self {
        Self {
            exit: Exit::Timeout,
            kind: "timeout",
            message: format!("timed out after {} s", limit.as_secs()),
        }
    }

    fn cancelled(signal: &str) -> Self {
        Self {
            exit: Exit::Timeout,
            kind: "cancelled",
            message: format!("cancelled by {signal}"),
        }
    }

    fn turn(message: impl Into<String>, kind: ErrorKind) -> Self {
        Self {
            exit: match kind {
                ErrorKind::Auth => Exit::Credential,
                // A policy failure here is `tool execution is disabled`: the
                // model asked for a tool no policy offered. That is the
                // turn's failure; a bad policy file never reaches the model
                // (exit 5 from the preflight).
                _ => Exit::Turn,
            },
            kind: kind_name(kind),
            message: message.into(),
        }
    }
}

fn kind_name(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Other => "other",
        ErrorKind::Auth => "auth",
        ErrorKind::Policy => "policy",
        ErrorKind::Transport => "transport",
        ErrorKind::Protocol => "protocol",
        ErrorKind::Limit => "limit",
    }
}

/// Everything the result object reports besides the outcome, gathered as
/// the turn runs so a failure after partial progress still has it.
struct Report {
    run_id: String,
    gateway: String,
    model: String,
    usage: Option<Value>,
    context_window: Option<u64>,
    tools: ToolCounts,
    ledger: Ledger,
}

#[derive(Default)]
struct ToolCounts {
    calls: u64,
    denied: u64,
    ask_denied: u64,
    invalid: u64,
    rounds: u64,
}

/// What the turn did to the working directory, as the audit log records
/// it. `effects` is decided from the records and from what never came back.
#[derive(Default)]
struct Ledger {
    entries: Vec<Value>,
    /// A write landed or a program ran.
    some: bool,
    /// A command failed in a way whose side effects cannot be known.
    unknown: bool,
    /// Effectful calls requested whose result has not arrived; a run that
    /// ends with any of these left things in an unknown state.
    in_flight: Vec<String>,
}

impl Ledger {
    fn requested(&mut self, calls: &[super::backend::ToolCall]) {
        for call in calls {
            if matches!(
                call.name.as_str(),
                tools::EDIT_FILE | tools::WRITE_FILE | tools::RUN_COMMAND
            ) {
                self.in_flight.push(call.id.clone());
            }
        }
    }

    fn returned(&mut self, call_id: &str, record: Option<ToolRecord>) {
        self.in_flight.retain(|id| id != call_id);
        let Some(record) = record else {
            return;
        };
        match record.effect() {
            Some(true) => self.some = true,
            Some(false) => {}
            None => self.unknown = true,
        }
        self.entries.push(match record {
            ToolRecord::Write {
                tool,
                path,
                outcome,
                before,
                after,
                lines_added,
                lines_removed,
            } => json!({
                "tool": tool,
                "target": path,
                "outcome": outcome,
                "before": before,
                "after": after,
                "lines_added": lines_added,
                "lines_removed": lines_removed,
            }),
            ToolRecord::Command {
                argv,
                cwd,
                outcome,
                ran,
                exit_code,
                duration_ms,
            } => json!({
                "tool": tools::RUN_COMMAND,
                "argv": argv,
                "cwd": if cwd.is_empty() { "." } else { cwd.as_str() },
                "outcome": outcome,
                "ran": ran,
                "exit_code": exit_code,
                "duration_ms": duration_ms,
            }),
        });
    }

    fn effects(&self) -> &'static str {
        if self.unknown || !self.in_flight.is_empty() {
            "unknown"
        } else if self.some {
            "some"
        } else {
            "none"
        }
    }
}

impl Report {
    fn new(run_id: String, gateway: String, model: String) -> Self {
        Self {
            run_id,
            gateway,
            model,
            usage: None,
            context_window: None,
            tools: ToolCounts::default(),
            ledger: Ledger::default(),
        }
    }

    fn result(&self, outcome: &Result<String, Failure>) -> Value {
        let mut object = json!({
            "schema": RESULT_SCHEMA,
            "ahi_version": crate::VERSION,
            "run_id": self.run_id,
            "status": match outcome {
                Ok(_) => "ok",
                Err(failure) if failure.kind == "timeout" => "timeout",
                Err(failure) if failure.kind == "cancelled" => "cancelled",
                Err(_) => "failed",
            },
            "gateway": self.gateway,
            "model": self.model,
        });
        match outcome {
            Ok(text) => {
                let (stored, truncated) = cap_text(text);
                object["text"] = Value::String(stored);
                object["text_truncated"] = Value::Bool(truncated);
            }
            Err(failure) => {
                object["error"] = json!({"kind": failure.kind, "message": failure.message});
            }
        }
        if let Some(usage) = &self.usage {
            object["usage"] = usage.clone();
        }
        if let Some(window) = self.context_window {
            object["context_window"] = json!(window);
        }
        object["tools"] = json!({
            "calls": self.tools.calls,
            "denied": self.tools.denied,
            "ask_denied": self.tools.ask_denied,
            "invalid": self.tools.invalid,
            "rounds": self.tools.rounds,
        });
        object["effects"] = json!(self.ledger.effects());
        object["ledger"] = Value::Array(self.ledger.entries.clone());
        object["history"] = json!("none");
        object
    }
}

/// Cut at a character boundary so the stored text is always valid.
fn cap_text(text: &str) -> (String, bool) {
    if text.len() <= TEXT_CAP {
        return (text.to_string(), false);
    }
    let mut cut = TEXT_CAP;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    (text[..cut].to_string(), true)
}

/// Run one turn. Diagnostics and progress go to stderr as they happen; the
/// answer comes back in `Finished` so `main` writes stdout exactly once.
pub fn run(state: &Directories, options: &Options) -> Finished {
    let run_id = match &options.run_id {
        Some(id) if crate::hooks::policy::valid_rule_id(id) => id.clone(),
        Some(_) => {
            return finish_without_result(Failure::usage(
                "--run-id must be 1-64 ASCII letters, digits, '-', '_', or '.'",
            ))
        }
        None => generate_run_id(),
    };
    let (gateway, model) = match options.model.split_once('/') {
        Some((gateway, model)) if !gateway.is_empty() && !model.is_empty() => {
            (gateway.to_string(), model.to_string())
        }
        _ => {
            return finish_without_result(Failure::usage(
                "--model takes `gateway/model-id`, as `ahi tui` saves it",
            ))
        }
    };
    let mut report = Report::new(run_id, gateway, model);
    // The result file is checked before anything else can fail, so every
    // later outcome has somewhere to go. An existing file is someone else's
    // result: refusing it is what keeps a coordinator from reading a stale
    // success.
    if let Some(path) = &options.result_file {
        if let Err(failure) = check_result_path(path) {
            return finish_without_result(failure);
        }
    }
    let outcome = match preflight(state, options, &report) {
        Err(failure) => Err(failure),
        Ok(prepared) if options.check => {
            eprintln!(
                "check: ok; {}/{}; tools: {}; working directory: {}",
                report.gateway,
                report.model,
                match &prepared.advertised {
                    None => "none".to_string(),
                    Some(names) => names.join(","),
                },
                prepared.root.display()
            );
            return Finished {
                code: Exit::Ok as u8,
                stdout: None,
            };
        }
        Ok(prepared) => complete(state, options, prepared, &mut report),
    };
    let result = report.result(&outcome);
    if let Some(path) = &options.result_file {
        if let Err(err) = write_new(path, format!("{result:#}\n").as_bytes()) {
            eprintln!("error: cannot write {}: {err}", path.display());
            eprintln!(
                "outcome: {}",
                match &outcome {
                    Ok(_) =>
                        "ok; the answer is withheld because the result file failed".to_string(),
                    Err(failure) => format!("{} ({})", failure.message, failure.kind),
                }
            );
            return Finished {
                code: Exit::ResultUnwritable as u8,
                stdout: None,
            };
        }
    }
    match outcome {
        Ok(text) => Finished {
            code: Exit::Ok as u8,
            stdout: Some(match options.format {
                Format::Text => with_newline(text),
                Format::Json => format!("{result:#}\n"),
            }),
        },
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            Finished {
                code: failure.exit as u8,
                stdout: None,
            }
        }
    }
}

fn finish_without_result(failure: Failure) -> Finished {
    eprintln!("error: {}", failure.message);
    Finished {
        code: failure.exit as u8,
        stdout: None,
    }
}

fn with_newline(mut text: String) -> String {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// Sixteen hex digits from the OS; distinct per run with overwhelming
/// probability, which is all a correlation key needs.
fn generate_run_id() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        bytes = (nanos as u64).to_be_bytes();
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The validated contract: everything that can be known before a byte is
/// sent, so every fault short of the model's has its own exit code.
struct Prepared {
    prompt: String,
    thinking: ThinkingLevel,
    /// Canonical tool root.
    root: PathBuf,
    /// Advertised tool names; `None` when no tool is offered to the model.
    advertised: Option<Vec<String>>,
    tools: ToolSetup,
}

enum ToolSetup {
    /// No policy is read; a requested call fails the turn.
    Disabled,
    /// The root's own `ahi.tools.yaml`, as the chat reads it.
    Workdir,
    /// An explicit `--tools` file.
    File(PathBuf),
}

fn preflight(state: &Directories, options: &Options, report: &Report) -> Result<Prepared, Failure> {
    if options.headless {
        if !matches!(options.prompt, PromptSource::File(_)) {
            return Err(Failure::usage(
                "--headless takes the prompt from --prompt-file only; -p and stdin are not read",
            ));
        }
        if options.result_file.is_none() {
            return Err(Failure::usage("--headless requires --result-file"));
        }
    }
    let thinking = ThinkingLevel::parse(&options.thinking)
        .ok_or_else(|| Failure::usage("--thinking must be off, minimal, low, medium, or high"))?;
    let root = resolve_workdir(options.workdir.as_deref())?;
    // Which policy, and whether anything is advertised. A bad file is exit
    // 5 here, before the prompt is even read: nothing about the task should
    // leak when the operator's own configuration is broken.
    let (tools, policy) = match (&options.tools, options.headless) {
        (Some(path), _) => {
            let policy = tool_policy::load_file(path, &tools::BUILTIN)
                .map_err(|err| Failure::policy(err.to_string()))?;
            (ToolSetup::File(path.clone()), Some(policy))
        }
        (None, true) => (ToolSetup::Disabled, None),
        (None, false) => {
            let policy = tool_policy::load(&root, &tools::BUILTIN)
                .map_err(|err| Failure::policy(err.to_string()))?;
            (ToolSetup::Workdir, policy)
        }
    };
    let advertised = policy
        .map(|policy| policy.advertised_tools())
        .filter(|names| !names.is_empty());
    let tools = if advertised.is_some() {
        tools
    } else {
        ToolSetup::Disabled
    };
    // With a tool that can read the working directory, the operator's own
    // files must not be in it: reads are not protected by name, and a
    // worker that can read its own result file or policy learns things the
    // coordinator did not say.
    if advertised.is_some() {
        let mut operator_files: Vec<(&str, &Path)> = Vec::new();
        if let PromptSource::File(path) = &options.prompt {
            operator_files.push(("prompt file", path));
        }
        if let Some(path) = &options.result_file {
            operator_files.push(("result file", path));
        }
        if let ToolSetup::File(path) = &tools {
            operator_files.push(("policy file", path));
        }
        for (label, path) in operator_files {
            if inside(&root, path) {
                return Err(Failure::usage(format!(
                    "{label} {} is inside the working directory {}; operator files must live outside it because tools may read it",
                    path.display(),
                    root.display()
                )));
            }
        }
    }
    let prompt = read_prompt(&options.prompt)?;
    if let Some(expected) = &options.prompt_sha256 {
        let actual = sha256_hex(prompt.as_bytes());
        if !expected.eq_ignore_ascii_case(&actual) {
            return Err(Failure::usage(
                "prompt digest mismatch: the prompt is not the one --prompt-sha256 names",
            ));
        }
    }
    // Resolve the destination and credential before any request so a
    // configuration fault and a missing credential get their own codes.
    // Secrets are read here and never printed.
    crate::harness::completion_connection(state, &report.gateway, &report.model).map_err(
        |err| match err.kind() {
            ErrorKind::Auth => Failure::turn(err.to_string(), ErrorKind::Auth),
            _ => Failure::config(err.to_string()),
        },
    )?;
    Ok(Prepared {
        prompt,
        thinking,
        root,
        advertised,
        tools,
    })
}

fn resolve_workdir(workdir: Option<&Path>) -> Result<PathBuf, Failure> {
    let path = match workdir {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().map_err(|err| {
            Failure::usage(format!("cannot resolve the working directory: {err}"))
        })?,
    };
    let shown = path.display();
    let metadata = fs::metadata(&path)
        .map_err(|err| Failure::usage(format!("working directory {shown}: {err}")))?;
    if !metadata.is_dir() {
        return Err(Failure::usage(format!(
            "working directory {shown} is not a directory"
        )));
    }
    path.canonicalize()
        .map_err(|err| Failure::usage(format!("working directory {shown}: {err}")))
}

/// Whether `path` (which may not exist yet) lies under `root`. The parent is
/// canonicalized so a symbolic link into the working directory counts.
fn inside(root: &Path, path: &Path) -> bool {
    let canonical = match path.canonicalize() {
        Ok(canonical) => canonical,
        Err(_) => {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            match (parent.canonicalize(), path.file_name()) {
                (Ok(parent), Some(name)) => parent.join(name),
                _ => return false,
            }
        }
    };
    canonical.starts_with(root)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The model, with tools when the preflight offered any.
fn complete(
    state: &Directories,
    options: &Options,
    prepared: Prepared,
    report: &mut Report,
) -> Result<String, Failure> {
    let deadline = Instant::now() + options.timeout;
    let mut backend = HttpBackend::new(state.clone())
        .in_workspace(prepared.root.clone())
        .with_caller(Caller {
            origin: "exec",
            run_id: Some(report.run_id.clone()),
        })
        .with_deadline(deadline);
    backend = match prepared.tools {
        ToolSetup::Disabled => backend.without_tools(),
        ToolSetup::Workdir => backend,
        ToolSetup::File(path) => backend.with_actions(Some(path)),
    };
    // From here a signal means "stop and report", not "die": the result
    // file is the point. Installed once the preflight is done so a Ctrl+C
    // during validation still ends the process the ordinary way.
    if !watch_signals() {
        eprintln!("warning: termination signals are not caught on this platform; an interrupted run writes no result file");
    }
    let receiver = backend.submit(ChatRequest {
        selection: Selection {
            gateway: report.gateway.clone(),
            model: report.model.clone(),
        },
        thinking: prepared.thinking,
        messages: vec![ChatMessage::new(TranscriptRole::Operator, prepared.prompt)],
    });
    // Approvals are answered only by a person at a terminal. A headless run
    // has none by contract; a run reading its prompt from a pipe has no
    // stdin to answer on either.
    let attended = !options.headless && io::stdin().is_terminal() && io::stderr().is_terminal();
    let progress = options.headless || options.verbose;
    let bound = Bound {
        deadline,
        limit: options.timeout,
    };
    drain(receiver, &mut backend, report, attended, progress, bound)
}

#[cfg(unix)]
fn watch_signals() -> bool {
    ahi_signal::watch()
}

#[cfg(not(unix))]
fn watch_signals() -> bool {
    false
}

#[cfg(unix)]
fn signal_received() -> Option<&'static str> {
    ahi_signal::received().map(ahi_signal::Signal::name)
}

#[cfg(not(unix))]
fn signal_received() -> Option<&'static str> {
    None
}

/// The run's time bound, for the loop and for the message.
struct Bound {
    deadline: Instant,
    limit: Duration,
}

fn read_prompt(source: &PromptSource) -> Result<String, Failure> {
    let text = match source {
        PromptSource::Inline(text) => text.clone(),
        PromptSource::File(path) => read_prompt_file(path)?,
        PromptSource::Stdin => {
            if io::stdin().is_terminal() {
                return Err(Failure::usage(
                    "no prompt: pass -p <text>, --prompt-file <path>, or pipe text on stdin",
                ));
            }
            let mut bytes = Vec::new();
            io::stdin()
                .lock()
                .take(PROMPT_CAP as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|err| Failure::usage(format!("cannot read stdin: {err}")))?;
            decode_prompt(bytes, "stdin")?
        }
    };
    if text.len() > PROMPT_CAP {
        return Err(Failure::usage(format!(
            "prompt is larger than {} bytes",
            PROMPT_CAP
        )));
    }
    if text.trim().is_empty() {
        return Err(Failure::usage("prompt is empty"));
    }
    Ok(text)
}

fn read_prompt_file(path: &Path) -> Result<String, Failure> {
    let shown = path.display();
    let metadata = fs::metadata(path)
        .map_err(|err| Failure::usage(format!("cannot read prompt file {shown}: {err}")))?;
    if !metadata.is_file() {
        return Err(Failure::usage(format!(
            "prompt file {shown} is not a regular file"
        )));
    }
    if metadata.len() > PROMPT_CAP as u64 {
        return Err(Failure::usage(format!(
            "prompt file {shown} is larger than {PROMPT_CAP} bytes"
        )));
    }
    let bytes = fs::read(path)
        .map_err(|err| Failure::usage(format!("cannot read prompt file {shown}: {err}")))?;
    decode_prompt(bytes, &shown.to_string())
}

fn decode_prompt(bytes: Vec<u8>, origin: &str) -> Result<String, Failure> {
    String::from_utf8(bytes).map_err(|_| Failure::usage(format!("{origin}: prompt is not UTF-8")))
}

/// Consume the turn's events. Text accumulates; usage and the window are
/// kept; every tool result feeds the ledger; `Completed` is the only
/// success. Approvals are answered per call, so the calls that rules alone
/// approved still run in an unattended batch.
///
/// The loop also watches the clock and the signal flag. Past the deadline,
/// or once a signal has arrived, the turn is cancelled and the loop keeps
/// draining for a bounded grace period so the ledger sees every result the
/// worker still reports; then the run is a timeout or a cancellation
/// whatever else arrives, because the caller asked for a bound and gets it.
fn drain(
    receiver: Receiver<ResponseEvent>,
    backend: &mut HttpBackend,
    report: &mut Report,
    attended: bool,
    progress: bool,
    bound: Bound,
) -> Result<String, Failure> {
    let mut text = String::new();
    let mut stop: Option<Failure> = None;
    let mut grace_until: Option<Instant> = None;
    loop {
        let now = Instant::now();
        if stop.is_none() {
            let reason = match signal_received() {
                Some(signal) => Some(Failure::cancelled(signal)),
                None if now >= bound.deadline => Some(Failure::timeout(bound.limit)),
                None => None,
            };
            if let Some(reason) = reason {
                eprintln!("progress: {}; stopping the turn", reason.message);
                backend.cancel();
                grace_until = Some(now + GRACE);
                stop = Some(reason);
            }
        }
        let wait = match grace_until {
            Some(until) if now >= until => {
                return Err(stop.expect("grace runs only after a stop"));
            }
            Some(until) => (until - now).min(POLL),
            None => bound.deadline.saturating_duration_since(now).min(POLL),
        };
        let event = match receiver.recv_timeout(wait) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return Err(stop.unwrap_or_else(|| {
                    Failure::turn(
                        "model worker ended without a terminal event",
                        ErrorKind::Other,
                    )
                }))
            }
        };
        match event {
            ResponseEvent::TextDelta(delta) => text.push_str(&delta),
            ResponseEvent::Usage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                cost_usd,
            } => {
                let mut usage = json!({
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens,
                });
                if let Some(cached) = cached_input_tokens {
                    usage["cached_input_tokens"] = json!(cached);
                }
                if let Some(cost) = cost_usd {
                    usage["cost_usd"] = json!(cost);
                }
                report.usage = Some(usage);
            }
            ResponseEvent::ContextWindow(window) => report.context_window = Some(window),
            ResponseEvent::Reconnecting { attempt, attempts } => {
                eprintln!(
                    "progress: reconnecting to the gateway (attempt {attempt} of {attempts})"
                );
            }
            ResponseEvent::ToolCallsRequested { calls, .. } => {
                report.tools.rounds += 1;
                report.tools.calls += calls.len() as u64;
                report.ledger.requested(&calls);
            }
            ResponseEvent::ToolApprovalRequested(request) => {
                let decisions = if attended && stop.is_none() {
                    ask_operator(&request)
                } else {
                    vec![ToolDecision::Unattended; request.calls.len()]
                };
                report.tools.ask_denied += decisions
                    .iter()
                    .filter(|decision| {
                        matches!(decision, ToolDecision::Deny | ToolDecision::Unattended)
                    })
                    .count() as u64;
                let ApprovalRequest { reply, .. } = request;
                reply.send(decisions);
            }
            ResponseEvent::ToolResult {
                call_id,
                tool,
                exit_code,
                elapsed,
                record,
                ..
            } => {
                match exit_code {
                    EXIT_DENIED => report.tools.denied += 1,
                    EXIT_OK => {}
                    _ => report.tools.invalid += 1,
                }
                if progress {
                    eprintln!(
                        "progress: run {} round {} {} exit {} {}",
                        report.run_id,
                        report.tools.rounds,
                        describe(&tool, record.as_deref()),
                        exit_code,
                        elapsed_text(elapsed)
                    );
                }
                report
                    .ledger
                    .returned(&call_id, record.map(|record| *record));
            }
            ResponseEvent::ToolCallReturned => {}
            ResponseEvent::Completed => {
                return match stop {
                    Some(reason) => Err(reason),
                    None => Ok(text),
                }
            }
            ResponseEvent::Failed { message, kind } => {
                // A request that gave up at the deadline is the timeout, not
                // a transport fault, whichever clock noticed first.
                return Err(stop.unwrap_or_else(|| {
                    if Instant::now() >= bound.deadline {
                        Failure::timeout(bound.limit)
                    } else {
                        Failure::turn(message, kind)
                    }
                }));
            }
        }
    }
}

/// `tool target` for the progress line, from the record alone: never from
/// arguments. A read or a denial shows the tool name only.
fn describe(tool: &str, record: Option<&ToolRecord>) -> String {
    match record {
        Some(ToolRecord::Write { path, .. }) => format!("{tool} {path}"),
        Some(ToolRecord::Command { cwd, .. }) if !cwd.is_empty() => format!("{tool} {cwd}"),
        _ => tool.to_string(),
    }
}

fn elapsed_text(elapsed: Duration) -> String {
    if elapsed < Duration::from_secs(1) {
        format!("{}ms", elapsed.as_millis())
    } else {
        format!("{:.1}s", elapsed.as_secs_f64())
    }
}

/// A terminal prompt per call, on stderr, answered on stdin: `y` once, `s`
/// for the session when a grant is offered, anything else denies. End of
/// input denies the rest.
fn ask_operator(request: &ApprovalRequest) -> Vec<ToolDecision> {
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    request
        .calls
        .iter()
        .map(|call| {
            let detail = if call.detail.is_empty() {
                String::new()
            } else {
                format!(" ({})", call.detail)
            };
            let session = if call.grant_prefix.is_empty() && call.tool != tools::RUN_COMMAND {
                ""
            } else {
                ", s for the session"
            };
            eprint!(
                "approve {} {}{detail}? [y once{session}, anything else denies] ",
                call.tool, call.target
            );
            let _ = io::stderr().flush();
            match lines.next() {
                Some(Ok(answer)) => match answer.trim() {
                    "y" | "Y" => ToolDecision::Approve,
                    "s" | "S" if !session.is_empty() => ToolDecision::ApproveForSession,
                    _ => ToolDecision::Deny,
                },
                _ => ToolDecision::Deny,
            }
        })
        .collect()
}

fn check_result_path(path: &Path) -> Result<(), Failure> {
    let shown = path.display();
    if fs::symlink_metadata(path).is_ok() {
        return Err(Failure::usage(format!(
            "result file {shown} already exists; remove it or name another path"
        )));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(Failure::usage(format!(
            "result file {shown}: parent is not a directory"
        )));
    }
    Ok(())
}

/// Create `path` with `bytes` in one step: a sibling temporary file, fsync,
/// rename. The file is owner-only, since a prompt or an answer may be
/// sensitive. Nothing is written over an existing file by `check_result_path`;
/// the rename here is the only step that could, and only in a race.
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?;
    let tmp = parent.join(format!(
        ".{}.ahi-tmp-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    let written = (|| {
        let mut file = crate::directories::private_file()
            .create_new(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        Report::new("r1".into(), "local".into(), "vendor/model".into())
    }

    #[test]
    fn result_object_has_one_shape_for_success_and_failure() {
        let mut report = report();
        report.usage = Some(json!({"input_tokens": 3, "output_tokens": 4}));
        report.context_window = Some(128_000);
        let ok = report.result(&Ok("answer".into()));
        assert_eq!(ok["schema"], RESULT_SCHEMA);
        assert_eq!(ok["ahi_version"], crate::VERSION);
        assert_eq!(ok["status"], "ok");
        assert_eq!(ok["text"], "answer");
        assert_eq!(ok["text_truncated"], false);
        assert_eq!(ok["usage"]["output_tokens"], 4);
        assert_eq!(ok["context_window"], 128_000);
        assert_eq!(ok["effects"], "none");
        assert_eq!(ok["history"], "none");
        assert_eq!(ok["tools"]["calls"], 0);
        assert!(ok.get("error").is_none());

        let failed = report.result(&Err(Failure::turn("stream ended", ErrorKind::Protocol)));
        assert_eq!(failed["status"], "failed");
        assert_eq!(failed["error"]["kind"], "protocol");
        assert_eq!(failed["error"]["message"], "stream ended");
        assert!(failed.get("text").is_none());
        assert!(failed.get("text_truncated").is_none());
        // Usage reported before a failure is kept: the tokens were billed.
        assert_eq!(failed["usage"]["input_tokens"], 3);
    }

    #[test]
    fn exit_codes_follow_the_failure_class() {
        assert_eq!(Failure::usage("x").exit, Exit::Usage);
        assert_eq!(Failure::config("x").exit, Exit::Config);
        assert_eq!(Failure::policy("x").exit, Exit::Policy);
        assert_eq!(Failure::timeout(Duration::from_secs(3)).exit, Exit::Timeout);
        assert_eq!(Failure::cancelled("SIGHUP").exit, Exit::Timeout);
        let report = report();
        assert_eq!(
            report.result(&Err(Failure::timeout(Duration::from_secs(3))))["status"],
            "timeout"
        );
        let cancelled = report.result(&Err(Failure::cancelled("SIGHUP")));
        assert_eq!(cancelled["status"], "cancelled");
        assert_eq!(cancelled["error"]["message"], "cancelled by SIGHUP");
        assert_eq!(Failure::turn("x", ErrorKind::Auth).exit, Exit::Credential);
        for kind in [
            ErrorKind::Other,
            ErrorKind::Policy,
            ErrorKind::Transport,
            ErrorKind::Protocol,
            ErrorKind::Limit,
        ] {
            assert_eq!(Failure::turn("x", kind).exit, Exit::Turn, "{kind:?}");
        }
        assert_eq!(Failure::turn("x", ErrorKind::Policy).kind, "policy");
    }

    #[test]
    fn the_ledger_decides_effects_from_records_and_what_never_returned() {
        use super::super::backend::ToolCall;
        let call = |id: &str, name: &str| ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: serde_json::Map::new(),
        };
        let write_ok = ToolRecord::Write {
            tool: "edit_file".into(),
            path: "src/a.rs".into(),
            outcome: "ok".into(),
            before: Some("00".into()),
            after: Some("ff".into()),
            lines_added: 1,
            lines_removed: 1,
        };
        let write_failed = ToolRecord::Write {
            tool: "edit_file".into(),
            path: "src/a.rs".into(),
            outcome: "old text not found".into(),
            before: None,
            after: None,
            lines_added: 0,
            lines_removed: 0,
        };
        let command = |ran: Option<bool>| ToolRecord::Command {
            argv: vec!["true".into()],
            cwd: String::new(),
            outcome: "ok".into(),
            ran,
            exit_code: Some(0),
            duration_ms: 3,
        };

        let mut ledger = Ledger::default();
        ledger.requested(&[call("r", "read_file"), call("w", "edit_file")]);
        assert_eq!(ledger.effects(), "unknown", "a write is in flight");
        ledger.returned("r", None);
        assert_eq!(
            ledger.effects(),
            "unknown",
            "the read returning changes nothing"
        );
        ledger.returned("w", Some(write_failed));
        assert_eq!(ledger.effects(), "none", "a failed write wrote nothing");
        assert_eq!(ledger.entries.len(), 1, "but it is on the ledger");

        ledger.requested(&[call("c", "run_command")]);
        ledger.returned("c", Some(command(Some(false))));
        assert_eq!(ledger.effects(), "none", "a program that never started");
        ledger.requested(&[call("c2", "run_command")]);
        ledger.returned("c2", Some(command(Some(true))));
        assert_eq!(ledger.effects(), "some");
        ledger.requested(&[call("c3", "run_command")]);
        ledger.returned("c3", Some(command(None)));
        assert_eq!(ledger.effects(), "unknown", "unknown beats some");

        let mut ledger = Ledger::default();
        ledger.requested(&[call("w", "write_file")]);
        ledger.returned("w", Some(write_ok));
        assert_eq!(ledger.effects(), "some");
        let entry = &ledger.entries[0];
        assert_eq!(entry["tool"], "edit_file");
        assert_eq!(entry["target"], "src/a.rs");
        assert_eq!(entry["after"], "ff");
        // A read-only in-flight call never makes the outcome unknown.
        ledger.requested(&[call("r2", "read_file")]);
        assert_eq!(ledger.effects(), "some");
    }

    #[test]
    fn stored_text_is_capped_at_a_character_boundary() {
        let short = "é".repeat(10);
        assert_eq!(cap_text(&short), (short.clone(), false));
        let long = "é".repeat(TEXT_CAP / 2 + 5);
        let (stored, truncated) = cap_text(&long);
        assert!(truncated);
        assert!(stored.len() <= TEXT_CAP);
        assert!(stored.chars().all(|c| c == 'é'));
    }

    #[test]
    fn prompt_files_are_validated_before_any_request() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("exec-prompt");
        let error = |source: PromptSource| read_prompt(&source).err().unwrap().message;
        assert!(error(PromptSource::File(dir.join("missing.md"))).contains("cannot read"));
        assert!(error(PromptSource::File(dir.clone())).contains("not a regular file"));
        fs::write(dir.join("empty.md"), "  \n").unwrap();
        assert_eq!(
            error(PromptSource::File(dir.join("empty.md"))),
            "prompt is empty"
        );
        fs::write(dir.join("binary.md"), [0xff, 0xfe]).unwrap();
        assert!(error(PromptSource::File(dir.join("binary.md"))).ends_with("prompt is not UTF-8"));
        fs::write(dir.join("big.md"), vec![b'x'; PROMPT_CAP + 1]).unwrap();
        assert!(error(PromptSource::File(dir.join("big.md"))).contains("larger than"));
        fs::write(dir.join("ok.md"), "Summarize.\n").unwrap();
        assert_eq!(
            read_prompt(&PromptSource::File(dir.join("ok.md"))).unwrap(),
            "Summarize.\n"
        );
        assert_eq!(error(PromptSource::Inline("   ".into())), "prompt is empty");
        assert_eq!(
            read_prompt(&PromptSource::Inline("hi".into())).unwrap(),
            "hi"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn operator_files_are_located_against_the_canonical_root() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("exec-inside");
        let root = dir.join("work");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::create_dir_all(dir.join("ops")).unwrap();
        let root = resolve_workdir(Some(&root)).unwrap();
        assert!(
            inside(&root, &root.join("sub/result.json")),
            "not yet created"
        );
        assert!(inside(&root, &root.join("task.md")));
        assert!(!inside(&root, &dir.join("ops/task.md")));
        assert!(
            !inside(&root, &dir.join("ops/nowhere/task.md")),
            "unknowable parent is outside"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&root, dir.join("ops/link")).unwrap();
            assert!(
                inside(&root, &dir.join("ops/link/result.json")),
                "a link into the root counts"
            );
        }
        let error = resolve_workdir(Some(&dir.join("missing"))).err().unwrap();
        assert_eq!(error.exit, Exit::Usage);
        fs::write(dir.join("file"), "x").unwrap();
        assert!(resolve_workdir(Some(&dir.join("file")))
            .err()
            .unwrap()
            .message
            .contains("not a directory"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn digests_are_hex_sha256_compared_without_case() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn result_path_must_be_free_and_the_write_is_owner_only() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("exec-result");
        let path = dir.join("result.json");
        check_result_path(&path).unwrap();
        assert!(check_result_path(&dir.join("nodir/result.json"))
            .err()
            .unwrap()
            .message
            .contains("parent is not a directory"));
        write_new(&path, b"{}\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}\n");
        assert!(check_result_path(&path)
            .err()
            .unwrap()
            .message
            .contains("already exists"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(
            fs::read_dir(&dir).unwrap().count() == 1,
            "no temporary file left behind"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn generated_run_ids_are_valid_and_distinct() {
        let a = generate_run_id();
        let b = generate_run_id();
        assert_eq!(a.len(), 16);
        assert!(crate::hooks::policy::valid_rule_id(&a));
        assert_ne!(a, b);
    }
}
