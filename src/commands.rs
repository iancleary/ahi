use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandContext {
    Chat,
    Models,
    Catalog,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommonAction {
    Gateway,
    Models,
    Catalog,
    Login,
    Quit,
}

pub(crate) struct CommandDefinition {
    pub(crate) command: &'static str,
    label: &'static str,
    description: &'static str,
    aliases: &'static [&'static str],
    action: Option<CommonAction>,
    contexts: &'static [CommandContext],
}

const COMMON_CONTEXTS: &[CommandContext] = &[
    CommandContext::Chat,
    CommandContext::Models,
    CommandContext::Catalog,
];
const CHAT_CONTEXTS: &[CommandContext] = &[CommandContext::Chat];

const COMMANDS: &[CommandDefinition] = &[
    CommandDefinition {
        command: "/gateway",
        label: "Add gateway",
        description: "Discover gateway by URL",
        aliases: &[],
        action: Some(CommonAction::Gateway),
        contexts: COMMON_CONTEXTS,
    },
    CommandDefinition {
        command: "/models",
        label: "Choose model",
        description: "Use a saved model",
        aliases: &["/model"],
        action: Some(CommonAction::Models),
        contexts: COMMON_CONTEXTS,
    },
    CommandDefinition {
        command: "/catalog",
        label: "Browse catalog",
        description: "Add or remove available models",
        aliases: &[],
        action: Some(CommonAction::Catalog),
        contexts: COMMON_CONTEXTS,
    },
    CommandDefinition {
        command: "/login",
        label: "OpenAI sign-in",
        description: "Sign in with ChatGPT (OpenAI)",
        aliases: &[],
        action: Some(CommonAction::Login),
        contexts: COMMON_CONTEXTS,
    },
    CommandDefinition {
        command: "/quit",
        label: "Quit ahi",
        description: "Close the application",
        aliases: &["/exit"],
        action: Some(CommonAction::Quit),
        contexts: COMMON_CONTEXTS,
    },
    CommandDefinition {
        command: "/thinking off",
        label: "Thinking: off",
        description: "Disable thinking",
        aliases: &[],
        action: None,
        contexts: CHAT_CONTEXTS,
    },
    CommandDefinition {
        command: "/thinking minimal",
        label: "Thinking: minimal",
        description: "Minimal reasoning",
        aliases: &[],
        action: None,
        contexts: CHAT_CONTEXTS,
    },
    CommandDefinition {
        command: "/thinking low",
        label: "Thinking: low",
        description: "Low reasoning",
        aliases: &[],
        action: None,
        contexts: CHAT_CONTEXTS,
    },
    CommandDefinition {
        command: "/thinking medium",
        label: "Thinking: medium",
        description: "Medium reasoning",
        aliases: &[],
        action: None,
        contexts: CHAT_CONTEXTS,
    },
    CommandDefinition {
        command: "/thinking high",
        label: "Thinking: high",
        description: "High reasoning",
        aliases: &[],
        action: None,
        contexts: CHAT_CONTEXTS,
    },
    CommandDefinition {
        command: "/mouse on",
        label: "Mouse controls: on",
        description: "Select and copy in ahi",
        aliases: &[],
        action: None,
        contexts: CHAT_CONTEXTS,
    },
    CommandDefinition {
        command: "/mouse off",
        label: "Native text selection",
        description: "Use terminal selection",
        aliases: &[],
        action: None,
        contexts: CHAT_CONTEXTS,
    },
    CommandDefinition {
        command: "/clear",
        label: "Clear conversation",
        description: "Remove the current transcript",
        aliases: &[],
        action: None,
        contexts: CHAT_CONTEXTS,
    },
];

pub(crate) fn common_action(input: &str) -> Option<CommonAction> {
    COMMANDS
        .iter()
        .find(|entry| entry.command == input.trim() || entry.aliases.contains(&input.trim()))
        .and_then(|entry| entry.action)
}

#[derive(Default)]
pub(crate) struct CommandMenu {
    pub(crate) selected: usize,
    pub(crate) dismissed: bool,
    pub(crate) navigated: bool,
}

impl CommandMenu {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn matching(
        input: &str,
        context: CommandContext,
    ) -> Vec<&'static CommandDefinition> {
        if !input.starts_with('/') || input.contains('\n') {
            return Vec::new();
        }
        let query = input.trim_start_matches('/').to_lowercase();
        let applicable = |entry: &&CommandDefinition| entry.contexts.contains(&context);
        let spelling_matches = |entry: &&CommandDefinition| {
            entry.command.trim_start_matches('/').starts_with(&query)
                || entry
                    .aliases
                    .iter()
                    .any(|alias| alias.trim_start_matches('/').starts_with(&query))
        };
        let matches: Vec<_> = COMMANDS
            .iter()
            .filter(applicable)
            .filter(spelling_matches)
            .collect();
        if !matches.is_empty() {
            return matches;
        }
        COMMANDS
            .iter()
            .filter(applicable)
            .filter(|entry| {
                entry.label.to_lowercase().starts_with(&query)
                    || entry.description.to_lowercase().starts_with(&query)
            })
            .collect()
    }

    pub(crate) fn navigate(&mut self, input: &str, context: CommandContext, delta: isize) -> bool {
        let count = Self::matching(input, context).len();
        if self.dismissed || count == 0 {
            return false;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(count as isize) as usize;
        self.navigated = true;
        true
    }

    pub(crate) fn chosen(&self, input: &str, context: CommandContext) -> Option<&'static str> {
        if self.dismissed {
            return None;
        }
        if !self.navigated && common_action(input).is_some() {
            return None;
        }
        let matches = Self::matching(input, context);
        matches
            .get(self.selected.min(matches.len().saturating_sub(1)))
            .map(|entry| entry.command)
    }

    pub(crate) fn draw(
        &self,
        frame: &mut ratatui::Frame,
        area: Rect,
        input: &str,
        context: CommandContext,
        status: &str,
    ) {
        let commands = Self::matching(input, context);
        if self.dismissed || commands.is_empty() || area.width < 4 || area.height < 3 {
            return;
        }
        let height = (commands.len() as u16 + 2).min(area.height).min(8);
        let popup = Rect::new(area.x, area.bottom() - height, area.width.min(80), height);
        let items = commands
            .iter()
            .map(|entry| {
                ListItem::new(Line::from(format!("{:<19} {}", entry.command, entry.label)))
            })
            .collect::<Vec<_>>();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Commands · ↑/↓ select · Enter apply · Esc dismiss ")
                    .title_bottom(Line::raw(status.to_string())),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(45, 65, 85))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("› ");
        let mut state =
            ListState::default().with_selected(Some(self.selected.min(commands.len() - 1)));
        frame.render_widget(Clear, popup);
        frame.render_stateful_widget(list, popup, &mut state);
    }
}

pub(crate) enum PickerOutcome {
    Selected(String),
    Back,
    Exit,
}
