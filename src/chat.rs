//! Conversation screen shown after the model picker.
//!
//! Part 1 draws the screen and answers through [`EchoBackend`]. That backend
//! echoes the operator's text and reports synthetic token counts. It does not
//! call a model and it does not use the network.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::error::{Error, Result};

const INPUT_ROW_LIMIT: usize = 8;
const FRAME_INTERVAL: Duration = Duration::from_millis(50);
const STAND_IN_NOTE: &str = "stand-in backend: replies and token counts are not from a model";
const CONTEXT_WINDOW: u64 = 128_000;
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const ACCENT: Color = Color::Cyan;

/// Gateway and model, already split on the first slash of a selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub gateway: String,
    pub model: String,
}

/// Reasoning display level stored in `<state>/thinking`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
}

impl ThinkingLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "off" => Some(Self::Off),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Off => Self::Minimal,
            Self::Minimal => Self::Low,
            Self::Low => Self::Medium,
            Self::Medium => Self::High,
            Self::High => Self::Off,
        }
    }
}

/// One transcript entry sent to a backend, or a local notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: TranscriptRole,
    pub text: String,
}

/// Who produced a transcript line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptRole {
    Operator,
    Assistant,
    Notice,
}

/// What the screen asks a backend to complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRequest {
    pub selection: Selection,
    pub thinking: ThinkingLevel,
    pub messages: Vec<ChatMessage>,
}

/// One streaming result from a backend.
#[derive(Debug, Clone, PartialEq)]
pub enum ResponseEvent {
    TextDelta(String),
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cached_input_tokens: Option<u64>,
        cost_usd: Option<f64>,
    },
    ContextWindow(u64),
    Completed,
    Failed(String),
}

/// Completes one turn without blocking the screen.
pub trait CompletionBackend {
    fn submit(&mut self, request: ChatRequest) -> Receiver<ResponseEvent>;
}

/// Stand-in backend. Replies and token counts are synthetic.
#[derive(Debug, Default)]
pub struct EchoBackend;

impl EchoBackend {
    pub fn new() -> Self {
        Self
    }
}

impl CompletionBackend for EchoBackend {
    fn submit(&mut self, request: ChatRequest) -> Receiver<ResponseEvent> {
        let (sender, receiver) = mpsc::channel();
        let submitted = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == TranscriptRole::Operator)
            .map(|message| message.text.clone())
            .unwrap_or_default();
        let reply = format!("[stand-in] {submitted}");
        let characters = submitted.chars().count() as u64;
        let tokens = characters / 4;
        thread::spawn(move || {
            let pieces = delta_pieces(&reply);
            for (index, piece) in pieces.into_iter().enumerate() {
                if index > 0 {
                    thread::sleep(Duration::from_millis(20));
                }
                if sender.send(ResponseEvent::TextDelta(piece)).is_err() {
                    return;
                }
            }
            let usage = ResponseEvent::Usage {
                input_tokens: tokens,
                output_tokens: tokens,
                cached_input_tokens: None,
                cost_usd: None,
            };
            if sender.send(usage).is_err() {
                return;
            }
            if sender
                .send(ResponseEvent::ContextWindow(CONTEXT_WINDOW))
                .is_err()
            {
                return;
            }
            let _ = sender.send(ResponseEvent::Completed);
        });
        receiver
    }
}

fn delta_pieces(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 2 {
        return vec![chars.into_iter().collect()];
    }
    chars
        .chunks(4)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

/// Session figures shown on the left of the session row.
///
/// `None` means the figure is unknown and must be omitted. Zero is a known
/// value and is shown.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionMeters {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub subscription_sign_in: bool,
    pub context_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub automatic_compaction: bool,
}

/// How a key changed the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenAction {
    Continue,
    Exit,
    ReopenModelPicker,
}

/// Why the chat terminal loop returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    Exit,
    ReopenModelPicker,
}

/// Screen state, independent of the terminal.
pub struct ChatScreen {
    state_dir: PathBuf,
    selection: Selection,
    thinking: ThinkingLevel,
    messages: Vec<ChatMessage>,
    input: InputEditor,
    location_directory: String,
    location_git: Option<String>,
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
    pub fn load(state_dir: &Path, selection: &str) -> Self {
        let (thinking, notice) = read_thinking(state_dir);
        let mut screen = Self {
            state_dir: state_dir.to_path_buf(),
            selection: parse_selection(selection),
            thinking,
            messages: Vec::new(),
            input: InputEditor::new(),
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

    pub fn thinking(&self) -> ThinkingLevel {
        self.thinking
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn set_selection(&mut self, selection: &str) {
        self.selection = parse_selection(selection);
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub fn input_text(&self) -> String {
        self.input.contents()
    }

    pub fn input_row_count(&self) -> usize {
        self.input.visible_row_count()
    }

    pub fn meters(&self) -> &SessionMeters {
        &self.meters
    }

    pub fn set_meters(&mut self, meters: SessionMeters) {
        self.meters = meters;
    }

    pub fn response_in_progress(&self) -> bool {
        self.response_in_progress
    }

    pub fn set_input_text(&mut self, text: &str) {
        self.input = InputEditor::from_text(text);
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        backend: &mut dyn CompletionBackend,
    ) -> Result<ScreenAction> {
        if key.kind != KeyEventKind::Press {
            return Ok(ScreenAction::Continue);
        }
        if is_newline(&key) {
            self.input.insert_newline();
            return Ok(ScreenAction::Continue);
        }
        match key.code {
            KeyCode::Enter => self.submit_input(backend),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.input.is_blank() {
                    Ok(ScreenAction::Exit)
                } else {
                    self.input.clear();
                    Ok(ScreenAction::Continue)
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.input.is_blank() {
                    Ok(ScreenAction::Exit)
                } else {
                    Ok(ScreenAction::Continue)
                }
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.thinking = self.thinking.next();
                self.persist_thinking()?;
                Ok(ScreenAction::Continue)
            }
            KeyCode::Esc | KeyCode::PageUp | KeyCode::PageDown => {
                self.handle_navigation(key.code);
                Ok(ScreenAction::Continue)
            }
            KeyCode::Left => {
                self.input.move_left();
                Ok(ScreenAction::Continue)
            }
            KeyCode::Right => {
                self.input.move_right();
                Ok(ScreenAction::Continue)
            }
            KeyCode::Up => {
                self.input.move_up();
                Ok(ScreenAction::Continue)
            }
            KeyCode::Down => {
                self.input.move_down();
                Ok(ScreenAction::Continue)
            }
            KeyCode::Home => {
                self.input.move_home();
                Ok(ScreenAction::Continue)
            }
            KeyCode::End => {
                self.input.move_end();
                Ok(ScreenAction::Continue)
            }
            KeyCode::Backspace => {
                self.input.backspace();
                Ok(ScreenAction::Continue)
            }
            KeyCode::Delete => {
                self.input.delete();
                Ok(ScreenAction::Continue)
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.input.insert_char(ch);
                Ok(ScreenAction::Continue)
            }
            _ => Ok(ScreenAction::Continue),
        }
    }

    pub fn poll_backend(&mut self) {
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

    pub fn draw(&mut self, frame: &mut ratatui::Frame) {
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
                Constraint::Length(1),
            ])
            .split(frame.area());
        self.transcript_width = chunks[0].width as usize;
        self.transcript_height = chunks[0].height as usize;
        self.clamp_scroll();
        let lines = self.transcript_lines(self.transcript_width);
        let visible = visible_transcript(&lines, self.transcript_height, self.scroll_from_bottom);
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

        let rule = rule_line(chunks[2].width as usize);
        let accent = Style::default().fg(ACCENT);
        frame.render_widget(Paragraph::new(rule.clone()).style(accent), chunks[2]);
        let (scroll, input_lines) = self.input.visible_lines();
        frame.render_widget(Paragraph::new(input_lines.join("\n")), chunks[3]);
        let cursor_line = self.input.line.saturating_sub(scroll);
        let cursor_column = self
            .input
            .column
            .min(chunks[3].width.saturating_sub(1) as usize);
        frame.set_cursor_position((
            chunks[3].x + cursor_column as u16,
            chunks[3].y + cursor_line as u16,
        ));
        frame.render_widget(Paragraph::new(rule).style(accent), chunks[4]);

        let dim = Style::default().add_modifier(Modifier::DIM);
        let location = render_location_row(
            chunks[5].width as usize,
            &self.location_directory,
            self.location_git.as_deref(),
        );
        frame.render_widget(Paragraph::new(location).style(dim), chunks[5]);

        let session = render_session_row(
            chunks[6].width as usize,
            &self.meters,
            &self.selection,
            self.thinking,
        );
        let model_at = session
            .rfind(&session_model_segment(&self.selection, self.thinking))
            .unwrap_or(session.len());
        let left = session.chars().take(model_at).collect::<String>();
        let right = session.chars().skip(model_at).collect::<String>();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(left, dim),
                Span::styled(right, Style::default()),
            ])),
            chunks[6],
        );
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
        let mut parts = text.split_whitespace();
        let name = parts.next().unwrap_or(text);
        let rest: Vec<&str> = parts.collect();
        match (name, rest.as_slice()) {
            ("/model", []) => Ok(ScreenAction::ReopenModelPicker),
            ("/exit", []) => Ok(ScreenAction::Exit),
            ("/clear", []) => {
                self.messages.clear();
                self.meters = SessionMeters {
                    subscription_sign_in: self.meters.subscription_sign_in,
                    automatic_compaction: self.meters.automatic_compaction,
                    ..SessionMeters::default()
                };
                self.scroll_from_bottom = 0;
                self.receiver = None;
                self.response_in_progress = false;
                self.assistant_open = false;
                Ok(ScreenAction::Continue)
            }
            ("/thinking", []) => {
                let level = self.thinking.as_str();
                self.push_notice(format!("thinking {level}"));
                Ok(ScreenAction::Continue)
            }
            ("/thinking", [level]) => match ThinkingLevel::parse(level) {
                Some(level) => {
                    self.thinking = level;
                    self.persist_thinking()?;
                    Ok(ScreenAction::Continue)
                }
                None => {
                    self.push_notice(format!("unknown thinking level {level}"));
                    Ok(ScreenAction::Continue)
                }
            },
            _ => {
                self.push_notice(format!("unknown command {name}"));
                Ok(ScreenAction::Continue)
            }
        }
    }

    fn persist_thinking(&self) -> Result<()> {
        let path = self.state_dir.join("thinking");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                Error::new(format!("failed to create {}: {err}", parent.display()))
            })?;
        }
        let level = self.thinking.as_str();
        fs::write(&path, format!("{level}\n"))
            .map_err(|err| Error::new(format!("failed to write {}: {err}", path.display())))
    }

    fn handle_navigation(&mut self, code: KeyCode) {
        match code {
            KeyCode::PageUp => {
                let page = self.transcript_height.max(1);
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(page);
                self.clamp_scroll();
            }
            KeyCode::PageDown => {
                let page = self.transcript_height.max(1);
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(page);
            }
            KeyCode::Esc => {}
            _ => {}
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
                add_count(&mut self.meters.input_tokens, input_tokens);
                add_count(&mut self.meters.output_tokens, output_tokens);
                if let Some(cached) = cached_input_tokens {
                    add_count(&mut self.meters.cached_input_tokens, cached);
                }
                if let Some(cost) = cost_usd {
                    self.meters.cost_usd = Some(self.meters.cost_usd.unwrap_or(0.0) + cost);
                }
                self.meters.context_tokens = Some(input_tokens);
                false
            }
            ResponseEvent::ContextWindow(window) => {
                self.meters.context_window = Some(window);
                false
            }
            ResponseEvent::Completed => {
                self.attach_stand_in_note();
                true
            }
            ResponseEvent::Failed(message) => {
                self.push_notice(message);
                true
            }
        }
    }

    fn attach_stand_in_note(&mut self) {
        let assistants = self
            .messages
            .iter()
            .filter(|message| message.role == TranscriptRole::Assistant)
            .count();
        if assistants != 1 {
            return;
        }
        let Some(message) = self
            .messages
            .iter_mut()
            .rev()
            .find(|message| message.role == TranscriptRole::Assistant)
        else {
            return;
        };
        if message.text.starts_with("[stand-in]") && !message.text.contains(STAND_IN_NOTE) {
            if !message.text.ends_with('\n') {
                message.text.push('\n');
            }
            message.text.push_str(STAND_IN_NOTE);
        }
    }

    fn push_notice(&mut self, text: String) {
        self.messages.push(ChatMessage {
            role: TranscriptRole::Notice,
            text,
        });
    }

    fn refresh_location(&mut self) {
        let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        self.location_directory = display_directory(&current);
        self.location_git = read_git_label(&current);
    }

    fn transcript_lines(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for message in &self.messages {
            match message.role {
                TranscriptRole::Operator => {
                    lines.extend(wrap_labeled("you ", &message.text, width))
                }
                TranscriptRole::Assistant => {
                    lines.extend(wrap_labeled("ahi ", &message.text, width))
                }
                TranscriptRole::Notice => lines.extend(wrap_plain(&message.text, width)),
            }
        }
        lines
    }

    fn clamp_scroll(&mut self) {
        let lines = self.transcript_lines(self.transcript_width.max(1));
        let max_scroll = lines.len().saturating_sub(self.transcript_height);
        if self.scroll_from_bottom > max_scroll {
            self.scroll_from_bottom = max_scroll;
        }
    }
}

fn add_count(slot: &mut Option<u64>, value: u64) {
    *slot = Some(slot.unwrap_or(0).saturating_add(value));
}

fn is_newline(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            true
        }
        KeyCode::Char('j' | 'J') if key.modifiers.contains(KeyModifiers::CONTROL) => true,
        KeyCode::Char('\n') => true,
        _ => false,
    }
}

fn read_thinking(state_dir: &Path) -> (ThinkingLevel, Option<String>) {
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

pub fn parse_selection(text: &str) -> Selection {
    let text = text.trim();
    match text.split_once('/') {
        Some((gateway, model)) => Selection {
            gateway: gateway.to_string(),
            model: model.to_string(),
        },
        None => Selection {
            gateway: text.to_string(),
            model: String::new(),
        },
    }
}

fn session_model_segment(selection: &Selection, thinking: ThinkingLevel) -> String {
    format!(
        "({}) {} • {}",
        selection.gateway,
        selection.model,
        thinking.as_str()
    )
}

/// The session row, with the model segment right-aligned and never truncated.
pub fn render_session_row(
    width: usize,
    meters: &SessionMeters,
    selection: &Selection,
    thinking: ThinkingLevel,
) -> String {
    let right = session_model_segment(selection, thinking);
    let fields = session_fields(meters);
    if width == 0 {
        return String::new();
    }
    let right_len = right.chars().count();
    if right_len >= width {
        return right;
    }
    let budget = width - right_len - 1;
    let left = fit_prefix(&fields, budget);
    if left.is_empty() {
        let pad = width - right_len;
        format!("{}{right}", " ".repeat(pad))
    } else {
        let pad = width - left.chars().count() - right_len;
        format!("{left}{}{right}", " ".repeat(pad))
    }
}

fn session_fields(meters: &SessionMeters) -> Vec<String> {
    let mut fields = Vec::new();
    if let Some(count) = meters.input_tokens {
        fields.push(format!("↑{}", format_count(count)));
    }
    if let Some(count) = meters.output_tokens {
        fields.push(format!("↓{}", format_count(count)));
    }
    if let Some(count) = meters.cached_input_tokens {
        fields.push(format!("R{}", format_count(count)));
    }
    if let (Some(cached), Some(input)) = (meters.cached_input_tokens, meters.input_tokens) {
        if input > 0 {
            if let Some(percent) = format_percent(cached, input) {
                fields.push(format!("CH{percent}%"));
            }
        }
    }
    if let Some(cost) = meters.cost_usd {
        let mut field = format!("${cost:.3}");
        if meters.subscription_sign_in {
            field.push_str(" (sub)");
        }
        fields.push(field);
    }
    if let (Some(used), Some(window)) = (meters.context_tokens, meters.context_window) {
        if let Some(percent) = format_percent(used, window) {
            fields.push(format!("{percent}%/{}", format_count(window)));
        }
    }
    if meters.automatic_compaction {
        fields.push("(auto)".to_string());
    }
    fields
}

fn fit_prefix(fields: &[String], budget: usize) -> String {
    let mut end = fields.len();
    while end > 0 {
        let text = fields[..end].join(" ");
        if text.chars().count() <= budget {
            return text;
        }
        end -= 1;
    }
    String::new()
}

pub fn format_count(count: u64) -> String {
    if count < 1_000 {
        return count.to_string();
    }
    if count < 1_000_000 {
        return format_scaled(count, 1_000, "k");
    }
    format_scaled(count, 1_000_000, "M")
}

fn format_scaled(count: u64, unit: u64, suffix: &str) -> String {
    let tenths = (count as u128 * 10 + unit as u128 / 2) / unit as u128;
    let whole = tenths / 10;
    let fraction = tenths % 10;
    if fraction == 0 {
        format!("{whole}{suffix}")
    } else {
        format!("{whole}.{fraction}{suffix}")
    }
}

fn format_percent(part: u64, whole: u64) -> Option<String> {
    if whole == 0 {
        return None;
    }
    let tenths = (part as u128 * 1000 + whole as u128 / 2) / whole as u128;
    let whole_percent = tenths / 10;
    let fraction = tenths % 10;
    if fraction == 0 {
        Some(whole_percent.to_string())
    } else {
        Some(format!("{whole_percent}.{fraction}"))
    }
}

/// Directory, with the home directory shortened to `~`, and an optional git label.
pub fn render_location_row(width: usize, directory: &str, git_label: Option<&str>) -> String {
    let full = match git_label {
        Some(label) if !label.is_empty() => format!("{directory} ({label})"),
        _ => directory.to_string(),
    };
    shorten_from_the_left(&full, width)
}

fn shorten_from_the_left(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut shortened = String::from("…");
    let keep = width - 1;
    shortened.extend(chars[chars.len() - keep..].iter());
    shortened
}

fn display_directory(path: &Path) -> String {
    if let Some(home) = home_directory() {
        if let Ok(rest) = path.strip_prefix(&home) {
            let rest = rest.display().to_string();
            if rest.is_empty() {
                return "~".to_string();
            }
            return format!("~{}{rest}", std::path::MAIN_SEPARATOR);
        }
    }
    path.display().to_string()
}

fn home_directory() -> Option<PathBuf> {
    for name in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(name) {
            if !value.is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }
    None
}

/// Branch name, or a 7-character commit id when `HEAD` is detached.
pub fn read_git_label(start: &Path) -> Option<String> {
    let git_dir = find_git_directory(start)?;
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref: ") {
        let name = reference
            .trim()
            .strip_prefix("refs/heads/")
            .unwrap_or(reference.trim());
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    } else if head.len() >= 7 && head.chars().all(|character| character.is_ascii_hexdigit()) {
        Some(head.chars().take(7).collect())
    } else {
        None
    }
}

fn find_git_directory(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(directory) = current {
        let dot_git = directory.join(".git");
        if dot_git.is_dir() {
            return Some(dot_git);
        }
        if dot_git.is_file() {
            return git_directory_from_file(&dot_git);
        }
        current = directory.parent();
    }
    None
}

fn git_directory_from_file(dot_git: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(dot_git).ok()?;
    let line = text.lines().find(|line| line.starts_with("gitdir:"))?;
    let path = line["gitdir:".len()..].trim();
    let git_dir = PathBuf::from(path);
    let parent = dot_git.parent()?;
    if git_dir.is_absolute() {
        Some(git_dir)
    } else {
        Some(parent.join(git_dir))
    }
}

fn rule_line(width: usize) -> String {
    "─".repeat(width)
}

fn wrap_labeled(label: &str, text: &str, width: usize) -> Vec<String> {
    if width <= label.chars().count() {
        return wrap_plain(&format!("{label}{text}"), width);
    }
    let indent = " ".repeat(label.chars().count());
    let mut lines = Vec::new();
    for (index, paragraph) in text.split('\n').enumerate() {
        let prefix = if index == 0 { label } else { &indent };
        let wrapped = wrap_paragraph(prefix, paragraph, width);
        if wrapped.is_empty() {
            lines.push(prefix.trim_end().to_string());
        } else {
            lines.extend(wrapped);
        }
    }
    lines
}

fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        lines.extend(wrap_paragraph("", paragraph, width));
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn wrap_paragraph(prefix: &str, paragraph: &str, width: usize) -> Vec<String> {
    let prefix_len = prefix.chars().count();
    if width <= prefix_len {
        return vec![format!("{prefix}{paragraph}")];
    }
    let room = width - prefix_len;
    if paragraph.chars().count() <= room {
        return vec![format!("{prefix}{paragraph}")];
    }
    let mut lines = Vec::new();
    let chars: Vec<char> = paragraph.chars().collect();
    let mut offset = 0;
    let mut first = true;
    while offset < chars.len() {
        let end = (offset + room).min(chars.len());
        let piece: String = chars[offset..end].iter().collect();
        if first {
            lines.push(format!("{prefix}{piece}"));
            first = false;
        } else {
            lines.push(format!("{}{piece}", " ".repeat(prefix_len)));
        }
        offset = end;
    }
    lines
}

fn visible_transcript(lines: &[String], height: usize, scroll_from_bottom: usize) -> String {
    if height == 0 || lines.is_empty() {
        return String::new();
    }
    let end = lines.len().saturating_sub(scroll_from_bottom);
    let start = end.saturating_sub(height);
    lines[start..end].join("\n")
}

struct InputEditor {
    lines: Vec<String>,
    line: usize,
    column: usize,
}

impl InputEditor {
    fn new() -> Self {
        Self {
            lines: vec![String::new()],
            line: 0,
            column: 0,
        }
    }

    fn from_text(text: &str) -> Self {
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(str::to_string).collect()
        };
        let line = lines.len() - 1;
        let column = lines[line].chars().count();
        Self {
            lines,
            line,
            column,
        }
    }

    fn contents(&self) -> String {
        self.lines.join("\n")
    }

    fn is_blank(&self) -> bool {
        self.contents().trim().is_empty()
    }

    fn clear(&mut self) {
        *self = Self::new();
    }

    fn visible_row_count(&self) -> usize {
        self.lines.len().clamp(1, INPUT_ROW_LIMIT)
    }

    fn visible_lines(&self) -> (usize, Vec<String>) {
        let start = if self.lines.len() <= INPUT_ROW_LIMIT {
            0
        } else {
            self.line
                .saturating_sub(INPUT_ROW_LIMIT - 1)
                .min(self.lines.len() - INPUT_ROW_LIMIT)
        };
        let end = start + self.visible_row_count();
        (start, self.lines[start..end].to_vec())
    }

    fn insert_char(&mut self, character: char) {
        let column = self.column.min(self.lines[self.line].chars().count());
        let byte = byte_index(&self.lines[self.line], column);
        self.lines[self.line].insert(byte, character);
        self.column = column + 1;
    }

    fn insert_newline(&mut self) {
        let column = self.column.min(self.lines[self.line].chars().count());
        let byte = byte_index(&self.lines[self.line], column);
        let right = self.lines[self.line].split_off(byte);
        self.lines.insert(self.line + 1, right);
        self.line += 1;
        self.column = 0;
    }

    fn backspace(&mut self) {
        if self.column > 0 {
            let end = byte_index(&self.lines[self.line], self.column);
            let start = byte_index(&self.lines[self.line], self.column - 1);
            self.lines[self.line].replace_range(start..end, "");
            self.column -= 1;
            return;
        }
        if self.line == 0 {
            return;
        }
        let current = self.lines.remove(self.line);
        self.line -= 1;
        self.column = self.lines[self.line].chars().count();
        self.lines[self.line].push_str(&current);
    }

    fn delete(&mut self) {
        let length = self.lines[self.line].chars().count();
        if self.column < length {
            let start = byte_index(&self.lines[self.line], self.column);
            let end = byte_index(&self.lines[self.line], self.column + 1);
            self.lines[self.line].replace_range(start..end, "");
            return;
        }
        if self.line + 1 < self.lines.len() {
            let next = self.lines.remove(self.line + 1);
            self.lines[self.line].push_str(&next);
        }
    }

    fn move_left(&mut self) {
        if self.column > 0 {
            self.column -= 1;
        } else if self.line > 0 {
            self.line -= 1;
            self.column = self.lines[self.line].chars().count();
        }
    }

    fn move_right(&mut self) {
        let length = self.lines[self.line].chars().count();
        if self.column < length {
            self.column += 1;
        } else if self.line + 1 < self.lines.len() {
            self.line += 1;
            self.column = 0;
        }
    }

    fn move_up(&mut self) {
        if self.line > 0 {
            self.line -= 1;
            let length = self.lines[self.line].chars().count();
            if self.column > length {
                self.column = length;
            }
        }
    }

    fn move_down(&mut self) {
        if self.line + 1 < self.lines.len() {
            self.line += 1;
            let length = self.lines[self.line].chars().count();
            if self.column > length {
                self.column = length;
            }
        }
    }

    fn move_home(&mut self) {
        self.column = 0;
    }

    fn move_end(&mut self) {
        self.column = self.lines[self.line].chars().count();
    }
}

fn byte_index(text: &str, column: usize) -> usize {
    text.chars().take(column).map(char::len_utf8).sum()
}

/// Run the chat screen until the operator exits or asks for the model picker.
pub fn interact(
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
                match screen.handle_key(key, backend)? {
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
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::process::Command;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn enter() -> KeyEvent {
        key(KeyCode::Enter, KeyModifiers::NONE)
    }

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
    fn enter_submits_and_newline_keys_do_not() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-keys");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.set_input_text("   ");
        screen.handle_key(enter(), &mut backend).unwrap();
        assert!(backend.requests.is_empty());
        assert_eq!(screen.input_text(), "   ");

        screen.set_input_text("explain");
        screen.handle_key(enter(), &mut backend).unwrap();
        assert_eq!(backend.requests.len(), 1);
        assert_eq!(backend.requests[0].messages[0].text, "explain");
        assert!(screen.input_text().is_empty());

        for (label, event) in [
            ("shift", key(KeyCode::Enter, KeyModifiers::SHIFT)),
            ("alt", key(KeyCode::Enter, KeyModifiers::ALT)),
            ("ctrl-j", key(KeyCode::Char('j'), KeyModifiers::CONTROL)),
        ] {
            screen.set_input_text("one");
            screen.handle_key(event, &mut backend).unwrap();
            assert_eq!(screen.input_text(), "one\n", "{label}");
            assert_eq!(backend.requests.len(), 1, "{label}");
        }
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn input_grows_to_eight_rows_and_no_further() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-rows");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.set_input_text("a");
        assert_eq!(screen.input_row_count(), 1);
        for _ in 0..7 {
            screen
                .handle_key(key(KeyCode::Enter, KeyModifiers::SHIFT), &mut backend)
                .unwrap();
        }
        assert_eq!(screen.input_row_count(), 8);
        screen
            .handle_key(key(KeyCode::Char('j'), KeyModifiers::CONTROL), &mut backend)
            .unwrap();
        assert_eq!(screen.input.lines.len(), 9);
        assert_eq!(screen.input_row_count(), 8);
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
        let action = screen.handle_key(enter(), &mut backend).unwrap();
        assert_eq!(action, ScreenAction::Continue);
        assert_eq!(screen.thinking(), ThinkingLevel::High);
        assert_eq!(
            fs::read_to_string(state.join("thinking")).unwrap().trim(),
            "high"
        );

        screen.set_input_text("/thinking");
        screen.handle_key(enter(), &mut backend).unwrap();
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
        screen.handle_key(enter(), &mut backend).unwrap();
        assert!(screen.messages().is_empty());
        assert_eq!(screen.meters().input_tokens, None);

        screen.set_input_text("/no-such");
        screen.handle_key(enter(), &mut backend).unwrap();
        assert!(screen
            .messages()
            .iter()
            .any(|message| message.text == "unknown command /no-such"));
        assert!(backend.requests.is_empty());

        screen.set_input_text("/model");
        let action = screen.handle_key(enter(), &mut backend).unwrap();
        assert_eq!(action, ScreenAction::ReopenModelPicker);

        screen.set_input_text("/exit");
        let action = screen.handle_key(enter(), &mut backend).unwrap();
        assert_eq!(action, ScreenAction::Exit);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn thinking_level_is_persisted_and_an_invalid_file_falls_back() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-thinking");
        fs::write(state.join("thinking"), "high\n").unwrap();
        let screen = ChatScreen::load(&state, "openai/gpt-4.1");
        assert_eq!(screen.thinking(), ThinkingLevel::High);

        fs::write(state.join("thinking"), "loud\n").unwrap();
        let screen = ChatScreen::load(&state, "openai/gpt-4.1");
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

    fn reference_meters() -> SessionMeters {
        SessionMeters {
            input_tokens: Some(139_000),
            output_tokens: Some(11_000),
            cached_input_tokens: Some(1_200_000),
            cost_usd: Some(0.502),
            subscription_sign_in: true,
            context_tokens: Some(77_248),
            context_window: Some(272_000),
            automatic_compaction: true,
        }
    }

    fn selection() -> Selection {
        Selection {
            gateway: "openai".to_string(),
            model: "gpt-6.1-sol".to_string(),
        }
    }

    #[test]
    fn session_row_matches_the_reference_order_and_drops_fields() {
        let left = "↑139k ↓11k R1.2M CH863.3% $0.502 (sub) 28.4%/272k (auto)";
        let right = "(openai) gpt-6.1-sol • medium";
        let wide = render_session_row(
            120,
            &reference_meters(),
            &selection(),
            ThinkingLevel::Medium,
        );
        assert_eq!(wide.chars().count(), 120);
        assert!(wide.starts_with(left), "{wide}");
        assert!(wide.ends_with(right), "{wide}");
        let mut previous = 0;
        for marker in ["↑", "↓", "R", "CH", "$", "(sub)", "28.4%/", "(auto)"] {
            let position = wide
                .find(marker)
                .unwrap_or_else(|| panic!("missing {marker} in {wide}"));
            assert!(position >= previous, "{marker} out of order in {wide}");
            previous = position;
        }

        let narrow =
            render_session_row(60, &reference_meters(), &selection(), ThinkingLevel::Medium);
        assert_eq!(narrow.chars().count(), 60, "{narrow}");
        assert!(narrow.ends_with(right), "{narrow}");
        assert!(narrow.contains("↑139k ↓11k R1.2M CH863.3%"), "{narrow}");
        assert!(!narrow.contains("(auto)"), "{narrow}");
        assert!(!narrow.contains("28.4%"), "{narrow}");
        assert!(!narrow.contains("(sub)"), "{narrow}");
        assert!(!narrow.contains("$0.502"), "{narrow}");
    }

    #[test]
    fn unknown_usage_fields_are_omitted_and_zero_is_shown() {
        let meters = SessionMeters {
            input_tokens: Some(8),
            output_tokens: Some(2),
            context_tokens: Some(8),
            context_window: Some(128_000),
            ..SessionMeters::default()
        };
        let row = render_session_row(80, &meters, &selection(), ThinkingLevel::Medium);
        assert!(row.contains("↑8 ↓2 0%/128k"), "{row}");
        assert!(!row.contains('R'), "{row}");
        assert!(!row.contains("CH"), "{row}");
        assert!(!row.contains('$'), "{row}");
        assert!(!row.contains("(sub)"), "{row}");
        assert!(!row.contains("(auto)"), "{row}");

        let mut known_zero = meters;
        known_zero.cached_input_tokens = Some(0);
        let row = render_session_row(80, &known_zero, &selection(), ThinkingLevel::Medium);
        assert!(row.contains("R0"), "{row}");
        assert!(row.contains("CH0%"), "{row}");
    }

    #[test]
    fn drawn_screen_has_rules_activity_and_an_unframed_input() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-draw");
        let mut screen = screen_in(&state);
        screen.set_selection("openai/gpt-6.1-sol");
        screen.location_directory = "~/Work/gh/ahi".to_string();
        screen.location_git = Some("main".to_string());
        screen.set_meters(reference_meters());
        screen.messages.push(ChatMessage {
            role: TranscriptRole::Operator,
            text: "explain the hook runner".to_string(),
        });
        let idle = draw_lines(&mut screen, 120, 16);
        let activity = &idle[idle.len() - 6];
        assert!(activity.trim().is_empty(), "{activity}");
        assert!(idle[idle.len() - 5]
            .chars()
            .all(|character| character == '─'));
        assert!(idle[idle.len() - 3]
            .chars()
            .all(|character| character == '─'));
        assert!(!idle[idle.len() - 4].starts_with('>'));
        assert!(idle
            .iter()
            .all(|line| !line.contains('│') && !line.contains('┌')));
        assert!(idle[idle.len() - 2].contains("(main)"));
        assert!(idle
            .last()
            .unwrap()
            .ends_with("(openai) gpt-6.1-sol • medium"));

        screen.response_in_progress = true;
        let working = draw_lines(&mut screen, 120, 16);
        assert!(
            working[working.len() - 6].contains("Working"),
            "{}",
            working[working.len() - 6]
        );
        let _ = fs::remove_dir_all(state);
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
    fn location_row_shows_branch_detached_commit_or_directory_only() {
        let repo = crate::fixture_directory::create_unique_fixture_directory("chat-git");
        git(&repo, &["init", "-b", "main"]);
        fs::write(repo.join("file"), "a\n").unwrap();
        git(&repo, &["add", "file"]);
        git(&repo, &["commit", "-m", "init"]);
        let branch = read_git_label(&repo);
        assert_eq!(branch.as_deref(), Some("main"));
        let row = render_location_row(80, "/tmp/ahi", branch.as_deref());
        assert!(row.ends_with("(main)"), "{row}");

        let sha = git_stdout(&repo, &["rev-parse", "HEAD"]);
        let short = sha.trim().chars().take(7).collect::<String>();
        git(&repo, &["checkout", "--detach", "HEAD"]);
        assert_eq!(read_git_label(&repo).as_deref(), Some(short.as_str()));
        let detached = render_location_row(80, "/tmp/ahi", Some(&short));
        assert!(detached.contains(&format!("({short})")), "{detached}");
        assert!(!detached.contains("(main)"), "{detached}");

        let outside = crate::fixture_directory::create_unique_fixture_directory("chat-nogit");
        assert!(read_git_label(&outside).is_none());
        let row = render_location_row(80, "/tmp/plain", None);
        assert_eq!(row, "/tmp/plain");
        assert!(!row.contains('('));

        let shortened = render_location_row(12, "/home/user/src/ahi", Some("main"));
        assert!(shortened.starts_with('…'), "{shortened}");
        assert!(shortened.ends_with("(main)"), "{shortened}");
        assert_eq!(shortened.chars().count(), 12);

        let work = repo.join("work");
        fs::create_dir(&work).unwrap();
        fs::write(work.join(".git"), "gitdir: ../.git\n").unwrap();
        assert_eq!(read_git_label(&work).as_deref(), Some(short.as_str()));
        let _ = fs::remove_dir_all(repo);
        let _ = fs::remove_dir_all(outside);
    }

    fn git(directory: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(directory)
            .env("GIT_AUTHOR_NAME", "Ahi Test")
            .env("GIT_AUTHOR_EMAIL", "ahi@example.com")
            .env("GIT_COMMITTER_NAME", "Ahi Test")
            .env("GIT_COMMITTER_EMAIL", "ahi@example.com")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn git_stdout(directory: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(directory)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn echo_backend_streams_usage_then_completion_and_notes_the_first_reply() {
        let mut backend = EchoBackend::new();
        let request = ChatRequest {
            selection: selection(),
            thinking: ThinkingLevel::Medium,
            messages: vec![ChatMessage {
                role: TranscriptRole::Operator,
                text: "hello!!!!".to_string(),
            }],
        };
        let first = collect(&backend.submit(request.clone()));
        assert!(first
            .deltas
            .concat_text()
            .starts_with("[stand-in] hello!!!!"));
        assert!(!first.deltas.concat_text().contains(STAND_IN_NOTE));
        assert!(matches!(
            first.kinds.as_slice(),
            [
                EventKind::Delta,
                ..,
                EventKind::Usage,
                EventKind::Window,
                EventKind::Completed
            ]
        ));
        assert!(first.deltas.len() > 1);
        match first.usage {
            Some(ResponseEvent::Usage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                cost_usd,
            }) => {
                assert_eq!(input_tokens, 2);
                assert_eq!(output_tokens, 2);
                assert_eq!(cached_input_tokens, None);
                assert_eq!(cost_usd, None);
            }
            other => panic!("expected usage, got {other:?}"),
        }
        assert_eq!(first.window, Some(128_000));

        let second = collect(&backend.submit(request));
        assert!(!second.deltas.concat_text().contains(STAND_IN_NOTE));

        let state = crate::fixture_directory::create_unique_fixture_directory("chat-echo");
        let mut screen = screen_in(&state);
        screen.set_input_text("hello!!!!");
        let mut live = EchoBackend::new();
        screen.handle_key(enter(), &mut live).unwrap();
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
        screen.handle_key(enter(), &mut live).unwrap();
        screen.set_input_text("again!!!");
        screen.handle_key(enter(), &mut live).unwrap();
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

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum EventKind {
        Delta,
        Usage,
        Window,
        Completed,
        Failed,
    }

    struct Collected {
        kinds: Vec<EventKind>,
        deltas: Vec<String>,
        usage: Option<ResponseEvent>,
        window: Option<u64>,
    }

    trait ConcatText {
        fn concat_text(&self) -> String;
    }

    impl ConcatText for Vec<String> {
        fn concat_text(&self) -> String {
            self.concat()
        }
    }

    fn collect(receiver: &Receiver<ResponseEvent>) -> Collected {
        let mut collected = Collected {
            kinds: Vec::new(),
            deltas: Vec::new(),
            usage: None,
            window: None,
        };
        loop {
            let event = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            match event {
                ResponseEvent::TextDelta(text) => {
                    collected.kinds.push(EventKind::Delta);
                    collected.deltas.push(text);
                }
                ResponseEvent::Usage {
                    input_tokens,
                    output_tokens,
                    cached_input_tokens,
                    cost_usd,
                } => {
                    collected.kinds.push(EventKind::Usage);
                    collected.usage = Some(ResponseEvent::Usage {
                        input_tokens,
                        output_tokens,
                        cached_input_tokens,
                        cost_usd,
                    });
                }
                ResponseEvent::ContextWindow(window) => {
                    collected.kinds.push(EventKind::Window);
                    collected.window = Some(window);
                }
                ResponseEvent::Completed => {
                    collected.kinds.push(EventKind::Completed);
                    break;
                }
                ResponseEvent::Failed(_) => {
                    collected.kinds.push(EventKind::Failed);
                    break;
                }
            }
        }
        collected
    }
}
