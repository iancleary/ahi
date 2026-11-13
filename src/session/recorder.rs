//! Recording a conversation as it happens.
//!
//! The screen owns one of these for its lifetime. The session is created at
//! the first turn, so a chat that is opened and closed again leaves nothing
//! behind; `/clear` starts a new one at the next turn. Every step is
//! appended when it happens: the operator's message before the request,
//! each assistant message with its calls before the tools run, each tool
//! result after, and a `turn` line that closes the branch, also when the
//! turn is cancelled. The first failure to write is kept for the screen to
//! show once; recording then stops for the session rather than failing the
//! conversation.

use std::path::Path;

use serde_json::json;

use super::{Header, Kind, Role, Session, Status, Store, StoredMessage, Turn};
use crate::chat::backend::{Selection, ToolCall};
use crate::chat::transcript::{NativeReplay, ToolPayload};

pub(crate) struct Recorder {
    enabled: bool,
    /// Named in every `turn` result: `tui`, `exec`, or `serve`.
    origin: &'static str,
    session: Option<Session>,
    /// Where the next entry hangs.
    parent: Option<u64>,
    /// Changes announced before the session exists; written with its first
    /// entry unless the header already says so.
    pending: Vec<Kind>,
    /// Assistant text since the last tool round.
    segment: String,
    turn: Option<OpenTurn>,
    failed: Option<String>,
    /// Entries written since `take_fresh`, for a fan-out to followers.
    fresh: Vec<super::Entry>,
}

struct OpenTurn {
    run_id: String,
    prompt_sha256: String,
}

impl Recorder {
    /// Records into the store from the first turn on; `enabled = false` is
    /// `--no-history`.
    pub(crate) fn new(enabled: bool, origin: &'static str) -> Self {
        Self {
            enabled,
            origin,
            session: None,
            parent: None,
            pending: Vec::new(),
            segment: String::new(),
            turn: None,
            failed: None,
            fresh: Vec::new(),
        }
    }

    /// Continues an existing session: new turns hang off its active parent.
    pub(crate) fn resumed(session: Session, origin: &'static str) -> Self {
        let parent = session.active_parent();
        Self {
            enabled: true,
            origin,
            session: Some(session),
            parent,
            pending: Vec::new(),
            segment: String::new(),
            turn: None,
            failed: None,
            fresh: Vec::new(),
        }
    }

    /// The session being written, once it exists.
    #[allow(dead_code)] // the chat screen will show it
    pub(crate) fn session_id(&self) -> Option<&str> {
        self.session.as_ref().map(Session::id)
    }

    /// The session being written, for readers that share the recorder.
    pub(crate) fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    /// Entries written since the last call, in write order.
    pub(crate) fn take_fresh(&mut self) -> Vec<super::Entry> {
        std::mem::take(&mut self.fresh)
    }

    /// `/clear`: the next turn starts a new session.
    pub(crate) fn reset(&mut self) {
        self.session = None;
        self.parent = None;
        self.pending.clear();
        self.segment.clear();
        self.turn = None;
        self.failed = None;
        self.fresh.clear();
    }

    /// The first write failure, once. The screen shows it as an alert.
    pub(crate) fn take_failure(&mut self) -> Option<String> {
        self.failed.take()
    }

    /// The operator sent `prompt`: open the session when this is its first
    /// turn, then put the message on disk before the request goes out.
    pub(crate) fn begin_turn(
        &mut self,
        store: &Store,
        cwd: &Path,
        selection: &Selection,
        thinking: &str,
        prompt: &str,
    ) {
        if !self.enabled || self.failed.is_some() {
            return;
        }
        if self.session.is_none() {
            let header = Header::new(
                &cwd.display().to_string(),
                &selection.gateway,
                &selection.model,
                thinking,
            );
            match store.create(header) {
                Ok(session) => {
                    self.session = Some(session);
                    self.parent = None;
                    // The header carries the model and thinking level;
                    // announcements before the first turn have nothing to add.
                    self.pending.clear();
                }
                Err(err) => {
                    self.failed = Some(err.to_string());
                    return;
                }
            }
        }
        self.begin_turn_as(super::run_id(), prompt);
    }

    /// Open a turn with a caller's run id on a session that already exists.
    /// Nothing is written when there is no session or recording stopped.
    pub(crate) fn begin_turn_as(&mut self, run_id: String, prompt: &str) {
        if !self.enabled || self.failed.is_some() || self.session.is_none() {
            return;
        }
        self.segment.clear();
        self.turn = Some(OpenTurn {
            run_id,
            prompt_sha256: super::sha256_hex(prompt.as_bytes()),
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

    /// Whether the session has reached its size budget, so the next turn is
    /// refused before anything is sent.
    pub(crate) fn over_budget(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.over_budget())
    }

    /// The model read a skill; written after the tool result it accompanies.
    pub(crate) fn skill_load(&mut self, record: &crate::chat::backend::ToolRecord) {
        if self.turn.is_none() {
            return;
        }
        if let crate::chat::backend::ToolRecord::Skill {
            name,
            root,
            file,
            outcome,
            sha256,
            bytes,
        } = record
        {
            if outcome != "ok" {
                return;
            }
            self.record(Kind::SkillLoad {
                name: name.clone(),
                root: root.clone(),
                file: file.clone(),
                sha256: sha256.clone(),
                bytes: *bytes,
            });
        }
    }

    /// The turn completed.
    pub(crate) fn end_turn(&mut self) {
        self.close(Status::Ok, json!({"status": "ok", "origin": self.origin}));
    }

    /// The turn failed with an error of `kind`.
    pub(crate) fn fail_turn(&mut self, kind: &str, message: &str) {
        self.close(
            Status::Failed,
            json!({"status": "failed", "origin": self.origin, "error": {"kind": kind, "message": message}}),
        );
    }

    /// The operator cancelled the turn. The partial reply, if any, is kept
    /// as an assistant message so the branch shows what was said.
    pub(crate) fn cancel_turn(&mut self) {
        self.close(
            Status::Cancelled,
            json!({"status": "cancelled", "origin": self.origin}),
        );
    }

    /// The turn ended as `status` with a caller-built result object (the
    /// server's result document without its text).
    pub(crate) fn finish_turn(&mut self, status: Status, result: serde_json::Value) {
        self.close(status, result);
    }

    fn close(&mut self, status: Status, result: serde_json::Value) {
        let Some(open) = self.turn.take() else {
            return;
        };
        if !self.segment.is_empty() {
            let text = std::mem::take(&mut self.segment);
            self.record(Kind::Message(StoredMessage {
                role: Role::Assistant,
                text,
                tool: None,
            }));
        }
        self.record(Kind::Turn(Turn {
            run_id: open.run_id,
            status,
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
        if self.session.is_some() {
            self.record(kind);
        } else {
            self.pending.push(kind);
        }
    }

    /// The one place entries are written.
    fn record(&mut self, kind: Kind) {
        if self.failed.is_some() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let mut kinds = std::mem::take(&mut self.pending);
        kinds.push(kind);
        match session.append(self.parent, kinds) {
            Ok(entries) => {
                self.parent = entries.last().map(|entry| entry.seq);
                self.fresh.extend_from_slice(entries);
            }
            Err(err) => self.failed = Some(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directories::Directories;
    use serde_json::Map;

    fn selection(model: &str) -> Selection {
        Selection {
            gateway: "local".into(),
            model: model.into(),
        }
    }

    fn kinds(session: &Session) -> Vec<String> {
        session
            .entries()
            .iter()
            .map(|entry| match &entry.kind {
                Kind::Message(StoredMessage { role, text, .. }) => format!("{role:?} {text}"),
                Kind::Turn(turn) => format!("turn:{}", turn.status.as_str()),
                Kind::ModelChange { model, .. } => format!("model:{model}"),
                Kind::ThinkingChange { level } => format!("thinking:{level}"),
                Kind::SkillLoad { name, .. } => format!("skill:{name}"),
                Kind::Unknown(value) => format!("unknown:{}", value["type"]),
            })
            .collect()
    }

    #[test]
    fn a_chat_turn_is_recorded_step_by_step_and_a_clear_starts_a_new_session() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("recorder");
        let store = Store::new(&Directories::single_root(dir.join("state")));
        let mut recorder = Recorder::new(true, "tui");
        // Announcements before the first turn add nothing: the header says so.
        recorder.thinking_changed("low");
        assert!(recorder.session_id().is_none());
        recorder.begin_turn(&store, &dir, &selection("m"), "low", "q1");
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
        recorder.end_turn();
        recorder.model_changed(&selection("m2"));
        recorder.begin_turn(&store, &dir, &selection("m2"), "low", "q2");
        recorder.fail_turn("transport", "down");
        recorder.begin_turn(&store, &dir, &selection("m2"), "low", "q3");
        recorder.text("half");
        recorder.cancel_turn();
        assert!(recorder.take_failure().is_none());

        let id = recorder.session_id().unwrap().to_string();
        let session = store.open(&id).unwrap();
        assert_eq!(
            kinds(&session),
            [
                "Operator q1",
                "Assistant reading",
                "Tool { exit_code: 0 } read_file a.rs\nfn a",
                "Assistant done",
                "turn:ok",
                "model:m2",
                "Operator q2",
                "turn:failed",
                "Operator q3",
                "Assistant half",
                "turn:cancelled",
            ]
        );
        assert_eq!(session.header().thinking, "low");
        let Kind::Turn(failed) = &session.entries()[7].kind else {
            panic!()
        };
        assert_eq!(failed.result["error"]["kind"], "transport");
        assert_eq!(failed.result["origin"], "tui");
        // Both later branches hang off the success and neither is active.
        assert_eq!(session.entries()[5].parent, Some(5));
        assert_eq!(
            session.entries()[8].parent,
            Some(8),
            "the next turn follows the last entry written"
        );
        assert_eq!(session.active_parent(), Some(5));
        assert_eq!(
            session.settings().model,
            "m2",
            "the model change is on the active path"
        );

        recorder.reset();
        recorder.begin_turn(&store, &dir, &selection("m2"), "off", "fresh");
        recorder.end_turn();
        assert_ne!(recorder.session_id(), Some(id.as_str()), "a new session");
        assert_eq!(store.list().unwrap().0.len(), 2);

        let mut off = Recorder::new(false, "tui");
        off.begin_turn(&store, &dir, &selection("m"), "off", "never");
        off.end_turn();
        assert_eq!(store.list().unwrap().0.len(), 2);

        let mut resumed = Recorder::resumed(store.hold(&id).unwrap(), "tui");
        resumed.begin_turn(&store, &dir, &selection("m2"), "low", "q4");
        resumed.end_turn();
        let session = store.open(&id).unwrap();
        assert_eq!(
            session.entries()[11].parent,
            Some(5),
            "resumed under the last success"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
