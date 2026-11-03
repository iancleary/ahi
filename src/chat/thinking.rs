//! Thinking level and the `<state>/thinking` file.

use std::fs;
use std::path::Path;

use crate::error::{Error, Result};

/// Reasoning display level stored in `<state>/thinking`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
}

impl ThinkingLevel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "off" => Some(Self::Off),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::Off => Self::Minimal,
            Self::Minimal => Self::Low,
            Self::Low => Self::Medium,
            Self::Medium => Self::High,
            Self::High => Self::Off,
        }
    }
}

/// Read the stored level. A missing file is `medium` with no notice.
pub(crate) fn read(state_dir: &Path) -> (ThinkingLevel, Option<String>) {
    let path = state_dir.join("thinking");
    if !path.exists() {
        return (ThinkingLevel::Medium, None);
    }
    match fs::read_to_string(&path) {
        Ok(text) => match ThinkingLevel::parse(text.trim()) {
            Some(level) => (level, None),
            None => (
                ThinkingLevel::Medium,
                Some("unknown thinking level; using medium".to_string()),
            ),
        },
        Err(_) => (
            ThinkingLevel::Medium,
            Some("thinking level unreadable; using medium".to_string()),
        ),
    }
}

/// Write the level as a single line. An invalid file is left unchanged.
pub(crate) fn write(state_dir: &Path, level: ThinkingLevel) -> Result<()> {
    let path = state_dir.join("thinking");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| Error::new(format!("failed to create {}: {err}", parent.display())))?;
    }
    let level = level.as_str();
    fs::write(&path, format!("{level}\n"))
        .map_err(|err| Error::new(format!("failed to write {}: {err}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_level_is_persisted_and_an_invalid_file_falls_back() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-thinking");
        fs::write(state.join("thinking"), "high\n").unwrap();
        let screen = super::super::ChatScreen::load(&state, "openai/gpt-4.1");
        assert_eq!(screen.thinking(), ThinkingLevel::High);

        fs::write(state.join("thinking"), "loud\n").unwrap();
        let screen = super::super::ChatScreen::load(&state, "openai/gpt-4.1");
        assert_eq!(screen.thinking(), ThinkingLevel::Medium);
        assert!(screen
            .messages()
            .iter()
            .any(|message| message.text == "unknown thinking level; using medium"));
        assert_eq!(
            fs::read_to_string(state.join("thinking")).unwrap().trim(),
            "loud"
        );
        let _ = fs::remove_dir_all(state);
    }
}
