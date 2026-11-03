# Chat screen

Status: Part 1 (screen with a stand-in backend) is implemented. Part 2 (real completions) follows the model catalog work in `docs/MODEL_CATALOG.md`. The baseline is `src/tui.rs` at tag `v0.2.0`, plus `src/chat/`.

## Purpose

`ahi tui` ends today after the operator picks a model and `<state>/selection` is written. This document adds the screen the operator works in after that: a conversation view, an input box, and a two-row status area. The layout follows the familiar Claude Code and pi.dev arrangement.

## Layout

Reference: Ian's screenshot of pi.dev's Claude Code-style screen. Match that arrangement.

```
  conversation transcript (scrolls, no frame)
  tool output and diffs render as shaded blocks

⠋ Working
────────────────────────────────────────────────────────────────
█ input (no frame, no prompt character)
────────────────────────────────────────────────────────────────
~/Work/gh/ahi (main)
↑139k ↓11k R1.2M $0.502 (sub) 28.4%/272k (auto)     (openai) gpt-6.1-sol • medium
```

From top to bottom:

1. **Transcript.** Every row not used below, with no frame. Pinned to the bottom unless the operator has scrolled up.
2. **Activity row.** A spinner and `Working` while a response is in progress. Blank otherwise.
3. **Input.** A full-width horizontal rule above and below, in the accent color (cyan), with no side borders and no prompt character. Starts at one row and grows to at most 8 rows. Past 8 rows the input scrolls internally so the cursor stays in view.
4. **Location row.** The working directory (home shortened to `~`), then the git branch in parentheses. A detached HEAD shows the short commit id. Outside a repository, only the directory is shown. Shorten from the left with `…` when narrow.
5. **Session row**, left segment, in order, separated by single spaces:
   - `↑<n>` session input tokens
   - `↓<n>` session output tokens
   - `R<n>` cached input tokens read
   - `$<cost>` session cost, followed by `(sub)` when the credential is a subscription sign-in rather than metered API billing
   - `<p>%/<window>` context used as a percent of the model's window, for example `28.4%/272k`. Context used is the input token count of the most recent response.
   - `(auto)` when automatic context compaction is on (display only until compaction exists)

   Counts use `k` and `M` suffixes with at most one decimal. An unknown value is omitted together with its label, never shown as zero.
   Right segment: `(gateway) model • thinking-level`, right-aligned.

   When the row is too narrow, drop left-segment fields from the right end (`(auto)` first, then context, cost, cache read) until it fits. If the input and output counts still leave no room for the whole model segment, those counts are omitted as well. The model segment is never truncated.

Both status rows use the dim style except the model segment.

## Keys

| Key | Action |
|---|---|
| `Enter` | Submit the input. An empty or whitespace-only input does nothing. |
| `Shift+Enter`, `Alt+Enter` | Insert a newline. Both are accepted because many terminals do not report `Shift+Enter`. |
| `Ctrl+J` | Insert a newline (fallback for terminals that report neither). |
| `Left`, `Right`, `Home`, `End`, `Backspace`, `Delete` | Edit within the input. `Up` and `Down` move between input lines. |
| `PageUp`, `PageDown` | Scroll the transcript. |
| `Ctrl+T` | Cycle the thinking level. |
| `Ctrl+C` | Clear the input if it is not empty. Otherwise exit. |
| `Ctrl+D` | Exit when the input is empty. |
| `Esc` | Cancel an in-progress response (part 2). Does nothing in part 1. |

## Commands

An input that begins with `/` and matches a command is a command, not a message. An unknown command is reported in the transcript as `unknown command /name` and is not sent.

| Command | Action |
|---|---|
| `/model` | Reopen the model picker. A new selection is written to `<state>/selection` and takes effect for the next message. Cancelling the picker keeps the current selection. |
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
    Completed,
    Failed(String),
}

pub trait CompletionBackend {
    fn submit(&mut self, request: ChatRequest) -> std::sync::mpsc::Receiver<ResponseEvent>;
}
```

The screen drains the receiver on each frame tick (about 50 ms), so the user interface never blocks on the backend. No async runtime is added.

### Stand-in backend (part 1)

`EchoBackend` replies with the submitted text prefixed by `[stand-in]`, delivered as several `TextDelta` events about 20 ms apart, so streaming display is exercised. It reports `Usage` with input and output computed as the character count divided by four, `cached_input_tokens` and `cost_usd` as `None` (so those fields are omitted from the row), and a `ContextWindow` of 128000. Those figures are synthetic test data, and the transcript says so: the first reply in a session carries a note `stand-in backend: replies and token counts are not from a model`.

The stand-in is the only backend in part 1. `ahi tui` uses it after the picker, until part 2 replaces it.

## Flow

1. `ahi tui` in a terminal: load the registry and open the picker, unchanged.
2. On `Enter` in the picker: write `<state>/selection`, then open the chat screen with that selection.
3. If the picker is cancelled and a previous `<state>/selection` exists, open the chat screen with it. If none exists, exit as today.
4. `ahi tui` on a pipe: unchanged. It prints the load and never opens the chat screen.

## Implementation constraints

- No new dependencies. The input box is a small editor in `src/chat/input.rs` (a `Vec<String>` of lines and a cursor) on top of `ratatui` 0.30.0 and `crossterm` 0.29.0, which are already pinned. Do not add `tui-textarea` or an async runtime. The editor accepts an `EditorKey`. `src/chat/mod.rs` translates terminal events into that key.
- The git branch is read from the repository's `HEAD` file, following a `gitdir:` file for worktrees, using the existing `src/git.rs` helpers where they fit. Do not spawn `git` on each frame; read once at start and again after each submitted message.
- Keep the screen state in a plain struct (`ChatScreen`) separate from drawing, so key handling and layout are testable without a terminal.
- Names follow the project naming rule: precise, descriptive, no slang (`CompletionBackend`, `EchoBackend`, `ThinkingLevel`, `render_session_row`).
- `unsafe_code` stays forbidden. Clippy must pass with the existing lint set.

## Module layout

`src/chat/` is the conversation screen.

| File | Responsibility |
|---|---|
| `mod.rs` | `ChatScreen` state, the frame loop, key translation, and drawing |
| `input.rs` | The line editor. It accepts an `EditorKey`, not a terminal event |
| `transcript.rs` | Messages, wrapping, and the scroll window. It returns plain text |
| `status.rs` | Location-row and session-row text |
| `usage.rs` | Session token, cost, and context figures |
| `location.rs` | Working directory and the git label read from `HEAD` |
| `command.rs` | Parsing of `/model`, `/thinking`, `/clear`, and `/exit` |
| `thinking.rs` | `ThinkingLevel` and `<state>/thinking` |
| `backend/mod.rs` | `CompletionBackend`, `ChatRequest`, and `ResponseEvent` |
| `backend/echo.rs` | `EchoBackend`, the stand-in |

Only `mod.rs` and `status.rs` may import `ratatui` or `crossterm`. `status.rs` does not currently import either: it returns row text, and `mod.rs` applies the dim and normal styles when it draws. `transcript.rs` does not either: wrapping produces strings, and `mod.rs` places that string in a paragraph. `input.rs`, `usage.rs`, `command.rs`, `thinking.rs`, `location.rs`, and `backend/` are plain logic.

## Tests (part 1)

- Key handling: `Enter` submits, `Shift+Enter`, `Alt+Enter` and `Ctrl+J` insert a newline, and an empty input does not submit.
- The input box grows to 8 rows and no further.
- Commands: `/model`, `/thinking high`, `/thinking`, `/clear`, `/exit`, and an unknown command, each without a terminal.
- The thinking level is persisted to `<state>/thinking` and read back. An invalid file falls back to `medium`.
- Layout using `ratatui::backend::TestBackend`: at 120 columns with every usage field known, the session row matches the reference ordering exactly. At 60 columns, fields are dropped from the right end of the left segment and the model segment is kept whole. Fields with unknown values are omitted, not shown as zero.
- The location row shows the branch for a fixture repository, the short commit id for a detached HEAD, and no parentheses outside a repository.
- `EchoBackend` produces deltas, usage, and completion in order, and the first reply carries the stand-in note.
- Piped `ahi tui` output is byte-identical to v0.2.0.

## Part 2 (not in this PR)

- An OpenAI-compatible completions backend with streaming, for OpenAI and for `models.json` gateways.
- Token counts from the API's usage report. Context window per model, from the catalog or from `models.json`.
- Thinking level mapped to reasoning effort.
- `Esc` cancels a response.
- Depends on the open item in `docs/MODEL_CATALOG.md`: whether the `ahi login` credential is accepted by the API.

No conversation persistence in either part. The transcript is lost on exit until the Rust-native conversation store exists.
