//! The database backend: every session of a state directory in one
//! SQLite-format file, `<state>/sessions.db`, through the in-repo
//! `ahi-store-turso` crate. Rows hold the same JSON a JSONL line holds, so
//! `export` and `import` are a copy, not a translation.
//!
//! The engine locks the file for the process that has it open, so this
//! backend never keeps the database open between operations: each read or
//! append opens, works inside one transaction, and closes. Another process
//! that finds the file locked retries for a bounded time and then reports a
//! conflict, which is exit 7 for `ahi exec`, the same as a moved JSONL tip.
//! Tip checks still happen inside the transaction, so two writers on one
//! session cannot both win even when their opens interleave.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ahi_store_turso::{Connection, Database, Error, Value};

use super::{entry_id, Entry, Header, Kind, SessionStore, StoreError};

/// How long an open waits for another process to let go of the file.
const LOCK_WAIT: Duration = Duration::from_secs(3);
const LOCK_POLL: Duration = Duration::from_millis(25);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY,
    started TEXT NOT NULL,
    host TEXT NOT NULL,
    cwd TEXT NOT NULL,
    gateway TEXT NOT NULL,
    model TEXT NOT NULL,
    header TEXT NOT NULL,
    tip TEXT
);
CREATE TABLE IF NOT EXISTS entries (
    conversation_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    id TEXT NOT NULL,
    parent_id TEXT,
    type TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (conversation_id, seq)
);
CREATE INDEX IF NOT EXISTS entries_by_id ON entries (conversation_id, id);
CREATE TABLE IF NOT EXISTS submissions (
    conversation_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    entry_id TEXT NOT NULL,
    PRIMARY KEY (conversation_id, run_id)
);
";

pub struct Sqlite {
    path: PathBuf,
    header: Header,
    entries: Vec<Entry>,
    tip: Option<String>,
}

/// One row of `ahi session list`: the header plus what the last turn said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub header: Header,
    pub tip: Option<String>,
    pub entries: usize,
    pub turns: usize,
    pub last_turn: Option<super::Turn>,
}

impl Sqlite {
    /// Start a new session in the database at `path`, creating the file and
    /// the schema when needed.
    pub fn create(path: &Path, header: Header) -> Result<Self, StoreError> {
        let connection = open(path)?;
        connection
            .execute(
                "INSERT INTO conversations (id, started, host, cwd, gateway, model, header, tip) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
                &[
                    header.id.as_str().into(),
                    header.started.as_str().into(),
                    header.host.as_str().into(),
                    header.cwd.as_str().into(),
                    header.gateway.as_str().into(),
                    header.model.as_str().into(),
                    header.to_json().to_string().into(),
                ],
            )
            .map_err(|err| store_error(path, err))?;
        Ok(Self {
            path: path.to_path_buf(),
            header,
            entries: Vec::new(),
            tip: None,
        })
    }

    /// Open the session `id` in the database at `path`.
    pub fn open(path: &Path, id: &str) -> Result<Self, StoreError> {
        let connection = open(path)?;
        let rows = connection
            .query(
                "SELECT header, tip FROM conversations WHERE id = ?1",
                &[id.into()],
            )
            .map_err(|err| store_error(path, err))?;
        let Some(row) = rows.first() else {
            return Err(StoreError::Io(format!(
                "{}: no session {id}",
                path.display()
            )));
        };
        let header = Header::from_json(&parse(path, row[0].as_str().unwrap_or("{}"))?)?;
        let tip = row[1].as_str().map(str::to_string);
        let entries = load_entries(&connection, path, id)?;
        Ok(Self {
            path: path.to_path_buf(),
            header,
            entries,
            tip,
        })
    }

    /// Every session in the database, newest first, with its turn count and
    /// last `turn` line: the first and last lines a listing needs, and
    /// nothing in between.
    pub fn list(path: &Path) -> Result<Vec<Listed>, StoreError> {
        let connection = open(path)?;
        let rows = connection
            .query(
                "SELECT c.header, c.tip, \
                   (SELECT COUNT(*) FROM entries e WHERE e.conversation_id = c.id), \
                   (SELECT COUNT(*) FROM entries e WHERE e.conversation_id = c.id AND e.type = 'turn'), \
                   (SELECT body FROM entries e WHERE e.conversation_id = c.id AND e.type = 'turn' ORDER BY seq DESC LIMIT 1) \
                 FROM conversations c ORDER BY c.started DESC, c.id",
                &[],
            )
            .map_err(|err| store_error(path, err))?;
        rows.iter()
            .map(|row| {
                let last_turn = match row[4].as_str() {
                    Some(body) => match Entry::from_json(&parse(path, body)?)?.kind {
                        Kind::Turn(turn) => Some(turn),
                        _ => None,
                    },
                    None => None,
                };
                Ok(Listed {
                    header: Header::from_json(&parse(path, row[0].as_str().unwrap_or("{}"))?)?,
                    tip: row[1].as_str().map(str::to_string),
                    entries: row[2].as_i64().unwrap_or(0).max(0) as usize,
                    turns: row[3].as_i64().unwrap_or(0).max(0) as usize,
                    last_turn,
                })
            })
            .collect()
    }

    /// Put a whole session, header and entries as read from a JSONL file,
    /// into the database. Refused when the id is already present unless the
    /// stored entries are a prefix of `entries`, in which case the new tail
    /// is appended: this is how a worker's follow-up turns arrive.
    pub fn import(path: &Path, header: &Header, entries: &[Entry]) -> Result<usize, StoreError> {
        let connection = open(path)?;
        let existing = connection
            .query(
                "SELECT 1 FROM conversations WHERE id = ?1",
                &[header.id.as_str().into()],
            )
            .map_err(|err| store_error(path, err))?;
        let stored = if existing.is_empty() {
            connection
                .execute(
                    "INSERT INTO conversations (id, started, host, cwd, gateway, model, header, tip) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
                    &[
                        header.id.as_str().into(),
                        header.started.as_str().into(),
                        header.host.as_str().into(),
                        header.cwd.as_str().into(),
                        header.gateway.as_str().into(),
                        header.model.as_str().into(),
                        header.to_json().to_string().into(),
                    ],
                )
                .map_err(|err| store_error(path, err))?;
            Vec::new()
        } else {
            load_entries(&connection, path, &header.id)?
        };
        if stored.len() > entries.len() || stored[..] != entries[..stored.len()] {
            return Err(StoreError::Conflict(format!(
                "{}: session {} is already present with a different history; nothing was imported",
                path.display(),
                header.id
            )));
        }
        let tail = &entries[stored.len()..];
        let mut statements = Vec::with_capacity(tail.len() + 1);
        let mut seq = stored.len() as i64;
        for entry in tail {
            seq += 1;
            statements.push(insert_entry(&header.id, seq, entry));
            if let Kind::Turn(turn) = &entry.kind {
                statements.push(insert_submission(&header.id, &turn.run_id, &entry.id));
            }
        }
        let tip = entries.last().map(|entry| entry.id.clone());
        statements.push((
            "UPDATE conversations SET tip = ?1 WHERE id = ?2",
            vec![tip.into(), header.id.as_str().into()],
        ));
        let owned: Vec<(&str, Vec<Value>)> = statements
            .iter()
            .map(|(sql, params)| (*sql, params.clone()))
            .collect();
        connection
            .transaction(&owned)
            .map_err(|err| store_error(path, err))?;
        Ok(tail.len())
    }
}

/// Open the database, waiting out another process's lock for a while, and
/// make sure the schema exists.
fn open(path: &Path) -> Result<Connection, StoreError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        crate::directories::create_private_dir(parent).map_err(|err| {
            StoreError::Io(format!(
                "cannot create the session directory {}: {err}",
                parent.display()
            ))
        })?;
    }
    let deadline = Instant::now() + LOCK_WAIT;
    let database = loop {
        match Database::open(path) {
            Ok(database) => break database,
            Err(Error::Locked(message)) if Instant::now() < deadline => {
                let _ = message;
                std::thread::sleep(LOCK_POLL);
            }
            Err(err) => return Err(store_error(path, err)),
        }
    };
    let connection = database.connect().map_err(|err| store_error(path, err))?;
    connection
        .execute_batch(SCHEMA)
        .map_err(|err| store_error(path, err))?;
    // The database holds prompts and answers: owner-only, like the JSONL.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(connection)
}

fn load_entries(connection: &Connection, path: &Path, id: &str) -> Result<Vec<Entry>, StoreError> {
    let rows = connection
        .query(
            "SELECT body FROM entries WHERE conversation_id = ?1 ORDER BY seq",
            &[id.into()],
        )
        .map_err(|err| store_error(path, err))?;
    rows.iter()
        .map(|row| Entry::from_json(&parse(path, row[0].as_str().unwrap_or("{}"))?))
        .collect()
}

fn insert_entry(session: &str, seq: i64, entry: &Entry) -> (&'static str, Vec<Value>) {
    let kind = match &entry.kind {
        Kind::Message(_) => "message",
        Kind::Turn(_) => "turn",
        Kind::ModelChange { .. } => "model_change",
        Kind::ThinkingChange { .. } => "thinking_change",
    };
    (
        "INSERT INTO entries (conversation_id, seq, id, parent_id, type, timestamp, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        vec![
            session.into(),
            seq.into(),
            entry.id.as_str().into(),
            entry.parent.as_deref().into(),
            kind.into(),
            entry.timestamp.as_str().into(),
            entry.to_json().to_string().into(),
        ],
    )
}

fn insert_submission(session: &str, run_id: &str, entry_id: &str) -> (&'static str, Vec<Value>) {
    (
        "INSERT INTO submissions (conversation_id, run_id, entry_id) VALUES (?1, ?2, ?3)",
        vec![session.into(), run_id.into(), entry_id.into()],
    )
}

fn parse(path: &Path, text: &str) -> Result<serde_json::Value, StoreError> {
    serde_json::from_str(text).map_err(|err| {
        StoreError::Corrupt(format!("{}: stored row is not JSON: {err}", path.display()))
    })
}

fn store_error(path: &Path, err: Error) -> StoreError {
    match err {
        Error::Locked(message) => StoreError::Conflict(format!(
            "{}: the session database is held by another process ({message})",
            path.display()
        )),
        Error::Sql(message) => StoreError::Corrupt(format!("{}: {message}", path.display())),
        Error::Io(message) => StoreError::Io(format!("{}: {message}", path.display())),
    }
}

impl SessionStore for Sqlite {
    fn header(&self) -> &Header {
        &self.header
    }

    fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn tip(&self) -> Option<&str> {
        self.tip.as_deref()
    }

    fn append(
        &mut self,
        parent: Option<&str>,
        kinds: Vec<Kind>,
    ) -> Result<Vec<String>, StoreError> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let connection = open(&self.path)?;
        // The tip check and the writes share one transaction: a writer that
        // moved the tip between our open and our BEGIN is still caught.
        connection
            .execute("BEGIN", &[])
            .map_err(|err| store_error(&self.path, err))?;
        let outcome = (|| {
            let rows = connection
                .query(
                    "SELECT tip FROM conversations WHERE id = ?1",
                    &[self.header.id.as_str().into()],
                )
                .map_err(|err| store_error(&self.path, err))?;
            let on_disk = rows
                .first()
                .and_then(|row| row[0].as_str())
                .map(str::to_string);
            if rows.is_empty() {
                return Err(StoreError::Io(format!(
                    "{}: session {} is gone",
                    self.path.display(),
                    self.header.id
                )));
            }
            if on_disk != self.tip {
                return Err(StoreError::Conflict(format!(
                    "{}: another writer appended to session {} (tip {} stored, {} expected); nothing was written",
                    self.path.display(),
                    self.header.id,
                    on_disk.as_deref().unwrap_or("header"),
                    self.tip.as_deref().unwrap_or("header"),
                )));
            }
            let timestamp = crate::util::rfc3339::now();
            let mut previous = parent.map(str::to_string);
            let mut entries = Vec::with_capacity(kinds.len());
            let mut seq = self.entries.len() as i64;
            for kind in kinds {
                seq += 1;
                let entry = Entry {
                    id: entry_id(),
                    parent: previous.clone(),
                    timestamp: timestamp.clone(),
                    kind,
                };
                let (sql, params) = insert_entry(&self.header.id, seq, &entry);
                connection
                    .execute(sql, &params)
                    .map_err(|err| store_error(&self.path, err))?;
                if let Kind::Turn(turn) = &entry.kind {
                    let (sql, params) = insert_submission(&self.header.id, &turn.run_id, &entry.id);
                    connection.execute(sql, &params).map_err(|err| match err {
                        Error::Sql(message)
                            if message.to_ascii_lowercase().contains("constraint") =>
                        {
                            StoreError::Conflict(format!(
                                "{}: run id {} is already recorded in session {}",
                                self.path.display(),
                                turn.run_id,
                                self.header.id
                            ))
                        }
                        other => store_error(&self.path, other),
                    })?;
                }
                previous = Some(entry.id.clone());
                entries.push(entry);
            }
            connection
                .execute(
                    "UPDATE conversations SET tip = ?1 WHERE id = ?2",
                    &[previous.as_deref().into(), self.header.id.as_str().into()],
                )
                .map_err(|err| store_error(&self.path, err))?;
            Ok((entries, previous))
        })();
        let (entries, tip) = match outcome {
            Ok(written) => written,
            Err(err) => {
                let _ = connection.execute("ROLLBACK", &[]);
                return Err(err);
            }
        };
        connection
            .execute("COMMIT", &[])
            .map_err(|err| store_error(&self.path, err))?;
        let ids = entries.iter().map(|entry| entry.id.clone()).collect();
        self.tip = tip;
        self.entries.extend(entries);
        Ok(ids)
    }

    fn backend(&self) -> &'static str {
        "sqlite"
    }

    fn location(&self) -> String {
        format!("{}#{}", self.path.display(), self.header.id)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Role, StoredMessage, Turn};
    use super::*;
    use serde_json::json;

    fn message(role: Role, text: &str) -> Kind {
        Kind::Message(StoredMessage {
            role,
            text: text.into(),
            tool: None,
        })
    }

    fn turn(run_id: &str, status: &str) -> Kind {
        Kind::Turn(Turn {
            run_id: run_id.into(),
            status: status.into(),
            exit_code: u8::from(status != "ok"),
            prompt_sha256: "00".into(),
            result: json!({}),
        })
    }

    #[test]
    fn sessions_append_reopen_branch_and_refuse_a_second_writer() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("sqlite");
        let path = dir.join("state/sessions.db");
        let mut store = Sqlite::create(&path, Header::new("/w", "g", "m")).unwrap();
        let id = store.header().id.clone();
        let first = store
            .append(
                None,
                vec![
                    message(Role::Operator, "q1"),
                    message(Role::Assistant, "a1"),
                    turn("r1", "ok"),
                ],
            )
            .unwrap();
        assert_eq!(store.tip(), Some(first[2].as_str()));
        let parent = store.active_parent();
        store
            .append(
                parent.as_deref(),
                vec![message(Role::Operator, "q2"), turn("r2", "failed")],
            )
            .unwrap();
        assert_eq!(store.active_parent().as_deref(), Some(first[2].as_str()));
        assert_eq!(
            store
                .active_messages(None)
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            ["q1", "a1"]
        );

        // Reopened from the database: the same tree, by id.
        let reopened = Sqlite::open(&path, &id).unwrap();
        assert_eq!(reopened.entries(), store.entries());
        assert_eq!(reopened.tip(), store.tip());
        assert_eq!(reopened.backend(), "sqlite");
        assert!(reopened.location().ends_with(&id));
        assert!(Sqlite::open(&path, "nope").is_err());

        // A second session in the same database is independent.
        let mut other = Sqlite::create(&path, Header::new("/elsewhere", "g", "m")).unwrap();
        other
            .append(None, vec![message(Role::Operator, "other")])
            .unwrap();
        let listed = Sqlite::list(&path).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|l| l.header.id == id
            && l.entries == 5
            && l.turns == 2
            && l.last_turn.as_ref().map(|t| t.status.as_str()) == Some("failed")));

        // Two handles on one session: the one that appends second loses.
        let mut stale = Sqlite::open(&path, &id).unwrap();
        store
            .append(parent.as_deref(), vec![message(Role::Operator, "q3")])
            .unwrap();
        let refused = stale
            .append(parent.as_deref(), vec![message(Role::Operator, "q3 again")])
            .unwrap_err();
        assert!(matches!(refused, StoreError::Conflict(_)), "{refused}");
        assert_eq!(Sqlite::open(&path, &id).unwrap().entries().len(), 6);

        // A run id is unique within a session, enforced by the database.
        let mut current = Sqlite::open(&path, &id).unwrap();
        let tip = current.tip().map(str::to_string);
        let duplicate = current
            .append(tip.as_deref(), vec![turn("r1", "ok")])
            .unwrap_err();
        assert!(matches!(duplicate, StoreError::Conflict(_)), "{duplicate}");
        assert_eq!(
            Sqlite::open(&path, &id).unwrap().entries().len(),
            6,
            "rolled back"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn import_adds_a_session_or_extends_its_own_tail_and_refuses_a_fork() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("sqlite-import");
        let path = dir.join("sessions.db");
        let file = dir.join("s.jsonl");
        let mut jsonl =
            super::super::jsonl::Jsonl::create(&file, Header::new("/w", "g", "m")).unwrap();
        jsonl
            .append(None, vec![message(Role::Operator, "q1"), turn("r1", "ok")])
            .unwrap();
        let header = jsonl.header().clone();
        assert_eq!(Sqlite::import(&path, &header, jsonl.entries()).unwrap(), 2);
        assert_eq!(
            Sqlite::import(&path, &header, jsonl.entries()).unwrap(),
            0,
            "idempotent"
        );
        let parent = jsonl.active_parent();
        jsonl
            .append(
                parent.as_deref(),
                vec![message(Role::Operator, "q2"), turn("r2", "ok")],
            )
            .unwrap();
        assert_eq!(
            Sqlite::import(&path, &header, jsonl.entries()).unwrap(),
            2,
            "the tail"
        );
        let stored = Sqlite::open(&path, &header.id).unwrap();
        assert_eq!(stored.entries(), jsonl.entries());
        assert_eq!(stored.tip(), jsonl.tip());
        assert!(stored.find_turn("r2").is_some());

        // A different history under the same id is a fork, not an update.
        let mut forked: Vec<Entry> = jsonl.entries().to_vec();
        forked[0].kind = message(Role::Operator, "rewritten");
        let refused = Sqlite::import(&path, &header, &forked).unwrap_err();
        assert!(matches!(refused, StoreError::Conflict(_)), "{refused}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
