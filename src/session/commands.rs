//! `ahi session`: reading the store, and moving sessions in and out of it.
//! Everything renders to a string the caller prints once.
//!
//! These commands read the state directory's session files by id, or a file
//! by path, and never anything a model can reach. Across machines a
//! coordinator runs them over ssh; `import` brings a ferried file into the
//! local store so one listing covers it.

use std::path::Path;

use serde_json::{json, Value};

use super::{Entry, Kind, Role, Session, Store, StoreError, StoredMessage};
use crate::directories::Directories;
use crate::error::{Error, Result};

/// `ahi session list [--json]`: one row per session, newest first. Files
/// this ahi cannot read are named on stderr by the caller.
pub fn list(state: &Directories, json: bool) -> Result<(String, Vec<String>)> {
    let store = Store::new(state);
    let (rows, skipped) = store.list().map_err(store_error)?;
    let mut out = String::new();
    if json {
        for row in &rows {
            let mut object = row.header.to_json();
            object["entries"] = json!(row.entries);
            object["turns"] = json!(row.turns);
            object["bytes"] = json!(row.bytes);
            if let Some((run_id, status)) = &row.last_turn {
                object["last_status"] = json!(status.as_str());
                object["last_run_id"] = json!(run_id);
            }
            out.push_str(&object.to_string());
            out.push('\n');
        }
        return Ok((out, skipped));
    }
    if rows.is_empty() {
        return Ok(("no sessions yet\n".to_string(), skipped));
    }
    for row in &rows {
        let last = row
            .last_turn
            .as_ref()
            .map(|(run_id, status)| format!("{} ({run_id})", status.as_str()))
            .unwrap_or_else(|| "no turn".to_string());
        out.push_str(&format!(
            "{}  {}  {}/{}  {}  {} turn{}  last {}\n",
            row.header.id,
            row.header.started,
            row.header.gateway,
            row.header.model,
            row.header.cwd,
            row.turns,
            if row.turns == 1 { "" } else { "s" },
            last
        ));
    }
    Ok((out, skipped))
}

/// `ahi session show <id|path> [--json] [--branch <seq>]`: the active path
/// in conversation order, or the branch ending at `seq`; `--json` prints
/// the header and those entries as lines.
pub fn show(state: &Directories, target: &str, json: bool, branch: Option<u64>) -> Result<String> {
    let session = open(state, target)?;
    let from = match branch {
        Some(seq) => {
            if seq == 0 || seq > session.entries().len() as u64 {
                return Err(Error::new(format!(
                    "no entry {seq} in session {} ({} entries)",
                    session.id(),
                    session.entries().len()
                )));
            }
            Some(seq)
        }
        None => session.active_parent(),
    };
    let path = session.path_to_root(from);
    let mut out = String::new();
    if json {
        out.push_str(&session.header().to_json().to_string());
        out.push('\n');
        for entry in path {
            out.push_str(&entry.to_json().to_string());
            out.push('\n');
        }
        return Ok(out);
    }
    let header = session.header();
    out.push_str(&format!(
        "session {}  {}/{}  {}  started {}  host {}\n",
        header.id, header.gateway, header.model, header.cwd, header.started, header.host
    ));
    for entry in path {
        out.push_str(&render(entry));
    }
    Ok(out)
}

fn render(entry: &Entry) -> String {
    match &entry.kind {
        Kind::Message(StoredMessage {
            role: Role::Operator,
            text,
            ..
        }) => format!("\n> {}\n", text.trim_end()),
        Kind::Message(StoredMessage {
            role: Role::Assistant,
            text,
            tool,
        }) => {
            let mut out = String::new();
            if !text.trim().is_empty() {
                out.push_str(&format!("\n{}\n", text.trim_end()));
            }
            if let Some(super::ToolPayload::Calls { calls, .. }) = tool {
                for call in calls {
                    out.push_str(&format!("  [call {} {}]\n", call.name, target_of(call)));
                }
            }
            out
        }
        Kind::Message(StoredMessage {
            role: Role::Tool { exit_code },
            text,
            ..
        }) => format!(
            "  [tool exit {exit_code}] {}\n",
            text.lines().next().unwrap_or_default()
        ),
        Kind::Turn(turn) => format!(
            "— turn {} {}  {}\n",
            turn.run_id,
            turn.status.as_str(),
            entry.timestamp
        ),
        Kind::ModelChange { gateway, model } => format!("— model {gateway}/{model}\n"),
        Kind::ThinkingChange { level } => format!("— thinking {level}\n"),
    }
}

/// The path or argv a call names, for `show`: never the whole argument
/// object, which may hold file contents.
fn target_of(call: &crate::chat::backend::ToolCall) -> String {
    if let Some(argv) = call.arguments.get("argv").and_then(Value::as_array) {
        return argv
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ");
    }
    call.arguments
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(".")
        .to_string()
}

/// `ahi session export <id> --to <path>`: a copy of the session file.
/// Refuses an existing destination.
pub fn export(state: &Directories, id: &str, to: &Path) -> Result<String> {
    let session = Store::new(state).export(id, to).map_err(store_error)?;
    Ok(format!(
        "exported session {} ({} entries) to {}\n",
        session.id(),
        session.entries().len(),
        to.display()
    ))
}

/// `ahi session import <path>`: a session file into the store. A session
/// already present is extended by the file's new tail, or refused when the
/// histories differ.
pub fn import(state: &Directories, path: &Path) -> Result<String> {
    let (session, added) = Store::new(state).import(path).map_err(store_error)?;
    Ok(format!(
        "imported session {} from {}: {added} new entr{}\n",
        session.id(),
        path.display(),
        if added == 1 { "y" } else { "ies" }
    ))
}

/// A session by id in the store, else a session file by path.
fn open(state: &Directories, target: &str) -> Result<Session> {
    let store = Store::new(state);
    if super::valid_id(target) {
        return store.open(target).map_err(store_error);
    }
    let path = Path::new(target);
    if path.is_file() {
        return Session::read(path).map_err(store_error);
    }
    Err(Error::new(format!(
        "no session {target}: not a session id in {} and not a file",
        store.dir().display()
    )))
}

fn store_error(err: StoreError) -> Error {
    Error::new(err.to_string())
}
