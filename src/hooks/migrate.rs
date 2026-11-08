//! Best-effort suggestion of an ahi config from a pre-commit config.
//!
//! `ahi migrate` prints a report and a suggested document. It does not
//! guarantee feature parity or the same file scope. `ahi run` does not read
//! this table and does not substitute remote hooks. The command is not
//! upstream `migrate-config` (it does not rewrite `sha`, `python_venv`, or
//! legacy stage names).

use yaml_rust2::yaml::{Hash, Yaml};
use yaml_rust2::YamlEmitter;

use crate::error::{Error, Result};
use crate::hooks::policy;
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

/// One upstream hook with an offline builtin, or a builtin copied as is.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BuiltinNote {
    id: String,
    builtin_id: String,
    detail: String,
}

/// One `repo: local` hook that ahi can run once a rule approves it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalNote {
    id: String,
    language: String,
    entry: String,
}

/// One hook, repo, or entry that is left out, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Skip {
    subject: String,
    reason: String,
}

#[derive(Debug, Default)]
struct Notes {
    builtin: Vec<BuiltinNote>,
    local: Vec<LocalNote>,
    skipped: Vec<Skip>,
}

impl Notes {
    fn skip(&mut self, subject: impl Into<String>, reason: impl Into<String>) {
        self.skipped.push(Skip {
            subject: subject.into(),
            reason: reason.into(),
        });
    }
}

pub fn suggest(src: &str, origin: &str) -> Result<Suggestion> {
    check_config_yaml(src, origin)?;
    let value = crate::yaml_safe::load_single(src, origin)?;
    let Some(root) = value.as_hash() else {
        let mut notes = Notes::default();
        notes.skip(
            origin,
            "the document is not a mapping. Upstream migrate-config wraps a top-level list in `repos:`; ahi migrate does not rewrite that shape",
        );
        return Ok(Suggestion {
            report: render_report(origin, &notes, false),
            suggested: format!("{HEADER}repos: []\n"),
        });
    };
    let mut notes = Notes::default();
    let mut builtin_hooks = Vec::new();
    let mut local_hooks = Vec::new();
    let repos_key = Yaml::String("repos".to_string());
    let Some(repos) = root.get(&repos_key).and_then(Yaml::as_vec) else {
        return Err(Error::new(format!("{origin}: config has no repos")));
    };
    for repo in repos {
        let Some(map) = repo.as_hash() else {
            notes.skip("a repo entry", "not a mapping");
            continue;
        };
        classify_repo(map, &mut builtin_hooks, &mut local_hooks, &mut notes);
    }
    let suggested = render_suggested(root, &builtin_hooks, &local_hooks)?;
    let report = render_report(origin, &notes, true);
    Ok(Suggestion { report, suggested })
}

fn classify_repo(
    map: &Hash,
    builtin_hooks: &mut Vec<Yaml>,
    local_hooks: &mut Vec<Yaml>,
    notes: &mut Notes,
) {
    let url = mapping_str(map, "repo").unwrap_or_default();
    let rev = mapping_str(map, "rev").unwrap_or_default();
    let hooks = map
        .get(&Yaml::String("hooks".to_string()))
        .and_then(Yaml::as_vec);
    let Some(hooks) = hooks else {
        notes.skip(format!("repo {url}"), "has no hooks");
        return;
    };
    match url.as_str() {
        "builtin" => {
            for hook in hooks {
                copy_builtin_already(hook, builtin_hooks, notes);
            }
        }
        "local" => {
            for hook in hooks {
                copy_local(hook, local_hooks, notes);
            }
        }
        "meta" => {
            for hook in hooks {
                let id = hook
                    .as_hash()
                    .and_then(|hook| mapping_str(hook, "id"))
                    .unwrap_or_else(|| "(missing id)".to_string());
                notes.skip(id, "repo: meta hooks are not executed by ahi");
            }
        }
        _ => {
            if mapping_str(map, "sha").is_some() && rev.is_empty() {
                notes.skip(
                    format!("repo {url}"),
                    "uses `sha` instead of `rev`; that upstream migrate-config rewrite is not performed and the repo was not fetched",
                );
            }
            for hook in hooks {
                copy_remote(hook, &url, &rev, builtin_hooks, notes);
            }
        }
    }
}

fn copy_builtin_already(hook: &Yaml, builtin_hooks: &mut Vec<Yaml>, notes: &mut Notes) {
    let Some(map) = hook.as_hash() else {
        notes.skip("a builtin hook entry", "not a mapping");
        return;
    };
    let Some(id) = mapping_str(map, "id") else {
        notes.skip("a builtin hook", "missing id");
        return;
    };
    if let Some(stage) = legacy_stage_in(map) {
        notes.skip(id, legacy_stage_reason(&stage));
        return;
    }
    notes.builtin.push(BuiltinNote {
        builtin_id: id.clone(),
        detail: "already repo: builtin; copied as is".to_string(),
        id: id.clone(),
    });
    builtin_hooks.push(copy_fields(map, &id, BUILTIN_COPY));
}

fn copy_remote(
    hook: &Yaml,
    url: &str,
    rev: &str,
    builtin_hooks: &mut Vec<Yaml>,
    notes: &mut Notes,
) {
    let Some(map) = hook.as_hash() else {
        notes.skip(format!("a hook from {url}"), "not a mapping");
        return;
    };
    let Some(id) = mapping_str(map, "id") else {
        notes.skip(format!("a hook from {url}"), "missing id");
        return;
    };
    let rev_text = if rev.is_empty() { "no rev" } else { rev };
    let Some(known) = equivalent(&id) else {
        notes.skip(
            id,
            format!(
                "no offline builtin for this id; remote hooks never run  ({} {rev_text})",
                short_repo(url)
            ),
        );
        return;
    };
    if let Some(stage) = legacy_stage_in(map) {
        notes.skip(id, legacy_stage_reason(&stage));
        return;
    }
    let fix = if known.fixer {
        "rewrites only with `ahi run --fix`"
    } else {
        "report-only"
    };
    notes.builtin.push(BuiltinNote {
        id: id.clone(),
        builtin_id: known.builtin_id.to_string(),
        detail: format!("{fix}  ({} {rev_text})", short_repo(url)),
    });
    builtin_hooks.push(copy_fields(map, known.builtin_id, BUILTIN_COPY));
}

fn copy_local(hook: &Yaml, local_hooks: &mut Vec<Yaml>, notes: &mut Notes) {
    let Some(map) = hook.as_hash() else {
        notes.skip("a local hook entry", "not a mapping");
        return;
    };
    let Some(id) = mapping_str(map, "id") else {
        notes.skip("a local hook", "missing id");
        return;
    };
    let Some(language) = mapping_str(map, "language") else {
        notes.skip(id, "repo: local hook has no language");
        return;
    };
    let Some(entry) = mapping_str(map, "entry") else {
        notes.skip(id, "repo: local hook has no entry");
        return;
    };
    if let Some(stage) = legacy_stage_in(map) {
        notes.skip(id, legacy_stage_reason(&stage));
        return;
    }
    if !carries_local(&language) {
        notes.skip(
            id,
            format!(
                "language {language} is not supported by ahi; only system and script run locally"
            ),
        );
        return;
    }
    let written = match language.as_str() {
        "unsupported" => "system",
        "unsupported_script" => "script",
        other => other,
    };
    notes.local.push(LocalNote {
        id: id.clone(),
        language: written.to_string(),
        entry,
    });
    local_hooks.push(copy_fields(map, &id, LOCAL_COPY));
}

fn legacy_stage_reason(stage: &str) -> String {
    format!("uses legacy stage `{stage}`; ahi migrate does not rewrite legacy stage names")
}

/// `https://github.com/pre-commit/pre-commit-hooks` -> `pre-commit/pre-commit-hooks`.
fn short_repo(url: &str) -> String {
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = trimmed.rsplit('/');
    match (parts.next(), parts.next()) {
        (Some(name), Some(owner)) if !owner.contains(':') && !owner.is_empty() => {
            format!("{owner}/{name}")
        }
        _ => trimmed.to_string(),
    }
}

fn carries_local(language: &str) -> bool {
    matches!(
        language,
        "system" | "script" | "unsupported" | "unsupported_script"
    )
}

fn legacy_stage_in(map: &Hash) -> Option<String> {
    let stages = map
        .get(&Yaml::String("stages".to_string()))
        .and_then(Yaml::as_vec)?;
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

fn copy_fields(map: &Hash, id: &str, keys: &[&str]) -> Yaml {
    let mut out = Hash::new();
    out.insert(Yaml::String("id".to_string()), Yaml::String(id.to_string()));
    for key in keys {
        if *key == "id" {
            continue;
        }
        if let Some(value) = map.get(&Yaml::String((*key).to_string())) {
            out.insert(Yaml::String((*key).to_string()), value.clone());
        }
    }
    Yaml::Hash(out)
}

fn render_suggested(root: &Hash, builtin_hooks: &[Yaml], local_hooks: &[Yaml]) -> Result<String> {
    let mut out = Hash::new();
    for key in TOP_LEVEL {
        if let Some(value) = root.get(&Yaml::String((*key).to_string())) {
            out.insert(Yaml::String((*key).to_string()), value.clone());
        }
    }
    let mut repos = Vec::new();
    if !builtin_hooks.is_empty() {
        let mut repo = Hash::new();
        repo.insert(
            Yaml::String("repo".to_string()),
            Yaml::String("builtin".to_string()),
        );
        repo.insert(
            Yaml::String("hooks".to_string()),
            Yaml::Array(builtin_hooks.to_vec()),
        );
        repos.push(Yaml::Hash(repo));
    }
    if !local_hooks.is_empty() {
        let mut repo = Hash::new();
        repo.insert(
            Yaml::String("repo".to_string()),
            Yaml::String("local".to_string()),
        );
        repo.insert(
            Yaml::String("hooks".to_string()),
            Yaml::Array(local_hooks.to_vec()),
        );
        repos.push(Yaml::Hash(repo));
    }
    out.insert(Yaml::String("repos".to_string()), Yaml::Array(repos));
    let mut body = String::new();
    YamlEmitter::new(&mut body)
        .dump(&Yaml::Hash(out))
        .map_err(|err| Error::new(format!("failed to render the suggested config: {err}")))?;
    body.push('\n');
    if let Some(rest) = body.strip_prefix("---\n") {
        body = rest.to_string();
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
    Ok(format!("{HEADER}{body}"))
}

/// Width of the widest `name` so the reason column lines up.
fn column_width<'a>(names: impl Iterator<Item = &'a str>) -> usize {
    names.map(str::len).max().unwrap_or(0)
}

fn section(out: &mut String, title: &str, count: usize) {
    out.push('\n');
    out.push_str(title);
    out.push_str(&format!(" ({count})\n"));
}

/// The report printed before the suggested config.
///
/// Four parts, each a titled list with aligned columns: hooks that map to a
/// builtin, local hooks kept as `repo: local`, what was not ported and why,
/// and the `ahi.hooks.yaml` rules the local hooks need. The caveats that
/// every reader must see fit in the three header lines.
fn render_report(origin: &str, notes: &Notes, parsed: bool) -> String {
    let mut out = format!(
        "ahi migrate: a best-effort suggestion from {origin}\n\
         This does not guarantee feature parity or the same file scope. Remote repositories and\n\
         revs are not fetched, comments are not copied, and `ahi run` does not use this mapping.\n"
    );
    if !parsed {
        section(&mut out, "Not ported", notes.skipped.len());
        push_skips(&mut out, &notes.skipped);
        return out;
    }

    section(&mut out, "Builtin equivalents", notes.builtin.len());
    if notes.builtin.is_empty() {
        out.push_str("  (none)\n");
    } else {
        let id_width = column_width(notes.builtin.iter().map(|n| n.id.as_str()));
        let builtin_width = column_width(notes.builtin.iter().map(|n| n.builtin_id.as_str()));
        for note in &notes.builtin {
            out.push_str(&format!(
                "  {:<id_width$}  -> {:<builtin_width$}  {}\n",
                note.id, note.builtin_id, note.detail
            ));
        }
    }

    section(
        &mut out,
        "Local hooks kept as repo: local",
        notes.local.len(),
    );
    if notes.local.is_empty() {
        out.push_str("  (none)\n");
    } else {
        let id_width = column_width(notes.local.iter().map(|n| n.id.as_str()));
        let language_width = column_width(notes.local.iter().map(|n| n.language.as_str()));
        for note in &notes.local {
            out.push_str(&format!(
                "  {:<id_width$}  {:<language_width$}  {}\n",
                note.id, note.language, note.entry
            ));
        }
    }

    section(&mut out, "Not ported", notes.skipped.len());
    if notes.skipped.is_empty() {
        out.push_str("  (none)\n");
    } else {
        push_skips(&mut out, &notes.skipped);
    }

    if !notes.local.is_empty() {
        out.push_str(&format!(
            "\nAdd to {} so the local hooks may run (deny by default; the id and exact entry must match):\n\n",
            policy::FILE_NAME
        ));
        out.push_str("rules:\n");
        for note in &notes.local {
            out.push_str(&policy::approve_rule(&note.id, &note.entry));
        }
    }

    out.push_str("\nSuggested config\n");
    out.push_str("----------------\n");
    out
}

fn push_skips(out: &mut String, skipped: &[Skip]) {
    let width = column_width(skipped.iter().map(|s| s.subject.as_str()));
    for skip in skipped {
        out.push_str(&format!("  {:<width$}  {}\n", skip.subject, skip.reason));
    }
}

fn mapping_str(map: &Hash, key: &str) -> Option<String> {
    map.get(&Yaml::String(key.to_string()))
        .and_then(Yaml::as_str)
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
        assert!(suggestion
            .report
            .contains("pre-commit/pre-commit-hooks v6.0.0"));
        assert!(suggestion.report.contains("ahi/check-yaml"));
        assert!(suggestion.report.contains("ahi/end-of-file"));
        assert!(suggestion.report.contains("ahi/trailing-whitespace"));
        assert!(suggestion.report.contains("ahi run --fix"));
        assert!(suggestion.report.contains("Add to ahi.hooks.yaml"));
        assert!(suggestion.report.contains("  - id: rust-linting\n"));
        assert_eq!(
            suggestion.report.matches("rules:").count(),
            1,
            "{}",
            suggestion.report
        );
        assert!(suggestion.report.contains("entry: \"cargo fmt --all --\""));
        assert!(suggestion.report.contains("check-ast"));
        assert!(suggestion.report.contains("not supported by ahi"));
        assert!(suggestion.suggested.contains("repo: builtin"));
        assert!(suggestion.suggested.contains("repo: local"));
        assert!(suggestion.suggested.contains("rust-clippy"));
        assert!(!suggestion.suggested.contains("check-ast"));
        assert!(!suggestion
            .suggested
            .contains("https://github.com/pre-commit/pre-commit-hooks"));
        let parsed =
            crate::hooks::config::parse_config_str(&suggestion.suggested, "suggested").unwrap();
        assert!(parsed.repos.iter().any(|repo| repo.repo == "builtin"));
        assert!(parsed.repos.iter().any(|repo| repo.repo == "local"));
    }

    #[test]
    fn an_ahi_config_is_copied_and_not_treated_as_a_remote() {
        let src = "repos:\n- repo: builtin\n  hooks:\n  - id: ahi/check-yaml\n";
        let suggestion = suggest(src, "config").unwrap();
        assert!(suggestion.report.contains("already repo: builtin"));
        assert!(suggestion.suggested.contains("ahi/check-yaml"));
        assert!(suggestion.report.contains("Not ported (0)\n  (none)"));
        assert!(!suggestion.report.contains("Add to"));
    }
}
