//! Optional `ahi.tools.yaml`: which builtin tools the chat model may call, and where.
//!
//! Absence denies every tool call and disables tool advertisement. The file
//! shares its rule grammar with `ahi.hooks.yaml`. Only the
//! `tool_call` kind is implemented, and only for tools compiled into ahi; the
//! file selects and bounds builtin tools, it never defines or fetches one.
//! The file is operator policy. Model output, tool results, and transcript text
//! never write it and never create a grant.

use std::fs;
use std::path::{Component, Path};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::yaml_safe::check_config_yaml;

pub(crate) const FILE_NAME: &str = "ahi.tools.yaml";
/// The name this file had before 0.7.0. Its presence is an error, never silence.
pub(crate) const RETIRED_FILE_NAME: &str = "ahi.actions.yaml";

/// Builtin tools that change files. They are governed by `action: file_write`
/// rules, never by `tool_call`, so the filename states the trust boundary.
pub(crate) const WRITE_TOOLS: [&str; 2] = ["edit_file", "write_file"];
/// The builtin tool that runs a program. Governed by `action: command`
/// rules, matched on an argv prefix and a working directory.
pub(crate) const COMMAND_TOOL: &str = "run_command";
/// The builtin tool that hands a task to a worker. Governed by
/// `action: delegate` rules, matched on the worker's label.
pub(crate) const DELEGATE_TOOL: &str = "delegate";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionKind {
    ToolCall,
    FileWrite,
    Command,
    Delegate,
    /// `action: skill`: the model may read the named skill's text through
    /// `load_skill`. Matched on the name alone.
    Skill,
}

/// The builtin tool `action: skill` rules govern. The tool is advertised
/// only when an approved skill exists and validates; the runtime decides.
pub(crate) const SKILL_TOOL: &str = "load_skill";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolRule {
    pub(crate) id: String,
    pub(crate) action: ActionKind,
    /// The tools this rule governs. One for `tool_call`; one or both write
    /// tools for `file_write` (`match.tool` omitted means both).
    pub(crate) tools: Vec<String>,
    /// Normalized workspace-relative prefix. Empty means the whole workspace.
    /// For `command`, the working directory the command may run in.
    pub(crate) path: Option<String>,
    /// `command` only: the argv prefix the command must start with, compared
    /// token by token, exactly. For `delegate`, the one worker label the rule
    /// names, matched the same way against the call's label; for `skill`,
    /// the one skill name.
    pub(crate) argv: Option<Vec<String>>,
    /// `command` only: toolchain variable names passed through to the
    /// process in addition to the base set, as in `ahi.hooks.yaml`.
    pub(crate) toolchain_env: Vec<String>,
    pub(crate) effect: Effect,
    pub(crate) ask: bool,
}

impl ToolRule {
    fn governs(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }

    fn argv_matches(&self, argv: &[String]) -> bool {
        self.argv
            .as_deref()
            .is_none_or(|prefix| argv.len() >= prefix.len() && argv[..prefix.len()] == *prefix)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Effect {
    Approve,
    Deny,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ActionPolicy {
    pub(crate) rules: Vec<ToolRule>,
}

/// The verdict from declared rules alone. Operator grants are applied by the
/// caller between `Approve` and `Ask`; a matching deny rule beats any grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Approve { rule_id: String },
    Ask { rule_id: String },
    Deny { rule_id: Option<String> },
}

impl ActionPolicy {
    /// Builtin tools that at least one rule could approve, in rule order.
    pub(crate) fn advertised_tools(&self) -> Vec<String> {
        let mut tools: Vec<String> = Vec::new();
        for rule in &self.rules {
            if rule.effect == Effect::Approve {
                for tool in &rule.tools {
                    if !tools.contains(tool) {
                        tools.push(tool.clone());
                    }
                }
            }
        }
        tools
    }

    pub(crate) fn rule(&self, id: &str) -> Option<&ToolRule> {
        self.rules.iter().find(|rule| rule.id == id)
    }

    /// Skill names at least one rule could approve, in rule order.
    pub(crate) fn approved_skills(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for rule in &self.rules {
            if rule.action == ActionKind::Skill && rule.effect == Effect::Approve {
                if let Some(name) = rule.argv.as_ref().and_then(|argv| argv.first()) {
                    if !names.contains(name) {
                        names.push(name.clone());
                    }
                }
            }
        }
        names
    }

    /// The verdict for loading the skill called `name`: a deny rule matches
    /// without regard to case (`Deploy` is `deploy` to a deny), an approval
    /// only exactly, and a name no rule approves is denied.
    pub(crate) fn evaluate_skill(&self, name: &str) -> Verdict {
        let skill_rules = || {
            self.rules
                .iter()
                .filter(|rule| rule.action == ActionKind::Skill)
        };
        let named = |rule: &ToolRule| rule.argv.as_ref().and_then(|argv| argv.first().cloned());
        if let Some(rule) = skill_rules().find(|rule| {
            rule.effect == Effect::Deny && named(rule).is_some_and(|n| n.eq_ignore_ascii_case(name))
        }) {
            return Verdict::Deny {
                rule_id: Some(rule.id.clone()),
            };
        }
        let approving = |rule: &&ToolRule| {
            rule.effect == Effect::Approve && named(rule).is_some_and(|n| n == name)
        };
        if let Some(rule) = skill_rules().find(|rule| approving(rule) && !rule.ask) {
            return Verdict::Approve {
                rule_id: rule.id.clone(),
            };
        }
        if let Some(rule) = skill_rules().find(approving) {
            return Verdict::Ask {
                rule_id: rule.id.clone(),
            };
        }
        Verdict::Deny { rule_id: None }
    }

    /// `path` is the file, directory, or working directory the call names;
    /// `argv` is empty for every tool but `run_command`.
    pub(crate) fn evaluate(&self, tool: &str, path: &str, argv: &[String]) -> Verdict {
        let matching = || {
            self.rules.iter().filter(move |rule| {
                rule.governs(tool)
                    && rule.argv_matches(argv)
                    && rule
                        .path
                        .as_deref()
                        .is_none_or(|prefix| path_within(prefix, path))
            })
        };
        // Deny rules match without regard to case: macOS and Windows
        // filesystems usually resolve `SRC/x` and `src/x` to the same file, and
        // canonicalization keeps the requested spelling. Approvals stay exact,
        // so a differently cased path falls through to ask or default deny.
        let folded = path.to_lowercase();
        if let Some(rule) = self.rules.iter().find(|rule| {
            rule.effect == Effect::Deny
                && rule.governs(tool)
                && rule.argv_matches(argv)
                && rule
                    .path
                    .as_deref()
                    .is_none_or(|prefix| path_within(&prefix.to_lowercase(), &folded))
        }) {
            return Verdict::Deny {
                rule_id: Some(rule.id.clone()),
            };
        }
        if let Some(rule) = matching().find(|rule| rule.effect == Effect::Approve && !rule.ask) {
            return Verdict::Approve {
                rule_id: rule.id.clone(),
            };
        }
        if let Some(rule) = matching().find(|rule| rule.effect == Effect::Approve) {
            return Verdict::Ask {
                rule_id: rule.id.clone(),
            };
        }
        Verdict::Deny { rule_id: None }
    }
}

/// Component-wise prefix test on normalized relative paths.
pub(crate) fn path_within(prefix: &str, path: &str) -> bool {
    prefix.is_empty()
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Lexically normalize a workspace-relative path to `a/b/c` form.
///
/// Refuses absolute paths, `..`, backslashes, drive prefixes, NUL, control
/// characters, and invisible formatting characters that could make an
/// approval prompt show a different path than the one used. `.` and empty components are dropped, so `.` becomes `""`.
/// This is a policy key, not a containment proof: callers that touch the
/// filesystem must still check the canonical path.
pub(crate) fn normalize_relative_path(path: &str) -> Option<String> {
    if path.starts_with('/')
        || path.contains('\\')
        || path
            .chars()
            .any(|c| c.is_control() || is_invisible_format(c))
    {
        return None;
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        let mut components = Path::new(part).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(_)), None) => parts.push(part),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

/// Bidirectional controls, zero-width characters, and similar `Cf` code points.
fn is_invisible_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2069}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

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
    let path = root.join(FILE_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    read(&path, FILE_NAME, builtin_tools).map(Some)
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

#[derive(Deserialize)]
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
    /// `skill` only: the skill name, required.
    #[serde(default)]
    skill: Option<String>,
}

const PLANNED_KINDS: [&str; 3] = ["vcs_push", "outbound_message", "pre_commit"];

pub(crate) fn parse(src: &str, origin: &str, builtin_tools: &[&str]) -> Result<ActionPolicy> {
    check_config_yaml(src, origin)?;
    let raw: RawPolicy = crate::yaml_safe::typed(src, origin)?;
    if raw.version != 1 {
        return Err(Error::new(format!(
            "{origin}: {FILE_NAME} version {} is not supported (expected 1)",
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
        if !crate::hooks::policy::valid_rule_id(&id) {
            return Err(Error::new(format!(
                "{origin}: rule ids must be 1-64 ASCII letters, digits, '-', '_', or '.'"
            )));
        }
        if rules.iter().any(|existing| existing.id == id) {
            return Err(fail("duplicate rule id".into()));
        }
        if rule.action != "command" {
            if rule.matcher.argv.is_some() {
                return Err(fail("match.argv applies only to action: command".into()));
            }
            if rule.toolchain_env.is_some() {
                return Err(fail("toolchain_env applies only to action: command".into()));
            }
        }
        if rule.action != "delegate" && rule.matcher.worker.is_some() {
            return Err(fail("match.worker applies only to action: delegate".into()));
        }
        if rule.action != "skill" && rule.matcher.skill.is_some() {
            return Err(fail("match.skill applies only to action: skill".into()));
        }
        let mut argv = None;
        let mut toolchain_env = Vec::new();
        let (action, tools) = match rule.action.as_str() {
            "command" => {
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
                let Some(prefix) = rule.matcher.argv.take() else {
                    return Err(fail(
                        "match.argv is required for action: command (the argv prefix, as a list)"
                            .into(),
                    ));
                };
                if prefix.is_empty() || prefix.iter().any(|token| token.is_empty()) {
                    return Err(fail(
                        "match.argv must list at least one non-empty token".into(),
                    ));
                }
                argv = Some(prefix);
                toolchain_env = crate::hooks::policy::parse_toolchain_names(
                    &rule.toolchain_env.unwrap_or_default(),
                    origin,
                    &format!("rule `{id}`: toolchain_env"),
                )?;
                (ActionKind::Command, vec![COMMAND_TOOL.to_string()])
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
                let Some(worker) = rule.matcher.worker.take() else {
                    return Err(fail(
                        "match.worker is required for action: delegate (the worker's label)".into(),
                    ));
                };
                if !crate::hooks::policy::valid_rule_id(&worker) {
                    return Err(fail(
                        "match.worker is a label of 1-64 ASCII letters, digits, '-', '_', or '.'"
                            .into(),
                    ));
                }
                argv = Some(vec![worker]);
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
                        "match.skill is required for action: skill (the skill's name)".into(),
                    ));
                };
                if !crate::hooks::policy::valid_rule_id(&skill) {
                    return Err(fail(
                        "match.skill is a name of 1-64 ASCII letters, digits, '-', '_', or '.'"
                            .into(),
                    ));
                }
                if skill == "*" {
                    return Err(fail(
                        "match.skill names one skill; there is no wildcard".into(),
                    ));
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
            effect: rule.effect,
            ask: rule.ask,
        });
    }
    Ok(ActionPolicy { rules })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS: [&str; 7] = [
        "read_file",
        "list_dir",
        "find_files",
        "search_files",
        "edit_file",
        "write_file",
        "run_command",
    ];

    fn argv(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| t.to_string()).collect()
    }

    fn policy(src: &str) -> ActionPolicy {
        parse(src, "ahi.tools.yaml", &TOOLS).unwrap()
    }

    fn error(src: &str) -> String {
        parse(src, "ahi.tools.yaml", &TOOLS)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn deny_wins_then_approve_then_ask_then_default_deny() {
        let policy = policy(
            "version: 1\ndefault: deny\nrules:\n\
             - {id: ask-all, action: tool_call, match: {tool: read_file}, effect: approve, ask: true}\n\
             - {id: src, action: tool_call, match: {tool: read_file, path: ./src/}, effect: approve, sandbox: default}\n\
             - {id: secrets, action: tool_call, match: {tool: read_file, path: src/secrets}, effect: deny}\n",
        );
        let approve = |rule: &str| Verdict::Approve {
            rule_id: rule.into(),
        };
        assert_eq!(
            policy.evaluate("read_file", "src/main.rs", &[]),
            approve("src")
        );
        assert_eq!(
            policy.evaluate("read_file", "src/secrets/key", &[]),
            Verdict::Deny {
                rule_id: Some("secrets".into())
            }
        );
        assert_eq!(
            policy.evaluate("read_file", "srcx/main.rs", &[]),
            Verdict::Ask {
                rule_id: "ask-all".into()
            }
        );
        assert_eq!(
            policy.evaluate("list_dir", "", &[]),
            Verdict::Deny { rule_id: None }
        );
        // Case variants of a denied path stay denied, never approved.
        for variant in ["SRC/secrets/key", "src/SECRETS/key", "Src/Secrets"] {
            assert_eq!(
                policy.evaluate("read_file", variant, &[]),
                Verdict::Deny {
                    rule_id: Some("secrets".into())
                },
                "{variant}"
            );
        }
        assert_eq!(
            policy.evaluate("read_file", "SRC/main.rs", &[]),
            Verdict::Ask {
                rule_id: "ask-all".into()
            }
        );
        assert_eq!(policy.advertised_tools(), ["read_file"]);
    }

    #[test]
    fn file_write_rules_cover_one_or_both_write_tools_by_path() {
        let policy = policy(
            "version: 1\nrules:\n\
             - {id: edit-src, action: file_write, match: {tool: edit_file, path: src}, effect: approve}\n\
             - {id: new-docs, action: file_write, match: {tool: write_file, path: docs}, effect: approve, ask: true}\n\
             - {id: any-tests, action: file_write, match: {path: tests}, effect: approve}\n\
             - {id: no-lock, action: file_write, match: {path: Cargo.lock}, effect: deny}\n",
        );
        assert_eq!(
            policy.advertised_tools(),
            ["edit_file", "write_file"],
            "both advertised, in first-seen order"
        );
        assert_eq!(policy.rules[2].tools, ["edit_file", "write_file"]);
        assert_eq!(policy.rules[0].action, ActionKind::FileWrite);
        let approve = |rule: &str| Verdict::Approve {
            rule_id: rule.into(),
        };
        assert_eq!(
            policy.evaluate("edit_file", "src/main.rs", &[]),
            approve("edit-src")
        );
        assert_eq!(
            policy.evaluate("write_file", "src/main.rs", &[]),
            Verdict::Deny { rule_id: None },
            "edit-src governs edit_file only"
        );
        assert_eq!(
            policy.evaluate("write_file", "docs/new.md", &[]),
            Verdict::Ask {
                rule_id: "new-docs".into()
            }
        );
        assert_eq!(
            policy.evaluate("write_file", "tests/x.rs", &[]),
            approve("any-tests")
        );
        assert_eq!(
            policy.evaluate("edit_file", "tests/x.rs", &[]),
            approve("any-tests")
        );
        assert_eq!(
            policy.evaluate("edit_file", "Cargo.lock", &[]),
            Verdict::Deny {
                rule_id: Some("no-lock".into())
            }
        );
        assert_eq!(
            policy.evaluate("read_file", "src/main.rs", &[]),
            Verdict::Deny { rule_id: None },
            "write rules never approve reads"
        );
    }

    #[test]
    fn command_rules_match_an_argv_prefix_and_a_working_directory() {
        let policy = policy(
            "version: 1\nrules:\n\
             - {id: cargo-test, action: command, match: {argv: [cargo, test]}, effect: approve, toolchain_env: [CARGO_TARGET_DIR]}\n\
             - {id: cargo-any, action: command, match: {argv: [cargo]}, effect: approve, ask: true}\n\
             - {id: scripts, action: command, match: {argv: [sh], path: scripts}, effect: approve, ask: true}\n\
             - {id: no-publish, action: command, match: {argv: [cargo, publish]}, effect: deny}\n",
        );
        assert_eq!(policy.advertised_tools(), ["run_command"]);
        assert_eq!(policy.rules[0].action, ActionKind::Command);
        assert_eq!(policy.rules[0].toolchain_env, ["CARGO_TARGET_DIR"]);
        let approve = |rule: &str| Verdict::Approve {
            rule_id: rule.into(),
        };
        let ask = |rule: &str| Verdict::Ask {
            rule_id: rule.into(),
        };
        assert_eq!(
            policy.evaluate(
                "run_command",
                "",
                &argv(&["cargo", "test", "--all-features"])
            ),
            approve("cargo-test")
        );
        assert_eq!(
            policy.evaluate("run_command", "", &argv(&["cargo", "test"])),
            approve("cargo-test"),
            "the prefix itself matches"
        );
        assert_eq!(
            policy.evaluate("run_command", "", &argv(&["cargo", "build"])),
            ask("cargo-any")
        );
        assert_eq!(
            policy.evaluate("run_command", "", &argv(&["cargo"])),
            ask("cargo-any")
        );
        assert_eq!(
            policy.evaluate("run_command", "", &argv(&["cargo", "publish", "--dry-run"])),
            Verdict::Deny {
                rule_id: Some("no-publish".into())
            },
            "deny wins over the wider approve"
        );
        assert_eq!(
            policy.evaluate("run_command", "", &argv(&["cargotest"])),
            Verdict::Deny { rule_id: None },
            "tokens compare whole, not as string prefixes"
        );
        assert_eq!(
            policy.evaluate("run_command", "scripts/ci", &argv(&["sh", "run.sh"])),
            ask("scripts")
        );
        assert_eq!(
            policy.evaluate("run_command", "src", &argv(&["sh", "run.sh"])),
            Verdict::Deny { rule_id: None },
            "cwd outside the rule's path"
        );
        assert_eq!(
            policy.evaluate("read_file", "src/a.rs", &[]),
            Verdict::Deny { rule_id: None },
            "command rules never approve reads"
        );
        assert_eq!(policy.rule("cargo-any").map(|r| r.ask), Some(true));
    }

    #[test]
    fn rejects_unknown_fields_kinds_tools_profiles_and_escaping_paths() {
        let rule = |body: &str| format!("version: 1\nrules:\n- {{id: r, {body}}}\n");
        for (src, expected) in [
            ("version: 2\n".to_string(), "version 2 is not supported"),
            ("version: 1\ndefault: approve\n".into(), "default must be deny"),
            ("version: 1\nextra: 1\n".into(), "unknown field `extra`"),
            (
                rule("action: outbound_message, match: {tool: read_file}, effect: approve"),
                "action `outbound_message` is not implemented yet",
            ),
            (
                rule("action: tool_call, match: {tool: bash}, effect: approve"),
                "`bash` is not a builtin tool",
            ),
            (
                rule("action: tool_call, match: {tool: edit_file}, effect: approve"),
                "govern it with action: file_write",
            ),
            (
                rule("action: file_write, match: {tool: read_file}, effect: approve"),
                "is not a file-writing tool",
            ),
            (
                rule("action: tool_call, match: {path: src}, effect: approve"),
                "match.tool is required",
            ),
            (
                rule("action: vcs_push, match: {tool: read_file}, effect: approve"),
                "action `vcs_push` is not implemented yet",
            ),
            (
                rule("action: tool_call, match: {tool: run_command}, effect: approve"),
                "govern it with action: command",
            ),
            (
                rule("action: command, match: {path: src}, effect: approve"),
                "match.argv is required",
            ),
            (
                rule("action: command, match: {argv: []}, effect: approve"),
                "at least one non-empty token",
            ),
            (
                rule("action: command, match: {tool: read_file, argv: [ls]}, effect: approve"),
                "governs `run_command` only",
            ),
            (
                rule("action: tool_call, match: {tool: read_file, argv: [ls]}, effect: approve"),
                "match.argv applies only to action: command",
            ),
            (
                rule("action: file_write, match: {path: src}, effect: approve, toolchain_env: [CARGO_HOME]"),
                "toolchain_env applies only to action: command",
            ),
            (
                rule("action: command, match: {argv: [cargo]}, effect: approve, toolchain_env: [AWS_SECRET_ACCESS_KEY]"),
                "toolchain_env",
            ),
            (
                rule("action: tool_call, match: {tool: read_file}, effect: deny, ask: true"),
                "ask applies only to effect: approve",
            ),
            (
                rule("action: tool_call, match: {tool: read_file}, effect: approve, sandbox: docker"),
                "only sandbox: default is implemented",
            ),
            (
                rule("action: tool_call, match: {tool: read_file, path: ../up}, effect: approve"),
                "match.path must be relative",
            ),
            (
                rule("action: tool_call, match: {tool: read_file, path: /etc}, effect: approve"),
                "match.path must be relative",
            ),
            (
                rule("action: tool_call, match: {tool: read_file, glob: x}, effect: approve"),
                "unknown field `glob`",
            ),
            (
                "version: 1\nrules:\n- &a {id: r, action: tool_call, match: {tool: read_file}, effect: approve}\n".into(),
                "aliases and anchors are not allowed",
            ),
        ] {
            let message = error(&src);
            assert!(message.contains(expected), "{src}: {message}");
        }
    }

    #[test]
    fn normalization_is_lexical_and_refuses_escapes() {
        assert_eq!(normalize_relative_path(".").as_deref(), Some(""));
        assert_eq!(
            normalize_relative_path("./a//b/./c/").as_deref(),
            Some("a/b/c")
        );
        for bad in [
            "/etc",
            "a/../b",
            "..",
            "a\\b",
            "a\0b",
            "a\nb",
            "a\u{202E}b",
            "\u{200B}x",
        ] {
            assert_eq!(normalize_relative_path(bad), None, "{bad:?}");
        }
        assert!(path_within("", "anything"));
        assert!(path_within("src", "src"));
        assert!(path_within("src", "src/a"));
        assert!(!path_within("src", "srcx/a"));
    }

    #[test]
    fn missing_file_is_absent_policy() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("actions-missing");
        assert_eq!(load(&dir, &TOOLS).unwrap(), None);
        fs::write(
            dir.join(FILE_NAME),
            "version: 1\nrules:\n- {id: r, action: tool_call, match: {tool: list_dir}, effect: approve}\n",
        )
        .unwrap();
        assert_eq!(
            load(&dir, &TOOLS).unwrap().unwrap().advertised_tools(),
            ["list_dir"]
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn an_explicit_file_must_exist_and_the_examples_parse() {
        let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/tools");
        let mut seen = 0;
        for entry in std::fs::read_dir(&examples).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|ext| ext != "yaml") {
                continue;
            }
            seen += 1;
            let result = load_file(&path, &TOOLS);
            let name = path.file_name().unwrap().to_str().unwrap();
            if name == "invalid-version.yaml" {
                let error = result.unwrap_err().to_string();
                assert!(error.contains("version 2 is not supported"), "{error}");
                assert!(error.contains(name), "origin names the file: {error}");
            } else {
                let policy = result.unwrap_or_else(|err| panic!("{name}: {err}"));
                let advertised = policy.advertised_tools();
                if name == "deny-all.yaml" {
                    assert!(advertised.is_empty(), "{name}");
                } else {
                    assert!(!advertised.is_empty(), "{name}");
                }
            }
        }
        assert_eq!(
            seen, 10,
            "examples/tools changed; update this test and its README"
        );
        // The explicit lookup never treats a missing file as "no policy".
        let missing = examples.join("missing.yaml");
        let error = load_file(&missing, &TOOLS).unwrap_err().to_string();
        assert!(
            error.starts_with("failed to read") && error.contains("missing.yaml"),
            "{error}"
        );
        assert!(load(&examples, &TOOLS).unwrap().is_none());
    }
}

#[cfg(test)]
mod delegate_tests {
    use super::*;

    fn parse_rules(rules: &str) -> Result<ActionPolicy> {
        parse(
            &format!("version: 1\nrules:\n{rules}"),
            "test",
            &crate::chat::backend::tools::BUILTIN,
        )
    }

    #[test]
    fn delegate_rules_name_a_worker_label_and_nothing_else() {
        let policy = parse_rules(
            "- {id: ci, action: delegate, match: {worker: rust-ci}, effect: approve, ask: true}\n\
             - {id: docs, action: delegate, match: {worker: docs}, effect: approve}\n\
             - {id: no-prod, action: delegate, match: {worker: prod}, effect: deny}\n",
        )
        .unwrap();
        assert_eq!(policy.advertised_tools(), ["delegate"]);
        assert_eq!(policy.rules[0].action, ActionKind::Delegate);
        assert_eq!(policy.rules[0].argv, Some(vec!["rust-ci".to_string()]));
        let label = |name: &str| vec![name.to_string()];
        assert!(matches!(
            policy.evaluate(DELEGATE_TOOL, "", &label("rust-ci")),
            Verdict::Ask { rule_id } if rule_id == "ci"
        ));
        assert!(matches!(
            policy.evaluate(DELEGATE_TOOL, "", &label("docs")),
            Verdict::Approve { rule_id } if rule_id == "docs"
        ));
        assert!(matches!(
            policy.evaluate(DELEGATE_TOOL, "", &label("prod")),
            Verdict::Deny { rule_id: Some(rule_id) } if rule_id == "no-prod"
        ));
        assert!(matches!(
            policy.evaluate(DELEGATE_TOOL, "", &label("other")),
            Verdict::Deny { rule_id: None }
        ));
        for (bad, message) in [
            ("- {id: x, action: delegate, match: {}, effect: approve}\n", "match.worker is required"),
            ("- {id: x, action: delegate, match: {worker: 'a b'}, effect: approve}\n", "label"),
            ("- {id: x, action: delegate, match: {worker: w, path: src}, effect: approve}\n", "match.path does not apply"),
            ("- {id: x, action: tool_call, match: {tool: read_file, worker: w}, effect: approve}\n", "match.worker applies only"),
            ("- {id: x, action: tool_call, match: {tool: delegate}, effect: approve}\n", "govern it with action: delegate"),
        ] {
            let error = parse_rules(bad).unwrap_err().to_string();
            assert!(error.contains(message), "{bad}: {error}");
        }
    }

    #[test]
    fn skill_rules_name_a_skill_and_govern_load_skill() {
        let policy = parse_rules(
            "- {id: review, action: skill, match: {skill: rust-review}, effect: approve}\n\
             - {id: release, action: skill, match: {skill: release}, effect: approve, ask: true}\n\
             - {id: no-deploy, action: skill, match: {skill: deploy}, effect: deny}\n\
             - {id: twice, action: skill, match: {skill: rust-review}, effect: approve}\n",
        )
        .unwrap();
        assert_eq!(policy.advertised_tools(), ["load_skill"]);
        assert_eq!(policy.approved_skills(), ["rust-review", "release"]);
        assert_eq!(policy.rules[0].action, ActionKind::Skill);
        assert_eq!(policy.rules[0].argv, Some(vec!["rust-review".to_string()]));
        assert!(matches!(
            policy.evaluate_skill("rust-review"),
            Verdict::Approve { rule_id } if rule_id == "review"
        ));
        assert!(matches!(
            policy.evaluate_skill("release"),
            Verdict::Ask { rule_id } if rule_id == "release"
        ));
        assert!(matches!(
            policy.evaluate_skill("deploy"),
            Verdict::Deny { rule_id: Some(rule_id) } if rule_id == "no-deploy"
        ));
        assert!(
            matches!(
                policy.evaluate_skill("Deploy"),
                Verdict::Deny { rule_id: Some(rule_id) } if rule_id == "no-deploy"
            ),
            "deny ignores case"
        );
        assert!(
            matches!(
                policy.evaluate_skill("Rust-Review"),
                Verdict::Deny { rule_id: None }
            ),
            "approve is exact"
        );
        assert!(matches!(
            policy.evaluate_skill("other"),
            Verdict::Deny { rule_id: None }
        ));
        // Deny beats approve for the same name.
        let both = parse_rules(
            "- {id: yes, action: skill, match: {skill: x}, effect: approve}\n\
             - {id: no, action: skill, match: {skill: X}, effect: deny}\n",
        )
        .unwrap();
        assert!(matches!(
            both.evaluate_skill("x"),
            Verdict::Deny { rule_id: Some(rule_id) } if rule_id == "no"
        ));
        for (bad, message) in [
            ("- {id: x, action: skill, match: {}, effect: approve}\n", "match.skill is required"),
            ("- {id: x, action: skill, match: {skill: 'a b'}, effect: approve}\n", "name of 1-64"),
            ("- {id: x, action: skill, match: {skill: '*'}, effect: approve}\n", "name of 1-64"),
            ("- {id: x, action: skill, match: {skill: s, path: src}, effect: approve}\n", "match.path does not apply"),
            ("- {id: x, action: skill, match: {skill: s, tool: read_file}, effect: approve}\n", "omit match.tool"),
            ("- {id: x, action: skill, match: {skill: s, worker: w}, effect: approve}\n", "match.worker applies only"),
            ("- {id: x, action: tool_call, match: {tool: read_file, skill: s}, effect: approve}\n", "match.skill applies only"),
            ("- {id: x, action: delegate, match: {worker: w, skill: s}, effect: approve}\n", "match.skill applies only"),
            ("- {id: x, action: tool_call, match: {tool: load_skill}, effect: approve}\n", "govern it with action: skill"),
        ] {
            let error = parse_rules(bad).unwrap_err().to_string();
            assert!(error.contains(message), "{bad}: {error}");
        }
    }
}
