//! `ahi exec`: one model turn from the command line, for a person at a
//! shell or a coordinator driving a worker over ssh.
//!
//! The command is a client of the session service. On a host where
//! `ahi serve` answers on its socket, the turn is submitted there and
//! followed over the socket; otherwise the service is constructed in this
//! process and called directly. Either way the engine, the policy, the
//! record, and the result document are the same, and the command's own
//! surface is files and an exit code: a prompt in, a result file out,
//! progress on stderr, the answer on stdout.
//!
//! `docs/SERVE.md` has the contract; the exit codes are in `Exit`.

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use serde_json::{json, Value};

use crate::directories::Directories;
use crate::service::client::Client;
use crate::service::wire;
use crate::service::{Event, TurnRequest};

/// The largest prompt accepted, from any source.
const MAX_PROMPT_BYTES: usize = 1024 * 1024;
/// The turn's bound when `--timeout` is not given.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// The command's exit codes, the one interface besides the result file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Validated successful completion.
    Ok = 0,
    /// The turn failed: transport, protocol, a refusal, tools disabled.
    Turn = 1,
    /// Bad arguments or files: prompt, workdir, result file, digest.
    Usage = 2,
    /// Configuration: `models.json`, an unknown gateway or model.
    Config = 3,
    /// A credential is missing or could not be refreshed.
    Auth = 4,
    /// The policy file is missing or invalid.
    Policy = 5,
    /// The turn was cancelled or timed out.
    Cancelled = 6,
    /// The session store refused: unknown session, used `run_id`, unreadable file.
    Session = 7,
    /// The turn ran but the result file could not be written.
    ResultFile = 8,
}

impl Exit {
    pub fn code(self) -> u8 {
        self as u8
    }

    /// The exit for a refused request, by the error kind the service names.
    fn for_kind(kind: &str) -> Self {
        match kind {
            "usage" => Self::Usage,
            "config" => Self::Config,
            "auth" => Self::Auth,
            "policy" => Self::Policy,
            "timeout" | "cancelled" => Self::Cancelled,
            "session" | "conflict" | "not_found" => Self::Session,
            _ => Self::Turn,
        }
    }

    /// The exit for a closed turn's result document.
    fn for_result(doc: &Value) -> Self {
        match doc["status"].as_str() {
            Some("ok") => Self::Ok,
            Some("cancelled" | "timeout") => Self::Cancelled,
            _ => match doc["error"]["kind"].as_str() {
                Some("auth") => Self::Auth,
                Some("policy") => Self::Policy,
                _ => Self::Turn,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
}

/// What the command line said.
#[derive(Debug, Clone)]
pub struct Options {
    /// `gateway/model`.
    pub model: String,
    pub prompt: Option<String>,
    pub prompt_file: Option<PathBuf>,
    pub headless: bool,
    pub workdir: Option<PathBuf>,
    pub tools: Option<PathBuf>,
    pub timeout_secs: Option<u64>,
    pub run_id: Option<String>,
    pub prompt_sha256: Option<String>,
    /// Continue a recorded session instead of starting one.
    pub session: Option<String>,
    pub result_file: Option<PathBuf>,
    pub format: Format,
    pub thinking: Option<String>,
    pub check: bool,
    pub socket: Option<PathBuf>,
    pub standalone: bool,
    pub verbose: bool,
}

/// Run the command. Returns the exit code; everything a person needs to
/// read is already on stderr, the answer on stdout, the document in the
/// result file.
pub fn run(state: &Directories, options: &Options) -> u8 {
    match execute(state, options) {
        Ok(exit) => exit.code(),
        Err((exit, message)) => {
            eprintln!("error: {message}");
            exit.code()
        }
    }
}

type Outcome<T> = Result<T, (Exit, String)>;

fn usage(message: impl Into<String>) -> (Exit, String) {
    (Exit::Usage, message.into())
}

fn execute(state: &Directories, options: &Options) -> Outcome<Exit> {
    let selection = crate::chat::backend::parse_selection(&options.model);
    if selection.gateway.is_empty() || selection.model.is_empty() {
        return Err(usage("--model takes gateway/model-id"));
    }
    if let Some(level) = &options.thinking {
        if crate::chat::thinking::ThinkingLevel::parse(level).is_none() {
            return Err(usage(format!("unknown thinking level `{level}`")));
        }
    }
    if let Some(run_id) = &options.run_id {
        if run_id.is_empty()
            || run_id.len() > 64
            || !run_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(usage("--run-id is 1 to 64 letters, digits, `-`, or `_`"));
        }
    }
    let workdir = options
        .workdir
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| usage("cannot determine the working directory"))?;
    let workdir = workdir
        .canonicalize()
        .ok()
        .filter(|path| path.is_dir())
        .ok_or_else(|| {
            usage(format!(
                "--workdir {} is not a directory",
                workdir.display()
            ))
        })?;
    if options.headless {
        if options.prompt.is_some() {
            return Err(usage(
                "--headless takes the prompt from --prompt-file, not -p",
            ));
        }
        if options.prompt_file.is_none() {
            return Err(usage("--headless requires --prompt-file"));
        }
        if options.result_file.is_none() {
            return Err(usage("--headless requires --result-file"));
        }
    }
    // Operator files live outside the working directory, where the model's
    // tools cannot read or overwrite them.
    for (flag, path) in [
        ("--prompt-file", options.prompt_file.as_deref()),
        ("--result-file", options.result_file.as_deref()),
        ("--tools", options.tools.as_deref()),
    ] {
        if let Some(path) = path {
            if inside(path, &workdir) {
                return Err(usage(format!(
                    "{flag} {} lies inside the working directory; operator files must be outside it",
                    path.display()
                )));
            }
        }
    }
    if let Some(result_file) = &options.result_file {
        if std::fs::symlink_metadata(result_file).is_ok() {
            return Err(usage(format!(
                "--result-file {} already exists; a stale result must not be mistaken for this run's",
                result_file.display()
            )));
        }
    }
    if let Some(tools) = &options.tools {
        crate::chat::check_actions_file(tools).map_err(|err| (Exit::Policy, err.to_string()))?;
    }
    if options.check {
        crate::harness::completion_connection(state, &selection.gateway, &selection.model)
            .map_err(|err| {
                (
                    if err.kind() == crate::error::ErrorKind::Auth {
                        Exit::Auth
                    } else {
                        Exit::Config
                    },
                    err.to_string(),
                )
            })?;
        eprintln!("ok: {} resolves and the files are in place", options.model);
        return Ok(Exit::Ok);
    }
    let prompt = read_prompt(options)?;
    let digest = crate::session::sha256_hex(prompt.as_bytes());
    if let Some(given) = &options.prompt_sha256 {
        if !given.eq_ignore_ascii_case(&digest) {
            return Err(usage(
                "the prompt's SHA-256 does not match --prompt-sha256; the file may have arrived changed",
            ));
        }
    }

    let mut client = Client::open(state, options.socket.as_deref(), options.standalone)
        .map_err(|err| (Exit::for_kind(err.kind), err.message))?;
    if options.verbose {
        if let Some(path) = client.socket() {
            eprintln!("using the server on {}", path.display());
        }
    }
    let mut call = |method: &str, params: Value| -> Outcome<Value> {
        client
            .call(method, params)
            .map_err(|err| (Exit::for_kind(err.kind), err.message))
    };
    let session = match &options.session {
        Some(id) => {
            call("session.get", json!({"session": id}))?;
            id.clone()
        }
        None => {
            let header = call(
                "session.create",
                json!({
                    "cwd": workdir.display().to_string(),
                    "gateway": selection.gateway,
                    "model": selection.model,
                    "thinking": options.thinking.clone().unwrap_or_else(|| "off".into()),
                }),
            )?;
            header["id"]
                .as_str()
                .ok_or_else(|| (Exit::Session, "the service returned no session id".into()))?
                .to_string()
        }
    };
    let attended =
        !options.headless && std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let progress = options.headless || options.verbose;
    let request = TurnRequest {
        prompt,
        run_id: options.run_id.clone(),
        prompt_sha256: Some(digest),
        workdir: workdir.clone(),
        tools: options.tools.clone(),
        timeout: Some(Duration::from_secs(
            options.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
        )),
        thinking: options.thinking.clone(),
        unattended: !attended,
        origin: "exec",
        client: "ahi exec".to_string(),
    };
    // Subscribe before submitting so no event is missed.
    let tip = call("session.get", json!({"session": session}))?["tip"]
        .as_u64()
        .unwrap_or(0);
    call(
        "session.subscribe",
        json!({"session": session, "after": tip}),
    )?;
    let submitted = call("turn.submit", wire::turn_params(&session, &request))?;
    let doc = if submitted["status"] == "running" {
        let run_id = submitted["run_id"].as_str().unwrap_or_default().to_string();
        if progress {
            eprintln!("run {run_id} session {session} started");
        }
        follow(&mut client, &session, &run_id, attended, progress)?
    } else {
        if progress {
            eprintln!(
                "run {} session {session} answered from the record",
                submitted["run_id"].as_str().unwrap_or_default()
            );
        }
        submitted
    };

    let exit = Exit::for_result(&doc);
    if let Some(path) = &options.result_file {
        write_new(path, &format!("{doc:#}\n")).map_err(|err| {
            (
                Exit::ResultFile,
                format!(
                    "the turn ended {} but the result file could not be written: {err}",
                    doc["status"].as_str().unwrap_or("?")
                ),
            )
        })?;
    }
    match options.format {
        Format::Json => {
            println!("{doc:#}");
        }
        Format::Text => {
            if exit == Exit::Ok {
                let text = doc["text"].as_str().unwrap_or_default();
                print!("{text}");
                if !text.ends_with('\n') {
                    println!();
                }
            } else if let Some(message) = doc["error"]["message"].as_str() {
                eprintln!(
                    "turn {}: {message}",
                    doc["status"].as_str().unwrap_or("failed")
                );
            } else {
                eprintln!("turn {}", doc["status"].as_str().unwrap_or("failed"));
            }
        }
    }
    Ok(exit)
}

/// Follow the running turn to its `turn` event: progress on stderr, asks
/// answered at the terminal when a person is attending, a signal becomes a
/// cancel.
fn follow(
    client: &mut Client,
    session: &str,
    run_id: &str,
    attended: bool,
    progress: bool,
) -> Outcome<Value> {
    let watched = ahi_unix::watch(&[
        ahi_unix::Signal::Interrupt,
        ahi_unix::Signal::Terminate,
        ahi_unix::Signal::Hangup,
    ]);
    if !watched && progress {
        eprintln!("warning: termination signals are not watched on this platform");
    }
    let mut cancelled = false;
    loop {
        if !cancelled {
            if let Some(signal) = ahi_unix::received() {
                eprintln!("{}: cancelling run {run_id}", signal.name());
                cancelled = true;
                let _ = client.call("turn.cancel", json!({"session": session, "run_id": run_id}));
            }
        }
        let event = match client.next_event(Duration::from_millis(100)) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return Err((
                    Exit::Turn,
                    "the connection to the service closed before the turn ended".into(),
                ))
            }
        };
        match event {
            Event::Turn {
                run_id: ended,
                result,
                ..
            } if ended == run_id => return Ok(result),
            Event::Entry { entry, .. } if progress => {
                if entry["type"] == "message" && entry["role"] == "tool" {
                    let name = entry["result"]["name"].as_str().unwrap_or("tool");
                    let first = entry["text"].as_str().unwrap_or_default();
                    // The preview's first line is `<tool> <target>`, with
                    // `: <reason>` after it for a denial or an error.
                    let target = first
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .strip_prefix(name)
                        .map(str::trim)
                        .unwrap_or_default()
                        .split(": ")
                        .next()
                        .unwrap_or_default();
                    eprintln!(
                        "run {run_id} tool {name} {target} exit {}",
                        entry["exit_code"]
                    );
                }
            }
            Event::Ask {
                ask_id,
                calls,
                run_id: asked,
                ..
            } if asked == run_id => {
                let allow: Vec<bool> = if attended {
                    calls.iter().map(ask_terminal).collect()
                } else {
                    vec![false; calls.len()]
                };
                client
                    .call(
                        "ask.answer",
                        json!({"session": session, "ask_id": ask_id, "allow": allow}),
                    )
                    .map_err(|err| (Exit::for_kind(err.kind), err.message))?;
            }
            _ => {}
        }
    }
}

/// One yes/no question on the terminal for a call the policy could not decide.
fn ask_terminal(call: &Value) -> bool {
    let tool = call["tool"].as_str().unwrap_or("tool");
    let target = call["target"].as_str().unwrap_or(".");
    let detail = call["detail"].as_str().unwrap_or_default();
    eprint!("allow {tool} {target}");
    if !detail.is_empty() {
        eprint!(" ({detail})");
    }
    eprint!("? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim(), "y" | "Y" | "yes")
}

/// The prompt from exactly one source: `-p`, `--prompt-file`, or stdin when
/// it is not a terminal.
fn read_prompt(options: &Options) -> Outcome<String> {
    let text = match (&options.prompt, &options.prompt_file) {
        (Some(_), Some(_)) => return Err(usage("give -p or --prompt-file, not both")),
        (Some(text), None) => text.clone(),
        (None, Some(path)) => {
            let metadata = std::fs::metadata(path)
                .map_err(|err| usage(format!("cannot read {}: {err}", path.display())))?;
            if !metadata.is_file() {
                return Err(usage(format!("{} is not a file", path.display())));
            }
            if metadata.len() > MAX_PROMPT_BYTES as u64 {
                return Err(usage(format!(
                    "{} is larger than the {MAX_PROMPT_BYTES} byte prompt cap",
                    path.display()
                )));
            }
            let bytes = std::fs::read(path)
                .map_err(|err| usage(format!("cannot read {}: {err}", path.display())))?;
            String::from_utf8(bytes)
                .map_err(|_| usage(format!("{} is not UTF-8 text", path.display())))?
        }
        (None, None) => {
            if options.headless || std::io::stdin().is_terminal() {
                return Err(usage(
                    "no prompt: give -p <text>, --prompt-file <path>, or pipe text on stdin",
                ));
            }
            let mut bytes = Vec::new();
            std::io::stdin()
                .lock()
                .take(MAX_PROMPT_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|err| usage(format!("cannot read stdin: {err}")))?;
            if bytes.len() > MAX_PROMPT_BYTES {
                return Err(usage(format!(
                    "stdin is larger than the {MAX_PROMPT_BYTES} byte prompt cap"
                )));
            }
            String::from_utf8(bytes).map_err(|_| usage("stdin is not UTF-8 text"))?
        }
    };
    if text.trim().is_empty() {
        return Err(usage("the prompt is empty"));
    }
    if text.len() > MAX_PROMPT_BYTES {
        return Err(usage(format!(
            "the prompt is larger than the {MAX_PROMPT_BYTES} byte cap"
        )));
    }
    Ok(text)
}

/// Whether `path` (which may not exist yet) lies inside `root`.
fn inside(path: &Path, root: &Path) -> bool {
    let resolved = match path.canonicalize() {
        Ok(resolved) => resolved,
        Err(_) => match path.parent().and_then(|parent| parent.canonicalize().ok()) {
            Some(parent) => parent.join(path.file_name().unwrap_or_default()),
            None => return false,
        },
    };
    resolved.starts_with(root)
}

/// Write `text` to a file that must not exist yet: a temporary beside it,
/// owner-only, then a rename, so a reader never sees a partial document.
fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let tmp = match parent {
        Some(parent) => parent.join(format!(
            ".{}.tmp-{}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        )),
        None => PathBuf::from(format!(".result.tmp-{}", std::process::id())),
    };
    let mut file = crate::directories::private_file()
        .create_new(true)
        .open(&tmp)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(err) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    if std::fs::symlink_metadata(path).is_ok() {
        let _ = std::fs::remove_file(&tmp);
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "the result file appeared during the run",
        ));
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_the_result_and_the_refusal_kind() {
        assert_eq!(Exit::for_result(&json!({"status": "ok"})), Exit::Ok);
        assert_eq!(
            Exit::for_result(&json!({"status": "cancelled"})),
            Exit::Cancelled
        );
        assert_eq!(
            Exit::for_result(&json!({"status": "timeout"})),
            Exit::Cancelled
        );
        assert_eq!(
            Exit::for_result(&json!({"status": "failed", "error": {"kind": "auth"}})),
            Exit::Auth
        );
        assert_eq!(
            Exit::for_result(&json!({"status": "failed", "error": {"kind": "policy"}})),
            Exit::Policy
        );
        assert_eq!(
            Exit::for_result(&json!({"status": "failed", "error": {"kind": "transport"}})),
            Exit::Turn
        );
        assert_eq!(Exit::for_kind("config").code(), 3);
        assert_eq!(Exit::for_kind("not_found").code(), 7);
        assert_eq!(Exit::for_kind("weird").code(), 1);
    }

    #[test]
    fn result_files_are_created_once_and_inside_is_lexical_after_canonical() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("exec-result");
        let path = dir.join("out/result.json");
        std::fs::create_dir(dir.join("out")).unwrap();
        write_new(&path, "{}\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
        assert_eq!(
            write_new(&path, "{}\n").unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert!(
            std::fs::read_dir(dir.join("out")).unwrap().count() == 1,
            "no temp file left"
        );
        let root = dir.canonicalize().unwrap();
        assert!(
            inside(&dir.join("out/new.json"), &root),
            "a file that does not exist yet"
        );
        assert!(inside(&path, &root));
        assert!(!inside(Path::new("/"), &root));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
