use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use ahi_widgets::{Block, Borders, Paragraph, Wrap};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use ratatui_core::layout::{Constraint, Layout};
use ratatui_core::style::{Color, Style};

use crate::chat::input::{EditorKey, InputEditor};
use crate::directories::Directories;
use crate::error::{Error, Result};
use crate::harness::{self, DiscoveredGateway, GatewayRegistration};

enum DiscoveryState {
    Editing,
    Fetching(Receiver<Result<DiscoveredGateway>>),
    Preview(DiscoveredGateway),
}

pub(crate) fn run(state: &Directories) -> Result<Option<GatewayRegistration>> {
    let registry = harness::load_registry(state)?;
    let mut terminal = crate::term::try_init().map_err(|error| Error::new(error.to_string()))?;
    let result = crossterm::execute!(std::io::stdout(), EnableBracketedPaste)
        .map_err(|error| Error::new(error.to_string()))
        .and_then(|()| discovery_loop(&mut terminal, state, &registry));
    let cleanup = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    crate::term::restore();
    cleanup.map_err(|error| Error::new(error.to_string()))?;
    result
}

fn discovery_loop(
    terminal: &mut crate::term::DefaultTerminal,
    state: &Directories,
    registry: &harness::Registry,
) -> Result<Option<GatewayRegistration>> {
    let mut input = InputEditor::new();
    let mut discovery = DiscoveryState::Editing;
    let mut status = "Nothing is saved until you confirm in Review".to_string();
    loop {
        terminal
            .draw(|frame| {
                let areas = Layout::vertical([
                    Constraint::Length(2),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Min(1),
                    Constraint::Length(4),
                ])
                .split(frame.area());
                frame.render_widget(
                    Paragraph::new(match discovery {
                        DiscoveryState::Preview(_) => "Add gateway · 2 Review\n1 Connect → 2 Review → 3 Choose models",
                        _ => "Add gateway · 1 Connect\n1 Connect → 2 Review → 3 Choose models",
                    })
                        .wrap(Wrap { trim: false }),
                    areas[0],
                );
                let editing = matches!(discovery, DiscoveryState::Editing);
                let block = Block::default().borders(Borders::ALL)
                    .border_style(Style::default().fg(if editing { Color::Cyan } else { Color::DarkGray }))
                    .title(if editing { " Gateway URL · editing " } else { " Gateway URL " });
                let field = block.inner(areas[1]);
                frame.render_widget(block, areas[1]);
                let viewport = input.viewport(field.width as usize, field.height as usize);
                frame.render_widget(Paragraph::new(viewport.lines.join("\n")), field);
                if matches!(discovery, DiscoveryState::Editing) && field.width > 0 && field.height > 0 {
                    frame.set_cursor_position((
                        field.x + viewport.cursor_column as u16,
                        field.y + viewport.cursor_row as u16,
                    ));
                }
                frame.render_widget(Paragraph::new("Validated catalog baseUrl overrides the entered URL.\nEntered URL is the fallback; paths are never guessed.\nDiscovery: <origin>/agent/models.json · no credentials").wrap(Wrap { trim: false }), areas[2]);
                let detail = match &discovery {
                    DiscoveryState::Editing => "Enter discovers; review before registering. Errors keep your URL for retry.".to_string(),
                    DiscoveryState::Fetching(_) => "Discovering gateway… Esc cancels without saving".to_string(),
                    DiscoveryState::Preview(gateway) => {
                        let preview_limit = areas[3].height.saturating_sub(7) as usize;
                        format!("{}\n{} models discovered; no models selected automatically\n{}\n{}", gateway.review_summary(registry), gateway.models().len(), gateway.models().iter().take(preview_limit).map(String::as_str).collect::<Vec<_>>().join("\n"), if gateway.models().len() > preview_limit { "More models in Catalog after confirmation" } else { "" })
                    },
                };
                frame.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }), areas[3]);
                let footer = Layout::vertical([Constraint::Length(2), Constraint::Length(2)]).split(areas[4]);
                frame.render_widget(Paragraph::new(&*status).wrap(Wrap { trim: false }), footer[0]);
                let controls = match discovery {
                    DiscoveryState::Preview(_) => "Enter register → Catalog · e edit URL\nEsc/Ctrl+C cancel without saving",
                    DiscoveryState::Fetching(_) => "Loading… Esc/Ctrl+C cancel without saving",
                    DiscoveryState::Editing => "Enter discover/retry · Ctrl+U clear URL\nEsc/Ctrl+C cancel without saving",
                };
                frame.render_widget(Paragraph::new(controls).wrap(Wrap { trim: false }), footer[1]);
            })
            .map_err(|error| Error::new(error.to_string()))?;
        if let DiscoveryState::Fetching(receiver) = &discovery {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err(Error::new("gateway discovery worker ended")))
                }
            };
            if let Some(result) = result {
                discovery = match result {
                    Ok(gateway) => {
                        status = "Review the gateway before registering".to_string();
                        DiscoveryState::Preview(gateway)
                    }
                    Err(error) => {
                        status = error.to_string();
                        DiscoveryState::Editing
                    }
                };
                continue;
            }
        }
        if !event::poll(Duration::from_millis(50)).map_err(|error| Error::new(error.to_string()))? {
            continue;
        }
        let event = event::read().map_err(|error| Error::new(error.to_string()))?;
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            if key.code == KeyCode::Esc
                || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
            {
                return Ok(None);
            }
            match (&discovery, key.code) {
                (DiscoveryState::Preview(gateway), KeyCode::Enter) => {
                    match gateway.register(state, registry) {
                        Ok(registration) => return Ok(Some(registration)),
                        Err(error) => status = error.to_string(),
                    }
                }
                (DiscoveryState::Preview(_), KeyCode::Char('e')) => {
                    discovery = DiscoveryState::Editing;
                    status = "Edit the base URL and press Enter to discover again".to_string();
                }
                (DiscoveryState::Editing, KeyCode::Enter) if !input.is_blank() => {
                    let url = input.contents();
                    let (sender, receiver) = mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = sender.send(harness::discover_gateway(&url));
                    });
                    status = "Discovering gateway… Esc cancels without saving".to_string();
                    discovery = DiscoveryState::Fetching(receiver);
                }
                (DiscoveryState::Editing, KeyCode::Char('u'))
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    input.clear()
                }
                (DiscoveryState::Editing, code) => {
                    let editor_key = match code {
                        KeyCode::Left => Some(EditorKey::Left),
                        KeyCode::Right => Some(EditorKey::Right),
                        KeyCode::Home => Some(EditorKey::Home),
                        KeyCode::End => Some(EditorKey::End),
                        KeyCode::Backspace => Some(EditorKey::Backspace),
                        KeyCode::Delete => Some(EditorKey::Delete),
                        KeyCode::Char(character)
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                                && !character.is_control()
                                && input.contents().len() + character.len_utf8() <= 8192 =>
                        {
                            Some(EditorKey::Character(character))
                        }
                        _ => None,
                    };
                    if let Some(editor_key) = editor_key {
                        input.apply(
                            editor_key,
                            terminal
                                .size()
                                .map_err(|error| Error::new(error.to_string()))?
                                .width as usize,
                        );
                    }
                }
                _ => {}
            }
        } else if let Event::Paste(text) = event {
            if matches!(discovery, DiscoveryState::Editing) {
                let text = text.trim();
                if input.contents().len() + text.len() <= 8192
                    && !text.chars().any(char::is_control)
                {
                    input.insert_paste(text);
                } else {
                    status = "Paste a single URL without control characters (maximum 8192 bytes)"
                        .to_string();
                }
            }
        }
    }
}
