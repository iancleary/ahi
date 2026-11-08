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

/// Shipped policies, in the order `ahi tools list` prints them. The
/// deliberately invalid `invalid-version.yaml` stays in `examples/tools` for
/// the fail-closed walkthrough and is not offered here.
pub const EXAMPLES: [Example; 7] = [
    Example {
        name: "read-all",
        text: include_str!("../examples/tools/read-all.yaml"),
        summary: "read, list, find, and search anywhere except .env; no prompts",
    },
    Example {
        name: "ask-every-read",
        text: include_str!("../examples/tools/ask-every-read.yaml"),
        summary: "every read and listing asks in chat",
    },
    Example {
        name: "sources-only",
        text: include_str!("../examples/tools/sources-only.yaml"),
        summary: "list anywhere; read only src and docs; .env and target denied",
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
        summary: "read anywhere; cargo check and test run; other cargo asks; publish denied",
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
            "no example named `{name}`; `ahi tools list` prints them: {}",
            names.join(", ")
        )
    })
}

/// `ahi tools list`: one line per example, name then summary.
pub fn render_list() -> String {
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

    #[test]
    fn summary_counts_rules_and_advertised_tools() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("tools-summary");
        assert_eq!(summary(&dir).unwrap(), None);
        assert!(doctor_lines(&dir)[0].contains("absent"));
        fs::write(dir.join(FILE_NAME), example("commands").unwrap().text).unwrap();
        let got = summary(&dir).unwrap().unwrap();
        assert_eq!((got.approve, got.ask, got.deny), (6, 2, 2));
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
        assert_eq!(lines[1], "tool rules: 6 approve, 2 ask, 2 deny");
        fs::write(dir.join(FILE_NAME), example("deny-all").unwrap().text).unwrap();
        assert!(doctor_lines(&dir)[2].ends_with("none (no approve rule)"));
        fs::write(dir.join(FILE_NAME), "version: 2\n").unwrap();
        assert!(doctor_lines(&dir)[0].starts_with("tools policy: "));
        let _ = fs::remove_dir_all(&dir);
    }
}
