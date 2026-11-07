//! Builtin, read-only model tools. Tools are compiled into ahi; repository
//! configuration may only select and bound them (see `crate::tool_policy`).
//!
//! Preparation is lexical and touches no file, so an unapproved call learns
//! nothing about the workspace. Execution re-checks containment on disk.
//! Every tool is deterministic: sorted output, fixed caps, no shell.

mod find_files;
mod glob;
mod ignore;
mod list_dir;
mod read_file;
mod search_files;
mod walk;
mod workspace;

use serde::Deserialize;
use serde_json::{json, Value};

pub(super) use workspace::Workspace;

use super::ToolCall;
use crate::tool_policy::normalize_relative_path;

pub(crate) const READ_FILE: &str = "read_file";
pub(crate) const LIST_DIR: &str = "list_dir";
pub(crate) const FIND_FILES: &str = "find_files";
pub(crate) const SEARCH_FILES: &str = "search_files";
pub(crate) const BUILTIN: [&str; 4] = [READ_FILE, LIST_DIR, FIND_FILES, SEARCH_FILES];

/// Arguments beyond the path, already validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Arguments {
    Read {
        offset: Option<usize>,
        limit: Option<usize>,
    },
    List,
    Find {
        pattern: String,
        limit: usize,
    },
    Search {
        pattern: String,
        literal: bool,
        ignore_case: bool,
        glob: Option<String>,
        limit: usize,
    },
}

/// A call whose arguments parsed into a tool's typed input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreparedCall {
    pub(super) tool: &'static str,
    /// Normalized workspace-relative path; `""` is the workspace root. For
    /// `find_files` and `search_files` it is the directory searched.
    pub(super) path: String,
    pub(super) arguments: Arguments,
}

impl PreparedCall {
    /// What the operator sees and approves: the path only. Patterns are
    /// never shown, so a prompt cannot carry model-chosen text.
    pub(super) fn target(&self) -> &str {
        if self.path.is_empty() {
            "."
        } else {
            &self.path
        }
    }

    /// The narrowest useful session-grant prefix for this call: the parent
    /// directory of a file, or the directory listed or searched.
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

/// One builtin's schema, independent of any wire protocol. Each protocol
/// serializes this into its own `tools` shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolSchema {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    /// A JSON Schema object for the arguments.
    pub(crate) parameters: Value,
}

/// Schemas for the enabled builtins, in builtin order.
pub(super) fn schemas(enabled: &[String]) -> Vec<ToolSchema> {
    let string = |description: &str| json!({"type": "string", "description": description});
    let integer = |description: &str, minimum: usize| json!({"type": "integer", "minimum": minimum, "description": description});
    BUILTIN
        .iter()
        .filter(|name| enabled.iter().any(|tool| tool == *name))
        .map(|name| match *name {
            READ_FILE => ToolSchema {
                name: READ_FILE,
                description: "Read a UTF-8 text file inside the working directory, whole or a window of lines. Output is capped at 64 KiB; a tail line names the next offset when more remains.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": string("File path relative to the working directory"),
                        "offset": integer("First line to return, 1-based", 1),
                        "limit": integer("Maximum number of lines to return", 1)
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }),
            },
            LIST_DIR => ToolSchema {
                name: LIST_DIR,
                description: "List a directory inside the working directory. Directories end with '/', symbolic links with '@'. At most 500 entries.",
                parameters: json!({
                    "type": "object",
                    "properties": {"path": string("Directory relative to the working directory; defaults to '.'")},
                    "additionalProperties": false
                }),
            },
            FIND_FILES => ToolSchema {
                name: FIND_FILES,
                description: "Find files under a directory by glob. A pattern without '/' matches names at any depth ('*.rs'); with '/' it matches paths below the directory ('src/**/*.rs'). Honors .gitignore; never follows symbolic links. Sorted; default 200 results, maximum 1000.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": string("Glob: '*', '**', '?', '[abc]'"),
                        "path": string("Directory relative to the working directory; defaults to '.'"),
                        "limit": integer("Maximum results", 1)
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                }),
            },
            _ => ToolSchema {
                name: SEARCH_FILES,
                description: "Search text files under a directory for a regular expression (Rust syntax) or a literal string. Returns 'path:line:text' with files sorted and lines ascending; binary files and .gitignore'd paths are skipped. Default 100 matches, maximum 500, output capped at 64 KiB.",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": string("Regular expression, or exact text when literal is true"),
                        "path": string("Directory relative to the working directory; defaults to '.'"),
                        "glob": string("Only files matching this glob, e.g. '*.rs'"),
                        "literal": {"type": "boolean", "description": "Treat pattern as exact text"},
                        "ignore_case": {"type": "boolean", "description": "Case-insensitive match"},
                        "limit": integer("Maximum matching lines", 1)
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                }),
            },
        })
        .collect()
}

/// Chat Completions `tools[]` definitions for the enabled builtins.
#[cfg(test)]
pub(super) fn definitions(enabled: &[String]) -> Vec<Value> {
    schemas(enabled)
        .iter()
        .map(crate::chat::backend::protocol::chat_completions::definition)
        .collect()
}

/// Every argument any builtin accepts. Unknown keys are rejected; the
/// per-tool match below rejects keys a tool does not take.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawArguments {
    path: Option<String>,
    offset: Option<u64>,
    limit: Option<u64>,
    pattern: Option<String>,
    glob: Option<String>,
    literal: Option<bool>,
    ignore_case: Option<bool>,
}

const INVALID: &str = "invalid tool arguments";

/// Parse a call's arguments. Errors never repeat the arguments.
pub(super) fn prepare(call: &ToolCall) -> Result<PreparedCall, &'static str> {
    let tool = BUILTIN
        .into_iter()
        .find(|name| *name == call.name)
        .ok_or("unknown tool")?;
    let raw: RawArguments =
        serde_json::from_value(Value::Object(call.arguments.clone())).map_err(|_| INVALID)?;
    let path = match (tool, raw.path) {
        (READ_FILE, None) => return Err(INVALID),
        (_, None) => String::new(),
        (_, Some(path)) => normalize_relative_path(&path)
            .ok_or("path must be relative to the working directory, without `..`")?,
    };
    let small = |value: Option<u64>| -> Result<Option<usize>, &'static str> {
        match value {
            None => Ok(None),
            Some(0) => Err(INVALID),
            Some(n) if n <= 1 << 31 => Ok(Some(n as usize)),
            Some(_) => Err(INVALID),
        }
    };
    let only = |allowed: bool| if allowed { Ok(()) } else { Err(INVALID) };
    let arguments = match tool {
        READ_FILE => {
            if path.is_empty() {
                return Err("read_file needs a file path");
            }
            only(
                raw.pattern.is_none()
                    && raw.glob.is_none()
                    && raw.literal.is_none()
                    && raw.ignore_case.is_none(),
            )?;
            Arguments::Read {
                offset: small(raw.offset)?,
                limit: small(raw.limit)?,
            }
        }
        LIST_DIR => {
            only(
                raw.offset.is_none()
                    && raw.limit.is_none()
                    && raw.pattern.is_none()
                    && raw.glob.is_none()
                    && raw.literal.is_none()
                    && raw.ignore_case.is_none(),
            )?;
            Arguments::List
        }
        FIND_FILES => {
            only(
                raw.offset.is_none()
                    && raw.glob.is_none()
                    && raw.literal.is_none()
                    && raw.ignore_case.is_none(),
            )?;
            let pattern = raw.pattern.ok_or(INVALID)?;
            glob::Glob::new(&pattern).ok_or("invalid glob pattern")?;
            Arguments::Find {
                pattern,
                limit: small(raw.limit)?.unwrap_or(find_files::DEFAULT_LIMIT),
            }
        }
        _ => {
            only(raw.offset.is_none())?;
            let pattern = raw.pattern.ok_or(INVALID)?;
            if pattern.is_empty() || pattern.len() > 512 {
                return Err("pattern must be 1 to 512 bytes");
            }
            if let Some(glob) = &raw.glob {
                glob::Glob::new(glob).ok_or("invalid glob pattern")?;
            }
            Arguments::Search {
                pattern,
                literal: raw.literal.unwrap_or(false),
                ignore_case: raw.ignore_case.unwrap_or(false),
                glob: raw.glob,
                limit: small(raw.limit)?.unwrap_or(search_files::DEFAULT_LIMIT),
            }
        }
    };
    Ok(PreparedCall {
        tool,
        path,
        arguments,
    })
}

/// Run an approved call. `Err` text is safe to show and to send to the model.
pub(super) fn execute(call: &PreparedCall, workspace: &Workspace) -> Result<String, &'static str> {
    let path = workspace.resolve(&call.path)?;
    match &call.arguments {
        Arguments::Read { offset, limit } => read_file::run(&path, *offset, *limit),
        Arguments::List => list_dir::run(&path),
        Arguments::Find { pattern, limit } => {
            find_files::run(workspace, &path, &call.path, pattern, *limit)
        }
        Arguments::Search {
            pattern,
            literal,
            ignore_case,
            glob,
            limit,
        } => search_files::run(
            workspace,
            &path,
            &call.path,
            &search_files::Query {
                pattern,
                literal: *literal,
                ignore_case: *ignore_case,
                glob: glob.as_deref(),
                limit: *limit,
            },
        ),
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
    use std::path::PathBuf;

    #[test]
    fn preparation_is_strict_and_lexical() {
        let ok = |name, args| prepare(&call("c", name, args)).unwrap();
        assert_eq!(
            ok(READ_FILE, json!({"path": "./src//main.rs"})),
            PreparedCall {
                tool: READ_FILE,
                path: "src/main.rs".into(),
                arguments: Arguments::Read {
                    offset: None,
                    limit: None
                }
            }
        );
        assert_eq!(
            ok(READ_FILE, json!({"path": "a", "offset": 10, "limit": 5})).arguments,
            Arguments::Read {
                offset: Some(10),
                limit: Some(5)
            }
        );
        assert_eq!(ok(LIST_DIR, json!({})).target(), ".");
        assert_eq!(
            ok(READ_FILE, json!({"path": "a/b/c"})).grant_prefix(),
            "a/b"
        );
        assert_eq!(ok(READ_FILE, json!({"path": "top"})).grant_prefix(), "");
        assert_eq!(ok(LIST_DIR, json!({"path": "a/b"})).grant_prefix(), "a/b");
        let find = ok(FIND_FILES, json!({"pattern": "*.rs", "path": "src"}));
        assert_eq!(find.grant_prefix(), "src");
        assert_eq!(find.target(), "src");
        assert_eq!(
            find.arguments,
            Arguments::Find {
                pattern: "*.rs".into(),
                limit: 200
            }
        );
        assert_eq!(
            ok(SEARCH_FILES, json!({"pattern": "fn main", "literal": true})).arguments,
            Arguments::Search {
                pattern: "fn main".into(),
                literal: true,
                ignore_case: false,
                glob: None,
                limit: 100
            }
        );
        for (name, args, expected) in [
            ("bash", json!({}), "unknown tool"),
            (READ_FILE, json!({}), INVALID),
            (READ_FILE, json!({"path": 3}), INVALID),
            (READ_FILE, json!({"path": "a", "x": 1}), INVALID),
            (READ_FILE, json!({"path": "a", "offset": 0}), INVALID),
            (READ_FILE, json!({"path": "a", "pattern": "x"}), INVALID),
            (LIST_DIR, json!({"limit": 3}), INVALID),
            (FIND_FILES, json!({}), INVALID),
            (FIND_FILES, json!({"pattern": "[x"}), "invalid glob pattern"),
            (FIND_FILES, json!({"pattern": "*", "glob": "*"}), INVALID),
            (
                SEARCH_FILES,
                json!({"pattern": ""}),
                "pattern must be 1 to 512 bytes",
            ),
            (
                SEARCH_FILES,
                json!({"pattern": "a", "glob": "[x"}),
                "invalid glob pattern",
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
            assert_eq!(
                prepare(&call("c", name, args.clone())),
                Err(expected),
                "{name} {args}"
            );
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
        assert_eq!(
            names(&[SEARCH_FILES, LIST_DIR, READ_FILE]),
            [READ_FILE, LIST_DIR, SEARCH_FILES]
        );
        assert_eq!(names(&[LIST_DIR]), [LIST_DIR]);
        assert!(names(&[]).is_empty());
    }

    fn fixture(purpose: &str) -> (PathBuf, Workspace) {
        let root = crate::fixture_directory::create_unique_fixture_directory(purpose);
        fs::create_dir(root.join("state")).unwrap();
        fs::write(root.join("state/models.json"), "{}").unwrap();
        let workspace = Workspace::open(
            &root,
            &crate::directories::Directories::single_root(root.join("state")),
        )
        .unwrap();
        (root, workspace)
    }

    fn read(workspace: &Workspace, path: &str) -> Result<String, &'static str> {
        execute(
            &PreparedCall {
                tool: READ_FILE,
                path: path.into(),
                arguments: Arguments::Read {
                    offset: None,
                    limit: None,
                },
            },
            workspace,
        )
    }

    fn list(workspace: &Workspace, path: &str) -> Result<String, &'static str> {
        execute(
            &PreparedCall {
                tool: LIST_DIR,
                path: path.into(),
                arguments: Arguments::List,
            },
            workspace,
        )
    }

    #[test]
    fn execution_reads_lists_and_refuses_escapes() {
        let (root, workspace) = fixture("tools-exec");
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
        assert_eq!(read(&workspace, "src/main.rs").unwrap(), "fn main() {}\n");
        assert_eq!(read(&workspace, "binary"), Err("file is not UTF-8 text"));
        let big = read(&workspace, "big").unwrap();
        // One 70 KiB line does not fit the cap: nothing is shown, the tail
        // says how to continue, and no partial line is emitted.
        assert!(big.starts_with("[lines 1-0 of 1]\n"), "{big}");
        assert!(
            big.ends_with("[1 more lines; continue with offset=1]"),
            "{big}"
        );
        assert_eq!(read(&workspace, "src"), Err("not a regular file"));
        let state = Err("path is inside ahi's own directories, which tools cannot read");
        assert_eq!(read(&workspace, "state/models.json"), state);
        assert_eq!(list(&workspace, "state"), state);
        for (tool, path) in [
            (READ_FILE, "STATE/models.json"),
            (LIST_DIR, "State"),
            (READ_FILE, "SRC/main.rs"),
        ] {
            let result = if tool == READ_FILE {
                read(&workspace, path)
            } else {
                list(&workspace, path)
            };
            assert!(
                matches!(
                    result,
                    Err("path must be spelled exactly as on disk inside the working directory; symbolic links and case variants are refused")
                        | Err("path does not exist or cannot be read")
                ),
                "{path}: {result:?}"
            );
        }
        assert_eq!(list(&workspace, "src/main.rs"), Err("not a directory"));
        assert_eq!(
            read(&workspace, "missing"),
            Err("path does not exist or cannot be read")
        );
        #[cfg(unix)]
        {
            let listing = list(&workspace, "").unwrap();
            assert_eq!(listing, "big\nbinary\ninner@\nlink@\nsrc/\nstate/");
            let escaped = "path must be spelled exactly as on disk inside the working directory; symbolic links and case variants are refused";
            assert_eq!(read(&workspace, "link"), Err(escaped));
            assert_eq!(read(&workspace, "inner/main.rs"), Err(escaped));
        }
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn reading_pages_by_line_and_names_the_next_offset() {
        let (root, workspace) = fixture("tools-paging");
        let lines: Vec<String> = (1..=10).map(|n| format!("line {n}")).collect();
        fs::write(root.join("ten.txt"), lines.join("\n") + "\n").unwrap();
        fs::write(root.join("no-newline.txt"), "a\nb").unwrap();
        fs::write(root.join("empty.txt"), "").unwrap();
        let page = |path: &str, offset, limit| {
            execute(
                &PreparedCall {
                    tool: READ_FILE,
                    path: path.into(),
                    arguments: Arguments::Read { offset, limit },
                },
                &workspace,
            )
        };
        assert_eq!(
            page("ten.txt", None, None).unwrap(),
            lines.join("\n") + "\n",
            "whole file: verbatim, no header"
        );
        assert_eq!(
            page("ten.txt", Some(3), Some(2)).unwrap(),
            "[lines 3-4 of 10]\nline 3\nline 4\n[6 more lines; continue with offset=5]"
        );
        assert_eq!(
            page("ten.txt", Some(9), None).unwrap(),
            "[lines 9-10 of 10]\nline 9\nline 10\n"
        );
        assert_eq!(
            page("ten.txt", None, Some(10)).unwrap(),
            format!("[lines 1-10 of 10]\n{}\n", lines.join("\n"))
        );
        assert_eq!(
            page("ten.txt", Some(11), None),
            Err("offset is past the end of the file")
        );
        assert_eq!(page("no-newline.txt", None, None).unwrap(), "a\nb");
        assert_eq!(page("empty.txt", None, None).unwrap(), "");
        // A file over the cap pages by bytes and says where to resume.
        let many: String = (1..=3000).map(|n| format!("{n:0>30}\n")).collect();
        fs::write(root.join("many.txt"), &many).unwrap();
        let first = page("many.txt", None, None).unwrap();
        let shown = first.lines().count() - 2;
        assert!(
            first.starts_with(&format!("[lines 1-{shown} of 3000]\n")),
            "{}",
            &first[..40]
        );
        assert!(
            first.ends_with(&format!(
                "[{} more lines; continue with offset={}]",
                3000 - shown,
                shown + 1
            )),
            "{}",
            &first[first.len() - 60..]
        );
        assert!(first.len() <= read_file::MAX_BYTES + 128);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finding_and_searching_are_sorted_bounded_and_honor_gitignore() {
        let (root, workspace) = fixture("tools-search");
        fs::create_dir_all(root.join("src/deep")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {\n    helper();\n}\n").unwrap();
        fs::write(
            root.join("src/deep/lib.rs"),
            "pub fn helper() {}\nfn Main() {}\n",
        )
        .unwrap();
        fs::write(root.join("src/notes.log"), "fn main\n").unwrap();
        fs::write(root.join("target/debug/out.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("docs/guide.md"), "Call `fn main` first.\n").unwrap();
        fs::write(
            root.join("docs/image.bin"),
            [0x66, 0x6e, 0x20, 0x6d, 0x61, 0x69, 0x6e, 0x00],
        )
        .unwrap();
        let find = |path: &str, pattern: &str, limit| {
            execute(
                &PreparedCall {
                    tool: FIND_FILES,
                    path: path.into(),
                    arguments: Arguments::Find {
                        pattern: pattern.into(),
                        limit,
                    },
                },
                &workspace,
            )
            .unwrap()
        };
        assert_eq!(find("", "*.rs", 200), "src/deep/lib.rs\nsrc/main.rs");
        assert_eq!(find("src", "*.rs", 200), "src/deep/lib.rs\nsrc/main.rs");
        assert_eq!(
            find("", "src/*.rs", 200),
            "src/main.rs",
            "anchored to one level"
        );
        assert_eq!(
            find("src", "deep/*", 200),
            "src/deep/lib.rs",
            "anchored below the searched directory"
        );
        assert_eq!(find("", "*.log", 200), "[no matches]", "gitignored");
        assert_eq!(
            find("", "models.json", 200),
            "[no matches]",
            "ahi directory pruned"
        );
        assert_eq!(find("", "d*", 200), "docs/\nsrc/deep/");
        assert_eq!(
            find("", "*.rs", 1),
            "src/deep/lib.rs\n[1 more matches not shown; raise limit or narrow the pattern]"
        );
        let search =
            |path: &str, pattern: &str, literal, ignore_case, glob: Option<&str>, limit| {
                execute(
                    &PreparedCall {
                        tool: SEARCH_FILES,
                        path: path.into(),
                        arguments: Arguments::Search {
                            pattern: pattern.into(),
                            literal,
                            ignore_case,
                            glob: glob.map(str::to_string),
                            limit,
                        },
                    },
                    &workspace,
                )
            };
        assert_eq!(
            search("", "fn main", true, false, None, 100).unwrap(),
            "docs/guide.md:1:Call `fn main` first.\nsrc/main.rs:1:fn main() {\n[1 binary or oversized files skipped]"
        );
        assert_eq!(
            search("", "fn main", true, false, Some("*.rs"), 100).unwrap(),
            "src/main.rs:1:fn main() {"
        );
        assert_eq!(
            search("", "fn main", false, true, Some("*.rs"), 100).unwrap(),
            "src/deep/lib.rs:2:fn Main() {}\nsrc/main.rs:1:fn main() {"
        );
        assert_eq!(
            search("src", r"fn \w+\(\)", false, false, None, 100).unwrap(),
            "src/deep/lib.rs:1:pub fn helper() {}\nsrc/deep/lib.rs:2:fn Main() {}\nsrc/main.rs:1:fn main() {"
        );
        assert_eq!(
            search("src", r"fn \w+\(\)", false, false, None, 1).unwrap(),
            "src/deep/lib.rs:1:pub fn helper() {}\n[2 more matches not shown; raise limit or narrow the path, glob, or pattern]"
        );
        assert_eq!(
            search("", "fn main(", false, false, None, 100),
            Err("invalid regular expression")
        );
        assert_eq!(
            search("", "nothing here", true, false, None, 100).unwrap(),
            "[no matches]\n[1 binary or oversized files skipped]"
        );
        assert_eq!(
            search("src/main.rs", "x", true, false, None, 1),
            Err("not a directory")
        );
        fs::remove_dir_all(root).unwrap();
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

#[cfg(test)]
mod timing {
    //! `cargo test --lib tools::timing -- --ignored --nocapture` prints how
    //! long a walk and a search of this repository take.
    use super::*;
    use std::path::PathBuf;
    use std::time::Instant;

    #[test]
    #[ignore]
    fn walk_and_search_this_repository() {
        let root = std::env::var_os("AHI_TIMING_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        let workspace = Workspace::open(
            &root,
            &crate::directories::Directories::single_root(root.join("target/ahi-timing-state")),
        )
        .unwrap();
        for round in 1..=3 {
            println!("round {round}");
            let started = Instant::now();
            let entries = walk::walk(&workspace, &root, "").unwrap();
            println!(
                "  walk: {} entries in {:?}",
                entries.len(),
                started.elapsed()
            );
            let started = Instant::now();
            let mut bytes = 0usize;
            let mut buffer = Vec::new();
            let mut path = PathBuf::new();
            for relative in entries.files() {
                buffer.clear();
                walk::absolute(&workspace, relative, &mut path);
                if let Ok(mut file) = std::fs::File::open(&path) {
                    use std::io::Read;
                    let _ = file.read_to_end(&mut buffer);
                    bytes += buffer.len();
                }
            }
            println!(
                "  baseline open+read all files: {bytes} bytes in {:?}",
                started.elapsed()
            );
            let started = Instant::now();
            let found = find_files::run(&workspace, &root, "", "*.rs", 1000).unwrap();
            println!(
                "  find *.rs: {} lines in {:?}",
                found.lines().count(),
                started.elapsed()
            );
            let started = Instant::now();
            let query = search_files::Query {
                pattern: "fn main",
                literal: true,
                ignore_case: false,
                glob: None,
                limit: 500,
            };
            let hits = search_files::run(&workspace, &root, "", &query).unwrap();
            println!(
                "  search 'fn main': {} lines in {:?}",
                hits.lines().count(),
                started.elapsed()
            );
            let started = Instant::now();
            let query = search_files::Query {
                pattern: r"pub\s+fn\s+\w+",
                literal: false,
                ignore_case: false,
                glob: Some("*.rs"),
                limit: 500,
            };
            let hits = search_files::run(&workspace, &root, "", &query).unwrap();
            println!(
                "  search regex in *.rs: {} lines in {:?}",
                hits.lines().count(),
                started.elapsed()
            );
        }
    }
}
