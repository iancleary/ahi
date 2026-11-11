//! Recording a chat session as it happens.
//!
//! The screen owns one of these for its lifetime. The session is created
//! lazily, at the first turn, so a chat that is opened and closed again
//! leaves nothing behind; `/clear` starts a new one at the next turn. Every
//! step is appended when it happens: the operator's message before the
//! request, each assistant message with its calls before the tools run,
//! each tool result after, and a `turn` line that closes the branch. The
//! first failure to write is kept for the screen to show once; recording
//! then stops for the session rather than failing the conversation.

use std::path::Path;

use serde_json::json;

use super::sqlite::Sqlite;
use super::{Header, Kind, Role, SessionStore, StoredMessage, Turn};
use crate::chat::backend::{Selection, ToolCall};
use crate::chat::transcript::{NativeReplay, ToolPayload};
use crate::directories::Directories;

pub(crate) struct Recorder {
    enabled: bool,
    store: Option<Box<dyn SessionStore>>,
    /// Where the next entry hangs.
    parent: Option<String>,
    /// Changes announced before the session exists; written with its first
    /// entry unless the header already says so.
    pending: Vec<Kind>,
    /// Assistant text since the last tool round.
    segment: String,
    turn: Option<OpenTurn>,
    failed: Option<String>,
}

struct OpenTurn {
    run_id: String,
    prompt_sha256: String,
}

impl Recorder {
    /// Records into the state directory's database from the first turn on.
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            store: None,
            parent: None,
            pending: Vec::new(),
            segment: String::new(),
            turn: None,
            failed: None,
        }
    }

    /// Continues an existing session: new turns hang off its active parent.
    pub(crate) fn resumed(store: Box<dyn SessionStore>) -> Self {
        let parent = store.active_parent();
        Self {
            enabled: true,
            store: Some(store),
            parent,
            pending: Vec::new(),
            segment: String::new(),
            turn: None,
            failed: None,
        }
    }

    /// `/clear`: the next turn starts a new session.
    pub(crate) fn reset(&mut self) {
        self.store = None;
        self.parent = None;
        self.pending.clear();
        self.segment.clear();
        self.turn = None;
        self.failed = None;
    }

    /// The first write failure, once. The screen shows it as an alert.
    pub(crate) fn take_failure(&mut self) -> Option<String> {
        self.failed.take()
    }

    /// The operator sent `prompt` under `selection`: open the session when
    /// this is its first turn, then put the message on disk before the
    /// request goes out.
    pub(crate) fn begin_turn(
        &mut self,
        state: &Directories,
        cwd: &Path,
        selection: &Selection,
        prompt: &str,
    ) {
        if !self.enabled || self.failed.is_some() {
            return;
        }
        if self.store.is_none() {
            let header = Header::new(
                &cwd.display().to_string(),
                &selection.gateway,
                &selection.model,
            );
            match Sqlite::create(&crate::chat::exec::sessions_database(state), header) {
                Ok(store) => {
                    self.store = Some(Box::new(store));
                    self.parent = None;
                    // The header carries the model and the state file the
                    // thinking level; announcements before the first turn
                    // have nothing to add.
                    self.pending.clear();
                }
                Err(err) => {
                    self.failed = Some(err.to_string());
                    return;
                }
            }
        }
        self.segment.clear();
        self.turn = Some(OpenTurn {
            run_id: crate::chat::exec::generate_run_id(),
            prompt_sha256: crate::chat::exec::sha256_hex(prompt.as_bytes()),
        });
        self.record(Kind::Message(StoredMessage {
            role: Role::Operator,
            text: prompt.to_string(),
            tool: None,
        }));
    }

    pub(crate) fn text(&mut self, delta: &str) {
        if self.turn.is_some() {
            self.segment.push_str(delta);
        }
    }

    /// The assistant asked for tools: its message is on disk before any runs.
    pub(crate) fn calls(&mut self, calls: &[ToolCall], native: Option<&NativeReplay>) {
        if self.turn.is_none() {
            return;
        }
        let text = std::mem::take(&mut self.segment);
        self.record(Kind::Message(StoredMessage {
            role: Role::Assistant,
            text,
            tool: Some(ToolPayload::Calls {
                calls: calls.to_vec(),
                native: native.cloned(),
            }),
        }));
    }

    pub(crate) fn tool_result(
        &mut self,
        call_id: &str,
        name: &str,
        output: &str,
        content: &str,
        exit_code: i32,
    ) {
        if self.turn.is_none() {
            return;
        }
        self.record(Kind::Message(StoredMessage {
            role: Role::Tool { exit_code },
            text: output.to_string(),
            tool: Some(ToolPayload::Result {
                call_id: call_id.to_string(),
                name: name.to_string(),
                content: content.to_string(),
            }),
        }));
    }

    /// The turn ended. `error` is `None` for a completed turn.
    pub(crate) fn end_turn(&mut self, error: Option<(&str, &str)>) {
        let Some(open) = self.turn.take() else {
            return;
        };
        if error.is_none() && !self.segment.is_empty() {
            let text = std::mem::take(&mut self.segment);
            self.record(Kind::Message(StoredMessage {
                role: Role::Assistant,
                text,
                tool: None,
            }));
        }
        self.segment.clear();
        let (status, exit_code, result) = match error {
            None => ("ok", 0, json!({"status": "ok", "origin": "tui"})),
            Some((kind, message)) => (
                "failed",
                1,
                json!({"status": "failed", "origin": "tui", "error": {"kind": kind, "message": message}}),
            ),
        };
        self.record(Kind::Turn(Turn {
            run_id: open.run_id,
            status: status.to_string(),
            exit_code,
            prompt_sha256: open.prompt_sha256,
            result,
        }));
    }

    /// `/model`: recorded on the active path so a resume knows what to use.
    pub(crate) fn model_changed(&mut self, selection: &Selection) {
        self.announce(Kind::ModelChange {
            gateway: selection.gateway.clone(),
            model: selection.model.clone(),
        });
    }

    /// `/thinking`.
    pub(crate) fn thinking_changed(&mut self, level: &str) {
        self.announce(Kind::ThinkingChange {
            level: level.to_string(),
        });
    }

    /// A change outside a turn: written now when the session exists, else
    /// held for its first entry.
    fn announce(&mut self, kind: Kind) {
        if !self.enabled {
            return;
        }
        if self.store.is_some() {
            self.record(kind);
        } else {
            self.pending.push(kind);
        }
    }

    fn record(&mut self, kind: Kind) {
        if self.failed.is_some() {
            return;
        }
        let Some(store) = self.store.as_mut() else {
            return;
        };
        let mut kinds = std::mem::take(&mut self.pending);
        kinds.push(kind);
        match store.append(self.parent.as_deref(), kinds) {
            Ok(ids) => self.parent = ids.last().cloned(),
            Err(err) => self.failed = Some(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::session::Kind;
    use serde_json::Map;

    fn selection(model: &str) -> Selection {
        Selection {
            gateway: "local".into(),
            model: model.into(),
        }
    }

    #[test]
    fn a_chat_turn_is_recorded_step_by_step_and_a_clear_starts_a_new_session() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("recorder");
        let state = Directories::single_root(dir.join("state"));
        let mut recorder = Recorder::new(true);
        // Announcements before the first turn add nothing: the header and
        // the state file already say so.
        recorder.thinking_changed("low");
        recorder.begin_turn(&state, &dir, &selection("m"), "q1");
        recorder.text("read");
        recorder.text("ing");
        let call = ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            arguments: Map::new(),
        };
        recorder.calls(&[call], None);
        recorder.tool_result("c1", "read_file", "read_file a.rs\nfn a", "fn a", 0);
        recorder.text("done");
        recorder.end_turn(None);
        recorder.model_changed(&selection("m2"));
        recorder.begin_turn(&state, &dir, &selection("m2"), "q2");
        recorder.end_turn(Some(("transport", "down")));
        assert!(recorder.take_failure().is_none());

        let database = crate::chat::exec::sessions_database(&state);
        let listed = Sqlite::list(&database).unwrap();
        assert_eq!(listed.len(), 1);
        let store = Sqlite::open(&database, &listed[0].header.id).unwrap();
        let kinds: Vec<String> = store
            .entries()
            .iter()
            .map(|entry| match &entry.kind {
                Kind::Message(StoredMessage { role, .. }) => format!("{role:?}"),
                Kind::Turn(turn) => format!("turn:{}", turn.status),
                Kind::ModelChange { model, .. } => format!("model:{model}"),
                Kind::ThinkingChange { level } => format!("thinking:{level}"),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "Operator",
                "Assistant",
                "Tool { exit_code: 0 }",
                "Assistant",
                "turn:ok",
                "model:m2",
                "Operator",
                "turn:failed"
            ]
        );
        // The assistant message that asked for the tool carries the text
        // streamed before it, and the final one the text after.
        let Kind::Message(StoredMessage { text, tool, .. }) = &store.entries()[1].kind else {
            panic!()
        };
        assert_eq!(text, "reading");
        assert!(matches!(tool, Some(ToolPayload::Calls { .. })));
        let Kind::Message(StoredMessage { text, .. }) = &store.entries()[3].kind else {
            panic!()
        };
        assert_eq!(text, "done");
        let Kind::Turn(failed) = &store.entries()[7].kind else {
            panic!()
        };
        assert_eq!(failed.result["error"]["kind"], "transport");
        assert_eq!(failed.result["origin"], "tui");
        // The failed branch hangs off the success and is not the active path.
        assert_eq!(
            store.entries()[6].parent,
            store.entries()[5].id.clone().into()
        );
        assert_eq!(store.active_parent(), Some(store.entries()[4].id.clone()));

        recorder.reset();
        recorder.begin_turn(&state, &dir, &selection("m2"), "fresh");
        recorder.end_turn(None);
        assert_eq!(Sqlite::list(&database).unwrap().len(), 2, "a new session");

        let mut off = Recorder::new(false);
        off.begin_turn(&state, &dir, &selection("m"), "never");
        off.end_turn(None);
        assert_eq!(Sqlite::list(&database).unwrap().len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
