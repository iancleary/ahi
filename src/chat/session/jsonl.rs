//! The JSONL backend: one header line, then one entry per line, never
//! rewritten. This is the interchange format: a coordinator ferries it
//! between machines, `rsync` syncs it, and a human reads it with `less`.
//!
//! Concurrency is the tip check, not a lock file. The store remembers the
//! id of the last line it saw; before appending it reads the file's last
//! line again and refuses if someone else moved it. Two writers on one file
//! cannot both win, and a reader never waits.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{entry_id, Entry, Header, Kind, SessionStore, StoreError};

pub struct Jsonl {
    path: PathBuf,
    header: Header,
    entries: Vec<Entry>,
    tip: Option<String>,
}

impl Jsonl {
    /// Create `path` with `header` as its first line. The file must not
    /// exist; it is created owner-only, since prompts and answers live in it.
    pub fn create(path: &Path, header: Header) -> Result<Self, StoreError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            crate::directories::create_private_dir(parent)
                .map_err(|err| io(path, "cannot create the session directory", err))?;
        }
        let mut file = crate::directories::private_file()
            .create_new(true)
            .open(path)
            .map_err(|err| io(path, "cannot create", err))?;
        let line = format!("{}\n", header.to_json());
        file.write_all(line.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|err| io(path, "cannot write", err))?;
        Ok(Self {
            path: path.to_path_buf(),
            header,
            entries: Vec::new(),
            tip: None,
        })
    }

    /// Open an existing file, reading every line. A final line without its
    /// newline is a write that was cut short; it is ignored, since nothing
    /// after a torn line can be trusted and the lines before it are whole.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let text = fs::read_to_string(path).map_err(|err| io(path, "cannot read", err))?;
        let (header, entries) = parse(path, &text)?;
        let tip = entries.last().map(|entry| entry.id.clone());
        Ok(Self {
            path: path.to_path_buf(),
            header,
            entries,
            tip,
        })
    }

    /// `open` when the file exists, `create` otherwise.
    pub fn open_or_create(
        path: &Path,
        header: impl FnOnce() -> Header,
    ) -> Result<Self, StoreError> {
        if fs::symlink_metadata(path).is_ok() {
            Self::open(path)
        } else {
            Self::create(path, header())
        }
    }

    /// The id of the last whole line on disk right now.
    fn tip_on_disk(&self) -> Result<Option<String>, StoreError> {
        let text =
            fs::read_to_string(&self.path).map_err(|err| io(&self.path, "cannot read", err))?;
        let (_, entries) = parse(&self.path, &text)?;
        Ok(entries.last().map(|entry| entry.id.clone()))
    }
}

/// Lines to header and entries. The header must be first and parse; a torn
/// final line is skipped; any other unparsable line is corruption.
fn parse(path: &Path, text: &str) -> Result<(Header, Vec<Entry>), StoreError> {
    let complete = match text.rfind('\n') {
        Some(end) => &text[..=end],
        None if text.is_empty() => {
            return Err(StoreError::Corrupt(format!(
                "{}: empty session file",
                path.display()
            )))
        }
        None => "",
    };
    let mut lines = complete.lines().filter(|line| !line.trim().is_empty());
    let first = lines.next().ok_or_else(|| {
        StoreError::Corrupt(format!("{}: no complete header line", path.display()))
    })?;
    let header = Header::from_json(&parse_line(path, first)?)?;
    let entries = lines
        .map(|line| Entry::from_json(&parse_line(path, line)?))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((header, entries))
}

fn parse_line(path: &Path, line: &str) -> Result<serde_json::Value, StoreError> {
    serde_json::from_str(line)
        .map_err(|err| StoreError::Corrupt(format!("{}: not JSON: {err}", path.display())))
}

fn io(path: &Path, what: &str, err: std::io::Error) -> StoreError {
    StoreError::Io(format!("{what} {}: {err}", path.display()))
}

impl SessionStore for Jsonl {
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
        let on_disk = self.tip_on_disk()?;
        if on_disk != self.tip {
            return Err(StoreError::Conflict(format!(
                "{}: another writer appended to this session (tip {} on disk, {} expected); nothing was written",
                self.path.display(),
                on_disk.as_deref().unwrap_or("header"),
                self.tip.as_deref().unwrap_or("header"),
            )));
        }
        let timestamp = crate::util::rfc3339::now();
        let mut previous = parent.map(str::to_string);
        let mut entries = Vec::with_capacity(kinds.len());
        let mut bytes = Vec::new();
        for kind in kinds {
            let entry = Entry {
                id: entry_id(),
                parent: previous.clone(),
                timestamp: timestamp.clone(),
                kind,
            };
            bytes.extend_from_slice(entry.to_json().to_string().as_bytes());
            bytes.push(b'\n');
            previous = Some(entry.id.clone());
            entries.push(entry);
        }
        let mut file = crate::directories::private_file()
            .append(true)
            .open(&self.path)
            .map_err(|err| io(&self.path, "cannot open for append", err))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|err| io(&self.path, "cannot append to", err))?;
        let ids = entries.iter().map(|entry| entry.id.clone()).collect();
        self.tip = previous;
        self.entries.extend(entries);
        Ok(ids)
    }

    fn backend(&self) -> &'static str {
        "jsonl"
    }

    fn location(&self) -> String {
        self.path.display().to_string()
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
    fn a_session_is_appended_reopened_and_branches_after_a_failed_turn() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("jsonl");
        let path = dir.join("sessions/slug/s.jsonl");
        let mut store = Jsonl::create(&path, Header::new("/w", "g", "m")).unwrap();
        assert_eq!(store.tip(), None);
        assert_eq!(store.active_parent(), None);
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
        assert_eq!(first.len(), 3);
        assert_eq!(store.tip(), Some(first[2].as_str()));
        assert_eq!(store.active_parent().as_deref(), Some(first[2].as_str()));

        // A failed turn hangs off the success and stays a dead branch.
        let parent = store.active_parent();
        let failed = store
            .append(
                parent.as_deref(),
                vec![message(Role::Operator, "q2"), turn("r2", "failed")],
            )
            .unwrap();
        assert_eq!(store.tip(), Some(failed[1].as_str()));
        assert_eq!(
            store.active_parent().as_deref(),
            Some(first[2].as_str()),
            "the next turn continues from the last success"
        );
        let messages = store.active_messages(None);
        assert_eq!(
            messages.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            ["q1", "a1"]
        );
        assert_eq!(store.answer_of(&first[2]).as_deref(), Some("a1"));
        assert!(store.find_turn("r2").is_some());
        assert!(store.find_turn("r9").is_none());

        // Reopen: same tree, same tip, owner-only file.
        let reopened = Jsonl::open(&path).unwrap();
        assert_eq!(reopened.entries(), store.entries());
        assert_eq!(reopened.tip(), store.tip());
        assert_eq!(reopened.header(), store.header());
        assert_eq!(reopened.backend(), "jsonl");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(
            Jsonl::create(&path, Header::new("/w", "g", "m")).is_err(),
            "never clobbers"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_second_writer_is_refused_and_a_torn_tail_is_ignored() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("jsonl-tip");
        let path = dir.join("s.jsonl");
        let mut first = Jsonl::create(&path, Header::new("/w", "g", "m")).unwrap();
        let mut second = Jsonl::open(&path).unwrap();
        first
            .append(None, vec![message(Role::Operator, "from first")])
            .unwrap();
        let refused = second
            .append(None, vec![message(Role::Operator, "from second")])
            .unwrap_err();
        assert!(matches!(refused, StoreError::Conflict(_)), "{refused}");
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2, "nothing from the loser landed");

        // A crash mid-line leaves a tail without a newline: not an entry.
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"id\":\"torn\",\"type\":\"mess")
            .unwrap();
        let reopened = Jsonl::open(&path).unwrap();
        assert_eq!(reopened.entries().len(), 1);
        assert_eq!(reopened.tip(), first.tip());

        // But a whole line that is not an entry is corruption, and a newer
        // schema is refused before anything is read.
        fs::write(
            dir.join("bad.jsonl"),
            format!(
                "{}\n{{\"nope\":1}}\n",
                Header::new("/w", "g", "m").to_json()
            ),
        )
        .unwrap();
        assert!(matches!(
            Jsonl::open(&dir.join("bad.jsonl")),
            Err(StoreError::Corrupt(_))
        ));
        let mut newer = Header::new("/w", "g", "m").to_json();
        newer["schema"] = json!(99);
        fs::write(dir.join("newer.jsonl"), format!("{newer}\n")).unwrap();
        assert!(matches!(
            Jsonl::open(&dir.join("newer.jsonl")),
            Err(StoreError::Schema(_))
        ));
        assert!(matches!(
            Jsonl::open(&dir.join("missing.jsonl")),
            Err(StoreError::Io(_))
        ));
        fs::remove_dir_all(dir).unwrap();
    }
}
