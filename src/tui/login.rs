use std::time::Duration;

use ahi_widgets::{Block, Borders, Paragraph, Wrap};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use ratatui_core::layout::{Constraint, Layout};

use crate::directories::Directories;
use crate::error::{Error, Result};
use crate::harness::OpenAiLogin;

pub(crate) fn run(state: &Directories) -> Result<()> {
    let mut terminal = crate::term::try_init().map_err(|error| Error::new(error.to_string()))?;
    let result = crossterm::execute!(std::io::stdout(), EnableBracketedPaste)
        .map_err(|error| Error::new(error.to_string()))
        .and_then(|()| login_loop(&mut terminal, state));
    let cleanup = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    crate::term::restore();
    cleanup.map_err(|error| Error::new(error.to_string()))?;
    result
}

fn login_loop(terminal: &mut crate::term::DefaultTerminal, state: &Directories) -> Result<()> {
    let mut login = OpenAiLogin::start(&state.data)?;
    let mut callback = String::new();
    let mut status =
        "Waiting for browser login. Paste the full callback URL if needed.".to_string();
    let mut scroll = 0_u16;
    let mut browser_opened = false;
    loop {
        terminal
            .draw(|frame| {
                let areas = Layout::vertical([Constraint::Min(1), Constraint::Length(5)])
                    .split(frame.area());
                frame.render_widget(
                    Paragraph::new(format!(
                        "Open this URL to sign in with ChatGPT:\n\n{}\n\nThe browser redirects to http://127.0.0.1:1455/auth/callback.\nLogin does not refresh catalogs or enable OpenAI inference.",
                        login.authorization_url()
                    ))
                    .block(Block::default().borders(Borders::ALL).title(" OpenAI login "))
                    .wrap(Wrap { trim: false })
                    .scroll((scroll, 0)),
                    areas[0],
                );
                frame.render_widget(
                    Paragraph::new(format!(
                        "{status}\nCallback: {} characters (hidden)\nEnter submit  Ctrl+U clear  Up/Down scroll URL  Esc/Ctrl+C cancel",
                        callback.chars().count()
                    ))
                    .wrap(Wrap { trim: false }),
                    areas[1],
                );
            })
            .map_err(|error| Error::new(error.to_string()))?;
        if !browser_opened {
            login.open_browser();
            browser_opened = true;
        }
        if let Some(result) = login.poll() {
            return result;
        }
        if !event::poll(Duration::from_millis(50)).map_err(|error| Error::new(error.to_string()))? {
            continue;
        }
        match event::read().map_err(|error| Error::new(error.to_string()))? {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => return Err(Error::new("OpenAI login cancelled")),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(Error::new("OpenAI login cancelled"));
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    callback.clear();
                }
                KeyCode::Enter if !callback.trim().is_empty() => {
                    match login.submit_callback(&callback) {
                        Ok(()) => status = "Completing OpenAI login…".to_string(),
                        Err(error) => status = error.to_string(),
                    }
                    callback.clear();
                }
                KeyCode::Backspace => {
                    callback.pop();
                }
                KeyCode::Up => scroll = scroll.saturating_sub(1),
                KeyCode::Down => scroll = scroll.saturating_add(1),
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && !character.is_control()
                        && callback.len() + character.len_utf8() <= 8192 =>
                {
                    callback.push(character);
                }
                _ => {}
            },
            Event::Paste(text) => {
                if callback.len() + text.len() <= 8192 {
                    callback.push_str(text.trim());
                } else {
                    status = "Callback URL exceeds 8192 bytes; clear and paste again".to_string();
                }
            }
            _ => {}
        }
    }
}
