//! `ahi session`: reading sessions, here and from files other machines
//! wrote. Everything prints to a string the caller writes once.
//!
//! The store is useful only if something reads it. These commands read the
//! state directory's database by session id, or a JSONL file by path, and
//! never anything a model can reach. Across machines a coordinator runs
//! them over ssh and merges the rows; `import` brings a ferried file into
//! the local database so one query covers it.

use std::fs;
use std::io::Write;
use std::path::Path;

use serde_json::{json, Value};

use super::jsonl::Jsonl;
use super::sqlite::Sqlite;
use super::{Entry, Kind, Role, SessionStore, StoreError, StoredMessage};
use crate::chat::exec::sessions_database;
use crate::directories::Directories;
use crate::error::{Error, Result};

/// `ahi session list [--json]`: one row per session, newest first.
pub fn list(state: &Directories, json: bool) -> Result<String> {
    let database = sessions_database(state);
    if !database.exists() {
        return Ok(if json {
            String::new()
        } else {
            "no sessions yet\n".to_string()
        });
    }
    let listed = Sqlite::list(&database).map_err(store_error)?;
    let mut out = String::new();
    if json {
        for row in &listed {
            let mut object = row.header.to_json();
            object["entries"] = json!(row.entries);
            object["tip"] = json!(row.tip);
            if let Some(turn) = &row.last_turn {
                object["turns"] = json!(row.turns);
                object["last_status"] = json!(turn.status);
                object["last_run_id"] = json!(turn.run_id);
            } else {
                object["turns"] = json!(0);
            }
            out.push_str(&object.to_string());
            out.push('\n');
        }
        return Ok(out);
    }
    if listed.is_empty() {
        return Ok("no sessions yet\n".to_string());
    }
    for row in &listed {
        let last = row
            .last_turn
            .as_ref()
            .map(|turn| format!("{} ({})", turn.status, turn.run_id))
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
    Ok(out)
}

/// `ahi session show <id|path> [--json] [--branch <entry>]`: the active path
/// from the tip to the root in conversation order, or another branch.
pub fn show(state: &Directories, target: &str, json: bool, branch: Option<&str>) -> Result<String> {
    let store = open(state, target)?;
    let from = match branch {
        Some(id) => {
            if !store.entries().iter().any(|entry| entry.id == id) {
                return Err(Error::new(format!(
                    "no entry {id} in session {}",
                    store.header().id
                )));
            }
            Some(id.to_string())
        }
        None => store.tip().map(str::to_string),
    };
    let path = store.path_to_root(from.as_deref());
    let mut out = String::new();
    if json {
        out.push_str(&store.header().to_json().to_string());
        out.push('\n');
        for entry in path {
            out.push_str(&entry.to_json().to_string());
            out.push('\n');
        }
        return Ok(out);
    }
    let header = store.header();
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
            "— turn {} {} (exit {})  {}\n",
            turn.run_id, turn.status, turn.exit_code, entry.timestamp
        ),
        Kind::ModelChange { gateway, model } => format!("— model {gateway}/{model}\n"),
        Kind::ThinkingChange { level } => format!("— thinking {level}\n"),
    }
}

/// The path or argv a call names, for `show` and `grep`: never the whole
/// argument object, which may hold file contents.
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

/// `ahi session grep <pattern> [--path <jsonl>] [--json]`: every matching
/// line of message text, tool target, or tool output, as
/// `<session>:<entry>: <line>`.
pub fn grep(state: &Directories, pattern: &str, path: Option<&Path>, json: bool) -> Result<String> {
    let regex = regex::Regex::new(pattern)
        .map_err(|err| Error::new(format!("invalid regular expression: {err}")))?;
    let mut sources: Vec<Box<dyn SessionStore>> = Vec::new();
    match path {
        Some(path) => sources.push(Box::new(Jsonl::open(path).map_err(store_error)?)),
        None => {
            let database = sessions_database(state);
            if database.exists() {
                for row in Sqlite::list(&database).map_err(store_error)? {
                    match Sqlite::open(&database, &row.header.id) {
                        Ok(store) => sources.push(Box::new(store)),
                        Err(StoreError::Schema(_)) => {}
                        Err(err) => return Err(store_error(err)),
                    }
                }
            }
        }
    }
    let mut out = String::new();
    for store in &sources {
        let session = &store.header().id;
        for entry in store.entries() {
            for line in searchable(entry) {
                if regex.is_match(&line) {
                    if json {
                        out.push_str(
                            &json!({"session": session, "entry": entry.id, "timestamp": entry.timestamp, "line": line})
                                .to_string(),
                        );
                    } else {
                        out.push_str(&format!("{session}:{}: {line}", entry.id));
                    }
                    out.push('\n');
                }
            }
        }
    }
    Ok(out)
}

/// The lines of an entry a search sees: message text, call targets, and
/// tool output; never native replay items or result objects.
fn searchable(entry: &Entry) -> Vec<String> {
    match &entry.kind {
        Kind::Message(message) => {
            let mut lines: Vec<String> = message.text.lines().map(str::to_string).collect();
            if let Some(super::ToolPayload::Calls { calls, .. }) = &message.tool {
                lines.extend(
                    calls
                        .iter()
                        .map(|call| format!("{} {}", call.name, target_of(call))),
                );
            }
            lines
        }
        Kind::Turn(turn) => vec![format!("turn {} {}", turn.run_id, turn.status)],
        Kind::ModelChange { gateway, model } => vec![format!("model {gateway}/{model}")],
        Kind::ThinkingChange { level } => vec![format!("thinking {level}")],
    }
}

/// `ahi session export <id> --to <path>`: the session as a JSONL file.
/// Refuses an existing destination.
pub fn export(state: &Directories, id: &str, to: &Path) -> Result<String> {
    let store = Sqlite::open(&sessions_database(state), id).map_err(store_error)?;
    if fs::symlink_metadata(to).is_ok() {
        return Err(Error::new(format!(
            "{} already exists; remove it or name another path",
            to.display()
        )));
    }
    let mut file = crate::directories::private_file()
        .create_new(true)
        .open(to)
        .map_err(|err| Error::new(format!("cannot create {}: {err}", to.display())))?;
    let mut bytes = store.header().to_json().to_string().into_bytes();
    bytes.push(b'\n');
    for entry in store.entries() {
        bytes.extend_from_slice(entry.to_json().to_string().as_bytes());
        bytes.push(b'\n');
    }
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|err| Error::new(format!("cannot write {}: {err}", to.display())))?;
    Ok(format!(
        "exported session {} ({} entries) to {}\n",
        store.header().id,
        store.entries().len(),
        to.display()
    ))
}

/// `ahi session import <path>`: a JSONL session into the database. A session
/// already present is extended by the file's new tail, or refused when the
/// histories differ.
pub fn import(state: &Directories, path: &Path) -> Result<String> {
    let file = Jsonl::open(path).map_err(store_error)?;
    let added = Sqlite::import(&sessions_database(state), file.header(), file.entries())
        .map_err(store_error)?;
    Ok(format!(
        "imported session {} from {}: {added} new entr{}\n",
        file.header().id,
        path.display(),
        if added == 1 { "y" } else { "ies" }
    ))
}

/// A session by id in the database, or a JSONL file by path.
fn open(state: &Directories, target: &str) -> Result<Box<dyn SessionStore>> {
    let as_path = Path::new(target);
    if as_path.is_file() {
        return Ok(Box::new(Jsonl::open(as_path).map_err(store_error)?));
    }
    let database = sessions_database(state);
    if !database.exists() {
        return Err(Error::new(format!(
            "no session {target}: {} does not exist and {target} is not a file",
            database.display()
        )));
    }
    Ok(Box::new(
        Sqlite::open(&database, target).map_err(store_error)?,
    ))
}

fn store_error(err: StoreError) -> Error {
    Error::new(err.to_string())
}
