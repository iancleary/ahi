//! Operator approval of tool calls that policy left to `ask: true`.
//!
//! Each call in the batch gets its own decision. The cursor starts at the
//! first call and advances to the next undecided one after every answer;
//! Shift+Up/Down move it freely, and answering a decided call replaces its
//! decision. The answer is sent once, when every call has a decision.
//! Dropping a pending approval (exit, a failed turn) drops the reply channel,
//! which the backend treats as a denial.

use super::backend::{ApprovalRequest, ToolDecision};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalKey {
    Approve,
    ApproveForSession,
    Deny,
    /// Approve every call still undecided.
    ApproveRest,
    /// Deny every call still undecided.
    DenyRest,
    /// Move the highlight to the previous listed option (Up).
    Previous,
    /// Move the highlight to the next listed option (Down).
    Next,
    /// Apply the highlighted option (Enter).
    Choose,
    /// Move the cursor to the previous call, wrapping (Shift+Up).
    PreviousCall,
    /// Move the cursor to the next call, wrapping (Shift+Down).
    NextCall,
}

pub(crate) struct PendingApproval {
    request: ApprovalRequest,
    /// One slot per call, in request order.
    decisions: Vec<Option<ToolDecision>>,
    /// Transcript index of the prompt notice this approval keeps current.
    pub(crate) notice_index: usize,
    /// The call the listed options answer.
    current: usize,
    /// Highlighted entry of `options()`.
    selected: usize,
}

impl PendingApproval {
    pub(crate) fn new(request: ApprovalRequest, notice_index: usize) -> Self {
        let decisions = vec![None; request.calls.len()];
        Self {
            request,
            decisions,
            notice_index,
            current: 0,
            selected: 0,
        }
    }

    fn undecided(&self) -> usize {
        self.decisions.iter().filter(|slot| slot.is_none()).count()
    }

    fn complete(&self) -> bool {
        self.undecided() == 0
    }

    /// The answers offered for the current call, in the order the prompt
    /// lists them, each with its letter shortcut. Empty once every call is
    /// decided. A whole-workspace session grant is never offered, and the
    /// "all remaining" answers only while more than one call is undecided.
    fn options(&self) -> Vec<(ApprovalKey, String, &'static str)> {
        let Some(call) = self.request.calls.get(self.current) else {
            return Vec::new();
        };
        if self.complete() {
            return Vec::new();
        }
        let mut options = vec![(ApprovalKey::Approve, "Approve once".to_string(), "y")];
        if !call.grant_prefix.is_empty() {
            options.push((
                ApprovalKey::ApproveForSession,
                format!("Approve for session {}", scope(&call.grant_prefix)),
                "s",
            ));
        }
        options.push((ApprovalKey::Deny, "Deny".to_string(), "n"));
        let remaining = self.undecided();
        if remaining > 1 {
            options.push((
                ApprovalKey::ApproveRest,
                format!("Approve all {remaining} remaining"),
                "a",
            ));
            options.push((
                ApprovalKey::DenyRest,
                format!("Deny all {remaining} remaining"),
                "Esc",
            ));
        }
        options
    }

    /// Move the cursor by `step` calls, wrapping around the batch.
    fn move_cursor(&mut self, step: isize) {
        let count = self.request.calls.len() as isize;
        if count > 0 {
            self.current = (self.current as isize + step).rem_euclid(count) as usize;
            self.selected = 0;
        }
    }

    /// Apply a key. Returns true when every call has a decision.
    pub(crate) fn apply(&mut self, key: ApprovalKey) -> bool {
        let key = match key {
            ApprovalKey::Previous => {
                self.selected = self.selected.saturating_sub(1);
                return false;
            }
            ApprovalKey::Next => {
                let last = self.options().len().saturating_sub(1);
                self.selected = (self.selected + 1).min(last);
                return false;
            }
            ApprovalKey::PreviousCall => {
                self.move_cursor(-1);
                return false;
            }
            ApprovalKey::NextCall => {
                self.move_cursor(1);
                return false;
            }
            ApprovalKey::Choose => match self.options().get(self.selected) {
                Some((key, _, _)) => *key,
                None => return false,
            },
            key => key,
        };
        if self.complete() {
            return true;
        }
        let Some(call) = self.request.calls.get(self.current) else {
            return false;
        };
        // A whole-workspace session grant is not offered.
        if key == ApprovalKey::ApproveForSession && call.grant_prefix.is_empty() {
            return false;
        }
        match key {
            ApprovalKey::Approve => self.decisions[self.current] = Some(ToolDecision::Approve),
            ApprovalKey::ApproveForSession => {
                self.decisions[self.current] = Some(ToolDecision::ApproveForSession)
            }
            ApprovalKey::Deny => self.decisions[self.current] = Some(ToolDecision::Deny),
            ApprovalKey::ApproveRest | ApprovalKey::DenyRest => {
                let decision = if key == ApprovalKey::ApproveRest {
                    ToolDecision::Approve
                } else {
                    ToolDecision::Deny
                };
                for slot in self.decisions.iter_mut().filter(|slot| slot.is_none()) {
                    *slot = Some(decision);
                }
            }
            ApprovalKey::Previous
            | ApprovalKey::Next
            | ApprovalKey::Choose
            | ApprovalKey::PreviousCall
            | ApprovalKey::NextCall => return false,
        }
        // Continue at the next undecided call after this one, wrapping.
        let count = self.decisions.len();
        if let Some(next) = (1..=count)
            .map(|offset| (self.current + offset) % count)
            .find(|&index| self.decisions[index].is_none())
        {
            self.current = next;
        }
        self.selected = 0;
        self.complete()
    }

    /// Send the decisions. Only called once every call is decided.
    pub(crate) fn finish(self) {
        let decisions = self
            .decisions
            .into_iter()
            .map(|slot| slot.unwrap_or(ToolDecision::Deny))
            .collect();
        self.request.reply.send(decisions);
    }

    pub(crate) fn prompt(&self) -> String {
        let calls = &self.request.calls;
        let complete = self.complete();
        let mut lines = vec![format!(
            "Tool approval · {} call{}",
            calls.len(),
            if calls.len() == 1 { "" } else { "s" }
        )];
        for (index, call) in calls.iter().enumerate() {
            let status = match self.decisions[index] {
                Some(ToolDecision::Approve) => " · approved once".to_string(),
                Some(ToolDecision::ApproveForSession) => {
                    format!(" · approved for session {}", scope(&call.grant_prefix))
                }
                Some(ToolDecision::Deny) => " · denied".to_string(),
                None => String::new(),
            };
            let marker = match self.decisions[index] {
                _ if index == self.current && !complete => "›",
                Some(ToolDecision::Deny) => "✗",
                Some(_) => "✓",
                None => " ",
            };
            let detail = if call.detail.is_empty() {
                String::new()
            } else {
                format!(" ({})", call.detail)
            };
            lines.push(format!(
                "{marker} {}. {} {}{detail}{status}",
                index + 1,
                call.tool,
                call.target
            ));
        }
        for (index, (_, label, shortcut)) in self.options().iter().enumerate() {
            let marker = if index == self.selected { "▸" } else { " " };
            lines.push(format!("  {marker} {label} · {shortcut}"));
        }
        lines.join("\n")
    }

    /// Key hint for the input's upper rule: the arrows plus the letters of
    /// the options currently listed.
    pub(crate) fn hint(&self) -> String {
        let Some(call) = self.request.calls.get(self.current) else {
            return String::new();
        };
        if self.complete() {
            return String::new();
        }
        let mut hint = format!("Approve {}? ↑/↓ choose · Enter apply", call.tool);
        if self.request.calls.len() > 1 {
            hint.push_str(" · Shift+↑/↓ call");
        }
        for (key, _, shortcut) in self.options() {
            let short = match key {
                ApprovalKey::Approve => "once".to_string(),
                ApprovalKey::ApproveForSession => format!("session {}", scope(&call.grant_prefix)),
                ApprovalKey::Deny => "deny".to_string(),
                ApprovalKey::ApproveRest => "all".to_string(),
                ApprovalKey::DenyRest => "deny all".to_string(),
                _ => continue,
            };
            hint.push_str(&format!(" · {shortcut} {short}"));
        }
        hint.push(' ');
        hint
    }
}

fn scope(prefix: &str) -> String {
    format!("under {prefix}/")
}
