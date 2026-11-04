//! Curated-model picker, catalog curation, editor handoff, and chat entry.
//!
//! Enter saves a curated selection and opens the local echo-backed chat.
//! Non-terminal invocation prints the local registry without opening chat.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::chat;
use crate::error::{Error, Result};
use crate::harness::{self, Registry};

pub struct RunOutput {
    pub text: String,
    pub refresh_failed: bool,
}

pub fn run(state: &Path, refresh: bool) -> Result<RunOutput> {
    let mut registry = harness::load_registry(state)?;
    if !io::stdout().is_terminal() {
        let errors = if refresh {
            harness::refresh_all(state)?
        } else {
            BTreeMap::new()
        };
        if refresh {
            reload(&mut registry, state, &errors)?;
        }
        return Ok(RunOutput {
            text: harness::render_text(&registry),
            refresh_failed: !errors.is_empty(),
        });
    }
    let mut picker = Picker::new(registry);
    // Cancelling startup exits, even if an old selection file exists. Only
    // Enter starts chat; cancelling /model instead keeps the active selection.
    if let Some(selection) = pick(state, &mut picker, refresh)? {
        chat::interact(state, &selection, || {
            // Reopen the same curated picker, with a fresh local snapshot.
            // A malformed external edit retains the last valid registry.
            if let Err(err) = reload(&mut picker.registry, state, &picker.refresh_errors) {
                picker.status = err.to_string();
            }
            picker.reset_view();
            pick(state, &mut picker, false)
        })?;
    }
    Ok(RunOutput {
        text: picker.print_on_exit.join("\n"),
        refresh_failed: false,
    })
}

fn pick(state: &Path, picker: &mut Picker, refresh: bool) -> Result<Option<String>> {
    let mut terminal = ratatui::try_init().map_err(|err| Error::new(format!("terminal: {err}")))?;
    let result = pick_loop(&mut terminal, state, picker, refresh);
    ratatui::restore();
    result
}

#[derive(Clone, Copy)]
enum Screen {
    Picker,
    Catalog { provider: usize },
}

#[derive(Clone, Copy)]
enum Row {
    Header(usize),
    Empty(usize),
    Model { provider: usize, model: usize },
}

impl Row {
    fn provider(self) -> usize {
        match self {
            Self::Header(provider) | Self::Empty(provider) | Self::Model { provider, .. } => {
                provider
            }
        }
    }
}

struct Picker {
    registry: Registry,
    screen: Screen,
    cursor: ListState,
    status: String,
    refresh_errors: BTreeMap<String, String>,
    print_on_exit: Vec<String>,
}

impl Picker {
    fn new(registry: Registry) -> Self {
        let mut cursor = ListState::default();
        cursor.select(Some(0));
        Self {
            registry,
            screen: Screen::Picker,
            cursor,
            status:
                "Enter saves the highlighted model and opens local echo chat (no model API calls)"
                    .to_string(),
            refresh_errors: BTreeMap::new(),
            print_on_exit: Vec::new(),
        }
    }

    fn rows(&self) -> Vec<Row> {
        match self.screen {
            Screen::Picker => self
                .registry
                .providers
                .iter()
                .enumerate()
                .flat_map(|(provider, entry)| {
                    let mut rows = vec![Row::Header(provider)];
                    if entry.models.is_empty() {
                        rows.push(Row::Empty(provider));
                    } else {
                        rows.extend(
                            (0..entry.models.len()).map(|model| Row::Model { provider, model }),
                        );
                    }
                    rows
                })
                .collect(),
            Screen::Catalog { provider } => {
                let count = catalog_ids(&self.registry.providers[provider]).len();
                if count == 0 {
                    vec![Row::Empty(provider)]
                } else {
                    (0..count)
                        .map(|model| Row::Model { provider, model })
                        .collect()
                }
            }
        }
    }

    fn selected(&self) -> Row {
        let rows = self.rows();
        rows[self.cursor.selected().unwrap_or(0).min(rows.len() - 1)]
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.rows().len();
        let next = (self.cursor.selected().unwrap_or(0) as isize + delta).rem_euclid(len as isize)
            as usize;
        self.cursor.select(Some(next));
    }

    fn navigate(&mut self, state: &Path, key: KeyEvent) -> bool {
        match key.code {
            // Legacy terminals send uppercase letters; enhanced keyboard
            // protocols can send lowercase letters with a Shift modifier.
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J')
                if matches!(self.screen, Screen::Catalog { .. })
                    && (key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.code == KeyCode::Char('J')) =>
            {
                self.reorder(state, 1)
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K')
                if matches!(self.screen, Screen::Catalog { .. })
                    && (key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.code == KeyCode::Char('K')) =>
            {
                self.reorder(state, -1)
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            _ => return false,
        }
        true
    }

    fn reset_view(&mut self) {
        self.screen = Screen::Picker;
        self.cursor = ListState::default();
        self.cursor.select(Some(0));
    }

    fn save_curated(&mut self, state: &Path, provider: usize, ids: &[String], focus: &str) -> bool {
        match harness::save_curated_models(
            state,
            &self.registry,
            &self.registry.providers[provider].id,
            ids,
        ) {
            Ok(mut next) => {
                apply_refresh_errors(&mut next, &self.refresh_errors);
                self.registry = next;
                let ids = catalog_ids(&self.registry.providers[provider]);
                let position = ids.iter().position(|id| *id == focus).unwrap_or_else(|| {
                    self.cursor
                        .selected()
                        .unwrap_or(0)
                        .min(ids.len().saturating_sub(1))
                });
                self.cursor.select(Some(position));
                self.status =
                    "Saved models.json (pretty JSON; settings and key order preserved)".to_string();
                true
            }
            Err(err) => {
                self.status = format!("Save failed: {err}; loaded selection unchanged");
                false
            }
        }
    }

    fn toggle(&mut self, state: &Path) {
        let Row::Model { provider, model } = self.selected() else {
            return;
        };
        let entry = &self.registry.providers[provider];
        let id = catalog_ids(entry)[model].to_string();
        let mut ids: Vec<String> = entry.models.iter().map(|model| model.id.clone()).collect();
        if ids.contains(&id) {
            ids.retain(|selected| *selected != id);
        } else {
            ids.push(id.clone());
        }
        self.save_curated(state, provider, &ids, &id);
    }

    fn reorder(&mut self, state: &Path, delta: isize) {
        let Row::Model { provider, model } = self.selected() else {
            return;
        };
        let entry = &self.registry.providers[provider];
        if model >= entry.models.len() {
            self.status = "Check a model with Space before reordering it".to_string();
            return;
        }
        let target = model as isize + delta;
        if target < 0 || target >= entry.models.len() as isize {
            return;
        }
        let id = entry.models[model].id.clone();
        let mut ids: Vec<String> = entry.models.iter().map(|model| model.id.clone()).collect();
        ids.swap(model, target as usize);
        if self.save_curated(state, provider, &ids, &id) {
            self.cursor.select(Some(target as usize));
        }
    }

    fn refresh(&mut self, terminal: &mut ratatui::DefaultTerminal, state: &Path) -> Result<()> {
        self.status = "Refreshing catalogs (up to 15 seconds per provider)…".to_string();
        self.draw(terminal)?;
        match harness::refresh_all(state) {
            Ok(errors) => {
                self.refresh_errors = errors;
                match reload(&mut self.registry, state, &self.refresh_errors) {
                    Ok(()) => {
                        self.status = if self.refresh_errors.is_empty() {
                            "Catalogs refreshed; curated selection unchanged".to_string()
                        } else {
                            format!(
                                "Refresh finished with {} error(s); previous catalogs retained",
                                self.refresh_errors.len()
                            )
                        }
                    }
                    Err(err) => self.status = err.to_string(),
                }
            }
            Err(err) => self.status = err.to_string(),
        }
        self.reset_view();
        Ok(())
    }

    fn draw(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        terminal
            .draw(|frame| self.draw_frame(frame))
            .map_err(|err| Error::new(format!("terminal draw: {err}")))?;
        Ok(())
    }

    fn draw_frame(&mut self, frame: &mut ratatui::Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(4),
                Constraint::Length(if matches!(self.screen, Screen::Catalog { .. }) {
                    5
                } else {
                    3
                }),
            ])
            .split(frame.area());
        let rows = self.rows();
        let catalog_models = match self.screen {
            Screen::Picker => Vec::new(),
            Screen::Catalog { provider } => catalog_ids(&self.registry.providers[provider]),
        };
        let items: Vec<ListItem> = rows
            .iter()
            .map(|row| {
                let provider = &self.registry.providers[row.provider()];
                let label = match row {
                    Row::Header(_) => format!(
                        "{}  auth {}  catalog {}{}",
                        provider.id,
                        provider.auth,
                        provider.catalog_age(),
                        provider
                            .catalog_error
                            .as_ref()
                            .map(|err| format!("  error: {err}"))
                            .unwrap_or_default()
                    ),
                    Row::Empty(_) => match self.screen {
                        Screen::Picker => provider.empty_reason.clone().unwrap_or_default(),
                        Screen::Catalog { .. } if provider.catalog.is_none() => {
                            "catalog unavailable: press r to refresh".to_string()
                        }
                        Screen::Catalog { .. } => "catalog contains no models".to_string(),
                    },
                    Row::Model { model, .. } => match self.screen {
                        Screen::Picker => {
                            let choice = &provider.models[*model];
                            format!("  {}  [{}]", choice.id, choice.status)
                        }
                        Screen::Catalog { .. } => {
                            let id = catalog_models[*model];
                            let choice = provider.models.iter().find(|choice| choice.id == id);
                            format!(
                                "  [{}] {id}{}",
                                if choice.is_some() { "x" } else { " " },
                                choice
                                    .filter(|choice| choice.status
                                        != harness::CatalogMembership::Listed)
                                    .map(|choice| format!("  [{}]", choice.status))
                                    .unwrap_or_default()
                            )
                        }
                    },
                };
                ListItem::new(label)
            })
            .collect();
        let title = match self.screen {
            Screen::Picker => "models.json — curated models (Enter opens chat)",
            Screen::Catalog { .. } => "catalog — curate models.json (Space toggle; Esc back)",
        };
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::default().borders(Borders::ALL).title(title))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
            chunks[0],
            &mut self.cursor,
        );
        let provider = &self.registry.providers[self.selected().provider()];
        let detail = format!(
            "{}  auth {}  catalog {}{}",
            provider.base_url,
            provider.auth,
            provider.catalog_age(),
            provider
                .catalog_error
                .as_ref()
                .map(|error| format!("\nLast refresh/catalog error: {error}"))
                .unwrap_or_default()
        );
        frame.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }), chunks[1]);
        let help = match self.screen {
            Screen::Picker => "r refresh all  c catalog  e edit models.json  Up/Down or j/k move  Enter save & chat  q quit",
            Screen::Catalog { .. } => "Up/Down or j/k: move highlight\nShift+Up/Down or Shift+j/k: reorder highlighted checked model and save\nSpace toggle  y copy id  e edit  r refresh  Esc back  q quit",
        };
        frame.render_widget(
            Paragraph::new(format!("{}\n{help}", self.status)).wrap(Wrap { trim: false }),
            chunks[2],
        );
    }
}

fn pick_loop(
    terminal: &mut ratatui::DefaultTerminal,
    state: &Path,
    picker: &mut Picker,
    refresh: bool,
) -> Result<Option<String>> {
    if refresh {
        picker.refresh(terminal, state)?;
    }
    let mut selection = None;
    loop {
        picker.draw(terminal)?;
        if !event::poll(Duration::from_millis(200)).map_err(|err| Error::new(err.to_string()))? {
            continue;
        }
        let Event::Key(key) = event::read().map_err(|err| Error::new(err.to_string()))? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if picker.navigate(state, key) {
            continue;
        }
        match key.code {
            KeyCode::Char('q') => break,
            KeyCode::Esc | KeyCode::Left if matches!(picker.screen, Screen::Catalog { .. }) => {
                picker.reset_view()
            }
            KeyCode::Esc => break,
            KeyCode::Char(' ') if matches!(picker.screen, Screen::Catalog { .. }) => {
                picker.toggle(state)
            }
            KeyCode::Char('c') => {
                picker.screen = Screen::Catalog {
                    provider: picker.selected().provider(),
                };
                picker.cursor = ListState::default();
                picker.cursor.select(Some(0));
            }
            KeyCode::Char('r') => picker.refresh(terminal, state)?,
            KeyCode::Char('e') => {
                // Editor-only reload preserves the editor's bytes; it never resaves JSON.
                ratatui::restore();
                let edited = edit_models(state);
                *terminal = ratatui::try_init()
                    .map_err(|err| Error::new(format!("terminal resume: {err}")))?;
                match edited.and_then(|exit| {
                    reload(&mut picker.registry, state, &picker.refresh_errors)?;
                    Ok(exit)
                }) {
                    Ok(exit) => {
                        picker.status = if exit.success() {
                            "Reloaded models.json (no network request)".to_string()
                        } else {
                            format!(
                                "Editor exited {exit}; reloaded models.json (no network request)"
                            )
                        };
                        picker.reset_view();
                    }
                    Err(err) => picker.status = err.to_string(),
                }
            }
            KeyCode::Char('y') if matches!(picker.screen, Screen::Catalog { .. }) => {
                if let Row::Model { provider, model } = picker.selected() {
                    let id = catalog_ids(&picker.registry.providers[provider])[model].to_string();
                    if copy_to_clipboard(&id) {
                        picker.status = format!("Copied {id}");
                    } else {
                        picker.status =
                            format!("No clipboard available; {id} will be printed on exit");
                        picker.print_on_exit.push(id);
                    }
                }
            }
            KeyCode::Enter if matches!(picker.screen, Screen::Picker) => {
                if let Row::Model { provider, model } = picker.selected() {
                    let provider = &picker.registry.providers[provider];
                    selection = Some(format!("{}/{}", provider.id, provider.models[model].id));
                    break;
                }
            }
            _ => {}
        }
    }
    if let Some(selection) = &selection {
        std::fs::create_dir_all(state)
            .map_err(|err| Error::new(format!("{}: {err}", state.display())))?;
        let path = state.join("selection");
        std::fs::write(&path, format!("{selection}\n"))
            .map_err(|err| Error::new(format!("{}: {err}", path.display())))?;
        picker.print_on_exit.push(format!(
            "Selected {selection}; saved to {}.",
            path.display()
        ));
    }
    Ok(selection)
}

/// Commit the new registry only after the complete local load succeeds.
fn reload(registry: &mut Registry, state: &Path, errors: &BTreeMap<String, String>) -> Result<()> {
    let mut next = harness::load_registry(state)?;
    apply_refresh_errors(&mut next, errors);
    *registry = next;
    Ok(())
}

fn apply_refresh_errors(registry: &mut Registry, errors: &BTreeMap<String, String>) {
    for provider in &mut registry.providers {
        if let Some(error) = errors.get(&provider.id) {
            provider.catalog_error = Some(error.clone());
        }
    }
}

/// Curated IDs always lead, including IDs absent from the reference catalog.
/// Remaining reference IDs are unchecked and never become picker rows implicitly.
fn catalog_ids(provider: &harness::LoadedProvider) -> Vec<&str> {
    let mut ids: Vec<&str> = provider
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    let mut seen: BTreeSet<&str> = ids.iter().copied().collect();
    if let Some(catalog) = &provider.catalog {
        for id in &catalog.models {
            if seen.insert(id) {
                ids.push(id);
            }
        }
    }
    ids
}

fn edit_models(state: &Path) -> Result<std::process::ExitStatus> {
    let command = std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
    run_editor(state, command.as_deref())
}

fn run_editor(state: &Path, command: Option<&str>) -> Result<std::process::ExitStatus> {
    let command = match command {
        Some(command) => command,
        None if cfg!(windows) => "notepad",
        None => return Err(Error::new("set VISUAL or EDITOR to edit models.json")),
    };
    let args = crate::shlex::split(command)?;
    std::fs::create_dir_all(state)
        .map_err(|err| Error::new(format!("{}: {err}", state.display())))?;
    let path = state.join("models.json");
    Command::new(&args[0])
        .args(&args[1..])
        .arg(&path)
        .status()
        .map_err(|err| Error::new(format!("editor for {}: {err}", path.display())))
}

fn copy_to_clipboard(text: &str) -> bool {
    let commands: &[&[&str]] = if cfg!(windows) {
        &[&[
            "powershell",
            "-NoProfile",
            "-Command",
            "$input | Set-Clipboard",
        ]]
    } else if cfg!(target_os = "macos") {
        &[&["pbcopy"]]
    } else {
        &[
            &["wl-copy"],
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
        ]
    };
    for args in commands {
        let Ok(mut child) = Command::new(args[0])
            .args(&args[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let written = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
        if child.wait().is_ok_and(|status| status.success()) && written {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_checkboxes_reorder_curated_ids_not_reference_catalog() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("catalog-curation");
        std::fs::create_dir_all(dir.join("catalogs")).unwrap();
        let catalog = r#"["reference-only","vendor/one/two","first"]"#;
        std::fs::write(dir.join("catalogs/openai.json"), catalog).unwrap();
        std::fs::write(dir.join("models.json"), r#"{"providers":{"openai":{"baseUrl":"https://api.openai.com/v1","api":"openai-completions","models":[{"id":"missing"},{"id":"first"}]}}}"#).unwrap();
        let mut picker = Picker::new(harness::load_registry(&dir).unwrap());
        picker.screen = Screen::Catalog { provider: 0 };
        assert_eq!(
            catalog_ids(&picker.registry.providers[0]),
            ["missing", "first", "reference-only", "vendor/one/two"]
        );
        let backend = ratatui::backend::TestBackend::new(140, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| picker.draw_frame(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("catalog — curate models.json"));
        assert!(text.contains("[x] missing  [absent from catalog]"));
        assert!(text.contains("[ ] reference-only"));
        assert!(text.contains("retrieval time unknown (old catalog format)"));
        picker.reorder(&dir, 1);
        assert_eq!(
            catalog_ids(&picker.registry.providers[0]),
            ["first", "missing", "reference-only", "vendor/one/two"]
        );
        assert_eq!(picker.cursor.selected(), Some(1));
        picker.cursor.select(Some(3));
        picker.toggle(&dir);
        assert_eq!(
            catalog_ids(&picker.registry.providers[0]),
            ["first", "missing", "vendor/one/two", "reference-only"]
        );
        picker.reorder(&dir, -1);
        assert_eq!(
            catalog_ids(&picker.registry.providers[0]),
            ["first", "vendor/one/two", "missing", "reference-only"]
        );
        let fresh = harness::load_registry(&dir).unwrap();
        assert_eq!(fresh, picker.registry);
        assert_eq!(
            fresh.providers[0]
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "vendor/one/two", "missing"]
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("catalogs/openai.json")).unwrap(),
            catalog
        );
        picker.cursor.select(Some(2));
        picker.toggle(&dir);
        assert_eq!(
            catalog_ids(&picker.registry.providers[0]),
            ["first", "vendor/one/two", "reference-only"]
        );
        assert_eq!(picker.registry.providers[0].models.len(), 2);
        picker.reset_view();
        assert_eq!(picker.rows().len(), 3); // header plus only the two checked models
        assert!(!dir.join("selection").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shifted_letter_and_arrow_events_persist_the_same_reorder() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("reorder-keys");
        let config = r#"{"providers":{"openai":{"baseUrl":"https://api.openai.com/v1","api":"openai-completions","models":[{"id":"first"},{"id":"second"}]}}}"#;
        for (down, up) in [
            (
                KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('K'), KeyModifiers::NONE),
            ),
            (
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::SHIFT),
                KeyEvent::new(KeyCode::Char('k'), KeyModifiers::SHIFT),
            ),
            (
                KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
                KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT),
            ),
        ] {
            std::fs::write(dir.join("models.json"), config).unwrap();
            let mut picker = Picker::new(harness::load_registry(&dir).unwrap());
            picker.screen = Screen::Catalog { provider: 0 };
            assert!(picker.navigate(&dir, down));
            let fresh = harness::load_registry(&dir).unwrap();
            assert_eq!(
                fresh.providers[0]
                    .models
                    .iter()
                    .map(|model| model.id.as_str())
                    .collect::<Vec<_>>(),
                ["second", "first"]
            );
            assert_eq!(picker.cursor.selected(), Some(1));
            assert!(picker.navigate(&dir, up));
            let fresh = harness::load_registry(&dir).unwrap();
            assert_eq!(
                fresh.providers[0]
                    .models
                    .iter()
                    .map(|model| model.id.as_str())
                    .collect::<Vec<_>>(),
                ["first", "second"]
            );
            assert_eq!(picker.cursor.selected(), Some(0));
            let before = std::fs::read(dir.join("models.json")).unwrap();
            assert!(picker.navigate(&dir, KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)));
            assert_eq!(picker.cursor.selected(), Some(1));
            assert!(picker.navigate(&dir, KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)));
            assert_eq!(picker.cursor.selected(), Some(0));
            assert_eq!(std::fs::read(dir.join("models.json")).unwrap(), before);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn checkbox_save_failure_keeps_loaded_selection_and_reports_recovery() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("catalog-save-error");
        std::fs::create_dir_all(dir.join("catalogs")).unwrap();
        std::fs::write(dir.join("catalogs/openai.json"), r#"["check/me"]"#).unwrap();
        let mut picker = Picker::new(harness::load_registry(&dir).unwrap());
        picker.screen = Screen::Catalog { provider: 0 };
        let previous = picker.registry.clone();
        assert_eq!(catalog_ids(&picker.registry.providers[0]), ["check/me"]);
        assert!(!dir.join("models.json").exists());
        std::fs::write(dir.join("models.json.tmp"), "busy").unwrap();
        picker.toggle(&dir);
        assert_eq!(picker.registry, previous);
        assert!(picker.status.contains("Save failed: cannot create"));
        assert!(!dir.join("models.json").exists());
        std::fs::remove_file(dir.join("models.json.tmp")).unwrap();
        picker.toggle(&dir);
        assert_eq!(picker.registry.providers[0].models[0].id, "check/me");
        let previous = picker.registry.clone();
        let external = "{\n  \"providers\": {}\n}\n";
        std::fs::write(dir.join("models.json"), external).unwrap();
        picker.toggle(&dir);
        assert_eq!(picker.registry, previous);
        assert!(picker.status.contains("changed outside this view; press e"));
        assert!(picker.status.contains("loaded selection unchanged"));
        assert_eq!(
            std::fs::read_to_string(dir.join("models.json")).unwrap(),
            external
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_edit_preserves_registry_and_reports_file_and_line() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("editor-reload");
        let mut registry = harness::load_registry(&dir).unwrap();
        let previous = registry.clone();
        for broken in [
            "{\ninvalid\n}",
            "{\"providers\": {\n\"openai\": {\"baseUrl\":\"https://api.openai.com/v1\",\"api\":\"unsupported\"}}}",
        ] {
            std::fs::write(dir.join("models.json"), broken).unwrap();
            let error = reload(&mut registry, &dir, &BTreeMap::new())
                .unwrap_err()
                .to_string();
            assert!(error.contains(&dir.join("models.json").display().to_string()), "{error}");
            assert!(error.contains("line 2"), "{error}");
            assert_eq!(registry, previous);
            assert_eq!(std::fs::read_to_string(dir.join("models.json")).unwrap(), broken);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn editor_reload_preserves_bytes_and_curated_order() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("editor-save");
        let script = dir.join("test editor.sh");
        let body = "{\n  \"providers\": {\"openai\": {\"baseUrl\": \"https://api.openai.com/v1\", \"api\": \"openai-completions\", \"models\": [{\"id\":\"second\"},{\"id\":\"first\"}]}}\n}\n";
        std::fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s' '{}' > \"$1\"\n", body),
        )
        .unwrap();
        run_editor(&dir, Some(&format!("sh '{}'", script.display()))).unwrap();
        let mut registry = harness::load_registry(&dir).unwrap();
        reload(&mut registry, &dir, &BTreeMap::new()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("models.json")).unwrap(),
            body
        );
        assert_eq!(
            registry.providers[0]
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["second", "first"]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(not(windows))]
    fn no_editor_does_not_create_models_json() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("editor-unset");
        assert_eq!(
            run_editor(&dir, None).unwrap_err().to_string(),
            "set VISUAL or EDITOR to edit models.json"
        );
        assert!(!dir.join("models.json").exists());
        assert_eq!(
            harness::load_registry(&dir).unwrap().providers[0]
                .empty_reason
                .as_deref(),
            Some("no models in models.json: press e to edit, c to browse the catalog")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn picker_draws_empty_reasons_and_catalog_membership() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("picker-render");
        let registry = harness::load_registry(&dir).unwrap();
        let mut picker = Picker::new(registry);
        let backend = ratatui::backend::TestBackend::new(120, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| picker.draw_frame(frame)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("no models in models.json: press e to edit, c to browse the catalog"));
        assert!(text.contains("Enter opens chat"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
