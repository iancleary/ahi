# Clipboard and text selection TODO

## Clipboard module structure (pi.dev-style)

- [x] Split `src/clipboard.rs` into `src/clipboard/`: `mod.rs` (platform routing), `command.rs` (bounded helper execution), `osc52.rs` (terminal writes), `windows.rs` (PowerShell handoff through a private UTF-8 file). WSL detection lives in `src/clipboard/wsl.rs`.
- [x] Make routing a pure function: `Environment` → `Plan` of ordered `Step`s, so macOS, native Linux (Wayland/X11/Termux/headless), WSL (Windows Terminal vs. other emulators), native Windows, and SSH/mosh remote sessions are covered by unit tests without touching a real clipboard.
- [x] Native Windows writes through the Win32 clipboard API (in-repo `crates/ahi-win32` workspace crate, the workspace's only `unsafe`, no registry dependency, so `ahi` keeps `unsafe_code = "forbid"`) and falls back to the private-file PowerShell handoff only if that fails.
- [ ] Run the Windows round-trip test on a real Windows machine or CI; the code is only cross-checked from macOS so far.
- [ ] Add clipboard read (paste) adapters when an image or file paste feature needs them; pi.dev reads via `wl-paste`/`xclip`/PowerShell on WSL, which this module does not yet do.

## Drag-selection auto-scroll

- [x] Allow the chat transcript to scroll while the user holds the mouse button and drags a text selection toward or beyond the top or bottom edge of the transcript viewport. (`Selection::edge` + `ChatScreen::tick`, one row per 50 ms frame. The top edge triggers on the first transcript row because terminals report no pointer position above the window.)
- [x] Keep the original selection anchor stable as the viewport scrolls, extending the highlight into newly revealed text. (`Selection::follow` re-applies the last pointer position after each scroll step.)
- [x] Stop auto-scrolling when the drag returns inside the viewport or the mouse button is released.
- [x] Preserve the existing copy-on-release behavior, including exact text across soft wraps and original line breaks.
- [x] Verify upward and downward selection across multiple viewport heights without changing the input draft or selecting UI chrome. (Unit test in `selection/tests.rs`, screen test `dragging_past_the_bottom_edge_auto_scrolls_and_extends_the_selection`.)
- [ ] Try it in a real terminal: auto-scroll speed is fixed at one row per frame and may want to accelerate the further the pointer is past the edge.

## Vi-like keyboard selection mode

- [x] Add an explicit keyboard-driven selection mode for selecting transcript text without a mouse.
- [x] Define discoverable controls to enter and exit the mode, move a transcript cursor, start or extend a selection, and yank the selected text. (`Ctrl+S` or `/select` in the Commands popup; the activity line shows `SELECT`/`VISUAL` with key hints.)
- [x] Support vi-like navigation (`h`/`j`/`k`/`l`), visual selection (`v`), yank (`y`), and Escape to leave selection mode. Final bindings: `h`/`j`/`k`/`l` and arrows, `w`/`b`, `0`/`$`, `g`/`G`, Page Up/Down, `v`/`V`, `y` or `Ctrl+Y`, `Esc` or `Ctrl+C`. Not bound: counts, `e`/`W`/`B`, search.
- [x] Scroll the transcript as the selection cursor moves outside the visible viewport, preserving its anchor and highlight.
- [x] Keep selection-mode keys separate from draft editing and preserve the draft when entering or leaving the mode. (Selection mode is matched before approval and editing keys in `apply_terminal_event`.)
- [x] Reuse the shared clipboard adapters and existing copy feedback, retaining the selection if copying fails. (`y` calls the same `copy_selection`; the highlight is kept after yank so `Ctrl+Y` retries.)
- [x] Verify keyboard-only selection, scrolling, yank, and cancellation, including Unicode and soft-wrapped text. (`keyboard_cursor_snaps_to_graphemes_and_visual_range_yanks_exact_text`, `keyboard_selection_mode_moves_selects_yanks_and_preserves_the_draft`.)
- [x] `w`/`b` word motions and `V` line-wise visual, matching Vim/Neovim: word classes follow `utf_class` (keyword per default `iskeyword`, punctuation, emoji, one class per CJK script block), `V` selects whole logical lines with soft wraps rejoined and a trailing newline on yank, `v`/`V` switch kinds and repeat to leave.
