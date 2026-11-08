//! Parsing of chat slash commands.
//!
//! The screen applies the result. An unknown command is not sent to a backend.

use super::preferences::{Clock, CopyForm};
use super::thinking::ThinkingLevel;

/// One parsed slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedCommand {
    ReopenModelPicker,
    OpenCatalog,
    Login,
    Gateway,
    Exit,
    Clear,
    ShowMouse,
    SetMouse(bool),
    Select,
    ShowThinking,
    SetThinking(ThinkingLevel),
    UnknownThinkingLevel {
        level: String,
    },
    /// `/copy`, `/copy rendered`, `/copy raw`.
    Copy(Option<CopyForm>),
    /// `/clock`, `/clock local`, `/clock utc`.
    Clock(Option<Clock>),
    /// `/editor` shows, `/editor <name or argv template>` sets.
    Editor(Option<String>),
    /// `/plain` and `/markdown`.
    Markdown(bool),
    Unknown {
        name: String,
    },
}

pub(crate) fn parse(text: &str) -> ParsedCommand {
    use crate::tui::commands::{common_action, CommonAction};
    if let Some(action) = common_action(text) {
        return match action {
            CommonAction::Models => ParsedCommand::ReopenModelPicker,
            CommonAction::Catalog => ParsedCommand::OpenCatalog,
            CommonAction::Login => ParsedCommand::Login,
            CommonAction::Gateway => ParsedCommand::Gateway,
            CommonAction::Quit => ParsedCommand::Exit,
        };
    }
    let mut parts = text.split_whitespace();
    let name = parts.next().unwrap_or(text);
    let rest: Vec<&str> = parts.collect();
    match (name, rest.as_slice()) {
        ("/clear", []) => ParsedCommand::Clear,
        ("/mouse", []) => ParsedCommand::ShowMouse,
        ("/mouse", ["on"]) => ParsedCommand::SetMouse(true),
        ("/mouse", ["off"]) => ParsedCommand::SetMouse(false),
        ("/select", []) => ParsedCommand::Select,
        ("/copy", []) => ParsedCommand::Copy(None),
        ("/copy", ["rendered"]) => ParsedCommand::Copy(Some(CopyForm::Rendered)),
        ("/copy", ["raw"]) => ParsedCommand::Copy(Some(CopyForm::Raw)),
        ("/clock", []) => ParsedCommand::Clock(None),
        ("/clock", ["local"]) => ParsedCommand::Clock(Some(Clock::Local)),
        ("/clock", ["utc"]) => ParsedCommand::Clock(Some(Clock::Utc)),
        ("/editor", []) => ParsedCommand::Editor(None),
        ("/editor", _) => ParsedCommand::Editor(Some(
            text.trim()
                .strip_prefix("/editor")
                .unwrap_or("")
                .trim()
                .to_string(),
        )),
        ("/plain", []) => ParsedCommand::Markdown(false),
        ("/markdown", []) => ParsedCommand::Markdown(true),
        ("/thinking", []) => ParsedCommand::ShowThinking,
        ("/thinking", [level]) => match ThinkingLevel::parse(level) {
            Some(level) => ParsedCommand::SetThinking(level),
            None => ParsedCommand::UnknownThinkingLevel {
                level: (*level).to_string(),
            },
        },
        _ => ParsedCommand::Unknown {
            name: name.to_string(),
        },
    }
}
