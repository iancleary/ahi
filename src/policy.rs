//! Optional `ahi.policy.yaml`. Absence means deny-by-default empty allowlists.
//! Upstream pre-commit does not read this file.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::yaml_safe::check_config_yaml;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    pub repo_allow: Vec<String>,
    pub runtime_allow: Vec<String>,
    pub trusted_system_hooks: Vec<String>,
    pub network_hooks: Vec<String>,
}

impl Policy {
    pub fn trusts_system(&self, hook_id: &str) -> bool {
        self.trusted_system_hooks.iter().any(|id| id == hook_id)
    }
}

pub fn load_policy(root: &Path) -> Result<Policy> {
    let path = root.join("ahi.policy.yaml");
    if !path.is_file() {
        return Ok(Policy::default());
    }
    let text = fs::read_to_string(&path)
        .map_err(|err| Error::new(format!("failed to read {}: {err}", path.display())))?;
    parse_policy_str(&text, &path.display().to_string())
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
    Ok(Policy {
        repo_allow: raw.repo_allow.unwrap_or_default(),
        runtime_allow: raw.runtime_allow.unwrap_or_default(),
        trusted_system_hooks: raw.trusted_system_hooks.unwrap_or_default(),
        network_hooks: raw.network_hooks.unwrap_or_default(),
    })
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
    trusted_system_hooks: Option<Vec<String>>,
    #[serde(default)]
    network_hooks: Option<Vec<String>>,
}
