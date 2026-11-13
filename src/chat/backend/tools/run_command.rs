//! `run_command`: run one program with an argv, no shell, in a directory
//! inside the working directory, with the hook runner's scrubbed
//! environment, a timeout, and bounded output.
//!
//! The model receives the exit code, then stdout and stderr as separate
//! sections. Nothing about the process is interpreted: a non-zero exit is a
//! successful tool call whose content says `exit N`.

use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

pub(super) const DEFAULT_TIMEOUT_SECS: u64 = 120;
pub(super) const MAX_TIMEOUT_SECS: u64 = 600;
/// Per-stream cap; the two streams together stay under `read_file`'s cap.
pub(super) const MAX_STREAM_BYTES: usize = 32 * 1024;
/// Argument tokens accepted.
pub(super) const MAX_ARGV: usize = 64;
/// Total argv bytes accepted.
pub(super) const MAX_ARGV_BYTES: usize = 8 * 1024;

/// The two streams as the screen shows them, each capped like `content`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandOutput {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) stdout_cut: bool,
    pub(crate) stderr_cut: bool,
}

/// What ran, for the audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandReport {
    pub(crate) exit_code: Option<i32>,
    pub(crate) duration_ms: u128,
    pub(crate) env_names: Vec<String>,
}

/// The turn was cancelled (`Esc`, a timeout, a signal) while the program ran.
pub(super) const CANCELLED: &str = "command cancelled and killed";

/// Why `run` failed, and whether a process ran first. Every failure before
/// `spawn` returns means nothing ran; a kill or a lost wait means something
/// did, and a caller's ledger must say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunError {
    pub(crate) message: &'static str,
    pub(crate) ran: bool,
}

impl RunError {
    fn before(message: &'static str) -> Self {
        Self {
            message,
            ran: false,
        }
    }

    fn after(message: &'static str) -> Self {
        Self { message, ran: true }
    }
}

/// Run `argv` until it exits, its timeout passes, or `cancel` is set. The
/// child leads its own process group on Unix, and a kill takes the whole
/// group, so a program that spawned helpers cannot leave them holding the
/// output pipes open.
pub(super) fn run(
    argv: &[String],
    cwd: &Path,
    timeout_secs: u64,
    toolchain_env: &[String],
    cancel: &AtomicBool,
) -> Result<(String, CommandReport, CommandOutput), RunError> {
    let (program, args) = argv
        .split_first()
        .ok_or(RunError::before("argv must not be empty"))?;
    if !cwd.is_dir() {
        return Err(RunError::before("not a directory"));
    }
    let mut env = crate::hooks::local_hook::scrub_env(std::env::vars_os(), toolchain_env);
    // The hook runner's marker is not this; name the caller honestly.
    env.retain(|(key, _)| key != "PRE_COMMIT");
    env.push((OsString::from("AHI_TOOL"), OsString::from("run_command")));
    let mut env_names: Vec<String> = env
        .iter()
        .filter_map(|(key, _)| key.to_str().map(str::to_string))
        .collect();
    env_names.sort();

    let started = Instant::now();
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A new group whose leader is the child, so `kill_tree` reaches
        // everything it starts. The child never reads the terminal (stdin is
        // null), so leaving the foreground group costs it nothing.
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|_| RunError::before("cannot start the program (not found or not executable)"))?;
    let stdout = child
        .stdout
        .take()
        .map(|pipe| thread::spawn(move || drain(pipe)));
    let stderr = child
        .stderr
        .take()
        .map(|pipe| thread::spawn(move || drain(pipe)));
    let deadline = started + Duration::from_secs(timeout_secs);
    let mut cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if cancel.load(Ordering::Relaxed) => {
                kill_tree(&mut child);
                cancelled = true;
                break None;
            }
            Ok(None) if Instant::now() >= deadline => {
                kill_tree(&mut child);
                break None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(_) => {
                kill_tree(&mut child);
                return Err(RunError::after("cannot wait for the program"));
            }
        }
    };
    let join = |handle: Option<thread::JoinHandle<(Vec<u8>, bool)>>| {
        handle
            .and_then(|h| h.join().ok())
            .unwrap_or((Vec::new(), false))
    };
    let (out, out_cut) = join(stdout);
    let (err, err_cut) = join(stderr);
    let duration_ms = started.elapsed().as_millis();
    let Some(status) = status else {
        return Err(RunError::after(if cancelled {
            CANCELLED
        } else {
            "command timed out and was killed"
        }));
    };
    let exit_code = status.code();
    let mut content = match exit_code {
        Some(code) => format!("exit {code}"),
        None => "exit (killed by signal)".to_string(),
    };
    section(&mut content, "stdout", &out, out_cut);
    section(&mut content, "stderr", &err, err_cut);
    if out.is_empty() && err.is_empty() {
        content.push_str(" (no output)");
    }
    Ok((
        content,
        CommandReport {
            exit_code,
            duration_ms,
            env_names,
        },
        CommandOutput {
            stdout: String::from_utf8_lossy(&out)
                .trim_end_matches('\n')
                .to_string(),
            stderr: String::from_utf8_lossy(&err)
                .trim_end_matches('\n')
                .to_string(),
            stdout_cut: out_cut,
            stderr_cut: err_cut,
        },
    ))
}

/// End the child and, on Unix, every process in its group, then reap it.
/// The group goes first so a helper cannot outlive the kill and keep the
/// pipes open; the direct kill is the fallback where groups do not exist.
fn kill_tree(child: &mut Child) {
    ahi_unix::kill_process_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// Read a pipe to its end, keeping the first `MAX_STREAM_BYTES` and noting
/// whether more arrived. The pipe is drained fully so the child never
/// blocks on a full buffer.
fn drain(mut pipe: impl Read) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut cut = false;
    let mut buffer = [0u8; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = MAX_STREAM_BYTES.saturating_sub(kept.len());
                if n > room {
                    cut = true;
                }
                kept.extend_from_slice(&buffer[..n.min(room)]);
            }
        }
    }
    (kept, cut)
}

fn section(content: &mut String, name: &str, bytes: &[u8], cut: bool) {
    if bytes.is_empty() {
        return;
    }
    content.push_str("\n--- ");
    content.push_str(name);
    content.push('\n');
    let text = String::from_utf8_lossy(bytes);
    content.push_str(text.trim_end_matches('\n'));
    if cut {
        content.push_str(&format!("\n[{name} truncated at {MAX_STREAM_BYTES} bytes]"));
    }
}

/// Render argv for the approval prompt: tokens separated by spaces, a token
/// with spaces or quotes shown in single quotes, control characters
/// replaced, and the whole cut at 200 characters.
pub(crate) fn render(argv: &[String]) -> String {
    let mut out = String::new();
    for (index, token) in argv.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let clean: String = token
            .chars()
            .map(|c| if c.is_control() { '?' } else { c })
            .collect();
        if clean.is_empty()
            || clean
                .chars()
                .any(|c| c.is_whitespace() || c == '\'' || c == '"')
        {
            out.push('\'');
            out.push_str(&clean.replace('\'', "'\\''"));
            out.push('\'');
        } else {
            out.push_str(&clean);
        }
    }
    if out.chars().count() > 200 {
        let mut cut: String = out.chars().take(200).collect();
        cut.push_str("...");
        return cut;
    }
    out
}
