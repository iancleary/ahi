//! Provider picker and chat screen for the single-machine harness.
//!
//! A terminal shows OpenAI and the gateways from `models.json`, then opens the
//! chat screen. A pipe prints the same load as text, so tests do not need a
//! terminal.

use std::fs;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::chat;
use crate::error::{Error, Result};
use crate::harness::{self, Registry};

pub fn run(state: &Path) -> Result<String> {
    let registry = harness::load_registry(state)?;
    if !io::stdout().is_terminal() {
        return Ok(harness::render_text(&registry));
    }
    let picked = pick(&registry)?;
    let selection = if let Some(value) = picked {
        write_selection(state, &value)?;
        value
    } else if let Some(existing) = read_selection(state) {
        existing
    } else {
        return Ok(harness::render_text(&registry));
    };
    chat::interact(state, &selection, || match pick(&registry)? {
        Some(value) => {
            write_selection(state, &value)?;
            Ok(Some(value))
        }
        None => Ok(None),
    })?;
    Ok(harness::render_text(&registry))
}

fn write_selection(state: &Path, selection: &str) -> Result<()> {
    let path = state.join("selection");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| Error::new(format!("failed to create {}: {err}", parent.display())))?;
    }
    fs::write(&path, format!("{selection}\n"))
        .map_err(|err| Error::new(format!("failed to write {}: {err}", path.display())))
}

fn read_selection(state: &Path) -> Option<String> {
    let text = fs::read_to_string(state.join("selection")).ok()?;
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn pick(registry: &Registry) -> Result<Option<String>> {
    let mut terminal = ratatui::try_init().map_err(|err| Error::new(format!("terminal: {err}")))?;
    let result = pick_loop(&mut terminal, registry);
    ratatui::restore();
    result
}

fn pick_loop(
    terminal: &mut ratatui::DefaultTerminal,
    registry: &Registry,
) -> Result<Option<String>> {
    let mut provider_state = ListState::default();
    let mut model_state = ListState::default();
    provider_state.select(Some(0));
    let mut focus_models = false;
    let result = loop {
        let provider_index = provider_state.selected().unwrap_or(0);
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    registry,
                    provider_index,
                    focus_models,
                    &mut provider_state,
                    &mut model_state,
                )
            })
            .map_err(|err| Error::new(format!("terminal draw: {err}")))?;
        if !event::poll(Duration::from_millis(200)).map_err(|err| Error::new(err.to_string()))? {
            continue;
        }
        let event = event::read().map_err(|err| Error::new(err.to_string()))?;
        let Event::Key(key) = event else {
            continue;
        };
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break None,
            KeyCode::Right | KeyCode::Enter if !focus_models => {
                focus_models = true;
                model_state.select(Some(0));
            }
            KeyCode::Left if focus_models => focus_models = false,
            KeyCode::Down | KeyCode::Char('j') => {
                move_selection(
                    registry,
                    provider_index,
                    focus_models,
                    &mut provider_state,
                    &mut model_state,
                    1,
                );
            }
            KeyCode::Up | KeyCode::Char('k') => {
                move_selection(
                    registry,
                    provider_index,
                    focus_models,
                    &mut provider_state,
                    &mut model_state,
                    -1,
                );
            }
            KeyCode::Enter if focus_models => {
                let provider = registry.providers.get(provider_index);
                let model_index = model_state.selected().unwrap_or(0);
                if let Some(provider) = provider {
                    if let Some(model) = provider.models.get(model_index) {
                        break Some(format!("{}/{}", provider.id, model));
                    }
                }
            }
            _ => {}
        }
    };
    Ok(result)
}

fn move_selection(
    registry: &Registry,
    provider_index: usize,
    focus_models: bool,
    provider_state: &mut ListState,
    model_state: &mut ListState,
    delta: isize,
) {
    if focus_models {
        let len = registry
            .providers
            .get(provider_index)
            .map(|provider| provider.models.len())
            .unwrap_or(0);
        model_state.select(Some(step(model_state.selected().unwrap_or(0), len, delta)));
    } else {
        provider_state.select(Some(step(
            provider_state.selected().unwrap_or(0),
            registry.providers.len(),
            delta,
        )));
    }
}

fn step(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let next = current as isize + delta;
    if next < 0 {
        len - 1
    } else if next as usize >= len {
        0
    } else {
        next as usize
    }
}

fn draw(
    frame: &mut ratatui::Frame,
    registry: &Registry,
    provider_index: usize,
    focus_models: bool,
    provider_state: &mut ListState,
    model_state: &mut ListState,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(frame.area());
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(chunks[0]);
    let providers: Vec<ListItem> = registry
        .providers
        .iter()
        .map(|provider| ListItem::new(format!("{}  {}", provider.id, provider.auth)))
        .collect();
    let models = registry
        .providers
        .get(provider_index)
        .map(|provider| {
            if provider.models.is_empty() {
                vec![ListItem::new("(no models loaded)")]
            } else {
                provider
                    .models
                    .iter()
                    .map(|model| ListItem::new(model.clone()))
                    .collect()
            }
        })
        .unwrap_or_default();
    let highlight = Style::default().add_modifier(Modifier::REVERSED);
    frame.render_stateful_widget(
        List::new(providers)
            .block(Block::default().borders(Borders::ALL).title("providers"))
            .highlight_style(highlight),
        columns[0],
        provider_state,
    );
    frame.render_stateful_widget(
        List::new(models)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(if focus_models {
                        "models"
                    } else {
                        "models (right to focus)"
                    }),
            )
            .highlight_style(highlight),
        columns[1],
        model_state,
    );
    let note = registry
        .providers
        .get(provider_index)
        .map(|provider| {
            format!(
                "{}  {}  catalog {}",
                provider.base_url, provider.auth, provider.catalog
            )
        })
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(Line::from(format!(
            "{note}   q quit  arrows move  enter selects a model"
        ))),
        chunks[1],
    );
}
