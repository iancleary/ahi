//! Append-only JSON lines. No file contents and no environment values.

use std::io::Write;
use std::path::Path;

use serde_json::json;

use crate::error::{Error, Result};

pub struct AuditEvent<'a> {
    pub command: &'a str,
    pub repo: &'a Path,
    pub hook_ids: &'a [String],
    pub outcome: &'a str,
    pub finding_count: usize,
    /// Local system/script hook ids that ran.
    pub local_hooks: &'a [String],
    /// Environment names passed to each local hook. No values.
    pub local_env: &'a [HookEnv<'a>],
}

/// One file change made by a chat tool. Hashes, never contents.
pub struct WriteEvent<'a> {
    /// Which front end ran the turn: `tui`, `exec`, or `serve`.
    pub origin: &'a str,
    /// The turn's run id, so a result and this log can be joined.
    pub run_id: Option<&'a str>,
    pub tool: &'a str,
    pub repo: &'a Path,
    /// Workspace-relative path.
    pub path: &'a str,
    pub outcome: &'a str,
    /// SHA-256 of the file before and after, as hex; `None` when absent.
    pub before: Option<&'a str>,
    pub after: Option<&'a str>,
    pub lines_added: usize,
    pub lines_removed: usize,
}

/// Append one `write` line to `audit.log`.
pub fn append_write(state_dir: &Path, event: &WriteEvent<'_>) -> Result<()> {
    append_line(
        state_dir,
        json!({
            "time": now(),
            "command": "tool",
            "origin": event.origin,
            "run_id": event.run_id,
            "tool": event.tool,
            "repo": event.repo.display().to_string(),
            "path": event.path,
            "outcome": event.outcome,
            "before": event.before,
            "after": event.after,
            "lines_added": event.lines_added,
            "lines_removed": event.lines_removed,
        }),
    )
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn append_line(state_dir: &Path, line: serde_json::Value) -> Result<()> {
    crate::directories::create_private_dir(state_dir)
        .map_err(|err| Error::new(format!("failed to create audit directory: {err}")))?;
    let mut file = crate::directories::private_file()
        .create(true)
        .append(true)
        .open(state_dir.join("audit.log"))
        .map_err(|err| Error::new(format!("failed to open audit log: {err}")))?;
    writeln!(file, "{line}")
        .map_err(|err| Error::new(format!("failed to write audit log: {err}")))?;
    Ok(())
}

/// One program run by a chat tool. The argv is recorded because it is the
/// grant; output is not.
pub struct CommandEvent<'a> {
    pub origin: &'a str,
    pub run_id: Option<&'a str>,
    pub repo: &'a Path,
    pub argv: &'a [String],
    /// Workspace-relative working directory; empty is the root.
    pub cwd: &'a str,
    pub outcome: &'a str,
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    /// Environment names passed to the process. No values.
    pub env_names: &'a [String],
}

/// Append one `command` line to `audit.log`.
pub fn append_command(state_dir: &Path, event: &CommandEvent<'_>) -> Result<()> {
    append_line(
        state_dir,
        json!({
            "time": now(),
            "command": "tool",
            "origin": event.origin,
            "run_id": event.run_id,
            "tool": "run_command",
            "repo": event.repo.display().to_string(),
            "argv": event.argv,
            "cwd": event.cwd,
            "outcome": event.outcome,
            "exit_code": event.exit_code,
            "duration_ms": event.duration_ms,
            "env": event.env_names,
        }),
    )
}

pub struct HookEnv<'a> {
    pub hook: &'a str,
    pub names: &'a [String],
}

/// Append one line to `audit.log` in the state directory. The directory and
/// the log are owner-only on Unix: the log names repositories and hooks.
pub fn append(state_dir: &Path, event: &AuditEvent<'_>) -> Result<()> {
    crate::directories::create_private_dir(state_dir)
        .map_err(|err| Error::new(format!("failed to create audit directory: {err}")))?;
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let line = json!({
        "time": time,
        "command": event.command,
        "repo": event.repo.display().to_string(),
        "hooks": event.hook_ids,
        "outcome": event.outcome,
        "findings": event.finding_count,
        "local_hooks": event.local_hooks,
        "local_env": event.local_env.iter().map(|item| json!({
            "hook": item.hook,
            "names": item.names,
        })).collect::<Vec<_>>(),
    });
    let mut file = crate::directories::private_file()
        .create(true)
        .append(true)
        .open(state_dir.join("audit.log"))
        .map_err(|err| Error::new(format!("failed to open audit log: {err}")))?;
    writeln!(file, "{line}")
        .map_err(|err| Error::new(format!("failed to write audit log: {err}")))?;
    Ok(())
}
