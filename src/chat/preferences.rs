//! Operator preferences for the chat screen, stored one per line as
//! `key=value` in `<state>/preferences`: the clock (`local` or `utc`), the
//! copy form for rendered markdown (`rendered` or `raw`), the editor that
//! opens clicked file references, and whether markdown renders at all.
//! Unknown keys are kept so a newer ahi's settings survive an older one.

use std::fs;

use crate::directories::Directories;
use crate::error::{Error, Result};

const FILE: &str = "preferences";
const MAX_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Clock {
    Local,
    Utc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyForm {
    /// Text as drawn: markdown markers removed.
    Rendered,
    /// The source text the model wrote.
    Raw,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Preferences {
    pub(crate) clock: Clock,
    pub(crate) copy: CopyForm,
    /// A built-in editor name or an argv template with `{path}`, `{line}`,
    /// `{column}`. `None` probes for `code` on `PATH` when needed.
    pub(crate) editor: Option<String>,
    pub(crate) markdown: bool,
    /// Lines that were not understood, written back unchanged.
    other: Vec<String>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            clock: Clock::Local,
            copy: CopyForm::Rendered,
            editor: None,
            markdown: true,
            other: Vec::new(),
        }
    }
}

impl Preferences {
    fn parse(text: &str) -> Self {
        let mut prefs = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                prefs.other.push(line.to_string());
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match (key, value) {
                ("clock", "local") => prefs.clock = Clock::Local,
                ("clock", "utc") => prefs.clock = Clock::Utc,
                ("copy", "rendered") => prefs.copy = CopyForm::Rendered,
                ("copy", "raw") => prefs.copy = CopyForm::Raw,
                ("markdown", "on" | "true") => prefs.markdown = true,
                ("markdown", "off" | "false") => prefs.markdown = false,
                ("editor", "") => prefs.editor = None,
                ("editor", value) if value.chars().all(|c| !c.is_control()) => {
                    prefs.editor = Some(value.to_string())
                }
                _ => prefs.other.push(line.to_string()),
            }
        }
        prefs
    }

    fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "clock={}\n",
            match self.clock {
                Clock::Local => "local",
                Clock::Utc => "utc",
            }
        ));
        out.push_str(&format!(
            "copy={}\n",
            match self.copy {
                CopyForm::Rendered => "rendered",
                CopyForm::Raw => "raw",
            }
        ));
        out.push_str(&format!(
            "markdown={}\n",
            if self.markdown { "on" } else { "off" }
        ));
        if let Some(editor) = &self.editor {
            out.push_str(&format!("editor={editor}\n"));
        }
        for line in &self.other {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

/// Read the stored preferences. A missing or unreadable file is the default.
pub(crate) fn read(state_dir: &Directories) -> Preferences {
    let path = state_dir.state.join(FILE);
    let Ok(metadata) = path.metadata() else {
        return Preferences::default();
    };
    if metadata.len() > MAX_BYTES {
        return Preferences::default();
    }
    fs::read_to_string(&path)
        .map(|text| Preferences::parse(&text))
        .unwrap_or_default()
}

pub(crate) fn write(state_dir: &Directories, prefs: &Preferences) -> Result<()> {
    let path = state_dir.state.join(FILE);
    crate::directories::create_private_dir(&state_dir.state).map_err(|err| {
        Error::new(format!(
            "failed to create {}: {err}",
            state_dir.state.display()
        ))
    })?;
    crate::directories::write_private(&path, prefs.render().as_bytes())
        .map_err(|err| Error::new(format!("failed to write {}: {err}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_round_trip_and_keep_unknown_lines() {
        let text =
            "clock=utc\n# comment\ncopy=raw\nmarkdown=off\neditor=zed\nfuture=thing\nnoise\n";
        let prefs = Preferences::parse(text);
        assert_eq!(prefs.clock, Clock::Utc);
        assert_eq!(prefs.copy, CopyForm::Raw);
        assert!(!prefs.markdown);
        assert_eq!(prefs.editor.as_deref(), Some("zed"));
        assert_eq!(prefs.other, vec!["future=thing", "noise"]);
        let again = Preferences::parse(&prefs.render());
        assert_eq!(again, prefs);
        assert_eq!(Preferences::parse(""), Preferences::default());
        assert_eq!(Preferences::parse("clock=mars").clock, Clock::Local);
    }

    #[test]
    fn preferences_are_persisted_in_the_state_directory() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-preferences");
        let dirs = Directories::single_root(&state);
        assert_eq!(read(&dirs), Preferences::default());
        let prefs = Preferences {
            clock: Clock::Utc,
            editor: Some("code -g {path}:{line}".into()),
            ..Preferences::default()
        };
        write(&dirs, &prefs).unwrap();
        assert_eq!(read(&dirs), prefs);
        let _ = fs::remove_dir_all(state);
    }
}
