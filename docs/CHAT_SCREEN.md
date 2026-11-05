# Chat screen

Status: configured gateways stream real Chat Completions responses, and curated OpenAI selections use the browser-login [Responses transport](OPENAI_INFERENCE.md). Chat, Models, and Catalog share the **Commands** popup and guided gateway onboarding. Gateway builtin read-only tools are policy-gated by `ahi.actions.yaml`; OpenAI tool execution and conversation persistence remain unimplemented; [Tool calls](TOOL_CALLS.md) separates implemented parsing from the proposed execution lifecycle. The current interaction design and terminal evidence are documented in [Terminal experience](TERMINAL_EXPERIENCE.md); [SPEC.md](SPEC.md) remains the compatibility/security contract.

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
3. **Workspace/model row.** The left side is a Powerlevel10k-style Nerd Font location: OS logo, folder icon, working directory (home shortened to `~`), then `on  ` and the git branch. Arch Linux uses ``; other platforms use their own logo or a generic fallback. A detached HEAD shows the short commit id instead of the branch. Outside a repository, omit the git segment. Use the operator's Powerlevel10k-style palette at normal brightness: terminal-default distro logo, blue path/folder (256-color index 31), brighter bold repository name (39), and green Git icons/branch (76). Follow the branch with nonzero Powerlevel10k worktree counts, in order: red `~N` conflicts (196), yellow `+N` staged and `!N` unstaged (178), blue `?N` untracked (39). A file can count as both staged and unstaged. Counts refer to files, not lines; a rename counts once. Clean worktrees omit markers. If status cannot be read, show `status unavailable` instead of implying clean. On the right, show `(gateway) model • thinking-level`, right-aligned. Reserve its width first, with a gap from the workspace; shorten the workspace from the left with `…` when narrow, preserving visible colors.
There is no second footer row. Cumulative input/output tokens, cache counts, cost, subscription, and compaction indicators are not permanently displayed. Commands shows cumulative reported-usage cost when calculable; it is not the provider's billing total. Other backend accounting remains internal. Context used is the input token count of the latest response, not the cumulative session total; capacity uses `k`/`M` suffixes with at most one decimal.

The workspace uses the colors above at normal brightness. The provider/model/thinking segment remains muted lavender-blue (256-color index 103) with dim styling.

## Keys

| Key | Action |
|---|---|
| `Enter` | Submit the input. An empty or whitespace-only input does nothing. |
| `Shift+Enter`, `Alt+Enter` | Insert a newline. Both are accepted because many terminals do not report `Shift+Enter`. |
| `Ctrl+J` | Insert a newline (fallback for terminals that report neither). |
| `Left`, `Right`, `Home`, `End`, `Backspace`, `Delete` | Edit within the input. `Up` and `Down` move between visual input rows, including soft-wrapped rows. |
| `PageUp`, `PageDown` | Scroll the transcript. |
| `Ctrl+T` | Cycle the thinking level. |
| `Ctrl+C` | Clear the input if it is not empty. Otherwise exit. |
| `Ctrl+D` | Exit when the input is empty. |
| `Esc` | Dismiss Commands while retaining the chat draft, or clear transcript selection. It does not currently cancel an upstream model request. During a tool approval with a blank input, deny the remaining calls. |
| `y`, `s`, `n`, `a` | Only during a tool approval with a blank input: approve once, approve for the session under the shown directory, deny, or approve the remaining calls. Any other key edits the draft as usual, so `/quit` still works. |

## Commands

Typing `/` immediately opens the shared **Commands** popup, also present in Models and Catalog. **Add gateway** is first. Keep typing to filter by spelling, alias, or action label; Up/Down selects, Enter applies, and Esc dismisses. The chat draft is retained on dismissal. An unknown command is reported in the transcript as `unknown command /name` and is not sent to inference. Common entries come from [`src/commands.rs`](../src/commands.rs), not a chat-only Settings list. A short `/ commands` hint remains in the input rule.

Non-quit command submission during an active response preserves the draft; submit again after completion. `/quit` exits immediately even during a response, without claiming upstream cancellation.

| Command | Action |
|---|---|
| `/model` | Reload local settings and reopen the curated model picker, retaining the last valid registry if an edit is invalid. A new selection is written to `<state>/selection` and takes effect for the next message without clearing the transcript. Cancelling the picker keeps the current selection. |
| `/login` | Open the shared [OpenAI login view](OPENAI_LOGIN.md), also available in the model and catalog screens. Return to chat after success, failure, or cancellation without changing the active model or clearing the transcript. Like other commands except quit, wait for any active response to finish before applying. |
| `/gateway` | Enter an inference base URL, preview its discovered catalog, and confirm registration. Opens the discovered gateway's catalog with an immediate registration/cache result. Space curates; Enter on a checked model explicitly uses it. Registration alone leaves the active model and transcript unchanged. Also available in both model screens; see [Gateway discovery](GATEWAY_DISCOVERY.md). |
| `/thinking <level>` | Set the thinking level. With no argument, show the current level. |
| `/clear` | Clear the transcript and reset the session token totals. |
| `/quit` (alias `/exit`) | Exit the application, including from nested Models or Catalog views. `q` in a nested picker instead cancels back to chat. |

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
    Failed(String),
}

pub trait CompletionBackend {
    fn submit(&mut self, request: ChatRequest) -> std::sync::mpsc::Receiver<ResponseEvent>;
}
```

The screen drains the receiver on each frame tick (about 50 ms), so the user interface never blocks on the backend. No async runtime is added.

**Tool-return hook:** the backend emits `ResponseEvent::ToolCallReturned` after every tool call returns (success, error, or denial), right after its `ToolResult` and before resuming the assistant response. Its handler reads the current directory/Git HEAD and worktree counts, compares them with the screen's stored snapshot, and updates changed values for the next redraw. It does not end the response or clear the transcript. Location/status are also refreshed at chat load and on normal message submission, not polled on frame ticks. HEAD is read directly; file counts use local `git --no-optional-locks -c core.fsmonitor=false status --porcelain=v1 -z --untracked-files=all`. This reads the worktree without optional index writes or network requests. Parsing uses NUL-delimited bytes so spaces, newlines, non-UTF-8 names, renames, and conflicts are counted correctly. Ahead/behind, stash counts, and in-progress Git actions are not displayed yet. The builtin tools are read-only, so a tool-driven branch change is not yet exercised; that integration test arrives with the first writing tool.

## Tool calls

Tools are deny-by-default and builtin-only, on every protocol including browser-login OpenAI Responses. ahi is not a package manager: it never downloads a tool, names a program from configuration, or loads a remote tool definition. `ahi.actions.yaml` in the working directory only selects and bounds the builtin tools compiled into ahi, the same posture as `repo: builtin` checkers. The file is found only in the working directory where `ahi tui` started, not in a parent repository root; starting in a subdirectory disables tools.

| Tool | Arguments | Output |
|---|---|---|
| `read_file` | `path` (required) | UTF-8 text, capped at 64 KiB with a truncation marker |
| `list_dir` | `path` (default `.`) | Sorted names; `/` marks directories and `@` symbolic links; at most 500 entries |

Paths are lexically normalized and must be relative, without `..`, backslashes, control characters, or invisible formatting characters. On disk, the canonical path must equal the requested one, so no symbolic link is followed and a differently cased spelling is refused, and nothing under ahi's state directory is readable even when it lies inside the working directory. A turn may run at most 8 tool rounds and return 256 KiB of tool content. Every round's request is checked against `inputLimits.maxRequestBytes`.

**Policy.** Rules use `version: 1`, optional `default: deny`, and `rules` entries with `id`, `action: tool_call`, `match.tool`, optional `match.path` (a component-wise prefix), `effect: approve|deny`, optional `ask: true`, and optional `sandbox: default`. Unknown keys, other action kinds, unknown tools, and other sandbox profiles are errors. Like `ahi.policy.yaml`, an invalid file fails closed: every chat turn in that directory, plain messages included, fails with the file's error until it is fixed. For each call: a matching deny rule denies (case-insensitively, so `SRC/x` cannot bypass a deny on `src/x`); otherwise a matching approve rule without `ask` runs it; otherwise a session grant runs it; otherwise a matching `ask` rule prompts; otherwise it is denied. Only tools an approve rule names are advertised. Without such a rule, the request has no `tools` field.

**Approval.** Calls that need the operator arrive as one `ToolApprovalRequested` batch. The transcript shows `Tool approval · N calls`, each normalized `tool target`, and the decision; the upper input rule shows the key hint. Decisions are sent once, after every call is decided. Exiting or a failed turn drops the reply, which denies the batch and ends the turn. `s` grants the tool under the call's directory (the file's parent, or the listed directory) for the rest of the process, for this working directory and gateway only. A whole-workspace grant is not offered.

**Authority.** Model output, tool arguments, tool results, and transcript text cannot write a rule or a grant. Grants come only from `ahi.actions.yaml` and from decisions on the reply channel the screen owns. Notices and errors never repeat raw arguments; the prompt and results show only the normalized target.

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

- Reuse the existing dependency graph. The input box is a small editor in `src/chat/input.rs` (logical lines, cursor, and a separate visual layout) on top of pinned `ratatui` 0.30.0 and `crossterm` 0.29.0. Cell-aware wrapping directly uses `unicode-segmentation` 1.12.0 and `unicode-width` 0.2.0, both already resolved in Cargo.lock; no new package versions are introduced. Do not add `tui-textarea` or an async runtime. The editor accepts an `EditorKey`. `src/chat/key_binding.rs` translates a terminal key into a `KeyAction`, which carries that `EditorKey` when the key edits the input.
- Read the Git branch from `HEAD`, following `gitdir:` indirection for worktrees. Read worktree counts with read-only porcelain status. Refresh at chat load, normal message submission, and each `ToolCallReturned` event, never on frame ticks.
- Keep the screen state in a plain struct (`ChatScreen`) separate from drawing, so key handling and layout are testable without a terminal.
- Names follow the project naming rule: precise, descriptive, no slang (`CompletionBackend`, `EchoBackend`, `ThinkingLevel`, `render_session_row`).
- `unsafe_code` stays forbidden. Clippy must pass with the existing lint set.

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
| `backend/tools/` | Builtin `read_file` and `list_dir`, and working-directory containment |
| `../actions.rs` | `ahi.actions.yaml` parsing and rule evaluation |
| `../commands.rs` | Shared context-aware command registry, popup state/rendering, and explicit picker outcomes |

`mod.rs` may import `ratatui` and `crossterm`. `key_binding.rs` may import `crossterm` and must not import `ratatui`. `status.rs` may import either; it returns styled location spans and session row text. `mod.rs` applies dim usage styling and the muted model color when it draws. `transcript.rs` does not either: wrapping produces strings, and `mod.rs` places that string in a paragraph. `input.rs`, `usage.rs`, `command.rs`, `thinking.rs`, `location.rs`, and `backend/` have no terminal-rendering dependencies. `location.rs` owns the filesystem/subprocess boundary for the workspace snapshot.

## Verification

- Key handling: `Enter` submits, `Shift+Enter`, `Alt+Enter` and `Ctrl+J` insert a newline, and an empty input does not submit.
- Typing a draft wider than the terminal wraps it before submission and moves the upper rule upward. Cursor coordinates follow terminal-cell widths, including wide/combining Unicode text. Visual Up/Down, resize reflow, the eight-row limit, short-terminal scrolling, and unchanged submitted text are verified.
- The upper rule shows an advancing spinner at its left only while a response is active, with context still aligned at the right; completion restores the plain rule prefix.
- Commands: `/model`, `/thinking high`, `/thinking`, `/clear`, `/exit`, and an unknown command, each without a terminal.
- The thinking level is persisted to `<state>/thinking` and read back. An invalid file falls back to `medium`.
- Layout using `ratatui::backend::TestBackend`: colorful workspace and muted model share one footer row. Context is muted within the input's upper rule, with spacing and trailing rule cells. Narrow labels compact to percentage then disappear without wrapping. Unknown context is omitted, while known zero remains visible. Cumulative accounting is not rendered and the freed row goes to the transcript.
- The location row renders the Powerlevel10k-style directory and branch for a fixture repository, the short commit id for a detached HEAD, and no git segment outside a repository.
- HTTP fixture tests over loopback prove streamed text, usage, bounded failures, and tool-call validation; with no approving rule, no `tools` and no execution; an approve rule's call running and resubmitting `tool_calls` plus `role: "tool"` messages; approve, deny, and session-grant decisions; a dropped reply denying the batch; invalid, unknown, escaping, case-variant, and state-directory paths; the round cap; `maxRequestBytes` on resubmission; and cross-turn replay. The PTY workflow approves one call with `y` and denies another with `n`. The PTY workflow additionally takes an empty installation through Commands → gateway registration → explicit curation → checked-model Enter → an actual local streamed reply.
- Piped `ahi tui` output is byte-identical to v0.2.0.

## Remaining work and limits

- OpenAI Responses is implemented and verified with local HTTP fixtures; live browser-token inference acceptance was not checked in this implementation.
- Token counts are consumed when reported. Capacity and cost remain unknown without authoritative metadata. Without a tokenizer, context capacity is displayed, not an exact preflight token-budget guarantee.
- Reliable upstream cancellation, per-model reasoning-effort maps, image input, and native Responses reasoning/tool-result history remain future work. Gateway builtin exchanges replay in memory.
- The transcript, including gateway tool exchanges, is not persisted and is lost on exit. Session grants last for the process only.
- Gateway tools run in process with software path checks, not an OS sandbox. Other sandbox profiles are refused. OpenAI tool execution remains disabled.

Run the maintained PTY workflow and optionally capture 80×24/60×24 screens using the commands in [Terminal experience](TERMINAL_EXPERIENCE.md). Local POSIX fixtures do not prove remote gateway reachability, live OpenAI acceptance, or native Windows terminal behavior.
