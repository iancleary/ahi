# Chat screen

Status: Part 1 (screen with a stand-in backend) is implemented. Part 2 (real completions) follows the model catalog work in `docs/MODEL_CATALOG.md`. The baseline is `src/tui.rs` at tag `v0.2.0`, plus `src/chat/`.

## Purpose

`ahi tui` opens a local echo-backed chat after the operator picks a curated model and `<state>/selection` is written. The screen has a conversation view, an input box with context in its upper rule, and a single-row workspace/model footer. The layout follows the familiar Claude Code and pi.dev arrangement.

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
There is no second footer row. Cumulative input/output tokens, cache counts, cost, subscription, and compaction indicators are not permanently displayed. Backend accounting remains available internally; an on-demand usage view is future work, not implemented here. Context used is the input token count of the latest response, not the cumulative session total; capacity uses `k`/`M` suffixes with at most one decimal.

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
| `Esc` | Cancel an in-progress response (part 2). Does nothing in part 1. |

## Commands

An input that begins with `/` and matches a command is a command, not a message. An unknown command is reported in the transcript as `unknown command /name` and is not sent.

| Command | Action |
|---|---|
| `/model` | Reload local settings and reopen the curated model picker, retaining the last valid registry if an edit is invalid. A new selection is written to `<state>/selection` and takes effect for the next message without clearing the transcript. Cancelling the picker keeps the current selection. |
| `/thinking <level>` | Set the thinking level. With no argument, show the current level. |
| `/clear` | Clear the transcript and reset the session token totals. |
| `/exit` | Exit. |

## Thinking level

Levels: `off`, `minimal`, `low`, `medium`, `high`. Default is `medium`. The level is stored in `<state>/thinking` as a single line, beside `selection`, and is read at start. An unreadable or unknown value falls back to `medium` and is reported once in the transcript.

In part 1 the level is display and storage only. In part 2 it maps to the provider's reasoning-effort parameter. A model that does not support reasoning shows its level as `off` and ignores the setting; how ahi knows which models support it is part 2.

## Backend interface

`src/chat/` talks to a model through one trait, so the screen can be built and tested before real completions exist.

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
    ToolCallReturned, // future runner: every return, including failures
    Completed,
    Failed(String),
}

pub trait CompletionBackend {
    fn submit(&mut self, request: ChatRequest) -> std::sync::mpsc::Receiver<ResponseEvent>;
}
```

The screen drains the receiver on each frame tick (about 50 ms), so the user interface never blocks on the backend. No async runtime is added.

**Future tool-runner hook:** `ResponseEvent::ToolCallReturned` is an explicit integration stub. The future runner must emit it after every tool call returns (success or failure), before resuming the assistant response. Its handler reads the current directory/Git HEAD and worktree counts, compares them with the screen's stored snapshot, and updates changed values for the next redraw. It does not end the response or clear the transcript. Location/status are also refreshed at chat load and on normal message submission, not polled on frame ticks. HEAD is read directly; file counts use local `git --no-optional-locks -c core.fsmonitor=false status --porcelain=v1 -z --untracked-files=all`. This reads the worktree without optional index writes or network requests. Parsing uses NUL-delimited bytes so spaces, newlines, non-UTF-8 names, renames, and conflicts are counted correctly. Ahead/behind, stash counts, and in-progress Git actions are not displayed yet. The echo backend emits no tool events; actual tool execution and a tool-driven branch-change integration test remain future work.

### Stand-in backend (part 1)

`EchoBackend` replies with the submitted text prefixed by `[stand-in]`, delivered as several `TextDelta` events about 20 ms apart, so streaming display is exercised. It reports `Usage` with input and output computed as the character count divided by four, `cached_input_tokens` and `cost_usd` as `None`, and a `ContextWindow` of 128000. Those figures are synthetic test data, and the transcript says so: the first reply in a session carries a note `stand-in backend: replies and token counts are not from a model`.

The stand-in is the only backend in part 1. `ahi tui` uses it after the picker, until part 2 replaces it.

## Flow

1. `ahi tui` in a terminal: load the registry and open the picker, unchanged.
2. On `Enter` in the picker: write `<state>/selection`, then open the chat screen with that selection.
3. Cancelling the initial picker exits without changing `<state>/selection`, even if a previous selection exists. This deliberately avoids silently resuming a stale model. Cancelling a picker reopened by `/model` instead returns to the active chat and keeps its current selection.
4. `ahi tui` on a pipe: unchanged. It prints the load and never opens the chat screen.

## Implementation constraints

- Reuse the existing dependency graph. The input box is a small editor in `src/chat/input.rs` (logical lines, cursor, and a separate visual layout) on top of pinned `ratatui` 0.30.0 and `crossterm` 0.29.0. Cell-aware wrapping directly uses `unicode-segmentation` 1.12.0 and `unicode-width` 0.2.0, both already resolved in Cargo.lock; no new package versions are introduced. Do not add `tui-textarea` or an async runtime. The editor accepts an `EditorKey`. `src/chat/key_binding.rs` translates a terminal key into a `KeyAction`, which carries that `EditorKey` when the key edits the input.
- Read the Git branch from `HEAD`, following `gitdir:` indirection for worktrees. Read worktree counts with read-only porcelain status. Refresh at chat load, normal message submission, and the future `ToolCallReturned` event, never on frame ticks.
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
| `command.rs` | Parsing of `/model`, `/thinking`, `/clear`, and `/exit` |
| `thinking.rs` | `ThinkingLevel` and `<state>/thinking` |
| `backend/mod.rs` | `CompletionBackend`, `ChatRequest`, and `ResponseEvent` |
| `backend/echo.rs` | `EchoBackend`, the stand-in |

`mod.rs` may import `ratatui` and `crossterm`. `key_binding.rs` may import `crossterm` and must not import `ratatui`. `status.rs` may import either; it returns styled location spans and session row text. `mod.rs` applies dim usage styling and the muted model color when it draws. `transcript.rs` does not either: wrapping produces strings, and `mod.rs` places that string in a paragraph. `input.rs`, `usage.rs`, `command.rs`, `thinking.rs`, `location.rs`, and `backend/` have no terminal-rendering dependencies. `location.rs` owns the filesystem/subprocess boundary for the workspace snapshot.

## Tests (part 1)

- Key handling: `Enter` submits, `Shift+Enter`, `Alt+Enter` and `Ctrl+J` insert a newline, and an empty input does not submit.
- Typing a draft wider than the terminal wraps it before submission and moves the upper rule upward. Cursor coordinates follow terminal-cell widths, including wide/combining Unicode text. Visual Up/Down, resize reflow, the eight-row limit, short-terminal scrolling, and unchanged submitted text are verified.
- The upper rule shows an advancing spinner at its left only while a response is active, with context still aligned at the right; completion restores the plain rule prefix.
- Commands: `/model`, `/thinking high`, `/thinking`, `/clear`, `/exit`, and an unknown command, each without a terminal.
- The thinking level is persisted to `<state>/thinking` and read back. An invalid file falls back to `medium`.
- Layout using `ratatui::backend::TestBackend`: colorful workspace and muted model share one footer row. Context is muted within the input's upper rule, with spacing and trailing rule cells. Narrow labels compact to percentage then disappear without wrapping. Unknown context is omitted, while known zero remains visible. Cumulative accounting is not rendered and the freed row goes to the transcript.
- The location row renders the Powerlevel10k-style directory and branch for a fixture repository, the short commit id for a detached HEAD, and no git segment outside a repository.
- `EchoBackend` produces deltas, usage, and completion in order, and the first reply carries the stand-in note.
- Piped `ahi tui` output is byte-identical to v0.2.0.

## Part 2 (not in this PR)

- An OpenAI-compatible completions backend with streaming, for OpenAI and for `models.json` gateways.
- Token counts from the API's usage report. Context window per model, from the catalog or from `models.json`.
- Thinking level mapped to reasoning effort.
- `Esc` cancels a response.
- Depends on the open item in `docs/MODEL_CATALOG.md`: whether the `ahi login` credential is accepted by the API.

No conversation persistence in either part. The transcript is lost on exit until the Rust-native conversation store exists.
