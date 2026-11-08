//! Floating transcript jump controls and their mouse hit targets.

use ahi_widgets::{Clear, Paragraph};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui_core::layout::{Position, Rect};
use ratatui_core::style::{Color, Modifier, Style};

use super::key_binding::KeyAction;

#[derive(Default)]
pub(super) struct ScrollNavigation {
    top: Option<Rect>,
    bottom: Option<Rect>,
    pointer: Option<Position>,
}

impl ScrollNavigation {
    pub(super) fn mouse(&mut self, event: MouseEvent) -> Option<KeyAction> {
        let position = Position::new(event.column, event.row);
        self.pointer = Some(position);
        if event.kind != MouseEventKind::Down(MouseButton::Left) {
            return None;
        }
        if self.top.is_some_and(|area| area.contains(position)) {
            Some(KeyAction::JumpToTop)
        } else if self.bottom.is_some_and(|area| area.contains(position)) {
            Some(KeyAction::JumpToBottom)
        } else {
            None
        }
    }

    pub(super) fn draw(
        &mut self,
        frame: &mut ratatui_core::terminal::Frame,
        area: Rect,
        can_go_up: bool,
        can_go_down: bool,
    ) {
        self.top = None;
        self.bottom = None;
        if area.height == 0 || area.width < 8 {
            return;
        }
        for (show, top, label, compact) in [
            (
                can_go_up && (area.height > 1 || !can_go_down),
                true,
                " ↑ Top (Ctrl+K) ",
                " ↑ Top ",
            ),
            (can_go_down, false, " ↓ Bottom (Ctrl+L) ", " ↓ End "),
        ] {
            if !show {
                continue;
            }
            let text = if area.width as usize >= label.chars().count() {
                label
            } else {
                compact
            };
            let width = text.chars().count() as u16;
            let button = Rect::new(
                area.right() - width,
                if top { area.y } else { area.bottom() - 1 },
                width,
                1,
            );
            let hovered = self
                .pointer
                .is_some_and(|position| button.contains(position));
            let style = if hovered {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Cyan).bg(Color::Rgb(30, 40, 50))
            };
            frame.render_widget(Clear, button);
            frame.render_widget(Paragraph::new(text).style(style), button);
            if top {
                self.top = Some(button);
            } else {
                self.bottom = Some(button);
            }
        }
    }
}
