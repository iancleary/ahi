//! `ahi exec`: one model turn without the chat screen.
//!
//! A prompt in (`-p`, a file, or stdin), the final answer out (stdout, and
//! a result file when asked), one exit code a caller can act on without
//! parsing text. The turn runs through the same `HttpBackend` as `ahi tui`,
//! so every transport, credential, and stream boundary is shared; what
//! differs is the input discipline and the absence of a terminal.
//!
//! `--headless` is the worker contract a coordinator drives: prompt file
//! only, result file required, nothing inferred from the environment. See
//! `docs/HEADLESS_EXEC.md` for the plan this slice implements the first
//! part of: no tools are advertised yet, so `tools`, `effects`, and
//! `ledger` in the result are present but trivial.

use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use serde_json::{json, Value};

use super::backend::{
    ChatRequest, CompletionBackend, HttpBackend, ResponseEvent, Selection, EXIT_DENIED, EXIT_OK,
};
use super::thinking::ThinkingLevel;
use super::transcript::{ChatMessage, TranscriptRole};
use crate::directories::Directories;
use crate::error::ErrorKind;

/// Version of the result object. Bumped only for an incompatible change.
pub const RESULT_SCHEMA: u32 = 1;
/// Largest prompt accepted, in bytes. A prompt is one user message; provider
/// request limits sit well above this, so the cap guards the caller, not ahi.
pub const PROMPT_CAP: usize = 1024 * 1024;
/// Largest `text` stored in the result object. stdout gets the whole answer.
pub const TEXT_CAP: usize = 4 * 1024 * 1024;

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
    /// `off`, `minimal`, `low`, `medium`, or `high`.
    pub thinking: String,
    pub format: Format,
    pub result_file: Option<PathBuf>,
    pub headless: bool,
    pub run_id: Option<String>,
}

/// Process exit codes. A coordinator reacts to these, not to text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Exit {
    /// Validated successful completion.
    Ok = 0,
    /// Transport, malformed stream, refusal, truncation, tools disabled.
    Turn = 1,
    /// Bad arguments, unreadable or invalid prompt, result file in the way.
    Usage = 2,
    /// `models.json` missing or invalid, unknown gateway or model.
    Config = 3,
    /// Credential missing, rejected, or its refresh failed.
    Credential = 4,
    /// Timeout or a signal (a later slice).
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
    /// `usage`, `config`, or an `ErrorKind` name.
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

    fn turn(message: impl Into<String>, kind: ErrorKind) -> Self {
        Self {
            exit: match kind {
                ErrorKind::Auth => Exit::Credential,
                // A policy failure here is `tool execution is disabled`: the
                // model asked for a tool no policy offered. That is the
                // turn's failure, not a bad policy file (exit 5, later).
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
}

#[derive(Default)]
struct ToolCounts {
    calls: u64,
    denied: u64,
    ask_denied: u64,
    invalid: u64,
    rounds: u64,
}

impl Report {
    fn result(&self, outcome: &Result<String, Failure>) -> Value {
        let mut object = json!({
            "schema": RESULT_SCHEMA,
            "ahi_version": crate::VERSION,
            "run_id": self.run_id,
            "status": match outcome {
                Ok(_) => "ok",
                Err(failure) if failure.exit == Exit::Timeout => "timeout",
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
        // No tool can change a file or run a program until a policy is
        // accepted, so this slice never has effects to account for.
        object["effects"] = json!("none");
        object["ledger"] = json!([]);
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
    let mut report = Report {
        run_id,
        gateway,
        model,
        usage: None,
        context_window: None,
        tools: ToolCounts::default(),
    };
    // The result file is checked before anything else can fail, so every
    // later outcome has somewhere to go. An existing file is someone else's
    // result: refusing it is what keeps a coordinator from reading a stale
    // success.
    if let Some(path) = &options.result_file {
        if let Err(failure) = check_result_path(path) {
            return finish_without_result(failure);
        }
    }
    let outcome = turn(state, options, &mut report);
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

/// The validated contract, then the model. Everything a result object needs
/// lands in `report` on the way.
fn turn(state: &Directories, options: &Options, report: &mut Report) -> Result<String, Failure> {
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
    let prompt = read_prompt(&options.prompt)?;
    // Resolve the destination and credential before any request so a
    // configuration fault and a missing credential get their own codes.
    // Secrets are read here and never printed.
    crate::harness::completion_connection(state, &report.gateway, &report.model).map_err(
        |err| match err.kind() {
            ErrorKind::Auth => Failure::turn(err.to_string(), ErrorKind::Auth),
            _ => Failure::config(err.to_string()),
        },
    )?;
    let mut backend = HttpBackend::new(state.clone()).without_tools();
    let receiver = backend.submit(ChatRequest {
        selection: Selection {
            gateway: report.gateway.clone(),
            model: report.model.clone(),
        },
        thinking,
        messages: vec![ChatMessage::new(TranscriptRole::Operator, prompt)],
    });
    drain(receiver, report)
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
/// kept; `Completed` is the only success. An approval request cannot arrive
/// without tools, and if one ever did, dropping its reply denies the batch.
fn drain(receiver: Receiver<ResponseEvent>, report: &mut Report) -> Result<String, Failure> {
    let mut text = String::new();
    loop {
        let event = receiver.recv().map_err(|_| {
            Failure::turn(
                "model worker ended without a terminal event",
                ErrorKind::Other,
            )
        })?;
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
            }
            ResponseEvent::ToolApprovalRequested(request) => {
                report.tools.ask_denied += request.calls.len() as u64;
            }
            ResponseEvent::ToolResult { exit_code, .. } => match exit_code {
                EXIT_DENIED => report.tools.denied += 1,
                EXIT_OK => {}
                _ => report.tools.invalid += 1,
            },
            ResponseEvent::ToolCallReturned => {}
            ResponseEvent::Completed => return Ok(text),
            ResponseEvent::Failed { message, kind } => return Err(Failure::turn(message, kind)),
        }
    }
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
        Report {
            run_id: "r1".into(),
            gateway: "local".into(),
            model: "vendor/model".into(),
            usage: None,
            context_window: None,
            tools: ToolCounts::default(),
        }
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
