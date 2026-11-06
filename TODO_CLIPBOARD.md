# Clipboard and text selection TODO

## Clipboard module structure (pi.dev-style)

- [x] Split `src/clipboard.rs` into `src/clipboard/`: `mod.rs` (platform routing), `command.rs` (bounded helper execution), `osc52.rs` (terminal writes), `windows.rs` (PowerShell handoff through a private UTF-8 file). WSL detection stays in `src/wsl.rs`.
- [x] Make routing a pure function: `Environment` → `Plan` of ordered `Step`s, so macOS, native Linux (Wayland/X11/Termux/headless), WSL (Windows Terminal vs. other emulators), native Windows, and SSH/mosh remote sessions are covered by unit tests without touching a real clipboard.
- [x] Native Windows uses the same private-file PowerShell handoff as WSL instead of piping text through PowerShell stdin, so console code pages cannot re-encode it.
- [ ] Add clipboard read (paste) adapters when an image or file paste feature needs them; pi.dev reads via `wl-paste`/`xclip`/PowerShell on WSL, which this module does not yet do.

## Drag-selection auto-scroll

- [ ] Allow the chat transcript to scroll while the user holds the mouse button and drags a text selection toward or beyond the top or bottom edge of the transcript viewport.
- [ ] Keep the original selection anchor stable as the viewport scrolls, extending the highlight into newly revealed text.
- [ ] Stop auto-scrolling when the drag returns inside the viewport or the mouse button is released.
- [ ] Preserve the existing copy-on-release behavior, including exact text across soft wraps and original line breaks.
- [ ] Verify upward and downward selection across multiple viewport heights without changing the input draft or selecting UI chrome.

## Vi-like keyboard selection mode

- [ ] Add an explicit keyboard-driven selection mode for selecting transcript text without a mouse.
- [ ] Define discoverable controls to enter and exit the mode, move a transcript cursor, start or extend a selection, and yank the selected text.
- [ ] Support vi-like navigation (`h`/`j`/`k`/`l`), visual selection (`v`), yank (`y`), and Escape to leave selection mode; finalize the exact bindings before implementation.
- [ ] Scroll the transcript as the selection cursor moves outside the visible viewport, preserving its anchor and highlight.
- [ ] Keep selection-mode keys separate from draft editing and preserve the draft when entering or leaving the mode.
- [ ] Reuse the shared clipboard adapters and existing copy feedback, retaining the selection if copying fails.
- [ ] Verify keyboard-only selection, scrolling, yank, and cancellation, including Unicode and soft-wrapped text.
