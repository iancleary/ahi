//! Curated-model picker, catalog curation, editor handoff, and chat entry.
//!
//! Enter saves a curated selection and opens chat.
//! Non-terminal invocation prints the local registry without opening chat.
pub(crate) mod commands;
pub(crate) mod gateway;
pub(crate) mod login;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use ahi_widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui_core::layout::{Constraint, Direction, Layout};
use ratatui_core::style::{Modifier, Style};

use crate::chat;
use crate::chat::input::{EditorKey, InputEditor};
use crate::directories::Directories;
use crate::error::{Error, Result};
use crate::harness::{self, Registry};
use commands::{CommandContext, CommandMenu, CommonAction, PickerOutcome};

pub struct RunOutput {
    pub text: String,
    pub refresh_failed: bool,
}

/// `actions` names a tool policy file to use instead of the working
/// directory's `ahi.tools.yaml`. It is read once here, so an invalid or
/// missing file fails before the picker opens, and again on every chat turn.
/// `resume` names a stored session to continue instead of opening the
/// picker; `record` is false for `--no-history`.
pub fn run(
    state: &Directories,
    refresh: bool,
    actions: Option<&Path>,
    resume: Option<&str>,
    record: bool,
) -> Result<RunOutput> {
    let actions =
        match actions {
            Some(path) => {
                chat::check_actions_file(path)?;
                Some(path.canonicalize().map_err(|err| {
                    Error::new(format!("cannot resolve {}: {err}", path.display()))
                })?)
            }
            None => None,
        };
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
    // A resumed session skips the picker: its model is on record.
    let selection = match resume {
        Some(id) => Some(Selected::Resumed(id.to_string())),
        None => match pick(state, &mut picker, refresh)? {
            PickerOutcome::Selected(selection) => Some(Selected::Picked(selection)),
            _ => None,
        },
    };
    if let Some(selection) = selection {
        let reopen = |view: chat::PickerView, gateway: &str, registration| {
            // Reopen the same curated picker, with a fresh local snapshot.
            // A malformed external edit retains the last valid registry.
            if let Err(err) = reload(&mut picker.registry, state, &picker.refresh_errors) {
                picker.status = err.to_string();
            }
            picker.reset_view();
            if let Some(registration) = registration {
                picker.set_registration(registration);
            }
            picker.focus_provider(gateway);
            if matches!(view, chat::PickerView::Catalog) {
                if let Some(provider) = picker
                    .registry
                    .providers
                    .iter()
                    .position(|entry| entry.id == gateway)
                {
                    picker.screen = Screen::Catalog { provider };
                    picker.cursor.select(Some(0));
                }
            }
            pick(state, &mut picker, false)
        };
        match selection {
            Selected::Picked(selection) => {
                chat::interact(state, &selection, actions.as_deref(), record, reopen)?
            }
            Selected::Resumed(id) => {
                chat::interact_resumed(state, &id, actions.as_deref(), reopen)?
            }
        }
    }
    Ok(RunOutput {
        text: picker.print_on_exit.join("\n"),
        refresh_failed: false,
    })
}

/// How the chat's first model was chosen.
enum Selected {
    Picked(String),
    Resumed(String),
}

fn pick(state: &Directories, picker: &mut Picker, refresh: bool) -> Result<PickerOutcome> {
    let mut terminal =
        crate::term::try_init().map_err(|err| Error::new(format!("terminal: {err}")))?;
    let result =
        enable_command_paste().and_then(|()| pick_loop(&mut terminal, state, picker, refresh));
    let cleanup = disable_command_paste();
    crate::term::restore();
    cleanup?;
    result
}

fn enable_command_paste() -> Result<()> {
    crossterm::execute!(io::stdout(), EnableBracketedPaste)
        .map_err(|error| Error::new(format!("terminal paste: {error}")))
}

fn disable_command_paste() -> Result<()> {
    crossterm::execute!(io::stdout(), DisableBracketedPaste)
        .map_err(|error| Error::new(format!("terminal paste: {error}")))
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
    command_input: Option<InputEditor>,
    command_menu: CommandMenu,
    registration: Option<harness::GatewayRegistration>,
    new_installation: bool,
    refresh_errors: BTreeMap<String, String>,
    print_on_exit: Vec<String>,
}

impl Picker {
    fn new(registry: Registry) -> Self {
        let mut cursor = ListState::default();
        cursor.select(Some(0));
        let status = if registry
            .providers
            .iter()
            .all(|entry| entry.models.is_empty())
        {
            "No models selected; c choose models · / commands"
        } else {
            "Enter saves the highlighted model and opens chat"
        }
        .to_string();
        Self {
            registry,
            screen: Screen::Picker,
            cursor,
            status,
            command_input: None,
            command_menu: CommandMenu::default(),
            registration: None,
            new_installation: false,
            refresh_errors: BTreeMap::new(),
            print_on_exit: Vec::new(),
        }
    }

    fn set_registration(&mut self, registration: harness::GatewayRegistration) {
        self.status = match registration.catalog {
            harness::CatalogPersistence::Saved => {
                "Space adds/removes models; Enter uses a checked model".to_string()
            }
            harness::CatalogPersistence::Failed(_) => {
                "Gateway registered; resolve cache conflict, then r refresh".to_string()
            }
        };
        self.registration = Some(registration);
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

    fn navigate(&mut self, state: &Directories, key: KeyEvent) -> bool {
        match key.code {
            // Legacy terminals send uppercase letters; enhanced keyboard
            // protocols can send lowercase letters with a Shift modifier.
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J')
                if (key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.code == KeyCode::Char('J')) =>
            {
                self.reorder(state, 1)
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K')
                if (key.modifiers.contains(KeyModifiers::SHIFT)
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

    fn reorder_provider(&mut self, state: &Directories, provider: usize, delta: isize) {
        let target = provider as isize + delta;
        if target < 0 || target >= self.registry.providers.len() as isize {
            return;
        }
        let id = self.registry.providers[provider].id.clone();
        let mut order = self
            .registry
            .providers
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();
        order.swap(provider, target as usize);
        match harness::save_provider_order(state, &self.registry, &order) {
            Ok(mut next) => {
                apply_refresh_errors(&mut next, &self.refresh_errors);
                self.registry = next;
                let row = self.rows().iter().position(|row| matches!(row, Row::Header(index) if self.registry.providers[*index].id == id)).unwrap_or(0);
                self.cursor.select(Some(row));
                self.status = "Saved gateway order in models.json".to_string();
            }
            Err(error) => self.status = format!("Save failed: {error}; gateway order unchanged"),
        }
    }

    fn reset_view(&mut self) {
        self.registration = None;
        self.screen = Screen::Picker;
        self.cursor = ListState::default();
        self.cursor.select(Some(0));
    }

    fn focus_provider(&mut self, id: &str) {
        let row = self
            .rows()
            .iter()
            .position(|row| {
                matches!(row, Row::Model { .. }) && self.registry.providers[row.provider()].id == id
            })
            .or_else(|| {
                self.rows()
                    .iter()
                    .position(|row| self.registry.providers[row.provider()].id == id)
            });
        self.cursor.select(row.or(Some(0)));
    }

    fn back_to_models(&mut self) {
        let selected = self.selected();
        let entry = &self.registry.providers[selected.provider()];
        let id = entry.id.clone();
        let model_id = match (self.screen, selected) {
            (Screen::Catalog { .. }, Row::Model { model, .. }) => {
                Some(catalog_ids(entry)[model].to_string())
            }
            _ => None,
        };
        self.reset_view();
        self.focus_provider(&id);
        if let Some(position) = self.rows().iter().position(|row| {
            matches!(row, Row::Model { provider, model }
                if self.registry.providers[*provider].id == id
                    && Some(&self.registry.providers[*provider].models[*model].id) == model_id.as_ref())
        }) {
            self.cursor.select(Some(position));
        }
        self.status = "c choose models · / commands · Enter use highlighted model".to_string();
    }

    fn command_context(&self) -> CommandContext {
        match self.screen {
            Screen::Picker => CommandContext::Models,
            Screen::Catalog { .. } => CommandContext::Catalog,
        }
    }

    fn save_curated(
        &mut self,
        state: &Directories,
        provider: usize,
        ids: &[String],
        focus: &str,
    ) -> bool {
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

    fn toggle(&mut self, state: &Directories) {
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

    fn reorder(&mut self, state: &Directories, delta: isize) {
        if matches!(self.screen, Screen::Picker) {
            if let Row::Header(provider) = self.selected() {
                self.reorder_provider(state, provider, delta);
            } else {
                self.status = "Highlight a gateway header to reorder gateways; use c to reorder curated models".to_string();
            }
            return;
        }
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

    fn refresh(
        &mut self,
        terminal: &mut crate::term::DefaultTerminal,
        state: &Directories,
    ) -> Result<()> {
        let provider_id = self.registry.providers[self.selected().provider()]
            .id
            .clone();
        let catalog = matches!(self.screen, Screen::Catalog { .. });
        let selected = self.cursor.selected().unwrap_or(0);
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
        self.registration = None;
        self.reset_view();
        self.focus_provider(&provider_id);
        if catalog {
            if let Some(provider) = self
                .registry
                .providers
                .iter()
                .position(|entry| entry.id == provider_id)
            {
                self.screen = Screen::Catalog { provider };
                self.cursor
                    .select(Some(selected.min(self.rows().len().saturating_sub(1))));
            }
        }
        Ok(())
    }

    fn draw(&mut self, terminal: &mut crate::term::DefaultTerminal) -> Result<()> {
        // A chat copy may still be completing after switching to the picker.
        let stdout = io::stdout();
        let _output = stdout.lock();
        terminal
            .draw(|frame| self.draw_frame(frame))
            .map_err(|err| Error::new(format!("terminal draw: {err}")))?;
        Ok(())
    }

    fn draw_frame(&mut self, frame: &mut ratatui_core::terminal::Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(4),
                Constraint::Length(5),
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
                        Screen::Picker => "  No models selected · c choose models".to_string(),
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
            Screen::Picker => "Models · Enter use model".to_string(),
            Screen::Catalog { provider } => {
                let entry = &self.registry.providers[provider];
                format!(
                    "Catalog · {} · {} checked{}",
                    entry.id,
                    entry.models.len(),
                    if self
                        .registration
                        .as_ref()
                        .is_some_and(|registration| registration.provider_id == entry.id)
                    {
                        " · 3 Choose models"
                    } else {
                        ""
                    }
                )
            }
        };
        self.draw_list(frame, chunks[0], items, &title);
        self.draw_details(frame, &chunks);
    }

    fn draw_list(
        &mut self,
        frame: &mut ratatui_core::terminal::Frame,
        area: ratatui_core::layout::Rect,
        items: Vec<ListItem<'_>>,
        title: &str,
    ) {
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::default().borders(Borders::ALL).title(title))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
            area,
            &mut self.cursor,
        );
    }

    fn draw_details(
        &self,
        frame: &mut ratatui_core::terminal::Frame,
        chunks: &[ratatui_core::layout::Rect],
    ) {
        let provider = &self.registry.providers[self.selected().provider()];
        let detail = if let Some(registration) = self.registration.as_ref().filter(|registration| {
            registration.provider_id == provider.id && matches!(self.screen, Screen::Catalog { .. })
        }) {
            match &registration.catalog {
                harness::CatalogPersistence::Saved => format!("{}\nNo chat model has changed. Space adds models; Enter uses a checked model.", registration.notice),
                harness::CatalogPersistence::Failed(error) => format!("Gateway {} registered, but catalog save failed.\nResolve cache conflict; r refresh or /gateway retry.\nCache error: {error}", registration.provider_id),
            }
        } else if provider.models.is_empty() && matches!(self.screen, Screen::Picker) {
            if self.new_installation
                && self.registry.providers.len() == 1
                && provider.id == "openai"
                && provider.auth == "signed-out"
                && provider.catalog.is_none()
            {
                "Add your first gateway: press / and choose Add gateway.\nOr sign in with OpenAI from Commands, or set ANTHROPIC_API_KEY.".to_string()
            } else if provider.catalog.is_none() {
                "No models selected. Press c to choose models.\nCatalog unavailable: r discovers models; /gateway retries discovery.".to_string()
            } else {
                "No models selected. Press c to choose models from the catalog.\nFor another gateway, press / and choose Add gateway.".to_string()
            }
        } else {
            format!(
                "{}  auth {}  catalog {}{}\nSaved in models.json",
                provider.base_url,
                provider.auth,
                provider.catalog_age(),
                provider
                    .catalog_error
                    .as_ref()
                    .map(|error| format!("\nLast refresh/catalog error: {error}"))
                    .unwrap_or_default()
            )
        };
        let metadata = match self.selected() {
            Row::Model { model, .. } => match self.screen {
                Screen::Picker => Some(&provider.models[model].metadata),
                Screen::Catalog { .. } => catalog_ids(provider).get(model).and_then(|id| {
                    provider
                        .models
                        .iter()
                        .find(|choice| choice.id == **id)
                        .map(|choice| &choice.metadata)
                        .or_else(|| {
                            provider
                                .catalog
                                .as_ref()
                                .and_then(|catalog| catalog.metadata.get(*id))
                        })
                }),
            },
            _ => None,
        };
        let detail = match metadata {
            Some(metadata) if !metadata.summary().is_empty() => {
                format!("{}\n{detail}", metadata.summary())
            }
            _ => detail,
        };
        frame.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }), chunks[1]);
        let help = match self.screen {
            Screen::Picker => "/ commands · Enter use model · c catalog · Esc/q cancel\n↑/↓ move · Shift+↑/↓ reorder gateway · r refresh · e edit",
            Screen::Catalog { .. } => "/ commands · Space add/remove · Enter use checked model\nEsc models · q cancel · ↑/↓ move · Shift+↑/↓ reorder\ny copy · e edit · r refresh",
        };
        let footer =
            Layout::vertical([Constraint::Length(2), Constraint::Length(3)]).split(chunks[2]);
        if let Some(input) = &self.command_input {
            let viewport = input.viewport(footer[0].width as usize, footer[0].height as usize);
            frame.render_widget(Paragraph::new(viewport.lines.join("\n")), footer[0]);
            if footer[0].width > 0 && footer[0].height > 0 {
                frame.set_cursor_position((
                    footer[0].x + viewport.cursor_column as u16,
                    footer[0].y + viewport.cursor_row as u16,
                ));
            }
        } else {
            frame.render_widget(
                Paragraph::new(&*self.status).wrap(Wrap { trim: false }),
                footer[0],
            );
        }
        frame.render_widget(Paragraph::new(help).wrap(Wrap { trim: false }), footer[1]);
        if let Some(input) = &self.command_input {
            let popup_area = ratatui_core::layout::Rect::new(
                chunks[0].x,
                chunks[0].y,
                chunks[0].width,
                chunks[2].y - chunks[0].y,
            );
            self.command_menu.draw(
                frame,
                popup_area,
                &input.contents(),
                self.command_context(),
                " / commands ",
            );
        }
    }
}

fn pick_loop(
    terminal: &mut crate::term::DefaultTerminal,
    state: &Directories,
    picker: &mut Picker,
    refresh: bool,
) -> Result<PickerOutcome> {
    picker.new_installation = !state.config.join("models.json").exists();
    if refresh {
        picker.refresh(terminal, state)?;
    }
    let mut selection = None;
    let mut outcome = PickerOutcome::Back;
    loop {
        picker.draw(terminal)?;
        if !event::poll(Duration::from_millis(200)).map_err(|err| Error::new(err.to_string()))? {
            continue;
        }
        let event = event::read().map_err(|err| Error::new(err.to_string()))?;
        if let Event::Paste(text) = &event {
            if let Some(command) = &mut picker.command_input {
                if !text.chars().any(char::is_control)
                    && command.contents().len().saturating_add(text.len()) <= 128
                {
                    command.insert_paste(text);
                    picker.command_menu.reset();
                } else {
                    picker.status = "Paste a single-line command (maximum 128 bytes)".to_string();
                }
            }
            continue;
        }
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let context = picker.command_context();
        if let Some(command) = &mut picker.command_input {
            let text = command.contents();
            match key.code {
                KeyCode::Up | KeyCode::Down => {
                    picker.command_menu.navigate(
                        &text,
                        context,
                        if key.code == KeyCode::Up { -1 } else { 1 },
                    );
                }
                KeyCode::Esc => {
                    picker.command_menu.dismissed = true;
                    picker.command_input = None;
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    picker.command_input = None;
                }
                KeyCode::Backspace => {
                    command.apply(EditorKey::Backspace, 80);
                    picker.command_menu.reset();
                    if command.contents().is_empty() {
                        picker.command_input = None;
                    }
                }
                KeyCode::Enter => {
                    let command = picker
                        .command_menu
                        .chosen(&text, context)
                        .unwrap_or(&text)
                        .to_string();
                    picker.command_input = None;
                    picker.command_menu.reset();
                    if commands::common_action(&command) == Some(CommonAction::Login) {
                        disable_command_paste()?;
                        crate::term::restore();
                        let result = login::run(state);
                        *terminal = crate::term::try_init()
                            .map_err(|error| Error::new(format!("terminal resume: {error}")))?;
                        enable_command_paste()?;
                        picker.status = match result {
                            Ok(()) => match reload(
                                &mut picker.registry,
                                state,
                                &picker.refresh_errors,
                            ) {
                                Ok(()) => {
                                    "OpenAI login completed; catalogs and model selection unchanged"
                                        .to_string()
                                }
                                Err(error) => {
                                    format!("OpenAI login completed; reload failed: {error}")
                                }
                            },
                            Err(error) => error.to_string(),
                        };
                    } else if commands::common_action(&command) == Some(CommonAction::Gateway) {
                        disable_command_paste()?;
                        crate::term::restore();
                        let result = gateway::run(state);
                        *terminal = crate::term::try_init()
                            .map_err(|error| Error::new(format!("terminal resume: {error}")))?;
                        enable_command_paste()?;
                        match result {
                            Ok(Some(registration)) => {
                                let provider_id = registration.provider_id.clone();
                                picker.set_registration(registration);
                                match reload(&mut picker.registry, state, &picker.refresh_errors) {
                                    Ok(()) => {
                                        if let Some(provider) = picker
                                            .registry
                                            .providers
                                            .iter()
                                            .position(|entry| entry.id == provider_id)
                                        {
                                            picker.screen = Screen::Catalog { provider };
                                            picker.cursor.select(Some(0));
                                        }
                                    }
                                    Err(error) => {
                                        picker.status =
                                            format!("{}; reload failed: {error}", picker.status)
                                    }
                                }
                            }
                            Ok(None) => picker.status = "Gateway discovery cancelled".to_string(),
                            Err(error) => picker.status = error.to_string(),
                        }
                    } else {
                        match commands::common_action(&command) {
                            Some(CommonAction::Quit) => {
                                outcome = PickerOutcome::Exit;
                                break;
                            }
                            Some(CommonAction::Models) => picker.back_to_models(),
                            Some(CommonAction::Catalog) => {
                                picker.screen = Screen::Catalog {
                                    provider: picker.selected().provider(),
                                };
                                picker.cursor.select(Some(0));
                            }
                            _ => {
                                picker.status = "Unknown command; press / for Commands".to_string()
                            }
                        }
                    }
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && !character.is_control()
                        && command.contents().len() + character.len_utf8() <= 128 =>
                {
                    command.apply(EditorKey::Character(character), 80);
                    picker.command_menu.reset();
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End | KeyCode::Delete => {
                    let editor_key = match key.code {
                        KeyCode::Left => EditorKey::Left,
                        KeyCode::Right => EditorKey::Right,
                        KeyCode::Home => EditorKey::Home,
                        KeyCode::End => EditorKey::End,
                        _ => EditorKey::Delete,
                    };
                    command.apply(editor_key, 80);
                    picker.command_menu.reset();
                }
                _ => {}
            }
            continue;
        }
        if picker.navigate(state, key) {
            continue;
        }
        match key.code {
            KeyCode::Char('/') => {
                let mut input = InputEditor::new();
                input.insert_paste("/");
                picker.command_input = Some(input);
                picker.command_menu.reset();
            }
            KeyCode::Char('q') => break,
            KeyCode::Esc | KeyCode::Left if matches!(picker.screen, Screen::Catalog { .. }) => {
                picker.back_to_models()
            }
            KeyCode::Esc => break,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
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
                disable_command_paste()?;
                crate::term::restore();
                let edited = edit_models(state);
                *terminal = crate::term::try_init()
                    .map_err(|err| Error::new(format!("terminal resume: {err}")))?;
                enable_command_paste()?;
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
                    if crate::clipboard::copy(&id) {
                        picker.status = format!("Copied {id}");
                    } else {
                        picker.status =
                            format!("No clipboard available; {id} will be printed on exit");
                        picker.print_on_exit.push(id);
                    }
                }
            }
            KeyCode::Enter => {
                if let Row::Model { provider, model } = picker.selected() {
                    let entry = &picker.registry.providers[provider];
                    let id = match picker.screen {
                        Screen::Picker => entry.models[model].id.as_str(),
                        Screen::Catalog { .. } => catalog_ids(entry)[model],
                    };
                    if entry.models.iter().any(|choice| choice.id == id) {
                        selection = Some(format!("{}/{id}", entry.id));
                        break;
                    }
                    picker.status =
                        "Press Space to add this model, then Enter to use it".to_string();
                } else if matches!(picker.screen, Screen::Catalog { .. }) {
                    picker.status =
                        "No models available; r refresh or /gateway retry discovery".to_string();
                }
            }
            _ => {}
        }
    }
    picker.registration = None;
    if let Some(selection) = &selection {
        crate::directories::create_private_dir(&state.state)
            .map_err(|err| Error::new(format!("{}: {err}", state.state.display())))?;
        let path = state.state.join("selection");
        crate::directories::write_private(&path, format!("{selection}\n").as_bytes())
            .map_err(|err| Error::new(format!("{}: {err}", path.display())))?;
        picker.print_on_exit.push(format!(
            "Selected {selection}; saved to {}.",
            path.display()
        ));
    }
    Ok(match selection {
        Some(selection) => PickerOutcome::Selected(selection),
        None => outcome,
    })
}

/// Commit the new registry only after the complete local load succeeds.
fn reload(
    registry: &mut Registry,
    state: &Directories,
    errors: &BTreeMap<String, String>,
) -> Result<()> {
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

fn edit_models(state: &Directories) -> Result<std::process::ExitStatus> {
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

fn run_editor(state: &Directories, command: Option<&str>) -> Result<std::process::ExitStatus> {
    let command = match command {
        Some(command) => command,
        None if cfg!(windows) => "notepad",
        None => return Err(Error::new("set VISUAL or EDITOR to edit models.json")),
    };
    let args = crate::shlex::split(command)?;
    crate::directories::create_private_dir(&state.config)
        .map_err(|err| Error::new(format!("{}: {err}", state.config.display())))?;
    let path = state.config.join("models.json");
    Command::new(&args[0])
        .args(&args[1..])
        .arg(&path)
        .status()
        .map_err(|err| Error::new(format!("editor for {}: {err}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_header_reorder_persists_and_stale_saves_keep_the_view() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("gateway-order");
        let path = dir.join("models.json");
        std::fs::write(&path, r#"{"providers":{"home":{"baseUrl":"http://127.0.0.1:1234/v1","api":"openai-completions","apiKey":"gateway","models":[{"id":"local/model"}]},"work":{"baseUrl":"https://work.example/v1","api":"openai-completions","models":[{"id":"work/model"}]}}}"#).unwrap();
        let before: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut picker =
            Picker::new(harness::load_registry(&Directories::single_root(&dir)).unwrap());
        assert_eq!(
            picker
                .registry
                .providers
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["openai", "home", "work"]
        );
        // The empty built-in contributes a header and an empty row.
        picker.cursor.select(Some(2));
        picker.navigate(
            &Directories::single_root(&dir),
            KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT),
        );
        assert_eq!(
            picker
                .registry
                .providers
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["home", "openai", "work"]
        );
        assert!(matches!(picker.selected(), Row::Header(0)));
        let reloaded = harness::load_registry(&Directories::single_root(&dir)).unwrap();
        assert_eq!(
            reloaded
                .providers
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["home", "openai", "work"]
        );
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(after["providers"]["home"], before["providers"]["home"]);
        assert_eq!(after["providers"]["work"], before["providers"]["work"]);
        assert_eq!(reloaded.providers[0].models[0].id, "local/model");
        let original = picker.registry.clone();
        let mut edited = std::fs::read(&path).unwrap();
        edited.push(b'\n');
        std::fs::write(&path, &edited).unwrap();
        picker.navigate(
            &Directories::single_root(&dir),
            KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
        );
        assert_eq!(picker.registry, original);
        assert_eq!(std::fs::read(&path).unwrap(), edited);
        assert!(picker.status.contains("changed outside this view"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn catalog_checkboxes_reorder_curated_ids_not_reference_catalog() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("catalog-curation");
        std::fs::create_dir_all(dir.join("catalogs")).unwrap();
        let catalog = r#"["reference-only","vendor/one/two","first"]"#;
        std::fs::write(dir.join("catalogs/openai.json"), catalog).unwrap();
        std::fs::write(dir.join("models.json"), r#"{"providers":{"openai":{"baseUrl":"https://api.openai.com/v1","api":"openai-completions","models":[{"id":"missing"},{"id":"first"}]}}}"#).unwrap();
        let mut picker =
            Picker::new(harness::load_registry(&Directories::single_root(&dir)).unwrap());
        picker.screen = Screen::Catalog { provider: 0 };
        assert_eq!(
            catalog_ids(&picker.registry.providers[0]),
            ["missing", "first", "reference-only", "vendor/one/two"]
        );
        let backend = ratatui_core::backend::TestBackend::new(140, 20);
        let mut terminal = ratatui_core::terminal::Terminal::new(backend).unwrap();
        terminal.draw(|frame| picker.draw_frame(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Catalog · openai"));
        assert!(text.contains("[x] missing  [absent from catalog]"));
        assert!(text.contains("[ ] reference-only"));
        assert!(text.contains("retrieval time unknown (old catalog format)"));
        picker.reorder(&Directories::single_root(&dir), 1);
        assert_eq!(
            catalog_ids(&picker.registry.providers[0]),
            ["first", "missing", "reference-only", "vendor/one/two"]
        );
        assert_eq!(picker.cursor.selected(), Some(1));
        picker.cursor.select(Some(3));
        picker.toggle(&Directories::single_root(&dir));
        assert_eq!(
            catalog_ids(&picker.registry.providers[0]),
            ["first", "missing", "vendor/one/two", "reference-only"]
        );
        picker.reorder(&Directories::single_root(&dir), -1);
        assert_eq!(
            catalog_ids(&picker.registry.providers[0]),
            ["first", "vendor/one/two", "missing", "reference-only"]
        );
        let fresh = harness::load_registry(&Directories::single_root(&dir)).unwrap();
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
        picker.toggle(&Directories::single_root(&dir));
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
            let mut picker =
                Picker::new(harness::load_registry(&Directories::single_root(&dir)).unwrap());
            picker.screen = Screen::Catalog { provider: 0 };
            assert!(picker.navigate(&Directories::single_root(&dir), down));
            let fresh = harness::load_registry(&Directories::single_root(&dir)).unwrap();
            assert_eq!(
                fresh.providers[0]
                    .models
                    .iter()
                    .map(|model| model.id.as_str())
                    .collect::<Vec<_>>(),
                ["second", "first"]
            );
            assert_eq!(picker.cursor.selected(), Some(1));
            assert!(picker.navigate(&Directories::single_root(&dir), up));
            let fresh = harness::load_registry(&Directories::single_root(&dir)).unwrap();
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
            assert!(picker.navigate(
                &Directories::single_root(&dir),
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)
            ));
            assert_eq!(picker.cursor.selected(), Some(1));
            assert!(picker.navigate(
                &Directories::single_root(&dir),
                KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)
            ));
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
        let mut picker =
            Picker::new(harness::load_registry(&Directories::single_root(&dir)).unwrap());
        picker.screen = Screen::Catalog { provider: 0 };
        let previous = picker.registry.clone();
        assert_eq!(catalog_ids(&picker.registry.providers[0]), ["check/me"]);
        assert!(!dir.join("models.json").exists());
        std::fs::write(dir.join("models.json.tmp"), "busy").unwrap();
        picker.toggle(&Directories::single_root(&dir));
        assert_eq!(picker.registry, previous);
        assert!(picker.status.contains("Save failed: cannot create"));
        assert!(!dir.join("models.json").exists());
        std::fs::remove_file(dir.join("models.json.tmp")).unwrap();
        picker.toggle(&Directories::single_root(&dir));
        assert_eq!(picker.registry.providers[0].models[0].id, "check/me");
        let previous = picker.registry.clone();
        let external = "{\n  \"providers\": {}\n}\n";
        std::fs::write(dir.join("models.json"), external).unwrap();
        picker.toggle(&Directories::single_root(&dir));
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
        let mut registry = harness::load_registry(&Directories::single_root(&dir)).unwrap();
        let previous = registry.clone();
        for broken in [
            "{\ninvalid\n}",
            "{\"providers\": {\n\"openai\": {\"baseUrl\":\"https://api.openai.com/v1\",\"api\":\"unsupported\"}}}",
        ] {
            std::fs::write(dir.join("models.json"), broken).unwrap();
            let error = reload(&mut registry, &Directories::single_root(&dir), &BTreeMap::new())
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
            format!("#!/bin/sh\nprintf '%s' '{body}' > \"$1\"\n"),
        )
        .unwrap();
        run_editor(
            &Directories::single_root(&dir),
            Some(&format!("sh '{}'", script.display())),
        )
        .unwrap();
        let mut registry = harness::load_registry(&Directories::single_root(&dir)).unwrap();
        reload(
            &mut registry,
            &Directories::single_root(&dir),
            &BTreeMap::new(),
        )
        .unwrap();
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
            run_editor(&Directories::single_root(&dir), None)
                .unwrap_err()
                .to_string(),
            "set VISUAL or EDITOR to edit models.json"
        );
        assert!(!dir.join("models.json").exists());
        assert_eq!(
            harness::load_registry(&Directories::single_root(&dir))
                .unwrap()
                .providers[0]
                .empty_reason
                .as_deref(),
            Some("no models in models.json: press e to edit, c to browse the catalog")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn picker_draws_empty_reasons_and_catalog_membership() {
        let dir = crate::fixture_directory::create_unique_fixture_directory("picker-render");
        let registry = harness::load_registry(&Directories::single_root(&dir)).unwrap();
        let mut picker = Picker::new(registry);
        let backend = ratatui_core::backend::TestBackend::new(120, 20);
        let mut terminal = ratatui_core::terminal::Terminal::new(backend).unwrap();
        terminal.draw(|frame| picker.draw_frame(frame)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("No models selected · c choose models"));
        assert!(text.contains("Enter use model"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
