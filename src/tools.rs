//! `ahi tools`: write a shipped tool policy into the working directory and
//! describe the one that is there. The chat screen never writes
//! `ahi.tools.yaml`; this command is the operator's pen, run before any
//! model is involved, and it follows `ahi migrate`: print first, write only
//! with `--write`, replace only with `--write --force`.
//!
//! The examples are the files under `examples/tools`, embedded at build
//! time so a `cargo install` has them. A test keeps the two in step.

use std::path::Path;

use crate::chat::backend::tools::BUILTIN;
use crate::chat::transcript::theme;
use crate::error::Result;
use crate::tool_policy::{self, Effect};

/// The policy file name, `ahi.tools.yaml`.
pub const FILE_NAME: &str = crate::tool_policy::FILE_NAME;

/// One shipped policy: its file stem, the text, and what the model gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Example {
    pub name: &'static str,
    pub text: &'static str,
    pub summary: &'static str,
}

/// The default for `ahi tools init`: the policy this repository ships.
pub const DEFAULT_EXAMPLE: &str = "read-all";

/// Shipped policies, in the order `ahi tools examples` prints them. The
/// deliberately invalid `invalid-version.yaml` stays in `examples/tools` for
/// the fail-closed walkthrough and is not offered here.
pub const EXAMPLES: [Example; 7] = [
    Example {
        name: "read-all",
        text: include_str!("../examples/tools/read-all.yaml"),
        summary: "read, list, find, and search anywhere; root .env denied; no prompts",
    },
    Example {
        name: "ask-every-read",
        text: include_str!("../examples/tools/ask-every-read.yaml"),
        summary: "every read, listing, find, and search asks in chat",
    },
    Example {
        name: "sources-only",
        text: include_str!("../examples/tools/sources-only.yaml"),
        summary: "list anywhere; read, find, and search only src and docs",
    },
    Example {
        name: "list-only",
        text: include_str!("../examples/tools/list-only.yaml"),
        summary: "list directories only; reads are denied",
    },
    Example {
        name: "edit-sources",
        text: include_str!("../examples/tools/edit-sources.yaml"),
        summary: "read anywhere; edit under src and tests after a prompt; create under tests",
    },
    Example {
        name: "commands",
        text: include_str!("../examples/tools/commands.yaml"),
        summary:
            "read anywhere; cargo check and test run; other cargo asks; publish and install denied",
    },
    Example {
        name: "deny-all",
        text: include_str!("../examples/tools/deny-all.yaml"),
        summary: "valid, no approve rule: no tool is advertised",
    },
];

/// The example called `name`, or the names that exist.
pub fn example(name: &str) -> std::result::Result<&'static Example, String> {
    EXAMPLES.iter().find(|e| e.name == name).ok_or_else(|| {
        let names: Vec<&str> = EXAMPLES.iter().map(|e| e.name).collect();
        format!(
            "no example named `{name}`; `ahi tools examples` prints them: {}",
            names.join(", ")
        )
    })
}

/// The index at the top of `ahi tools examples`: one line per example,
/// name then summary.
pub fn render_index() -> String {
    let width = EXAMPLES.iter().map(|e| e.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for e in EXAMPLES {
        let default = if e.name == DEFAULT_EXAMPLE {
            " (default)"
        } else {
            ""
        };
        out.push_str(&format!("{:width$}  {}{default}\n", e.name, e.summary));
    }
    out
}

/// How `init` paints a rule, by its outcome for the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Approve,
    Ask,
    Deny,
}

impl Outcome {
    fn background(self) -> (u8, u8, u8) {
        match self {
            Outcome::Approve => theme::APPROVE_BG,
            Outcome::Ask => theme::ASK_BG,
            Outcome::Deny => theme::DENY_BG,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Outcome::Approve => "approve",
            Outcome::Ask => "ask",
            Outcome::Deny => "deny",
        }
    }
}

/// Whether `line` starts a rule in a `rules:` list.
fn starts_rule(line: &str) -> bool {
    line.trim_start().starts_with("- id:")
}

/// Whether `line` continues the rule begun above it: deeper indentation,
/// not a comment at the list's own indentation.
fn continues_rule(line: &str) -> bool {
    let indent = line.len() - line.trim_start().len();
    !line.trim().is_empty() && indent >= 4 && !line.trim_start().starts_with('#')
}

/// Read a rule's outcome from its lines. A rule the parser would reject
/// still gets a color here; the parse is the chat screen's job.
fn outcome(block: &[&str]) -> Outcome {
    let has = |key: &str| {
        block
            .iter()
            .any(|line| line.trim_start().trim_start_matches("- ").starts_with(key))
    };
    if has("effect: deny") {
        Outcome::Deny
    } else if has("ask: true") {
        Outcome::Ask
    } else {
        Outcome::Approve
    }
}

fn paint(line: &str, width: usize, (r, g, b): (u8, u8, u8)) -> String {
    format!("\x1b[48;2;{r};{g};{b}m{line:<width$}\x1b[0m")
}

/// The policy text for `init`. With `color`, every rule's lines carry a
/// background the transcript uses for the same outcome (deny like a removed
/// diff line, approve like an added one, ask in gray), padded to the widest
/// line so each rule reads as one block, and a legend follows. Comments and
/// the header keep the terminal's colors. Without `color` the text is the
/// file exactly.
pub fn render_policy(text: &str, color: bool) -> String {
    if !color {
        return text.to_string();
    }
    let lines: Vec<&str> = text.lines().collect();
    // Pad to the widest line so a rule reads as one block, but never to the
    // terminal's last column: a painted line that reaches the edge wraps and
    // shows as a blank row. Cut lines wider than the terminal are left as
    // they are; the terminal wraps those either way.
    let widest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let width = match crossterm::terminal::size() {
        Ok((columns, _)) if columns > 1 => widest.min(usize::from(columns) - 1),
        _ => widest,
    };
    let mut out = String::new();
    let mut index = 0;
    while index < lines.len() {
        if !starts_rule(lines[index]) {
            out.push_str(lines[index]);
            out.push('\n');
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        while index < lines.len() && continues_rule(lines[index]) {
            index += 1;
        }
        let block = &lines[start..index];
        let background = outcome(block).background();
        for line in block {
            out.push_str(&paint(line, width, background));
            out.push('\n');
        }
    }
    out.push('\n');
    let legend: Vec<String> = [Outcome::Approve, Outcome::Ask, Outcome::Deny]
        .iter()
        .map(|o| paint(&format!(" {} ", o.label()), 0, o.background()))
        .collect();
    out.push_str(&format!(
        "{}  the model gets the call, is asked in chat first, or is refused\n",
        legend.join(" ")
    ));
    out
}

/// `ahi tools examples`: the index, then every example with a heading, or
/// one example when `name` is given.
pub fn render_examples(name: Option<&str>, color: bool) -> std::result::Result<String, String> {
    let chosen: Vec<&Example> = match name {
        Some(name) => vec![example(name)?],
        None => EXAMPLES.iter().collect(),
    };
    let mut out = String::new();
    if name.is_none() {
        out.push_str(&render_index());
        out.push('\n');
    }
    for (index, e) in chosen.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&format!("== {} (examples/tools/{}.yaml)\n", e.name, e.name));
        out.push_str(&render_policy(e.text, color));
    }
    Ok(out)
}

/// `ahi tools list`: the working directory's `ahi.tools.yaml`, painted by
/// outcome, followed by the doctor summary. Without the file, the doctor
/// line alone, which names `ahi tools init`. An invalid file is still
/// shown, with its parse error after it, so the operator sees what to fix.
pub fn render_current(root: &Path, color: bool) -> String {
    let path = root.join(FILE_NAME);
    let mut out = String::new();
    if let Ok(text) = std::fs::read_to_string(&path) {
        out.push_str(&format!("== {}\n", path.display()));
        out.push_str(&render_policy(&text, color));
        out.push('\n');
    }
    for line in doctor_lines(root) {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// What the policy in `root` approves, for `ahi doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub approve: usize,
    pub ask: usize,
    pub deny: usize,
    /// Builtin tools at least one rule could approve, in rule order.
    pub advertised: Vec<String>,
}

/// Read `root/ahi.tools.yaml`. `Ok(None)` when it is absent; an error when
/// it is present and invalid, or when the retired name is present.
pub fn summary(root: &Path) -> Result<Option<Summary>> {
    let Some(policy) = tool_policy::load(root, &BUILTIN)? else {
        return Ok(None);
    };
    let mut out = Summary {
        approve: 0,
        ask: 0,
        deny: 0,
        advertised: policy.advertised_tools(),
    };
    for rule in &policy.rules {
        match (rule.effect, rule.ask) {
            (Effect::Deny, _) => out.deny += 1,
            (Effect::Approve, true) => out.ask += 1,
            (Effect::Approve, false) => out.approve += 1,
        }
    }
    Ok(Some(out))
}

/// Doctor lines for `root`, the same text whether or not a file exists.
pub fn doctor_lines(root: &Path) -> Vec<String> {
    let path = root.join(FILE_NAME);
    match summary(root) {
        Ok(Some(summary)) => {
            let advertised = if summary.advertised.is_empty() {
                "none (no approve rule)".to_string()
            } else {
                summary.advertised.join(", ")
            };
            vec![
                format!("tools policy: {}", path.display()),
                format!(
                    "tool rules: {} approve, {} ask, {} deny",
                    summary.approve, summary.ask, summary.deny
                ),
                format!("tools advertised: {advertised}"),
            ]
        }
        Ok(None) => vec![
            "tools policy: absent (tools off; `ahi tools init --write` creates a read-only one)"
                .to_string(),
        ],
        Err(err) => vec![format!("tools policy: {err}")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn every_example_parses_and_matches_the_directory() {
        for e in EXAMPLES {
            tool_policy::parse(e.text, e.name, &BUILTIN)
                .unwrap_or_else(|err| panic!("{}: {err}", e.name));
        }
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/tools");
        let mut on_disk: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter_map(|name| name.strip_suffix(".yaml").map(str::to_string))
            .filter(|name| name != "invalid-version")
            .collect();
        on_disk.sort();
        let mut embedded: Vec<String> = EXAMPLES.iter().map(|e| e.name.to_string()).collect();
        embedded.sort();
        assert_eq!(
            embedded, on_disk,
            "EXAMPLES must list every file in examples/tools"
        );
        assert!(example(DEFAULT_EXAMPLE).is_ok());
        assert!(example("nope").unwrap_err().contains("read-all"));
    }

    /// The repository's own policy begins with the `read-all` example, rule
    /// for rule, and adds only command rules after it, so `ahi tools init`
    /// here shows the same default the repository uses.
    #[test]
    fn repository_policy_starts_with_read_all() {
        let root = tool_policy::parse(
            include_str!("../ahi.tools.yaml"),
            "ahi.tools.yaml",
            &BUILTIN,
        )
        .unwrap();
        let read_all =
            tool_policy::parse(example("read-all").unwrap().text, "read-all", &BUILTIN).unwrap();
        let shared = read_all.rules.len();
        assert_eq!(root.rules[..shared], read_all.rules[..]);
        assert!(
            root.rules.len() > shared,
            "the repository policy adds command rules"
        );
        for rule in &root.rules[shared..] {
            assert_eq!(rule.action, tool_policy::ActionKind::Command, "{}", rule.id);
        }
    }

    #[test]
    fn render_policy_paints_each_rule_by_outcome_and_leaves_comments_plain() {
        let text = example("commands").unwrap().text;
        assert_eq!(render_policy(text, false), text);
        let painted = render_policy(text, true);
        let bg = |(r, g, b): (u8, u8, u8)| format!("\x1b[48;2;{r};{g};{b}m");
        let line_with = |needle: &str| {
            painted
                .lines()
                .find(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle}"))
                .to_string()
        };
        assert!(line_with("id: no-env").starts_with(&bg(theme::DENY_BG)));
        assert!(line_with("match: {tool: read_file, path: .env}").starts_with(&bg(theme::DENY_BG)));
        assert!(line_with("id: cargo-check").starts_with(&bg(theme::APPROVE_BG)));
        assert!(line_with("toolchain_env: [CARGO_TARGET_DIR]").starts_with(&bg(theme::APPROVE_BG)));
        assert!(line_with("id: cargo-other").starts_with(&bg(theme::ASK_BG)));
        assert!(line_with("id: no-publish").starts_with(&bg(theme::DENY_BG)));
        assert!(line_with("# Reads anywhere").starts_with("# Reads"));
        assert!(line_with("version: 1").starts_with("version: 1"));
        assert!(painted.lines().last().unwrap().contains("approve"));
        let all = render_examples(None, false).unwrap();
        assert!(all.starts_with("read-all"), "{all}");
        for e in EXAMPLES {
            assert!(all.contains(&format!("== {} (examples/tools/{}.yaml)", e.name, e.name)));
            assert!(all.contains(e.text));
        }
        let one = render_examples(Some("deny-all"), false).unwrap();
        assert!(one.starts_with("== deny-all"), "{one}");
        assert!(!one.contains("== read-all"));
        assert!(render_examples(Some("nope"), false).is_err());
        // The repository policy's section comments sit between rules and
        // stay unpainted; every rule still gets its own outcome.
        let root = render_policy(include_str!("../ahi.tools.yaml"), true);
        let section = root
            .lines()
            .find(|l| l.contains("Repository commands"))
            .unwrap();
        assert!(section.starts_with("  #"), "{section}");
        assert!(root
            .lines()
            .find(|l| l.contains("id: no-git-push"))
            .unwrap()
            .starts_with(&bg(theme::DENY_BG)));
    }

    #[test]
    fn summary_counts_rules_and_advertised_tools() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("tools-summary");
        assert_eq!(summary(&dir).unwrap(), None);
        assert!(doctor_lines(&dir)[0].contains("absent"));
        fs::write(dir.join(FILE_NAME), example("commands").unwrap().text).unwrap();
        let got = summary(&dir).unwrap().unwrap();
        assert_eq!((got.approve, got.ask, got.deny), (6, 2, 3));
        assert_eq!(
            got.advertised,
            [
                "read_file",
                "list_dir",
                "find_files",
                "search_files",
                "run_command"
            ]
        );
        let lines = doctor_lines(&dir);
        assert_eq!(lines[1], "tool rules: 6 approve, 2 ask, 3 deny");
        fs::write(dir.join(FILE_NAME), example("deny-all").unwrap().text).unwrap();
        assert!(doctor_lines(&dir)[2].ends_with("none (no approve rule)"));
        fs::write(dir.join(FILE_NAME), "version: 2\n").unwrap();
        assert!(doctor_lines(&dir)[0].starts_with("tools policy: "));
        let current = render_current(&dir, false);
        assert!(current.starts_with("== "), "{current}");
        assert!(current.contains("version: 2"), "{current}");
        assert!(current.contains("tools policy: "), "{current}");
        fs::remove_file(dir.join(FILE_NAME)).unwrap();
        assert!(render_current(&dir, false).starts_with("tools policy: absent"));
        let _ = fs::remove_dir_all(&dir);
    }
}
