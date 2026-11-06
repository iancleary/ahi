//! Translation of a terminal key into a [`KeyAction`].
//!
//! This module imports `crossterm` and does not import `ratatui`. The screen
//! applies the action. It does not match key codes.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::approval::ApprovalKey;
use super::input::EditorKey;
use super::selection::CursorMove;

/// What one key press asks the screen to do.
///
/// `ClearInputOrExit` and `ExitIfInputEmpty` stay one action each because the
/// choice depends on the input contents, which the binding does not read.
/// Page Up scrolls toward older transcript lines because the transcript is
/// pinned to the bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyAction {
    Submit,
    ClearInputOrExit,
    ExitIfInputEmpty,
    Cancel,
    ScrollTowardOlder,
    ScrollTowardNewer,
    JumpToTop,
    JumpToBottom,
    CycleThinkingLevel,
    CopySelection,
    /// Ctrl+S: keyboard selection mode over the transcript.
    EnterSelectionMode,
    Edit(EditorKey),
    Ignore,
}

/// What one key press asks of keyboard selection mode. These replace
/// [`translate`] while the mode is active, so no key reaches the draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectionKey {
    Move(CursorMove),
    ToggleVisual,
    Yank,
    Exit,
    Ignore,
}

/// vi-like movement plus the arrow and paging keys the rest of chat uses.
pub(crate) fn translate_selection(key: KeyEvent) -> SelectionKey {
    if key.kind != KeyEventKind::Press {
        return SelectionKey::Ignore;
    }
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => SelectionKey::Exit,
        KeyCode::Char('c') if control => SelectionKey::Exit,
        KeyCode::Char('y' | 'Y') => SelectionKey::Yank,
        _ if control || key.modifiers.contains(KeyModifiers::ALT) => SelectionKey::Ignore,
        KeyCode::Char('v') => SelectionKey::ToggleVisual,
        KeyCode::Char('h') | KeyCode::Left => SelectionKey::Move(CursorMove::Left),
        KeyCode::Char('l') | KeyCode::Right => SelectionKey::Move(CursorMove::Right),
        KeyCode::Char('k') | KeyCode::Up => SelectionKey::Move(CursorMove::Up),
        KeyCode::Char('j') | KeyCode::Down => SelectionKey::Move(CursorMove::Down),
        KeyCode::Char('0') | KeyCode::Home => SelectionKey::Move(CursorMove::LineStart),
        KeyCode::Char('$') | KeyCode::End => SelectionKey::Move(CursorMove::LineEnd),
        KeyCode::Char('g') => SelectionKey::Move(CursorMove::Top),
        KeyCode::Char('G') => SelectionKey::Move(CursorMove::Bottom),
        KeyCode::PageUp => SelectionKey::Move(CursorMove::PageOlder),
        KeyCode::PageDown => SelectionKey::Move(CursorMove::PageNewer),
        _ => SelectionKey::Ignore,
    }
}

/// Keys that answer a pending tool approval. Other keys fall back to
/// [`translate`], and the screen admits only exit and scrolling actions.
pub(crate) fn translate_approval(key: KeyEvent) -> Option<ApprovalKey> {
    if key.kind != KeyEventKind::Press
        || key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        KeyCode::Char('y' | 'Y') => Some(ApprovalKey::Approve),
        KeyCode::Char('s' | 'S') => Some(ApprovalKey::ApproveForSession),
        KeyCode::Char('n' | 'N') => Some(ApprovalKey::Deny),
        KeyCode::Char('a' | 'A') => Some(ApprovalKey::ApproveRest),
        KeyCode::Esc => Some(ApprovalKey::DenyRest),
        KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(ApprovalKey::PreviousCall)
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => Some(ApprovalKey::NextCall),
        KeyCode::Up => Some(ApprovalKey::Previous),
        KeyCode::Down => Some(ApprovalKey::Next),
        KeyCode::Enter if !key.modifiers.intersects(KeyModifiers::SHIFT) => {
            Some(ApprovalKey::Choose)
        }
        _ => None,
    }
}

pub(crate) fn translate(key: KeyEvent) -> KeyAction {
    if key.kind != KeyEventKind::Press {
        return KeyAction::Ignore;
    }
    if is_newline(&key) {
        return KeyAction::Edit(EditorKey::Newline);
    }
    match key.code {
        KeyCode::Enter => KeyAction::Submit,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            KeyAction::ClearInputOrExit
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            KeyAction::ExitIfInputEmpty
        }
        KeyCode::Char('k' | 'K') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            KeyAction::JumpToTop
        }
        KeyCode::Char('l' | 'L') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            KeyAction::JumpToBottom
        }
        KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            KeyAction::CycleThinkingLevel
        }
        KeyCode::Char('s' | 'S') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            KeyAction::EnterSelectionMode
        }
        KeyCode::Char('y' | 'Y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            KeyAction::CopySelection
        }
        KeyCode::Esc => KeyAction::Cancel,
        KeyCode::PageUp => KeyAction::ScrollTowardOlder,
        KeyCode::PageDown => KeyAction::ScrollTowardNewer,
        KeyCode::Left => KeyAction::Edit(EditorKey::Left),
        KeyCode::Right => KeyAction::Edit(EditorKey::Right),
        KeyCode::Up => KeyAction::Edit(EditorKey::Up),
        KeyCode::Down => KeyAction::Edit(EditorKey::Down),
        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::JumpToTop,
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::JumpToBottom,
        KeyCode::Home => KeyAction::Edit(EditorKey::Home),
        KeyCode::End => KeyAction::Edit(EditorKey::End),
        KeyCode::Backspace => KeyAction::Edit(EditorKey::Backspace),
        KeyCode::Delete => KeyAction::Edit(EditorKey::Delete),
        KeyCode::Char(character)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            KeyAction::Edit(EditorKey::Character(character))
        }
        _ => KeyAction::Ignore,
    }
}

fn is_newline(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            true
        }
        KeyCode::Char('j' | 'J') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
        KeyCode::Char('\n') => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn enter_submits_and_newline_keys_insert_a_newline() {
        assert_eq!(
            translate(press(KeyCode::Enter, KeyModifiers::NONE)),
            KeyAction::Submit
        );
        for (label, event) in [
            ("shift", press(KeyCode::Enter, KeyModifiers::SHIFT)),
            ("alt", press(KeyCode::Enter, KeyModifiers::ALT)),
            ("ctrl-j", press(KeyCode::Char('j'), KeyModifiers::CONTROL)),
        ] {
            assert_eq!(
                translate(event),
                KeyAction::Edit(EditorKey::Newline),
                "{label}"
            );
        }
    }

    #[test]
    fn control_chords_scroll_and_thinking_map_to_screen_actions() {
        assert_eq!(
            translate(press(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            KeyAction::ClearInputOrExit
        );
        assert_eq!(
            translate(press(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            KeyAction::ExitIfInputEmpty
        );
        assert_eq!(
            translate(press(KeyCode::PageUp, KeyModifiers::NONE)),
            KeyAction::ScrollTowardOlder
        );
        assert_eq!(
            translate(press(KeyCode::PageDown, KeyModifiers::NONE)),
            KeyAction::ScrollTowardNewer
        );
        assert_eq!(
            translate(press(KeyCode::Char('t'), KeyModifiers::CONTROL)),
            KeyAction::CycleThinkingLevel
        );
        assert_eq!(
            translate(press(KeyCode::Esc, KeyModifiers::NONE)),
            KeyAction::Cancel
        );
    }

    #[test]
    fn jump_shortcuts_preserve_plain_editing_keys() {
        for (code, modifiers, expected) in [
            (
                KeyCode::Char('k'),
                KeyModifiers::CONTROL,
                KeyAction::JumpToTop,
            ),
            (
                KeyCode::Char('K'),
                KeyModifiers::CONTROL,
                KeyAction::JumpToTop,
            ),
            (
                KeyCode::Char('l'),
                KeyModifiers::CONTROL,
                KeyAction::JumpToBottom,
            ),
            (
                KeyCode::Char('L'),
                KeyModifiers::CONTROL,
                KeyAction::JumpToBottom,
            ),
            (
                KeyCode::Char('k'),
                KeyModifiers::NONE,
                KeyAction::Edit(EditorKey::Character('k')),
            ),
            (
                KeyCode::Char('l'),
                KeyModifiers::NONE,
                KeyAction::Edit(EditorKey::Character('l')),
            ),
            (KeyCode::Home, KeyModifiers::CONTROL, KeyAction::JumpToTop),
            (KeyCode::End, KeyModifiers::CONTROL, KeyAction::JumpToBottom),
            (
                KeyCode::Home,
                KeyModifiers::NONE,
                KeyAction::Edit(EditorKey::Home),
            ),
            (
                KeyCode::End,
                KeyModifiers::NONE,
                KeyAction::Edit(EditorKey::End),
            ),
        ] {
            assert_eq!(translate(press(code, modifiers)), expected);
        }
    }

    #[test]
    fn selection_mode_keys_map_vi_and_arrow_keys_and_ignore_the_rest() {
        assert_eq!(
            translate(press(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            KeyAction::EnterSelectionMode
        );
        for (code, modifiers, expected) in [
            (
                KeyCode::Char('h'),
                KeyModifiers::NONE,
                SelectionKey::Move(CursorMove::Left),
            ),
            (
                KeyCode::Left,
                KeyModifiers::NONE,
                SelectionKey::Move(CursorMove::Left),
            ),
            (
                KeyCode::Char('j'),
                KeyModifiers::NONE,
                SelectionKey::Move(CursorMove::Down),
            ),
            (
                KeyCode::Char('k'),
                KeyModifiers::NONE,
                SelectionKey::Move(CursorMove::Up),
            ),
            (
                KeyCode::Char('l'),
                KeyModifiers::NONE,
                SelectionKey::Move(CursorMove::Right),
            ),
            (
                KeyCode::Char('0'),
                KeyModifiers::NONE,
                SelectionKey::Move(CursorMove::LineStart),
            ),
            (
                KeyCode::Char('$'),
                KeyModifiers::SHIFT,
                SelectionKey::Move(CursorMove::LineEnd),
            ),
            (
                KeyCode::Char('g'),
                KeyModifiers::NONE,
                SelectionKey::Move(CursorMove::Top),
            ),
            (
                KeyCode::Char('G'),
                KeyModifiers::SHIFT,
                SelectionKey::Move(CursorMove::Bottom),
            ),
            (
                KeyCode::PageUp,
                KeyModifiers::NONE,
                SelectionKey::Move(CursorMove::PageOlder),
            ),
            (
                KeyCode::Char('v'),
                KeyModifiers::NONE,
                SelectionKey::ToggleVisual,
            ),
            (KeyCode::Char('y'), KeyModifiers::NONE, SelectionKey::Yank),
            (
                KeyCode::Char('y'),
                KeyModifiers::CONTROL,
                SelectionKey::Yank,
            ),
            (KeyCode::Esc, KeyModifiers::NONE, SelectionKey::Exit),
            (
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
                SelectionKey::Exit,
            ),
            (KeyCode::Char('x'), KeyModifiers::NONE, SelectionKey::Ignore),
            (KeyCode::Enter, KeyModifiers::NONE, SelectionKey::Ignore),
            (
                KeyCode::Char('k'),
                KeyModifiers::CONTROL,
                SelectionKey::Ignore,
            ),
        ] {
            assert_eq!(
                translate_selection(press(code, modifiers)),
                expected,
                "{code:?}"
            );
        }
        let mut release = press(KeyCode::Char('j'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(translate_selection(release), SelectionKey::Ignore);
    }

    #[test]
    fn a_key_release_is_ignored() {
        let mut release = press(KeyCode::Enter, KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(translate(release), KeyAction::Ignore);
    }
}
