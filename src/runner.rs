//! Select files, filter them per hook, and run builtin checks.
//!
//! Hooks that need another language fail the run. They are not skipped.

use std::collections::BTreeSet;
use std::path::Path;

use crate::check::{builtin_by_id, CheckContext, Finding};
use crate::config::{stage_selects_files, Config, Hook, HookOriginKind};
use crate::error::{Error, Result};
use crate::filter::{existing_paths, filter_by_include_exclude, filter_by_types};
use crate::git::{self, GitFiles};
use crate::policy::{self, Policy};

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
}

#[derive(Debug, Clone)]
pub struct Report {
    pub result: Outcome,
    pub warnings: Vec<String>,
    pub hooks: Vec<HookReport>,
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
    if matches!(request.selection, Selection::Staged) && git_files.unstaged {
        warnings.push(
            "unstaged changes are present; v0.1 checks read the worktree and do not stash (no check rewrites files)"
                .to_string(),
        );
    }
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
            if hook.kind != HookOriginKind::Builtin {
                let message = unsupported_message(hook, &policy);
                hooks.push(failed(hook, vec![Finding::bare(message)]));
                if should_stop(request, hook) {
                    stop = true;
                }
                continue;
            }
            let filtered = filter_by_types(
                request.root,
                &names,
                &hook.types,
                &hook.types_or,
                &hook.exclude_types,
                &git_files.modes,
            );
            let filtered = filter_by_include_exclude(
                filtered.iter().map(String::as_str),
                &hook.files_re,
                &hook.exclude_re,
            );
            if filtered.is_empty() && !hook.always_run {
                hooks.push(skipped(hook, "(no files to check)"));
                continue;
            }
            let inputs: Vec<String> = if hook.pass_filenames {
                filtered
            } else {
                Vec::new()
            };
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
            let ctx = CheckContext {
                root: request.root,
                args: &hook.args,
            };
            let outcome = check.check(&ctx, &inputs);
            if outcome.findings.is_empty() {
                hooks.push(HookReport {
                    id: hook.id.clone(),
                    name: hook.name.clone(),
                    outcome: Outcome::Passed,
                    findings: Vec::new(),
                    skip_reason: None,
                });
            } else {
                hooks.push(failed(hook, outcome.findings));
                if should_stop(request, hook) {
                    stop = true;
                }
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
    })
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
    }
}

fn failed(hook: &Hook, findings: Vec<Finding>) -> HookReport {
    HookReport {
        id: hook.id.clone(),
        name: hook.name.clone(),
        outcome: Outcome::Failed,
        findings,
        skip_reason: None,
    }
}

fn unsupported_message(hook: &Hook, policy: &Policy) -> String {
    if hook.kind == HookOriginKind::Meta {
        return format!("hook `{}` (repo: meta) is not executed until v0.3", hook.id);
    }
    if let Some(language) = &hook.language {
        if (language == "unsupported" || language == "unsupported_script")
            && !policy.trusts_system(&hook.id)
        {
            return format!(
                "hook `{}` uses language `{language}`, which is refused unless listed in ahi.policy.yaml trusted_system_hooks",
                hook.id
            );
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
