//! Conversation screen shown after the model picker.
//!
//! Part 1 draws the screen and answers through `EchoBackend`. That backend
//! echoes the operator's text and reports synthetic token counts. It does not
//! call a model and it does not use the network.
//!
//! Terminal drawing stays in this module. Key codes are translated by
//! `key_binding` before the screen applies them.

mod backend;
mod command;
mod input;
mod key_binding;
mod location;
mod status;
mod thinking;
mod transcript;
mod usage;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use crate::error::{Error, Result};

use crossterm::event::{Event, KeyEvent};
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use backend::{
    parse_selection, ChatRequest, CompletionBackend, EchoBackend, ResponseEvent, Selection,
};
use command::ParsedCommand;
use input::InputEditor;
use key_binding::KeyAction;
use thinking::ThinkingLevel;
use transcript::{ChatMessage, TranscriptRole};
use usage::SessionMeters;

const FRAME_INTERVAL: Duration = Duration::from_millis(50);
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const ACCENT: Color = Color::Cyan;

/// How a key changed the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenAction {
    Continue,
    Exit,
    ReopenModelPicker,
}

/// Why the chat terminal loop returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEnd {
    Exit,
    ReopenModelPicker,
}

/// Screen state, independent of the terminal.
struct ChatScreen {
    state_dir: PathBuf,
    selection: Selection,
    thinking: ThinkingLevel,
    messages: Vec<ChatMessage>,
    input: InputEditor,
    location_os_icon: &'static str,
    location_directory: String,
    location_git: Option<location::GitStatus>,
    meters: SessionMeters,
    response_in_progress: bool,
    receiver: Option<Receiver<ResponseEvent>>,
    assistant_open: bool,
    scroll_from_bottom: usize,
    transcript_width: usize,
    transcript_height: usize,
    spinner_frame: usize,
}

impl ChatScreen {
    fn load(state_dir: &Path, selection: &str) -> Self {
        let (thinking, notice) = thinking::read(state_dir);
        let mut screen = Self {
            state_dir: state_dir.to_path_buf(),
            selection: parse_selection(selection),
            thinking,
            messages: Vec::new(),
            input: InputEditor::new(),
            location_os_icon: location::os_icon(),
            location_directory: String::new(),
            location_git: None,
            meters: SessionMeters::default(),
            response_in_progress: false,
            receiver: None,
            assistant_open: false,
            scroll_from_bottom: 0,
            transcript_width: 80,
            transcript_height: 1,
            spinner_frame: 0,
        };
        screen.refresh_location();
        if let Some(notice) = notice {
            screen.push_notice(notice);
        }
        screen
    }

    #[cfg(test)]
    fn thinking(&self) -> ThinkingLevel {
        self.thinking
    }

    fn set_selection(&mut self, selection: &str) {
        self.selection = parse_selection(selection);
    }

    #[cfg(test)]
    fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    #[cfg(test)]
    fn input_text(&self) -> String {
        self.input.contents()
    }

    #[cfg(test)]
    fn meters(&self) -> &SessionMeters {
        &self.meters
    }

    #[cfg(test)]
    fn set_meters(&mut self, meters: SessionMeters) {
        self.meters = meters;
    }

    #[cfg(test)]
    fn response_in_progress(&self) -> bool {
        self.response_in_progress
    }

    #[cfg(test)]
    fn set_input_text(&mut self, text: &str) {
        self.input = InputEditor::from_text(text);
    }

    fn apply_key(
        &mut self,
        key: KeyEvent,
        backend: &mut dyn CompletionBackend,
    ) -> Result<ScreenAction> {
        self.apply_action(key_binding::translate(key), backend)
    }

    fn apply_action(
        &mut self,
        action: KeyAction,
        backend: &mut dyn CompletionBackend,
    ) -> Result<ScreenAction> {
        match action {
            KeyAction::Ignore | KeyAction::Cancel => Ok(ScreenAction::Continue),
            KeyAction::Submit => self.submit_input(backend),
            KeyAction::ClearInputOrExit => {
                if self.input.is_blank() {
                    Ok(ScreenAction::Exit)
                } else {
                    self.input.clear();
                    Ok(ScreenAction::Continue)
                }
            }
            KeyAction::ExitIfInputEmpty => {
                if self.input.is_blank() {
                    Ok(ScreenAction::Exit)
                } else {
                    Ok(ScreenAction::Continue)
                }
            }
            KeyAction::CycleThinkingLevel => {
                self.thinking = self.thinking.next();
                thinking::write(&self.state_dir, self.thinking)?;
                Ok(ScreenAction::Continue)
            }
            KeyAction::ScrollTowardOlder => {
                let page = self.transcript_height.max(1);
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(page);
                self.clamp_scroll();
                Ok(ScreenAction::Continue)
            }
            KeyAction::ScrollTowardNewer => {
                let page = self.transcript_height.max(1);
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(page);
                Ok(ScreenAction::Continue)
            }
            KeyAction::Edit(key) => {
                self.input.apply(key);
                Ok(ScreenAction::Continue)
            }
        }
    }

    fn poll_backend(&mut self) {
        let Some(receiver) = self.receiver.take() else {
            return;
        };
        loop {
            match receiver.try_recv() {
                Ok(event) => {
                    let finished = self.apply_event(event);
                    if finished {
                        self.response_in_progress = false;
                        self.assistant_open = false;
                        return;
                    }
                }
                Err(TryRecvError::Empty) => {
                    self.receiver = Some(receiver);
                    return;
                }
                Err(TryRecvError::Disconnected) => {
                    self.response_in_progress = false;
                    self.assistant_open = false;
                    return;
                }
            }
        }
    }

    fn draw(&mut self, frame: &mut ratatui::Frame) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
        let input_rows = self.input.visible_row_count() as u16;
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(input_rows),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(frame.area());
        self.transcript_width = chunks[0].width as usize;
        self.transcript_height = chunks[0].height as usize;
        self.clamp_scroll();
        let lines = transcript::wrapped_lines(&self.messages, self.transcript_width);
        let visible =
            transcript::visible_text(&lines, self.transcript_height, self.scroll_from_bottom);
        frame.render_widget(Paragraph::new(visible), chunks[0]);

        let activity = if self.response_in_progress {
            let frame_index = self.spinner_frame % SPINNER.len();
            format!("{} Working", SPINNER[frame_index])
        } else {
            String::new()
        };
        frame.render_widget(
            Paragraph::new(activity).style(Style::default().add_modifier(Modifier::DIM)),
            chunks[1],
        );

        let context_rule = status::render_context_rule(chunks[2].width as usize, &self.meters);
        let accent = Style::default().fg(ACCENT);
        frame.render_widget(Paragraph::new(context_rule).style(accent), chunks[2]);
        let (scroll, input_lines) = self.input.visible_lines();
        frame.render_widget(Paragraph::new(input_lines.join("\n")), chunks[3]);
        let cursor_line = self.input.cursor_line().saturating_sub(scroll);
        let cursor_column = self
            .input
            .cursor_column()
            .min(chunks[3].width.saturating_sub(1) as usize);
        frame.set_cursor_position((
            chunks[3].x + cursor_column as u16,
            chunks[3].y + cursor_line as u16,
        ));
        frame.render_widget(
            Paragraph::new(rule_line(chunks[4].width as usize)).style(accent),
            chunks[4],
        );

        let model = Line::styled(
            status::session_model_segment(&self.selection, self.thinking),
            Style::default()
                .fg(Color::Indexed(103))
                .add_modifier(Modifier::DIM),
        );
        // Reserve the model first, including a gap from the workspace.
        // Context lives in the input rule and cannot move the model sideways.
        let model_width = model
            .width()
            .saturating_add(1)
            .min(chunks[5].width as usize) as u16;
        let top = Layout::horizontal([Constraint::Min(0), Constraint::Length(model_width)])
            .split(chunks[5]);
        let location = status::render_location_row(
            top[0].width as usize,
            self.location_os_icon,
            &self.location_directory,
            self.location_git.as_ref(),
        );
        frame.render_widget(Paragraph::new(location), top[0]);
        frame.render_widget(Paragraph::new(model).alignment(Alignment::Right), top[1]);
    }

    fn submit_input(&mut self, backend: &mut dyn CompletionBackend) -> Result<ScreenAction> {
        let text = self.input.contents();
        if text.trim().is_empty() {
            return Ok(ScreenAction::Continue);
        }
        self.input.clear();
        if !text.contains('\n') && text.trim_start().starts_with('/') {
            return self.dispatch_command(text.trim());
        }
        self.messages.push(ChatMessage {
            role: TranscriptRole::Operator,
            text,
        });
        self.scroll_from_bottom = 0;
        self.refresh_location();
        let request = ChatRequest {
            selection: self.selection.clone(),
            thinking: self.thinking,
            messages: self
                .messages
                .iter()
                .filter(|message| message.role != TranscriptRole::Notice)
                .cloned()
                .collect(),
        };
        self.receiver = Some(backend.submit(request));
        self.response_in_progress = true;
        self.assistant_open = false;
        Ok(ScreenAction::Continue)
    }

    fn dispatch_command(&mut self, text: &str) -> Result<ScreenAction> {
        match command::parse(text) {
            ParsedCommand::ReopenModelPicker => Ok(ScreenAction::ReopenModelPicker),
            ParsedCommand::Exit => Ok(ScreenAction::Exit),
            ParsedCommand::Clear => {
                self.messages.clear();
                self.meters.clear_counts();
                self.scroll_from_bottom = 0;
                self.receiver = None;
                self.response_in_progress = false;
                self.assistant_open = false;
                Ok(ScreenAction::Continue)
            }
            ParsedCommand::ShowThinking => {
                let level = self.thinking.as_str();
                self.push_notice(format!("thinking {level}"));
                Ok(ScreenAction::Continue)
            }
            ParsedCommand::SetThinking(level) => {
                self.thinking = level;
                thinking::write(&self.state_dir, level)?;
                Ok(ScreenAction::Continue)
            }
            ParsedCommand::UnknownThinkingLevel { level } => {
                self.push_notice(format!("unknown thinking level {level}"));
                Ok(ScreenAction::Continue)
            }
            ParsedCommand::Unknown { name } => {
                self.push_notice(format!("unknown command {name}"));
                Ok(ScreenAction::Continue)
            }
        }
    }

    fn apply_event(&mut self, event: ResponseEvent) -> bool {
        match event {
            ResponseEvent::TextDelta(text) => {
                if !self.assistant_open {
                    self.messages.push(ChatMessage {
                        role: TranscriptRole::Assistant,
                        text: String::new(),
                    });
                    self.assistant_open = true;
                }
                if let Some(message) = self.messages.last_mut() {
                    message.text.push_str(&text);
                }
                false
            }
            ResponseEvent::Usage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                cost_usd,
            } => {
                self.meters.record_usage(
                    input_tokens,
                    output_tokens,
                    cached_input_tokens,
                    cost_usd,
                );
                false
            }
            ResponseEvent::ContextWindow(window) => {
                self.meters.set_context_window(window);
                false
            }
            ResponseEvent::ToolCallReturned => {
                // A tool may change HEAD even when it reports an error.
                // Refresh once per return, never on the rendering timer.
                self.refresh_location();
                false
            }
            ResponseEvent::Completed => {
                transcript::attach_stand_in_note(&mut self.messages);
                true
            }
            ResponseEvent::Failed(message) => {
                self.push_notice(message);
                true
            }
        }
    }

    fn push_notice(&mut self, text: String) {
        self.messages.push(ChatMessage {
            role: TranscriptRole::Notice,
            text,
        });
    }

    fn refresh_location(&mut self) {
        let (directory, git_status) = location::current();
        if self.location_directory != directory || self.location_git != git_status {
            self.location_directory = directory;
            self.location_git = git_status;
        }
    }

    fn clamp_scroll(&mut self) {
        let lines = transcript::wrapped_lines(&self.messages, self.transcript_width.max(1));
        transcript::clamp_scroll(
            &mut self.scroll_from_bottom,
            lines.len(),
            self.transcript_height,
        );
    }
}

fn rule_line(width: usize) -> String {
    "─".repeat(width)
}

/// Run the chat screen until the operator exits or asks for the model picker.
pub(crate) fn interact(
    state_dir: &Path,
    selection: &str,
    mut reopen_picker: impl FnMut() -> Result<Option<String>>,
) -> Result<()> {
    let mut screen = ChatScreen::load(state_dir, selection);
    let mut backend = EchoBackend::new();
    loop {
        let mut terminal =
            ratatui::try_init().map_err(|err| Error::new(format!("terminal: {err}")))?;
        let outcome = session_loop(&mut terminal, &mut screen, &mut backend);
        ratatui::restore();
        match outcome? {
            SessionEnd::Exit => return Ok(()),
            SessionEnd::ReopenModelPicker => {
                if let Some(next) = reopen_picker()? {
                    screen.set_selection(&next);
                }
            }
        }
    }
}

fn session_loop(
    terminal: &mut ratatui::DefaultTerminal,
    screen: &mut ChatScreen,
    backend: &mut EchoBackend,
) -> Result<SessionEnd> {
    loop {
        terminal
            .draw(|frame| screen.draw(frame))
            .map_err(|err| Error::new(format!("terminal draw: {err}")))?;
        if crossterm::event::poll(FRAME_INTERVAL).map_err(|err| Error::new(err.to_string()))? {
            let event = crossterm::event::read().map_err(|err| Error::new(err.to_string()))?;
            if let Event::Key(key) = event {
                match screen.apply_key(key, backend)? {
                    ScreenAction::Continue => {}
                    ScreenAction::Exit => return Ok(SessionEnd::Exit),
                    ScreenAction::ReopenModelPicker => {
                        return Ok(SessionEnd::ReopenModelPicker);
                    }
                }
            }
        }
        screen.poll_backend();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::transcript::STAND_IN_NOTE;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::fs;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::Duration;

    struct RecordingBackend {
        requests: Vec<ChatRequest>,
    }

    impl CompletionBackend for RecordingBackend {
        fn submit(&mut self, request: ChatRequest) -> Receiver<ResponseEvent> {
            self.requests.push(request);
            let (sender, receiver) = mpsc::channel();
            let _ = sender.send(ResponseEvent::Completed);
            receiver
        }
    }

    fn screen_in(state: &Path) -> ChatScreen {
        ChatScreen::load(state, "openai/gpt-4.1")
    }

    #[test]
    fn submit_sends_the_input_and_a_newline_edit_does_not() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-keys");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.set_input_text("   ");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert!(backend.requests.is_empty());
        assert_eq!(screen.input_text(), "   ");

        screen.set_input_text("explain");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert_eq!(backend.requests.len(), 1);
        assert_eq!(backend.requests[0].messages[0].text, "explain");
        assert!(screen.input_text().is_empty());

        screen.set_input_text("one");
        screen
            .apply_action(KeyAction::Edit(input::EditorKey::Newline), &mut backend)
            .unwrap();
        assert_eq!(screen.input_text(), "one\n");
        assert_eq!(backend.requests.len(), 1);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn clear_exit_scroll_and_thinking_actions_change_the_screen() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-actions");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.set_input_text("draft");
        let action = screen
            .apply_action(KeyAction::ClearInputOrExit, &mut backend)
            .unwrap();
        assert_eq!(action, ScreenAction::Continue);
        assert!(screen.input_text().is_empty());
        let action = screen
            .apply_action(KeyAction::ClearInputOrExit, &mut backend)
            .unwrap();
        assert_eq!(action, ScreenAction::Exit);

        screen.set_input_text("draft");
        let action = screen
            .apply_action(KeyAction::ExitIfInputEmpty, &mut backend)
            .unwrap();
        assert_eq!(action, ScreenAction::Continue);
        assert_eq!(screen.input_text(), "draft");
        screen.input.clear();
        let action = screen
            .apply_action(KeyAction::ExitIfInputEmpty, &mut backend)
            .unwrap();
        assert_eq!(action, ScreenAction::Exit);

        screen.transcript_height = 1;
        for text in ["one", "two", "three"] {
            screen.messages.push(ChatMessage {
                role: TranscriptRole::Notice,
                text: text.to_string(),
            });
        }
        screen
            .apply_action(KeyAction::ScrollTowardOlder, &mut backend)
            .unwrap();
        assert_eq!(screen.scroll_from_bottom, 1);
        screen
            .apply_action(KeyAction::ScrollTowardNewer, &mut backend)
            .unwrap();
        assert_eq!(screen.scroll_from_bottom, 0);

        assert_eq!(screen.thinking(), ThinkingLevel::Medium);
        screen
            .apply_action(KeyAction::CycleThinkingLevel, &mut backend)
            .unwrap();
        assert_eq!(screen.thinking(), ThinkingLevel::High);
        assert_eq!(
            fs::read_to_string(state.join("thinking")).unwrap().trim(),
            "high"
        );
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn commands_run_without_a_terminal() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-commands");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };

        screen.set_input_text("/thinking high");
        let action = screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert_eq!(action, ScreenAction::Continue);
        assert_eq!(screen.thinking(), ThinkingLevel::High);
        assert_eq!(
            fs::read_to_string(state.join("thinking")).unwrap().trim(),
            "high"
        );

        screen.set_input_text("/thinking");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert!(screen
            .messages()
            .iter()
            .any(|message| message.text == "thinking high"));

        screen.messages.push(ChatMessage {
            role: TranscriptRole::Operator,
            text: "kept".to_string(),
        });
        screen.meters.input_tokens = Some(12);
        screen.set_input_text("/clear");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert!(screen.messages().is_empty());
        assert_eq!(screen.meters().input_tokens, None);

        screen.set_input_text("/no-such");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert!(screen
            .messages()
            .iter()
            .any(|message| message.text == "unknown command /no-such"));
        assert!(backend.requests.is_empty());

        screen.set_input_text("/model");
        let action = screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert_eq!(action, ScreenAction::ReopenModelPicker);

        screen.set_input_text("/exit");
        let action = screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert_eq!(action, ScreenAction::Exit);
        let _ = fs::remove_dir_all(state);
    }

    fn clean_main() -> location::GitStatus {
        location::GitStatus {
            label: "main".to_string(),
            changes: Some(location::GitChanges::default()),
        }
    }

    #[test]
    fn drawn_screen_has_rules_activity_and_an_unframed_input() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-draw");
        let mut screen = screen_in(&state);
        screen.set_selection("openai/gpt-6.1-sol");
        screen.location_os_icon = "";
        screen.location_directory = "~/Work/gh/ahi".to_string();
        screen.location_git = Some(clean_main());
        screen.set_meters(SessionMeters {
            input_tokens: Some(139_000),
            output_tokens: Some(11_000),
            cached_input_tokens: Some(1_200_000),
            cost_usd: Some(0.502),
            subscription_sign_in: true,
            context_tokens: Some(77_248),
            context_window: Some(272_000),
            automatic_compaction: true,
        });
        screen.messages.push(ChatMessage {
            role: TranscriptRole::Operator,
            text: "explain the hook runner".to_string(),
        });
        let idle = draw_lines(&mut screen, 120, 16);
        let activity = &idle[idle.len() - 5];
        assert!(activity.trim().is_empty(), "{activity}");
        assert!(idle[idle.len() - 4].ends_with(" 28.4% / 272k ──"));
        assert!(idle[idle.len() - 2]
            .chars()
            .all(|character| character == '─'));
        assert!(!idle[idle.len() - 3].starts_with('>'));
        assert!(idle
            .iter()
            .all(|line| !line.contains('│') && !line.contains('┌')));
        assert!(idle
            .last()
            .unwrap()
            .starts_with("  ~/Work/gh/ahi on   main"));
        assert!(idle
            .last()
            .unwrap()
            .ends_with("(openai) gpt-6.1-sol • medium"));
        assert!(!idle
            .iter()
            .any(|row| row.contains("↑139k") || row.contains("R1.2M") || row.contains("$0.502")));

        screen.response_in_progress = true;
        let working = draw_lines(&mut screen, 120, 16);
        assert!(
            working[working.len() - 5].contains("Working"),
            "{}",
            working[working.len() - 5]
        );
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn status_colors_separate_workspace_model_and_input_context() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-status-style");
        let mut screen = screen_in(&state);
        screen.set_selection("openai/gpt-6.1-sol");
        screen.location_os_icon = "";
        screen.location_directory = "~/Work/gh/ahi".to_string();
        screen.location_git = Some(location::GitStatus {
            label: "main".to_string(),
            changes: Some(location::GitChanges {
                unstaged: 6,
                ..location::GitChanges::default()
            }),
        });
        screen.set_meters(SessionMeters {
            input_tokens: Some(5),
            output_tokens: Some(5),
            context_tokens: Some(10),
            context_window: Some(128_000),
            ..SessionMeters::default()
        });
        for width in [80, 120] {
            let mut terminal = Terminal::new(TestBackend::new(width, 16)).unwrap();
            terminal.draw(|frame| screen.draw(frame)).unwrap();
            let buffer = terminal.backend().buffer();
            let location_text: String = (0..width).map(|x| buffer[(x, 15)].symbol()).collect();
            assert!(location_text.starts_with("  ~/Work/gh/ahi on   main !6"));
            for (prefix, expected_color) in [
                ("", Color::Reset),
                (" ", Color::Indexed(31)),
                ("  ~/Work/gh/", Color::Indexed(39)),
                ("  ~/Work/gh/ahi on ", Color::Indexed(76)),
                ("  ~/Work/gh/ahi on   ", Color::Indexed(76)),
                ("  ~/Work/gh/ahi on   main ", Color::Indexed(178)),
            ] {
                let cell = &buffer[(prefix.chars().count() as u16, 15)];
                assert_eq!(cell.fg, expected_color);
                assert!(!cell.modifier.contains(Modifier::DIM));
            }
            let rule_text: String = (0..width).map(|x| buffer[(x, 12)].symbol()).collect();
            assert!(rule_text.ends_with(" 0% / 128k ──"));
            let context_start = width - "0% / 128k ──".chars().count() as u16;
            assert_eq!(buffer[(context_start, 12)].symbol(), "0");
            assert_eq!(buffer[(context_start, 12)].fg, Color::Indexed(103));
            assert!(buffer[(context_start, 12)].modifier.contains(Modifier::DIM));
            assert_eq!(buffer[(width - 1, 12)].fg, ACCENT);
            assert!(!buffer[(width - 1, 12)].modifier.contains(Modifier::DIM));
            let expected = "(openai) gpt-6.1-sol • medium";
            let start = width - expected.chars().count() as u16;
            for (offset, character) in expected.chars().enumerate() {
                let cell = &buffer[(start + offset as u16, 15)];
                assert_eq!(cell.symbol(), character.to_string());
                assert_eq!(
                    cell.fg,
                    Color::Indexed(103),
                    "model character {character:?} has wrong color at width {width}"
                );
                assert!(cell.modifier.contains(Modifier::DIM));
            }
        }
        fs::remove_dir_all(state).unwrap();
    }

    fn draw_lines(screen: &mut ChatScreen, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn narrow_footer_reserves_model_with_context_in_the_input_rule() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-narrow-footer");
        let mut screen = screen_in(&state);
        screen.set_selection("openai/gpt-6.1-sol");
        screen.location_os_icon = "";
        screen.location_directory = "~/Work/gh/ahi".to_string();
        screen.location_git = Some(clean_main());
        screen.set_meters(SessionMeters {
            input_tokens: Some(5),
            output_tokens: Some(5),
            context_tokens: Some(10),
            context_window: Some(128_000),
            ..SessionMeters::default()
        });
        let rows = draw_lines(&mut screen, 40, 16);
        assert_eq!(rows[15], "…   main (openai) gpt-6.1-sol • medium");
        assert!(rows[12].ends_with(" 0% / 128k ──"));
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn tool_return_does_not_end_or_split_the_assistant_response() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-tool-return");
        let mut screen = screen_in(&state);
        assert!(!screen.apply_event(ResponseEvent::TextDelta("before".to_string())));
        assert!(!screen.apply_event(ResponseEvent::ToolCallReturned));
        assert!(!screen.apply_event(ResponseEvent::TextDelta(" after".to_string())));
        assert_eq!(
            screen
                .messages
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["before after"]
        );
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn echo_backend_streams_usage_then_completion_and_notes_the_first_reply() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-echo");
        let mut screen = screen_in(&state);
        screen.set_input_text("hello!!!!");
        let mut live = EchoBackend::new();
        screen.apply_action(KeyAction::Submit, &mut live).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while screen.response_in_progress() && std::time::Instant::now() < deadline {
            screen.poll_backend();
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!screen.response_in_progress());
        let assistant = screen
            .messages()
            .iter()
            .find(|message| message.role == TranscriptRole::Assistant)
            .unwrap();
        assert!(assistant.text.starts_with("[stand-in] hello!!!!"));
        assert!(assistant.text.contains(STAND_IN_NOTE));
        assert_eq!(screen.meters().input_tokens, Some(2));
        assert_eq!(screen.meters().cached_input_tokens, None);
        assert_eq!(screen.meters().cost_usd, None);
        assert_eq!(screen.meters().context_window, Some(128_000));

        screen.set_input_text("/clear");
        screen.apply_action(KeyAction::Submit, &mut live).unwrap();
        screen.set_input_text("again!!!");
        screen.apply_action(KeyAction::Submit, &mut live).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while screen.response_in_progress() && std::time::Instant::now() < deadline {
            screen.poll_backend();
            thread::sleep(Duration::from_millis(5));
        }
        let assistant = screen
            .messages()
            .iter()
            .find(|message| message.role == TranscriptRole::Assistant)
            .unwrap();
        assert!(assistant.text.contains(STAND_IN_NOTE));
        let _ = fs::remove_dir_all(state);
    }
}
