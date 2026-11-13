//! `delegate`: hand a task to a worker and wait for its answer.
//!
//! The worker is another `ahi serve`, reached the way herdr-core reaches a
//! remote core: a command whose stdin and stdout carry the JSON lines.
//! `AHI_DELEGATE_VIA` names that command as an argv template with
//! `{worker}` for the label; the default, `hc connect --worker {worker}`,
//! asks hc, which owns the registry of labels and hosts, to connect this
//! process to the worker it maps the label to. ahi never learns the host:
//! the policy names a label, the command resolves it, and this module speaks
//! the service protocol over whatever comes back.
//!
//! One call is one turn on the worker: a session is created on the worker
//! (continuing `task` when given) in the directory its server runs in, the
//! prompt is submitted unattended with the call's timeout, and the turn's
//! result document becomes the tool result: the answer, the status, and the
//! worker's `effects`, which is what a coordinator reads before deciding
//! whether to look at the working directory again.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::service::client::Client;
use crate::service::{Event, CANCEL_GRACE};
use std::io::IsTerminal;

pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 600;
pub(crate) use crate::service::wire::MAX_TIMEOUT_SECS;
pub(crate) const MAX_PROMPT_BYTES: usize = 1024 * 1024;
/// The default `AHI_DELEGATE_VIA`.
pub(crate) const DEFAULT_VIA: &str = "hc connect --worker {worker}";
/// How much of the worker's answer reaches the model.
const MAX_ANSWER_BYTES: usize = 64 * 1024;
/// A worker's error message kept for the model.
const MAX_ERROR_BYTES: usize = crate::service::wire::MAX_ERROR_LEN;

/// What a delegation did, for the audit log and the ledger. Never the
/// prompt or the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DelegationReport {
    /// The connect command that was run, resolved for the label.
    pub(crate) via: Vec<String>,
    pub(crate) worker: String,
    /// The worker session, which is the task id the model may continue.
    pub(crate) session: String,
    pub(crate) run_id: String,
    /// `ok`, `failed`, `cancelled`, `timeout`.
    pub(crate) status: String,
    /// The worker's `effects`: `none`, `some`, or `unknown`.
    pub(crate) effects: String,
    pub(crate) duration_ms: u128,
}

pub(crate) struct Outcome {
    pub(crate) content: String,
    pub(crate) report: DelegationReport,
}

/// The active connect command template: `AHI_DELEGATE_VIA`, or the
/// default when it is unset or blank. The flag says which.
pub(crate) fn via_template() -> (String, bool) {
    match std::env::var("AHI_DELEGATE_VIA")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        Some(set) => (set, true),
        None => (DEFAULT_VIA.to_string(), false),
    }
}

/// The connect command for `worker`: the template split like every other
/// connect command, `{worker}` replaced in every token after the split so
/// a label is always one token. A template that does not parse is a
/// configuration error the operator sees, not something the model can
/// work around, so it comes back as the refusal text.
pub(crate) fn via_command(worker: &str) -> Result<Vec<String>, String> {
    let (template, _) = via_template();
    Ok(crate::service::via::split(&template)?
        .into_iter()
        .map(|token| token.replace("{worker}", worker))
        .collect())
}

pub(super) fn run(
    worker: &str,
    prompt: &str,
    task: Option<&str>,
    timeout_secs: u64,
    cancel: &AtomicBool,
) -> Result<Outcome, &'static str> {
    let argv = via_command(worker).map_err(|_| {
        "the connect command AHI_DELEGATE_VIA names does not parse; see `ahi doctor`"
    })?;
    let mut client = Client::via(&argv).map_err(|_| {
        "cannot reach the worker: the connect command did not start (is hc on PATH?)"
    })?;
    // Every failure below is a fixed sentence for the model. What the
    // connect command said on stderr is the operator's, so it goes to this
    // process's stderr only when that is not a screen.
    let result = run_on(
        &mut client,
        &argv,
        worker,
        prompt,
        task,
        timeout_secs,
        cancel,
    );
    if result.is_err() {
        let said = client.via_stderr();
        if !said.is_empty() && !std::io::stderr().is_terminal() {
            eprintln!("delegate {worker}: the connect command said: {said}");
        }
    }
    result
}

fn run_on(
    client: &mut Client,
    via: &[String],
    worker: &str,
    prompt: &str,
    task: Option<&str>,
    timeout_secs: u64,
    cancel: &AtomicBool,
) -> Result<Outcome, &'static str> {
    let started = Instant::now();
    let session = match task {
        Some(task) => {
            client
                .call("session.get", json!({"session": task}))
                .map_err(|_| "the worker has no such task")?;
            task.to_string()
        }
        None => client
            .call("session.create", json!({}))
            .map_err(|_| "the worker refused to open a session")?["id"]
            .as_str()
            .filter(|id| crate::session::valid_id(id))
            .ok_or("the worker returned no usable session id")?
            .to_string(),
    };
    let tip = client
        .call("session.get", json!({"session": session}))
        .map_err(|_| "the worker lost the session")?["tip"]
        .as_u64()
        .unwrap_or(0);
    client
        .call(
            "session.subscribe",
            json!({"session": session, "after": tip}),
        )
        .map_err(|_| "the worker refused the subscription")?;
    let submitted = client
        .call(
            "turn.submit",
            json!({
                "session": session,
                "prompt": prompt,
                "timeout": timeout_secs,
                "unattended": true,
                "origin": "serve",
                "client": "ahi delegate",
            }),
        )
        .map_err(|_| "the worker refused the task")?;
    // Both ids go into the ledger and the audit log; a worker's word for
    // them is checked like a client's.
    let run_id = submitted["run_id"]
        .as_str()
        .filter(|id| crate::session::valid_run_id(id))
        .ok_or("the worker returned no usable run id")?
        .to_string();
    let doc = if submitted["status"] == "running" {
        let mut deadline = started + Duration::from_secs(timeout_secs) + Duration::from_secs(5);
        let mut cancelled = false;
        loop {
            if !cancelled && cancel.load(Ordering::Relaxed) {
                cancelled = true;
                let _ = client.call("turn.cancel", json!({"session": session, "run_id": run_id}));
                // The worker gets its own grace to report; a partitioned or
                // deaf worker must not hold this tool call for its whole
                // timeout. Past that, its effects are unknown.
                deadline = deadline.min(Instant::now() + CANCEL_GRACE + Duration::from_secs(2));
            }
            if Instant::now() > deadline {
                if cancelled {
                    break json!({
                        "status": "cancelled",
                        "effects": "unknown",
                        "error": {"kind": "turn", "message": "the worker did not confirm the cancel"},
                    });
                }
                return Err("the worker did not answer in time");
            }
            match client.next_event(Duration::from_millis(100)) {
                Ok(Event::Turn {
                    run_id: ended,
                    result,
                    ..
                }) if ended == run_id => break result,
                Ok(_) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("the connection to the worker closed before it answered")
                }
            }
        }
    } else {
        submitted
    };
    Ok(outcome(
        via,
        worker,
        &session,
        &run_id,
        &doc,
        started.elapsed().as_millis(),
    ))
}

fn outcome(
    via: &[String],
    worker: &str,
    session: &str,
    run_id: &str,
    doc: &Value,
    duration_ms: u128,
) -> Outcome {
    let status = doc["status"].as_str().unwrap_or("failed").to_string();
    let effects = doc["effects"].as_str().unwrap_or("unknown").to_string();
    let mut content = format!("task {session} on {worker}: {status}\n");
    if status == "ok" {
        let text = doc["text"].as_str().unwrap_or_default();
        let mut cut = text.len().min(MAX_ANSWER_BYTES);
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        content.push_str(&text[..cut]);
        if cut < text.len() {
            content.push_str("\n[answer truncated]");
        }
    } else if let Some(message) = doc["error"]["message"].as_str() {
        let mut cut = message.len().min(MAX_ERROR_BYTES);
        while !message.is_char_boundary(cut) {
            cut -= 1;
        }
        content.push_str(&format!(
            "{}: {}{}",
            doc["error"]["kind"].as_str().unwrap_or("error"),
            &message[..cut],
            if cut < message.len() {
                " [truncated]"
            } else {
                ""
            }
        ));
    }
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&format!("effects: {effects}"));
    Outcome {
        content,
        report: DelegationReport {
            via: via.to_vec(),
            worker: worker.to_string(),
            session: session.to_string(),
            run_id: run_id.to_string(),
            status,
            effects,
            duration_ms,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_connect_command_comes_from_the_environment_with_the_label_filled_in() {
        std::env::remove_var("AHI_DELEGATE_VIA");
        assert_eq!(
            via_command("rust-ci").unwrap(),
            ["hc", "connect", "--worker", "rust-ci"]
        );
        std::env::set_var(
            "AHI_DELEGATE_VIA",
            "ssh {worker}.example '/opt/my tools/ahi' connect",
        );
        assert_eq!(
            via_command("box").unwrap(),
            ["ssh", "box.example", "/opt/my tools/ahi", "connect"]
        );
        std::env::set_var("AHI_DELEGATE_VIA", "ssh 'unclosed");
        assert!(via_command("box").is_err());
        std::env::remove_var("AHI_DELEGATE_VIA");
    }

    #[test]
    fn the_result_names_the_task_the_status_and_the_effects() {
        let via: Vec<String> = vec!["hc".into(), "connect".into()];
        let ok = outcome(
            &via,
            "docs",
            "s1",
            "r1",
            &json!({"status": "ok", "text": "done", "effects": "some"}),
            5,
        );
        assert_eq!(ok.content, "task s1 on docs: ok\ndone\neffects: some");
        assert_eq!(ok.report.effects, "some");
        assert_eq!(ok.report.via, via);
        let failed = outcome(
            &via,
            "docs",
            "s1",
            "r1",
            &json!({"status": "failed", "error": {"kind": "transport", "message": "down"}}),
            5,
        );
        assert_eq!(
            failed.content,
            "task s1 on docs: failed\ntransport: down\neffects: unknown"
        );
        let unconfirmed = outcome(
            &via,
            "docs",
            "s1",
            "r1",
            &json!({
                "status": "cancelled", "effects": "unknown",
                "error": {"kind": "turn", "message": "the worker did not confirm the cancel"},
            }),
            5,
        );
        assert_eq!(
            unconfirmed.content,
            "task s1 on docs: cancelled\nturn: the worker did not confirm the cancel\neffects: unknown"
        );
        assert_eq!(unconfirmed.report.status, "cancelled");
        let long = outcome(
            &via,
            "docs",
            "s1",
            "r1",
            &json!({"status": "failed", "error": {"kind": "turn", "message": "x".repeat(MAX_ERROR_BYTES + 1)}}),
            5,
        );
        assert!(long.content.contains(" [truncated]"));
        assert!(long.content.len() < MAX_ERROR_BYTES + 100);
    }
}
