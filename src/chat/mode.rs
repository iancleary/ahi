//! Operator-owned workspace trust and a small, draft-preserving mode chooser.
use crate::error::{Error, Result};
use crate::tool_policy::ExecutionMode;
use ahi_widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui_core::layout::Rect;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
pub(super) enum Dialog {
    Trust,
    Choose,
    /// Confirms the built-in Full Access choice for this process only. It is
    /// reached from the picker alone: never from startup, Shift+Tab, or a
    /// policy, and confirming it records nothing on disk.
    FullAccess,
}

pub(super) struct Modes {
    pub selected: ExecutionMode,
    pub active: Option<ExecutionMode>,
    pub dialog: Option<Dialog>,
    pub has_policy: bool,
    pub trusted: bool,
    legacy_policy: bool,
    named: Vec<ExecutionMode>,
    query: String,
    cursor: usize,
}
impl Default for Modes {
    fn default() -> Self {
        Self {
            selected: ExecutionMode::Policy,
            active: None,
            dialog: None,
            has_policy: false,
            trusted: false,
            legacy_policy: false,
            named: Vec::new(),
            query: String::new(),
            cursor: 0,
        }
    }
}
impl Modes {
    pub fn startup(root: &Path, state: &Path, policy: Option<&Path>) -> Result<Self> {
        let installed = match policy {
            Some(path) => Some(crate::tool_policy::load_file(path, &crate::tools::BUILTIN)?),
            None => crate::tool_policy::load(root, &crate::tools::BUILTIN)?,
        };
        Ok(Self::from_installed(root, state, installed.as_ref()))
    }
    pub(super) fn from_installed(
        root: &Path,
        state: &Path,
        installed: Option<&crate::tool_policy::ActionPolicy>,
    ) -> Self {
        let has_policy = installed.is_some();
        let trusted = std::fs::read_to_string(trust_path(state, root))
            .is_ok_and(|text| text == root.to_string_lossy());
        Self {
            has_policy,
            trusted,
            legacy_policy: installed.as_ref().is_some_and(|p| p.default_mode.is_none()),
            named: installed
                .iter()
                .flat_map(|p| p.named.keys().cloned().map(ExecutionMode::Named))
                .collect(),
            selected: if has_policy {
                installed
                    .and_then(|p| p.default_mode.clone())
                    .map_or(ExecutionMode::Policy, workspace_mode)
            } else if trusted {
                ExecutionMode::ReadWrite
            } else {
                ExecutionMode::ReadOnly
            },
            dialog: None,
            ..Self::default()
        }
    }
    // None means the next key opens the trust prompt, not a mode switch.
    fn cycle_target(&self) -> Option<ExecutionMode> {
        use ExecutionMode::*;
        Some(match &self.selected {
            Named(_) => self
                .named
                .iter()
                .position(|m| m == &self.selected)
                .and_then(|i| self.named.get(i + 1))
                .cloned()
                .unwrap_or(ReadOnly),
            Policy => ReadOnly,
            ReadOnly if self.trusted => ReadWrite,
            ReadOnly => return None,
            ReadWrite if self.legacy_policy => Policy,
            ReadWrite if self.has_policy => self.named.first().cloned().unwrap_or(ReadOnly),
            ReadWrite => ReadOnly,
            // Leaving Full Access is one key; entering it never is.
            FullAccess => ReadOnly,
        })
    }
    pub fn cycle_hint(&self) -> String {
        self.cycle_target().map_or_else(
            || "Shift+Tab to review trust".into(),
            |mode| format!("Shift+Tab for {}", mode.label()),
        )
    }
    pub fn cycle(&mut self) {
        match self.cycle_target() {
            Some(target) => self.selected = target,
            None => self.dialog = Some(Dialog::Trust),
        }
    }
    pub fn color(&self) -> ratatui_core::style::Color {
        use ratatui_core::style::Color;
        match self.selected {
            ExecutionMode::Policy | ExecutionMode::Named(_) => Color::Cyan,
            ExecutionMode::ReadOnly => Color::Blue,
            ExecutionMode::ReadWrite => Color::Yellow,
            ExecutionMode::FullAccess => Color::Red,
        }
    }
    /// The picker's Full Access confirmation was accepted. Selected for the
    /// next turn of this process; nothing is remembered.
    pub fn confirm_full_access(&mut self) {
        if matches!(self.dialog, Some(Dialog::FullAccess)) {
            self.selected = ExecutionMode::FullAccess;
            self.dialog = None;
        }
    }
    pub fn status(&self) -> String {
        match &self.active {
            Some(active) if active != &self.selected => {
                format!("{} → {} next turn", active.label(), self.selected.label())
            }
            _ => self.selected.label().to_string(),
        }
    }
    fn choices(&self) -> Vec<ExecutionMode> {
        let mut modes = vec![
            ExecutionMode::ReadOnly,
            ExecutionMode::ReadWrite,
            ExecutionMode::FullAccess,
        ];
        if self.legacy_policy {
            modes.push(ExecutionMode::Policy);
        }
        modes.extend(self.named.iter().cloned());
        modes.retain(|m| {
            m.label()
                .to_ascii_lowercase()
                .contains(&self.query.to_ascii_lowercase())
        });
        modes
    }
    pub fn refresh_policy(&mut self, root: &Path, file: Option<&Path>) -> Result<()> {
        let installed = match file {
            Some(path) => Some(crate::tool_policy::load_file(path, &crate::tools::BUILTIN)?),
            None => crate::tool_policy::load(root, &crate::tools::BUILTIN)?,
        };
        if let Some(policy) = installed {
            self.legacy_policy = policy.default_mode.is_none();
            self.named = policy.named.into_keys().map(ExecutionMode::Named).collect();
            if !self.has_policy && self.selected != ExecutionMode::ReadOnly {
                self.selected = policy
                    .default_mode
                    .map_or(ExecutionMode::Policy, workspace_mode);
            }
            self.has_policy = true;
        } else if self.has_policy {
            return Err(Error::policy(
                "Workspace policy is missing; restore it or reopen chat to review trust.",
            ));
        }
        Ok(())
    }
    pub fn open_picker(&mut self) {
        self.query.clear();
        self.cursor = self
            .choices()
            .iter()
            .position(|m| m == &self.selected)
            .unwrap_or(0);
        self.dialog = Some(Dialog::Choose);
    }
    pub fn picker_key(&mut self, key: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode;
        let choices = self.choices();
        let len = choices.len();
        match key {
            KeyCode::Up if len > 0 => self.cursor = (self.cursor + len - 1) % len,
            KeyCode::Down if len > 0 => self.cursor = (self.cursor + 1) % len,
            KeyCode::Enter => {
                if let Some(mode) = choices.get(self.cursor) {
                    if *mode == ExecutionMode::ReadWrite && !self.trusted {
                        self.dialog = Some(Dialog::Trust);
                    } else if *mode == ExecutionMode::FullAccess {
                        // Always confirmed, even when already selected or
                        // the workspace is trusted: trust is not host access.
                        self.dialog = Some(Dialog::FullAccess);
                    } else {
                        self.selected = mode.clone();
                        self.dialog = None;
                    }
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.cursor = 0;
            }
            KeyCode::Char(c) => {
                self.query.push(c);
                self.cursor = 0;
            }
            _ => {}
        }
    }
    pub fn draw(&self, frame: &mut ratatui_core::terminal::Frame, root: &Path) {
        let Some(dialog) = self.dialog else {
            return;
        };
        let area = frame.area();
        let popup = Rect::new(
            area.x + 1,
            area.y + 1,
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        let (title, body) = match dialog {
            Dialog::Trust => (" Trust this workspace? ", format!("{}\n\nRead, edit, run commands, delegate to configured workers, and use repo/operator skills and bundled scripts. Commands run with your account's access. This is NOT a sandbox.\n\n[t] Trust and continue (remember this workspace)\n[r] Continue in Plan (no workspace edits or host commands)\n[Esc] Cancel\n\nTrust is stored outside the checkout; /mode revoke removes it.", root.display())),
            Dialog::FullAccess => (" Full Access to this host? ", format!("{}\n\nFile tools may read and change files outside this workspace on the host running the turn. File contents may be sent to the model provider. Commands have the same host access as in Trusted.\n\n[f] Use Full Access for this session (not remembered)\n[Esc] Cancel\n\nNothing is stored; Shift+Tab or /mode leaves it.", root.display())),
            Dialog::Choose => {
                use ahi_widgets::{List, ListItem, ListState};
                use ratatui_core::style::{Color, Style};
                let choices = self.choices();
                let height = popup.height.min(8).min(choices.len().max(1) as u16 + 2);
                let popup = Rect::new(popup.x, popup.bottom().saturating_sub(height), popup.width.min(80), height);
                let mut items: Vec<_> = choices.iter().map(|m| ListItem::new(format!("{}{}  {}", m.label(), if m == &self.selected { "  (selected)" } else { "" }, match m { ExecutionMode::ReadOnly => "Built-in · inspect Git and draft in scratch", ExecutionMode::ReadWrite => "Built-in · host commands; bypass workspace policy", ExecutionMode::FullAccess => "Built-in · Trusted plus files anywhere on this host; confirm each session", _ => "Workspace policy" }))).collect();
                if items.is_empty() {items.push(ListItem::new("No matching modes"));}
                let list = List::new(items).block(Block::default().borders(Borders::ALL).title(format!(" Modes · filter: {} · Enter select · Esc dismiss ",self.query))).highlight_symbol("› ").highlight_style(Style::default().bg(Color::Rgb(45,65,85)));
                frame.render_widget(Clear,popup);
                frame.render_stateful_widget(list,popup,&mut ListState::default().with_selected((!choices.is_empty()).then_some(self.cursor)));
                return;
            },
        };
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(body)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title(title)),
            popup,
        );
    }
}
fn workspace_mode(name: String) -> ExecutionMode {
    if name == "plan" {
        ExecutionMode::ReadOnly
    } else {
        ExecutionMode::Named(name)
    }
}
fn trust_path(state: &Path, root: &Path) -> PathBuf {
    state
        .join("workspace-trust")
        .join(crate::session::sha256_hex(
            root.to_string_lossy().as_bytes(),
        ))
}
pub(super) fn remember(state: &Path, root: &Path, trust: bool) -> Result<()> {
    let path = trust_path(state, root);
    if !trust {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::new(e.to_string())),
        };
    }
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| Error::new(e.to_string()))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    match options.open(&path) {
        Ok(mut file) => file
            .write_all(root.to_string_lossy().as_bytes())
            .map_err(|e| Error::new(e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::read_to_string(path).is_ok_and(|text| text == root.to_string_lossy()) {
                Ok(())
            } else {
                Err(Error::new(
                    "invalid workspace trust record; revoke it before trusting again",
                ))
            }
        }
        Err(e) => Err(Error::new(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trust_is_outside_checkout_and_policy_always_takes_precedence() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("workspace-trust");
        let root = dir.join("work");
        let state = dir.join("state");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let mut initial = Modes::startup(&root, &state, None).unwrap();
        assert_eq!(initial.selected, ExecutionMode::ReadOnly);
        assert!(initial.dialog.is_none());
        initial.cycle();
        assert!(matches!(initial.dialog, Some(Dialog::Trust)));
        remember(&state, &root, true).unwrap();
        let mut modes = Modes::startup(&root, &state, None).unwrap();
        assert_eq!(modes.selected, ExecutionMode::ReadWrite);
        assert!(modes.dialog.is_none());
        modes.cycle();
        assert_eq!(modes.selected, ExecutionMode::ReadOnly);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::write(root.join("ahi.tools.yaml"), "version: 1\nrules: []\n").unwrap();
        let mut modes = Modes::startup(&root, &state, None).unwrap();
        assert_eq!(modes.selected, ExecutionMode::Policy);
        modes.cycle();
        assert_eq!(modes.selected, ExecutionMode::ReadOnly);
        modes.cycle();
        assert_eq!(modes.selected, ExecutionMode::ReadWrite);
        assert!(modes.dialog.is_none());
        modes.cycle();
        assert_eq!(modes.selected, ExecutionMode::Policy);
        std::fs::write(root.join("ahi.tools.yaml"), "version: broken\n").unwrap();
        assert!(Modes::startup(&root, &state, None).is_err());
        remember(&state, &root, false).unwrap();
        std::fs::remove_file(root.join("ahi.tools.yaml")).unwrap();
        assert_eq!(
            Modes::startup(&root, &state, None).unwrap().selected,
            ExecutionMode::ReadOnly
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shortcut_names_the_next_available_mode_or_the_trust_prompt() {
        use ExecutionMode::*;
        let mut modes = Modes {
            selected: ReadOnly,
            ..Default::default()
        };
        assert_eq!(modes.cycle_hint(), "Shift+Tab to review trust");
        modes.cycle();
        assert!(matches!(modes.dialog, Some(Dialog::Trust)));
        modes.trusted = true;
        modes.dialog = None;
        assert_eq!(modes.cycle_hint(), "Shift+Tab for Trusted");
        modes.cycle();
        assert_eq!(modes.cycle_hint(), "Shift+Tab for Plan");
        modes.has_policy = true;
        modes.named = vec![Named("dev".into()), Named("review".into())];
        for (hint, target) in [
            ("Shift+Tab for dev", Named("dev".into())),
            ("Shift+Tab for review", Named("review".into())),
            ("Shift+Tab for Plan", ReadOnly),
        ] {
            assert_eq!(modes.cycle_hint(), hint);
            modes.cycle();
            assert_eq!(modes.selected, target);
        }
        modes.selected = FullAccess;
        assert_eq!(modes.cycle_hint(), "Shift+Tab for Plan");
    }

    #[test]
    fn full_access_is_picked_and_confirmed_never_cycled_defaulted_or_remembered() {
        use crossterm::event::KeyCode;
        let dir = crate::fixture_directory::create_unique_fixture_directory("full-access-mode");
        let root = dir.join("work");
        let state = dir.join("state");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        // Trust is remembered; Full Access still needs its own confirmation.
        remember(&state, &root, true).unwrap();
        let mut modes = Modes::startup(&root, &state, None).unwrap();
        assert_eq!(modes.selected, ExecutionMode::ReadWrite);
        // Shift+Tab never lands on Full Access, from any mode.
        for _ in 0..6 {
            modes.cycle();
            assert_ne!(modes.selected, ExecutionMode::FullAccess);
        }
        // The picker lists it after the two other built-ins, and Enter opens
        // the confirmation instead of selecting.
        modes.open_picker();
        assert_eq!(
            modes.choices(),
            [
                ExecutionMode::ReadOnly,
                ExecutionMode::ReadWrite,
                ExecutionMode::FullAccess
            ]
        );
        for c in "full".chars() {
            modes.picker_key(KeyCode::Char(c));
        }
        assert_eq!(modes.choices(), [ExecutionMode::FullAccess]);
        modes.picker_key(KeyCode::Enter);
        assert!(matches!(modes.dialog, Some(Dialog::FullAccess)));
        assert_ne!(modes.selected, ExecutionMode::FullAccess);
        // Cancelling (the screen clears the dialog on Esc) leaves it unselected,
        // and confirming without the dialog open is a no-op.
        modes.dialog = None;
        modes.confirm_full_access();
        assert_eq!(modes.selected, ExecutionMode::ReadWrite);
        // The cursor opens on the selected mode (Trusted); one step down.
        modes.open_picker();
        modes.picker_key(KeyCode::Down);
        modes.picker_key(KeyCode::Enter);
        assert!(matches!(modes.dialog, Some(Dialog::FullAccess)));
        modes.confirm_full_access();
        assert_eq!(modes.selected, ExecutionMode::FullAccess);
        assert!(modes.dialog.is_none());
        assert_eq!(modes.status(), "Full Access");
        assert_eq!(modes.color(), ratatui_core::style::Color::Red);
        // One Shift+Tab leaves it, and a fresh start never selects it: only
        // workspace trust was stored.
        modes.cycle();
        assert_eq!(modes.selected, ExecutionMode::ReadOnly);
        assert_eq!(
            Modes::startup(&root, &state, None).unwrap().selected,
            ExecutionMode::ReadWrite
        );
        // A workspace policy cannot name the built-in as a mode.
        std::fs::write(
            root.join("ahi.modes.yaml"),
            "version: 2\ndefault_mode: full-access\nmodes:\n  full-access:\n    allow:\n      tools: [read_file]\n",
        )
        .unwrap();
        assert!(Modes::startup(&root, &state, None).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
