//! Experimental input-anchored settings picker. `$` is reserved for skills.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

pub(super) struct Setting {
    pub(super) command: &'static str,
    description: &'static str,
}

const SETTINGS: &[Setting] = &[
    Setting {
        command: "/models",
        description: "Choose provider and model",
    },
    Setting {
        command: "/catalog",
        description: "Browse full provider catalog",
    },
    Setting {
        command: "/quit",
        description: "Close ahi",
    },
    Setting {
        command: "/thinking off",
        description: "Thinking: off",
    },
    Setting {
        command: "/thinking minimal",
        description: "Thinking: minimal",
    },
    Setting {
        command: "/thinking low",
        description: "Thinking: low",
    },
    Setting {
        command: "/thinking medium",
        description: "Thinking: medium",
    },
    Setting {
        command: "/thinking high",
        description: "Thinking: high",
    },
    Setting {
        command: "/mouse on",
        description: "Mouse controls: on",
    },
    Setting {
        command: "/mouse off",
        description: "Native text selection",
    },
];

pub(super) fn matching(input: &str) -> Vec<&'static Setting> {
    if !input.starts_with('/') || input.contains('\n') {
        return Vec::new();
    }
    SETTINGS
        .iter()
        .filter(|setting| setting.command.starts_with(input))
        .collect()
}

#[derive(Default)]
pub(super) struct SettingsMenu {
    pub(super) selected: usize,
    pub(super) dismissed: bool,
    pub(super) navigated: bool,
}

impl SettingsMenu {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(super) fn draw(&self, frame: &mut ratatui::Frame, area: Rect, input: &str, status: &str) {
        let settings = matching(input);
        if self.dismissed || settings.is_empty() || area.width < 4 || area.height < 3 {
            return;
        }
        let height = (settings.len() as u16 + 2).min(area.height).min(8);
        let popup = Rect::new(area.x, area.bottom() - height, area.width.min(72), height);
        let items = settings
            .iter()
            .map(|setting| {
                ListItem::new(Line::from(format!(
                    "{:<20} {}",
                    setting.command, setting.description
                )))
            })
            .collect::<Vec<_>>();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Settings · ↑/↓ Enter Esc ")
                    .title_bottom(Line::raw(status.to_string())),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(45, 65, 85))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("› ");
        let mut state =
            ListState::default().with_selected(Some(self.selected.min(settings.len() - 1)));
        frame.render_widget(Clear, popup);
        frame.render_stateful_widget(list, popup, &mut state);
    }
}
