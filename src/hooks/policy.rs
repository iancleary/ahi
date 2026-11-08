//! Optional `ahi.hooks.yaml`: which local `system` and `script` hooks may run.
//! Absence means deny-by-default. Upstream pre-commit does not read this file.
//!
//! The file shares its rule grammar with `ahi.tools.yaml`: a `rules` list of
//! `{id, action, match, effect}`. Here `action` is `hook`, `match` names the
//! hook `id` and its exact `entry` string, and `effect` is `approve` or `deny`.
//! A hook runs only when an approve rule matches both the id and the exact
//! entry; a deny rule beats every approve rule. `ask` is not available for
//! hooks, since a pre-commit run has no operator to ask.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::yaml_safe::check_config_yaml;

pub const FILE_NAME: &str = "ahi.hooks.yaml";
/// The name this file had before 0.7.0. Its presence is an error, never silence.
pub const RETIRED_FILE_NAME: &str = "ahi.policy.yaml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Approve,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRule {
    /// The rule id, unique within the file.
    pub id: String,
    /// `match.id`: the hook id in `.pre-commit-config.yaml`.
    pub hook: String,
    /// `match.entry`: the exact entry string. Required for approve; a deny
    /// rule may omit it to deny every entry for that hook id.
    pub entry: Option<String>,
    pub effect: Effect,
    /// Extra toolchain location names for this rule. Names only.
    pub toolchain_env: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    pub rules: Vec<HookRule>,
    /// Extra toolchain location names for every local system/script hook.
    pub toolchain_env: Vec<String>,
    pub path_present: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemAuth {
    Allowed,
    /// No rule names this hook id.
    Missing,
    /// A rule names the id, but not with this exact entry.
    EntryMismatch,
    /// A deny rule matches.
    Denied,
}

impl Policy {
    pub fn authorize_system(&self, hook_id: &str, entry: &str) -> SystemAuth {
        let matches = |rule: &HookRule| {
            rule.hook == hook_id && rule.entry.as_deref().is_none_or(|e| e == entry)
        };
        if self
            .rules
            .iter()
            .any(|rule| rule.effect == Effect::Deny && matches(rule))
        {
            return SystemAuth::Denied;
        }
        let mut saw_id = false;
        for rule in self.rules.iter().filter(|r| r.effect == Effect::Approve) {
            if rule.hook != hook_id {
                continue;
            }
            saw_id = true;
            if rule.entry.as_deref() == Some(entry) {
                return SystemAuth::Allowed;
            }
        }
        if saw_id {
            SystemAuth::EntryMismatch
        } else {
            SystemAuth::Missing
        }
    }

    /// Rules that approve a hook, for `ahi doctor`.
    pub fn approved_hooks(&self) -> usize {
        self.rules
            .iter()
            .filter(|rule| rule.effect == Effect::Approve)
            .count()
    }

    /// Global names plus the names on the approve rule that matches this id and entry.
    pub fn toolchain_names(&self, hook_id: &str, entry: &str) -> Vec<String> {
        let mut names = self.toolchain_env.clone();
        for rule in &self.rules {
            if rule.effect == Effect::Approve
                && rule.hook == hook_id
                && rule.entry.as_deref() == Some(entry)
            {
                names.extend(rule.toolchain_env.iter().cloned());
            }
        }
        names.sort();
        names.dedup();
        names
    }
}

/// Load `ahi.hooks.yaml` from `root`. Absence is the empty, deny-by-default
/// policy. The retired `ahi.policy.yaml` is an error so a renamed allowlist is
/// never silently ignored.
pub fn load_policy(root: &Path) -> Result<Policy> {
    let retired = root.join(RETIRED_FILE_NAME);
    if retired.is_file() {
        return Err(Error::new(format!(
            "{} is no longer read; rename it to {FILE_NAME} and move each trusted hook into a rule: {{id, action: hook, match: {{id, entry}}, effect: approve}}",
            retired.display()
        )));
    }
    let path = root.join(FILE_NAME);
    if !path.is_file() {
        return Ok(Policy::default());
    }
    let text = fs::read_to_string(&path)
        .map_err(|err| Error::new(format!("failed to read {}: {err}", path.display())))?;
    let mut policy = parse_policy_str(&text, &path.display().to_string())?;
    policy.path_present = true;
    Ok(policy)
}

pub fn parse_policy_str(src: &str, origin: &str) -> Result<Policy> {
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
            "{origin}: default must be deny; local hooks are deny-by-default"
        )));
    }
    let toolchain_env = parse_toolchain_names(
        &raw.toolchain_env.unwrap_or_default(),
        origin,
        "toolchain_env",
    )?;
    let mut rules: Vec<HookRule> = Vec::new();
    for rule in raw.rules {
        let id = rule.id;
        let fail = |message: String| Error::new(format!("{origin}: rule `{id}`: {message}"));
        if !valid_rule_id(&id) {
            return Err(Error::new(format!(
                "{origin}: rule ids must be 1-64 ASCII letters, digits, '-', '_', or '.'"
            )));
        }
        if rules.iter().any(|existing| existing.id == id) {
            return Err(fail("duplicate rule id".into()));
        }
        if rule.action != "hook" {
            return Err(fail(format!(
                "unknown action `{}` (expected hook)",
                rule.action
            )));
        }
        if rule.ask {
            return Err(fail(
                "ask is not available for hooks; a pre-commit run has no one to ask".into(),
            ));
        }
        let hook = rule.matcher.id;
        if hook.is_empty() {
            return Err(fail("match.id needs the hook id".into()));
        }
        let entry = rule.matcher.entry.filter(|entry| !entry.is_empty());
        if rule.effect == Effect::Approve && entry.is_none() {
            return Err(fail(format!(
                "approving hook `{hook}` needs match.entry with the exact entry string; an id alone does not authorize a command"
            )));
        }
        let toolchain_env = parse_toolchain_names(
            &rule.toolchain_env.unwrap_or_default(),
            origin,
            "toolchain_env",
        )?;
        rules.push(HookRule {
            id,
            hook,
            entry,
            effect: rule.effect,
            toolchain_env,
        });
    }
    Ok(Policy {
        rules,
        toolchain_env,
        path_present: false,
    })
}

/// The rule id grammar shared with `ahi.tools.yaml`.
pub fn valid_rule_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

pub(crate) fn parse_toolchain_names(
    names: &[String],
    origin: &str,
    what: &str,
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for name in names {
        if let Some(err) = crate::hooks::local_hook::toolchain_name_error(name) {
            return Err(Error::new(format!("{origin}: {what}: {err}")));
        }
        if !out.iter().any(|existing| existing == name) {
            out.push(name.clone());
        }
    }
    Ok(out)
}

pub fn policy_snippet(hook_id: &str, entry: &str, language: &str) -> String {
    format!(
        "hook `{hook_id}` uses language `{language}` with entry {entry_quoted}, which is refused unless an {FILE_NAME} rule approves this id and exact entry.\nAdd:\n\nrules:\n{rule}",
        entry_quoted = yaml_double_quote(entry),
        rule = approve_rule(hook_id, entry),
    )
}

/// One approve rule for `rules:` in `ahi.hooks.yaml`, indented as a list
/// item under that key and ending with a newline.
pub fn approve_rule(hook_id: &str, entry: &str) -> String {
    format!(
        "  - id: {hook_id}\n    action: hook\n    match: {{id: {hook_id}, entry: {quoted}}}\n    effect: approve\n",
        quoted = yaml_double_quote(entry),
    )
}

fn yaml_double_quote(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    version: u32,
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    toolchain_env: Option<Vec<String>>,
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Debug, Deserialize)]
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
    toolchain_env: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMatch {
    id: String,
    #[serde(default)]
    entry: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, hook: &str, entry: &str, effect: &str) -> String {
        format!("- {{id: {id}, action: hook, match: {{id: {hook}, entry: \"{entry}\"}}, effect: {effect}}}\n")
    }

    #[test]
    fn exact_entry_authorizes_and_deny_beats_approve() {
        let src = format!(
            "version: 1\ndefault: deny\nrules:\n{}{}- {{id: no-fmt, action: hook, match: {{id: rust-fmt}}, effect: deny}}\n",
            rule("lint", "rust-linting", "cargo fmt --all --", "approve"),
            rule("fmt", "rust-fmt", "cargo fmt", "approve"),
        );
        let policy = parse_policy_str(&src, "hooks").unwrap();
        assert_eq!(
            policy.authorize_system("rust-linting", "cargo fmt --all --"),
            SystemAuth::Allowed
        );
        assert_eq!(
            policy.authorize_system("rust-linting", "cargo fmt"),
            SystemAuth::EntryMismatch
        );
        assert_eq!(
            policy.authorize_system("rust-fmt", "cargo fmt"),
            SystemAuth::Denied
        );
        assert_eq!(
            policy.authorize_system("other", "true"),
            SystemAuth::Missing
        );
        assert_eq!(policy.approved_hooks(), 2);
    }

    #[test]
    fn an_approve_rule_without_an_entry_is_refused() {
        let err = parse_policy_str(
            "version: 1\nrules:\n- {id: bare, action: hook, match: {id: rust-clippy}, effect: approve}\n",
            "hooks",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("exact entry string"), "{err}");
        let err = parse_policy_str(
            "version: 1\nrules:\n- {id: ask, action: hook, match: {id: a, entry: b}, effect: approve, ask: true}\n",
            "hooks",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ask is not available"), "{err}");
        let err = parse_policy_str(
            "version: 1\nrules:\n- {id: t, action: tool_call, match: {id: a, entry: b}, effect: approve}\n",
            "hooks",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("expected hook"), "{err}");
        let err = parse_policy_str("version: 1\ntrusted_system_hooks: []\n", "hooks")
            .unwrap_err()
            .to_string();
        assert!(err.contains("trusted_system_hooks"), "{err}");
    }

    #[test]
    fn snippet_quotes_the_entry() {
        let text = policy_snippet("rust-linting", "cargo fmt --all --", "system");
        assert!(text.contains("action: hook"));
        assert!(text.contains("match: {id: rust-linting, entry: \"cargo fmt --all --\"}"));
        assert!(text.contains("language `system`"));
    }

    #[test]
    fn toolchain_names_merge_and_secrets_are_refused() {
        let policy = parse_policy_str(
            "version: 1\ntoolchain_env: [GOROOT]\nrules:\n- {id: lint, action: hook, match: {id: rust-linting, entry: cargo fmt --all --}, effect: approve, toolchain_env: [GOPATH]}\n",
            "hooks",
        )
        .unwrap();
        assert_eq!(
            policy.toolchain_names("rust-linting", "cargo fmt --all --"),
            vec!["GOPATH".to_string(), "GOROOT".to_string()]
        );
        let secret = parse_policy_str("version: 1\ntoolchain_env: [GITHUB_TOKEN]\n", "hooks");
        let err = secret.unwrap_err().to_string();
        assert!(err.contains("never passed through"), "{err}");
        let value = parse_policy_str(
            "version: 1\nrules:\n- {id: lint, action: hook, match: {id: rust-linting, entry: cargo fmt --all --}, effect: approve, toolchain_env: ['CARGO_HOME=/tmp']}\n",
            "hooks",
        );
        let err = value.unwrap_err().to_string();
        assert!(err.contains("never values"), "{err}");
    }
}
