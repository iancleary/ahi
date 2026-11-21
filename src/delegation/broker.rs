//! Assignment RPCs exist only on a peer-checked local service connection. The
//! nonce authorizes a single claim; only that connection can publish the result.
#![cfg_attr(not(unix), allow(dead_code))]
use super::*;
use crate::service::{Service, ServiceError, ServiceResult};
use crate::tool_policy::{ActionPolicy, ExecutionMode};
use serde::de::DeserializeOwned;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Source {
    Operator { cwd: PathBuf },
    Parent { session: String, run: String },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub id: String,
    pub source: Source,
    pub mode: Option<ExecutionMode>,
    pub spec: Spec,
    pub follow_ups: Vec<String>,
    pub preview: bool,
}

#[derive(Serialize, Deserialize)]
pub(super) struct Admission {
    pub mode: ExecutionMode,
    pub policy: ActionPolicy,
    pub follow_ups: Vec<String>,
}

pub(super) fn denied(message: impl ToString) -> ServiceError {
    ServiceError {
        kind: "policy",
        message: message.to_string(),
    }
}
pub(super) fn parse<T: DeserializeOwned>(value: Value) -> ServiceResult<T> {
    serde_json::from_value(value).map_err(denied)
}
impl From<Error> for ServiceError {
    fn from(e: Error) -> Self {
        Self {
            kind: match e.kind {
                ErrorKind::Input => "usage",
                ErrorKind::Conflict => "conflict",
                ErrorKind::Launch => "launch",
                ErrorKind::Io => "io",
            },
            message: e.message,
        }
    }
}

// Conservative inclusion proof, not a ranking of workspace mode names.
// None means unrestricted in each matcher. Reject incomparable selectors.
fn covers(broad: &crate::tool_policy::ToolRule, narrow: &crate::tool_policy::ToolRule) -> bool {
    broad.action == narrow.action
        && narrow.tools.iter().all(|t| broad.tools.contains(t))
        && broad.skill_script == narrow.skill_script
        && broad.path.as_deref().is_none_or(|p| {
            crate::tool_policy::path_within(p, narrow.path.as_deref().unwrap_or(""))
        })
        && broad
            .argv
            .as_ref()
            .is_none_or(|argv| narrow.argv.as_ref().is_some_and(|n| n.starts_with(argv)))
        && narrow
            .toolchain_env
            .iter()
            .all(|v| broad.toolchain_env.contains(v))
}
// A later admission may lose grants, never acquire wider ones. Carry forward deny rules.
pub(super) fn ceiling(
    mut current: ActionPolicy,
    old: &ActionPolicy,
) -> ServiceResult<ActionPolicy> {
    use crate::tool_policy::Effect::{Approve, Deny};
    for r in current.rules.iter().filter(|r| r.effect == Approve) {
        if !old
            .rules
            .iter()
            .any(|p| p.effect == Approve && (!p.ask || r.ask) && covers(p, r))
        {
            return Err(denied("requested rules exceed the admitted ceiling"));
        }
    }
    for p in old.rules.iter().filter(|p| p.effect == Deny) {
        if !current.rules.contains(p) {
            current.rules.push(p.clone());
        }
    }
    Ok(current)
}
// A parent's still-running turn is necessary but not sufficient: trust can
// be revoked or the workspace's default changed while inference is running.
pub(super) fn workspace_cap(
    cwd: &Path,
    dirs: &Directories,
    mode: &ExecutionMode,
    policy: ActionPolicy,
) -> ServiceResult<ActionPolicy> {
    let (now, workspace) = crate::chat::operator_authority(cwd, &dirs.state).map_err(denied)?;
    if !super::local::granted(&now, &workspace) {
        return Err(denied("workspace no longer authorizes local delegation"));
    }
    ceiling(policy, &restrict(now, workspace, Some(mode.clone()))?.1)
}
pub(super) fn restrict(
    parent: ExecutionMode,
    mut policy: ActionPolicy,
    requested: Option<ExecutionMode>,
) -> ServiceResult<(ExecutionMode, ActionPolicy)> {
    use ExecutionMode::*;
    let child = requested.unwrap_or_else(|| parent.clone());
    match (&parent, &child) {
        (p, c) if p == c => {}
        (_, ReadOnly) | (FullAccess, ReadWrite) => {}
        (p, Named(name)) if *p != ReadOnly => {
            let next = policy
                .named
                .remove(name)
                .ok_or_else(|| denied("requested workspace mode is unavailable"))?;
            use crate::tool_policy::Effect::{Approve, Deny};
            let safe = matches!(parent, ReadWrite | FullAccess)
                || (next.iter().filter(|r| r.effect == Approve).all(|r| {
                    policy
                        .rules
                        .iter()
                        .any(|p| p.effect == Approve && (!p.ask || r.ask) && covers(p, r))
                }) && policy
                    .rules
                    .iter()
                    .filter(|r| r.effect == Deny)
                    .all(|p| next.iter().any(|r| r.effect == Deny && covers(r, p))));
            if !safe {
                return Err(denied(
                    "requested workspace mode is not a proven subset of the parent",
                ));
            }
            policy.rules = next;
        }
        _ => {
            return Err(denied(format!(
                "cannot delegate {} from {}",
                child.label(),
                parent.label()
            )))
        }
    }
    // The scratch profile never grants host paths, executable tools, skill
    // loading or recursion. Preserve deny/path/ask constraints, not just names.
    policy.named.clear();
    policy.default_mode = None;
    for rule in &mut policy.rules {
        rule.tools.retain(|t| {
            matches!(
                t.as_str(),
                "read_file"
                    | "list_dir"
                    | "find_files"
                    | "search_files"
                    | "write_file"
                    | "edit_file"
                    | "plan_scratch"
            ) && child.allows(t)
                && parent.allows(t)
        });
    }
    policy.rules.retain(|r| !r.tools.is_empty());
    Ok((child, policy))
}

#[cfg(test)]
mod mode_tests {
    use super::*;
    use ExecutionMode::*;
    #[test]
    fn downgrades_preserve_denies_and_ask_boundaries() {
        let source = r#"version: 2
default_mode: review
modes:
  review:
    ask:
      files: [{tools: [read_file], paths: [src]}]
    deny:
      files: [{tools: [read_file], paths: [src/private]}]
  smaller:
    ask:
      files: [{tools: [read_file], paths: [src/lib]}]
    deny:
      files: [{tools: [read_file], paths: [src/private]}]
  silent:
    allow:
      files: [{tools: [read_file], paths: [src/lib]}]
    deny:
      files: [{tools: [read_file], paths: [src/private]}]
  removed-deny:
    ask:
      files: [{tools: [read_file], paths: [src]}]
"#;
        let parent = Named("review".into());
        let policy = crate::tool_policy::parse(source, "fixture", &crate::tools::BUILTIN)
            .unwrap()
            .select(&parent)
            .unwrap();
        let child = |mode| restrict(parent.clone(), policy.clone(), Some(mode));
        assert!(child(Named("smaller".into())).is_ok());
        for mode in [
            Named("silent".into()),
            Named("removed-deny".into()),
            ReadWrite,
            FullAccess,
        ] {
            assert!(child(mode).is_err());
        }
        let (_, plan) = child(ReadOnly).unwrap();
        assert!(matches!(
            plan.evaluate("read_file", "src/private/a", &[]),
            crate::tool_policy::Verdict::Deny { .. }
        ));
        assert!(matches!(
            plan.evaluate("read_file", "src/a", &[]),
            crate::tool_policy::Verdict::Ask { .. }
        ));
        assert!(restrict(ReadOnly, ActionPolicy::plan(), Some(ReadWrite)).is_err());
        assert!(restrict(ReadWrite, ActionPolicy::trusted(), Some(FullAccess)).is_err());
        let (_, p) = restrict(FullAccess, ActionPolicy::trusted(), Some(ReadWrite)).unwrap();
        assert!(p.advertised_tools().contains(&"write_file".into()));
        assert!(!p.advertised_tools().contains(&"run_command".into()));
    }
}

pub(crate) fn admit(service: &Service, socket: &Path, r: Request) -> ServiceResult<Value> {
    let dirs = service.directories();
    preview(dirs, &r.spec)?;
    assignment_dir(dirs, &r.id)?;
    if r.follow_ups.len() > 15
        || r.follow_ups.iter().any(|p| p.trim().is_empty())
        || r.follow_ups.iter().map(String::len).sum::<usize>() + r.spec.prompt.len()
            > MAX_PROMPT_BYTES
    {
        return Err(denied("at most 16 nonempty turns totaling 1 MiB"));
    }
    if !matches!(r.spec.target, Target::RexServer { .. }) {
        return Err(denied(
            "a dedicated Rex session requires a server-only profile",
        ));
    }
    let (parent, policy, workspace, previous) = match &r.source {
        Source::Operator { cwd } => {
            let cwd = cwd.canonicalize().map_err(denied)?;
            let (m, p) = crate::chat::operator_authority(&cwd, &dirs.state).map_err(denied)?;
            (m, p, cwd, None)
        }
        Source::Parent { session, run } => {
            let crate::service::ParentAuthority {
                mode,
                current: policy,
                workspace: cwd,
                frozen: prior,
            } = service.parent_authority(
                session,
                run,
                &r.spec.gateway,
                &r.spec.model,
                &r.spec.target,
            )?;
            if !super::local::granted(&mode, &policy) || !super::local::granted(&mode, &prior) {
                return Err(denied("parent mode has no local delegation grant"));
            }
            (mode, policy, cwd, Some(prior))
        }
    };
    let (mode, policy) = restrict(parent.clone(), policy, r.mode.clone())?;
    let policy = if let Some(prior) = previous {
        workspace_cap(
            &workspace,
            dirs,
            &mode,
            ceiling(policy, &restrict(parent.clone(), prior, r.mode)?.1)?,
        )?
    } else {
        policy
    };
    crate::harness::completion_connection(dirs, &r.spec.gateway, &r.spec.model).map_err(|e| {
        ServiceError {
            kind: "config",
            message: e.to_string(),
        }
    })?;
    let executable = std::env::current_exe().map_err(denied)?;
    let summary = json!({"parent_mode":parent,"mode":mode,"tools":policy.advertised_tools(),"turns":1+r.follow_ups.len(),"source":r.source,"workspace":workspace});
    if r.preview {
        return Ok(
            json!({"dry_run":true,"reserved":false,"gateway":r.spec.gateway,"model":r.spec.model,"thinking":r.spec.thinking,"timeout_secs":r.spec.timeout_secs,"worker_executable":executable,"prompt_sha256":crate::session::sha256_hex(r.spec.prompt.as_bytes()),"prompt_bytes":r.spec.prompt.len(),"target":r.spec.target,"placement":"scratch","tools":summary["tools"],"authority":summary}),
        );
    }
    let token = credential(socket)?;
    let scratch = std::env::temp_dir()
        .canonicalize()
        .map_err(denied)?
        .join(format!("ahi-scratch-{}", r.id));
    let admission = Admission {
        mode,
        policy,
        follow_ups: r.follow_ups,
    };
    let bytes = serde_json::to_vec(&admission).map_err(denied)?;
    if bytes.len() > crate::service::wire::MAX_LINE - 8192 {
        return Err(denied("admission exceeds the service claim limit"));
    }
    let mut receipt = reserve_at(dirs, r.spec, r.id, scratch)?;
    let dir = assignment_dir(dirs, &receipt.id)?;
    write_private_new(&dir.join("admission.json"), &bytes)?;
    write_private_new(
        &dir.join("authority.json"),
        &serde_json::to_vec(&summary).map_err(denied)?,
    )?;
    write_private_new(
        &dir.join("broker.json"),
        &serde_json::to_vec(&token).map_err(denied)?,
    )?;
    if let Err(e) = launch(dirs, &mut receipt, &executable, None) {
        return Err(ServiceError {
            kind: "launch",
            message: format!("assignment {} is {}: {e}", receipt.id, receipt.phase.name()),
        });
    }
    Ok(status(dirs, &receipt.id)?)
}

pub(super) fn credential(socket: &Path) -> ServiceResult<Value> {
    let mut nonce = [0u8; 32];
    getrandom::fill(&mut nonce).map_err(denied)?;
    let nonce: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    Ok(json!({"socket":socket,"nonce":nonce}))
}

// One per accepted connection; dropping the connection never implicitly
// releases a claim or replays inference.
#[derive(Default)]
pub(crate) struct Connection {
    held: Option<Lease>,
}
struct Lease {
    dir: PathBuf,
    receipt: Receipt,
    attempt: String,
    turns: usize,
}
impl Connection {
    pub(crate) fn dispatch(
        &mut self,
        service: &Service,
        socket: &Path,
        method: &str,
        params: Value,
    ) -> ServiceResult<Value> {
        match method {
            "child.admit" => admit(service, socket, parse(params)?),
            "child.follow" => super::resident::follow(service, socket, parse(params)?, None),
            "child.manage" => super::resident::manage(service, socket, parse(params)?),
            "child.close" => super::resident::close(service.directories(), parse(params)?),
            "child.claim" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct ClaimRequest {
                    id: String,
                    attempt: String,
                    nonce: String,
                    pid: u32,
                    cwd: PathBuf,
                    observed: Observed,
                }
                let r: ClaimRequest = parse(params)?;
                if self.held.is_some() {
                    return Err(denied("connection already holds a claim"));
                }
                let dir = assignment_dir(service.directories(), &r.id)?;
                let receipt = load_dir(&dir)?;
                let token: Value = read_json(&dir.join("broker.json"))?
                    .ok_or_else(|| denied("no service-owned assignment"))?;
                if token["socket"].as_str().map(Path::new) != Some(socket)
                    || token["nonce"].as_str() != Some(&r.nonce)
                    || !r
                        .observed
                        .rex_block
                        .as_deref()
                        .and_then(|id| id.strip_prefix("block:"))
                        .is_some_and(crate::session::valid_id)
                    || matches!(&receipt.phase,Phase::Launched{terminal,..} if Some(&terminal.id)!=r.observed.rex_block.as_ref())
                    || !super::resident::session_matches(&receipt, &r.observed)
                    || r.cwd.canonicalize().map_err(denied)?
                        != receipt.scratch.canonicalize().map_err(denied)?
                    || (super::resident::child_id(&dir, &receipt)? == receipt.id
                        && fs::read_dir(&receipt.scratch)
                            .map_err(denied)?
                            .next()
                            .is_some())
                {
                    return Err(denied(
                        "claim credential, cwd or Rex session does not match",
                    ));
                }
                let admission: Admission = read_json(&dir.join("admission.json"))?
                    .ok_or_else(|| denied("missing admission"))?;
                let (receipt, claim) = claim_pid(&dir, &r.attempt, &r.observed, r.pid)?;
                self.held = Some(Lease {
                    dir,
                    receipt,
                    attempt: r.attempt,
                    turns: admission.follow_ups.len() + 1,
                });
                Ok(json!({"claim":claim,"admission":admission}))
            }
            "child.finish" => {
                let Lease {
                    dir,
                    receipt: r,
                    attempt: a,
                    turns,
                } = self
                    .held
                    .as_ref()
                    .ok_or_else(|| denied("connection holds no claim"))?;
                let mut out: Outcome = parse(params)?;
                if out.turns_completed > *turns
                    || (out.status == Status::Ok && out.turns_completed != *turns)
                    || out.assignment != r.id
                    || &out.attempt != a
                    || out.gateway != r.gateway
                    || out.model != r.model
                    || out.text.len() > MAX_RESULT_TEXT_BYTES
                    || out.text_bytes != out.text.len() as u64
                {
                    return Err(denied("invalid result for claim"));
                }
                // The worker has already committed its disposition to the transcript.
                // A late cancel request must not rewrite that terminal result.
                out.finished = crate::util::rfc3339::now();
                finish(dir, &out)?;
                self.held = None;
                Ok(json!({"status":out.status}))
            }
            _ => Err(denied("unknown child method")),
        }
    }
}
