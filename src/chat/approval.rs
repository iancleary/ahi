//! Operator approval of tool calls that policy left to `ask: true`.
//!
//! Calls are decided one at a time, in request order. The answer is sent once,
//! when every call has a decision. Dropping a pending approval (exit, a failed
//! turn) drops the reply channel, which the backend treats as a denial.

use super::backend::{ApprovalRequest, ToolDecision};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalKey {
    Approve,
    ApproveForSession,
    Deny,
    ApproveRest,
    DenyRest,
}

pub(crate) struct PendingApproval {
    request: ApprovalRequest,
    decisions: Vec<ToolDecision>,
    /// Transcript index of the prompt notice this approval keeps current.
    pub(crate) notice_index: usize,
}

impl PendingApproval {
    pub(crate) fn new(request: ApprovalRequest, notice_index: usize) -> Self {
        Self {
            request,
            decisions: Vec::new(),
            notice_index,
        }
    }

    /// Apply a key. Returns true when every call has a decision.
    pub(crate) fn apply(&mut self, key: ApprovalKey) -> bool {
        let remaining = self.request.calls.len() - self.decisions.len();
        // A whole-workspace session grant is not offered.
        if key == ApprovalKey::ApproveForSession
            && self
                .request
                .calls
                .get(self.decisions.len())
                .is_some_and(|call| call.grant_prefix.is_empty())
        {
            return false;
        }
        let (decision, count) = match key {
            ApprovalKey::Approve => (ToolDecision::Approve, 1),
            ApprovalKey::ApproveForSession => (ToolDecision::ApproveForSession, 1),
            ApprovalKey::Deny => (ToolDecision::Deny, 1),
            ApprovalKey::ApproveRest => (ToolDecision::Approve, remaining),
            ApprovalKey::DenyRest => (ToolDecision::Deny, remaining),
        };
        self.decisions
            .extend(std::iter::repeat_n(decision, count.min(remaining)));
        self.decisions.len() == self.request.calls.len()
    }

    /// Send the decisions. Only called once every call is decided.
    pub(crate) fn finish(self) {
        self.request.reply.send(self.decisions);
    }

    pub(crate) fn prompt(&self) -> String {
        let calls = &self.request.calls;
        let mut lines = vec![format!(
            "Tool approval · {} call{}",
            calls.len(),
            if calls.len() == 1 { "" } else { "s" }
        )];
        for (index, call) in calls.iter().enumerate() {
            let (marker, status) = match self.decisions.get(index) {
                Some(ToolDecision::Approve) => ("✓", " · approved once".to_string()),
                Some(ToolDecision::ApproveForSession) => (
                    "✓",
                    format!(" · approved for session {}", scope(&call.grant_prefix)),
                ),
                Some(ToolDecision::Deny) => ("✗", " · denied".to_string()),
                None if index == self.decisions.len() => ("›", String::new()),
                None => (" ", String::new()),
            };
            lines.push(format!(
                "{marker} {}. {} {}{status}",
                index + 1,
                call.tool,
                call.target
            ));
        }
        lines.join("\n")
    }

    /// Key hint for the input's upper rule.
    pub(crate) fn hint(&self) -> String {
        match self.request.calls.get(self.decisions.len()) {
            Some(call) if call.grant_prefix.is_empty() => format!(
                "Approve {}? y once · n deny · a all · Esc deny all ",
                call.tool
            ),
            Some(call) => format!(
                "Approve {}? y once · s session {} · n deny · a all · Esc deny all ",
                call.tool,
                scope(&call.grant_prefix)
            ),
            None => String::new(),
        }
    }
}

fn scope(prefix: &str) -> String {
    format!("under {prefix}/")
}
