//! Append-only JSON lines. No file contents and no environment values.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde_json::json;

use crate::error::{Error, Result};

pub fn append(
    cache_dir: &Path,
    command: &str,
    repo: &Path,
    hook_ids: &[String],
    outcome: &str,
    finding_count: usize,
) -> Result<()> {
    fs::create_dir_all(cache_dir)
        .map_err(|err| Error::new(format!("failed to create audit directory: {err}")))?;
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let line = json!({
        "time": time,
        "command": command,
        "repo": repo.display().to_string(),
        "hooks": hook_ids,
        "outcome": outcome,
        "findings": finding_count,
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
