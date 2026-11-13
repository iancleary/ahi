# Chat screen

Status: configured gateways stream real responses over the protocol their `api` declares ([Providers](PROVIDERS.md)), and curated OpenAI selections use the browser-login [Responses transport](OPENAI_INFERENCE.md). Chat, Models, and Catalog share the **Commands** popup and guided gateway onboarding. Builtin read-only tools are policy-gated by `ahi.tools.yaml` on every protocol; conversation persistence remains unimplemented; [Tool calls](TOOL_CALLS.md) separates implemented parsing from the proposed execution lifecycle. The current interaction design and terminal evidence are documented in [Terminal experience](TERMINAL_EXPERIENCE.md); [SPEC.md](SPEC.md) remains the compatibility/security contract. Planned transcript formatting (markdown, diffs, links, timing, header) is drafted in [Chat formatting plan](CHAT_FORMATTING_PLAN.md).

## Purpose

`ahi tui` opens chat after the operator explicitly uses a curated model in Models or a checked model in Catalog and `<state>/selection` is written. The screen has a conversation view, an input box with context in its upper rule, and a single-row workspace/model footer. The layout follows the familiar Claude Code and pi.dev arrangement.

## Layout

The transcript/input follow the pi.dev/Claude Code-style reference. The single-row footer groups workspace context on the left and the model on the right. Context usage sits near the right end of the input's upper rule.

```
  conversation transcript (scrolls, no frame)
  tool output and diffs render as shaded blocks

⠋ Working ────────────────────────────────────── 28.4% / 272k ──
█ input (no frame, no prompt character)
────────────────────────────────────────────────────────────────
  ~/Work/gh/ahi on   main !6     (openai) gpt-6.1-sol • medium
```

From top to bottom:

1. **Transcript.** Every row not used below, with no frame. Pinned to the bottom unless the operator has scrolled up.
2. **Input.** A full-width horizontal rule above and below, in the accent color (cyan), with no side borders and no prompt character. Embed known context usage, such as `28.4% / 272k`, near the right end of the upper rule. Use muted lavender-blue (256-color index 103) with dim styling, one space around the label, and two trailing rule cells. Leave at least two leading rule cells. On narrow terminals, compact to percentage only, then omit the label if even that cannot fit; never wrap. Unknown usage, unknown capacity, or zero capacity leaves a plain rule, while known zero usage displays `0%`. No warning thresholds are invented. While a response is active, animate a cyan spinner and `Working` at the upper rule's left edge, opposite context; remove it on completion. In very narrow terminals show only the spinner, and shorten/hide context rather than overlap it. There is no separate activity row.

   Drafts soft-wrap before submission, measuring terminal cells and preserving Unicode grapheme clusters. The box grows upward from one to at most eight visual rows, moving its upper rule upward while leaving the footer and lower rule anchored. Taller drafts scroll internally to keep the insertion cursor visible; short terminals further reduce the viewport. Resizing reflows the draft without changing its contents. Visual wrapping never inserts submitted newlines; Shift+Enter still inserts a real newline. An insertion cursor after a completely full row gets a new visual row.
3. **Workspace/model row.** The left side is a Powerlevel10k-style Nerd Font location: OS logo, folder icon, working directory (home shortened to `~`), then `on  ` and the git branch. Arch Linux uses ``; other platforms use their own logo or a generic fallback. A detached HEAD shows the short commit id instead of the branch. Outside a repository, omit the git segment. Use the operator's Powerlevel10k-style palette at normal brightness: terminal-default distro logo, blue path/folder (256-color index 31), brighter bold repository name (39), and green Git icons/branch (76). Follow the branch with nonzero Powerlevel10k worktree counts, in order: red `~N` conflicts (196), yellow `+N` staged and `!N` unstaged (178), blue `?N` untracked (39). A file can count as both staged and unstaged. Counts refer to files, not lines; a rename counts once. Clean worktrees omit markers. If status cannot be read, show `status unavailable` instead of implying clean. Status is probed on a worker thread at load, after each message is sent, and once per drained batch of tool returns; the row keeps its last snapshot until the probe lands, so a slow `git status` never delays a frame. On the right, show `(gateway) model • thinking-level`, right-aligned. Reserve its width first, with a gap from the workspace; shorten the workspace from the left with `…` when narrow, preserving visible colors.
There is no second footer row. Cumulative input/output tokens, cache counts, cost, subscription, and compaction indicators are not permanently displayed. Commands shows cumulative reported-usage cost when calculable; it is not the provider's billing total. Other backend accounting remains internal. Context used is the input token count of the latest response, not the cumulative session total; capacity uses `k`/`M` suffixes with at most one decimal.

The workspace uses the colors above at normal brightness. The provider/model/thinking segment remains muted lavender-blue (256-color index 103) with dim styling.

## Keys

| Key | Action |
|---|---|
| `Enter` | Submit the input. An empty or whitespace-only input does nothing, except after a connection failure, when it sends the same transcript again (the activity line says `Enter resends the last message`). |
| `Shift+Enter`, `Alt+Enter` | Insert a newline. Both are accepted because many terminals do not report `Shift+Enter`. |
| `Ctrl+J` | Insert a newline (fallback for terminals that report neither). |
| `Left`, `Right`, `Home`, `End`, `Backspace`, `Delete` | Edit within the input. `Up` and `Down` move between visual input rows, including soft-wrapped rows. |
| `PageUp`, `PageDown` | Scroll the transcript. |
| `Ctrl+T` | Cycle the thinking level. |
| `Ctrl+S` | Enter keyboard selection mode (also `/select`). While active, keys go to the transcript cursor, never to the draft or a pending approval: `h`/`j`/`k`/`l` or arrows move, `w`/`b` move to the next/previous word start with Vim's `utf_class` rules (blank, keyword per the default `iskeyword`, punctuation, emoji, and one class per CJK/Hiragana/Katakana/Hangul block; an empty row is a word; motions cross rows), `0`/`$` go to row start/end, `g`/`G` to the oldest/newest row, `PageUp`/`PageDown` by a page, `v` starts a character-wise visual range and `V` a line-wise one over whole logical lines (soft-wrapped rows included, trailing newline on yank); pressing the active kind again drops the range and the other kind switches to it, as in Vim, `y` (or `Ctrl+Y`) yanks the range through the shared clipboard adapters and leaves the mode with the highlight kept, `Esc` or `Ctrl+C` leaves without copying. The viewport scrolls to keep the cursor visible; a mouse click leaves the mode. The activity line shows `SELECT`, `VISUAL` or `VISUAL LINE` with the key hints. |
| `Ctrl+Y` | Copy the highlighted transcript selection again. |
| `Ctrl+C` | Clear the input if it is not empty. Otherwise exit. |
| `Ctrl+D` | Exit when the input is empty. |
| `Esc` | Dismiss Commands while retaining the chat draft, or clear transcript selection. With neither open, cancel the streaming response: the partial reply stays in the transcript and is sent with the next message, an assistant turn that received no text is removed, and a highlighted notice records the cancellation. During a tool approval with a blank input, deny the remaining calls. |
| `y`, `s`, `n`, `a` | Only during a tool approval with a blank input: approve once, approve for the session under the shown directory, deny, or approve the remaining calls. Any other key edits the draft as usual, so `/quit` still works. |

## Commands

Typing `/` immediately opens the shared **Commands** popup, also present in Models and Catalog. **Add gateway** is first. Keep typing to filter by spelling, alias, or action label; Up/Down selects, Enter applies, and Esc dismisses. The chat draft is retained on dismissal. An unknown command is reported in the transcript as `unknown command /name` and is not sent to inference. Common entries come from [`src/tui/commands.rs`](../src/tui/commands.rs), not a chat-only Settings list. A short `/ commands` hint remains in the input rule.

Non-quit command submission during an active response preserves the draft; submit again after completion. `/quit` exits immediately even during a response, without claiming upstream cancellation.

| Command | Action |
|---|---|
| `/model` | Reload local settings and reopen the curated model picker, retaining the last valid registry if an edit is invalid. A new selection is written to `<state>/selection` and takes effect for the next message without clearing the transcript. Choosing a model on a different provider while the transcript has messages posts a notice that those messages will go to the new provider with the next message. Cancelling the picker keeps the current selection. |
| `/login` | Open the shared [OpenAI login view](OPENAI_LOGIN.md), also available in the model and catalog screens. Return to chat after success, failure, or cancellation without changing the active model or clearing the transcript. Like other commands except quit, wait for any active response to finish before applying. |
| `/gateway` | Enter an inference base URL, preview its discovered catalog, and confirm registration. Opens the discovered gateway's catalog with an immediate registration/cache result. Space curates; Enter on a checked model explicitly uses it. Registration alone leaves the active model and transcript unchanged. Also available in both model screens; see [Gateway discovery](GATEWAY_DISCOVERY.md). |
| `/thinking <level>` | Set the thinking level. With no argument, show the current level. |
| `/copy rendered` / `/copy raw` | What a selection copies when the row renders markdown: the text as drawn (default) or the model's source. `Y` in keyboard selection always yanks raw. With no argument, show the current form. Persisted in `<state>/preferences`. |
| `/clock local` / `/clock utc` | How the turn footer stamps completion time: the local zone read from `$TZ` or `/etc/localtime` (TZif, in-repo reader), or UTC with a `Z` suffix, which is also the fallback when no zone is readable. Persisted. |
| `/editor <name or argv>` | The editor a clicked file reference opens in: `code`, `vscode`, `cursor`, `zed`, `subl`, `idea`, `hx`, or an argv template with `{path}`, `{line}`, `{column}`. Unset, ahi tries `$AHI_EDITOR`, `$VISUAL`, `$EDITOR`, then `code` on `PATH`. Terminal editors (`vim`, `nvim`, `nano`, `hx`) are refused with a notice because they cannot open over the chat screen. Persisted. |
| `/plain` / `/markdown` | Stop or resume rendering assistant markdown. Plain mode shows text as written. Persisted. |
| `/clear` | Clear the transcript and reset the session token totals. |
| `/quit` (alias `/exit`) | Exit the application, including from nested Models or Catalog views. `q` in a nested picker instead cancels back to chat. |

## Transcript formatting

Rows carry styled spans and click targets beside their plain text, so selection and copy read the text alone and styling never leaks into the clipboard (`src/chat/transcript/render.rs`). `src/chat/transcript/format.rs` turns each message into paragraphs:

- **Assistant text** renders a markdown subset (`markdown.rs`): ATX headings, `**bold**`, `*italic*`, `~~strike~~`, inline code, fenced code with a language header and a `copy` affordance, bulleted, numbered, and task lists with nesting, `>` quotes, `---` rules, `[text](url)` and bare links, and pipe tables aligned when they fit the width. Every newline the model writes is a line break; CommonMark's soft-break joining is not applied because models use single newlines deliberately. Unknown constructs are shown as typed, never dropped. Fenced code and `read_file` bodies are highlighted by in-repo per-line lexers (`highlight.rs`) for rust, toml, yaml, json, sh, python, typescript/javascript, markdown, diff, typst, and powershell; other languages are monochrome on the block background.
- **Tool results** open with one header grammar: `tool target · summary · duration`, where the target is clickable when it exists in the workspace. `edit_file` and `write_file` show a unified diff (`diff.rs`, Myers over lines, word-level emphasis) with red and green row backgrounds and an old/new gutter. `run_command` shows the exact approved argv, then `stdout` and `stderr` sub-blocks, stderr on the red background when the exit code is non-zero. `search_files` rows are `path:line` targets. Patterns, globs, and argv appear in the post-run header; the approval prompt keeps its counts-only rule.
- **Wrapped rows hang.** A row that wraps continues under its content, not under its gutter or marker: `read_file` lines carry their line numbers (from the requested offset; the tool's notes stay unnumbered), diff lines the old/new gutter and marker, list items their marker, and quotes their bar. The padding is on screen only; selection and copy skip it, so a wrapped line copies as one line.
- **Links and file references** (`links.rs`) are underlined. A left click without a drag, or `Enter`/`o` in keyboard selection, opens a URL with the platform opener (`open`, `xdg-open`, `wslview`, `rundll32`) for `http` and `https` only, or a file in the configured editor. File candidates must exist under the working directory; a relative path with `..` is never a target. Opening is operator-initiated and not a tool call, so no `ahi.tools.yaml` rule is involved.
- **Timing.** The activity line counts up while a turn runs (`⠋ Working · 12s · Esc cancels`). Each tool header carries that call's wall time. A completed turn ends with a dim, right-aligned footer `done in 1m 12s · 14:02`; a cancel or failure posts its alert instead and no footer.
- **Startup header.** A fresh transcript opens with a text wordmark, `ahi <version>`, the model and gateway, the thinking level, the workspace directory, whether `ahi.tools.yaml` is present (and, when it is not, that `ahi tools init` writes one), and the first keys. It is a notice: never sent to a model, removed by `/clear`.

Copy forms: rows that render markdown keep the source text of their block, so a selection copies either the rendered text or the raw markdown of every block it touches (`/copy`, `Y`). Code block bodies are the same in both forms. Diff rows copy as `git diff` prints them: the `+`/`-` marker and content, without gutter numbers.

## Thinking level

Levels: `off`, `minimal`, `low`, `medium`, `high`. Default is `medium`. The level is stored in `<state>/thinking` as a single line, beside `selection`, and is read at start. An unreadable or unknown value falls back to `medium` and is reported once in the transcript.

The stored preference becomes a request parameter only when effective metadata explicitly enables both reasoning and reasoning-effort support. Gateways use `reasoning_effort`; OpenAI Responses uses nested `reasoning.effort`. Off omits the parameter, and unsupported/unknown capabilities send no effort. This does not disable reasoning a provider might perform by default. Per-model effort maps are not implemented, so a provider may reject an unsupported level. The screen does not infer capabilities or synthesize usage.

## Backend interface

`src/chat/` talks to a model through one trait. A `CompletionConnection` carries the wire protocol, the destination and authentication (`CompletionTransport`), and the model metadata separately; each protocol adapter normalizes streaming data into the same screen events. See [Gateway model configuration](GATEWAY_MODEL_CONFIGURATION.md) for metadata resolution and request limits.

```rust
pub struct ChatRequest {
    pub selection: Selection,        // gateway and model, already split
    pub thinking: ThinkingLevel,
    pub messages: Vec<ChatMessage>,  // the full transcript so far
}

pub enum ResponseEvent {
    TextDelta(String),
    Usage { input_tokens: u64, output_tokens: u64, cached_input_tokens: Option<u64>, cost_usd: Option<f64> },
    ContextWindow(u64),
    ToolCallsRequested(Vec<ToolCall>),        // validated; never authority
    ToolApprovalRequested(ApprovalRequest),   // answered once via its reply
    ToolResult { call_id: String, tool: String, output: String, content: String, exit_code: i32 },
    ToolCallReturned,                         // after every result, including failures
    Completed,
    Failed { message: String, kind: ErrorKind },   // what the screen acts on
    Reconnecting { attempt: u8, attempts: u8 },    // between connection attempts
}

pub trait CompletionBackend {
    fn submit(&mut self, request: ChatRequest) -> std::sync::mpsc::Receiver<ResponseEvent>;
    fn cancel(&mut self) {}                   // stop the active turn
}
```

The screen drains the receiver on each frame tick (about 50 ms), so the user interface never blocks on the backend. No async runtime is added.

**Cancellation:** `Esc` calls `cancel` and drops the receiver, so the screen is free at once. The worker checks the flag before each request, before each tool round, and around every read of the response body; bytes that arrive after the cancel are discarded, the stream ends with an error the screen never sees, and dropping the reader closes the connection, which is the only cancellation signal these APIs accept. A read blocked on a silent server returns when its next bytes arrive or the 120-second request deadline passes; closing earlier would not save tokens, since a server that is not writing cannot notice a closed socket until it writes. Rows the screen adds for itself (`Notice`, `Alert`) are never sent to a model.

**Failures:** `Failed` carries the `ErrorKind`. A `Transport` failure that leaves the transcript ending in an operator message or tool results sets a resend affordance: `Enter` on a blank input sends the same transcript again. `Reconnecting` shows in the activity line while the backend waits between connection attempts to an unreachable gateway. Kinds, hints, and the retry rule are in [Providers](PROVIDERS.md#errors).

**Tool-return hook:** the backend emits `ResponseEvent::ToolCallReturned` after every tool call returns (success, error, or denial), right after its `ToolResult` and before resuming the assistant response. Its handler reads the current directory/Git HEAD and worktree counts, compares them with the screen's stored snapshot, and updates changed values for the next redraw. It does not end the response or clear the transcript. Location/status are also refreshed at chat load and on normal message submission, not polled on frame ticks. HEAD is read directly; file counts use local `git --no-optional-locks -c core.fsmonitor=false status --porcelain=v1 -z --untracked-files=all`. This reads the worktree without optional index writes or network requests. Parsing uses NUL-delimited bytes so spaces, newlines, non-UTF-8 names, renames, and conflicts are counted correctly. Ahead/behind, stash counts, and in-progress Git actions are not displayed yet. The builtin tools are read-only, so a tool-driven branch change is not yet exercised; that integration test arrives with the first writing tool.

## Tool calls

Tools are deny-by-default and builtin-only, on every protocol including browser-login OpenAI Responses. ahi is not a package manager: it never downloads a tool, names a program from configuration, or loads a remote tool definition. `ahi.tools.yaml` in the working directory only selects and bounds the builtin tools compiled into ahi, the same posture as `repo: builtin` checkers. The file is found only in the working directory where `ahi tui` started, not in a parent repository root; starting in a subdirectory disables tools. `ahi tui --tools <file>` names the policy file explicitly instead: it is read once at startup, so a missing or invalid file exits before the picker opens, and again on every turn. Rule paths and tool confinement stay relative to the working directory, not to the file. `examples/tools` holds ready-made policies; `ahi tools init --write` copies one into the working directory, and `ahi doctor` reports what the file there approves. The chat screen never writes the policy file.

| Tool | Arguments | Output |
|---|---|---|
| `read_file` | `path` (required), `offset` (1-based first line), `limit` (lines) | The file's UTF-8 text verbatim, or the requested window with a `[lines A-B of N]` header. Output is capped at 64 KiB; when more remains, a final `[N more lines; continue with offset=M]` line names where to resume, and no partial line is emitted |
| `list_dir` | `path` (default `.`) | Sorted names; `/` marks directories and `@` symbolic links; at most 500 entries |
| `find_files` | `pattern` (required), `path` (default `.`), `limit` (default 200, max 1000) | Sorted workspace-relative paths under `path` whose name (pattern without `/`) or path below `path` (pattern with `/`) matches the glob (`*`, `**`, `?`, `[abc]`, `[!abc]`). `.gitignore` files under `path` are honored, `.git` and ahi's directories are pruned, symbolic links are listed with `@` and never entered |
| `edit_file` | `path`, `old`, `new` (required), `replace_all` | Replaces `old` with `new` in a UTF-8 file. `old` must occur exactly once unless `replace_all`; exact bytes, no whitespace normalization; absent or ambiguous text fails with the count. Atomic write through a sibling temporary file, never through a symbolic link. Returns `edited <name>: N replacements, +A -R lines` |
| `write_file` | `path`, `content` (required), `overwrite` | Creates a file; fails if it exists unless `overwrite`, and with `overwrite` the file must exist. Parent directories must exist. Returns `created <name>: N lines` or `replaced <name>: N lines (was M)` |
| `run_command` | `argv` (required, one element per argument), `path` (working directory, default `.`), `timeout` (seconds, default 120, max 600) | Runs the program named by `argv[0]` with no shell, in the given directory, with the hook runner's scrubbed environment plus the approving rule's `toolchain_env` names and `AHI_TOOL=run_command`. Returns `exit N`, then `--- stdout` and `--- stderr` sections when non-empty, each capped at 32 KiB. A non-zero exit is a successful call whose content says so; a timeout or a cancelled turn kills the process group (the program and everything it spawned, on Unix) and is a tool error |
| `search_files` | `pattern` (required), `path`, `glob`, `literal`, `ignore_case`, `limit` (default 100, max 500) | `path:line:text` lines, files sorted and lines ascending, for a Rust-syntax regular expression or an exact string. Same walk as `find_files`; files with a NUL in their first 8 KiB or over 2 MiB are skipped and counted; matched lines are cut at 240 characters; output is capped at 64 KiB and total bytes read at 256 MiB |

Every tool is deterministic: sorted output, fixed caps, and no shell or external program (`rg`, `fd`, `git`) is involved, so results do not depend on what is installed or on the operator's global git configuration. The approval prompt shows the tool and the path only; patterns are never displayed. Paths are lexically normalized and must be relative, without `..`, backslashes, control characters, or invisible formatting characters. On disk, the canonical path must equal the requested one, so no symbolic link is followed and a differently cased spelling is refused, and nothing under any of ahi's own directories (config, data, state, cache) is readable even when it lies inside the working directory. A turn may run at most 8 tool rounds and return 256 KiB of tool content. Every round's request is checked against `inputLimits.maxRequestBytes`.

**Writes.** `edit_file` and `write_file` run in process with the same containment as reads: lexical normalization, canonical path equal to the requested one, no symbolic link anywhere on the path, nothing under ahi's directories. They are governed by `action: file_write` rules, never `tool_call`, so a repository states which paths may change separately from which may be read, and whether files may be created (`write_file`) separately from whether existing ones may be edited (`edit_file`). A few paths are protected from the model whatever the rules say: `ahi.tools.yaml`, `ahi.hooks.yaml`, `ahi.dev.yaml`, `.pre-commit-config.yaml`, `.gitignore`, and anything under `.git`. Every attempted write appends one line to `audit.log` in the state directory with the path, the outcome, SHA-256 hashes before and after, and line counts; never contents. This is the alpha posture: the working directory is a git checkout, so every write is undoable, and an OS sandbox is planned, not present.

**Commands.** `run_command` is governed by `action: command` rules matched on `match.argv`, a list that the call's argv must start with, token by token and exactly (`[cargo, test]` approves `cargo test --all-features` and not `cargotest` or `cargo publish`), plus an optional `match.path` prefix for the working directory. There is no shell, so there is nothing to split and no `$(...)` to refuse; a model that wants a pipeline asks for `[sh, -c, ...]`, and a rule approving `[sh, -c]` is visibly wide in review. The approval prompt shows the rendered command line, since it is the one argument the operator must see. A session grant from `s` is the asking rule's argv prefix under the call's directory, never the full command. Commands run on the host in this alpha; a rule's `sandbox: default` names the in-process posture, and an outer sandbox such as `ahi dev run -- ahi tui` is the way to contain them until `ahi sandbox run` exists. Every attempted command appends an `audit.log` line with argv, working directory, exit code, duration, and environment names; output is not logged.

**Policy.** Rules use `version: 1`, optional `default: deny`, and `rules` entries with `id`, `action: tool_call`, `file_write`, or `command`, `match.tool` (required for `tool_call`; for `file_write` one of `edit_file`, `write_file`, or omitted for both; omitted for `command`), optional `match.path` (a component-wise prefix of the file, directory, or working directory), `match.argv` (required for `command`, forbidden elsewhere), `effect: approve|deny`, optional `ask: true`, optional `toolchain_env` (`command` only, the same names `ahi.hooks.yaml` accepts), and optional `sandbox: default`. Unknown keys, other action kinds, unknown tools, a write or command tool under `tool_call`, and other sandbox profiles are errors. Like `ahi.hooks.yaml`, an invalid file fails closed: every chat turn in that directory, plain messages included, fails with the file's error until it is fixed. For each call: a matching deny rule denies (case-insensitively, so `SRC/x` cannot bypass a deny on `src/x`); otherwise a matching approve rule without `ask` runs it; otherwise a session grant runs it; otherwise a matching `ask` rule prompts; otherwise it is denied. Only tools an approve rule names are advertised. Without such a rule, the request has no `tools` field.

**Approval.** Calls that need the operator arrive as one `ToolApprovalRequested` batch. The transcript shows `Tool approval · N calls`, each normalized `tool target`, for a write the line counts in parentheses (`+3 -1 lines`, `new file, 12 lines`, `replace with 40 lines`) computed from the arguments without touching the file, for a command the rendered command line (`cargo test --all-features`, quoted where a token has spaces, cut at 200 characters), and the decision, followed by the answers offered for the call awaiting one: `↑`/`↓` move the highlight and `Enter` applies it, and each answer's letter (`y`, `s`, `n`, `a`, `Esc`) still applies it directly. `Shift+↑`/`Shift+↓` move the `›` cursor to another numbered call, wrapping; after an answer the cursor continues at the next undecided call, and answering a decided call replaces its decision. "All remaining" answers apply to every undecided call and appear only while more than one is undecided. The upper input rule shows the key hint. Decisions are sent once, after every call is decided. Exiting or a failed turn drops the reply, which denies the batch and ends the turn. `s` grants the tool under the call's directory (the file's parent, or the listed directory) for the rest of the process, for this working directory and gateway only. A whole-workspace grant is not offered.

**Authority.** Model output, tool arguments, tool results, and transcript text cannot write a rule or a grant. Grants come only from `ahi.tools.yaml` and from decisions on the reply channel the screen owns. Notices and errors never repeat raw arguments; the prompt and results show only the normalized target.

**Results.** Each call yields exactly one `ToolResult` (exit 0 success, 1 tool error, 126 denied), then `ToolCallReturned`. The transcript shows a shaded preview; the model receives the full bounded content as a `role: "tool"` message after the assistant message carrying `tool_calls`. Both are stored as structured transcript data and replayed on later turns. Calls without results, or results without their call, are dropped from requests. `requiresToolResultName` and `requiresAssistantAfterToolResult` are honored.

### Historical baseline: stand-in backend

`EchoBackend` replies with the submitted text prefixed by `[stand-in]`, delivered as several `TextDelta` events about 20 ms apart, so streaming display is exercised. It reports `Usage` with input and output computed as the character count divided by four, `cached_input_tokens` and `cost_usd` as `None`, and a `ContextWindow` of 128000. Those figures are synthetic test data, and the transcript says so: the first reply in a session carries a note `stand-in backend: replies and token counts are not from a model`.

That was the original screen-development baseline, not current production behavior. `HttpBackend` has replaced it for gateways and OpenAI. Synthetic replies and usage are not used in `ahi tui`; authentication or protocol failures are explicit errors.

## Flow

1. `ahi tui` in a terminal: load the registry and open the picker, unchanged.
2. On `Enter` on a curated Models row or checked Catalog row: write `<state>/selection`, then open/resume the chat screen with that selection. Space in Catalog changes availability only, never active selection.
3. Cancelling the initial picker exits without changing `<state>/selection`, even if a previous selection exists. This deliberately avoids silently resuming a stale model. Cancelling a picker reopened by `/model` instead returns to the active chat and keeps its current selection.
4. `ahi tui` on a pipe: unchanged. It prints the load and never opens the chat screen.

## Implementation constraints

- Reuse the existing dependency graph. The input box is a small editor in `src/chat/input.rs` (logical lines, cursor, and a separate visual layout) on top of pinned `ratatui-core` 0.1.0, `ratatui-crossterm` 0.1.0, the in-repo `ahi-widgets` fork of six ratatui widgets, and `crossterm` 0.29.0. Cell-aware wrapping directly uses `unicode-segmentation` 1.12.0 and `unicode-width` 0.2.0, both already resolved in Cargo.lock; no new package versions are introduced. Do not add `tui-textarea` or an async runtime. The editor accepts an `EditorKey`. `src/chat/key_binding.rs` translates a terminal key into a `KeyAction`, which carries that `EditorKey` when the key edits the input.
- Read the Git branch from `HEAD`, following `gitdir:` indirection for worktrees. Read worktree counts with read-only porcelain status. Refresh at chat load, normal message submission, and each `ToolCallReturned` event, never on frame ticks.
- Keep the screen state in a plain struct (`ChatScreen`) separate from drawing, so key handling and layout are testable without a terminal.
- Names follow the project naming rule: precise, descriptive, no slang (`CompletionBackend`, `EchoBackend`, `ThinkingLevel`, `render_session_row`).
- `unsafe_code` stays forbidden in `ahi`; the in-repo `ahi-win32` crate holds the Win32 clipboard FFI and `ahi-unix` the POSIX signal and process-group calls. Clippy must pass with the existing lint set.

## Module layout

`src/chat/` is the conversation screen.

| File | Responsibility |
|---|---|
| `mod.rs` | `ChatScreen` state, the frame loop, applying a `KeyAction`, and drawing |
| `key_binding.rs` | Translates a crossterm key into a `KeyAction` |
| `input.rs` | Logical-line editor, cell-aware soft wrapping, visual cursor navigation, and bounded viewport. It accepts an `EditorKey`, not a terminal event |
| `transcript.rs` | Messages, wrapping, and the scroll window. It returns plain text |
| `status.rs` | Location-row and session-row text |
| `usage.rs` | Session token, cost, and context figures |
| `location.rs` | Working directory, Git label from `HEAD`, and read-only worktree change counts |
| `command.rs` | Parsing of `/model`, `/login`, `/gateway`, `/thinking`, `/clear`, and `/exit` |
| `approval.rs` | Pending tool approval: per-call decisions, prompt text, and key hint |
| `thinking.rs` | `ThinkingLevel` and `<state>/thinking` |
| `backend/mod.rs` | `CompletionBackend`, `ChatRequest`, and `ResponseEvent` |
| `backend/http.rs` | The protocol-neutral turn loop: send, decode, run approved builtins, continue |
| `backend/protocol/` | One adapter per wire protocol (Chat Completions, Responses, Anthropic Messages): request body, stream decoder, tool definitions; `replay.rs` pairs calls with results |
| `backend/sse.rs` | Bounded SSE framing with reusable buffers, shared by the adapters |
| `backend/http/responses.rs` | OpenAI Responses serialization, bounded stream normalization, and validated function requests |
| `backend/tool_loop.rs` | One tool round: policy, approval, execution, results, and session grants |
| `backend/tools/` | Builtin `read_file`, `list_dir`, `find_files`, `search_files`, `edit_file`, `write_file`, and `run_command`; the glob and `.gitignore` matchers; the bounded walk; working-directory containment; protected paths |
| `../tool_policy.rs` | `ahi.tools.yaml` parsing and rule evaluation |
| `../commands.rs` | Shared context-aware command registry, popup state/rendering, and explicit picker outcomes |

`mod.rs` may import `ratatui_core`, `ahi_widgets`, and `crossterm`. `key_binding.rs` may import `crossterm` and must not import `ratatui_core` or `ahi_widgets`. `status.rs` may import either; it returns styled location spans and session row text. `mod.rs` applies dim usage styling and the muted model color when it draws. `transcript.rs` does not either: wrapping produces strings, and `mod.rs` places that string in a paragraph. `input.rs`, `usage.rs`, `command.rs`, `thinking.rs`, `location.rs`, and `backend/` have no terminal-rendering dependencies. `location.rs` owns the filesystem/subprocess boundary for the workspace snapshot.

## Verification

- Key handling: `Enter` submits, `Shift+Enter`, `Alt+Enter` and `Ctrl+J` insert a newline, and an empty input does not submit.
- Typing a draft wider than the terminal wraps it before submission and moves the upper rule upward. Cursor coordinates follow terminal-cell widths, including wide/combining Unicode text. Visual Up/Down, resize reflow, the eight-row limit, short-terminal scrolling, and unchanged submitted text are verified.
- The upper rule shows an advancing spinner at its left only while a response is active, with context still aligned at the right; completion restores the plain rule prefix.
- Commands: `/model`, `/thinking high`, `/thinking`, `/clear`, `/exit`, and an unknown command, each without a terminal.
- The thinking level is persisted to `<state>/thinking` and read back. An invalid file falls back to `medium`.
- Layout using `ratatui_core::backend::TestBackend`: colorful workspace and muted model share one footer row. Context is muted within the input's upper rule, with spacing and trailing rule cells. Narrow labels compact to percentage then disappear without wrapping. Unknown context is omitted, while known zero remains visible. Cumulative accounting is not rendered and the freed row goes to the transcript.
- The location row renders the Powerlevel10k-style directory and branch for a fixture repository, the short commit id for a detached HEAD, and no git segment outside a repository.
- HTTP fixture tests over loopback prove streamed text, usage, bounded failures, and tool-call validation; with no approving rule, no `tools` and no execution; an approve rule's call running and resubmitting `tool_calls` plus `role: "tool"` messages; approve, deny, and session-grant decisions; a dropped reply denying the batch; invalid, unknown, escaping, case-variant, and state-directory paths; the round cap; `maxRequestBytes` on resubmission; and cross-turn replay. The PTY workflow approves one call with `y` and denies another with `n`. The PTY workflow additionally takes an empty installation through Commands → gateway registration → explicit curation → checked-model Enter → an actual local streamed reply.
- Piped `ahi tui` output is byte-identical to v0.2.0.

## Remaining work and limits

- OpenAI Responses is implemented and verified with local HTTP fixtures; live browser-token inference acceptance was not checked in this implementation.
- Token counts are consumed when reported. Capacity and cost remain unknown without authoritative metadata. Without a tokenizer, context capacity is displayed, not an exact preflight token-budget guarantee.
- Cancellation closes the connection at the next read; a provider that keeps generating after a client disconnect is outside ahi's control. Per-model reasoning-effort maps and image input remain future work. Gateway builtin exchanges replay in memory.
- The transcript, including gateway tool exchanges, is not persisted and is lost on exit. Session grants last for the process only.
- Gateway tools run in process with software path checks, not an OS sandbox. Other sandbox profiles are refused. OpenAI tool execution remains disabled.

Run the maintained PTY workflow and optionally capture 80×24/60×24 screens using the commands in [Terminal experience](TERMINAL_EXPERIENCE.md). Local POSIX fixtures do not prove remote gateway reachability, live OpenAI acceptance, or native Windows terminal behavior.
