//! Optional `ahi.tools.yaml`: which builtin tools the chat model may call, and where.
//!
//! Absence denies every tool call and disables tool advertisement. The file
//! shares its rule grammar with `ahi.hooks.yaml`. Only the
//! `tool_call` kind is implemented, and only for tools compiled into ahi; the
//! file selects and bounds builtin tools, it never defines or fetches one.
//! The file is operator policy. Model output, tool results, and transcript text
//! never write it and never create a grant.

mod config;
mod evaluate;
mod mode;
pub(crate) use mode::ExecutionMode;
pub(crate) mod examples;
pub(crate) mod grants;

#[cfg(test)]
pub(crate) use config::parse;
pub(crate) use config::{load, load_file};

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

pub(crate) const FILE_NAME: &str = "ahi.modes.yaml";
pub(crate) const LEGACY_FILE_NAME: &str = "ahi.tools.yaml";

pub(crate) fn workspace_file(root: &Path) -> std::path::PathBuf {
    let primary = root.join(FILE_NAME);
    if primary.symlink_metadata().is_err() && root.join(LEGACY_FILE_NAME).symlink_metadata().is_ok()
    {
        root.join(LEGACY_FILE_NAME)
    } else {
        primary
    }
}
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) enum ActionKind {
    ToolCall,
    FileWrite,
    Command,
    SkillCommand,
    Delegate,
    /// `action: skill`: the model may read the named skill's text through
    /// `load_skill`. Matched on the name alone.
    Skill,
}

/// The builtin tool `action: skill` rules govern. The tool is advertised
/// only when an approved skill exists and validates; the runtime decides.
pub(crate) const SKILL_TOOL: &str = "load_skill";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
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
    /// Skill identity and script selectors for a skill-relative command.
    pub(crate) skill_script: Option<(String, String)>,
    /// `command` only: toolchain variable names passed through to the
    /// process in addition to the base set, as in `ahi.hooks.yaml`.
    pub(crate) toolchain_env: Vec<String>,
    pub(crate) effect: Effect,
    pub(crate) ask: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Effect {
    Approve,
    Deny,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct ActionPolicy {
    pub(crate) rules: Vec<ToolRule>,
    pub(crate) default_mode: Option<String>,
    pub(crate) named: std::collections::BTreeMap<String, Vec<ToolRule>>,
}

impl ActionPolicy {
    pub(crate) fn resolve(
        directory: &std::path::Path,
        explicit: Option<&std::path::Path>,
        mode: &ExecutionMode,
    ) -> crate::error::Result<Self> {
        let installed = match explicit {
            Some(path) => Some(load_file(path, &crate::tools::BUILTIN)?),
            None => load(directory, &crate::tools::BUILTIN)?,
        };
        Self::from_installed(installed, mode)
    }
    pub(crate) fn from_installed(
        installed: Option<Self>,
        mode: &ExecutionMode,
    ) -> crate::error::Result<Self> {
        match mode {
            ExecutionMode::ReadOnly => Ok(Self::plan()),
            ExecutionMode::ReadWrite | ExecutionMode::FullAccess => {
                let mut policy = Self::trusted();
                if let Some(installed) = installed {
                    policy.named = installed.named;
                }
                Ok(policy)
            }
            ExecutionMode::Policy => Ok(installed.unwrap_or_default()),
            ExecutionMode::Named(_) => installed
                .ok_or_else(|| crate::error::Error::new("workspace modes file missing"))?
                .select(mode),
        }
    }

    pub(crate) fn select(mut self, mode: &ExecutionMode) -> crate::error::Result<Self> {
        if let ExecutionMode::Named(name) = mode {
            self.rules = self.named.remove(name).ok_or_else(|| {
                crate::error::Error::new(format!("workspace mode `{name}` is unavailable"))
            })?;
        }
        Ok(self)
    }
}

/// The verdict from declared rules alone. Operator grants are applied by the
/// caller between `Approve` and `Ask`; a matching deny rule beats any grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Approve { rule_id: String },
    Ask { rule_id: String },
    Deny { rule_id: Option<String> },
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

/// Lexically normalize an absolute host path to `/a/b/c` form (on Windows,
/// `C:/a/b/c`). Only Full Access accepts these; every other mode never calls
/// this. Refuses relative paths, `..` components, backslashes on
/// Unix, NUL, control characters, and invisible formatting characters, for
/// the same reasons as `normalize_relative_path`. Duplicate separators are
/// collapsed. A policy key, not a containment proof: the caller still checks
/// the canonical path before touching the filesystem.
pub(crate) fn normalize_host_path(path: &str) -> Option<String> {
    if path
        .chars()
        .any(|c| c.is_control() || is_invisible_format(c))
    {
        return None;
    }
    if cfg!(not(windows)) && path.contains('\\') {
        return None;
    }
    let mut components = Path::new(path).components();
    let mut out = String::new();
    match components.next()? {
        Component::RootDir if !cfg!(windows) => {}
        #[cfg(windows)]
        Component::Prefix(prefix) => {
            use std::path::Prefix;
            let (Prefix::Disk(drive) | Prefix::VerbatimDisk(drive)) = prefix.kind() else {
                return None;
            };
            out.push(char::from(drive));
            out.push(':');
            if !matches!(components.next(), Some(Component::RootDir)) {
                return None;
            }
        }
        _ => return None,
    }
    for part in components {
        let Component::Normal(name) = part else {
            return None;
        };
        let name = name.to_str()?;
        #[cfg(windows)]
        if name.contains([':', '/', '\\']) || name.ends_with(['.', ' ']) {
            return None;
        }
        out.push('/');
        out.push_str(name);
    }
    if out.is_empty() || (cfg!(windows) && out.ends_with(':')) {
        out.push('/');
    }
    Some(out)
}

/// Bidirectional controls, zero-width characters, and similar `Cf` code points.
fn is_invisible_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2069}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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

    #[test]
    fn scoped_example_enforces_files_scripts_and_workers() {
        let p = parse(
            include_str!("../../examples/modes/scoped-development.yaml"),
            "scoped-development",
            &crate::tools::BUILTIN,
        )
        .unwrap();
        assert!(matches!(
            p.evaluate("write_file", "docs/draft", &[]),
            Verdict::Approve { .. }
        ));
        assert!(matches!(
            p.evaluate("write_file", "docs/published/post", &[]),
            Verdict::Deny { .. }
        ));
        assert!(matches!(
            p.evaluate("read_file", "src-other/file", &[]),
            Verdict::Deny { .. }
        ));
        assert!(matches!(
            p.evaluate_skill_command("repo/review", "scripts/check.sh", "crates/api", &[]),
            Verdict::Ask { .. }
        ));
        assert!(matches!(
            p.evaluate_skill_command("repo/review", "scripts/check.sh", "crates/cli", &[]),
            Verdict::Deny { .. }
        ));
        assert!(matches!(
            p.evaluate("delegate", "", &["reviewer".into()]),
            Verdict::Ask { .. }
        ));
    }

    #[test]
    fn compact_commands_match_both_directory_and_argv_prefix() {
        let src = "version: 2\ndefault_mode: review\nmodes:\n  review:\n    ask:\n      commands:\n        - argv_prefixes: [[cargo, test], [cargo, check]]\n          working_dirs: [crates/api, crates/cli]\n";
        let policy = policy(src);
        for (dir, subcommand, asks) in [
            ("crates/api", "test", true),
            ("crates/cli/src", "check", true),
            ("crates/api-old", "test", false),
            ("", "check", false),
            ("crates/api", "publish", false),
        ] {
            let verdict = policy.evaluate(
                "run_command",
                dir,
                &["cargo".into(), subcommand.into(), "--quiet".into()],
            );
            match (asks, verdict) {
                (true, Verdict::Ask { .. }) | (false, Verdict::Deny { .. }) => {}
                (_, unexpected) => panic!("{dir} cargo {subcommand}: {unexpected:?}"),
            }
        }
        for invalid in ["[]", "['']", "[../elsewhere]", "null"] {
            assert!(parse(
                &src.replace("[crates/api, crates/cli]", invalid),
                "fixture",
                &TOOLS
            )
            .is_err());
        }
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
            ("version: 99\n".to_string(), "version 99 is not supported"),
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
        #[cfg(unix)]
        assert_eq!(normalize_host_path("/").as_deref(), Some("/"));
        #[cfg(unix)]
        for same in ["//etc///hosts/", "/etc/./hosts"] {
            assert_eq!(normalize_host_path(same).as_deref(), Some("/etc/hosts"));
        }
        for bad in [
            "",
            "etc/hosts",
            "./x",
            "/etc/../shadow",
            "/a\0b",
            "/a\nb",
            "/a\u{202E}b",
            "/\u{200B}x",
        ] {
            assert_eq!(normalize_host_path(bad), None, "{bad:?}");
        }
        #[cfg(not(windows))]
        assert_eq!(normalize_host_path("/a\\b"), None);
        #[cfg(windows)]
        {
            assert_eq!(normalize_host_path(r"C:\").as_deref(), Some("C:/"));
            assert_eq!(
                normalize_host_path(r"C:\work\todo").as_deref(),
                Some("C:/work/todo")
            );
            for bad in [
                r"C:relative",
                r"\rooted",
                r"\\server\share\f",
                r"\\.\pipe\f",
                r"C:\work\f:stream",
            ] {
                assert_eq!(normalize_host_path(bad), None);
            }
        }
        assert!(path_within("/etc", "/etc/hosts"));
        assert!(!path_within("/etc", "/etcx/hosts"));
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
                assert!(error.contains("version 99 is not supported"), "{error}");
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
            seen,
            examples::EXAMPLES.len() + 1,
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
    use crate::error::Result;

    fn parse_rules(rules: &str) -> Result<ActionPolicy> {
        parse(
            &format!("version: 1\nrules:\n{rules}"),
            "test",
            &crate::tools::BUILTIN,
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
            ("- {id: x, action: delegate, match: {}, effect: approve}\n", "match.worker must be a valid remote label"),
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
            "- {id: review, action: skill, match: {skill: repo/rust-review}, effect: approve}\n\
             - {id: release, action: skill, match: {skill: repo/release}, effect: approve, ask: true}\n\
             - {id: no-deploy, action: skill, match: {skill: repo/deploy}, effect: deny}\n\
             - {id: twice, action: skill, match: {skill: repo/rust-review}, effect: approve}\n",
        )
        .unwrap();
        assert_eq!(policy.advertised_tools(), ["load_skill"]);
        assert_eq!(
            policy.approved_skills(),
            ["repo/rust-review", "repo/release"]
        );
        assert_eq!(policy.rules[0].action, ActionKind::Skill);
        assert_eq!(
            policy.rules[0].argv,
            Some(vec!["repo/rust-review".to_string()])
        );
        assert!(matches!(
            policy.evaluate_skill("repo/rust-review"),
            Verdict::Approve { rule_id } if rule_id == "review"
        ));
        assert!(matches!(
            policy.evaluate_skill("repo/release"),
            Verdict::Ask { rule_id } if rule_id == "release"
        ));
        assert!(matches!(
            policy.evaluate_skill("repo/deploy"),
            Verdict::Deny { rule_id: Some(rule_id) } if rule_id == "no-deploy"
        ));
        assert!(
            matches!(
                policy.evaluate_skill("repo/Deploy"),
                Verdict::Deny { rule_id: Some(rule_id) } if rule_id == "no-deploy"
            ),
            "deny ignores case"
        );
        assert!(
            matches!(
                policy.evaluate_skill("repo/Rust-Review"),
                Verdict::Deny { rule_id: None }
            ),
            "approve is exact"
        );
        assert!(matches!(
            policy.evaluate_skill("repo/other"),
            Verdict::Deny { rule_id: None }
        ));
        let operator = parse_rules(
            "- {id: operators, action: skill, match: {skill: 'operator/*'}, effect: approve}\n\
             - {id: private, action: skill, match: {skill: operator/private}, effect: deny}\n",
        )
        .unwrap();
        assert!(
            matches!(operator.evaluate_skill("operator/review"), Verdict::Approve { rule_id } if rule_id == "operators")
        );
        assert!(
            matches!(operator.evaluate_skill("operator/private"), Verdict::Deny { rule_id: Some(rule_id) } if rule_id == "private")
        );
        assert!(matches!(
            operator.evaluate_skill("repo/review"),
            Verdict::Deny { rule_id: None }
        ));
        // Deny beats approve for the same name.
        let both = parse_rules(
            "- {id: yes, action: skill, match: {skill: repo/x}, effect: approve}\n\
             - {id: no, action: skill, match: {skill: repo/X}, effect: deny}\n",
        )
        .unwrap();
        assert!(matches!(
            both.evaluate_skill("repo/x"),
            Verdict::Deny { rule_id: Some(rule_id) } if rule_id == "no"
        ));
        for (bad, message) in [
            ("- {id: x, action: skill, match: {}, effect: approve}\n", "match.skill is required"),
            ("- {id: x, action: skill, match: {skill: 'a b'}, effect: approve}\n", "expected repo/<skill-name> or operator/<skill-name>"),
            ("- {id: x, action: skill, match: {skill: repo/*}, effect: approve}\n", "expected repo/<skill-name> or operator/<skill-name>"),
            ("- {id: x, action: skill, match: {skill: unscoped}, effect: approve}\n", "expected repo/<skill-name> or operator/<skill-name>"),
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
