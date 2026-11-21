//! Parse and load operator-authored tool policy.

use super::{
    normalize_relative_path, ActionKind, ActionPolicy, Effect, ToolRule, COMMAND_TOOL,
    DELEGATE_TOOL, FILE_NAME, RETIRED_FILE_NAME, SKILL_TOOL, WRITE_TOOLS,
};
use crate::error::{Error, Result};
use crate::yaml_safe::check_config_yaml;
use serde::Deserialize;
use std::{fs, path::Path};

/// Load `ahi.tools.yaml` from `root`. `Ok(None)` when the file is absent. The
/// retired `ahi.tools.yaml` is an error so a renamed allowlist is never
/// silently ignored.
pub(crate) fn load(root: &Path, builtin_tools: &[&str]) -> Result<Option<ActionPolicy>> {
    let retired = root.join(RETIRED_FILE_NAME);
    if retired.is_file() {
        return Err(Error::new(format!(
            "{} is no longer read; rename it to {FILE_NAME} (the rule grammar is unchanged)",
            retired.display()
        )));
    }
    let path = super::workspace_file(root);
    if root.join(FILE_NAME).symlink_metadata().is_ok()
        && root
            .join(super::LEGACY_FILE_NAME)
            .symlink_metadata()
            .is_ok()
    {
        return Err(Error::new(
            "both ahi.modes.yaml and ahi.tools.yaml exist; keep exactly one workspace policy",
        ));
    }
    match path.symlink_metadata() {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        _ => load_file(&path, builtin_tools).map(Some),
    }
}

/// Load the policy file the operator named (`ahi tui --tools`). Unlike the
/// working-directory lookup, a missing or unreadable file is an error: the
/// operator asked for this file, so its absence implies nothing.
pub(crate) fn load_file(path: &Path, builtin_tools: &[&str]) -> Result<ActionPolicy> {
    read(path, &path.display().to_string(), builtin_tools)
}

fn read(path: &Path, origin: &str, builtin_tools: &[&str]) -> Result<ActionPolicy> {
    let text = fs::read_to_string(path)
        .map_err(|err| Error::new(format!("failed to read {origin}: {err}")))?;
    parse(&text, origin, builtin_tools)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    version: u32,
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    id: String,
    action: String,
    #[serde(rename = "match")]
    matcher: RawMatch,
    effect: Effect,
    #[serde(default)]
    ask: bool,
    #[serde(default)]
    sandbox: Option<String>,
    /// `command` only.
    #[serde(default)]
    toolchain_env: Option<Vec<String>>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMatch {
    /// Required for `tool_call`; optional for `file_write` and `command`.
    #[serde(default)]
    tool: Option<String>,
    /// A prefix of the file, directory, or working directory.
    #[serde(default)]
    path: Option<String>,
    /// `command` only: the argv prefix, required.
    #[serde(default)]
    argv: Option<Vec<String>>,
    /// `delegate` only: the worker label, required.
    #[serde(default)]
    worker: Option<String>,
    #[serde(default)]
    local_profile: Option<String>,
    /// `skill` only: the qualified skill ID, required.
    #[serde(default)]
    skill: Option<String>,
    #[serde(default)]
    script: Option<String>,
}

const PLANNED_KINDS: [&str; 3] = ["vcs_push", "outbound_message", "pre_commit"];

pub(crate) fn parse(src: &str, origin: &str, builtin_tools: &[&str]) -> Result<ActionPolicy> {
    check_config_yaml(src, origin)?;
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    let version: Version = crate::yaml_safe::typed(src, origin)?;
    if version.version == 2 {
        let file: compact::Modes = crate::yaml_safe::typed(src, origin)?;
        return file.compile(origin, builtin_tools);
    }
    let raw: RawPolicy = crate::yaml_safe::typed(src, origin)?;
    compile_rules(raw, origin, builtin_tools)
}

mod compact;

fn compile_rules(raw: RawPolicy, origin: &str, builtin_tools: &[&str]) -> Result<ActionPolicy> {
    if raw.version != 1 {
        return Err(Error::new(format!(
            "{origin}: {FILE_NAME} version {} is not supported (expected 1 or 2)",
            raw.version
        )));
    }
    if raw.default.as_deref().is_some_and(|value| value != "deny") {
        return Err(Error::new(format!(
            "{origin}: default must be deny; tool calls are deny-by-default"
        )));
    }
    let mut rules: Vec<ToolRule> = Vec::new();
    for mut rule in raw.rules {
        let id = rule.id;
        let fail = |message: String| Error::new(format!("{origin}: rule `{id}`: {message}"));
        if !crate::precommit::policy::valid_rule_id(&id) {
            return Err(Error::new(format!(
                "{origin}: rule ids must be 1-64 ASCII letters, digits, '-', '_', or '.'"
            )));
        }
        if rules.iter().any(|existing| existing.id == id) {
            return Err(fail("duplicate rule id".into()));
        }
        if !matches!(rule.action.as_str(), "command" | "skill_command") {
            if rule.matcher.argv.is_some() {
                return Err(fail("match.argv applies only to action: command".into()));
            }
            if rule.toolchain_env.is_some() {
                return Err(fail("toolchain_env applies only to action: command".into()));
            }
        }
        if rule.action != "delegate" && rule.matcher.local_profile.is_some() {
            return Err(fail(
                "match.local_profile applies only to action: delegate".into(),
            ));
        }
        if rule.action != "delegate" && rule.matcher.worker.is_some() {
            return Err(fail("match.worker applies only to action: delegate".into()));
        }
        if !matches!(rule.action.as_str(), "skill" | "skill_command")
            && rule.matcher.skill.is_some()
        {
            return Err(fail(
                "match.skill applies only to action: skill or skill_command".into(),
            ));
        }
        if rule.action != "skill_command" && rule.matcher.script.is_some() {
            return Err(fail(
                "match.script applies only to action: skill_command".into(),
            ));
        }
        let mut argv = None;
        let mut skill_script = None;
        let mut toolchain_env = Vec::new();
        let (action, tools) = match rule.action.as_str() {
            "command" | "skill_command" => {
                if rule
                    .matcher
                    .tool
                    .as_deref()
                    .is_some_and(|tool| tool != COMMAND_TOOL)
                {
                    return Err(fail(format!(
                        "action: command governs `{COMMAND_TOOL}` only; omit match.tool"
                    )));
                }
                if rule.action == "skill_command" {
                    let skill = rule
                        .matcher
                        .skill
                        .take()
                        .ok_or_else(|| fail("match.skill is required".into()))?;
                    let script = rule
                        .matcher
                        .script
                        .take()
                        .ok_or_else(|| fail("match.script is required".into()))?;
                    if !matches!(skill.as_str(), "operator/*" | "repo/*")
                        && !crate::skills::valid_id(&skill)
                    {
                        return Err(fail(
                            "match.skill needs a scoped ID, operator/*, or repo/*".into(),
                        ));
                    }
                    if script != "scripts/**" && !crate::skills::valid_script(&script) {
                        return Err(fail(
                            "match.script needs an exact scripts/ path or scripts/**".into(),
                        ));
                    }
                    skill_script = Some((skill, script));
                }
                let prefix = rule.matcher.argv.take();
                if prefix.is_none() && skill_script.is_none() {
                    return Err(fail(
                        "match.argv is required for action: command (the argv prefix, as a list)"
                            .into(),
                    ));
                }
                if prefix.as_ref().is_some_and(|prefix| {
                    prefix.is_empty() || prefix.iter().any(|token| token.is_empty())
                }) {
                    return Err(fail(
                        "match.argv must list at least one non-empty token".into(),
                    ));
                }
                argv = prefix;
                toolchain_env = crate::precommit::policy::parse_toolchain_names(
                    &rule.toolchain_env.unwrap_or_default(),
                    origin,
                    &format!("rule `{id}`: toolchain_env"),
                )?;
                let kind = if skill_script.is_some() {
                    ActionKind::SkillCommand
                } else {
                    ActionKind::Command
                };
                (kind, vec![COMMAND_TOOL.to_string()])
            }
            "delegate" => {
                if rule
                    .matcher
                    .tool
                    .as_deref()
                    .is_some_and(|tool| tool != DELEGATE_TOOL)
                {
                    return Err(fail(format!(
                        "action: delegate governs `{DELEGATE_TOOL}` only; omit match.tool"
                    )));
                }
                if rule.matcher.path.is_some() {
                    return Err(fail(
                        "match.path does not apply to action: delegate; a worker is named by its label"
                            .into(),
                    ));
                }
                let selector=match (rule.matcher.worker.take(),rule.matcher.local_profile.take()) {
                    (Some(w),None) if crate::precommit::policy::valid_rule_id(&w)=>w,
                    (None,Some(p)) if p=="rex-scratch"=>crate::delegation::local::SELECTOR.to_owned(),
                    _=>return Err(fail("match.worker must be a valid remote label, or set only match.local_profile: rex-scratch".into()))
                };
                argv = Some(vec![selector]);
                (ActionKind::Delegate, vec![DELEGATE_TOOL.to_string()])
            }
            "skill" => {
                if rule
                    .matcher
                    .tool
                    .as_deref()
                    .is_some_and(|tool| tool != SKILL_TOOL)
                {
                    return Err(fail(format!(
                        "action: skill governs `{SKILL_TOOL}` only; omit match.tool"
                    )));
                }
                if rule.matcher.path.is_some() {
                    return Err(fail(
                        "match.path does not apply to action: skill; a skill is named".into(),
                    ));
                }
                let Some(skill) = rule.matcher.skill.take() else {
                    return Err(fail(
                        "match.skill is required for action: skill (repo/<skill-name> or operator/<skill-name>)".into(),
                    ));
                };
                if skill != "operator/*" {
                    crate::skills::parse_id(&skill).map_err(|reason| {
                        fail(format!(
                            "match.skill: {reason}; operator/* is also supported"
                        ))
                    })?;
                }
                argv = Some(vec![skill]);
                (ActionKind::Skill, vec![SKILL_TOOL.to_string()])
            }
            "tool_call" => {
                let Some(tool) = rule.matcher.tool else {
                    return Err(fail("match.tool is required for action: tool_call".into()));
                };
                if WRITE_TOOLS.contains(&tool.as_str()) {
                    return Err(fail(format!(
                        "`{tool}` changes files; govern it with action: file_write"
                    )));
                }
                if tool == COMMAND_TOOL {
                    return Err(fail(format!(
                        "`{tool}` runs a program; govern it with action: command"
                    )));
                }
                if tool == DELEGATE_TOOL {
                    return Err(fail(format!(
                        "`{tool}` hands a task to a worker; govern it with action: delegate"
                    )));
                }
                if tool == SKILL_TOOL {
                    return Err(fail(format!(
                        "`{tool}` reads a skill; govern it with action: skill"
                    )));
                }
                if !builtin_tools.contains(&tool.as_str()) {
                    let readers: Vec<&str> = builtin_tools
                        .iter()
                        .copied()
                        .filter(|t| {
                            !WRITE_TOOLS.contains(t) && *t != COMMAND_TOOL && *t != DELEGATE_TOOL
                        })
                        .collect();
                    return Err(fail(format!(
                        "`{tool}` is not a builtin tool (expected one of: {})",
                        readers.join(", ")
                    )));
                }
                (ActionKind::ToolCall, vec![tool])
            }
            "file_write" => {
                let tools = match rule.matcher.tool {
                    None => WRITE_TOOLS.iter().map(|t| t.to_string()).collect(),
                    Some(tool) if WRITE_TOOLS.contains(&tool.as_str()) => vec![tool],
                    Some(tool) => {
                        return Err(fail(format!(
                            "`{tool}` is not a file-writing tool (expected {}, or omit match.tool for both)",
                            WRITE_TOOLS.join(" or ")
                        )));
                    }
                };
                (ActionKind::FileWrite, tools)
            }
            other => {
                return Err(fail(if PLANNED_KINDS.contains(&other) {
                    format!("action `{other}` is not implemented yet")
                } else {
                    format!(
                        "unknown action `{other}` (expected tool_call, file_write, command, or delegate)"
                    )
                }));
            }
        };
        if rule.ask && rule.effect == Effect::Deny {
            return Err(fail("ask applies only to effect: approve".into()));
        }
        // Rules are written for the sandbox that will contain effects. Until a
        // profile beyond the in-process default exists, a wider one fails closed.
        if rule
            .sandbox
            .as_deref()
            .is_some_and(|value| value != "default")
        {
            return Err(fail(
                "only sandbox: default is implemented; other profiles are not available yet".into(),
            ));
        }
        let path = match rule.matcher.path {
            None => None,
            Some(path) => Some(normalize_relative_path(&path).ok_or_else(|| {
                fail("match.path must be relative to the repository, without `..`".into())
            })?),
        };
        rules.push(ToolRule {
            id,
            action,
            tools,
            path,
            argv,
            toolchain_env,
            skill_script,
            effect: rule.effect,
            ask: rule.ask,
        });
    }
    Ok(ActionPolicy {
        rules,
        ..Default::default()
    })
}
