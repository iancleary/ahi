//! `.pre-commit-config.yaml` and `.pre-commit-hooks.yaml` parsing.
//!
//! Unknown keys, legacy stage names, and Python-only regular expressions are
//! errors. Upstream pre-commit warns for several of those.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::Deserialize;

use crate::check::builtin_by_id;
use crate::error::{Error, Result};
use crate::yaml_safe::check_config_yaml;
use crate::PRE_COMMIT_SCHEMA;

pub const CONFIG_NAMES: &[&str] = &[".pre-commit-config.yaml", ".pre-commit-config.yml"];
pub const MANIFEST_NAME: &str = ".pre-commit-hooks.yaml";

pub const HOOK_TYPES: &[&str] = &[
    "commit-msg",
    "post-checkout",
    "post-commit",
    "post-merge",
    "post-rewrite",
    "pre-commit",
    "pre-merge-commit",
    "pre-push",
    "pre-rebase",
    "prepare-commit-msg",
];

pub const STAGES: &[&str] = &[
    "commit-msg",
    "post-checkout",
    "post-commit",
    "post-merge",
    "post-rewrite",
    "pre-commit",
    "pre-merge-commit",
    "pre-push",
    "pre-rebase",
    "prepare-commit-msg",
    "manual",
];

const LANGUAGES: &[&str] = &[
    "conda",
    "coursier",
    "dart",
    "docker",
    "docker_image",
    "dotnet",
    "fail",
    "golang",
    "haskell",
    "julia",
    "lua",
    "node",
    "perl",
    "pygrep",
    "python",
    "r",
    "ruby",
    "rust",
    "swift",
    "unsupported",
    "unsupported_script",
];

/// Languages the v0.3 runner is planned to execute. v0.1 parses them and refuses to run them.
pub const PLANNED_LANGUAGES: &[&str] = &[
    "docker_image",
    "fail",
    "golang",
    "pygrep",
    "python",
    "rust",
    "unsupported",
    "unsupported_script",
];

const META_HOOKS: &[&str] = &["check-hooks-apply", "check-useless-excludes", "identity"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOriginKind {
    Builtin,
    Local,
    Meta,
    Remote,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub repos: Vec<Repo>,
    pub default_install_hook_types: Vec<String>,
    pub default_language_version: Vec<(String, String)>,
    pub default_stages: Vec<String>,
    pub files: String,
    pub files_re: Regex,
    pub exclude: String,
    pub exclude_re: Regex,
    pub fail_fast: bool,
    pub minimum_pre_commit_version: String,
    pub ci: Option<CiConfig>,
}

#[derive(Debug, Clone)]
pub struct CiConfig {
    pub autofix_commit_msg: String,
    pub autofix_prs: bool,
    pub autoupdate_branch: String,
    pub autoupdate_commit_msg: String,
    pub autoupdate_schedule: String,
    pub skip: Vec<String>,
    pub submodules: bool,
}

#[derive(Debug, Clone)]
pub struct Repo {
    pub repo: String,
    pub rev: Option<String>,
    pub kind: HookOriginKind,
    pub hooks: Vec<Hook>,
}

#[derive(Debug, Clone)]
pub struct Hook {
    pub id: String,
    pub alias: String,
    pub name: String,
    pub entry: String,
    pub language: Option<String>,
    pub files: String,
    pub files_re: Regex,
    pub exclude: String,
    pub exclude_re: Regex,
    pub types: Vec<String>,
    pub types_or: Vec<String>,
    pub exclude_types: Vec<String>,
    pub additional_dependencies: Vec<String>,
    pub args: Vec<String>,
    pub always_run: bool,
    pub fail_fast: bool,
    pub pass_filenames: bool,
    pub description: String,
    pub language_version: String,
    pub log_file: String,
    pub require_serial: bool,
    pub stages: Vec<String>,
    pub verbose: bool,
    pub minimum_pre_commit_version: String,
    pub kind: HookOriginKind,
    pub repo: String,
    pub rev: Option<String>,
    /// Config set `types` explicitly. Builtin defaults apply only when this is false.
    pub types_explicit: bool,
}

#[derive(Debug, Clone)]
pub struct ManifestHook {
    pub id: String,
    pub name: String,
    pub entry: String,
    pub language: String,
    pub alias: String,
    pub files: String,
    pub exclude: String,
    pub types: Vec<String>,
    pub types_or: Vec<String>,
    pub exclude_types: Vec<String>,
    pub description: String,
    pub stages: Vec<String>,
}

pub fn discover_config(root: &Path, explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if !path.is_file() {
            return Err(Error::new(format!(
                "config file not found: {}",
                path.display()
            )));
        }
        return Ok(path.to_path_buf());
    }
    let yaml = root.join(".pre-commit-config.yaml");
    let yml = root.join(".pre-commit-config.yml");
    match (yaml.is_file(), yml.is_file()) {
        (true, true) => Err(Error::new(
            "both .pre-commit-config.yaml and .pre-commit-config.yml exist; rename one",
        )),
        (true, false) => Ok(yaml),
        (false, true) => Ok(yml),
        (false, false) => Err(Error::new(
            "no .pre-commit-config.yaml found in the repository root",
        )),
    }
}

pub fn load_config(path: &Path) -> Result<Config> {
    let bytes = fs::read(path)
        .map_err(|err| Error::new(format!("failed to read {}: {err}", path.display())))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::new(format!("{}: config is not valid UTF-8", path.display())))?;
    parse_config_str(text, &path.display().to_string())
}

pub fn parse_config_str(src: &str, origin: &str) -> Result<Config> {
    check_config_yaml(src, origin)?;
    let raw: RawConfig =
        serde_yaml::from_str(src).map_err(|err| Error::new(format!("{origin}: {err}")))?;
    materialize_config(raw, origin)
}

pub fn parse_config_bytes(bytes: &[u8]) -> Result<Config> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| Error::new("<input>: config is not valid UTF-8"))?;
    parse_config_str(text, "<input>")
}

pub fn parse_manifest_str(src: &str, origin: &str) -> Result<Vec<ManifestHook>> {
    check_config_yaml(src, origin)?;
    let raw: Vec<RawManifestHook> =
        serde_yaml::from_str(src).map_err(|err| Error::new(format!("{origin}: {err}")))?;
    raw.into_iter()
        .map(|hook| materialize_manifest(hook, origin))
        .collect()
}

pub fn stage_selects_files(stage: &str) -> bool {
    !matches!(
        stage,
        "post-checkout" | "post-commit" | "post-merge" | "post-rewrite" | "pre-rebase"
    )
}

pub fn language_is_planned(language: &str) -> bool {
    PLANNED_LANGUAGES.contains(&language)
}

fn materialize_config(raw: RawConfig, origin: &str) -> Result<Config> {
    let minimum = raw
        .minimum_pre_commit_version
        .unwrap_or_else(|| "0".to_string());
    check_minimum(&minimum, origin)?;
    let default_stages = match raw.default_stages {
        Some(stages) => check_stages(&stages, origin, "default_stages")?,
        None => STAGES.iter().map(|stage| (*stage).to_string()).collect(),
    };
    let default_install_hook_types = match raw.default_install_hook_types {
        Some(types) => {
            for hook_type in &types {
                if !HOOK_TYPES.contains(&hook_type.as_str()) {
                    return Err(Error::new(format!(
                        "{origin}: default_install_hook_types value `{hook_type}` is not a hook type"
                    )));
                }
            }
            types
        }
        None => vec!["pre-commit".to_string()],
    };
    let mut default_language_version = Vec::new();
    if let Some(map) = raw.default_language_version {
        for (language, version) in map {
            let language = translate_language(&language);
            if !LANGUAGES.contains(&language.as_str()) {
                return Err(Error::new(format!(
                    "{origin}: default_language_version language `{language}` is not a pre-commit language"
                )));
            }
            default_language_version.push((language, version));
        }
    }
    let files = raw.files.unwrap_or_default();
    let exclude = raw.exclude.unwrap_or_else(|| "^$".to_string());
    let files_re = compile_regex(&files, origin, "files")?;
    let exclude_re = compile_regex(&exclude, origin, "exclude")?;
    let mut repos = Vec::new();
    for repo in raw.repos {
        repos.push(materialize_repo(repo, origin, &default_stages)?);
    }
    let ci = raw.ci.map(|ci| materialize_ci(ci, origin)).transpose()?;
    if let Some(ci) = &ci {
        let ids: Vec<&str> = repos
            .iter()
            .flat_map(|repo| repo.hooks.iter().flat_map(|hook| [&hook.id, &hook.alias]))
            .map(String::as_str)
            .filter(|id| !id.is_empty())
            .collect();
        for skip in &ci.skip {
            if !ids.contains(&skip.as_str()) {
                return Err(Error::new(format!(
                    "{origin}: ci.skip hook id `{skip}` is not configured"
                )));
            }
        }
    }
    Ok(Config {
        repos,
        default_install_hook_types,
        default_language_version,
        default_stages,
        files,
        files_re,
        exclude,
        exclude_re,
        fail_fast: raw.fail_fast.unwrap_or(false),
        minimum_pre_commit_version: minimum,
        ci,
    })
}

fn materialize_ci(raw: RawCi, origin: &str) -> Result<CiConfig> {
    let schedule = raw
        .autoupdate_schedule
        .unwrap_or_else(|| "weekly".to_string());
    if !matches!(schedule.as_str(), "weekly" | "monthly" | "quarterly") {
        return Err(Error::new(format!(
            "{origin}: ci.autoupdate_schedule `{schedule}` is not weekly, monthly, or quarterly"
        )));
    }
    let branch = raw.autoupdate_branch.unwrap_or_default();
    if branch == "pre-commit-ci-update-config" {
        return Err(Error::new(format!(
            "{origin}: ci.autoupdate_branch cannot be `{branch}`"
        )));
    }
    let autofix_commit_msg = raw.autofix_commit_msg.unwrap_or_else(|| {
        "[pre-commit.ci] auto fixes from pre-commit.com hooks\n\nfor more information, see https://pre-commit.ci\n".to_string()
    });
    if autofix_commit_msg.is_empty() {
        return Err(Error::new(format!(
            "{origin}: ci.autofix_commit_msg cannot be empty"
        )));
    }
    let autoupdate_commit_msg = raw
        .autoupdate_commit_msg
        .unwrap_or_else(|| "[pre-commit.ci] pre-commit autoupdate".to_string());
    if autoupdate_commit_msg.is_empty() {
        return Err(Error::new(format!(
            "{origin}: ci.autoupdate_commit_msg cannot be empty"
        )));
    }
    Ok(CiConfig {
        autofix_commit_msg,
        autofix_prs: raw.autofix_prs.unwrap_or(true),
        autoupdate_branch: branch,
        autoupdate_commit_msg,
        autoupdate_schedule: schedule,
        skip: raw.skip.unwrap_or_default(),
        submodules: raw.submodules.unwrap_or(false),
    })
}

fn materialize_repo(raw: RawRepo, origin: &str, default_stages: &[String]) -> Result<Repo> {
    let kind = match raw.repo.as_str() {
        "builtin" => HookOriginKind::Builtin,
        "local" => HookOriginKind::Local,
        "meta" => HookOriginKind::Meta,
        _ => HookOriginKind::Remote,
    };
    match kind {
        HookOriginKind::Remote => {
            let Some(rev) = raw.rev.as_ref() else {
                return Err(Error::new(format!(
                    "{origin}: repo `{}` is missing rev",
                    raw.repo
                )));
            };
            if rev.is_empty() {
                return Err(Error::new(format!(
                    "{origin}: repo `{}` has an empty rev",
                    raw.repo
                )));
            }
        }
        HookOriginKind::Builtin | HookOriginKind::Local | HookOriginKind::Meta => {
            if raw.rev.is_some() {
                return Err(Error::new(format!(
                    "{origin}: repo `{}` must not set rev",
                    raw.repo
                )));
            }
        }
    }
    let mut hooks = Vec::new();
    for hook in raw.hooks {
        hooks.push(materialize_hook(
            hook,
            origin,
            kind,
            &raw.repo,
            raw.rev.clone(),
            default_stages,
        )?);
    }
    Ok(Repo {
        repo: raw.repo,
        rev: raw.rev,
        kind,
        hooks,
    })
}

fn materialize_hook(
    raw: RawHook,
    origin: &str,
    kind: HookOriginKind,
    repo: &str,
    rev: Option<String>,
    default_stages: &[String],
) -> Result<Hook> {
    if raw.id.is_empty() {
        return Err(Error::new(format!("{origin}: hook id cannot be empty")));
    }
    let minimum = raw
        .minimum_pre_commit_version
        .unwrap_or_else(|| "0".to_string());
    check_minimum(&minimum, origin)?;
    let language = raw.language.as_deref().map(translate_language);
    if let Some(language) = &language {
        if !LANGUAGES.contains(&language.as_str()) {
            return Err(Error::new(format!(
                "{origin}: hook `{}` language `{language}` is not a pre-commit language",
                raw.id
            )));
        }
    }
    match kind {
        HookOriginKind::Builtin => {
            if builtin_by_id(&raw.id).is_none() {
                return Err(Error::new(format!(
                    "{origin}: builtin hook `{}` is not a ahi check",
                    raw.id
                )));
            }
            if raw.entry.is_some() || raw.language.is_some() {
                return Err(Error::new(format!(
                    "{origin}: builtin hook `{}` cannot set entry or language",
                    raw.id
                )));
            }
        }
        HookOriginKind::Meta => {
            if !META_HOOKS.contains(&raw.id.as_str()) {
                return Err(Error::new(format!(
                    "{origin}: meta hook `{}` is not one of {}",
                    raw.id,
                    META_HOOKS.join(", ")
                )));
            }
            if raw.entry.is_some() {
                return Err(Error::new(format!(
                    "{origin}: meta hook `{}` cannot override entry",
                    raw.id
                )));
            }
            if let Some(language) = &language {
                if language != "unsupported" {
                    return Err(Error::new(format!(
                        "{origin}: meta hook `{}` language must be unsupported",
                        raw.id
                    )));
                }
            }
        }
        HookOriginKind::Local => {
            if raw
                .name
                .as_ref()
                .map(|name| name.is_empty())
                .unwrap_or(true)
            {
                return Err(Error::new(format!(
                    "{origin}: local hook `{}` requires name",
                    raw.id
                )));
            }
            if raw
                .entry
                .as_ref()
                .map(|entry| entry.is_empty())
                .unwrap_or(true)
            {
                return Err(Error::new(format!(
                    "{origin}: local hook `{}` requires entry",
                    raw.id
                )));
            }
            if language.is_none() {
                return Err(Error::new(format!(
                    "{origin}: local hook `{}` requires language",
                    raw.id
                )));
            }
        }
        HookOriginKind::Remote => {}
    }
    let stages = match raw.stages {
        Some(stages) if !stages.is_empty() => check_stages(&stages, origin, &raw.id)?,
        _ => default_stages.to_vec(),
    };
    let files_explicit = raw.files.is_some();
    let mut files = raw.files.unwrap_or_default();
    let exclude = raw.exclude.unwrap_or_else(|| "^$".to_string());
    let types_explicit = raw.types.is_some();
    let mut types = raw.types.unwrap_or_else(|| vec!["file".to_string()]);
    if kind == HookOriginKind::Builtin {
        if let Some(check) = builtin_by_id(&raw.id) {
            if !types_explicit {
                types = check
                    .default_types()
                    .iter()
                    .map(|tag| (*tag).to_string())
                    .collect();
            }
            if !files_explicit && !check.default_files().is_empty() {
                files = check.default_files().to_string();
            }
        }
    }
    let files_re = compile_regex(&files, origin, &format!("hook `{}` files", raw.id))?;
    let exclude_re = compile_regex(&exclude, origin, &format!("hook `{}` exclude", raw.id))?;
    let types_or = raw.types_or.unwrap_or_default();
    let exclude_types = raw.exclude_types.unwrap_or_default();
    for tag in types
        .iter()
        .chain(types_or.iter())
        .chain(exclude_types.iter())
    {
        if !crate::identify::is_known_tag(tag) {
            return Err(Error::new(format!(
                "{origin}: hook `{}` type tag `{tag}` is not recognized",
                raw.id
            )));
        }
    }
    let name = if let Some(name) = raw.name {
        name
    } else if let Some(check) = builtin_by_id(&raw.id) {
        check.name().to_string()
    } else {
        raw.id.clone()
    };
    Ok(Hook {
        id: raw.id,
        alias: raw.alias.unwrap_or_default(),
        name,
        entry: raw.entry.unwrap_or_default(),
        language,
        files,
        files_re,
        exclude,
        exclude_re,
        types,
        types_or,
        exclude_types,
        additional_dependencies: raw.additional_dependencies.unwrap_or_default(),
        args: raw.args.unwrap_or_default(),
        always_run: raw.always_run.unwrap_or(false),
        fail_fast: raw.fail_fast.unwrap_or(false),
        pass_filenames: raw.pass_filenames.unwrap_or(true),
        description: raw.description.unwrap_or_default(),
        language_version: raw
            .language_version
            .unwrap_or_else(|| "default".to_string()),
        log_file: raw.log_file.unwrap_or_default(),
        require_serial: raw.require_serial.unwrap_or(false),
        stages,
        verbose: raw.verbose.unwrap_or(false),
        minimum_pre_commit_version: minimum,
        kind,
        repo: repo.to_string(),
        rev,
        types_explicit,
    })
}

fn materialize_manifest(raw: RawManifestHook, origin: &str) -> Result<ManifestHook> {
    if raw.id.is_empty() || raw.name.is_empty() || raw.entry.is_empty() {
        return Err(Error::new(format!(
            "{origin}: manifest hooks require id, name, and entry"
        )));
    }
    let language = translate_language(&raw.language);
    if !LANGUAGES.contains(&language.as_str()) {
        return Err(Error::new(format!(
            "{origin}: manifest hook `{}` language `{language}` is not a pre-commit language",
            raw.id
        )));
    }
    let minimum = raw
        .minimum_pre_commit_version
        .unwrap_or_else(|| "0".to_string());
    check_minimum(&minimum, origin)?;
    let stages = check_stages(&raw.stages.unwrap_or_default(), origin, &raw.id)?;
    let types = raw.types.unwrap_or_else(|| vec!["file".to_string()]);
    for tag in types
        .iter()
        .chain(raw.types_or.iter().flatten())
        .chain(raw.exclude_types.iter().flatten())
    {
        if !crate::identify::is_known_tag(tag) {
            return Err(Error::new(format!(
                "{origin}: manifest hook `{}` type tag `{tag}` is not recognized",
                raw.id
            )));
        }
    }
    let _ = compile_regex(raw.files.as_deref().unwrap_or(""), origin, "files")?;
    let _ = compile_regex(raw.exclude.as_deref().unwrap_or("^$"), origin, "exclude")?;
    Ok(ManifestHook {
        id: raw.id,
        name: raw.name,
        entry: raw.entry,
        language,
        alias: raw.alias.unwrap_or_default(),
        files: raw.files.unwrap_or_default(),
        exclude: raw.exclude.unwrap_or_else(|| "^$".to_string()),
        types,
        types_or: raw.types_or.unwrap_or_default(),
        exclude_types: raw.exclude_types.unwrap_or_default(),
        description: raw.description.unwrap_or_default(),
        stages,
    })
}

fn check_stages(stages: &[String], origin: &str, what: &str) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(stages.len());
    for stage in stages {
        if let Some(modern) = legacy_stage(stage) {
            return Err(Error::new(format!(
                "{origin}: `{what}` uses legacy stage `{stage}`; use `{modern}` (ahi migrate will rewrite this in a later version)"
            )));
        }
        if !STAGES.contains(&stage.as_str()) {
            return Err(Error::new(format!(
                "{origin}: `{what}` stage `{stage}` is not a pre-commit stage"
            )));
        }
        out.push(stage.clone());
    }
    Ok(out)
}

fn legacy_stage(stage: &str) -> Option<&'static str> {
    match stage {
        "commit" => Some("pre-commit"),
        "push" => Some("pre-push"),
        "merge-commit" => Some("pre-merge-commit"),
        _ => None,
    }
}

fn translate_language(language: &str) -> String {
    match language {
        "system" => "unsupported".to_string(),
        "script" => "unsupported_script".to_string(),
        other => other.to_string(),
    }
}

fn check_minimum(version: &str, origin: &str) -> Result<()> {
    let required = parse_version(version).map_err(|err| {
        Error::new(format!(
            "{origin}: minimum_pre_commit_version `{version}` is not a version ({err})"
        ))
    })?;
    let have = parse_version(PRE_COMMIT_SCHEMA).expect("schema version is dotted integers");
    if cmp_version(&required, &have) == std::cmp::Ordering::Greater {
        return Err(Error::new(format!(
            "{origin}: minimum_pre_commit_version {version} is newer than the schema ahi implements ({PRE_COMMIT_SCHEMA})"
        )));
    }
    Ok(())
}

fn parse_version(version: &str) -> std::result::Result<Vec<u64>, String> {
    if version.is_empty() {
        return Err("empty".to_string());
    }
    version
        .split('.')
        .map(|part| {
            part.parse::<u64>()
                .map_err(|_| format!("component `{part}` is not an integer"))
        })
        .collect()
}

fn cmp_version(left: &[u64], right: &[u64]) -> std::cmp::Ordering {
    let len = left.len().max(right.len());
    for index in 0..len {
        let l = left.get(index).copied().unwrap_or(0);
        let r = right.get(index).copied().unwrap_or(0);
        match l.cmp(&r) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

fn compile_regex(pattern: &str, origin: &str, what: &str) -> Result<Regex> {
    Regex::new(pattern).map_err(|err| {
        Error::new(format!(
            "{origin}: {what} regex `{pattern}` is not supported ({err}). ahi uses linear-time regular expressions and rejects lookahead, lookbehind, and backreferences"
        ))
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    repos: Vec<RawRepo>,
    #[serde(default)]
    default_install_hook_types: Option<Vec<String>>,
    #[serde(default)]
    default_language_version: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default)]
    default_stages: Option<Vec<String>>,
    #[serde(default)]
    files: Option<String>,
    #[serde(default)]
    exclude: Option<String>,
    #[serde(default)]
    fail_fast: Option<bool>,
    #[serde(default)]
    minimum_pre_commit_version: Option<String>,
    #[serde(default)]
    ci: Option<RawCi>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCi {
    #[serde(default)]
    autofix_commit_msg: Option<String>,
    #[serde(default)]
    autofix_prs: Option<bool>,
    #[serde(default)]
    autoupdate_branch: Option<String>,
    #[serde(default)]
    autoupdate_commit_msg: Option<String>,
    #[serde(default)]
    autoupdate_schedule: Option<String>,
    #[serde(default)]
    skip: Option<Vec<String>>,
    #[serde(default)]
    submodules: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepo {
    repo: String,
    #[serde(default)]
    rev: Option<String>,
    hooks: Vec<RawHook>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHook {
    id: String,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    entry: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    language_version: Option<String>,
    #[serde(default)]
    files: Option<String>,
    #[serde(default)]
    exclude: Option<String>,
    #[serde(default)]
    types: Option<Vec<String>>,
    #[serde(default)]
    types_or: Option<Vec<String>>,
    #[serde(default)]
    exclude_types: Option<Vec<String>>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    stages: Option<Vec<String>>,
    #[serde(default)]
    additional_dependencies: Option<Vec<String>>,
    #[serde(default)]
    always_run: Option<bool>,
    #[serde(default)]
    fail_fast: Option<bool>,
    #[serde(default)]
    pass_filenames: Option<bool>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    log_file: Option<String>,
    #[serde(default)]
    require_serial: Option<bool>,
    #[serde(default)]
    verbose: Option<bool>,
    #[serde(default)]
    minimum_pre_commit_version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct RawManifestHook {
    id: String,
    name: String,
    entry: String,
    language: String,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    files: Option<String>,
    #[serde(default)]
    exclude: Option<String>,
    #[serde(default)]
    types: Option<Vec<String>>,
    #[serde(default)]
    types_or: Option<Vec<String>>,
    #[serde(default)]
    exclude_types: Option<Vec<String>>,
    #[serde(default)]
    additional_dependencies: Option<Vec<String>>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    always_run: Option<bool>,
    #[serde(default)]
    fail_fast: Option<bool>,
    #[serde(default)]
    pass_filenames: Option<bool>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    language_version: Option<String>,
    #[serde(default)]
    log_file: Option<String>,
    #[serde(default)]
    require_serial: Option<bool>,
    #[serde(default)]
    stages: Option<Vec<String>>,
    #[serde(default)]
    verbose: Option<bool>,
    #[serde(default)]
    minimum_pre_commit_version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_builtin_config() {
        let cfg = parse_config_str(
            "repos:\n- repo: builtin\n  hooks:\n  - id: ahi/trailing-whitespace\n",
            "test",
        )
        .unwrap();
        assert_eq!(cfg.repos.len(), 1);
        assert_eq!(cfg.repos[0].hooks[0].id, "ahi/trailing-whitespace");
        assert!(cfg.repos[0].hooks[0]
            .stages
            .contains(&"pre-commit".to_string()));
        assert!(cfg.files_re.is_match("any"));
        assert!(!cfg.exclude_re.is_match("any"));
    }

    #[test]
    fn rejects_duplicate_keys_and_aliases() {
        let dup = parse_config_str("repos: []\nrepos: []\n", "test");
        assert!(dup.unwrap_err().to_string().contains("duplicate"));
        let alias = parse_config_str("repos: &a []\nhooks: *a\n", "test");
        assert!(alias.unwrap_err().to_string().contains("anchor"));
    }

    #[test]
    fn rejects_legacy_stage_and_lookahead() {
        let stage = parse_config_str("repos: []\ndefault_stages: [commit]\n", "test");
        assert!(stage.unwrap_err().to_string().contains("legacy stage"));
        let look = parse_config_str(
            "repos:\n- repo: builtin\n  hooks:\n  - id: ahi/end-of-file\n    files: 'foo(?=bar)'\n",
            "test",
        );
        assert!(look.unwrap_err().to_string().contains("regex"));
    }

    #[test]
    fn rejects_unknown_keys_and_future_schema() {
        let unknown = parse_config_str("repos: []\nextra: true\n", "test");
        assert!(unknown.unwrap_err().to_string().contains("extra"));
        let future = parse_config_str("minimum_pre_commit_version: '9.0.0'\nrepos: []\n", "test");
        assert!(future.unwrap_err().to_string().contains("newer"));
    }

    #[test]
    fn parses_manifest_and_translates_system() {
        let hooks = parse_manifest_str(
            "- id: example\n  name: Example\n  entry: example\n  language: system\n  types: [text]\n",
            "manifest",
        )
        .unwrap();
        assert_eq!(hooks[0].language, "unsupported");
        assert_eq!(hooks[0].types, vec!["text".to_string()]);
    }

    #[test]
    fn fuzz_parser_does_not_panic_on_samples() {
        for sample in ["", "[]", ":", "repos: [", "\u{0000}", "---\n---\n"] {
            let _ = parse_config_str(sample, "fuzz");
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(64))]

        #[test]
        fn parser_accepts_or_rejects_without_panicking(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..96)) {
            let _ = parse_config_bytes(&bytes);
        }
    }
}
