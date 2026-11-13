//! Builtin model tools. Tools are compiled into ahi; repository configuration
//! may only select and bound them (see `crate::tool_policy`). Four read, two
//! write, one runs a program the policy names by argv prefix.
//!
//! Preparation is lexical and touches no file, so an unapproved call learns
//! nothing about the workspace. Execution re-checks containment on disk.
//! Every tool is deterministic: sorted output, fixed caps, no shell. Writes
//! are atomic, never follow a symbolic link, and are confined to the working
//! directory; a few paths are protected from the model whatever the rules
//! say, because a model that could edit its own policy would have none.

pub(crate) mod delegate;
mod edit_file;
mod find_files;
mod glob;
mod ignore;
mod list_dir;
mod read_file;
mod run_command;
mod search_files;
mod walk;
mod workspace;
mod write_file;

use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) use run_command::{render as render_argv, CommandReport};
use std::sync::atomic::AtomicBool;
pub(super) use workspace::Workspace;
pub(crate) use write_file::WriteReport;

use super::ToolCall;
use crate::tool_policy::normalize_relative_path;

pub(crate) const READ_FILE: &str = "read_file";
pub(crate) const LIST_DIR: &str = "list_dir";
pub(crate) const FIND_FILES: &str = "find_files";
pub(crate) const SEARCH_FILES: &str = "search_files";
pub(crate) const EDIT_FILE: &str = "edit_file";
pub(crate) const WRITE_FILE: &str = "write_file";
pub(crate) const RUN_COMMAND: &str = "run_command";
pub(crate) const DELEGATE: &str = "delegate";
/// The builtin tool that reads a skill's text, governed by `action: skill`
/// rules matched on the skill's name. Text, never authority.
pub(crate) const LOAD_SKILL: &str = crate::tool_policy::SKILL_TOOL;

pub(crate) const BUILTIN: [&str; 9] = [
    READ_FILE,
    LIST_DIR,
    FIND_FILES,
    SEARCH_FILES,
    EDIT_FILE,
    WRITE_FILE,
    RUN_COMMAND,
    DELEGATE,
    LOAD_SKILL,
];

/// Files the model may never change, whatever `ahi.tools.yaml` says: the
/// policy files themselves, the hook configuration, the ignore file the walk
/// trusts, and everything under `.git`. Matched on the normalized path.
pub(crate) const PROTECTED: [&str; 7] = [
    "ahi.tools.yaml",
    "ahi.hooks.yaml",
    "ahi.dev.yaml",
    "ahi.actions.yaml",
    "ahi.policy.yaml",
    ".pre-commit-config.yaml",
    ".gitignore",
];

fn protected(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    PROTECTED.contains(&name)
        || path.split('/').any(|part| part == ".git")
        || crate::skills::protected_write(path)
}

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
    Edit {
        old: String,
        new: String,
        replace_all: bool,
    },
    Write {
        content: String,
        overwrite: bool,
    },
    Command {
        argv: Vec<String>,
        timeout_secs: u64,
    },
    /// `load_skill`: `name` holds the one skill name, as a list so policy
    /// and grants match it the way they match an argv prefix; `file` names
    /// a declared reference instead of the skill's own text.
    Skill {
        name: Vec<String>,
        file: Option<String>,
    },
    /// A task for a worker. `worker` holds the one label, as a list so
    /// policy matches it the way it matches an argv prefix.
    Delegate {
        worker: Vec<String>,
        prompt: String,
        /// The worker session to continue, from an earlier result.
        task: Option<String>,
        timeout_secs: u64,
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
        match &self.arguments {
            Arguments::Delegate { worker, .. } => return &worker[0],
            Arguments::Skill { name, .. } => return &name[0],
            _ => {}
        }
        if self.path.is_empty() {
            "."
        } else {
            &self.path
        }
    }

    /// The argv of a `run_command` call, the worker label of a `delegate`
    /// call, empty for every other tool. This is what command and delegate
    /// rules and session grants match.
    pub(super) fn argv(&self) -> &[String] {
        match &self.arguments {
            Arguments::Command { argv, .. } => argv,
            Arguments::Delegate { worker, .. } => worker,
            Arguments::Skill { name, .. } => name,
            _ => &[],
        }
    }

    /// What the call would do, from its arguments alone, for the approval
    /// prompt: line counts for a write, the command line for a command,
    /// nothing for a read. No file is touched. The command line is the one
    /// argument the operator must see to decide, so it is shown.
    pub(super) fn detail(&self) -> String {
        match &self.arguments {
            Arguments::Command { argv, .. } => run_command::render(argv),
            Arguments::Skill { file, .. } => match file {
                Some(file) => format!("reference {file}"),
                None => "the skill's text".to_string(),
            },
            Arguments::Delegate { prompt, task, .. } => format!(
                "{} lines{}",
                write_file::line_count(prompt),
                match task {
                    Some(task) => format!(", continuing task {task}"),
                    None => String::new(),
                }
            ),
            Arguments::Edit { old, new, .. } => format!(
                "+{} -{} lines",
                write_file::line_count(new),
                write_file::line_count(old)
            ),
            Arguments::Write { content, overwrite } => format!(
                "{} {} lines",
                if *overwrite {
                    "replace with"
                } else {
                    "new file,"
                },
                write_file::line_count(content)
            ),
            _ => String::new(),
        }
    }

    /// The narrowest useful session-grant prefix for this call: the parent
    /// directory of a file, or the directory listed or searched.
    pub(super) fn grant_prefix(&self) -> String {
        match self.tool {
            READ_FILE | EDIT_FILE | WRITE_FILE => self
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
    pub(crate) description: std::borrow::Cow<'static, str>,
    /// A JSON Schema object for the arguments.
    pub(crate) parameters: Value,
}

/// Schemas for the enabled builtins, in builtin order. `skills` are the
/// approved, valid skills the `load_skill` description lists: the only
/// skill text in a request before a load.
pub(super) fn schemas(enabled: &[String], skills: &[crate::skills::Skill]) -> Vec<ToolSchema> {
    let string = |description: &str| json!({"type": "string", "description": description});
    let integer = |description: &str, minimum: usize| json!({"type": "integer", "minimum": minimum, "description": description});
    BUILTIN
        .iter()
        .filter(|name| enabled.iter().any(|tool| tool == *name))
        .map(|name| match *name {
            LOAD_SKILL => ToolSchema {
                name: LOAD_SKILL,
                description: skill_description(skills).into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "name": string("The skill's name, one of those listed"),
                        "file": string("A reference file the skill declares, instead of the skill's own text")
                    },
                    "required": ["name"],
                    "additionalProperties": false
                }),
            },
            READ_FILE => ToolSchema {
                name: READ_FILE,
                description: "Read a UTF-8 text file inside the working directory, whole or a window of lines. Output is capped at 64 KiB; a tail line names the next offset when more remains.".into(),
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
                description: "List a directory inside the working directory. Directories end with '/', symbolic links with '@'. At most 500 entries.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"path": string("Directory relative to the working directory; defaults to '.'")},
                    "additionalProperties": false
                }),
            },
            FIND_FILES => ToolSchema {
                name: FIND_FILES,
                description: "Find files under a directory by glob. A pattern without '/' matches names at any depth ('*.rs'); with '/' it matches paths below the directory ('src/**/*.rs'). Honors .gitignore; never follows symbolic links. Sorted; default 200 results, maximum 1000.".into(),
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
            SEARCH_FILES => ToolSchema {
                name: SEARCH_FILES,
                description: "Search text files under a directory for a regular expression (Rust syntax) or a literal string. Returns 'path:line:text' with files sorted and lines ascending; binary files and .gitignore'd paths are skipped. Default 100 matches, maximum 500, output capped at 64 KiB.".into(),
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
            EDIT_FILE => ToolSchema {
                name: EDIT_FILE,
                description: "Replace text in a UTF-8 file inside the working directory. old must occur exactly once (include enough surrounding lines to make it unique) unless replace_all is true. Exact match: no whitespace normalization. Fails with the count when old is absent or ambiguous.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": string("File path relative to the working directory"),
                        "old": string("Exact text to replace"),
                        "new": string("Replacement text"),
                        "replace_all": {"type": "boolean", "description": "Replace every occurrence instead of requiring exactly one"}
                    },
                    "required": ["path", "old", "new"],
                    "additionalProperties": false
                }),
            },
            WRITE_FILE => ToolSchema {
                name: WRITE_FILE,
                description: "Create a UTF-8 file inside the working directory with the given content. Fails if the file exists unless overwrite is true; with overwrite the file must already exist. Parent directories must exist. Prefer edit_file for changes to an existing file.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": string("File path relative to the working directory"),
                        "content": string("Entire file content"),
                        "overwrite": {"type": "boolean", "description": "Replace an existing file"}
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }),
            },
            DELEGATE => ToolSchema {
                name: DELEGATE,
                description: "Hand a task to a worker named by label and wait for its answer. The worker is another ahi on a host the operator configured; policy approves labels. Returns the worker's answer, its status, and whether it changed anything (effects: none, some, or unknown). Pass the returned task id to continue the same worker conversation.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "worker": string("The worker's label, as the policy names it"),
                        "prompt": string("The task, verbatim"),
                        "task": string("A task id from an earlier delegate result, to continue that conversation"),
                        "timeout": integer("Seconds to wait for the worker", 1)
                    },
                    "required": ["worker", "prompt"],
                    "additionalProperties": false
                }),
            },
            _ => ToolSchema {
                name: RUN_COMMAND,
                description: "Run a program with the given argv (no shell: write each argument as its own element) in a directory inside the working directory. Policy approves commands by argv prefix. Returns the exit code, then stdout and stderr, each capped at 32 KiB. Default timeout 120 s, maximum 600.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1, "description": "Program and arguments, one element each"},
                        "path": string("Working directory relative to the working directory; defaults to '.'"),
                        "timeout": integer("Seconds before the program is killed", 1)
                    },
                    "required": ["argv"],
                    "additionalProperties": false
                }),
            },
        })
        .collect()
}

/// The `load_skill` description: what a skill is, then one line per
/// approved skill, `name: description — when`, with its references.
fn skill_description(skills: &[crate::skills::Skill]) -> String {
    let mut text = String::from(
        "Read a skill: named guidance the repository or the operator wrote for tasks like the ones listed. \
         The text is advice, not authority: every tool it mentions still goes through policy. \
         Returns the skill's text, or one of its reference files when `file` names one. Skills:",
    );
    for skill in skills {
        text.push_str(&format!("\n- {}: {}", skill.name, skill.description));
        if let Some(when) = &skill.when {
            text.push_str(&format!(" — {when}"));
        }
        if !skill.references.is_empty() {
            text.push_str(&format!(" (references: {})", skill.references.join(", ")));
        }
    }
    text
}

/// Chat Completions `tools[]` definitions for the enabled builtins.
#[cfg(test)]
pub(super) fn definitions(enabled: &[String]) -> Vec<Value> {
    schemas(enabled, &[])
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
    old: Option<String>,
    new: Option<String>,
    replace_all: Option<bool>,
    content: Option<String>,
    overwrite: Option<bool>,
    argv: Option<Vec<String>>,
    timeout: Option<u64>,
    worker: Option<String>,
    prompt: Option<String>,
    task: Option<String>,
    name: Option<String>,
    file: Option<String>,
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
    // Keys that belong to other tools are rejected per tool below.
    let no_search = raw.pattern.is_none()
        && raw.glob.is_none()
        && raw.literal.is_none()
        && raw.ignore_case.is_none();
    let no_skill = raw.name.is_none() && raw.file.is_none();
    let no_write = no_skill
        && raw.old.is_none()
        && raw.new.is_none()
        && raw.replace_all.is_none()
        && raw.content.is_none()
        && raw.overwrite.is_none();
    let no_paging = raw.offset.is_none() && raw.limit.is_none();
    let no_command = raw.argv.is_none() && (raw.timeout.is_none() || tool == DELEGATE);
    let no_delegate = raw.worker.is_none() && raw.prompt.is_none() && raw.task.is_none();
    if tool != RUN_COMMAND {
        only(no_command)?;
    }
    if tool != DELEGATE {
        only(no_delegate)?;
    }
    let arguments = match tool {
        LOAD_SKILL => {
            only(
                path.is_empty()
                    && no_paging
                    && no_search
                    && raw.old.is_none()
                    && raw.new.is_none()
                    && raw.replace_all.is_none()
                    && raw.content.is_none()
                    && raw.overwrite.is_none()
                    && raw.argv.is_none()
                    && raw.timeout.is_none()
                    && raw.worker.is_none()
                    && raw.prompt.is_none()
                    && raw.task.is_none(),
            )?;
            let name = raw.name.ok_or(INVALID)?;
            if !crate::hooks::policy::valid_rule_id(&name) {
                return Err("name is a skill name of 1 to 64 letters, digits, '-', '_', or '.'");
            }
            if let Some(file) = &raw.file {
                if file.is_empty() || file.len() > 128 || file.contains(['/', '\\', '\0']) {
                    return Err("file is a bare file name the skill declares as a reference");
                }
            }
            Arguments::Skill {
                name: vec![name],
                file: raw.file,
            }
        }
        DELEGATE => {
            only(path.is_empty() && no_paging && no_search && no_write)?;
            let worker = raw.worker.ok_or(INVALID)?;
            if !crate::hooks::policy::valid_rule_id(&worker) {
                return Err("worker is a label of 1 to 64 letters, digits, '-', '_', or '.'");
            }
            let prompt = raw.prompt.ok_or(INVALID)?;
            if prompt.trim().is_empty() {
                return Err("prompt must not be empty");
            }
            if prompt.len() > delegate::MAX_PROMPT_BYTES {
                return Err("prompt is larger than 1 MiB");
            }
            if let Some(task) = &raw.task {
                if !crate::session::valid_id(task) {
                    return Err("task is a session id from an earlier delegate result");
                }
            }
            let timeout_secs = match raw.timeout {
                None => delegate::DEFAULT_TIMEOUT_SECS,
                Some(0) => return Err(INVALID),
                Some(secs) => secs.min(delegate::MAX_TIMEOUT_SECS),
            };
            Arguments::Delegate {
                worker: vec![worker],
                prompt,
                task: raw.task,
                timeout_secs,
            }
        }
        RUN_COMMAND => {
            only(no_paging && no_search && no_write)?;
            let argv = raw.argv.ok_or(INVALID)?;
            if argv.is_empty() || argv.len() > run_command::MAX_ARGV {
                return Err("argv must have 1 to 64 elements");
            }
            if argv[0].is_empty() || argv.iter().any(|token| token.contains('\0')) {
                return Err("argv tokens must be text without NUL and the program non-empty");
            }
            if argv.iter().map(String::len).sum::<usize>() > run_command::MAX_ARGV_BYTES {
                return Err("argv is larger than 8 KiB");
            }
            let timeout_secs = match raw.timeout {
                None => run_command::DEFAULT_TIMEOUT_SECS,
                Some(0) => return Err(INVALID),
                Some(secs) => secs.min(run_command::MAX_TIMEOUT_SECS),
            };
            Arguments::Command { argv, timeout_secs }
        }
        READ_FILE => {
            if path.is_empty() {
                return Err("read_file needs a file path");
            }
            only(no_search && no_write)?;
            Arguments::Read {
                offset: small(raw.offset)?,
                limit: small(raw.limit)?,
            }
        }
        LIST_DIR => {
            only(no_paging && no_search && no_write)?;
            Arguments::List
        }
        FIND_FILES => {
            only(
                raw.offset.is_none()
                    && raw.glob.is_none()
                    && raw.literal.is_none()
                    && raw.ignore_case.is_none()
                    && no_write,
            )?;
            let pattern = raw.pattern.ok_or(INVALID)?;
            glob::Glob::new(&pattern).ok_or("invalid glob pattern")?;
            Arguments::Find {
                pattern,
                limit: small(raw.limit)?.unwrap_or(find_files::DEFAULT_LIMIT),
            }
        }
        EDIT_FILE | WRITE_FILE => {
            if path.is_empty() {
                return Err("a file path is required");
            }
            if protected(&path) {
                return Err("path is protected: ahi policy files, hook configuration, .gitignore, .git, and .ahi/skills are never written by tools");
            }
            only(no_paging && no_search)?;
            if tool == EDIT_FILE {
                only(raw.content.is_none() && raw.overwrite.is_none())?;
                let old = raw.old.ok_or(INVALID)?;
                let new = raw.new.ok_or(INVALID)?;
                if old.is_empty() {
                    return Err("old must not be empty");
                }
                if old.len() > write_file::MAX_WRITE_BYTES
                    || new.len() > write_file::MAX_WRITE_BYTES
                {
                    return Err("text is larger than 8 MiB");
                }
                Arguments::Edit {
                    old,
                    new,
                    replace_all: raw.replace_all.unwrap_or(false),
                }
            } else {
                only(raw.old.is_none() && raw.new.is_none() && raw.replace_all.is_none())?;
                let content = raw.content.ok_or(INVALID)?;
                if content.len() > write_file::MAX_WRITE_BYTES {
                    return Err("text is larger than 8 MiB");
                }
                if content.contains('\0') {
                    return Err("content must be text without NUL");
                }
                Arguments::Write {
                    content,
                    overwrite: raw.overwrite.unwrap_or(false),
                }
            }
        }
        _ => {
            only(raw.offset.is_none() && no_write)?;
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

/// A finished call: what the model receives, and for a write or a command,
/// what happened, for the audit log and for the screen.
pub(super) struct Executed {
    pub(super) content: String,
    pub(super) write: Option<WriteReport>,
    pub(super) command: Option<CommandReport>,
    pub(super) delegation: Option<delegate::DelegationReport>,
    pub(super) detail: Option<ToolDetail>,
}

/// A call that did not finish: the text the model and the operator see,
/// and for a command whether a process ran first. `None` is "not known from
/// here": a read or a write that failed in the workspace, where nothing
/// about a process applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ToolFailure {
    pub(super) message: &'static str,
    pub(super) ran: Option<bool>,
}

impl From<&'static str> for ToolFailure {
    fn from(message: &'static str) -> Self {
        Self { message, ran: None }
    }
}

impl From<run_command::RunError> for ToolFailure {
    fn from(error: run_command::RunError) -> Self {
        Self {
            message: error.message,
            ran: Some(error.ran),
        }
    }
}

/// What the screen draws for a finished call beyond its text: the exact
/// texts a write replaced, the separated streams of a command, or the
/// query a search ran. Never sent to a model; `content` already carries
/// what the model needs. Bounded by the caps of the tool that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolDetail {
    Write {
        /// `None` for a created file.
        before: Option<String>,
        after: String,
    },
    Command {
        argv: Vec<String>,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
        stdout_cut: bool,
        stderr_cut: bool,
    },
    Search {
        pattern: String,
        literal: bool,
        glob: Option<String>,
        matches: usize,
    },
    Find {
        pattern: String,
        matches: usize,
    },
    Read {
        offset: Option<usize>,
        limit: Option<usize>,
        lines: usize,
    },
}

/// Run an approved call. `toolchain_env` is the approving command rule's
/// pass-through list; other tools ignore it. `cancel` is the turn's flag: a
/// running command is killed when it is set. `Err` text is safe to show and
/// to send to the model.
pub(super) fn execute(
    call: &PreparedCall,
    workspace: &Workspace,
    skills: &crate::skills::Catalog,
    toolchain_env: &[String],
    cancel: &AtomicBool,
) -> Result<Executed, ToolFailure> {
    // Result rows are one per line; headers and continuation notes are not
    // matches, so counts are approximate for the screen header only.
    let matches = |content: &str| content.lines().filter(|l| !l.is_empty()).count();
    match &call.arguments {
        Arguments::Skill { name, file } => {
            // The catalog was built at turn start from the workdir and the
            // operator root; a name the policy approves but no root holds is
            // not loadable, and the text is read again with every check.
            let skill = skills
                .get(&name[0])
                .ok_or("no valid skill of that name is installed; `ahi doctor` lists them")?;
            if file
                .as_deref()
                .is_some_and(|file| !skill.references.iter().any(|declared| declared == file))
            {
                return Err("the skill declares no reference of that name".into());
            }
            let text = crate::skills::read(skill, file.as_deref())
                .map_err(|_| "the skill could not be read; `ahi doctor` says why")?;
            let digest = crate::session::sha256_hex(text.as_bytes());
            let content = match file {
                Some(file) => format!(
                    "skill {} reference {file} from {} ({} bytes, sha256 {})\n{text}",
                    skill.name,
                    skill.root.as_str(),
                    text.len(),
                    &digest[..12]
                ),
                None => format!(
                    "skill {} from {} ({} bytes, sha256 {})\n{text}",
                    skill.name,
                    skill.root.as_str(),
                    text.len(),
                    &digest[..12]
                ),
            };
            Ok(Executed {
                content,
                detail: None,
                write: None,
                command: None,
                delegation: None,
            })
        }
        Arguments::Delegate {
            worker,
            prompt,
            task,
            timeout_secs,
        } => {
            let outcome =
                delegate::run(&worker[0], prompt, task.as_deref(), *timeout_secs, cancel)?;
            Ok(Executed {
                content: outcome.content,
                detail: None,
                write: None,
                command: None,
                delegation: Some(outcome.report),
            })
        }
        Arguments::Command { argv, timeout_secs } => {
            let cwd = workspace
                .resolve(&call.path)
                .map_err(|message| ToolFailure {
                    message,
                    ran: Some(false),
                })?;
            let (content, report, output) =
                run_command::run(argv, &cwd, *timeout_secs, toolchain_env, cancel)?;
            Ok(Executed {
                content,
                detail: Some(ToolDetail::Command {
                    argv: argv.clone(),
                    exit_code: report.exit_code,
                    stdout: output.stdout,
                    stderr: output.stderr,
                    stdout_cut: output.stdout_cut,
                    stderr_cut: output.stderr_cut,
                }),
                write: None,
                command: Some(report),
                delegation: None,
            })
        }
        Arguments::Edit {
            old,
            new,
            replace_all,
        } => {
            let (path, exists) = workspace.resolve_for_write(&call.path)?;
            if !exists {
                return Err("path does not exist or cannot be read".into());
            }
            let (content, report, texts) = edit_file::run(&path, old, new, *replace_all)?;
            Ok(Executed {
                content,
                write: Some(report),
                command: None,
                delegation: None,
                detail: Some(ToolDetail::Write {
                    before: texts.before,
                    after: texts.after,
                }),
            })
        }
        Arguments::Write { content, overwrite } => {
            let (path, exists) = workspace.resolve_for_write(&call.path)?;
            let (content, report, texts) = write_file::run(&path, content, *overwrite, exists)?;
            Ok(Executed {
                content,
                write: Some(report),
                command: None,
                delegation: None,
                detail: Some(ToolDetail::Write {
                    before: texts.before,
                    after: texts.after,
                }),
            })
        }
        Arguments::Read { offset, limit } => {
            let path = workspace.resolve(&call.path)?;
            let content = read_file::run(&path, *offset, *limit)?;
            Ok(Executed {
                detail: Some(ToolDetail::Read {
                    offset: *offset,
                    limit: *limit,
                    lines: content.lines().count(),
                }),
                content,
                write: None,
                command: None,
                delegation: None,
            })
        }
        Arguments::List => {
            let path = workspace.resolve(&call.path)?;
            let content = list_dir::run(&path)?;
            Ok(Executed {
                content,
                write: None,
                command: None,
                delegation: None,
                detail: None,
            })
        }
        Arguments::Find { pattern, limit } => {
            let path = workspace.resolve(&call.path)?;
            let content = find_files::run(workspace, &path, &call.path, pattern, *limit)?;
            Ok(Executed {
                detail: Some(ToolDetail::Find {
                    pattern: pattern.clone(),
                    matches: matches(&content),
                }),
                content,
                write: None,
                command: None,
                delegation: None,
            })
        }
        Arguments::Search {
            pattern,
            literal,
            ignore_case,
            glob,
            limit,
        } => {
            let path = workspace.resolve(&call.path)?;
            let content = search_files::run(
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
            )?;
            Ok(Executed {
                detail: Some(ToolDetail::Search {
                    pattern: pattern.clone(),
                    literal: *literal,
                    glob: glob.clone(),
                    matches: matches(&content),
                }),
                content,
                write: None,
                command: None,
                delegation: None,
            })
        }
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

    /// Tests never cancel a call unless they say so.
    static NEVER: AtomicBool = AtomicBool::new(false);

    fn run_text(call: &PreparedCall, workspace: &Workspace) -> Result<String, &'static str> {
        execute(
            call,
            workspace,
            &crate::skills::Catalog::default(),
            &[],
            &NEVER,
        )
        .map(|executed| executed.content)
        .map_err(|failure| failure.message)
    }

    fn read(workspace: &Workspace, path: &str) -> Result<String, &'static str> {
        run_text(
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
        run_text(
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
            run_text(
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
            run_text(
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
                run_text(
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
    fn writes_are_exact_atomic_confined_and_protected() {
        let (root, workspace) = fixture("tools-write");
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn a() {}\nfn b() {}\nfn a() {}\n").unwrap();
        fs::write(root.join("ahi.tools.yaml"), "version: 1\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("src/lib.rs"), root.join("link.rs")).unwrap();
        let edit = |path: &str, old: &str, new: &str, replace_all| {
            execute(
                &PreparedCall {
                    tool: EDIT_FILE,
                    path: path.into(),
                    arguments: Arguments::Edit {
                        old: old.into(),
                        new: new.into(),
                        replace_all,
                    },
                },
                &workspace,
                &crate::skills::Catalog::default(),
                &[],
                &NEVER,
            )
            .map_err(|failure| failure.message)
        };
        let write = |path: &str, content: &str, overwrite| {
            execute(
                &PreparedCall {
                    tool: WRITE_FILE,
                    path: path.into(),
                    arguments: Arguments::Write {
                        content: content.into(),
                        overwrite,
                    },
                },
                &workspace,
                &crate::skills::Catalog::default(),
                &[],
                &NEVER,
            )
            .map_err(|failure| failure.message)
        };
        // Ambiguous, then unique, then replace_all.
        assert_eq!(
            edit("src/lib.rs", "fn a() {}", "fn c() {}", false).err(),
            Some("old text occurs more than once; include more context or set replace_all")
        );
        let done = edit("src/lib.rs", "fn b() {}", "fn b() {}\nfn d() {}", false).unwrap();
        assert_eq!(done.content, "edited lib.rs: 1 replacement, +2 -1 lines");
        let report = done.write.unwrap();
        assert!(report.before.is_some());
        assert_eq!((report.lines_added, report.lines_removed), (2, 1));
        assert_eq!(
            fs::read_to_string(root.join("src/lib.rs")).unwrap(),
            "fn a() {}\nfn b() {}\nfn d() {}\nfn a() {}\n"
        );
        let all = edit("src/lib.rs", "fn a() {}", "fn z() {}", true).unwrap();
        assert_eq!(all.content, "edited lib.rs: 2 replacements, +2 -2 lines");
        assert_eq!(
            edit("src/lib.rs", "nowhere", "x", false).err(),
            Some("old text not found in the file")
        );
        assert_eq!(
            edit("src/missing.rs", "a", "b", false).err(),
            Some("path does not exist or cannot be read")
        );
        // Create, refuse to clobber, then overwrite explicitly.
        let created = write("src/new.rs", "pub fn n() {}\n", false).unwrap();
        assert_eq!(created.content, "created new.rs: 1 lines");
        assert!(created.write.unwrap().before.is_none());
        assert_eq!(
            write("src/new.rs", "x", false).err(),
            Some("file exists; set overwrite to replace it")
        );
        let replaced = write("src/new.rs", "a\nb\nc\n", true).unwrap();
        assert_eq!(replaced.content, "replaced new.rs: 3 lines (was 1)");
        assert_eq!(
            write("src/other.rs", "x", true).err(),
            Some("file does not exist; overwrite applies to an existing file")
        );
        assert_eq!(
            write("nodir/x.rs", "x", false).err(),
            Some("path does not exist or cannot be read")
        );
        assert_eq!(
            write("src", "x", true).err(),
            Some("path is not a regular file")
        );
        // Protected paths are refused at preparation, before any policy.
        for path in [
            "ahi.tools.yaml",
            "ahi.hooks.yaml",
            ".pre-commit-config.yaml",
            ".gitignore",
            ".git/config",
            "sub/.git/HEAD",
            "sub/ahi.dev.yaml",
        ] {
            let error = prepare(&call(
                "c",
                WRITE_FILE,
                json!({"path": path, "content": "x"}),
            ))
            .unwrap_err();
            assert!(error.starts_with("path is protected"), "{path}: {error}");
        }
        assert_eq!(
            prepare(&call(
                "c",
                EDIT_FILE,
                json!({"path": "a", "old": "", "new": "b"})
            ))
            .err(),
            Some("old must not be empty")
        );
        assert_eq!(
            prepare(&call(
                "c",
                WRITE_FILE,
                json!({"path": "a", "content": "x", "old": "y"})
            ))
            .err(),
            Some(INVALID)
        );
        let detail = |name, args| prepare(&call("c", name, args)).unwrap().detail();
        assert_eq!(
            detail(EDIT_FILE, json!({"path": "a", "old": "1\n2\n", "new": "x"})),
            "+1 -2 lines"
        );
        assert_eq!(
            detail(WRITE_FILE, json!({"path": "a", "content": "1\n2\n3"})),
            "new file, 3 lines"
        );
        assert_eq!(
            detail(
                WRITE_FILE,
                json!({"path": "a", "content": "", "overwrite": true})
            ),
            "replace with 0 lines"
        );
        assert_eq!(detail(READ_FILE, json!({"path": "a"})), "");
        #[cfg(unix)]
        {
            assert_eq!(
                write("link.rs", "x", true).err(),
                Some("path is a symbolic link; writes never follow links")
            );
            assert_eq!(
                edit("link.rs", "a", "b", false).err(),
                Some("path is a symbolic link; writes never follow links")
            );
            assert_eq!(
                fs::read_to_string(root.join("src/lib.rs")).unwrap(),
                "fn z() {}\nfn b() {}\nfn d() {}\nfn z() {}\n",
                "the link target is untouched"
            );
        }
        assert_eq!(
            write("state/models.json", "{}", true).err(),
            Some("path is inside ahi's own directories, which tools cannot read")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delegate_arguments_name_a_label_a_prompt_and_an_optional_task() {
        let ok = prepare(&call(
            "c",
            DELEGATE,
            json!({"worker": "rust-ci", "prompt": "run the tests\nreport", "timeout": 9999}),
        ))
        .unwrap();
        assert_eq!(ok.target(), "rust-ci");
        assert_eq!(ok.argv(), ["rust-ci"]);
        assert_eq!(ok.detail(), "2 lines");
        assert_eq!(ok.grant_prefix(), "");
        assert!(matches!(
            ok.arguments,
            Arguments::Delegate { timeout_secs, .. } if timeout_secs == delegate::MAX_TIMEOUT_SECS
        ));
        let task = "0f1e2d3c-4b5a-4978-8765-43210fedcba9";
        let continued = prepare(&call(
            "c",
            DELEGATE,
            json!({"worker": "docs", "prompt": "more", "task": task}),
        ))
        .unwrap();
        assert_eq!(
            continued.detail(),
            format!("1 lines, continuing task {task}")
        );
        for (bad, message) in [
            (json!({"worker": "a b", "prompt": "x"}), "label"),
            (json!({"worker": "w", "prompt": "  "}), "empty"),
            (
                json!({"worker": "w", "prompt": "x", "task": "nope"}),
                "session id",
            ),
            (
                json!({"worker": "w", "prompt": "x", "path": "src"}),
                INVALID,
            ),
            (json!({"prompt": "x"}), INVALID),
        ] {
            let error = prepare(&call("c", DELEGATE, bad.clone())).unwrap_err();
            assert!(error.contains(message), "{bad}: {error}");
        }
        assert_eq!(
            prepare(&call(
                "c",
                RUN_COMMAND,
                json!({"argv": ["true"], "worker": "w"})
            ))
            .unwrap_err(),
            INVALID,
            "delegate keys belong to delegate"
        );
    }

    #[test]
    fn command_arguments_are_strict_and_rendered_for_the_prompt() {
        let ok = |args| prepare(&call("c", RUN_COMMAND, args)).unwrap();
        let prepared = ok(json!({"argv": ["cargo", "test", "--all-features"], "path": "crates/x"}));
        assert_eq!(prepared.target(), "crates/x");
        assert_eq!(prepared.grant_prefix(), "crates/x");
        assert_eq!(prepared.argv(), ["cargo", "test", "--all-features"]);
        assert_eq!(prepared.detail(), "cargo test --all-features");
        assert_eq!(
            ok(json!({"argv": ["sh", "-c", "echo 'a b'"], "timeout": 5000})).arguments,
            Arguments::Command {
                argv: vec!["sh".into(), "-c".into(), "echo 'a b'".into()],
                timeout_secs: run_command::MAX_TIMEOUT_SECS
            },
            "timeout is clamped"
        );
        assert_eq!(
            ok(json!({"argv": ["printf", "a b\tc"]})).detail(),
            "printf 'a b?c'",
            "whitespace quoted, control characters replaced"
        );
        assert_eq!(ok(json!({"argv": ["x"]})).detail(), "x");
        assert!(ok(json!({"argv": ["x".repeat(300)]}))
            .detail()
            .ends_with("..."));
        for (args, expected) in [
            (json!({}), INVALID),
            (json!({"argv": []}), "argv must have 1 to 64 elements"),
            (
                json!({"argv": [""]}),
                "argv tokens must be text without NUL and the program non-empty",
            ),
            (
                json!({"argv": ["a\u{0}b"]}),
                "argv tokens must be text without NUL and the program non-empty",
            ),
            (json!({"argv": ["x"], "timeout": 0}), INVALID),
            (json!({"argv": ["x"], "content": "y"}), INVALID),
            (
                json!({"argv": ["x"], "path": "../up"}),
                "path must be relative to the working directory, without `..`",
            ),
            (json!({"argv": "x"}), INVALID),
        ] {
            assert_eq!(
                prepare(&call("c", RUN_COMMAND, args.clone())),
                Err(expected),
                "{args}"
            );
        }
        assert_eq!(
            prepare(&call("c", READ_FILE, json!({"path": "a", "argv": ["x"]}))),
            Err(INVALID),
            "argv belongs to run_command only"
        );
    }

    #[cfg(unix)]
    #[test]
    fn commands_run_without_a_shell_with_a_scrubbed_environment_and_a_timeout() {
        let (root, workspace) = fixture("tools-command");
        fs::create_dir(root.join("sub")).unwrap();
        let run = |argv: &[&str], path: &str, timeout| {
            execute(
                &PreparedCall {
                    tool: RUN_COMMAND,
                    path: path.into(),
                    arguments: Arguments::Command {
                        argv: argv.iter().map(|t| t.to_string()).collect(),
                        timeout_secs: timeout,
                    },
                },
                &workspace,
                &crate::skills::Catalog::default(),
                &["AHI_TEST_TOOLCHAIN".to_string()],
                &NEVER,
            )
            .map_err(|failure| failure.message)
        };
        std::env::set_var("AHI_TEST_SECRET_TOKEN", "leak");
        std::env::set_var("AHI_TEST_TOOLCHAIN", "kept");
        let both = run(
            &["sh", "-c", "printf out; printf err >&2; pwd; exit 3"],
            "sub",
            30,
        )
        .unwrap();
        let cwd = fs::canonicalize(root.join("sub")).unwrap();
        assert_eq!(
            both.content,
            format!("exit 3\n--- stdout\nout{}\n--- stderr\nerr", cwd.display()),
        );
        let report = both.command.unwrap();
        assert_eq!(report.exit_code, Some(3));
        assert!(report.env_names.contains(&"PATH".to_string()));
        assert!(report.env_names.contains(&"AHI_TEST_TOOLCHAIN".to_string()));
        assert!(!report
            .env_names
            .contains(&"AHI_TEST_SECRET_TOKEN".to_string()));
        let env = run(
            &[
                "sh",
                "-c",
                "echo ${AHI_TEST_SECRET_TOKEN:-scrubbed} $AHI_TOOL $PRE_COMMIT",
            ],
            "",
            30,
        )
        .unwrap();
        assert_eq!(env.content, "exit 0\n--- stdout\nscrubbed run_command");
        assert_eq!(
            run(&["true"], "", 30).unwrap().content,
            "exit 0 (no output)"
        );
        assert_eq!(
            run(&["ahi-no-such-program-xyz"], "", 30).err(),
            Some("cannot start the program (not found or not executable)")
        );
        assert_eq!(
            run(&["sh", "-c", "sleep 5"], "", 1).err(),
            Some("command timed out and was killed")
        );
        // A cancel kills the program and everything it started: the
        // grandchild holds stdout open, so a kill of the shell alone would
        // leave this call waiting for its 30 s.
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let started = std::time::Instant::now();
        let cancelled = execute(
            &PreparedCall {
                tool: RUN_COMMAND,
                path: String::new(),
                arguments: Arguments::Command {
                    argv: vec!["sh".into(), "-c".into(), "sleep 30 & sleep 30".into()],
                    timeout_secs: 60,
                },
            },
            &workspace,
            &crate::skills::Catalog::default(),
            &[],
            &cancel,
        )
        .err()
        .unwrap();
        assert_eq!(cancelled.message, run_command::CANCELLED);
        assert_eq!(cancelled.ran, Some(true));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let big = run(&["sh", "-c", "yes | head -c 100000"], "", 30).unwrap();
        assert!(
            big.content.contains("[stdout truncated at 32768 bytes]"),
            "{}",
            &big.content[..60]
        );
        assert!(big.content.len() < 34 * 1024);
        assert_eq!(
            run(&["true"], "state", 30).err(),
            Some("path is inside ahi's own directories, which tools cannot read")
        );
        assert_eq!(
            run(&["true"], "missing", 30).err(),
            Some("path does not exist or cannot be read")
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
