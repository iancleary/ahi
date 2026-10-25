//! Select files, filter them per hook, and run builtin checks or allowed local hooks.
//!
//! Hooks that need another language fail the run. They are not skipped.
//! Remote repos are not fetched and are not substituted with builtins.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use regex::Regex;

use crate::check::{self, builtin_by_id, writes_enabled, Check, CheckContext, Finding};
use crate::config::{stage_selects_files, Config, Hook, HookOriginKind};
use crate::error::{Error, Result};
use crate::filter::{existing_paths, filter_by_include_exclude, filter_by_types};
use crate::git::{self, GitFiles};
use crate::local_hook::{self, LocalRequest};
use crate::policy::{self, Policy, SystemAuth};

#[derive(Debug, Clone)]
pub enum Selection {
    Staged,
    All,
    Files(Vec<String>),
    Range { from: String, to: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone)]
pub struct HookReport {
    pub id: String,
    pub name: String,
    pub outcome: Outcome,
    pub findings: Vec<Finding>,
    pub skip_reason: Option<String>,
    pub detail: Option<String>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub result: Outcome,
    pub warnings: Vec<String>,
    pub hooks: Vec<HookReport>,
    /// Local system/script hook ids that were executed.
    pub local_hooks: Vec<String>,
    /// Environment names passed to each local hook. No values.
    pub local_env: Vec<PassedEnv>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassedEnv {
    pub hook: String,
    pub names: Vec<String>,
}

pub struct RunRequest<'a> {
    pub root: &'a Path,
    pub config: &'a Config,
    pub selection: Selection,
    pub stage: &'a str,
    pub only_hook: Option<&'a str>,
    pub fail_fast: bool,
    pub skip: BTreeSet<String>,
    pub commit_msg_filename: Option<&'a str>,
    /// Rewrite the three fixer builtins. Other checks stay report-only.
    pub fix: bool,
    /// `git add` rewritten paths. Used for the default staged run.
    pub stage_fixes: bool,
    pub verbose: bool,
}

pub fn run(request: &RunRequest<'_>) -> Result<Report> {
    if !crate::config::STAGES.contains(&request.stage) {
        return Err(Error::new(format!(
            "hook stage `{}` is not a pre-commit stage",
            request.stage
        )));
    }
    let policy = policy::load_policy(request.root)?;
    let git_files = select_files(request)?;
    let mut warnings = Vec::new();
    let mut local_hooks = Vec::new();
    let mut local_env = Vec::new();
    if matches!(request.selection, Selection::Staged) && git_files.unstaged {
        warnings.push(
            "unstaged changes are present; checks read the worktree. ahi does not stash, and it refuses to rewrite a file that has unstaged changes"
                .to_string(),
        );
    }
    let unstaged = git::unstaged_paths(request.root).unwrap_or_default();
    let names = existing_paths(request.root, git_files.paths.iter().map(String::as_str));
    let names = filter_by_include_exclude(
        names.iter().map(String::as_str),
        &request.config.files_re,
        &request.config.exclude_re,
    );
    let mut hooks = Vec::new();
    let mut stop = false;
    for repo in &request.config.repos {
        for hook in &repo.hooks {
            if stop {
                break;
            }
            if let Some(only) = request.only_hook {
                if hook.id != only && hook.alias != only {
                    continue;
                }
            }
            if !hook.stages.iter().any(|stage| stage == request.stage) {
                continue;
            }
            if request.skip.contains(&hook.id)
                || (!hook.alias.is_empty() && request.skip.contains(&hook.alias))
            {
                hooks.push(skipped(hook, "skipped by SKIP"));
                continue;
            }
            if hook.entry.split_whitespace().any(|part| part == "hazmat")
                && hook.entry.contains("pre-commit")
            {
                hooks.push(failed(
                    hook,
                    vec![Finding::bare(format!(
                        "hook `{}` uses pre-commit hazmat, which ahi does not implement",
                        hook.id
                    ))],
                ));
                if should_stop(request, hook) {
                    stop = true;
                }
                continue;
            }
            if hook.kind == HookOriginKind::Local {
                let outcome = run_local_hook(request, hook, &policy, &names, &git_files.modes);
                if let LocalRan::Ran { id, env_names } = &outcome.status {
                    local_hooks.push(id.clone());
                    local_env.push(PassedEnv {
                        hook: id.clone(),
                        names: env_names.clone(),
                    });
                }
                hooks.push(outcome.report);
                if outcome.failed && should_stop(request, hook) {
                    stop = true;
                }
                continue;
            }
            if hook.kind != HookOriginKind::Builtin {
                let message = unsupported_message(hook, &policy);
                hooks.push(failed(hook, vec![Finding::bare(message)]));
                if should_stop(request, hook) {
                    stop = true;
                }
                continue;
            }
            let Some(check) = builtin_by_id(&hook.id) else {
                hooks.push(failed(
                    hook,
                    vec![Finding::bare(format!(
                        "builtin hook `{}` is not implemented",
                        hook.id
                    ))],
                ));
                continue;
            };
            let report = run_builtin(request, hook, check, &names, &git_files.modes, &unstaged)?;
            let failed_hook = report.outcome == Outcome::Failed;
            hooks.push(report);
            if failed_hook && should_stop(request, hook) {
                stop = true;
            }
        }
    }
    if let Some(only) = request.only_hook {
        let known = request
            .config
            .repos
            .iter()
            .flat_map(|repo| repo.hooks.iter())
            .any(|hook| hook.id == only || hook.alias == only);
        if !known {
            return Err(Error::new(format!("no hook with id `{only}`")));
        }
        if hooks.is_empty() {
            warnings.push(format!(
                "hook `{only}` is not enabled for stage {}",
                request.stage
            ));
        }
    }
    let result = if hooks.iter().any(|hook| hook.outcome == Outcome::Failed) {
        Outcome::Failed
    } else {
        Outcome::Passed
    };
    Ok(Report {
        result,
        warnings,
        hooks,
        local_hooks,
        local_env,
    })
}

struct LocalStatus {
    report: HookReport,
    failed: bool,
    status: LocalRan,
}

enum LocalRan {
    Ran { id: String, env_names: Vec<String> },
    NotRan,
}

fn run_local_hook(
    request: &RunRequest<'_>,
    hook: &Hook,
    policy: &Policy,
    names: &[String],
    modes: &BTreeMap<String, String>,
) -> LocalStatus {
    let language = hook.language.as_deref().unwrap_or("");
    let script = language == "unsupported_script";
    let system = language == "unsupported";
    if !system && !script {
        let report = failed(hook, vec![Finding::bare(unsupported_message(hook, policy))]);
        return LocalStatus {
            failed: true,
            status: LocalRan::NotRan,
            report,
        };
    }
    let written =
        hook.language_source
            .as_deref()
            .unwrap_or(if script { "script" } else { "system" });
    if policy.authorize_system(&hook.id, &hook.entry) != SystemAuth::Allowed {
        let mut message = policy::policy_snippet(&hook.id, &hook.entry, written);
        if policy.authorize_system(&hook.id, &hook.entry) == SystemAuth::EntryMismatch {
            message.push_str(
                "\nThe id is listed, but the entry string does not match. ahi compares the entry exactly and does not invoke a shell.\n",
            );
        }
        return LocalStatus {
            failed: true,
            status: LocalRan::NotRan,
            report: failed(hook, vec![Finding::bare(message)]),
        };
    }
    if hook.language_version != "default" {
        return LocalStatus {
            failed: true,
            status: LocalRan::NotRan,
            report: failed(
                hook,
                vec![Finding::bare(format!(
                    "hook `{}` sets language_version `{}`; ahi uses the binary already on PATH and does not install a runtime",
                    hook.id, hook.language_version
                ))],
            ),
        };
    }
    if !hook.additional_dependencies.is_empty() {
        return LocalStatus {
            failed: true,
            status: LocalRan::NotRan,
            report: failed(
                hook,
                vec![Finding::bare(format!(
                    "hook `{}` sets additional_dependencies, which ahi does not install",
                    hook.id
                ))],
            ),
        };
    }
    let filtered = match filter_hook(request.root, names, hook, modes, None) {
        Ok(filtered) => filtered,
        Err(err) => {
            return LocalStatus {
                failed: true,
                status: LocalRan::NotRan,
                report: failed(hook, vec![Finding::bare(err.to_string())]),
            };
        }
    };
    if filtered.is_empty() && !hook.always_run {
        return LocalStatus {
            failed: false,
            status: LocalRan::NotRan,
            report: skipped(hook, "(no files to check)"),
        };
    }
    let files: Vec<String> = if hook.pass_filenames {
        filtered
    } else {
        Vec::new()
    };
    let toolchain_env = policy.toolchain_names(&hook.id, &hook.entry);
    let executed = local_hook::execute(&LocalRequest {
        root: request.root,
        entry: &hook.entry,
        args: &hook.args,
        files: &files,
        pass_filenames: hook.pass_filenames,
        require_serial: hook.require_serial,
        script,
        log_file: &hook.log_file,
        toolchain_env: &toolchain_env,
    });
    let env_names = match &executed {
        Ok(output) => output.env_names.clone(),
        Err(_) => Vec::new(),
    };
    let report = match executed {
        Ok(output) => {
            let show = output.failed || hook.verbose || request.verbose;
            HookReport {
                id: hook.id.clone(),
                name: hook.name.clone(),
                outcome: if output.failed {
                    Outcome::Failed
                } else {
                    Outcome::Passed
                },
                findings: if output.failed {
                    vec![Finding::bare(format!(
                        "command failed with exit status {}",
                        output.exit_code
                    ))]
                } else {
                    Vec::new()
                },
                skip_reason: None,
                detail: if show && !output.output.is_empty() {
                    Some(output.output)
                } else {
                    None
                },
                exit_code: Some(if output.failed { output.exit_code } else { 0 }),
            }
        }
        Err(err) => failed(hook, vec![Finding::bare(err.to_string())]),
    };
    let failed_hook = report.outcome == Outcome::Failed;
    LocalStatus {
        failed: failed_hook,
        status: LocalRan::Ran {
            id: hook.id.clone(),
            env_names,
        },
        report,
    }
}

fn run_builtin(
    request: &RunRequest<'_>,
    hook: &Hook,
    check: &dyn Check,
    names: &[String],
    modes: &BTreeMap<String, String>,
    unstaged: &BTreeSet<String>,
) -> Result<HookReport> {
    let filtered = filter_hook(request.root, names, hook, modes, Some(check))?;
    let always_run = effective_always_run(hook, Some(check));
    if filtered.is_empty() && !always_run {
        return Ok(skipped(hook, "(no files to check)"));
    }
    let pass_filenames = effective_pass_filenames(hook, Some(check));
    let inputs: Vec<String> = if pass_filenames { filtered } else { Vec::new() };
    let fix = writes_enabled(check, &hook.args, request.fix);
    let ctx = CheckContext {
        root: request.root,
        args: &hook.args,
        git_modes: modes,
        fix,
    };
    let mut outcome = check.check(&ctx, &inputs);
    if fix && !outcome.rewrites.is_empty() {
        outcome.findings.extend(apply_rewrites(
            request,
            &outcome.rewrites,
            unstaged,
            &inputs,
        ));
    }
    if outcome.findings.is_empty() {
        Ok(HookReport {
            id: hook.id.clone(),
            name: hook.name.clone(),
            outcome: Outcome::Passed,
            findings: Vec::new(),
            skip_reason: None,
            detail: None,
            exit_code: None,
        })
    } else {
        Ok(failed(hook, outcome.findings))
    }
}

fn apply_rewrites(
    request: &RunRequest<'_>,
    rewrites: &[check::Rewrite],
    unstaged: &BTreeSet<String>,
    inputs: &[String],
) -> Vec<Finding> {
    let allowed: BTreeSet<&str> = inputs.iter().map(String::as_str).collect();
    let mut findings = Vec::new();
    let mut added = Vec::new();
    for rewrite in rewrites {
        if !allowed.contains(rewrite.path.as_str()) {
            findings.push(Finding::bare(format!(
                "refusing to rewrite `{}` because it was not in the selected file set",
                rewrite.path
            )));
            continue;
        }
        if unstaged.contains(&rewrite.path) {
            findings.push(Finding::at(
                rewrite.path.clone(),
                None,
                "refusing to rewrite because the file has unstaged changes; ahi does not stash",
            ));
            continue;
        }
        match check::write_nofollow(request.root, &rewrite.path, &rewrite.bytes) {
            Ok(()) => added.push(rewrite.path.clone()),
            Err(err) => findings.push(Finding::bare(err.to_string())),
        }
    }
    if request.stage_fixes && !added.is_empty() {
        if let Err(err) = git::add_paths(request.root, &added) {
            findings.push(Finding::bare(err.to_string()));
        }
    }
    findings
}

fn filter_hook(
    root: &Path,
    names: &[String],
    hook: &Hook,
    modes: &BTreeMap<String, String>,
    check: Option<&dyn Check>,
) -> Result<Vec<String>> {
    let types;
    let types_ref: &[String] = if let Some(check) = check {
        if hook.types_explicit || hook.kind == HookOriginKind::Builtin {
            &hook.types
        } else {
            types = check
                .default_types()
                .iter()
                .map(|tag| (*tag).to_string())
                .collect::<Vec<_>>();
            &types
        }
    } else {
        &hook.types
    };
    let files_re;
    let files_ref: &Regex = if let Some(check) = check {
        if !hook.files_explicit
            && !check.default_files().is_empty()
            && hook.kind != HookOriginKind::Builtin
        {
            files_re = Regex::new(check.default_files()).map_err(|err| {
                Error::new(format!(
                    "hook `{}` default files regex is not supported ({err})",
                    hook.id
                ))
            })?;
            &files_re
        } else {
            &hook.files_re
        }
    } else {
        &hook.files_re
    };
    let filtered = filter_by_types(
        root,
        names,
        types_ref,
        &hook.types_or,
        &hook.exclude_types,
        modes,
    );
    Ok(filter_by_include_exclude(
        filtered.iter().map(String::as_str),
        files_ref,
        &hook.exclude_re,
    ))
}

fn effective_always_run(hook: &Hook, check: Option<&dyn Check>) -> bool {
    if let Some(check) = check {
        if !hook.always_run_explicit && hook.kind != HookOriginKind::Builtin {
            return check.default_always_run();
        }
    }
    hook.always_run
}

fn effective_pass_filenames(hook: &Hook, check: Option<&dyn Check>) -> bool {
    if let Some(check) = check {
        if !hook.pass_filenames_explicit && hook.kind != HookOriginKind::Builtin {
            return check.default_pass_filenames();
        }
    }
    hook.pass_filenames
}

fn should_stop(request: &RunRequest<'_>, hook: &Hook) -> bool {
    request.fail_fast || request.config.fail_fast || hook.fail_fast
}

fn skipped(hook: &Hook, reason: &str) -> HookReport {
    HookReport {
        id: hook.id.clone(),
        name: hook.name.clone(),
        outcome: Outcome::Skipped,
        findings: Vec::new(),
        skip_reason: Some(reason.to_string()),
        detail: None,
        exit_code: None,
    }
}

fn failed(hook: &Hook, findings: Vec<Finding>) -> HookReport {
    HookReport {
        id: hook.id.clone(),
        name: hook.name.clone(),
        outcome: Outcome::Failed,
        findings,
        skip_reason: None,
        detail: None,
        exit_code: None,
    }
}

fn unsupported_message(hook: &Hook, policy: &Policy) -> String {
    if hook.kind == HookOriginKind::Meta {
        return format!("hook `{}` (repo: meta) is not executed until v0.3", hook.id);
    }
    if let Some(language) = &hook.language {
        if language == "unsupported" || language == "unsupported_script" {
            let written = hook.language_source.as_deref().unwrap_or(language.as_str());
            if policy.authorize_system(&hook.id, &hook.entry) != SystemAuth::Allowed {
                return policy::policy_snippet(&hook.id, &hook.entry, written);
            }
        }
        if crate::config::language_is_planned(language) {
            return format!(
                "hook `{}` uses language `{language}`, which is not supported until v0.3",
                hook.id
            );
        }
        return format!(
            "hook `{}` uses language `{language}`, which is not supported by ahi",
            hook.id
        );
    }
    if hook.kind == HookOriginKind::Remote {
        format!(
            "hook `{}` from a remote repo is not executed until v0.3",
            hook.id
        )
    } else {
        format!("hook `{}` is not executed until v0.3", hook.id)
    }
}

fn select_files(request: &RunRequest<'_>) -> Result<GitFiles> {
    if matches!(request.stage, "commit-msg" | "prepare-commit-msg") {
        let Some(name) = request.commit_msg_filename else {
            return Err(Error::new(format!(
                "`--commit-msg-filename` is required for `--hook-stage {}`",
                request.stage
            )));
        };
        return git::explicit_files(request.root, &[name.to_string()]);
    }
    if !stage_selects_files(request.stage) {
        return Ok(GitFiles {
            paths: Vec::new(),
            modes: Default::default(),
            unstaged: false,
        });
    }
    match &request.selection {
        Selection::Staged => git::staged_files(request.root),
        Selection::All => git::all_files(request.root),
        Selection::Files(files) => git::explicit_files(request.root, files),
        Selection::Range { from, to } => git::changed_files(request.root, from, to),
    }
}
