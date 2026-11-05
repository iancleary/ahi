//! Optional `ahi.actions.yaml`: the per-repository policy for agent actions.
//!
//! Absence denies every agent action and disables tool advertisement. Only the
//! `tool_call` kind is implemented, and only for tools compiled into ahi; the
//! file selects and bounds builtin tools, it never defines or fetches one.
//! The file is operator policy. Model output, tool results, and transcript text
//! never write it and never create a grant.

use std::fs;
use std::path::{Component, Path};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::yaml_safe::check_config_yaml;

pub(crate) const FILE_NAME: &str = "ahi.actions.yaml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolRule {
    pub(crate) id: String,
    pub(crate) tool: String,
    /// Normalized workspace-relative prefix. Empty means the whole workspace.
    pub(crate) path: Option<String>,
    pub(crate) effect: Effect,
    pub(crate) ask: bool,
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
            if rule.effect == Effect::Approve && !tools.contains(&rule.tool) {
                tools.push(rule.tool.clone());
            }
        }
        tools
    }

    pub(crate) fn evaluate(&self, tool: &str, path: &str) -> Verdict {
        let matching = || {
            self.rules.iter().filter(move |rule| {
                rule.tool == tool
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
                && rule.tool == tool
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

/// Load `ahi.actions.yaml` from `root`. `Ok(None)` when the file is absent.
pub(crate) fn load(root: &Path, builtin_tools: &[&str]) -> Result<Option<ActionPolicy>> {
    let path = root.join(FILE_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)
        .map_err(|err| Error::new(format!("failed to read {FILE_NAME}: {err}")))?;
    parse(&text, FILE_NAME, builtin_tools).map(Some)
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMatch {
    tool: String,
    #[serde(default)]
    path: Option<String>,
}

const PLANNED_KINDS: [&str; 5] = [
    "command",
    "file_write",
    "vcs_push",
    "outbound_message",
    "pre_commit",
];

pub(crate) fn parse(src: &str, origin: &str, builtin_tools: &[&str]) -> Result<ActionPolicy> {
    check_config_yaml(src, origin)?;
    let raw: RawPolicy =
        serde_yaml::from_str(src).map_err(|err| Error::new(format!("{origin}: {err}")))?;
    if raw.version != 1 {
        return Err(Error::new(format!(
            "{origin}: {FILE_NAME} version {} is not supported (expected 1)",
            raw.version
        )));
    }
    if raw.default.as_deref().is_some_and(|value| value != "deny") {
        return Err(Error::new(format!(
            "{origin}: default must be deny; agent actions are deny-by-default"
        )));
    }
    let mut rules: Vec<ToolRule> = Vec::new();
    for rule in raw.rules {
        let id = rule.id;
        let fail = |message: String| Error::new(format!("{origin}: rule `{id}`: {message}"));
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(Error::new(format!(
                "{origin}: rule ids must be 1-64 ASCII letters, digits, '-', '_', or '.'"
            )));
        }
        if rules.iter().any(|existing| existing.id == id) {
            return Err(fail("duplicate rule id".into()));
        }
        if rule.action != "tool_call" {
            return Err(fail(if PLANNED_KINDS.contains(&rule.action.as_str()) {
                format!("action `{}` is not implemented yet", rule.action)
            } else {
                format!("unknown action `{}` (expected tool_call)", rule.action)
            }));
        }
        if !builtin_tools.contains(&rule.matcher.tool.as_str()) {
            return Err(fail(format!(
                "`{}` is not a builtin tool (expected one of: {})",
                rule.matcher.tool,
                builtin_tools.join(", ")
            )));
        }
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
            tool: rule.matcher.tool,
            path,
            effect: rule.effect,
            ask: rule.ask,
        });
    }
    Ok(ActionPolicy { rules })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS: [&str; 2] = ["read_file", "list_dir"];

    fn policy(src: &str) -> ActionPolicy {
        parse(src, "ahi.actions.yaml", &TOOLS).unwrap()
    }

    fn error(src: &str) -> String {
        parse(src, "ahi.actions.yaml", &TOOLS)
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
        assert_eq!(policy.evaluate("read_file", "src/main.rs"), approve("src"));
        assert_eq!(
            policy.evaluate("read_file", "src/secrets/key"),
            Verdict::Deny {
                rule_id: Some("secrets".into())
            }
        );
        assert_eq!(
            policy.evaluate("read_file", "srcx/main.rs"),
            Verdict::Ask {
                rule_id: "ask-all".into()
            }
        );
        assert_eq!(
            policy.evaluate("list_dir", ""),
            Verdict::Deny { rule_id: None }
        );
        // Case variants of a denied path stay denied, never approved.
        for variant in ["SRC/secrets/key", "src/SECRETS/key", "Src/Secrets"] {
            assert_eq!(
                policy.evaluate("read_file", variant),
                Verdict::Deny {
                    rule_id: Some("secrets".into())
                },
                "{variant}"
            );
        }
        assert_eq!(
            policy.evaluate("read_file", "SRC/main.rs"),
            Verdict::Ask {
                rule_id: "ask-all".into()
            }
        );
        assert_eq!(policy.advertised_tools(), ["read_file"]);
    }

    #[test]
    fn rejects_unknown_fields_kinds_tools_profiles_and_escaping_paths() {
        let rule = |body: &str| format!("version: 1\nrules:\n- {{id: r, {body}}}\n");
        for (src, expected) in [
            ("version: 2\n".to_string(), "version 2 is not supported"),
            ("version: 1\ndefault: approve\n".into(), "default must be deny"),
            ("version: 1\nextra: 1\n".into(), "unknown field `extra`"),
            (
                rule("action: command, match: {tool: read_file}, effect: approve"),
                "action `command` is not implemented yet",
            ),
            (
                rule("action: tool_call, match: {tool: bash}, effect: approve"),
                "`bash` is not a builtin tool",
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
}
