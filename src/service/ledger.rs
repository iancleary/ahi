//! What a turn did, counted from the engine's events: the calls the model
//! asked for, how each ended, and the records of the effectful ones. The
//! ledger is the part of a turn's result a coordinator reads before
//! deciding whether the working directory changed.

use serde_json::{json, Value};

use crate::chat::backend::ToolRecord;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Ledger {
    /// Calls the model requested.
    pub(crate) calls: usize,
    /// Calls a rule or an operator denied.
    pub(crate) denied: usize,
    /// Calls denied because nobody could answer an `ask`.
    pub(crate) ask_denied: usize,
    /// Calls that reached a tool and failed.
    pub(crate) errors: usize,
    /// Tool rounds the model ran.
    pub(crate) rounds: usize,
    /// Results still owed for the current round.
    pending: usize,
    /// Whether the turn ended with a call in flight.
    pub(crate) interrupted: bool,
    records: Vec<ToolRecord>,
}

/// Exit code the tool loop uses for a denial.
const EXIT_DENIED: i32 = 126;

impl Ledger {
    /// The model asked for `count` calls.
    pub(crate) fn requested(&mut self, count: usize) {
        self.calls += count;
        self.rounds += 1;
        self.pending = count;
    }

    /// Nobody could answer an `ask` for `count` calls.
    pub(crate) fn unattended(&mut self, count: usize) {
        self.ask_denied += count;
    }

    /// One call returned.
    pub(crate) fn returned(&mut self, exit_code: i32, record: Option<&ToolRecord>) {
        self.pending = self.pending.saturating_sub(1);
        if exit_code == EXIT_DENIED {
            self.denied += 1;
        } else if exit_code != 0 {
            self.errors += 1;
        }
        if let Some(record) = record {
            self.records.push(record.clone());
        }
    }

    /// The turn ended; `pending` calls never returned.
    pub(crate) fn close(&mut self) {
        self.interrupted = self.pending > 0;
        self.pending = 0;
    }

    /// `none` when nothing effectful ran, `some` when every effect is
    /// recorded, `unknown` when a call was in flight at the end or a
    /// command's fate is not known.
    pub(crate) fn effects(&self) -> &'static str {
        if self.interrupted || self.records.iter().any(|record| record.effect().is_none()) {
            return "unknown";
        }
        if self
            .records
            .iter()
            .any(|record| record.effect() == Some(true))
        {
            "some"
        } else {
            "none"
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "tools": {
                "calls": self.calls,
                "denied": self.denied,
                "ask_denied": self.ask_denied,
                "errors": self.errors,
                "rounds": self.rounds,
            },
            "effects": self.effects(),
            "ledger": self.records.iter().map(record_json).collect::<Vec<_>>(),
        })
    }
}

fn record_json(record: &ToolRecord) -> Value {
    match record {
        ToolRecord::Write {
            tool,
            path,
            outcome,
            before,
            after,
            lines_added,
            lines_removed,
        } => json!({
            "tool": tool, "path": path, "outcome": outcome,
            "before": before, "after": after,
            "lines_added": lines_added, "lines_removed": lines_removed,
        }),
        ToolRecord::Command {
            argv,
            cwd,
            outcome,
            ran,
            exit_code,
            duration_ms,
        } => json!({
            "tool": "run_command", "argv": argv, "cwd": cwd, "outcome": outcome,
            "ran": ran, "exit_code": exit_code, "duration_ms": duration_ms,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(outcome: &str) -> ToolRecord {
        ToolRecord::Write {
            tool: "write_file".into(),
            path: "a".into(),
            outcome: outcome.into(),
            before: None,
            after: Some("ab".into()),
            lines_added: 1,
            lines_removed: 0,
        }
    }

    fn command(ran: Option<bool>) -> ToolRecord {
        ToolRecord::Command {
            argv: vec!["true".into()],
            cwd: String::new(),
            outcome: "ok".into(),
            ran,
            exit_code: Some(0),
            duration_ms: 1,
        }
    }

    #[test]
    fn effects_follow_the_records_and_an_interrupted_round() {
        let mut ledger = Ledger::default();
        assert_eq!(ledger.effects(), "none");
        ledger.requested(3);
        ledger.returned(0, None);
        ledger.returned(126, None);
        ledger.returned(1, Some(&write("failed")));
        ledger.close();
        assert_eq!(ledger.effects(), "none", "a failed write changed nothing");
        assert_eq!((ledger.denied, ledger.errors, ledger.rounds), (1, 1, 1));

        ledger.requested(1);
        ledger.returned(0, Some(&write("ok")));
        ledger.close();
        assert_eq!(ledger.effects(), "some");
        let json = ledger.to_json();
        assert_eq!(json["tools"]["calls"], 4);
        assert_eq!(json["ledger"].as_array().unwrap().len(), 2);
        assert_eq!(json["ledger"][1]["after"], "ab");

        ledger.requested(1);
        ledger.unattended(1);
        ledger.returned(126, None);
        ledger.close();
        assert_eq!(ledger.ask_denied, 1);

        ledger.requested(2);
        ledger.returned(0, Some(&command(Some(true))));
        ledger.close();
        assert_eq!(ledger.effects(), "unknown", "a call never returned");
        assert!(ledger.interrupted);

        let mut unknown = Ledger::default();
        unknown.requested(1);
        unknown.returned(1, Some(&command(None)));
        unknown.close();
        assert_eq!(unknown.effects(), "unknown");
    }
}
