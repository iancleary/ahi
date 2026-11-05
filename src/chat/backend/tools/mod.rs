//! Builtin, read-only model tools. Tools are compiled into ahi; repository
//! configuration may only select and bound them (see `crate::actions`).
//!
//! Preparation is lexical and touches no file, so an unapproved call learns
//! nothing about the workspace. Execution re-checks containment on disk.

mod list_dir;
mod read_file;
mod workspace;

use serde::Deserialize;
use serde_json::{json, Value};

pub(super) use workspace::Workspace;

use super::ToolCall;
use crate::actions::normalize_relative_path;

pub(crate) const READ_FILE: &str = "read_file";
pub(crate) const LIST_DIR: &str = "list_dir";
pub(crate) const BUILTIN: [&str; 2] = [READ_FILE, LIST_DIR];

/// A call whose arguments parsed into a tool's typed input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreparedCall {
    pub(super) tool: &'static str,
    /// Normalized workspace-relative path; `""` is the workspace root.
    pub(super) path: String,
}

impl PreparedCall {
    /// What the operator sees and approves.
    pub(super) fn target(&self) -> &str {
        if self.path.is_empty() {
            "."
        } else {
            &self.path
        }
    }

    /// The narrowest useful session-grant prefix for this call: the parent
    /// directory of a file, or the listed directory itself.
    pub(super) fn grant_prefix(&self) -> String {
        match self.tool {
            READ_FILE => self
                .path
                .rsplit_once('/')
                .map(|(parent, _)| parent.to_string())
                .unwrap_or_default(),
            _ => self.path.clone(),
        }
    }
}

/// Function definitions for the request's `tools` field, in builtin order.
pub(super) fn definitions(enabled: &[String]) -> Vec<Value> {
    let path = |description: &str| json!({"type": "string", "description": description});
    BUILTIN
        .iter()
        .filter(|name| enabled.iter().any(|tool| tool == *name))
        .map(|name| {
            let (description, parameters) = match *name {
                READ_FILE => (
                    "Read a UTF-8 text file inside the working directory. Output is capped at 64 KiB.",
                    json!({
                        "type": "object",
                        "properties": {"path": path("File path relative to the working directory")},
                        "required": ["path"],
                        "additionalProperties": false
                    }),
                ),
                _ => (
                    "List a directory inside the working directory. Directories end with '/', symbolic links with '@'. At most 500 entries.",
                    json!({
                        "type": "object",
                        "properties": {"path": path("Directory relative to the working directory; defaults to '.'")},
                        "additionalProperties": false
                    }),
                ),
            };
            json!({
                "type": "function",
                "function": {"name": name, "description": description, "parameters": parameters}
            })
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgument {
    path: Option<String>,
}

/// Parse a call's arguments. Errors never repeat the arguments.
pub(super) fn prepare(call: &ToolCall) -> Result<PreparedCall, &'static str> {
    let tool = BUILTIN
        .into_iter()
        .find(|name| *name == call.name)
        .ok_or("unknown tool")?;
    let argument: PathArgument = serde_json::from_value(Value::Object(call.arguments.clone()))
        .map_err(|_| "invalid tool arguments")?;
    let path = match (tool, argument.path) {
        (READ_FILE, None) => return Err("invalid tool arguments"),
        (_, None) => String::new(),
        (_, Some(path)) => normalize_relative_path(&path)
            .ok_or("path must be relative to the working directory, without `..`")?,
    };
    if tool == READ_FILE && path.is_empty() {
        return Err("read_file needs a file path");
    }
    Ok(PreparedCall { tool, path })
}

/// Run an approved call. `Err` text is safe to show and to send to the model.
pub(super) fn execute(call: &PreparedCall, workspace: &Workspace) -> Result<String, &'static str> {
    let path = workspace.resolve(&call.path)?;
    match call.tool {
        READ_FILE => read_file::run(&path),
        _ => list_dir::run(&path),
    }
}

#[cfg(test)]
pub(super) fn call(id: &str, name: &str, arguments: Value) -> ToolCall {
    let arguments = match arguments {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn preparation_is_strict_and_lexical() {
        let ok = |name, args| prepare(&call("c", name, args)).unwrap();
        assert_eq!(
            ok(READ_FILE, json!({"path": "./src//main.rs"})),
            PreparedCall {
                tool: READ_FILE,
                path: "src/main.rs".into()
            }
        );
        assert_eq!(ok(LIST_DIR, json!({})).target(), ".");
        assert_eq!(
            ok(READ_FILE, json!({"path": "a/b/c"})).grant_prefix(),
            "a/b"
        );
        assert_eq!(ok(READ_FILE, json!({"path": "top"})).grant_prefix(), "");
        assert_eq!(ok(LIST_DIR, json!({"path": "a/b"})).grant_prefix(), "a/b");
        for (name, args, expected) in [
            ("bash", json!({}), "unknown tool"),
            (READ_FILE, json!({}), "invalid tool arguments"),
            (READ_FILE, json!({"path": 3}), "invalid tool arguments"),
            (
                READ_FILE,
                json!({"path": "a", "x": 1}),
                "invalid tool arguments",
            ),
            (
                READ_FILE,
                json!({"path": "."}),
                "read_file needs a file path",
            ),
            (
                READ_FILE,
                json!({"path": "../secret"}),
                "path must be relative to the working directory, without `..`",
            ),
            (
                LIST_DIR,
                json!({"path": "/etc"}),
                "path must be relative to the working directory, without `..`",
            ),
        ] {
            assert_eq!(prepare(&call("c", name, args)), Err(expected));
        }
    }

    #[test]
    fn definitions_follow_the_enabled_builtins() {
        let names = |enabled: &[&str]| {
            definitions(&enabled.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .into_iter()
                .map(|value| value["function"]["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&[LIST_DIR, READ_FILE]), [READ_FILE, LIST_DIR]);
        assert_eq!(names(&[LIST_DIR]), [LIST_DIR]);
        assert!(names(&[]).is_empty());
    }

    #[test]
    fn execution_reads_lists_and_refuses_escapes() {
        let root = crate::fixture_directory::create_unique_fixture_directory("tools-exec");
        let outside = crate::fixture_directory::create_unique_fixture_directory("tools-outside");
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("binary"), [0xff, 0xfe, 0x00]).unwrap();
        fs::write(root.join("big"), "x".repeat(70 * 1024)).unwrap();
        fs::write(outside.join("secret"), "secret").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.join("secret"), root.join("link")).unwrap();
            std::os::unix::fs::symlink(root.join("src"), root.join("inner")).unwrap();
        }
        fs::create_dir(root.join("state")).unwrap();
        fs::write(root.join("state/models.json"), "{}").unwrap();
        let workspace = Workspace::open(&root, &root.join("state")).unwrap();
        let run = |tool, path: &str| {
            execute(
                &PreparedCall {
                    tool,
                    path: path.into(),
                },
                &workspace,
            )
        };
        assert_eq!(run(READ_FILE, "src/main.rs").unwrap(), "fn main() {}\n");
        assert_eq!(run(READ_FILE, "binary"), Err("file is not UTF-8 text"));
        let big = run(READ_FILE, "big").unwrap();
        assert!(big.starts_with(&"x".repeat(1000)));
        assert!(big.ends_with("\n[truncated at 65536 bytes]"));
        assert_eq!(run(READ_FILE, "src"), Err("not a regular file"));
        let state = Err("path is inside ahi's state directory, which tools cannot read");
        assert_eq!(run(READ_FILE, "state/models.json"), state);
        assert_eq!(run(LIST_DIR, "state"), state);
        // Case variants resolve to the same directory on case-insensitive
        // filesystems; they must never be readable either.
        for (tool, path) in [
            (READ_FILE, "STATE/models.json"),
            (LIST_DIR, "State"),
            (READ_FILE, "SRC/main.rs"),
        ] {
            let result = run(tool, path);
            assert!(
                matches!(
                    result,
                    Err("path must be spelled exactly as on disk inside the working directory; symbolic links and case variants are refused")
                        | Err("path does not exist or cannot be read")
                ),
                "{path}: {result:?}"
            );
        }
        assert_eq!(run(LIST_DIR, "src/main.rs"), Err("not a directory"));
        assert_eq!(
            run(READ_FILE, "missing"),
            Err("path does not exist or cannot be read")
        );
        #[cfg(unix)]
        {
            let listing = run(LIST_DIR, "").unwrap();
            assert_eq!(listing, "big\nbinary\ninner@\nlink@\nsrc/\nstate/");
            let escaped = "path must be spelled exactly as on disk inside the working directory; symbolic links and case variants are refused";
            assert_eq!(run(READ_FILE, "link"), Err(escaped));
            assert_eq!(run(READ_FILE, "inner/main.rs"), Err(escaped));
        }
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn listing_is_sorted_and_capped() {
        let root = crate::fixture_directory::create_unique_fixture_directory("tools-many");
        for n in 0..510 {
            fs::write(root.join(format!("f{n:03}")), "").unwrap();
        }
        let listing = list_dir::run(&root).unwrap();
        let lines: Vec<_> = listing.lines().collect();
        assert_eq!(lines.len(), 501);
        assert_eq!(lines[0], "f000");
        assert_eq!(lines[499], "f499");
        assert_eq!(lines[500], "[10 more entries not shown]");
        fs::remove_dir_all(root).unwrap();
    }
}
