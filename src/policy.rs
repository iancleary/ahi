//! Optional `ahi.policy.yaml`. Absence means deny-by-default empty allowlists.
//! Upstream pre-commit does not read this file.
//!
//! `language: system` and `language: script` run only when `trusted_system_hooks`
//! contains the hook id and the exact `entry` string. A bare id does not authorize
//! a command. Remote hooks are not substituted from this file.

use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_yaml::Value;

use crate::error::{Error, Result};
use crate::yaml_safe::check_config_yaml;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedSystemHook {
    pub id: String,
    /// `None` is the legacy id-only form. It parses and does not authorize.
    pub entry: Option<String>,
    /// Extra toolchain location names for this pin. Names only.
    pub toolchain_env: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    pub repo_allow: Vec<String>,
    pub runtime_allow: Vec<String>,
    pub trusted_system_hooks: Vec<TrustedSystemHook>,
    pub network_hooks: Vec<String>,
    /// Extra toolchain location names for every local system/script hook.
    pub toolchain_env: Vec<String>,
    pub path_present: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemAuth {
    Allowed,
    Missing,
    /// The id is listed, but not with this exact entry.
    EntryMismatch,
}

impl Policy {
    pub fn authorize_system(&self, hook_id: &str, entry: &str) -> SystemAuth {
        let mut saw_id = false;
        for hook in &self.trusted_system_hooks {
            if hook.id != hook_id {
                continue;
            }
            saw_id = true;
            if hook.entry.as_deref() == Some(entry) {
                return SystemAuth::Allowed;
            }
        }
        if saw_id {
            SystemAuth::EntryMismatch
        } else {
            SystemAuth::Missing
        }
    }

    /// Global names plus the names on the pin that matches this id and entry.
    pub fn toolchain_names(&self, hook_id: &str, entry: &str) -> Vec<String> {
        let mut names = self.toolchain_env.clone();
        for hook in &self.trusted_system_hooks {
            if hook.id == hook_id && hook.entry.as_deref() == Some(entry) {
                names.extend(hook.toolchain_env.iter().cloned());
            }
        }
        names.sort();
        names.dedup();
        names
    }
}

pub fn load_policy(root: &Path) -> Result<Policy> {
    let path = root.join("ahi.policy.yaml");
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
    let raw: RawPolicy =
        serde_yaml::from_str(src).map_err(|err| Error::new(format!("{origin}: {err}")))?;
    if raw.version != 1 {
        return Err(Error::new(format!(
            "{origin}: ahi.policy.yaml version {} is not supported (expected 1)",
            raw.version
        )));
    }
    let mut trusted_system_hooks = Vec::new();
    for value in raw.trusted_system_hooks.unwrap_or_default() {
        trusted_system_hooks.push(parse_trusted(&value, origin)?);
    }
    let toolchain_env = parse_toolchain_names(
        &raw.toolchain_env.unwrap_or_default(),
        origin,
        "toolchain_env",
    )?;
    Ok(Policy {
        repo_allow: raw.repo_allow.unwrap_or_default(),
        runtime_allow: raw.runtime_allow.unwrap_or_default(),
        trusted_system_hooks,
        network_hooks: raw.network_hooks.unwrap_or_default(),
        toolchain_env,
        path_present: false,
    })
}

fn parse_toolchain_names(names: &[String], origin: &str, what: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for name in names {
        if let Some(err) = crate::local_hook::toolchain_name_error(name) {
            return Err(Error::new(format!("{origin}: {what}: {err}")));
        }
        if !out.iter().any(|existing| existing == name) {
            out.push(name.clone());
        }
    }
    Ok(out)
}

fn parse_trusted(value: &Value, origin: &str) -> Result<TrustedSystemHook> {
    match value {
        Value::String(id) => {
            if id.is_empty() {
                return Err(Error::new(format!(
                    "{origin}: trusted_system_hooks id cannot be empty"
                )));
            }
            Ok(TrustedSystemHook {
                id: id.clone(),
                entry: None,
                toolchain_env: Vec::new(),
            })
        }
        Value::Mapping(map) => {
            let mut id = None;
            let mut entry = None;
            let mut toolchain_env = Vec::new();
            for (key, item) in map {
                let Some(key) = key.as_str() else {
                    return Err(Error::new(format!(
                        "{origin}: trusted_system_hooks keys must be strings"
                    )));
                };
                match key {
                    "id" => {
                        id = Some(
                            item.as_str()
                                .ok_or_else(|| {
                                    Error::new(format!(
                                        "{origin}: trusted_system_hooks id must be a string"
                                    ))
                                })?
                                .to_string(),
                        );
                    }
                    "entry" => {
                        entry = Some(
                            item.as_str()
                                .ok_or_else(|| {
                                    Error::new(format!(
                                        "{origin}: trusted_system_hooks entry must be a string"
                                    ))
                                })?
                                .to_string(),
                        );
                    }
                    "toolchain_env" => {
                        let Some(values) = item.as_sequence() else {
                            return Err(Error::new(format!(
                                "{origin}: trusted_system_hooks toolchain_env must be a list of names"
                            )));
                        };
                        let mut names = Vec::new();
                        for value in values {
                            let Some(name) = value.as_str() else {
                                return Err(Error::new(format!(
                                    "{origin}: trusted_system_hooks toolchain_env values must be strings"
                                )));
                            };
                            names.push(name.to_string());
                        }
                        toolchain_env = parse_toolchain_names(&names, origin, "toolchain_env")?;
                    }
                    other => {
                        return Err(Error::new(format!(
                            "{origin}: trusted_system_hooks field `{other}` is not supported (expected id, entry, and toolchain_env)"
                        )));
                    }
                }
            }
            let Some(id) = id.filter(|id| !id.is_empty()) else {
                return Err(Error::new(format!(
                    "{origin}: trusted_system_hooks entry needs an id"
                )));
            };
            let Some(entry) = entry.filter(|entry| !entry.is_empty()) else {
                return Err(Error::new(format!(
                    "{origin}: trusted_system_hooks id `{id}` needs an exact entry string"
                )));
            };
            Ok(TrustedSystemHook {
                id,
                entry: Some(entry),
                toolchain_env,
            })
        }
        _ => Err(Error::new(format!(
            "{origin}: trusted_system_hooks values must be an id string or an id/entry mapping"
        ))),
    }
}

pub fn policy_snippet(hook_id: &str, entry: &str, language: &str) -> String {
    format!(
        "hook `{hook_id}` uses language `{language}` with entry {entry_quoted}, which is refused unless ahi.policy.yaml allows this id and exact entry.\nAdd:\n\ntrusted_system_hooks:\n  - id: {hook_id}\n    entry: {quoted}\n",
        entry_quoted = yaml_double_quote(entry),
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
    repo_allow: Option<Vec<String>>,
    #[serde(default)]
    runtime_allow: Option<Vec<String>>,
    #[serde(default)]
    trusted_system_hooks: Option<Vec<Value>>,
    #[serde(default)]
    network_hooks: Option<Vec<String>>,
    #[serde(default)]
    toolchain_env: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_entry_authorizes_and_bare_id_does_not() {
        let policy = parse_policy_str(
            "version: 1\ntrusted_system_hooks:\n- id: rust-linting\n  entry: cargo fmt --all --\n- rust-clippy\n",
            "policy",
        )
        .unwrap();
        assert_eq!(
            policy.authorize_system("rust-linting", "cargo fmt --all --"),
            SystemAuth::Allowed
        );
        assert_eq!(
            policy.authorize_system("rust-linting", "cargo fmt"),
            SystemAuth::EntryMismatch
        );
        assert_eq!(
            policy.authorize_system("rust-clippy", "cargo clippy"),
            SystemAuth::EntryMismatch
        );
        assert_eq!(
            policy.authorize_system("other", "true"),
            SystemAuth::Missing
        );
    }

    #[test]
    fn snippet_quotes_the_entry() {
        let text = policy_snippet("rust-linting", "cargo fmt --all --", "system");
        assert!(text.contains("id: rust-linting"));
        assert!(text.contains("entry: \"cargo fmt --all --\""));
        assert!(text.contains("language `system`"));
    }

    #[test]
    fn toolchain_names_merge_and_secrets_are_refused() {
        let policy = parse_policy_str(
            "version: 1\ntoolchain_env: [GOROOT]\ntrusted_system_hooks:\n- id: rust-linting\n  entry: cargo fmt --all --\n  toolchain_env: [GOPATH]\n",
            "policy",
        )
        .unwrap();
        assert_eq!(
            policy.toolchain_names("rust-linting", "cargo fmt --all --"),
            vec!["GOPATH".to_string(), "GOROOT".to_string()]
        );
        let secret = parse_policy_str("version: 1\ntoolchain_env: [GITHUB_TOKEN]\n", "policy");
        let err = secret.unwrap_err().to_string();
        assert!(err.contains("never passed through"), "{err}");
        let value = parse_policy_str(
            "version: 1\ntrusted_system_hooks:\n- id: rust-linting\n  entry: cargo fmt --all --\n  toolchain_env: ['CARGO_HOME=/tmp']\n",
            "policy",
        );
        let err = value.unwrap_err().to_string();
        assert!(err.contains("never values"), "{err}");
    }
}
