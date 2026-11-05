//! One round of model tool calls: policy, operator approval, execution, and
//! results. Every requested call produces exactly one result, including calls
//! that were invalid, denied, cancelled, or over a limit.
//!
//! Authority comes from two places only: rules in `ahi.actions.yaml` and
//! decisions received on the `ApprovalReply` channel the screen holds. Model
//! text, tool arguments, and tool output cannot add a rule or a grant.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use super::tools::{self, PreparedCall, Workspace};
use super::{
    ApprovalReply, ApprovalRequest, ProposedAction, ResponseEvent, ToolCall, ToolDecision,
};
use crate::actions::{self, path_within, ActionPolicy, Verdict};
use crate::chat::transcript::{ChatMessage, ToolPayload, TranscriptRole};
use crate::error::{Error, Result};

/// Model responses that may request tools within one operator turn.
pub(super) const MAX_ROUNDS: usize = 8;
/// Tool content returned to the model within one operator turn.
pub(super) const MAX_TURN_OUTPUT_BYTES: usize = 256 * 1024;
const PREVIEW_LINES: usize = 8;
const PREVIEW_LINE_CHARS: usize = 200;

pub(super) const EXIT_OK: i32 = 0;
pub(super) const EXIT_ERROR: i32 = 1;
pub(super) const EXIT_DENIED: i32 = 126;

static NEXT_BATCH: AtomicU64 = AtomicU64::new(1);

/// An operator approval that outlives one batch: this tool, under this
/// non-empty prefix, in this workspace, for this gateway at this destination.
/// Switching the gateway, reconfiguring its URL, or changing the working
/// directory needs a new approval, because the grant decides which files
/// leave the machine and where they go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionGrant {
    root: PathBuf,
    gateway: String,
    destination: String,
    tool: String,
    prefix: String,
}

/// Held by the backend for the life of the chat process.
pub(super) type SessionGrants = Arc<Mutex<Vec<SessionGrant>>>;

pub(super) struct ToolRuntime {
    policy: ActionPolicy,
    workspace: Workspace,
    advertised: Vec<String>,
    gateway: String,
    destination: String,
    grants: SessionGrants,
    rounds: usize,
    output_bytes: usize,
}

struct Outcome {
    exit_code: i32,
    content: String,
    output: String,
}

enum Plan {
    Done(Outcome),
    Run(PreparedCall),
    Ask(PreparedCall),
}

impl ToolRuntime {
    /// `None` when no `ahi.actions.yaml` rule can approve a builtin tool, in
    /// which case no tool is advertised and the request is unchanged.
    pub(super) fn load(
        directory: &Path,
        state: &Path,
        gateway: &str,
        destination: &str,
        grants: SessionGrants,
    ) -> Result<Option<Self>> {
        let Some(policy) = actions::load(directory, &tools::BUILTIN)? else {
            return Ok(None);
        };
        let advertised = policy.advertised_tools();
        if advertised.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            workspace: Workspace::open(directory, state)?,
            policy,
            advertised,
            gateway: gateway.to_string(),
            destination: destination.to_string(),
            grants,
            rounds: 0,
            output_bytes: 0,
        }))
    }

    /// Protocol-neutral schemas of the advertised builtins.
    pub(super) fn schemas(&self) -> Vec<tools::ToolSchema> {
        tools::schemas(&self.advertised)
    }

    /// Decide and run one batch, emitting `ToolResult` then `ToolCallReturned`
    /// for every call in request order. Returns the `role: "tool"` messages.
    /// An `Err` is returned only after every call has its result.
    pub(super) fn run_round(
        &mut self,
        calls: &[ToolCall],
        sender: &Sender<ResponseEvent>,
    ) -> Result<Vec<ChatMessage>> {
        self.rounds += 1;
        let over_limit = self.rounds > MAX_ROUNDS;
        let mut plans: Vec<Plan> = calls
            .iter()
            .map(|call| {
                if over_limit {
                    return Plan::Done(error(&call.name, None, "tool round limit reached"));
                }
                match tools::prepare(call) {
                    Err(message) => Plan::Done(error(&call.name, None, message)),
                    Ok(prepared) => self.plan(prepared),
                }
            })
            .collect();
        let cancelled = self.ask(calls, &mut plans, sender)?;
        let mut messages = Vec::with_capacity(calls.len());
        for (call, plan) in calls.iter().zip(plans) {
            let mut outcome = match plan {
                Plan::Done(outcome) => outcome,
                // A cancelled batch runs nothing further, including calls that
                // rules alone had approved: the operator walked away from it.
                Plan::Ask(prepared) | Plan::Run(prepared) if cancelled => {
                    denied(&prepared, "approval cancelled")
                }
                Plan::Ask(prepared) => denied(&prepared, "approval cancelled"),
                Plan::Run(prepared) => self.run(&prepared),
            };
            self.account(&mut outcome);
            let mut message = ChatMessage::new(
                TranscriptRole::Tool {
                    exit_code: outcome.exit_code,
                },
                &outcome.output,
            );
            message.tool = Some(ToolPayload::Result {
                call_id: call.id.clone(),
                name: call.name.clone(),
                content: outcome.content.clone(),
            });
            emit(
                sender,
                ResponseEvent::ToolResult {
                    call_id: call.id.clone(),
                    tool: call.name.clone(),
                    output: outcome.output,
                    content: outcome.content,
                    exit_code: outcome.exit_code,
                },
            )?;
            emit(sender, ResponseEvent::ToolCallReturned)?;
            messages.push(message);
        }
        if over_limit {
            return Err(Error::new(format!(
                "model exceeded {MAX_ROUNDS} tool rounds in one turn; no further calls ran"
            )));
        }
        if cancelled {
            return Err(Error::new(
                "tool approval was cancelled; no further calls ran",
            ));
        }
        Ok(messages)
    }

    fn plan(&self, prepared: PreparedCall) -> Plan {
        match self.policy.evaluate(prepared.tool, &prepared.path) {
            Verdict::Deny {
                rule_id: Some(rule),
            } => Plan::Done(denied(&prepared, &format!("denied by rule {rule}"))),
            Verdict::Deny { rule_id: None } => Plan::Done(denied(
                &prepared,
                &format!("no {} rule allows this call", actions::FILE_NAME),
            )),
            Verdict::Approve { .. } => Plan::Run(prepared),
            Verdict::Ask { .. } if self.granted(&prepared) => Plan::Run(prepared),
            Verdict::Ask { .. } => Plan::Ask(prepared),
        }
    }

    fn granted(&self, prepared: &PreparedCall) -> bool {
        self.grants.lock().is_ok_and(|grants| {
            grants.iter().any(|grant| {
                grant.root == self.workspace.root()
                    && grant.gateway == self.gateway
                    && grant.destination == self.destination
                    && grant.tool == prepared.tool
                    && path_within(&grant.prefix, &prepared.path)
            })
        })
    }

    /// Ask the operator about every `Plan::Ask`. Returns whether the batch was
    /// cancelled (reply dropped or malformed); cancelled calls stay `Ask`.
    fn ask(
        &mut self,
        calls: &[ToolCall],
        plans: &mut [Plan],
        sender: &Sender<ResponseEvent>,
    ) -> Result<bool> {
        let pending: Vec<usize> = plans
            .iter()
            .enumerate()
            .filter_map(|(index, plan)| matches!(plan, Plan::Ask(_)).then_some(index))
            .collect();
        if pending.is_empty() {
            return Ok(false);
        }
        let proposed = pending
            .iter()
            .map(|&index| {
                let Plan::Ask(prepared) = &plans[index] else {
                    unreachable!("pending holds only Ask plans")
                };
                ProposedAction {
                    call_id: calls[index].id.clone(),
                    tool: prepared.tool.to_string(),
                    target: prepared.target().to_string(),
                    grant_prefix: prepared.grant_prefix(),
                }
            })
            .collect();
        let batch = NEXT_BATCH.fetch_add(1, Ordering::Relaxed);
        let (reply, decisions) = mpsc::channel();
        emit(
            sender,
            ResponseEvent::ToolApprovalRequested(ApprovalRequest {
                batch,
                calls: proposed,
                reply: ApprovalReply::new(batch, reply),
            }),
        )?;
        let decisions = match decisions.recv() {
            Ok(decisions) if decisions.len() == pending.len() => decisions,
            _ => return Ok(true),
        };
        for (index, decision) in pending.into_iter().zip(decisions) {
            let Plan::Ask(prepared) = std::mem::replace(&mut plans[index], Plan::Done(empty()))
            else {
                unreachable!("pending holds only Ask plans")
            };
            plans[index] = match decision {
                ToolDecision::Approve => Plan::Run(prepared),
                ToolDecision::ApproveForSession => {
                    // A whole-workspace grant is never offered; treat one as once.
                    let prefix = prepared.grant_prefix();
                    if !prefix.is_empty() {
                        if let Ok(mut grants) = self.grants.lock() {
                            grants.push(SessionGrant {
                                root: self.workspace.root().to_path_buf(),
                                gateway: self.gateway.clone(),
                                destination: self.destination.clone(),
                                tool: prepared.tool.to_string(),
                                prefix,
                            });
                        }
                    }
                    Plan::Run(prepared)
                }
                ToolDecision::Deny => Plan::Done(denied(&prepared, "denied by the operator")),
            };
        }
        Ok(false)
    }

    fn run(&mut self, prepared: &PreparedCall) -> Outcome {
        if self.output_bytes >= MAX_TURN_OUTPUT_BYTES {
            return error(
                prepared.tool,
                Some(prepared.target()),
                "tool output budget for this turn is exhausted",
            );
        }
        match tools::execute(prepared, &self.workspace) {
            Ok(content) => Outcome {
                exit_code: EXIT_OK,
                output: format!(
                    "{} {}\n{}",
                    prepared.tool,
                    prepared.target(),
                    preview(&content)
                ),
                content,
            },
            Err(message) => error(prepared.tool, Some(prepared.target()), message),
        }
    }

    /// Charge every result's model-facing content, including denials and
    /// errors, against the turn budget, truncating what would exceed it so the
    /// documented bound is a cap and not a threshold.
    fn account(&mut self, outcome: &mut Outcome) {
        let remaining = MAX_TURN_OUTPUT_BYTES.saturating_sub(self.output_bytes);
        if outcome.content.len() > remaining {
            let marker = "\n[truncated: tool output budget for this turn is exhausted]";
            let keep = remaining.saturating_sub(marker.len());
            let mut cut = keep.min(outcome.content.len());
            while cut > 0 && !outcome.content.is_char_boundary(cut) {
                cut -= 1;
            }
            outcome.content.truncate(cut);
            outcome.content.push_str(marker);
            outcome.content.truncate(remaining.max(cut));
        }
        self.output_bytes += outcome.content.len();
    }
}

fn empty() -> Outcome {
    Outcome {
        exit_code: EXIT_ERROR,
        content: String::new(),
        output: String::new(),
    }
}

/// `target` is shown only once arguments were prepared; raw arguments never are.
fn error(tool: &str, target: Option<&str>, message: &str) -> Outcome {
    let label = target.map_or_else(|| tool.to_string(), |target| format!("{tool} {target}"));
    Outcome {
        exit_code: EXIT_ERROR,
        content: format!("error: {message}"),
        output: format!("{label}: {message}"),
    }
}

fn denied(prepared: &PreparedCall, reason: &str) -> Outcome {
    Outcome {
        exit_code: EXIT_DENIED,
        content: format!("denied: {reason}"),
        output: format!("{} {}: {reason}", prepared.tool, prepared.target()),
    }
}

fn preview(content: &str) -> String {
    let mut lines: Vec<String> = content
        .lines()
        .take(PREVIEW_LINES)
        .map(|line| {
            let mut short: String = line.chars().take(PREVIEW_LINE_CHARS).collect();
            if short.len() < line.len() {
                short.push('…');
            }
            short
        })
        .collect();
    let total = content.lines().count();
    if total > PREVIEW_LINES {
        lines.push(format!("… {} more lines", total - PREVIEW_LINES));
    }
    if lines.is_empty() {
        lines.push("(empty)".to_string());
    }
    lines.join("\n")
}

fn emit(sender: &Sender<ResponseEvent>, event: ResponseEvent) -> Result<()> {
    sender
        .send(event)
        .map_err(|_| Error::new("model response receiver closed"))
}
