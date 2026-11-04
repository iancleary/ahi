//! Conversation screen shown after the model picker.
//!
//! Registered gateways stream real model responses through `HttpBackend`.
//! OpenAI browser-login inference and tool execution are not implemented yet.
//!
//! Terminal drawing stays in this module. Key codes are translated by
//! `key_binding` before the screen applies them.

mod backend;
mod command;
mod input;
mod key_binding;
mod location;
mod scroll_navigation;
mod status;
mod thinking;
mod transcript;
mod usage;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use crate::error::{Error, Result};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    MouseEventKind,
};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use backend::{
    parse_selection, ChatRequest, CompletionBackend, HttpBackend, ResponseEvent, Selection,
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
    scroll_navigation: scroll_navigation::ScrollNavigation,
    scroll_from_bottom: usize,
    transcript_area: Rect,
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
            scroll_navigation: scroll_navigation::ScrollNavigation::default(),
            scroll_from_bottom: 0,
            transcript_area: Rect::default(),
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

    fn apply_terminal_event(
        &mut self,
        event: Event,
        backend: &mut dyn CompletionBackend,
    ) -> Result<ScreenAction> {
        if let Event::Mouse(mouse) = event {
            if let Some(action) = self.scroll_navigation.mouse(mouse) {
                return self.apply_action(action, backend);
            }
        }
        match event {
            Event::Key(key) => self.apply_action(key_binding::translate(key), backend),
            Event::Mouse(mouse)
                if self
                    .transcript_area
                    .contains(Position::new(mouse.column, mouse.row)) =>
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(3);
                    }
                    MouseEventKind::ScrollDown => {
                        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(3);
                    }
                    _ => return Ok(ScreenAction::Continue),
                }
                self.clamp_scroll();
                Ok(ScreenAction::Continue)
            }
            Event::Paste(text) => {
                self.input.insert_paste(&text);
                Ok(ScreenAction::Continue)
            }
            _ => Ok(ScreenAction::Continue),
        }
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
            KeyAction::JumpToTop => {
                self.scroll_from_bottom = usize::MAX;
                self.clamp_scroll();
                Ok(ScreenAction::Continue)
            }
            KeyAction::JumpToBottom => {
                self.scroll_from_bottom = 0;
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
                self.input.apply(key, self.transcript_width);
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
        let input = self.input.viewport(
            frame.area().width as usize,
            frame.area().height.saturating_sub(3) as usize,
        );
        let input_rows = input.lines.len() as u16;
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(input_rows),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(frame.area());
        self.transcript_area = chunks[0];
        self.transcript_width = chunks[0].width as usize;
        self.transcript_height = chunks[0].height as usize;
        self.clamp_scroll();
        let lines = transcript::render::wrapped_lines(&self.messages, self.transcript_width);
        let visible = transcript::render::visible_text(
            &lines,
            self.transcript_height,
            self.scroll_from_bottom,
        );
        frame.render_widget(Paragraph::new(visible), chunks[0]);
        let max_scroll = lines.len().saturating_sub(self.transcript_height);
        self.scroll_navigation.draw(
            frame,
            chunks[0],
            self.scroll_from_bottom < max_scroll,
            self.scroll_from_bottom > 0,
        );

        let activity = if self.response_in_progress {
            let spinner = SPINNER[self.spinner_frame % SPINNER.len()];
            if chunks[1].width < 12 {
                format!("{spinner} ")
            } else {
                format!("{spinner} Working ")
            }
        } else {
            String::new()
        };
        let activity_width = (activity.chars().count() as u16).min(chunks[1].width);
        let header = Layout::horizontal([Constraint::Length(activity_width), Constraint::Min(0)])
            .split(chunks[1]);
        let accent = Style::default().fg(ACCENT);
        frame.render_widget(Paragraph::new(activity).style(accent), header[0]);
        let context_rule = status::render_context_rule(header[1].width as usize, &self.meters);
        frame.render_widget(Paragraph::new(context_rule).style(accent), header[1]);
        frame.render_widget(Paragraph::new(input.lines.join("\n")), chunks[2]);
        if chunks[2].width > 0 && chunks[2].height > 0 {
            frame.set_cursor_position((
                chunks[2].x + input.cursor_column.min(chunks[2].width as usize - 1) as u16,
                chunks[2].y + input.cursor_row.min(chunks[2].height as usize - 1) as u16,
            ));
        }
        frame.render_widget(
            Paragraph::new(rule_line(chunks[3].width as usize)).style(accent),
            chunks[3],
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
            .min(chunks[4].width as usize) as u16;
        let top = Layout::horizontal([Constraint::Min(0), Constraint::Length(model_width)])
            .split(chunks[4]);
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
        // Keep the draft intact until the active turn ends; never launch a second
        // billed request while discarding the first turn's receiver.
        if self.response_in_progress {
            return Ok(ScreenAction::Continue);
        }
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
        // A nonzero offset means the reader has left live-follow mode. Grow
        // that offset with the transcript so the same rows remain visible.
        // Count wrapped rows, not newlines: streamed text can soft-wrap too.
        let previous_rows = (self.scroll_from_bottom > 0).then(|| {
            transcript::render::wrapped_lines(&self.messages, self.transcript_width.max(1)).len()
        });
        let finished = match event {
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
            ResponseEvent::ToolCallsRequested(calls) => {
                for call in calls {
                    self.push_notice(format!(
                        "tool requested: {} ({}); not executed",
                        call.name, call.id
                    ));
                }
                // A notice is not part of the assistant's streamed text.
                self.assistant_open = false;
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
            ResponseEvent::ToolResult { output, exit_code } => {
                self.messages.push(ChatMessage {
                    role: TranscriptRole::Tool { exit_code },
                    text: output,
                });
                self.assistant_open = false;
                self.refresh_location();
                false
            }
            ResponseEvent::Completed => true,
            ResponseEvent::Failed(message) => {
                self.push_notice(message);
                true
            }
        };
        if let Some(previous_rows) = previous_rows {
            let current_rows =
                transcript::render::wrapped_lines(&self.messages, self.transcript_width.max(1))
                    .len();
            self.scroll_from_bottom = self
                .scroll_from_bottom
                .saturating_add(current_rows.saturating_sub(previous_rows));
        }
        finished
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
        let lines = transcript::render::wrapped_lines(&self.messages, self.transcript_width.max(1));
        transcript::render::clamp_scroll(
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
    let mut backend = HttpBackend::new(state_dir.to_path_buf());
    loop {
        let mut terminal =
            ratatui::try_init().map_err(|err| Error::new(format!("terminal: {err}")))?;
        let outcome =
            crossterm::execute!(std::io::stdout(), EnableBracketedPaste, EnableMouseCapture)
                .map_err(|err| Error::new(format!("enable paste: {err}")))
                .and_then(|()| session_loop(&mut terminal, &mut screen, &mut backend));
        let cleanup = crossterm::execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste
        );
        ratatui::restore();
        cleanup.map_err(|err| Error::new(format!("disable paste: {err}")))?;
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
    backend: &mut dyn CompletionBackend,
) -> Result<SessionEnd> {
    loop {
        terminal
            .draw(|frame| screen.draw(frame))
            .map_err(|err| Error::new(format!("terminal draw: {err}")))?;
        if crossterm::event::poll(FRAME_INTERVAL).map_err(|err| Error::new(err.to_string()))? {
            let event = crossterm::event::read().map_err(|err| Error::new(err.to_string()))?;
            match screen.apply_terminal_event(event, backend)? {
                ScreenAction::Continue => {}
                ScreenAction::Exit => return Ok(SessionEnd::Exit),
                ScreenAction::ReopenModelPicker => {
                    return Ok(SessionEnd::ReopenModelPicker);
                }
            }
        }
        screen.poll_backend();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::fs;
    use std::sync::mpsc::{self, Receiver};

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
    fn multiline_paste_stays_in_draft_until_enter() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let state = crate::fixture_directory::create_unique_fixture_directory("chat-paste");
        for pasted in ["α\nβ\n", "α\r\nβ\r\n", "α\rβ\r"] {
            let mut screen = screen_in(&state);
            let mut backend = RecordingBackend {
                requests: Vec::new(),
            };
            screen.set_input_text("beforeafter");
            for _ in 0..5 {
                screen
                    .apply_action(KeyAction::Edit(input::EditorKey::Left), &mut backend)
                    .unwrap();
            }
            assert_eq!(
                screen
                    .apply_terminal_event(Event::Paste(pasted.into()), &mut backend)
                    .unwrap(),
                ScreenAction::Continue
            );
            assert_eq!(screen.input_text(), "beforeα\nβ\nafter");
            assert!(backend.requests.is_empty());
            assert!(screen.messages().is_empty());
            screen
                .apply_terminal_event(
                    Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                    &mut backend,
                )
                .unwrap();
            assert_eq!(backend.requests.len(), 1);
            assert_eq!(backend.requests[0].messages[0].text, "beforeα\nβ\nafter");
            assert!(screen.input_text().is_empty());
        }
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
        assert!(working[working.len() - 4].starts_with("⠹ Working "));
        assert!(working[working.len() - 4].ends_with(" 28.4% / 272k ──"));
        let next = draw_lines(&mut screen, 120, 16);
        assert!(next[next.len() - 4].starts_with("⠸ Working "));
        screen.response_in_progress = false;
        let finished = draw_lines(&mut screen, 120, 16);
        assert!(finished[finished.len() - 4].starts_with("──"));
        assert!(!finished[finished.len() - 4].contains("Working"));
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
    fn tool_results_render_as_tinted_blocks_between_streamed_text() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-blocks");
        let mut screen = screen_in(&state);
        screen.messages.push(ChatMessage {
            role: TranscriptRole::Operator,
            text: "question".into(),
        });
        assert!(!screen.apply_event(ResponseEvent::TextDelta("before".into())));
        assert!(!screen.apply_event(ResponseEvent::ToolResult {
            output: "ok".into(),
            exit_code: 0,
        }));
        assert!(!screen.apply_event(ResponseEvent::TextDelta("after".into())));
        assert!(!screen.apply_event(ResponseEvent::TextDelta(" more".into())));
        assert!(!screen.apply_event(ResponseEvent::ToolResult {
            output: "failed (exit 2)".into(),
            exit_code: 2,
        }));
        assert_eq!(screen.messages[3].text, "after more");
        assert_eq!(screen.messages[1].text, "before");
        let mut terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        for (row, text, background) in [
            (0, "question", Color::Rgb(30, 45, 60)),
            (2, "before", Color::Reset),
            (4, "ok", Color::Rgb(30, 60, 45)),
            (6, "after more", Color::Reset),
            (8, "failed (exit 2)", Color::Rgb(60, 30, 30)),
        ] {
            let rendered: String = (0..text.len() as u16)
                .map(|x| buffer[(x, row)].symbol())
                .collect();
            assert_eq!(rendered, text);
            // The tint covers the entire row, not only its text.
            assert_eq!(buffer[(0, row)].bg, background);
            assert_eq!(buffer[(39, row)].bg, background);
        }
        for row in [1, 3, 5, 7] {
            assert_eq!(buffer[(0, row)].symbol(), " ");
            assert_eq!(buffer[(0, row)].bg, Color::Reset);
        }
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn streaming_keeps_scrollback_fixed_and_follows_again_at_the_bottom() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-stream-scroll");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.set_input_text("draft stays here");
        screen.apply_event(ResponseEvent::TextDelta(
            (0..30).map(|n| format!("line {n:02}\n")).collect(),
        ));
        let bottom = draw_lines(&mut screen, 40, 12);
        screen.apply_event(ResponseEvent::TextDelta("live output\n".into()));
        let following = draw_lines(&mut screen, 40, 12);
        assert_ne!(following[0], bottom[0]);
        assert_eq!(screen.scroll_from_bottom, 0);

        screen
            .apply_action(KeyAction::ScrollTowardOlder, &mut backend)
            .unwrap();
        let anchored = draw_lines(&mut screen, 40, 12);
        let height = screen.transcript_height;
        for event in [
            ResponseEvent::TextDelta("new line\n".into()),
            ResponseEvent::TextDelta("x".repeat(125)),
            ResponseEvent::ToolResult {
                output: "tool output\nmore output".into(),
                exit_code: 0,
            },
            ResponseEvent::TextDelta("continuing after tool\n".into()),
            ResponseEvent::Completed,
        ] {
            screen.apply_event(event);
            let rendered = draw_lines(&mut screen, 40, 12);
            assert_eq!(&rendered[..height], &anchored[..height]);
        }
        assert_eq!(screen.input_text(), "draft stays here");

        while screen.scroll_from_bottom > 0 {
            screen
                .apply_action(KeyAction::ScrollTowardNewer, &mut backend)
                .unwrap();
        }
        let bottom = draw_lines(&mut screen, 40, 12);
        screen.apply_event(ResponseEvent::TextDelta("latest line\n".into()));
        let following = draw_lines(&mut screen, 40, 12);
        assert_ne!(&following[..height], &bottom[..height]);
        assert_eq!(screen.scroll_from_bottom, 0);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn jump_controls_hover_click_and_keep_the_draft() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent};
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-jumps");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.set_input_text("keep draft");
        screen.apply_event(ResponseEvent::TextDelta(
            (0..40).map(|n| format!("line {n:02}\n")).collect(),
        ));
        let bottom = draw_lines(&mut screen, 60, 12);
        assert!(bottom[0].contains("Top (Ctrl+K)"));
        let mouse = |kind, row| {
            Event::Mouse(MouseEvent {
                kind,
                column: 59,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        screen
            .apply_terminal_event(mouse(MouseEventKind::Moved, 0), &mut backend)
            .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        assert_eq!(terminal.backend().buffer()[(59, 0)].bg, Color::Cyan);
        screen
            .apply_terminal_event(
                mouse(MouseEventKind::Down(MouseButton::Left), 0),
                &mut backend,
            )
            .unwrap();
        let top = draw_lines(&mut screen, 60, 12);
        assert!(top[0].starts_with("line 00"));
        assert!(!top[0].contains("Ctrl+K"));
        let last_row = screen.transcript_height as u16 - 1;
        assert!(top[last_row as usize].contains("Bottom (Ctrl+L)"));
        screen
            .apply_terminal_event(
                mouse(MouseEventKind::Down(MouseButton::Left), last_row),
                &mut backend,
            )
            .unwrap();
        assert_eq!(screen.scroll_from_bottom, 0);
        screen
            .apply_terminal_event(
                Event::Key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL)),
                &mut backend,
            )
            .unwrap();
        assert!(screen.scroll_from_bottom > 0);
        screen
            .apply_terminal_event(
                Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL)),
                &mut backend,
            )
            .unwrap();
        assert_eq!(screen.scroll_from_bottom, 0);
        assert_eq!(screen.input_text(), "keep draft");
        assert!(backend.requests.is_empty());
        // Tiny terminals must not create out-of-bounds floating controls.
        draw_lines(&mut screen, 4, 3);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn mouse_wheel_scrolls_output_without_changing_the_draft() {
        use crossterm::event::{KeyModifiers, MouseEvent};

        let state = crate::fixture_directory::create_unique_fixture_directory("chat-wheel");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.messages.push(ChatMessage {
            role: TranscriptRole::Operator,
            text: (0..40).map(|n| format!("line {n:02}\n")).collect(),
        });
        screen.set_input_text("my draft");
        let bottom = draw_lines(&mut screen, 60, 12);
        let wheel = |kind, row| {
            Event::Mouse(MouseEvent {
                kind,
                column: 1,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        screen
            .apply_terminal_event(wheel(MouseEventKind::ScrollUp, 0), &mut backend)
            .unwrap();
        let older = draw_lines(&mut screen, 60, 12);
        assert_ne!(older[0], bottom[0]);
        assert_eq!(screen.scroll_from_bottom, 3);
        assert_eq!(
            &older[screen.transcript_height..],
            &bottom[screen.transcript_height..]
        );
        screen
            .apply_terminal_event(wheel(MouseEventKind::ScrollUp, 10), &mut backend)
            .unwrap();
        assert_eq!(screen.scroll_from_bottom, 3);
        screen
            .apply_terminal_event(wheel(MouseEventKind::ScrollDown, 0), &mut backend)
            .unwrap();
        assert_eq!(draw_lines(&mut screen, 60, 12), bottom);
        assert_eq!(screen.input_text(), "my draft");
        assert!(backend.requests.is_empty());
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn typing_a_long_draft_moves_the_rule_up_without_submitting_or_inserting_newlines() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-wrap-draft");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 16)).unwrap();
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        let draft = format!("{}abc", "0123456789".repeat(9));
        for character in draft.chars() {
            screen
                .apply_action(
                    KeyAction::Edit(input::EditorKey::Character(character)),
                    &mut backend,
                )
                .unwrap();
        }
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let row = |y| (0..40).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert_eq!(row(10), "────────────────────────────────────────");
        assert_eq!(row(11), "0123456789012345678901234567890123456789");
        assert_eq!(row(12), "0123456789012345678901234567890123456789");
        assert_eq!(row(13).trim_end(), "0123456789abc");
        assert_eq!(
            terminal.get_cursor_position().unwrap(),
            ratatui::layout::Position::new(13, 13)
        );
        assert!(backend.requests.is_empty());
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert_eq!(backend.requests[0].messages.last().unwrap().text, draft);
        assert_eq!(screen.input_text(), "");
        fs::remove_dir_all(state).unwrap();
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
    fn tool_requests_are_visible_as_notices_without_argument_disclosure() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-tool-request");
        let mut screen = screen_in(&state);
        assert!(
            !screen.apply_event(ResponseEvent::ToolCallsRequested(vec![backend::ToolCall {
                id: "call_a".into(),
                name: "read_file".into(),
                arguments: serde_json::from_value(serde_json::json!({"path": "private-path"}))
                    .unwrap(),
            }]))
        );
        assert_eq!(
            screen.messages,
            vec![ChatMessage {
                role: TranscriptRole::Notice,
                text: "tool requested: read_file (call_a); not executed".into(),
            }]
        );
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn submitting_during_a_turn_preserves_the_draft_and_active_request() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-busy");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.set_input_text("first");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        screen.set_input_text("next");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert_eq!(backend.requests.len(), 1);
        assert_eq!(backend.requests[0].messages[0].text, "first");
        assert_eq!(screen.input.contents(), "next");
        assert!(screen.response_in_progress());
        fs::remove_dir_all(state).unwrap();
    }
}
