# Tool catalog: what coding-agent harnesses let a model call

Status: survey, 2026-11-07. Input to the `ahi.tools.yaml` build-out beyond `read_file` and `list_dir`. Sections 5.1 and 5.2 shipped after this survey: `read_file` paging, `find_files`, `search_files`, and `edit_file` and `write_file` under `action: file_write` (in process for the alpha, with `match.tool` so creation and editing are approved separately). Section 5.3 remains proposal. Names are spelled as each harness spells them. Details marked *unverified* were not confirmed on a page or source file read during the survey.

Harnesses surveyed: pi (earendil-works/pi, formerly badlogic/pi-mono), Claude Code, OpenAI Codex CLI, OpenCode, Gemini CLI, Aider, Goose, Amp, Cline and Roo Code.

## 1. Tool families

Every harness converges on the same handful of effects. The table groups the built-in tools by what they do to the machine, which is the axis an allowlist has to reason about.

| Family | Effect | pi | Claude Code | Codex CLI | OpenCode | Gemini CLI | Goose | Amp | Cline / Roo |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Read one file | read | `read` | `Read` | via `exec_command` | `read` | `read_file`, `read_many_files` | (via `shell`) | `Read` | `read_file` |
| List a directory | read | `ls` (off by default) | via `Bash` | via `exec_command` | `read` on a dir | `list_directory` | `tree` | `list_directory` (unverified) | `list_files` |
| Find files by name | read | `find` (off) | `Glob` | via `exec_command` | `glob` | `glob` | (via `shell`) | `glob` (unverified) | `list_files recursive` |
| Search file contents | read | `grep` (off) | `Grep` | via `exec_command` | `grep` | `grep_search` | (via `shell`) | `Grep` (unverified) | `search_files` |
| Create or overwrite a file | write | `write` | `Write` | `apply_patch` | `write` | `write_file` | `write` | `create_file` (unverified) | `write_to_file` |
| Replace exact text in a file | write | `edit` (`edits[]{oldText,newText}`) | `Edit` (`old_string`,`new_string`,`replace_all`) | `apply_patch` | `edit` (`oldString`,`newString`,`replaceAll`) | `replace` (`old_string`,`new_string`,`allow_multiple`) | `edit` (`before`,`after`) | `edit_file` (unverified) | `replace_in_file`, `apply_diff`, `search_replace` |
| Multi-file patch | write | | | `apply_patch` (Lark grammar, not JSON) | `patch` (GPT models only) | | | | `apply_patch` |
| Run a shell command | exec | `bash`, `powershell` | `Bash`, `PowerShell` | `exec_command` + `write_stdin` | `shell` (was `bash`) | `run_shell_command` | `shell` | `Bash` | `execute_command` |
| Fetch a URL | network | | `WebFetch` | via `exec_command` | `webfetch` | `web_fetch` | `read_image` on a URL | `read_web_page` (unverified) | `web_fetch` |
| Web search | network | | `WebSearch` | `web_search` | `websearch` | `google_web_search` | | `web_search` (unverified) | `web_search` |
| Ask the operator | other | | `AskUserQuestion` | `request_user_input` | `question` | `ask_user` | | | `ask_followup_question` |
| Plan or todo list | other | | `TodoWrite`, `Task*` | `update_plan` | `todowrite` (experimental) | `write_todos` | | `todo_write` (unverified) | `update_todo_list` |
| Spawn a subagent | other | | `Agent` | `spawn_agent` and friends | `subagent` (was `task`) | | | `Task` | `new_task`, `use_subagents` |
| Load a skill | other | | `Skill` | | `skill` | `activate_skill` | | | `use_skill`, `skill` |
| Widen the sandbox | other | | | `request_permissions` | | | | | |

Observations:

- **Four effects carry all the risk**: file write, command execution, network fetch, and subagent spawn (which inherits the parent's authority). Read-only tools differ only in how much they reveal and how far they reach.
- **Edit is "replace an exact, unique match"** everywhere except Codex and Roo. Parameter names differ (`oldText`/`old_string`/`before`), the contract is the same: the old text must occur exactly once unless a replace-all flag is set, and the tool fails rather than guessing. Goose returns a 20-line preview on a miss; OpenCode may run a formatter afterwards.
- **Codex reads through the shell.** It has no `read_file`, `grep`, or `list_dir` handler on `main`; reading happens inside `exec_command` under an OS sandbox. That is the opposite design from ahi's lexical, in-process readers, and it only works because the sandbox is OS-enforced (Seatbelt, bubblewrap, Windows sandbox).
- **Aider has no tools.** The model emits an edit format (SEARCH/REPLACE, unified diff, whole file) and the operator drives everything else with slash commands. Its "permission" is file membership in the chat.
- **Search tools shell out** to ripgrep or fd in pi, Claude Code, OpenCode, Gemini CLI (`git grep` then `grep` then a JS fallback), and Roo. ahi's no-install posture means a builtin search has to be implemented in the binary.

## 2. Output bounds

Every harness caps what a tool returns to the model, usually by lines and bytes, and several spill the full output to a file the model can page through.

| Harness | Read cap | Command output cap | Overflow handling |
| --- | --- | --- | --- |
| pi | 2000 lines or 50 KB | last 2000 lines or 50 KB | full output saved to a `pi-bash` temp file |
| Claude Code | 2000 lines; about 25,000 tokens per call; refuses a whole file over 256 KB | about 30,000 chars inline, ceiling 150,000 | overflow saved to a file |
| Codex CLI | n/a (shell) | `max_output_tokens` default 10,000, policy-capped | PTY session is resumable with `write_stdin` |
| OpenCode | 2000 lines or 50 KiB per page; lines cut at 2000 chars | truncated, full output kept in a managed file | `offset`/`limit` paging |
| Gemini CLI | *unverified* | *unverified* | |
| Goose | n/a | 2000 lines per stream | overflow saved to a temp file |
| Roo Code | `maxReadFileLine` (UI default 500); 100 KB preview for huge files | throttled; `read_command_output` pages the artifact | `artifact_id` with `search`, `offset`, `limit` |
| ahi today | 64 KiB per read; 500 entries per listing; 8 rounds and 256 KiB per turn | n/a | truncation marker only |

ahi's per-turn budget (rounds and total bytes) is stricter than any surveyed harness and has no paging. Paging (`offset`, `limit`) is the one read-side feature every harness has that ahi lacks.

## 3. Permission models

This is where the harnesses differ most, and where ahi's existing design already sits at one end of the spectrum.

| Harness | Default posture | Rule grammar | Matched on | Where rules live | Session grants |
| --- | --- | --- | --- | --- | --- |
| pi | no approval at all; OS permissions of the process | none declarative; extension hook `tool_call` returns `{block}` | arbitrary code | `~/.pi/agent/settings.json`, `.pi/settings.json` (tool *selection* only) | n/a |
| Claude Code | ask for write and exec; reads inside the working dir are free | `Tool` or `Tool(specifier)` in `allow`/`ask`/`deny` lists; deny > ask > allow | Bash: argv prefix with `*`, shell operators split, wrappers stripped. Paths: gitignore-style globs, `//abs`, `~/`. `WebFetch(domain:...)` | `.claude/settings.json`, `.claude/settings.local.json`, `~/.claude/settings.json`, managed | "don't ask again" for Bash and WebFetch writes an allow rule to the local file; edit approvals last the session |
| Codex CLI | `workspace-write` sandbox plus `on-request` approval | `sandbox_mode` and `approval_policy` are orthogonal; `prefix_rule(pattern, decision)` in Starlark; newer `[permissions.<name>]` with filesystem `read`/`write`/`deny` and network domain lists | argv prefix; simple `&&`, `||`, `;`, `|` chains split; most restrictive decision wins | `~/.codex/config.toml`, `~/.codex/rules/default.rules`, project `.codex/` only when trusted | accept once, or persist a `prefix_rule` |
| OpenCode | `ask` for anything unmatched | ordered `permissions` array of `{action, resource, effect}`; last match wins; any deny wins across resources, else any ask | glob over normalized path, or over the command string for `shell` (`"git status *"` also matches bare `git status`) | `opencode.json` in project, `.opencode/`, `~/.config/opencode/`, managed; agent-level rules appended; `policies` hard-deny last | "saved approvals" mentioned, storage *unverified* |
| Gemini CLI | `default` mode asks for write and exec | legacy `tools.core`/`tools.allowed` with `run_shell_command(git)` prefix form; newer TOML policy engine `[[rule]]` with `toolName`, `commandPrefix`, `commandRegex`, `argsPattern`, `decision`, `priority`, `modes` | command prefix or regex; regex over JSON args | `settings.json` at user, project, system tiers; `~/.gemini/policies/*.toml` | "allow for all future sessions" persists and cascades from stricter to looser modes |
| Aider | Y/N prompt at the moment of adding a file, running a suggested command, adding output, scraping a URL | none | n/a | `.aider.conf.yml`, `--yes-always` | "(D)on't ask again" for the session |
| Goose | `auto`: no prompts | per-tool `always_allow`/`ask_before`/`never_allow` lists; `smart_approve` uses the tool's `read_only_hint` then an LLM classifier | tool name only; a separate store keys per-call decisions on tool name plus argument hash | `~/.config/goose/permission.yaml`, `permissions/tool_permissions.json` | per-call store with optional expiry |
| Amp | no approval by default | `amp.permissions` array, first match wins; actions `allow`, `reject`, `ask`, `delegate` (to a program on `PATH`, exit 0/1/2) | `*` globs over named arguments (`matches: {cmd: "*git commit*"}`); `tool` supports wildcards | `~/.config/amp/settings.json`, `.amp/settings.json`, managed | none documented; now labelled legacy in favour of a plugin `tool.call` hook |
| Cline | category toggles; reads, edits, browser, MCP on by default | `AutoApprovalSettings.actions` booleans; the model self-flags `requires_approval` on commands | category only; `.clineignore` for paths | extension state | n/a |
| Roo Code | category toggles | `allowedCommands`/`deniedCommands` prefix lists; longest prefix wins, deny wins ties | command prefix; `.rooignore` for paths; blocks `$(...)` and backticks outright | VS Code settings | n/a |

Patterns worth naming:

- **Deny beats approve everywhere a grammar exists** (Claude Code, Codex, OpenCode, Roo). ahi already does this.
- **Commands are matched on argv prefix, not on the raw string**, in the harnesses that take safety seriously (Claude Code, Codex, Gemini, Roo). Claude Code and Codex additionally split `&&`, `||`, `;`, `|` chains so every part must match, and Claude Code strips wrappers such as `timeout`, `nice`, `xargs`. Roo refuses command substitution entirely. Amp and OpenCode match the raw command string with globs, which is weaker (`*git commit*` matches `echo git commit; rm -rf /`).
- **Paths use gitignore-style globs** (Claude Code, OpenCode, Gemini's ignore files, Cline/Roo ignore files) or prefixes (ahi, Codex writable roots). Claude Code's `Write` is governed by `Edit(path)` rules, so one rule covers both write shapes.
- **Secrets are denied by path**, with `.env` the universal example (`Read(./.env)`, `{"action":"read","resource":"*.env","effect":"deny"}`). ahi's shipped `ahi.tools.yaml` does the same.
- **Sandbox and approval are separate knobs** in Codex, and Codex is the only harness where the sandbox is the primary control and approval is the escalation path (`require_escalated` with a `justification`, `request_permissions` to widen). ahi's `sandbox:` field on a rule and the `ahi sandbox` plan point the same way.
- **Prompt fatigue is a recognised failure.** pi and Amp dropped prompting altogether; Goose's `smart_approve` and Cline's `requires_approval` hand the decision to a model, which ahi's design explicitly rejects (model output cannot grant). Claude Code and Codex answer with persistent rules written on "don't ask again", so the policy file grows from use. That is the pattern compatible with ahi's "authorization carries forward": a confirmed `ask` becomes a stored grant, never a looser default.
- **Persistence location is a trust decision.** Claude Code writes grants to the repo-local `settings.local.json`; Codex to the user's `~/.codex/rules`; Gemini to a user store. Nobody writes grants into the committed project policy automatically. ahi's session grants are in memory only; a durable store is still future work.
- **Ignore files are the only path control in Cline/Roo**, and both apply them to shell reads by naively tokenising a fixed list of reader commands (`cat`, `head`, `grep`, ...). Cline is deprecating `.clineignore` for a `PreToolUse` hook. ahi should not adopt the pattern: a path rule should bind the tool that opens the path, not guess from a command line.

## 4. Confinement

| Harness | Path confinement for reads and writes |
| --- | --- |
| pi | none; `resolveToCwd` has no containment check |
| Claude Code | working directory plus `additionalDirectories`; outside paths prompt |
| Codex CLI | OS sandbox: read-only, or writable roots (`workspace_write.writable_roots`), network off by default |
| OpenCode | `external_directory` action for paths outside the project, allowed by default |
| Gemini CLI | workspace root; optional container, seatbelt, or gVisor sandbox |
| Goose | none |
| Amp | *unverified* |
| Cline / Roo | "outside workspace" toggles; `list_files` refuses root and home |
| ahi today | working directory only; lexical normalisation, canonical path must equal requested path (no symlinks, no case variants), ahi's own directories excluded even inside the workspace |

ahi's read confinement is already the strictest in the table. The design doc's standing rule holds: the first writing or command-running builtin needs OS-level containment in front of it, because the harnesses that skipped that step (pi, Goose, Amp) rely on the operator running inside a container or VM instead.

## 5. What this implies for `ahi.tools.yaml`

The survey supports the existing one-file-per-job, deny-by-default design and suggests the following order of work. These are proposals for discussion, not commitments.

### 5.1 Read-side additions (no new trust domain)

Three tools complete the read family and need no sandbox beyond today's in-process checks:

| Tool | Arguments | Notes |
| --- | --- | --- |
| `read_file` | `path`, `offset`, `limit` | Add paging. Every harness has it; ahi's 64 KiB cap with no continuation is the one read-side gap. |
| `find_files` | `pattern`, `path`, `limit` | Glob over names under a workspace-relative directory, respecting `.gitignore`. Implemented in the binary; no `fd`. |
| `search_files` | `pattern`, `path`, `glob`, `literal`, `limit` | Fixed-string or regex over contents, respecting `.gitignore`, with a result and byte cap. Implemented in the binary; no `rg`. |

Rule shape is unchanged: `match: {tool, path}` with the same prefix semantics. `find_files` and `search_files` take their `path` as the directory searched.

Both run in process and are bound by per-file `open` cost on very large trees; an `openat`-based walk in an in-repo unsafe crate is tracked in [issue #39](https://github.com/iancleary/ahi/issues/39).

### 5.2 Write-side (new trust domain, behind containment)

| Tool | Arguments | Notes |
| --- | --- | --- |
| `edit_file` | `path`, `old`, `new`, `replace_all` | Exact, unique match or fail, following the consensus contract. Atomic write, no symlink following, canonical path check, as `ahi run --fix` already does. |
| `write_file` | `path`, `content` | Create or overwrite. Governed by the same rules as `edit_file`, following Claude Code's precedent, so a repository writes one rule for "may change files under `src/`". |

Proposed grammar: `action: file_write` with `match: {path}` covering both tools, rather than a `tool_call` rule per tool, since the trust question is "which paths may change", not "which verb". A `tool_call` rule for `edit_file` without a `file_write` rule for its path is denied. The AGENT_RUNTIME sketch already names `file_write` as an action kind.

Prerequisite: a containment profile that can be named in `sandbox:`. Until one exists, `file_write` rules are a parse error ("not implemented yet"), never a silent in-process write.

### 5.3 Command execution (new trust domain, behind containment)

| Tool | Arguments | Notes |
| --- | --- | --- |
| `run_command` | `argv` (list), `cwd`, `timeout` | Takes argv, not a shell string. No shell is spawned, so there is nothing to split and no `$(...)` to refuse. Output capped by lines and bytes with overflow kept for paging. |

Proposed grammar: `action: command` with `match: {argv: [prefix...], cwd}`, as the AGENT_RUNTIME sketch already has. Argv-prefix matching is what Claude Code, Codex, Gemini, and Roo converge on; taking argv directly removes the chain-splitting and wrapper-stripping machinery those harnesses need because they accept a shell string. A model that wants a pipeline asks for `sh -c`, and a rule that approves `["sh", "-c"]` is visibly dangerous in review.

`ahi.hooks.yaml` already pins local hook programs by exact entry. `command` rules in `ahi.tools.yaml` are the model-facing counterpart and stay in the tools file because the trust domain (what the model may run) differs from the hooks file (what `ahi run` may run).

### 5.4 Not proposed

- **Network tools** (`web_fetch`, `web_search`). Every harness has them; none is needed for the hook-and-edit loop ahi targets first, and `ahi sandbox check` currently reports that network is not enforced. Defer until a profile can deny network by default.
- **Subagents.** A spawned worker inherits authority. The AGENT_RUNTIME design already says a worker cannot widen its own grant; the mechanism to prove that does not exist yet.
- **Model-classified approval** (Goose `smart_approve`, Cline `requires_approval`). Model output cannot grant.
- **Ignore files as the path control.** A rule binds a tool to a path; it does not infer paths from a command line.
- **Raw command-string globs** (Amp, OpenCode). Argv prefix only.

### 5.5 Grant persistence

Claude Code and Codex turn a confirmed prompt into a stored rule. ahi's `s` answer is in memory only. When a store exists, the grant should land in a file the repository does not commit (the state directory, keyed by working directory), never in `ahi.tools.yaml` itself, matching the behaviour of every surveyed harness and the design rule that the committed policy is reviewed, not accumulated.

## Sources

Gathered 2026-11-07. Page dates are given where a page showed one.

- pi: `raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/` README, `docs/{usage,security,settings,cli,configuration,extensions}.md`, `src/core/tools/*.ts`.
- Claude Code: `code.claude.com/docs/en/{settings,permissions,tools-reference,hooks}`.
- Codex CLI: `learn.chatgpt.com/docs/{config-file/config-reference,sandboxing,agent-approvals-security,agent-configuration/rules}`; `github.com/openai/codex` `codex-rs/core/src/tools/` on `main`.
- OpenCode: `opencode.ai/docs/{tools,permissions,agents,config}`.
- Gemini CLI: `geminicli.com/docs/reference/tools` (2026-09-01), `reference/policy-engine` (2026-04-30), `reference/configuration`, `tools/{web-fetch,web-search,memory}`, `cli/sandbox`; `github.com/google-gemini/gemini-cli` `docs/tools/{file-system,shell}.md`.
- Aider: `aider.chat/docs/usage/{commands,modes,lint-test,tips}.html`, `docs/config/options.html`, `docs/more/edit-formats.html`, `HISTORY.html`.
- Goose: `block.github.io/goose/docs/guides/managing-tools/{goose-permissions,tool-permissions}`, `guides/allowlist`, `mcp/developer-mcp`; `github.com/block/goose` `crates/goose/src/agents/platform_extensions/developer/`, `config/permission.rs`, `permission/`.
- Amp: `ampcode.com/news/tool-level-permissions` (2025-08-07), `ampcode.com/permissions`, `docs/cli/settings`, `docs/tools` (2026-11-05), `news/enterprise-managed-settings` (2025-08-25). `amp tools list` could not be run during the survey, so several Amp tool names are unverified.
- Cline: `docs.cline.bot/{features/auto-approve,customization/clineignore,resources/deprecations,tools-reference/all-cline-tools,sdk/guides/permission-handling}`; `github.com/cline/cline` `apps/vscode/src/shared/{tools,AutoApprovalSettings}.ts`, `core/ignore/ClineIgnoreController.ts`.
- Roo Code: `roocodeinc.github.io/Roo-Code/features/{auto-approving-actions,rooignore}`, `advanced-usage/available-tools/*` (2026-05-15); `github.com/RooCodeInc/Roo-Code` `src/core/prompts/tools/native-tools/`.
