//! Best-effort suggestion of an ahi config from a pre-commit config.
//!
//! `ahi migrate` prints a report and a suggested document. It does not
//! guarantee feature parity or the same file scope. `ahi run` does not read
//! this table and does not substitute remote hooks. The command is not
//! upstream `migrate-config` (it does not rewrite `sha`, `python_venv`, or
//! legacy stage names).

use serde_yaml::{Mapping, Value};

use crate::error::{Error, Result};
use crate::policy;
use crate::yaml_safe::check_config_yaml;

#[derive(Debug, Clone, Copy)]
struct Equivalent {
    upstream_id: &'static str,
    builtin_id: &'static str,
    /// Report text only. A fixer still rewrites only when `ahi run --fix`
    /// (or a per-hook `--fix` argument) is set.
    fixer: bool,
}

/// Ids with an obvious offline builtin. Edit this table; nothing else reads it.
fn equivalents() -> &'static [Equivalent] {
    const HOOKS: &[Equivalent] = &[
        Equivalent {
            upstream_id: "check-added-large-files",
            builtin_id: "ahi/large-files",
            fixer: false,
        },
        Equivalent {
            upstream_id: "check-case-conflict",
            builtin_id: "ahi/case-conflict",
            fixer: false,
        },
        Equivalent {
            upstream_id: "check-executables-have-shebangs",
            builtin_id: "ahi/shebang-executable",
            fixer: false,
        },
        Equivalent {
            upstream_id: "check-illegal-windows-names",
            builtin_id: "ahi/windows-reserved-names",
            fixer: false,
        },
        Equivalent {
            upstream_id: "check-json",
            builtin_id: "ahi/check-json",
            fixer: false,
        },
        Equivalent {
            upstream_id: "check-merge-conflict",
            builtin_id: "ahi/merge-conflict",
            fixer: false,
        },
        Equivalent {
            upstream_id: "check-shebang-scripts-are-executable",
            builtin_id: "ahi/executable-shebang",
            fixer: false,
        },
        Equivalent {
            upstream_id: "check-symlinks",
            builtin_id: "ahi/symlinks",
            fixer: false,
        },
        Equivalent {
            upstream_id: "check-toml",
            builtin_id: "ahi/check-toml",
            fixer: false,
        },
        Equivalent {
            upstream_id: "check-vcs-permalinks",
            builtin_id: "ahi/vcs-permalinks",
            fixer: false,
        },
        Equivalent {
            upstream_id: "check-xml",
            builtin_id: "ahi/check-xml",
            fixer: false,
        },
        Equivalent {
            upstream_id: "check-yaml",
            builtin_id: "ahi/check-yaml",
            fixer: false,
        },
        Equivalent {
            upstream_id: "detect-aws-credentials",
            builtin_id: "ahi/aws-credentials",
            fixer: false,
        },
        Equivalent {
            upstream_id: "detect-private-key",
            builtin_id: "ahi/private-key",
            fixer: false,
        },
        Equivalent {
            upstream_id: "end-of-file-fixer",
            builtin_id: "ahi/end-of-file",
            fixer: true,
        },
        Equivalent {
            upstream_id: "fix-byte-order-marker",
            builtin_id: "ahi/byte-order-marker",
            fixer: false,
        },
        Equivalent {
            upstream_id: "mixed-line-ending",
            builtin_id: "ahi/line-endings",
            fixer: true,
        },
        Equivalent {
            upstream_id: "no-commit-to-branch",
            builtin_id: "ahi/no-commit-to-branch",
            fixer: false,
        },
        Equivalent {
            upstream_id: "trailing-whitespace",
            builtin_id: "ahi/trailing-whitespace",
            fixer: true,
        },
    ];
    HOOKS
}

fn equivalent(id: &str) -> Option<&'static Equivalent> {
    equivalents().iter().find(|hook| hook.upstream_id == id)
}

const HEADER: &str = "\
# Suggested by `ahi migrate`.
# Best-effort only. This does not guarantee feature parity or the same file scope.
# Remote repositories were not fetched. `ahi run` does not substitute hook ids.
# End-of-file, trailing-whitespace, and mixed line endings rewrite only with `ahi run --fix`.
";

const TOP_LEVEL: &[&str] = &[
    "default_install_hook_types",
    "default_language_version",
    "default_stages",
    "files",
    "exclude",
    "fail_fast",
    "minimum_pre_commit_version",
    "ci",
];

const BUILTIN_COPY: &[&str] = &[
    "alias",
    "name",
    "files",
    "exclude",
    "types",
    "types_or",
    "exclude_types",
    "args",
    "stages",
    "always_run",
    "fail_fast",
    "pass_filenames",
    "verbose",
    "require_serial",
    "description",
    "log_file",
    "minimum_pre_commit_version",
];

const LOCAL_COPY: &[&str] = &[
    "id",
    "alias",
    "name",
    "entry",
    "language",
    "language_version",
    "files",
    "exclude",
    "types",
    "types_or",
    "exclude_types",
    "args",
    "stages",
    "additional_dependencies",
    "always_run",
    "fail_fast",
    "pass_filenames",
    "description",
    "log_file",
    "require_serial",
    "verbose",
    "minimum_pre_commit_version",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub report: String,
    pub suggested: String,
}

pub fn suggest(src: &str, origin: &str) -> Result<Suggestion> {
    check_config_yaml(src, origin)?;
    let value: Value =
        serde_yaml::from_str(src).map_err(|err| Error::new(format!("{origin}: {err}")))?;
    let Some(root) = value.as_mapping() else {
        return Ok(Suggestion {
            report: format!(
                "ahi migrate is a best-effort aid. It does not guarantee feature parity or the same file scope.\n\nNot ported:\n- {origin}: the document is not a mapping. Upstream migrate-config would wrap a top-level list in `repos:`. ahi migrate does not rewrite that shape.\n"
            ),
            suggested: format!("{HEADER}repos: []\n"),
        });
    };
    let mut builtin_hooks = Vec::new();
    let mut local_hooks = Vec::new();
    let mut builtin_notes = Vec::new();
    let mut local_notes = Vec::new();
    let mut skipped = Vec::new();
    let repos_key = Value::String("repos".to_string());
    let Some(repos) = root.get(&repos_key).and_then(Value::as_sequence) else {
        return Err(Error::new(format!("{origin}: config has no repos")));
    };
    for repo in repos {
        let Some(map) = repo.as_mapping() else {
            skipped.push("- a repo entry is not a mapping".to_string());
            continue;
        };
        classify_repo(
            map,
            &mut builtin_hooks,
            &mut local_hooks,
            &mut builtin_notes,
            &mut local_notes,
            &mut skipped,
        );
    }
    let suggested = render_suggested(root, &builtin_hooks, &local_hooks)?;
    let report = render_report(&builtin_notes, &local_notes, &skipped);
    Ok(Suggestion { report, suggested })
}

fn classify_repo(
    map: &Mapping,
    builtin_hooks: &mut Vec<Value>,
    local_hooks: &mut Vec<Value>,
    builtin_notes: &mut Vec<String>,
    local_notes: &mut Vec<String>,
    skipped: &mut Vec<String>,
) {
    let url = mapping_str(map, "repo").unwrap_or_default();
    let rev = mapping_str(map, "rev").unwrap_or_default();
    let hooks = map
        .get(Value::String("hooks".to_string()))
        .and_then(Value::as_sequence);
    let Some(hooks) = hooks else {
        skipped.push(format!("- repo `{url}` has no hooks"));
        return;
    };
    match url.as_str() {
        "builtin" => {
            for hook in hooks {
                copy_builtin_already(hook, builtin_hooks, builtin_notes, skipped);
            }
        }
        "local" => {
            for hook in hooks {
                copy_local(hook, local_hooks, local_notes, skipped);
            }
        }
        "meta" => {
            for hook in hooks {
                let id = hook
                    .as_mapping()
                    .and_then(|hook| mapping_str(hook, "id"))
                    .unwrap_or_else(|| "(missing id)".to_string());
                skipped.push(format!("- `{id}` (repo: meta): not executed until v0.3"));
            }
        }
        _ => {
            if mapping_str(map, "sha").is_some() && rev.is_empty() {
                skipped.push(format!(
                    "- repo `{url}` uses `sha` instead of `rev`. ahi migrate does not perform that upstream migrate-config rewrite, and the repo was not fetched"
                ));
            }
            for hook in hooks {
                copy_remote(hook, &url, &rev, builtin_hooks, builtin_notes, skipped);
            }
        }
    }
}

fn copy_builtin_already(
    hook: &Value,
    builtin_hooks: &mut Vec<Value>,
    builtin_notes: &mut Vec<String>,
    skipped: &mut Vec<String>,
) {
    let Some(map) = hook.as_mapping() else {
        skipped.push("- a builtin hook entry is not a mapping".to_string());
        return;
    };
    let Some(id) = mapping_str(map, "id") else {
        skipped.push("- a builtin hook is missing id".to_string());
        return;
    };
    if let Some(stage) = legacy_stage_in(map) {
        skipped.push(format!(
            "- `{id}` uses legacy stage `{stage}`. ahi migrate does not rewrite legacy stages"
        ));
        return;
    }
    builtin_notes.push(format!(
        "- `{id}` is already `repo: builtin` and was copied"
    ));
    builtin_hooks.push(copy_fields(map, &id, BUILTIN_COPY));
}

fn copy_remote(
    hook: &Value,
    url: &str,
    rev: &str,
    builtin_hooks: &mut Vec<Value>,
    builtin_notes: &mut Vec<String>,
    skipped: &mut Vec<String>,
) {
    let Some(map) = hook.as_mapping() else {
        skipped.push(format!("- a hook from `{url}` is not a mapping"));
        return;
    };
    let Some(id) = mapping_str(map, "id") else {
        skipped.push(format!("- a hook from `{url}` is missing id"));
        return;
    };
    let rev_text = if rev.is_empty() {
        "(no rev)".to_string()
    } else {
        rev.to_string()
    };
    let Some(known) = equivalent(&id) else {
        skipped.push(format!(
            "- `{id}` from `{url}` rev `{rev_text}`: no offline builtin for this id. Remote repos are not fetched and are not executed until v0.2"
        ));
        return;
    };
    if let Some(stage) = legacy_stage_in(map) {
        skipped.push(format!(
            "- `{id}` uses legacy stage `{stage}`. ahi migrate does not rewrite legacy stages"
        ));
        return;
    }
    let fix = if known.fixer {
        " Rewrites only with `ahi run --fix` or a hook argument `--fix`."
    } else {
        " Report-only. `ahi run --fix` does not change this check."
    };
    builtin_notes.push(format!(
        "- `{id}` from `{url}` rev `{rev_text}` -> `{}`.{fix} The rev was not fetched.",
        known.builtin_id
    ));
    builtin_hooks.push(copy_fields(map, known.builtin_id, BUILTIN_COPY));
}

fn copy_local(
    hook: &Value,
    local_hooks: &mut Vec<Value>,
    local_notes: &mut Vec<String>,
    skipped: &mut Vec<String>,
) {
    let Some(map) = hook.as_mapping() else {
        skipped.push("- a local hook entry is not a mapping".to_string());
        return;
    };
    let Some(id) = mapping_str(map, "id") else {
        skipped.push("- a local hook is missing id".to_string());
        return;
    };
    let Some(language) = mapping_str(map, "language") else {
        skipped.push(format!("- `{id}` (repo: local) has no language"));
        return;
    };
    let Some(entry) = mapping_str(map, "entry") else {
        skipped.push(format!("- `{id}` (repo: local) has no entry"));
        return;
    };
    if let Some(stage) = legacy_stage_in(map) {
        skipped.push(format!(
            "- `{id}` uses legacy stage `{stage}`. ahi migrate does not rewrite legacy stages"
        ));
        return;
    }
    if !carries_local(&language) {
        skipped.push(format!(
            "- `{id}` (repo: local) language `{language}`: {}",
            language_reason(&language)
        ));
        return;
    }
    let written = match language.as_str() {
        "unsupported" => "system",
        "unsupported_script" => "script",
        other => other,
    };
    local_notes.push(format!(
        "- `{id}` language `{language}` can stay as `repo: local` when ahi.policy.yaml pins the id and the exact entry.\n{}",
        policy::policy_snippet(&id, &entry, written)
    ));
    local_hooks.push(copy_fields(map, &id, LOCAL_COPY));
}

fn carries_local(language: &str) -> bool {
    matches!(
        language,
        "system" | "script" | "unsupported" | "unsupported_script"
    )
}

fn language_reason(language: &str) -> &'static str {
    if crate::config::language_is_planned(language)
        && !matches!(language, "unsupported" | "unsupported_script")
    {
        "not supported until v0.3"
    } else {
        "not supported by ahi"
    }
}

fn legacy_stage_in(map: &Mapping) -> Option<String> {
    let stages = map
        .get(Value::String("stages".to_string()))
        .and_then(Value::as_sequence)?;
    for stage in stages {
        let Some(stage) = stage.as_str() else {
            continue;
        };
        if matches!(stage, "commit" | "push" | "merge-commit") {
            return Some(stage.to_string());
        }
    }
    None
}

fn copy_fields(map: &Mapping, id: &str, keys: &[&str]) -> Value {
    let mut out = Mapping::new();
    out.insert(
        Value::String("id".to_string()),
        Value::String(id.to_string()),
    );
    for key in keys {
        if *key == "id" {
            continue;
        }
        if let Some(value) = map.get(Value::String((*key).to_string())) {
            out.insert(Value::String((*key).to_string()), value.clone());
        }
    }
    Value::Mapping(out)
}

fn render_suggested(
    root: &Mapping,
    builtin_hooks: &[Value],
    local_hooks: &[Value],
) -> Result<String> {
    let mut out = Mapping::new();
    for key in TOP_LEVEL {
        if let Some(value) = root.get(Value::String((*key).to_string())) {
            out.insert(Value::String((*key).to_string()), value.clone());
        }
    }
    let mut repos = Vec::new();
    if !builtin_hooks.is_empty() {
        let mut repo = Mapping::new();
        repo.insert(
            Value::String("repo".to_string()),
            Value::String("builtin".to_string()),
        );
        repo.insert(
            Value::String("hooks".to_string()),
            Value::Sequence(builtin_hooks.to_vec()),
        );
        repos.push(Value::Mapping(repo));
    }
    if !local_hooks.is_empty() {
        let mut repo = Mapping::new();
        repo.insert(
            Value::String("repo".to_string()),
            Value::String("local".to_string()),
        );
        repo.insert(
            Value::String("hooks".to_string()),
            Value::Sequence(local_hooks.to_vec()),
        );
        repos.push(Value::Mapping(repo));
    }
    out.insert(Value::String("repos".to_string()), Value::Sequence(repos));
    let mut body = serde_yaml::to_string(&Value::Mapping(out))
        .map_err(|err| Error::new(format!("failed to render the suggested config: {err}")))?;
    if let Some(rest) = body.strip_prefix("---\n") {
        body = rest.to_string();
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    Ok(format!("{HEADER}{body}"))
}

fn render_report(builtin_notes: &[String], local_notes: &[String], skipped: &[String]) -> String {
    let mut out = String::from(
        "ahi migrate is a best-effort aid. It does not guarantee feature parity or the same file scope as the hooks it reads.\n\
It does not fetch remote repositories. `ahi run` does not use this mapping and does not substitute hook ids.\n\
The suggestion is a new document. Comments in the original config are not copied.\n\
Upstream migrate-config rewrites (`sha` to `rev`, `python_venv`, legacy stage names) are not performed.\n\n",
    );
    out.push_str("Builtin equivalents:\n");
    if builtin_notes.is_empty() {
        out.push_str("- (none)\n");
    } else {
        for note in builtin_notes {
            out.push_str(note);
            out.push('\n');
        }
    }
    out.push_str("\nLocal hooks carried over:\n");
    if local_notes.is_empty() {
        out.push_str("- (none)\n");
    } else {
        for note in local_notes {
            out.push_str(note);
            if !note.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out.push_str("\nNot ported:\n");
    if skipped.is_empty() {
        out.push_str("- (none)\n");
    } else {
        for note in skipped {
            out.push_str(note);
            out.push('\n');
        }
    }
    out.push_str("\nSuggested config:\n");
    out
}

fn mapping_str(map: &Mapping, key: &str) -> Option<String> {
    map.get(Value::String(key.to_string()))
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IAN: &str = "\
repos:
- repo: https://github.com/pre-commit/pre-commit-hooks
  rev: v6.0.0
  hooks:
  - id: check-yaml
  - id: end-of-file-fixer
  - id: trailing-whitespace
  - id: check-ast
- repo: local
  hooks:
  - id: rust-linting
    name: Rust linting
    entry: cargo fmt --all --
    language: system
    pass_filenames: true
    types: [file, rust]
  - id: rust-clippy
    name: Rust clippy
    entry: cargo clippy --all-targets --all-features -- -Dclippy::all
    language: system
    pass_filenames: false
    types: [file, rust]
  - id: local-py
    name: Local Python
    entry: python hook.py
    language: python
";

    #[test]
    fn ians_config_suggests_builtins_and_names_what_it_cannot_port() {
        let suggestion = suggest(IAN, ".pre-commit-config.yaml").unwrap();
        assert!(suggestion.report.contains("does not guarantee"));
        assert!(suggestion.report.contains("not fetched"));
        assert!(suggestion.report.contains("ahi/check-yaml"));
        assert!(suggestion.report.contains("ahi/end-of-file"));
        assert!(suggestion.report.contains("ahi/trailing-whitespace"));
        assert!(suggestion.report.contains("ahi run --fix"));
        assert!(suggestion.report.contains("id: rust-linting"));
        assert!(suggestion.report.contains("entry: \"cargo fmt --all --\""));
        assert!(suggestion.report.contains("check-ast"));
        assert!(suggestion.report.contains("not supported until v0.3"));
        assert!(suggestion.suggested.contains("repo: builtin"));
        assert!(suggestion.suggested.contains("repo: local"));
        assert!(suggestion.suggested.contains("rust-clippy"));
        assert!(!suggestion.suggested.contains("check-ast"));
        assert!(!suggestion
            .suggested
            .contains("https://github.com/pre-commit/pre-commit-hooks"));
        let parsed = crate::config::parse_config_str(&suggestion.suggested, "suggested").unwrap();
        assert!(parsed.repos.iter().any(|repo| repo.repo == "builtin"));
        assert!(parsed.repos.iter().any(|repo| repo.repo == "local"));
    }

    #[test]
    fn an_ahi_config_is_copied_and_not_treated_as_a_remote() {
        let src = "repos:\n- repo: builtin\n  hooks:\n  - id: ahi/check-yaml\n";
        let suggestion = suggest(src, "config").unwrap();
        assert!(suggestion.report.contains("already `repo: builtin`"));
        assert!(suggestion.suggested.contains("ahi/check-yaml"));
        assert!(suggestion.report.contains("Not ported:\n- (none)"));
    }
}
