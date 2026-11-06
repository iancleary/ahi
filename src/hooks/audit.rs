//! Append-only JSON lines. No file contents and no environment values.

use std::fs::{self, OpenOptions};
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

pub struct HookEnv<'a> {
    pub hook: &'a str,
    pub names: &'a [String],
}

pub fn append(cache_dir: &Path, event: &AuditEvent<'_>) -> Result<()> {
    fs::create_dir_all(cache_dir)
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
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(cache_dir.join("audit.log"))
        .map_err(|err| Error::new(format!("failed to open audit log: {err}")))?;
    writeln!(file, "{line}")
        .map_err(|err| Error::new(format!("failed to write audit log: {err}")))?;
    Ok(())
}
