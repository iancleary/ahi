//! The store: a directory of session files, and one session open in memory.
//!
//! `<state>/sessions/<id>.jsonl`, owner-only. A `Session` holds the header
//! and every entry of one file; the `Store` creates, opens, holds, lists,
//! imports, and exports. A session is appended to only while held: `hold`
//! takes an advisory lock on the file (one writer per file across the
//! server, a standalone chat, and `import`), truncates a torn last line
//! the writer before it left behind, and refuses with `Conflict` when
//! another process holds it. A reader skips a torn last line.

use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::{valid_id, Entry, Header, Kind, Role, Settings, Status, StoreError, StoredMessage};
use crate::chat::transcript::ChatMessage;
use crate::directories::Directories;
use crate::harness::Protocol;

/// How long `hold` keeps trying a lock another description still has.
const LOCK_PATIENCE: std::time::Duration = std::time::Duration::from_millis(250);

pub(crate) struct Store {
    dir: PathBuf,
}

/// One row of `list`: the header and what the file holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub header: Header,
    pub entries: usize,
    pub turns: usize,
    /// The last `turn` entry's run id and status, if any turn closed.
    pub last_turn: Option<(String, Status)>,
}

impl Store {
    pub(crate) fn new(state: &Directories) -> Self {
        Self {
            dir: state.state.join("sessions"),
        }
    }

    /// Where the store lives, for messages.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.jsonl"))
    }

    /// A new session file with `header` as its first line.
    pub(crate) fn create(&self, header: Header) -> Result<Session, StoreError> {
        crate::directories::create_private_dir(&self.dir)
            .map_err(|err| io(&self.dir, "cannot create the session directory", err))?;
        let path = self.path(&header.id);
        let mut file = crate::directories::private_file()
            .read(true)
            .create_new(true)
            .open(&path)
            .map_err(|err| io(&path, "cannot create", err))?;
        if !ahi_unix::try_lock_exclusive(&file).map_err(|err| io(&path, "cannot lock", err))? {
            return Err(StoreError::Conflict(format!(
                "session {} is held by another process",
                header.id
            )));
        }
        let line = format!("{}\n", header.to_json());
        file.write_all(line.as_bytes())
            .map_err(|err| io(&path, "cannot write", err))?;
        Ok(Session {
            path,
            header,
            entries: Vec::new(),
            complete: line.len() as u64,
            writer: Some(file),
        })
    }

    /// The session with `id`, to read. It cannot be appended to.
    pub(crate) fn open(&self, id: &str) -> Result<Session, StoreError> {
        Session::read(&self.existing(id)?)
    }

    /// The session with `id`, to write: held under an advisory lock until
    /// it is dropped. `Conflict` when another process holds it. A torn
    /// last line is cut off so the first append starts a whole line.
    pub(crate) fn hold(&self, id: &str) -> Result<Session, StoreError> {
        Session::hold(&self.existing(id)?)
    }

    fn existing(&self, id: &str) -> Result<PathBuf, StoreError> {
        if !valid_id(id) {
            return Err(StoreError::NotFound(format!(
                "`{id}` is not a session id (a lower-case UUID)"
            )));
        }
        let path = self.path(id);
        if fs::symlink_metadata(&path).is_err() {
            return Err(StoreError::NotFound(format!(
                "no session {id} in {}",
                self.dir.display()
            )));
        }
        Ok(path)
    }

    /// Every session, newest first. A file this ahi cannot read is skipped
    /// with its problem, never a failure for the whole listing.
    pub(crate) fn list(&self) -> Result<(Vec<Summary>, Vec<String>), StoreError> {
        let mut rows = Vec::new();
        let mut skipped = Vec::new();
        let dir = match fs::read_dir(&self.dir) {
            Ok(dir) => dir,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok((rows, skipped)),
            Err(err) => return Err(io(&self.dir, "cannot read", err)),
        };
        for entry in dir {
            let path = entry
                .map_err(|err| io(&self.dir, "cannot read", err))?
                .path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            match Session::read(&path) {
                Ok(session) => rows.push(session.summary()),
                Err(err) => skipped.push(format!("{}: {err}", path.display())),
            }
        }
        rows.sort_by(|a, b| b.header.started.cmp(&a.header.started));
        Ok((rows, skipped))
    }

    /// Copy the session `id` to `to`, which must not exist.
    pub(crate) fn export(&self, id: &str, to: &Path) -> Result<Session, StoreError> {
        let session = self.open(id)?;
        if fs::symlink_metadata(to).is_ok() {
            return Err(StoreError::Conflict(format!(
                "{} already exists; remove it or name another path",
                to.display()
            )));
        }
        let bytes = fs::read(&session.path).map_err(|err| io(&session.path, "cannot read", err))?;
        let mut file = crate::directories::private_file()
            .create_new(true)
            .open(to)
            .map_err(|err| io(to, "cannot create", err))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|err| io(to, "cannot write", err))?;
        Ok(session)
    }

    /// Bring a session file written elsewhere into the store. A new id is
    /// copied whole; a known id is extended by the file's new tail when the
    /// stored session is a prefix of it, and refused when the two differ.
    /// Returns the session and how many entries were added.
    pub(crate) fn import(&self, from: &Path) -> Result<(Session, usize), StoreError> {
        let incoming = Session::read(from)?;
        let path = self.path(&incoming.header.id);
        if fs::symlink_metadata(&path).is_err() {
            let session = self.create(incoming.header.clone())?;
            let added = incoming.entries.len();
            let mut session = session;
            session.write_lines(&incoming.entries)?;
            session.entries = incoming.entries;
            return Ok((session, added));
        }
        let mut stored = Session::hold(&path)?;
        if stored.header != incoming.header
            || incoming.entries.len() < stored.entries.len()
            || incoming.entries[..stored.entries.len()] != stored.entries[..]
        {
            return Err(StoreError::Conflict(format!(
                "session {} is already in the store with a different history; nothing was written",
                incoming.header.id
            )));
        }
        let tail = &incoming.entries[stored.entries.len()..];
        stored.write_lines(tail)?;
        let added = tail.len();
        stored.entries.extend_from_slice(tail);
        Ok((stored, added))
    }
}

/// One session, read whole and, while held, appended to in place.
pub(crate) struct Session {
    path: PathBuf,
    header: Header,
    entries: Vec<Entry>,
    /// Bytes of whole lines: where the next line goes.
    complete: u64,
    /// The locked file, when this session is held for writing.
    writer: Option<File>,
}

impl Session {
    /// Read any session file. A final line without its newline is a write
    /// that was cut short and is skipped; the lines before it are whole.
    pub(crate) fn read(path: &Path) -> Result<Self, StoreError> {
        let text = fs::read_to_string(path).map_err(|err| io(path, "cannot read", err))?;
        let complete = match text.rfind('\n') {
            Some(end) => &text[..=end],
            None => "",
        };
        let mut lines = complete.lines().filter(|line| !line.trim().is_empty());
        let first = lines.next().ok_or_else(|| {
            StoreError::Corrupt(format!("{}: no complete header line", path.display()))
        })?;
        let header = Header::from_json(&parse_line(path, first)?)?;
        let mut entries = Vec::new();
        for line in lines {
            let entry = Entry::from_json(&parse_line(path, line)?)?;
            let expected = entries.len() as u64 + 1;
            if entry.seq != expected {
                return Err(StoreError::Corrupt(format!(
                    "{}: entry {} is out of order (expected seq {expected})",
                    path.display(),
                    entry.seq
                )));
            }
            if entry
                .parent
                .is_some_and(|parent| parent == 0 || parent >= entry.seq)
            {
                return Err(StoreError::Corrupt(format!(
                    "{}: entry {} names a parent that is not before it",
                    path.display(),
                    entry.seq
                )));
            }
            entries.push(entry);
        }
        Ok(Self {
            path: path.to_path_buf(),
            header,
            entries,
            complete: complete.len() as u64,
            writer: None,
        })
    }

    /// Read `path` for writing: lock first, then read what the lock
    /// guarantees is settled, then cut a torn last line.
    fn hold(path: &Path) -> Result<Self, StoreError> {
        let file = crate::directories::private_file()
            .read(true)
            .open(path)
            .map_err(|err| io(path, "cannot open", err))?;
        // A process forking at this moment holds a copy of every descriptor
        // until it execs, the lock's among them, so a refusal is final only
        // when it lasts.
        let deadline = std::time::Instant::now() + LOCK_PATIENCE;
        loop {
            if ahi_unix::try_lock_exclusive(&file).map_err(|err| io(path, "cannot lock", err))? {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Err(StoreError::Conflict(format!(
                    "{} is held by another process (a server or a chat)",
                    path.display()
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut session = Self::read(path)?;
        file.set_len(session.complete)
            .map_err(|err| io(path, "cannot cut the torn line", err))?;
        session.writer = Some(file);
        Ok(session)
    }

    pub(crate) fn header(&self) -> &Header {
        &self.header
    }

    pub(crate) fn id(&self) -> &str {
        &self.header.id
    }

    /// Every entry, in write order.
    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Entries after `seq` in write order; `0` is everything.
    #[allow(dead_code)] // the server's event cursor
    pub(crate) fn entries_after(&self, seq: u64) -> &[Entry] {
        let start = (seq as usize).min(self.entries.len());
        &self.entries[start..]
    }

    /// The last entry written.
    #[allow(dead_code)] // the server's event cursor
    pub(crate) fn tip(&self) -> Option<u64> {
        self.entries.last().map(|entry| entry.seq)
    }

    /// Append `kinds` as one write, each under the previous and the first
    /// under `parent`. Returns the new entries.
    pub(crate) fn append(
        &mut self,
        parent: Option<u64>,
        kinds: Vec<Kind>,
    ) -> Result<&[Entry], StoreError> {
        let first = self.entries.len();
        if kinds.is_empty() {
            return Ok(&self.entries[first..]);
        }
        let timestamp = crate::util::rfc3339::now();
        let mut previous = parent;
        let mut new = Vec::with_capacity(kinds.len());
        for (offset, kind) in kinds.into_iter().enumerate() {
            let seq = (first + offset) as u64 + 1;
            new.push(Entry {
                seq,
                parent: previous,
                timestamp: timestamp.clone(),
                kind,
            });
            previous = Some(seq);
        }
        self.write_lines(&new)?;
        self.entries.extend(new);
        Ok(&self.entries[first..])
    }

    fn write_lines(&mut self, entries: &[Entry]) -> Result<(), StoreError> {
        let mut bytes = Vec::new();
        for entry in entries {
            bytes.extend_from_slice(entry.to_json().to_string().as_bytes());
            bytes.push(b'\n');
        }
        let Some(file) = self.writer.as_mut() else {
            return Err(StoreError::Conflict(format!(
                "{} was opened to read, not to write",
                self.path.display()
            )));
        };
        // The lock makes this writer the only one; a length that moved
        // anyway is another process writing without it.
        let length = file
            .metadata()
            .map_err(|err| io(&self.path, "cannot stat", err))?
            .len();
        if length != self.complete {
            return Err(StoreError::Conflict(format!(
                "{} changed under its writer; nothing was written",
                self.path.display()
            )));
        }
        file.seek(SeekFrom::End(0))
            .map_err(|err| io(&self.path, "cannot seek", err))?;
        file.write_all(&bytes)
            .map_err(|err| io(&self.path, "cannot write", err))?;
        self.complete += bytes.len() as u64;
        Ok(())
    }

    /// The entry a new turn hangs off: the last `turn` that succeeded, or
    /// the root when none has. Other branches are left where they are.
    pub(crate) fn active_parent(&self) -> Option<u64> {
        self.entries
            .iter()
            .rev()
            .find(|entry| matches!(&entry.kind, Kind::Turn(turn) if turn.status == Status::Ok))
            .map(|entry| entry.seq)
    }

    /// Entries from `from` back to the root, in conversation order.
    pub(crate) fn path_to_root(&self, from: Option<u64>) -> Vec<&Entry> {
        let mut path = Vec::new();
        let mut next = from;
        while let Some(seq) = next {
            let Some(entry) = self.entries.get(seq as usize - 1) else {
                break;
            };
            path.push(entry);
            next = entry.parent;
        }
        path.reverse();
        path
    }

    /// The conversation a new turn continues: every message on the active
    /// path, as transcript messages for `protocol`.
    pub(crate) fn active_messages(&self, protocol: Option<Protocol>) -> Vec<ChatMessage> {
        self.path_to_root(self.active_parent())
            .into_iter()
            .filter_map(|entry| match &entry.kind {
                Kind::Message(message) => Some(message.to_chat(protocol)),
                _ => None,
            })
            .collect()
    }

    /// The model and thinking level in force: the header's, then every
    /// change in the order it was made. A change is a setting, not a step
    /// of the conversation, so one announced before a turn that then failed
    /// still holds; the failed branch loses its messages, not the operator's
    /// choice.
    pub(crate) fn settings(&self) -> Settings {
        let mut settings = Settings {
            gateway: self.header.gateway.clone(),
            model: self.header.model.clone(),
            thinking: self.header.thinking.clone(),
        };
        for entry in &self.entries {
            match &entry.kind {
                Kind::ModelChange { gateway, model } => {
                    settings.gateway = gateway.clone();
                    settings.model = model.clone();
                }
                Kind::ThinkingChange { level } => settings.thinking = level.clone(),
                _ => {}
            }
        }
        settings
    }

    /// The `turn` entry written for `run_id`, if this session has one.
    #[allow(dead_code)] // run_id replay in `ahi exec` and the server
    pub(crate) fn find_turn(&self, run_id: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| matches!(&entry.kind, Kind::Turn(turn) if turn.run_id == run_id))
    }

    /// The last assistant text before the turn at `seq` on its branch: the
    /// answer that turn produced.
    #[allow(dead_code)] // run_id replay in `ahi exec` and the server
    pub(crate) fn answer_of(&self, seq: u64) -> Option<String> {
        let turn = self.entries.get(seq as usize - 1)?;
        self.path_to_root(turn.parent)
            .into_iter()
            .rev()
            .find_map(|entry| match &entry.kind {
                Kind::Message(StoredMessage {
                    role: Role::Assistant,
                    text,
                    ..
                }) => Some(text.clone()),
                _ => None,
            })
    }

    pub(crate) fn summary(&self) -> Summary {
        let turns: Vec<&super::Turn> = self
            .entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                Kind::Turn(turn) => Some(turn),
                _ => None,
            })
            .collect();
        Summary {
            header: self.header.clone(),
            entries: self.entries.len(),
            turns: turns.len(),
            last_turn: turns.last().map(|turn| (turn.run_id.clone(), turn.status)),
        }
    }
}

fn parse_line(path: &Path, line: &str) -> Result<serde_json::Value, StoreError> {
    serde_json::from_str(line)
        .map_err(|err| StoreError::Corrupt(format!("{}: not JSON: {err}", path.display())))
}

fn io(path: &Path, what: &str, err: std::io::Error) -> StoreError {
    StoreError::Io(format!("{what} {}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message(role: Role, text: &str) -> Kind {
        Kind::Message(StoredMessage {
            role,
            text: text.into(),
            tool: None,
        })
    }

    fn turn(run_id: &str, status: Status) -> Kind {
        Kind::Turn(super::super::Turn {
            run_id: run_id.into(),
            status,
            prompt_sha256: String::new(),
            result: json!({}),
        })
    }

    #[test]
    fn a_session_is_a_tree_whose_active_path_skips_failed_branches() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("session-store");
        let store = Store::new(&Directories::single_root(dir.join("state")));
        let mut session = store
            .create(Header::new("/w", "local", "m1", "off"))
            .unwrap();
        let first = session
            .append(
                None,
                vec![
                    message(Role::Operator, "q1"),
                    message(Role::Assistant, "a1"),
                    turn("r1", Status::Ok),
                ],
            )
            .unwrap()
            .last()
            .unwrap()
            .seq;
        assert_eq!(first, 3);
        session
            .append(
                Some(3),
                vec![
                    Kind::ModelChange {
                        gateway: "local".into(),
                        model: "m2".into(),
                    },
                    message(Role::Operator, "q2"),
                    turn("r2", Status::Failed),
                ],
            )
            .unwrap();
        session
            .append(
                Some(3),
                vec![
                    Kind::ThinkingChange {
                        level: "high".into(),
                    },
                    message(Role::Operator, "q3"),
                    message(Role::Assistant, "a3"),
                    turn("r3", Status::Ok),
                ],
            )
            .unwrap();
        let id = session.id().to_string();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.dir().join(format!("{id}.jsonl")))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        drop(session);
        let reopened = store.open(&id).unwrap();
        assert_eq!(reopened.entries().len(), 10);
        assert_eq!(reopened.tip(), Some(10));
        assert_eq!(reopened.active_parent(), Some(10));
        let texts: Vec<String> = reopened
            .active_messages(None)
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert_eq!(
            texts,
            ["q1", "a1", "q3", "a3"],
            "the failed branch is skipped"
        );
        assert_eq!(
            reopened.settings(),
            Settings {
                gateway: "local".into(),
                model: "m2".into(),
                thinking: "high".into()
            },
            "settings follow every change in order, on whichever branch it was made"
        );
        assert_eq!(reopened.find_turn("r2").map(|e| e.seq), Some(6));
        assert_eq!(reopened.answer_of(10), Some("a3".into()));
        assert_eq!(reopened.answer_of(6), Some("a1".into()));
        assert_eq!(reopened.entries_after(8).len(), 2);
        assert_eq!(reopened.entries_after(99).len(), 0);
        let summary = reopened.summary();
        assert_eq!((summary.entries, summary.turns), (10, 3));
        assert_eq!(summary.last_turn, Some(("r3".into(), Status::Ok)));

        // A torn last line is skipped, not an error; the lines before it hold.
        let path = store.dir().join(format!("{id}.jsonl"));
        let mut torn = fs::read_to_string(&path).unwrap();
        torn.push_str("{\"seq\":11,\"type\":\"mess");
        fs::write(&path, &torn).unwrap();
        assert_eq!(store.open(&id).unwrap().entries().len(), 10);
        // Holding the session cuts the torn line, so an append starts whole.
        let mut held = store.hold(&id).unwrap();
        // No advisory locks off Unix: the stub says every holder holds it.
        #[cfg(unix)]
        assert!(matches!(store.hold(&id), Err(StoreError::Conflict(_))));
        held.append(Some(10), vec![message(Role::Operator, "q4")])
            .unwrap();
        drop(held);
        let mended = store.open(&id).unwrap();
        assert_eq!(mended.entries().len(), 11);
        assert_eq!(mended.entries()[10].seq, 11);
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .lines()
                .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()),
            "every line is whole"
        );
        // A session opened to read cannot be written.
        assert!(matches!(
            store
                .open(&id)
                .unwrap()
                .append(None, vec![message(Role::Operator, "no")]),
            Err(StoreError::Conflict(_))
        ));
        // A whole bad line is corruption.
        let mut bad = fs::read_to_string(&path).unwrap();
        bad.push_str("{\"seq\":12}\n");
        fs::write(&path, bad).unwrap();
        assert!(matches!(store.open(&id), Err(StoreError::Corrupt(_))));

        assert!(matches!(
            store.open("../../etc/passwd"),
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store.open(&super::super::uuid()),
            Err(StoreError::NotFound(_))
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn list_export_and_import_move_whole_files() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("session-files");
        let store = Store::new(&Directories::single_root(dir.join("state")));
        assert_eq!(
            store.list().unwrap(),
            (Vec::new(), Vec::new()),
            "no directory yet"
        );
        let mut a = store.create(Header::new("/a", "g", "m", "off")).unwrap();
        a.append(
            None,
            vec![message(Role::Operator, "q"), turn("r1", Status::Ok)],
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let b = store.create(Header::new("/b", "g", "m", "off")).unwrap();
        fs::write(store.dir().join("notes.txt"), "ignored").unwrap();
        fs::write(store.dir().join("broken.jsonl"), "{}\n").unwrap();
        let (rows, skipped) = store.list().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].header.id, b.id(), "newest first");
        assert_eq!(rows[1].last_turn, Some(("r1".into(), Status::Ok)));
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].contains("broken.jsonl"));

        let out = dir.join("a.jsonl");
        store.export(a.id(), &out).unwrap();
        assert!(matches!(
            store.export(a.id(), &out),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(
            fs::read(&out).unwrap(),
            fs::read(store.dir().join(format!("{}.jsonl", a.id()))).unwrap()
        );

        // Import into another store: whole file, then an extended copy, then a fork.
        let other = Store::new(&Directories::single_root(dir.join("other")));
        let (imported, added) = other.import(&out).unwrap();
        assert_eq!((imported.id(), added), (a.id(), 2));
        // While a process holds the session, import refuses it (Unix locks).
        #[cfg(unix)]
        assert!(matches!(other.import(&out), Err(StoreError::Conflict(_))));
        drop(imported);
        let (again, added) = other.import(&out).unwrap();
        assert_eq!(added, 0, "nothing new");
        drop(again);
        a.append(
            Some(2),
            vec![message(Role::Operator, "q2"), turn("r2", Status::Ok)],
        )
        .unwrap();
        let longer = dir.join("a2.jsonl");
        store.export(a.id(), &longer).unwrap();
        let (extended, added) = other.import(&longer).unwrap();
        assert_eq!((extended.entries().len(), added), (4, 2));
        drop(extended);
        let mut forked = fs::read_to_string(&out).unwrap();
        forked.push_str(
            &Entry {
                seq: 3,
                parent: Some(2),
                timestamp: String::new(),
                kind: message(Role::Operator, "elsewhere"),
            }
            .to_json()
            .to_string(),
        );
        forked.push('\n');
        let fork = dir.join("fork.jsonl");
        fs::write(&fork, forked).unwrap();
        assert!(matches!(other.import(&fork), Err(StoreError::Conflict(_))));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_skill_load_round_trips_and_an_unknown_entry_keeps_its_place() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("session-skill");
        let store = Store::new(&Directories::single_root(dir.join("state")));
        let mut session = store
            .create(Header::new("/w", "local", "m", "off"))
            .unwrap();
        session
            .append(
                None,
                vec![
                    message(Role::Operator, "q1"),
                    Kind::SkillLoad {
                        name: "rust-review".into(),
                        root: "repo".into(),
                        file: None,
                        sha256: "ab".repeat(32),
                        bytes: 21,
                    },
                    Kind::SkillLoad {
                        name: "rust-review".into(),
                        root: "repo".into(),
                        file: Some("checklist.md".into()),
                        sha256: "cd".repeat(32),
                        bytes: 9,
                    },
                    turn("r1", Status::Ok),
                ],
            )
            .unwrap();
        let id = session.id().to_string();
        let path = store.dir().join(format!("{id}.jsonl"));
        drop(session);
        // A newer ahi wrote an entry type this one does not know.
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str(
            "{\"seq\":5,\"parent\":4,\"time\":\"t\",\"type\":\"telemetry\",\"watts\":3}\n",
        );
        fs::write(&path, text).unwrap();
        let reopened = store.open(&id).unwrap();
        assert_eq!(reopened.entries().len(), 5);
        assert_eq!(
            reopened.entries()[1].kind,
            Kind::SkillLoad {
                name: "rust-review".into(),
                root: "repo".into(),
                file: None,
                sha256: "ab".repeat(32),
                bytes: 21,
            }
        );
        let Kind::Unknown(kept) = &reopened.entries()[4].kind else {
            panic!("the unknown entry is kept, not refused");
        };
        assert_eq!(kept["type"], "telemetry");
        assert_eq!(reopened.entries()[4].seq, 5);
        assert_eq!(
            reopened.entries()[4].to_json()["watts"],
            3,
            "written back whole"
        );
        assert_eq!(
            reopened.active_parent(),
            Some(4),
            "the turn is still the tip"
        );
        // It says nothing to the model, and the next turn hangs off it.
        assert_eq!(reopened.active_messages(None).len(), 1);
        let mut held = store.hold(&id).unwrap();
        held.append(Some(5), vec![message(Role::Operator, "q2")])
            .unwrap();
        assert_eq!(held.entries()[5].seq, 6);
        drop(held);
        let out = dir.join("out.jsonl");
        store.export(&id, &out).unwrap();
        assert!(fs::read_to_string(&out).unwrap().contains("\"watts\":3"));
        fs::remove_dir_all(dir).unwrap();
    }
}
