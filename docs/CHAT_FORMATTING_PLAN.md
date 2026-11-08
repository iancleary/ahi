# Chat formatting plan

Status: draft, decisions settled with the owner on 2026-11-07, nothing implemented. Eight requested improvements to how the chat transcript looks and what it responds to, each written against the code as it stands at 0.12.0. The **Shared foundation** section comes first because seven of the eight items need it; the per-item sections then say what the operator sees, what changes, and what to test. Companion documents: [Chat screen](CHAT_SCREEN.md) for the layout contract, [Terminal experience](TERMINAL_EXPERIENCE.md) for interaction design, [Tool calls](TOOL_CALLS.md) for the tool lifecycle, [SPEC.md](SPEC.md) for the compatibility and security contract.

Constraints carried over from the rest of the repository:

- **No new registry crates.** Every parser, lexer, and protocol writer below is an in-repo module with tests, the same way `base64`, `time`, and the forked widgets were brought in-house. No `pulldown-cmark`, no `syntect`, no `similar`, no `open`.
- **`unsafe_code = "forbid"` stays.** Nothing here needs FFI.
- **Copy stays exact.** Mouse and keyboard selection copy the original text of a row, never the padding and never styling. Formatting must therefore live beside the text, not inside it.
- **Streaming stays cheap.** `TranscriptLayout::resume_last` re-wraps only the last row of a streamed reply. Any formatter has to tolerate an unfinished message (an open code fence, a half-typed link) and settle without reflowing the whole transcript every frame.
- **Deterministic rendering.** Screen tests compare rendered buffers. Colors and glyphs are constants, not terminal queries.

## Shared foundation: styled rows

Today a `TranscriptRow` is one `String`, one `break_after` flag, and one `Style` for the whole row (`src/chat/transcript/render.rs`). The message role picks a background; nothing inside a row can differ from the rest of it.

Proposed change, the only data-model change in this plan:

```rust
pub(crate) struct TranscriptRow {
    pub(crate) text: String,
    pub(crate) break_after: bool,
    /// Row background and default foreground, from the message role.
    style: Style,
    /// Byte ranges of `text` drawn differently, non-overlapping, ascending.
    /// Empty for the common case. Selection and copy ignore this field.
    spans: Vec<RowSpan>,
    /// Things under the pointer: a URL, a file reference, a code block.
    /// Empty for the common case. Keyed by byte range of `text`.
    targets: Vec<RowTarget>,
}

struct RowSpan { start: usize, end: usize, style: Style }
enum RowTarget { Url { start: usize, end: usize }, FileRef { start: usize, end: usize, path: String, line: Option<u32> } }
```

- `padded_line` becomes a span builder: it slices `text` at span boundaries, applies `style.patch(span.style)` to each slice, and pads the tail. Rows without spans take the current single-`Line::styled` path, so a plain transcript costs what it costs now.
- `layout_paragraphs` grows a sibling, `layout_styled`, that takes `(&str, Vec<RowSpan>)` per paragraph and splits spans at soft-wrap points. The wrap loop is unchanged; it only carries a span cursor alongside the grapheme cursor.
- A `Formatter` trait with one implementation per message kind (`PlainText`, `Markdown`, `Diff`, `ToolHeader`) produces `Vec<(String, Vec<RowSpan>, Vec<RowTarget>)>` paragraphs from a `ChatMessage`. `layout_message` picks the formatter from the role and tool payload. Tests in `render.rs` already compare the incremental layout with the one-shot layout; they keep doing that, now with spans and targets compared too.
- Selection (`src/chat/selection.rs`) reads `row.text` and never looks at spans. The highlight is applied after span styles, so a selected code span is still visibly selected.

Theme constants go in one place, `src/chat/transcript/theme.rs`, next to the three role backgrounds that live inline in `layout_message` today. All colors are `Color::Rgb` so they do not depend on the terminal palette, matching the existing choice.

Estimated size: about 250 lines of change plus tests, all inside `src/chat/transcript/`. Every item below is then additive.

## 1. Clickable URLs

**What the operator sees.** A URL in any transcript row is underlined and colored. With mouse capture on, a left click on it without a drag opens it in the system browser. In keyboard selection mode, `Enter` or `o` on a row with a URL under the cursor does the same. A notice `opened https://…` confirms; a failure says why.

**Why not OSC 8 hyperlinks.** OSC 8 would make URLs clickable with the terminal's own modifier-click even with mouse capture off, which is the better experience. It is not reachable from `ratatui-core` 0.1.0: the cell buffer has no hyperlink attribute and `ratatui-crossterm` would not emit one. Writing escape sequences around `Frame::render_widget` would race the diff-based redraw. Record it as a follow-up for when ratatui's buffer carries hyperlinks, or for a forked backend in `ahi-widgets`; the in-row `RowTarget::Url` built here is the data such a backend would consume.

**Changes.**

- `src/chat/transcript/links.rs`: a scanner that finds `http://` and `https://` runs, trims trailing `.,;:)]}>"'` and matching close parens the way GitHub does, and refuses anything with control characters or more than 2 KiB. Tests from a fixed list of good and bad strings. No `url` crate.
- `PlainText` and `Markdown` formatters call it; Markdown's explicit `[text](url)` link form marks the text as the target (see item 6).
- `src/chat/open.rs`: `open_url(url) -> Result<()>` spawns `open` on macOS, `xdg-open` on Linux, `cmd /c start "" <url>` on Windows, `wslview` then `powershell.exe Start-Process` under WSL, reusing the environment detection in `src/clipboard/wsl.rs` and the bounded helper runner in `src/clipboard/command.rs`. Only `http` and `https` schemes are ever passed; the argument is passed as one argv element, no shell on any platform except the Windows `cmd` form, which is quoted the same way pi.dev does.
- `ChatScreen`: a `MouseEventKind::Up(Left)` with no drag (begin and release on the same cell) resolves the cell to a row and byte offset through the existing selection hit-testing and looks up `targets`. A drag still copies, so copy-by-drag of a URL is unchanged.
- Keyboard selection mode: `Enter`/`o` on a target. Documented in the `SELECT` hint line.
- Setting: `/mouse` already exists; add nothing. Opening is always explicit, so no auto-open setting is needed.

**Tests.** Scanner vectors; a screen test that clicks a URL cell and asserts the spawned argv through a test hook (the clipboard module already injects a command runner for this); a test that a drag over a URL copies and does not open.

## 2. Inline code and fenced code blocks

**What the operator sees.** Assistant text between single backticks is drawn in a distinct foreground on a slightly raised background, the backticks hidden. A fenced block is drawn as a full-width shaded block with a one-row header showing the language tag (or nothing) and, with mouse on, a `copy` affordance at the right end; its body keeps indentation and never soft-wraps mid-token. The body is highlighted when the language has an in-repo lexer; otherwise it is monochrome on the shaded background.

**Changes.**

- `Markdown` formatter (item 6) owns both. During streaming an open fence is treated as a fence to end of message, so the block appears shaded from its first row and does not flash plain then shaded when the closing fence arrives.
- Long code lines: wrap at width like today (the row model has no horizontal scroll), but mark continuation rows with a `↪` in a dim gutter so a wrapped line is distinguishable from two lines. Copy still yields the original line because `break_after` is false on the soft wrap.
- Clicking the `copy` affordance (a `RowTarget::CopyBlock { message, block }`) copies the block body through `clipboard::copy`, the same path as selection. Keyboard: `y` on a code-block header row in selection mode.
- `src/chat/transcript/highlight.rs`: a small token lexer, one function per language, each about 40 lines: comments, strings, numbers, a keyword list, and punctuation. Start with `rust`, `toml`, `yaml`, `json`, `sh`/`bash`/`zsh`, `python`, `typescript`/`javascript`, `markdown`, `diff`, `typst`, `powershell`/`ps1`. Unknown tags fall through to monochrome. Tests are golden token lists for a short snippet per language. This replaces `syntect` (which would pull `onig` or `fancy-regex`, plus syntax definitions, far outside the dependency policy).
- Performance: highlighting runs per paragraph at layout time and is incremental the same way wrapping is, because the formatter runs inside `layout_message`. A fence's lexer state (inside a block comment or string) is reset per line, deliberately, so a resumed last row never depends on earlier rows.

**Tests.** Golden layouts for inline code crossing a soft wrap, an unterminated fence mid-stream, nested backticks, a fence with four-space indentation (not a fence), and tabs expanded to the width the row renders at.

## 3. Turn duration and completion time

**What the operator sees.**

- While a turn runs, the activity line reads `⠋ Working · 12s · Esc cancels`, counting up in whole seconds. During tool calls it reads `⠋ run_command cargo test · 4s`.
- When the turn ends, a right-aligned dim footer row under the assistant's last message reads `done in 1m 12s · 14:02` (local time, `HH:MM`). On failure, `failed after 12s · 14:02`. On cancel, `cancelled after 12s`.
- Each tool result's header row (item 5) ends with its own duration: `run_command cargo test · exit 0 · 3.8s`.

**Changes.**

- `ChatScreen` already keeps `Instant`s for frames and the copy status. Add `turn_started: Option<Instant>` set on send and cleared on `Completed`/`Failed`/cancel, read by the activity line. The 50 ms frame timer already exists, so the counter updates without a new timer.
- `ResponseEvent::ToolResult` gains `elapsed: Duration`, measured in `tool_loop.rs` around `tools::execute`. `ResponseEvent::Completed` carries nothing new; the screen computes the turn duration itself.
- Local time without the `time` or `chrono` crates: the in-repo RFC 3339 module already converts a Unix timestamp to a civil date and time. The offset comes from `src/localtime.rs`, a TZif v2/v3 reader (about 120 lines, tested against IANA vectors) that reads `$TZ` when it names a zone file, else `/etc/localtime`; on Windows it reads the bias from the registry-free `GetTimeZoneInformation` equivalent only if `ahi-win32` grows a safe wrapper, otherwise UTC. When the zone cannot be read, or `clock: utc` is set in settings (`/clock utc|local` for the session), the footer shows UTC with a `Z` suffix (`14:02Z`).
- Footer rows are `TranscriptRole::Notice` messages with a new `NoticeKind::TurnEnd` so they are excluded from what is sent to the model (already true for notices) and drawn dim and right-aligned by the `PlainText` formatter.

**Tests.** A screen test that drives a fake clock through send, two tool results, and completion and asserts the activity text at each frame and the final footer row. Duration formatting vectors: `0.4s`, `3.8s`, `12s`, `1m 12s`, `1h 02m`.

## 4. File edit diffs with colored backgrounds and highlighting

**What the operator sees.** After `edit_file` or `write_file`, the tool block shows a unified diff: a header row `edit_file src/main.rs · +3 -1 · 0.2s`, then hunks with `@@` rows, context rows on the normal tool background, removed rows on a red background, added rows on a green background, and a thin gutter with old and new line numbers. Within a changed pair, the changed characters are drawn with a stronger red or green (word-level emphasis). The body is syntax-highlighted by file extension using the item 2 lexers, with the diff background taking precedence over any token background. `write_file` creating a new file shows every row as added, capped like any tool preview, with `… N more lines` at the end.

**Changes.**

- `tools::WriteReport` already carries `before: Option<String>` and `after: String`. Keep them, but `Outcome.output` is a plain string today and `preview()` has already truncated it by the time the screen sees it. Add `ResponseEvent::ToolResult { detail: Option<ToolDetail> }` where `ToolDetail::Write { path, before, after, lines_added, lines_removed }`. The transcript stores it as `ChatMessage.detail`, bounded by the same 256 KiB turn budget that already bounds `content`.
- `src/chat/transcript/diff.rs`: a line-level diff. Myers with the linear-space refinement is about 150 lines; for the sizes involved (files under the 64 KiB read cap) a plain Myers on lines with a 10 000-line guard that falls back to "whole before removed, whole after added" is enough and simpler to test. Word-level emphasis inside a replaced block: a second pass of the same algorithm on the Unicode words of paired lines, only when both sides are under 1 KiB. Context is three lines, hunks merged when closer than six.
- `Diff` formatter: builds rows with `RowSpan`s for the gutter, the +/- marker, and emphasis, then calls the lexer on the content portion and merges spans so background comes from the diff and foreground from the lexer.
- The approval prompt is unchanged: it shows counts only, by design (`CHAT_SCREEN.md` says patterns and content are never displayed before approval). The diff appears only after the write ran.
- The model-facing `content` is unchanged (`edited path: 1 replacement, +3 -1 lines`), so no protocol or replay change.

**Tests.** Diff algorithm vectors (empty before, empty after, identical, one-line change, move detection not attempted, CRLF files kept byte-exact). Golden rendered rows for a two-hunk edit, checking the backgrounds, gutter widths, and that copy of a diff row yields `git diff` form: the `+`/`-` marker and content, without gutter line numbers (the gutter is a hidden span over a prefix of `text`).

## 5. File write and command output blocks

**What the operator sees.** Every tool result opens with a header row naming the tool, its target, and its result, in a consistent grammar:

```
read_file src/main.rs · 1–120 of 834 · 0.0s
search_files "TODO" in src · 14 matches · 0.1s
edit_file src/main.rs · +3 -1 · 0.2s
write_file docs/new.md · created · 42 lines · 0.0s
run_command cargo test --locked · exit 0 · 38.2s
run_command cargo test --locked · exit 101 · 12.4s
```

Under `run_command`, stdout and stderr are shown as two labeled sub-blocks (`stdout`, `stderr`), each capped as today, stderr on the alert background when the exit code is non-zero. The rendered command is the exact argv quoted shell-style, the same string the approval prompt shows, so what was approved is what is recorded on screen. Under `write_file`, the body is the item 4 diff. Under `read_file`, the body is the preview with the lexer applied by extension.

**Changes.**

- `ToolDetail` from item 4 grows variants: `Command { argv, cwd, exit_code, stdout, stderr, timed_out }`, `Read { path, offset, limit, total_lines }`, `Search { pattern_shown: bool, matches }`. For `Search`, the pattern is shown here because the call already ran and the operator should see what the model asked for; the counts-only rule applies to the approval prompt only.
- The `ToolHeader` formatter renders the first row; the role background stays the existing green or red by exit code.
- The `preview()` function moves from `tool_loop.rs` into the formatter so the cap applies to rows, not bytes, and `… N more lines` counts what was not shown. `content` sent to the model is untouched.
- Denied and errored calls keep their one-line form, now in the same grammar: `edit_file src/main.rs · denied: approval cancelled`.

**Tests.** The existing `tool_loop` tests assert `output` strings; they move to asserting `ToolDetail`. Golden rows per tool kind; a `run_command` with both streams and a non-zero exit; a timed-out command.

## 6. Markdown rendering

**What the operator sees.** Assistant messages render: headings (bold, accent color, `#` hidden, one blank row before), paragraphs, bold and italic, inline code and fences (item 2), bulleted and numbered lists with hanging indent across soft wraps, block quotes with a `▎` bar, links as underlined text with the URL as the click target (item 1), horizontal rules as a dim full-width line, and tables rendered with column alignment when they fit the width and as pipe text when they do not. Operator messages stay plain text; so do tool results and notices.

**Changes.**

- `src/chat/transcript/markdown.rs`: a block-level parser (heading, fence, list item, quote, rule, table row, paragraph) and an inline parser (code, bold, italic, link, autolink) that emit paragraphs with spans and targets. CommonMark in full is out of scope; the subset is what models produce. Each rule has a test with input and expected spans. Anything the parser does not recognize is shown as typed, so an edge case degrades to plain text, never to lost text.
- Streaming: the parser runs on the whole message text of the last message each time it changes, but only re-lays rows from the last block boundary before the change. This needs `resume_last` to resume from a block start instead of the last row. Block starts are cheap to find (a blank line or a fence line scanning backward). For the common case of a paragraph being appended to, the cost is the same as today.
- `/plain` command toggles rendering off for the session and `/markdown` turns it back on, with the state shown in the footer like `mouse on`. Keyboard selection and copy are unaffected either way because `text` keeps the original characters.
- Copy offers both forms. Hidden markers (`#`, `**`, backticks, link URLs) stay in `text` and are drawn with a `RowSpan { hidden: true }` style; the wrap loop skips hidden ranges when counting cells. A copy then has two outputs: **raw markdown** (the row text as is, what the model wrote) and **rendered** (hidden ranges dropped, so the text reads as it appears on screen). Mouse release and `y` copy the rendered form by default; `Y` and `Ctrl+Shift+Y` copy raw; `/copy rendered|raw` flips the default for the session and writes it to settings beside `thinking`. The copy status line names the form used (`Selection copied (raw markdown)`). Code block bodies are identical in both forms because fences contain no markers.
- Tables: only when every row has the same column count and the widest layout fits; otherwise the rows pass through as text.

**Tests.** Parser vectors; golden rows for a list item wrapping with hanging indent; a heading mid-stream (`#` then text arriving later) not flashing as a paragraph; a message that is pure markdown markers (`***`) rendering as a rule; `/plain` restoring the exact pre-render rows.

## 7. Clickable file references that open an editor

**What the operator sees.** Any `path` or `path:line` or `path:line:col` that resolves to an existing file under the working directory is underlined in tool output and assistant text. A click (or `Enter`/`o` in selection mode) opens it in the configured editor at that line. A notice confirms `opened src/main.rs:42 in code`. Tool headers (item 5) always make their target path clickable; `search_files` makes every `path:line:text` row's prefix clickable, which is the main reason to build this.

**Changes.**

- `src/chat/transcript/links.rs` gains a file-reference scanner: a token with a `/` or a known extension, optional `:line` and `:col`, no spaces, no `..`, no leading `/`. Candidates are checked against the filesystem once per message at layout time, through the same `Workspace` confinement used by the tools (`tools/workspace.rs`), so nothing outside the working directory is ever a target and no symlink is followed. Results are cached per message in the row targets so redraws do not stat.
- Editor configuration, in order: `editor` in the state-dir settings (next to `thinking`), then `$AHI_EDITOR`, then `$VISUAL`, then `$EDITOR`, then a probe for `code` on `PATH`, which is the expected default for most installs. The value is an argv template with `{path}`, `{line}`, `{col}`; built-in templates for `code` (`code -g {path}:{line}:{col}`), `cursor`, `zed` (`zed {path}:{line}`), `subl`, `nvim`/`vim` (`+{line} {path}`, opened through the terminal only if the chat is suspended, so the first version opens terminal editors in a new window via the platform opener and otherwise declines with a notice). `/editor <name|argv>` sets it for the session and writes it to settings.
- Spawn through `clipboard/command.rs`'s bounded runner, detached, with the hook runner's scrubbed environment. The path is passed as one argv element, never interpolated into a shell string.
- Approval: opening an editor is not a tool call and is operator-initiated, so no `ahi.tools.yaml` rule is involved. Document that in `SPEC.md`.

**Tests.** Scanner vectors including Windows-style paths (rejected, consistent with tool path rules), `Cargo.toml` with no slash (accepted by extension), `a/b.rs:12:5`, `a/b.rs:` (trailing colon trimmed). Template expansion for each built-in editor. A screen test clicking a `search_files` result row and asserting argv.

## 8. Startup header

**What the operator sees.** When the chat screen opens with an empty transcript, the first rows are a header block, drawn once as a notice and scrolled away like any message:

```
  ▄▀█ █░█ █
  █▀█ █▀█ █   ahi 0.12.0
              gpt-5.1 via openai · thinking medium
              ~/Work/gh/ahi on main · 14 tools approved by ahi.tools.yaml
              /help for commands · Ctrl+S selects · /mouse toggles native selection
```

The wordmark is text, not an image. The header is a notice, so it is never sent to the model and `/clear` removes it.

**Changes.**

- `src/chat/header.rs`: builds the header text from `env!("CARGO_PKG_VERSION")`, the selection, the thinking level, the location snapshot the footer already keeps, and the count of approved tools from the loaded policy. Width-aware: below 60 columns it drops the wordmark and shows two rows.
- The wordmark is a `const &str` of block glyphs, drawn in the accent color through `RowSpan`s. Half-block art for the three letters is four rows at most; the sample above is two rows, which is the recommendation so it does not push the first message off a small screen.

**Tests.** Golden header rows at 120, 80, and 50 columns; a test that the header is absent when a transcript is restored with messages; a test that `/clear` removes it.

## Delivery order

Each step is one PR and leaves `main` releasable.

1. **Styled rows** (foundation). No visible change except a theme module. Gate: every existing screen test passes untouched, as with the widget fork.
2. **Tool headers and detail events** (items 5 and 3's tool durations). Visible: consistent tool header rows with timing.
3. **Diffs** (item 4). Depends on 2.
4. **Markdown and code** (items 6 and 2, including the lexers). Largest step; ship the parser with `/plain` so it can be turned off if a model's output trips it.
5. **Turn timing footer** (rest of item 3). Small; placed here so the footer can be markdown-free by construction.
6. **Links and file references** (items 1 and 7). Depends on 1; shares the opener and scanner module.
7. **Startup header** (item 8).

## Decisions

All settled; kept as the record of why.

- **D1.** Decided: copy offers both rendered and raw markdown; rendered is the default, `/copy` switches.
- **D2.** Decided: local time through a small in-repo TZif reader; `clock: utc` in settings (and `/clock utc|local`) switches to UTC with a `Z` suffix.
- **D3.** Decided: show what the model asked for. Post-run tool headers show search patterns, globs, and rendered commands; only the pre-run approval prompt keeps its existing counts-only rule.
- **D4.** Decided: diff copy matches `git diff` output: `+`/`-` markers kept, gutter line numbers dropped.
- **D5.** Decided: probe for `code` on `PATH` when nothing is configured; `/editor <name|argv template>` sets the launch command (built-in names for vscode/code, cursor, zed, subl, vim/nvim) and persists it.
- **D6.** Decided: no image logo; the header is text only.
- **D7.** Decided: the nine listed plus `typst` and `powershell` (`ps1`), eleven lexers in the first release.

## Not in this plan

- OSC 8 hyperlinks (blocked on the rendering stack; see item 1).
- An inline image logo (kitty or iTerm2 graphics), dropped: it would be the only write outside the ratatui buffer.
- Horizontal scrolling of wide code or diff rows.
- Conversation persistence, which would make the header's "restored transcript" case reachable from disk; the header rule is written to be correct when it arrives.
- Editing the approval prompt to show diff content before a write runs. The current contract shows counts only.
