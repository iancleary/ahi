//! Conversation screen shown after the model picker.
//!
//! Registered gateways stream real model responses through `HttpBackend`.
//!
//! Terminal drawing stays in this module. Key codes are translated by
//! `key_binding` before the screen applies them.

mod approval;
pub(crate) mod backend;
mod command;
mod header;
pub(crate) mod input;
mod key_binding;
mod location;
mod open;
mod preferences;
mod scroll_navigation;
mod selection;
mod status;
mod thinking;
pub(crate) mod transcript;
mod usage;

use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crate::directories::Directories;
use crate::error::{Error, Result};
use crate::session;
use crate::tui::commands::{CommandContext, CommandMenu, PickerOutcome};

use ahi_widgets::Paragraph;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    MouseButton, MouseEventKind,
};
use ratatui_core::layout::{Alignment, Constraint, Direction, Layout, Position, Rect};
use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Text};

use backend::{
    parse_selection, ChatRequest, CompletionBackend, HttpBackend, ResponseEvent, Selection,
};
use command::ParsedCommand;
use input::InputEditor;
use key_binding::KeyAction;
use thinking::ThinkingLevel;
use transcript::{ChatMessage, ToolPayload, TranscriptRole};
use usage::SessionMeters;

const FRAME_INTERVAL: Duration = Duration::from_millis(50);
/// Rows per step while a drag holds a viewport edge: one row per frame.
const AUTO_SCROLL_INTERVAL: Duration = Duration::from_millis(50);
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const ACCENT: Color = Color::Cyan;

/// How a key changed the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenAction {
    Continue,
    Exit,
    ReopenModelPicker,
    OpenCatalog,
    Login,
    Gateway,
}

#[derive(Clone, Copy)]
pub(crate) enum PickerView {
    Models,
    Catalog,
}

/// Why the chat terminal loop returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEnd {
    Exit,
    ReopenModelPicker,
    OpenCatalog,
    Login,
    Gateway,
}

/// Screen state, independent of the terminal.
struct ChatScreen {
    state_dir: Directories,
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
    approval: Option<approval::PendingApproval>,
    scroll_navigation: scroll_navigation::ScrollNavigation,
    mouse_capture: bool,
    settings_menu: CommandMenu,
    text_selection: selection::Selection,
    copy_receiver: Option<Receiver<bool>>,
    pending_copy: Option<String>,
    copy_status: Option<(&'static str, Instant)>,
    scroll_from_bottom: usize,
    /// Last auto-scroll step while a drag sits on a viewport edge.
    auto_scroll_at: Option<Instant>,
    transcript_area: Rect,
    transcript_width: usize,
    transcript_height: usize,
    spinner_frame: usize,
    /// Wrapped rows, re-laid from `dirty_from` on demand, never per frame.
    layout: transcript::render::TranscriptLayout,
    dirty: transcript::render::Dirty,
    /// A tool returned since the last drain; refresh Git status once per drain.
    location_dirty: bool,
    /// A Git status probe in flight on its own thread; the UI never waits on it.
    location_receiver: Option<Receiver<(String, Option<location::GitStatus>)>>,
    /// The backend is between connection attempts: (next attempt, of).
    reconnecting: Option<(u8, u8)>,
    /// The last turn failed before the gateway was reached, so Enter on a
    /// blank input sends the same transcript again.
    resend_available: bool,
    preferences: preferences::Preferences,
    /// Set on send, cleared at the turn's end; drives the activity counter.
    turn_started: Option<Instant>,
    workspace_root: std::path::PathBuf,
    /// Where the left button went down, to tell a click from a drag.
    click_origin: Option<(u16, u16)>,
    /// What the next successful clipboard write reports.
    copy_label: &'static str,
    /// OSC 7501 reports for the multiplexer that holds this terminal.
    status: crate::program_status::Reporter,
    /// The session this chat is recorded into.
    recorder: session::recorder::Recorder,
}

impl ChatScreen {
    #[cfg(test)]
    fn load(state_dir: &Directories, selection: &str) -> Self {
        Self::load_recording(
            state_dir,
            selection,
            session::recorder::Recorder::new(true, "tui"),
        )
    }

    /// A screen over an existing session: its model and thinking level, its
    /// conversation on the active path, and new turns appended under its
    /// last success.
    fn load_resumed(state_dir: &Directories, stored: session::Session) -> Self {
        let settings = stored.settings();
        let messages = stored.active_messages(None);
        let id = stored.id().to_string();
        let mut screen = Self::load_recording(
            state_dir,
            &format!("{}/{}", settings.gateway, settings.model),
            session::recorder::Recorder::resumed(stored, "tui"),
        );
        if let Some(level) = thinking::ThinkingLevel::parse(&settings.thinking) {
            screen.thinking = level;
        }
        let count = messages.len();
        screen.messages.extend(messages);
        screen.dirty = transcript::render::Dirty::From(0);
        screen.push_notice(format!(
            "resumed session {id}: {count} message{} on its active path",
            if count == 1 { "" } else { "s" }
        ));
        screen
    }

    fn load_recording(
        state_dir: &Directories,
        selection: &str,
        recorder: session::recorder::Recorder,
    ) -> Self {
        let (thinking, notice) = thinking::read(state_dir);
        let mut screen = Self {
            recorder,
            preferences: preferences::read(state_dir),
            turn_started: None,
            workspace_root: std::env::current_dir().unwrap_or_default(),
            click_origin: None,
            copy_label: "Selection copied",
            status: crate::program_status::Reporter::for_stdout(),
            state_dir: state_dir.clone(),
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
            approval: None,
            scroll_navigation: scroll_navigation::ScrollNavigation::default(),
            mouse_capture: true,
            settings_menu: CommandMenu::default(),
            text_selection: selection::Selection::default(),
            copy_receiver: None,
            pending_copy: None,
            copy_status: None,
            scroll_from_bottom: 0,
            auto_scroll_at: None,
            transcript_area: Rect::default(),
            transcript_width: 80,
            transcript_height: 1,
            spinner_frame: 0,
            layout: transcript::render::TranscriptLayout::new(),
            dirty: transcript::render::Dirty::From(0),
            location_dirty: false,
            location_receiver: None,
            reconnecting: None,
            resend_available: false,
        };
        screen.refresh_location();
        if let Some(notice) = notice {
            screen.push_notice(notice);
        }
        screen
    }

    /// The startup header, built once for a fresh transcript. Width is not
    /// known before the first frame, so the wide form is used; the layout
    /// wraps it on a narrow terminal.
    fn push_header(&mut self) {
        let directory = self
            .workspace_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".to_string());
        let tools = self.workspace_root.join("ahi.tools.yaml").is_file();
        let text = header::text(
            120,
            &self.selection.model,
            &self.selection.gateway,
            self.thinking.as_str(),
            &directory,
            tools,
        );
        self.push_notice(text);
    }

    #[cfg(test)]
    fn thinking(&self) -> ThinkingLevel {
        self.thinking
    }

    /// Mark the message at `index` (and everything after it) for re-layout.
    fn mark_dirty(&mut self, index: usize) {
        use transcript::render::Dirty;
        self.dirty = match self.dirty {
            Dirty::From(from) => Dirty::From(from.min(index)),
            // An earlier full mark outranks an append of the last message.
            Dirty::AppendedLast => Dirty::From(index.min(self.messages.len().saturating_sub(1))),
        };
    }

    /// Text was appended to the last message and nothing else changed.
    fn mark_appended_last(&mut self) {
        use transcript::render::Dirty;
        let last = self.messages.len().saturating_sub(1);
        if let Dirty::From(from) = self.dirty {
            if from >= last {
                // Clean, or only the last message dirty: resume from its tail
                // unless it was already marked for a full re-layout.
                self.dirty = if from == last {
                    Dirty::From(last)
                } else {
                    Dirty::AppendedLast
                };
            }
        }
    }

    fn push_message(&mut self, message: ChatMessage) {
        self.mark_dirty(self.messages.len());
        self.messages.push(message);
    }

    /// Bring the row layout up to date with the transcript and width.
    fn sync_layout(&mut self) {
        let root = self.workspace_root.clone();
        let exists = move |path: &str| file_exists(&root, path);
        let options = transcript::format::Options {
            markdown: self.preferences.markdown,
            exists: &exists,
        };
        self.layout.sync(
            &self.messages,
            self.transcript_width,
            &mut self.dirty,
            &options,
        );
    }

    fn row_count(&mut self) -> usize {
        self.sync_layout();
        self.layout.len()
    }

    fn set_selection(&mut self, selection: &str) {
        let selection = parse_selection(selection);
        if selection != self.selection {
            self.meters.context_tokens = None;
            self.meters.context_window = None;
        }
        if selection != self.selection {
            self.recorder.model_changed(&selection);
        }
        if selection.gateway != self.selection.gateway {
            // The transcript follows the operator to the new provider; say so
            // before the next message carries it there.
            let carried = self
                .messages
                .iter()
                .filter(|message| !message.role.is_notice())
                .count();
            if carried > 0 {
                self.push_notice(format!(
                    "switched provider from {} to {}: the {carried} messages above \
                     will be sent to {} with your next message",
                    self.selection.gateway, selection.gateway, selection.gateway
                ));
                self.scroll_from_bottom = 0;
            }
        }
        self.selection = selection;
    }

    /// Show the recorder's first failure, once, where the operator sees it.
    fn surface_recording_failure(&mut self) {
        if let Some(problem) = self.recorder.take_failure() {
            self.push_alert(format!("session not recorded from here on: {problem}"));
        }
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
        // In mouse-off mode, native selection belongs to the terminal.
        // Ignore stale queued mouse reports after switching modes.
        if !self.mouse_capture && matches!(event, Event::Mouse(_)) {
            return Ok(ScreenAction::Continue);
        }
        if let Event::Mouse(mouse) = event {
            if !self.settings_menu.dismissed
                && !CommandMenu::matching(&self.input.contents(), CommandContext::Chat).is_empty()
            {
                return Ok(ScreenAction::Continue);
            }
            if let Some(action) = self.scroll_navigation.mouse(mouse) {
                self.text_selection.clear();
                return self.apply_action(action, backend);
            }
            let count = self.row_count();
            let start = count
                .saturating_sub(self.scroll_from_bottom)
                .saturating_sub(self.transcript_height);
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    self.text_selection.begin(
                        mouse.column,
                        mouse.row,
                        self.transcript_area,
                        start,
                        count,
                    );
                    self.click_origin = Some((mouse.column, mouse.row));
                    self.copy_status = None;
                }
                MouseEventKind::Drag(MouseButton::Left) => self.text_selection.drag(
                    mouse.column,
                    mouse.row,
                    self.transcript_area,
                    start,
                    count,
                ),
                MouseEventKind::Up(MouseButton::Left) if self.text_selection.release() => {
                    self.click_origin = None;
                    self.copy_selection(self.preferences.copy);
                }
                // No drag: a click. Anything under it that reacts, reacts.
                MouseEventKind::Up(MouseButton::Left)
                    if self.click_origin.take() == Some((mouse.column, mouse.row))
                        && self
                            .transcript_area
                            .contains(Position::new(mouse.column, mouse.row)) =>
                {
                    let row = start + (mouse.row - self.transcript_area.y) as usize;
                    let column = (mouse.column - self.transcript_area.x) as usize;
                    self.activate_target_at(row, column);
                }
                _ => {}
            }
        }
        match event {
            // Selection mode owns every key, so nothing reaches the draft or
            // a pending approval until Esc leaves it.
            Event::Key(key) if self.text_selection.keyboard_active() => {
                self.apply_selection_key(key_binding::translate_selection(key));
                Ok(ScreenAction::Continue)
            }
            // Approval keys apply only while the input is blank, so a draft or
            // `/quit` can still be typed; submission waits for the turn anyway.
            Event::Key(key) if self.approval.is_some() && self.input.is_blank() => {
                match key_binding::translate_approval(key) {
                    Some(answer) => {
                        self.answer_approval(answer);
                        Ok(ScreenAction::Continue)
                    }
                    None => self.apply_action(key_binding::translate(key), backend),
                }
            }
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
                self.settings_menu.reset();
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
        let text = self.input.contents();
        if action == KeyAction::Submit
            && !text.contains('\n')
            && matches!(command::parse(&text), ParsedCommand::Exit)
        {
            self.input.clear();
            self.settings_menu.reset();
            return Ok(ScreenAction::Exit);
        }
        if !self.settings_menu.dismissed
            && !CommandMenu::matching(&text, CommandContext::Chat).is_empty()
        {
            match action {
                KeyAction::Edit(input::EditorKey::Up) => {
                    self.settings_menu.navigate(&text, CommandContext::Chat, -1);
                    return Ok(ScreenAction::Continue);
                }
                KeyAction::Edit(input::EditorKey::Down) => {
                    self.settings_menu.navigate(&text, CommandContext::Chat, 1);
                    return Ok(ScreenAction::Continue);
                }
                KeyAction::Cancel => {
                    self.settings_menu.dismissed = true;
                    return Ok(ScreenAction::Continue);
                }
                KeyAction::Submit
                    if self
                        .settings_menu
                        .chosen(&text, CommandContext::Chat)
                        .is_some()
                        && (self.settings_menu.navigated
                            || text == "/"
                            || matches!(command::parse(&text), ParsedCommand::Unknown { .. })) =>
                {
                    // Preserve the draft and active turn, just like typed
                    // slash commands, until the current response finishes.
                    let command = self
                        .settings_menu
                        .chosen(&text, CommandContext::Chat)
                        .expect("matched command");
                    if self.response_in_progress
                        && !matches!(command::parse(command), ParsedCommand::Exit)
                    {
                        return Ok(ScreenAction::Continue);
                    }
                    self.input.clear();
                    self.settings_menu.reset();
                    return self.dispatch_command(command, backend);
                }
                _ => {}
            }
        }
        match action {
            KeyAction::Ignore => Ok(ScreenAction::Continue),
            KeyAction::Cancel => {
                // A selection or a pending approval owns Esc first; with
                // neither, Esc stops the streaming response.
                if self.response_in_progress
                    && self.approval.is_none()
                    && self.text_selection.is_empty()
                {
                    self.cancel_response(backend);
                } else {
                    self.text_selection.clear();
                    self.copy_status = None;
                }
                Ok(ScreenAction::Continue)
            }
            KeyAction::CopySelection => {
                self.copy_selection(self.preferences.copy);
                Ok(ScreenAction::Continue)
            }
            KeyAction::EnterSelectionMode => {
                self.enter_selection_mode();
                Ok(ScreenAction::Continue)
            }
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
                self.recorder.thinking_changed(self.thinking.as_str());
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
                self.settings_menu.reset();
                self.input.apply(key, self.transcript_width);
                Ok(ScreenAction::Continue)
            }
        }
    }

    /// Transcript row shown at the top of the viewport.
    fn visible_start(&mut self) -> usize {
        self.row_count()
            .saturating_sub(self.scroll_from_bottom)
            .saturating_sub(self.transcript_height)
    }

    /// One auto-scroll step for a drag held on a viewport edge. Returns
    /// whether the viewport moved; the selection focus follows the pointer.
    fn auto_scroll(&mut self) -> bool {
        let Some(direction) = self.text_selection.edge(self.transcript_area) else {
            return false;
        };
        let before = self.scroll_from_bottom;
        match direction {
            selection::AutoScroll::Older => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(1);
                self.clamp_scroll();
            }
            selection::AutoScroll::Newer => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(1);
            }
        }
        if self.scroll_from_bottom == before {
            return false;
        }
        let count = self.row_count();
        let start = self.visible_start();
        self.text_selection
            .follow(self.transcript_area, start, count);
        true
    }

    /// Per-frame work that is not driven by an event.
    fn tick(&mut self) {
        if self.text_selection.edge(self.transcript_area).is_none() {
            self.auto_scroll_at = None;
            return;
        }
        let due = self
            .auto_scroll_at
            .is_none_or(|last| last.elapsed() >= AUTO_SCROLL_INTERVAL);
        if due && self.auto_scroll() {
            self.auto_scroll_at = Some(Instant::now());
        }
    }

    fn enter_selection_mode(&mut self) {
        let count = self.row_count();
        if count == 0 {
            self.push_notice("nothing to select: the transcript is empty".to_string());
            return;
        }
        self.settings_menu.reset();
        self.copy_status = None;
        // Start on the newest visible row; the draft is left untouched.
        let last_visible = count
            .saturating_sub(self.scroll_from_bottom)
            .saturating_sub(1);
        self.text_selection.enter_keyboard(last_visible);
    }

    fn apply_selection_key(&mut self, key: key_binding::SelectionKey) {
        use key_binding::SelectionKey;
        match key {
            SelectionKey::Move(movement) => {
                self.sync_layout();
                let page = self.transcript_height.max(1);
                self.text_selection
                    .move_cursor(movement, self.layout.rows(), page);
                self.scroll_to_cursor();
            }
            SelectionKey::Visual(mode) => {
                self.sync_layout();
                self.text_selection.toggle_visual(mode, self.layout.rows());
            }
            SelectionKey::Yank | SelectionKey::YankRaw => {
                if self.text_selection.visual_active() {
                    // The highlight stays, as after a mouse copy, so a failed
                    // clipboard write can be retried with Ctrl+Y.
                    let form = if matches!(key, SelectionKey::YankRaw) {
                        preferences::CopyForm::Raw
                    } else {
                        self.preferences.copy
                    };
                    self.copy_selection(form);
                    self.text_selection.leave_keyboard();
                }
            }
            SelectionKey::Open => {
                if let Some(cursor) = self.text_selection.cursor() {
                    self.activate_target_at(cursor.row, cursor.column);
                }
            }
            SelectionKey::Exit => {
                self.text_selection.clear();
                self.copy_status = None;
            }
            SelectionKey::Ignore => {}
        }
    }

    /// Scrolls just enough to keep the keyboard cursor inside the viewport.
    fn scroll_to_cursor(&mut self) {
        let Some(cursor) = self.text_selection.cursor() else {
            return;
        };
        let count = self.row_count();
        let height = self.transcript_height.max(1);
        let start = self.visible_start();
        if cursor.row < start {
            self.scroll_from_bottom = count.saturating_sub(height).saturating_sub(cursor.row);
        } else if cursor.row >= start + height {
            self.scroll_from_bottom = count.saturating_sub(cursor.row + 1);
        }
        self.clamp_scroll();
    }

    fn copy_selection(&mut self, form: preferences::CopyForm) {
        self.sync_layout();
        let rows = self.layout.rows();
        let text = match form {
            preferences::CopyForm::Rendered => self.text_selection.text(rows),
            preferences::CopyForm::Raw => self.text_selection.raw_text(rows),
        };
        if let Some(text) = text {
            self.copy_label = match form {
                preferences::CopyForm::Rendered => "Selection copied",
                preferences::CopyForm::Raw => "Selection copied (raw markdown)",
            };
            self.pending_copy = Some(text);
            self.poll_clipboard();
        }
    }

    /// Act on the target under a transcript cell: open a URL, open a file in
    /// the editor, or copy a code block. Nothing happens on plain text.
    fn activate_target_at(&mut self, row: usize, column: usize) {
        self.sync_layout();
        let Some(target) = self
            .layout
            .rows()
            .get(row)
            .and_then(|row| row.target_at(column))
            .cloned()
        else {
            return;
        };
        match target {
            transcript::render::Target::Url(url) => {
                let env = crate::clipboard::Environment::detect();
                match open::url_argv(&env, &url) {
                    Some(argv) => match open::spawn(&argv) {
                        Ok(()) => self.push_notice(format!("opened {url}")),
                        Err(reason) => self.push_alert(format!("{reason} for {url}")),
                    },
                    None => self.push_alert(format!("not opened: {url}")),
                }
            }
            transcript::render::Target::File { path, line, column } => {
                let editor = open::editor_argv(
                    self.preferences.editor.as_deref(),
                    |name| std::env::var(name).ok().filter(|v| !v.is_empty()),
                    on_path,
                    &path,
                    line,
                    column,
                );
                let place = match line {
                    Some(line) => format!("{path}:{line}"),
                    None => path.clone(),
                };
                match editor {
                    open::Editor::Argv(argv) => match open::spawn(&argv) {
                        Ok(()) => self.push_notice(format!("opened {place} in {}", argv[0])),
                        Err(reason) => self.push_alert(format!("{reason} for {place}")),
                    },
                    open::Editor::Terminal(name) => self.push_alert(format!(
                        "{name} runs in the terminal and cannot open over chat; /editor code|cursor|zed|subl or an argv template"
                    )),
                    open::Editor::None => self.push_alert(
                        "no editor: set /editor code|cursor|zed|subl or an argv template with {path} {line} {column}".to_string(),
                    ),
                }
            }
            transcript::render::Target::Copy(text) => {
                self.copy_label = "Code block copied";
                self.pending_copy = Some(text.to_string());
                self.poll_clipboard();
            }
        }
    }

    fn poll_clipboard(&mut self) {
        if let Some(receiver) = &self.copy_receiver {
            match receiver.try_recv() {
                Ok(true) => self.copy_status = Some((self.copy_label, Instant::now())),
                Ok(false) | Err(TryRecvError::Disconnected) => {
                    self.copy_status =
                        Some(("Clipboard unavailable; Ctrl+Y retries", Instant::now()))
                }
                Err(TryRecvError::Empty) => return,
            }
            self.copy_receiver = None;
        }
        if let Some(text) = self.pending_copy.take() {
            let (sender, receiver) = std::sync::mpsc::channel();
            self.copy_receiver = Some(receiver);
            self.copy_status = Some(("Copying selection…", Instant::now()));
            std::thread::spawn(move || {
                let _ = sender.send(crate::clipboard::copy(&text));
            });
        }
    }

    fn poll_backend(&mut self) {
        if let Some(receiver) = self.receiver.take() {
            loop {
                match receiver.try_recv() {
                    Ok(event) => {
                        let finished = self.apply_event(event);
                        if finished {
                            self.response_in_progress = false;
                            self.assistant_open = false;
                            break;
                        }
                    }
                    Err(TryRecvError::Empty) => {
                        self.receiver = Some(receiver);
                        break;
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.response_in_progress = false;
                        self.assistant_open = false;
                        break;
                    }
                }
            }
        }
        self.poll_location();
    }

    /// Land a finished Git status probe, then start the one a drained batch
    /// of tool returns asked for. One probe at a time, never on this thread.
    fn poll_location(&mut self) {
        if let Some(receiver) = &self.location_receiver {
            match receiver.try_recv() {
                Ok((directory, git_status)) => {
                    self.location_receiver = None;
                    self.location_directory = directory;
                    self.location_git = git_status;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.location_receiver = None,
            }
        }
        if self.location_dirty && self.location_receiver.is_none() {
            self.location_dirty = false;
            self.refresh_location();
        }
    }

    fn draw(&mut self, frame: &mut ratatui_core::terminal::Frame) {
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
        if self.transcript_width != chunks[0].width as usize {
            self.text_selection.clear();
        }
        self.transcript_width = chunks[0].width as usize;
        self.transcript_height = chunks[0].height as usize;
        self.clamp_scroll();
        self.text_selection.clamp_cursor(self.layout.rows());
        let row_count = self.layout.len();
        let (mut visible, visible_start) = transcript::render::visible_lines(
            self.layout.rows(),
            self.transcript_width,
            self.transcript_height,
            self.scroll_from_bottom,
        );
        self.text_selection.highlight(&mut visible, visible_start);
        frame.render_widget(Paragraph::new(Text::from(visible)), chunks[0]);
        let max_scroll = row_count.saturating_sub(self.transcript_height);
        if self.mouse_capture {
            self.scroll_navigation.draw(
                frame,
                chunks[0],
                self.scroll_from_bottom < max_scroll,
                self.scroll_from_bottom > 0,
            );
        }

        let activity = if let Some((status, _)) = self
            .copy_status
            .filter(|(_, when)| when.elapsed() < Duration::from_secs(4))
        {
            format!("{status} ")
        } else if self.text_selection.keyboard_active() {
            let mode = match self.text_selection.visual_mode() {
                Some(selection::VisualMode::Char) => "VISUAL",
                Some(selection::VisualMode::Line) => "VISUAL LINE",
                None => "SELECT",
            };
            if chunks[1].width < 48 {
                format!("{mode} · Esc exits ")
            } else {
                format!("{mode} · h/j/k/l w/b move · v/V select · y yank · Esc exits ")
            }
        } else if let Some(pending) = &self.approval {
            // Approval keys apply only to a blank input; say so instead of
            // advertising keys that would edit the draft.
            if self.input.is_blank() {
                pending.hint()
            } else {
                "Tool approval waiting: clear the draft (Ctrl+C) to answer ".to_string()
            }
        } else if self.response_in_progress {
            let spinner = SPINNER[self.spinner_frame % SPINNER.len()];
            let verb = match self.reconnecting {
                Some((attempt, attempts)) => format!("Reconnecting {attempt}/{attempts}"),
                None => "Working".to_string(),
            };
            let clock = self
                .turn_started
                .map(|started| format!(" · {}", transcript::theme::duration(started.elapsed())))
                .unwrap_or_default();
            if chunks[1].width < 12 {
                format!("{spinner} ")
            } else if chunks[1].width < 48 {
                format!("{spinner} {verb}{clock} ")
            } else {
                format!("{spinner} {verb}{clock} · Esc cancels ")
            }
        } else if self.resend_available && self.input.is_blank() {
            "Enter resends the last message ".to_string()
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
        let selection_cursor = self
            .text_selection
            .cursor()
            .filter(|cursor| {
                cursor.row >= visible_start && cursor.row < visible_start + self.transcript_height
            })
            .map(|cursor| {
                (
                    chunks[0].x
                        + (cursor
                            .column
                            .min(chunks[0].width.saturating_sub(1) as usize))
                            as u16,
                    chunks[0].y + (cursor.row - visible_start) as u16,
                )
            });
        if let Some(position) = selection_cursor {
            frame.set_cursor_position(position);
        } else if chunks[2].width > 0 && chunks[2].height > 0 {
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
        let usage_cost = self
            .meters
            .cost_usd
            .map(|cost| format!("reported usage ${cost:.6} · "))
            .unwrap_or_default();
        let settings_status = format!(
            " {usage_cost}{}/{} · thinking {} · mouse {} ",
            self.selection.gateway,
            self.selection.model,
            self.thinking.as_str(),
            if self.mouse_capture { "on" } else { "off" },
        );
        self.settings_menu.draw(
            frame,
            chunks[0],
            &self.input.contents(),
            CommandContext::Chat,
            &settings_status,
        );
    }

    fn submit_input(&mut self, backend: &mut dyn CompletionBackend) -> Result<ScreenAction> {
        // Keep the draft intact until the active turn ends; never launch a second
        // billed request while discarding the first turn's receiver.
        if self.response_in_progress {
            return Ok(ScreenAction::Continue);
        }
        let text = self.input.contents();
        if text.trim().is_empty() {
            // A blank Enter after a connection failure sends the same
            // transcript again; otherwise it does nothing.
            if self.resend_available {
                self.send_transcript(backend);
            }
            return Ok(ScreenAction::Continue);
        }
        self.input.clear();
        self.settings_menu.reset();
        if text.trim_start().starts_with('/') {
            return self.dispatch_command(text.trim(), backend);
        }
        self.push_message(ChatMessage::new(TranscriptRole::Operator, text));
        self.scroll_from_bottom = 0;
        self.send_transcript(backend);
        Ok(ScreenAction::Continue)
    }

    /// Start a turn from the transcript as it stands. Screen-only rows stay
    /// behind.
    fn send_transcript(&mut self, backend: &mut dyn CompletionBackend) {
        self.resend_available = false;
        self.refresh_location();
        // The operator's line is on disk before the request goes out. A
        // resend after a transport failure is the same turn again.
        if let Some(prompt) = self
            .messages
            .iter()
            .rev()
            .find(|message| !message.role.is_notice())
            .filter(|message| message.role == TranscriptRole::Operator)
            .map(|message| message.text.clone())
        {
            self.recorder.begin_turn(
                &session::Store::new(&self.state_dir),
                &self.workspace_root,
                &self.selection,
                self.thinking.as_str(),
                &prompt,
            );
            self.surface_recording_failure();
        }
        let request = ChatRequest {
            selection: self.selection.clone(),
            thinking: self.thinking,
            messages: self
                .messages
                .iter()
                .filter(|message| !message.role.is_notice())
                .cloned()
                .collect(),
        };
        self.meters.context_tokens = None;
        self.meters.context_window = None;
        self.receiver = Some(backend.submit(request));
        self.response_in_progress = true;
        self.assistant_open = false;
        self.turn_started = Some(Instant::now());
        self.report_status(crate::program_status::State::Working, "");
    }

    /// The footer row that closes a turn: `done in 12s · 14:02`. A notice
    /// with `elapsed` set is what the formatter right-aligns and dims.
    fn push_turn_footer(&mut self, verb: &str) {
        let Some(started) = self.turn_started.take() else {
            return;
        };
        let elapsed = started.elapsed();
        let now = crate::util::rfc3339::unix_now();
        let stamp = match self.preferences.clock {
            preferences::Clock::Utc => format!("{}Z", crate::util::localtime::hhmm(now, 0)),
            preferences::Clock::Local => match crate::util::localtime::utc_offset(now) {
                Some(offset) => crate::util::localtime::hhmm(now, offset),
                None => format!("{}Z", crate::util::localtime::hhmm(now, 0)),
            },
        };
        let mut footer = ChatMessage::new(
            TranscriptRole::Notice,
            format!("{verb} {} · {stamp}", transcript::theme::duration(elapsed)),
        );
        footer.elapsed = Some(elapsed);
        self.push_message(footer);
    }

    /// The sidebar row for this chat: directory and model.
    fn status_title(&self) -> String {
        let directory = self
            .workspace_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".to_string());
        format!("{directory} · {}", self.selection.model)
    }

    fn report_status(&self, state: crate::program_status::State, msg: &str) {
        self.status.state(state, &self.status_title(), msg);
    }

    fn dispatch_command(
        &mut self,
        text: &str,
        backend: &mut dyn CompletionBackend,
    ) -> Result<ScreenAction> {
        match command::parse(text) {
            ParsedCommand::ReopenModelPicker => Ok(ScreenAction::ReopenModelPicker),
            ParsedCommand::OpenCatalog => Ok(ScreenAction::OpenCatalog),
            ParsedCommand::Login => Ok(ScreenAction::Login),
            ParsedCommand::Gateway => Ok(ScreenAction::Gateway),
            ParsedCommand::Exit => Ok(ScreenAction::Exit),
            ParsedCommand::Clear => {
                // Enter waits for the turn, so this is defensive: a cleared
                // transcript must never leave a worker streaming into it.
                if self.response_in_progress {
                    backend.cancel();
                    self.recorder.cancel_turn();
                }
                self.recorder.reset();
                self.messages.clear();
                self.dirty = transcript::render::Dirty::From(0);
                self.text_selection.clear();
                self.meters.clear_counts();
                self.scroll_from_bottom = 0;
                self.receiver = None;
                self.approval = None;
                self.response_in_progress = false;
                self.assistant_open = false;
                self.resend_available = false;
                Ok(ScreenAction::Continue)
            }
            ParsedCommand::SetMouse(enabled) => {
                self.mouse_capture = enabled;
                self.text_selection.clear();
                // Clear old pointer coordinates and hit targets across modes.
                self.scroll_navigation = scroll_navigation::ScrollNavigation::default();
                self.show_mouse_mode();
                Ok(ScreenAction::Continue)
            }
            ParsedCommand::ShowMouse => {
                self.show_mouse_mode();
                Ok(ScreenAction::Continue)
            }
            ParsedCommand::Copy(form) => {
                if let Some(form) = form {
                    self.preferences.copy = form;
                    preferences::write(&self.state_dir, &self.preferences)?;
                }
                self.push_notice(match self.preferences.copy {
                    preferences::CopyForm::Rendered => {
                        "copy rendered: selections copy text as drawn; Y yanks raw markdown"
                            .to_string()
                    }
                    preferences::CopyForm::Raw => {
                        "copy raw: selections copy the model's markdown source".to_string()
                    }
                });
                Ok(ScreenAction::Continue)
            }
            ParsedCommand::Clock(clock) => {
                if let Some(clock) = clock {
                    self.preferences.clock = clock;
                    preferences::write(&self.state_dir, &self.preferences)?;
                }
                self.push_notice(match self.preferences.clock {
                    preferences::Clock::Local => "clock local".to_string(),
                    preferences::Clock::Utc => "clock utc".to_string(),
                });
                Ok(ScreenAction::Continue)
            }
            ParsedCommand::Editor(editor) => {
                if let Some(editor) = editor {
                    self.preferences.editor = Some(editor).filter(|e| !e.is_empty());
                    preferences::write(&self.state_dir, &self.preferences)?;
                }
                self.push_notice(match &self.preferences.editor {
                    Some(editor) => format!("editor {editor}"),
                    None => format!(
                        "editor unset: {}; /editor code|cursor|zed|subl|idea|hx or an argv template with {{path}} {{line}} {{column}}",
                        if on_path("code") { "code found on PATH" } else { "$AHI_EDITOR, $VISUAL, $EDITOR, then code on PATH" }
                    ),
                });
                Ok(ScreenAction::Continue)
            }
            ParsedCommand::Markdown(enabled) => {
                self.preferences.markdown = enabled;
                preferences::write(&self.state_dir, &self.preferences)?;
                self.dirty = transcript::render::Dirty::From(0);
                self.push_notice(if enabled {
                    "markdown on".to_string()
                } else {
                    "plain text: markdown is shown as written; /markdown renders again".to_string()
                });
                Ok(ScreenAction::Continue)
            }
            ParsedCommand::Select => {
                self.enter_selection_mode();
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
                self.recorder.thinking_changed(level.as_str());
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
        let previous_rows = (self.scroll_from_bottom > 0).then(|| self.row_count());
        if !matches!(event, ResponseEvent::Reconnecting { .. }) {
            self.reconnecting = None;
        }
        let finished = match event {
            ResponseEvent::Reconnecting { attempt, attempts } => {
                self.reconnecting = Some((attempt, attempts));
                false
            }
            ResponseEvent::TextDelta(text) => {
                self.recorder.text(&text);
                if !self.assistant_open {
                    self.push_message(ChatMessage::new(TranscriptRole::Assistant, ""));
                    self.assistant_open = true;
                }
                if let Some(message) = self.messages.last_mut() {
                    message.text.push_str(&text);
                }
                self.mark_appended_last();
                false
            }
            ResponseEvent::ToolCallsRequested { calls, native } => {
                // The calls end the assistant message that requested them and
                // are replayed with their results on later requests. They are
                // on disk before any of them runs.
                self.recorder.calls(&calls, native.as_ref());
                if !self.assistant_open {
                    self.push_message(ChatMessage::new(TranscriptRole::Assistant, ""));
                }
                let names: Vec<&str> = calls.iter().map(|call| call.name.as_str()).collect();
                self.report_status(crate::program_status::State::Working, &names.join(", "));
                let last = self.messages.len().saturating_sub(1);
                if let Some(message) = self.messages.last_mut() {
                    message.tool = Some(ToolPayload::Calls { calls, native });
                }
                self.mark_dirty(last);
                self.assistant_open = false;
                false
            }
            ResponseEvent::ToolApprovalRequested(request) => {
                let waiting = request
                    .calls
                    .first()
                    .map(|call| format!("{} {}", call.tool, call.target))
                    .unwrap_or_default();
                self.status.blocked(
                    crate::program_status::Kind::Permission,
                    &self.status_title(),
                    &waiting,
                );
                self.push_notice(String::new());
                let pending = approval::PendingApproval::new(request, self.messages.len() - 1);
                self.messages[pending.notice_index].text = pending.prompt();
                self.mark_dirty(pending.notice_index);
                // Replacing an unanswered approval drops its reply: a denial.
                self.approval = Some(pending);
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
                // Refresh once per drained batch, never on the rendering timer.
                self.location_dirty = true;
                false
            }
            ResponseEvent::ToolResult {
                call_id,
                tool,
                output,
                content,
                exit_code,
                detail,
                elapsed,
                record: _,
            } => {
                self.recorder
                    .tool_result(&call_id, &tool, &output, &content, exit_code);
                let mut message = ChatMessage::new(TranscriptRole::Tool { exit_code }, output);
                message.tool = Some(ToolPayload::Result {
                    call_id,
                    name: tool,
                    content,
                });
                message.detail = detail;
                message.elapsed = Some(elapsed);
                self.push_message(message);
                self.assistant_open = false;
                false
            }
            ResponseEvent::Completed => {
                self.approval = None;
                self.recorder.end_turn();
                let answer = self
                    .messages
                    .iter()
                    .rev()
                    .find(|message| matches!(message.role, TranscriptRole::Assistant))
                    .map(|message| message.text.clone())
                    .unwrap_or_default();
                self.report_status(crate::program_status::State::Done, &answer);
                self.push_turn_footer("done in");
                true
            }
            ResponseEvent::Failed { message, kind } => {
                self.approval = None;
                self.recorder
                    .fail_turn(&format!("{kind:?}").to_ascii_lowercase(), &message);
                // A connection failure left the transcript as the operator
                // sent it (or ending in tool results): the same request can
                // go again. A partial reply at the tail cannot.
                self.resend_available = kind == crate::error::ErrorKind::Transport
                    && self
                        .messages
                        .iter()
                        .rev()
                        .find(|message| !message.role.is_notice())
                        .is_some_and(|message| {
                            matches!(
                                message.role,
                                TranscriptRole::Operator | TranscriptRole::Tool { .. }
                            )
                        });
                self.report_status(
                    crate::program_status::State::Error,
                    &format!("{kind:?}").to_ascii_lowercase(),
                );
                self.push_alert(message);
                self.turn_started = None;
                true
            }
        };
        if let Some(previous_rows) = previous_rows {
            let current_rows = self.row_count();
            self.scroll_from_bottom = self
                .scroll_from_bottom
                .saturating_add(current_rows.saturating_sub(previous_rows));
        }
        self.surface_recording_failure();
        finished
    }

    fn show_mouse_mode(&mut self) {
        self.push_notice(if self.mouse_capture {
            "mouse on: wheel and jump controls enabled; drag to select/copy, Ctrl+Y copies again, Ctrl+S selects by keyboard".to_string()
        } else {
            "mouse off: select/copy text with your terminal or Ctrl+S; use Page Up/Page Down to scroll, /mouse on for mouse controls".to_string()
        });
    }

    fn push_notice(&mut self, text: String) {
        self.push_message(ChatMessage::new(TranscriptRole::Notice, text));
    }

    fn push_alert(&mut self, text: String) {
        self.push_message(ChatMessage::new(TranscriptRole::Alert, text));
    }

    /// Esc during a response. The partial reply stays in the transcript and
    /// is sent with the next message, as other harnesses do; an assistant
    /// message that never received text is removed so no provider sees an
    /// empty turn. The receiver is dropped at once: the worker stops at its
    /// next read, request, or tool round and closes the connection.
    fn cancel_response(&mut self, backend: &mut dyn CompletionBackend) {
        backend.cancel();
        self.recorder.cancel_turn();
        self.receiver = None;
        self.response_in_progress = false;
        self.approval = None;
        // The alert below says what happened; no footer row for a cancel.
        self.turn_started = None;
        let partial = self.assistant_open
            && self
                .messages
                .last()
                .is_some_and(|message| message.role == TranscriptRole::Assistant);
        let kept = if partial {
            let empty = self
                .messages
                .last()
                .is_some_and(|message| message.text.is_empty() && message.tool.is_none());
            if empty {
                self.messages.pop();
                self.mark_dirty(self.messages.len());
            }
            !empty
        } else {
            false
        };
        self.assistant_open = false;
        self.push_alert(if kept {
            "response cancelled; the partial reply above stays in the transcript".to_string()
        } else {
            "response cancelled".to_string()
        });
        self.scroll_from_bottom = 0;
    }

    fn answer_approval(&mut self, answer: approval::ApprovalKey) {
        let Some(pending) = self.approval.as_mut() else {
            return;
        };
        let complete = pending.apply(answer);
        let prompt = pending.prompt();
        let notice_index = pending.notice_index;
        if let Some(message) = self.messages.get_mut(notice_index) {
            message.text = prompt;
        }
        self.mark_dirty(notice_index);
        if complete {
            if let Some(pending) = self.approval.take() {
                pending.finish();
            }
            self.report_status(crate::program_status::State::Working, "");
        }
        self.scroll_from_bottom = 0;
    }

    /// Probe the working directory and Git status on a worker thread. The
    /// footer keeps its last snapshot until the probe lands, so `git status`
    /// on a large tree never stalls a keystroke or a frame.
    fn refresh_location(&mut self) {
        if self.location_receiver.is_some() {
            self.location_dirty = true;
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(location::current());
        });
        self.location_receiver = Some(receiver);
    }

    /// Block until the in-flight Git status probe lands. Tests only.
    #[cfg(test)]
    fn wait_for_location(&mut self) {
        if let Some(receiver) = self.location_receiver.take() {
            if let Ok((directory, git_status)) = receiver.recv() {
                self.location_directory = directory;
                self.location_git = git_status;
            }
        }
    }

    fn clamp_scroll(&mut self) {
        let count = self.row_count();
        transcript::render::clamp_scroll(
            &mut self.scroll_from_bottom,
            count,
            self.transcript_height,
        );
    }
}

fn rule_line(width: usize) -> String {
    let hint = "/ commands ";
    format!(
        "{}{}",
        hint.chars().take(width).collect::<String>(),
        "─".repeat(width.saturating_sub(hint.len()))
    )
}

/// Validate an explicit `--tools` policy file before any screen opens, so
/// a typo fails at startup instead of on every chat turn.
pub(crate) fn check_actions_file(path: &Path) -> Result<()> {
    crate::tool_policy::load_file(path, &backend::tools::BUILTIN).map(drop)
}

/// Run the chat screen until the operator exits or asks for the model picker.
/// `actions` is the operator's explicit policy file, used for every turn in
/// place of the working directory's `ahi.tools.yaml`.
/// `record` is false for `ahi tui --no-history`.
pub(crate) fn interact(
    state_dir: &Directories,
    selection: &str,
    actions: Option<&Path>,
    record: bool,
    reopen_picker: impl FnMut(
        PickerView,
        &str,
        Option<crate::harness::GatewayRegistration>,
    ) -> Result<PickerOutcome>,
) -> Result<()> {
    let screen = ChatScreen::load_recording(
        state_dir,
        selection,
        session::recorder::Recorder::new(record, "tui"),
    );
    interact_screen(state_dir, screen, actions, reopen_picker)
}

/// `ahi tui --resume <id>`: the chat over a stored session, continuing it
/// with its own model and thinking level.
pub(crate) fn interact_resumed(
    state_dir: &Directories,
    session_id: &str,
    actions: Option<&Path>,
    reopen_picker: impl FnMut(
        PickerView,
        &str,
        Option<crate::harness::GatewayRegistration>,
    ) -> Result<PickerOutcome>,
) -> Result<()> {
    let stored = session::Store::new(state_dir)
        .open(session_id)
        .map_err(|err| Error::new(err.to_string()))?;
    let screen = ChatScreen::load_resumed(state_dir, stored);
    interact_screen(state_dir, screen, actions, reopen_picker)
}

fn interact_screen(
    state_dir: &Directories,
    mut screen: ChatScreen,
    actions: Option<&Path>,
    mut reopen_picker: impl FnMut(
        PickerView,
        &str,
        Option<crate::harness::GatewayRegistration>,
    ) -> Result<PickerOutcome>,
) -> Result<()> {
    screen.push_header();
    screen.report_status(crate::program_status::State::Idle, "");
    let mut backend =
        HttpBackend::new(state_dir.clone()).with_actions(actions.map(Path::to_path_buf));
    loop {
        let mut terminal =
            crate::term::try_init().map_err(|err| Error::new(format!("terminal: {err}")))?;
        let outcome =
            crossterm::execute!(std::io::stdout(), EnableBracketedPaste, DisableMouseCapture)
                .map_err(|err| Error::new(format!("enable paste: {err}")))
                .and_then(|()| session_loop(&mut terminal, &mut screen, &mut backend));
        let cleanup = crossterm::execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste
        );
        crate::term::restore();
        cleanup.map_err(|err| Error::new(format!("disable paste: {err}")))?;
        let outcome = outcome?;
        match outcome {
            SessionEnd::Exit => {
                screen.status.clear();
                return Ok(());
            }
            SessionEnd::Login => {
                let notice = match crate::tui::login::run(state_dir) {
                    Ok(()) => "OpenAI login completed; model selection unchanged".to_string(),
                    Err(error) => error.to_string(),
                };
                screen.push_notice(notice);
            }
            SessionEnd::Gateway => match crate::tui::gateway::run(state_dir) {
                Ok(Some(registration)) => {
                    screen.push_notice(registration.notice.clone());
                    let provider = registration.provider_id.clone();
                    match reopen_picker(PickerView::Catalog, &provider, Some(registration))? {
                        PickerOutcome::Selected(next) => screen.set_selection(&next),
                        PickerOutcome::Back => {}
                        PickerOutcome::Exit => {
                            screen.status.clear();
                            return Ok(());
                        }
                    }
                }
                Ok(None) => screen.push_notice("Gateway discovery cancelled".to_string()),
                Err(error) => screen.push_notice(error.to_string()),
            },
            SessionEnd::ReopenModelPicker | SessionEnd::OpenCatalog => {
                let view = match outcome {
                    SessionEnd::OpenCatalog => PickerView::Catalog,
                    _ => PickerView::Models,
                };
                match reopen_picker(view, &screen.selection.gateway, None)? {
                    PickerOutcome::Selected(next) => screen.set_selection(&next),
                    PickerOutcome::Back => {}
                    PickerOutcome::Exit => {
                        screen.status.clear();
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn session_loop(
    terminal: &mut crate::term::DefaultTerminal,
    screen: &mut ChatScreen,
    backend: &mut dyn CompletionBackend,
) -> Result<SessionEnd> {
    // Each terminal entry starts with capture disabled, including picker return.
    let mut mouse_capture = false;
    loop {
        if screen.mouse_capture != mouse_capture {
            if screen.mouse_capture {
                crossterm::execute!(std::io::stdout(), EnableMouseCapture)
            } else {
                crossterm::execute!(std::io::stdout(), DisableMouseCapture)
            }
            .map_err(|err| Error::new(format!("mouse mode: {err}")))?;
            mouse_capture = screen.mouse_capture;
        }
        {
            // Clipboard workers may emit OSC 52. Keep complete render sequences
            // together so a copy cannot split an ANSI command mid-frame.
            let stdout = std::io::stdout();
            let _output = stdout.lock();
            terminal
                .draw(|frame| screen.draw(frame))
                .map_err(|err| Error::new(format!("terminal draw: {err}")))?;
        }
        if crossterm::event::poll(FRAME_INTERVAL).map_err(|err| Error::new(err.to_string()))? {
            let event = crossterm::event::read().map_err(|err| Error::new(err.to_string()))?;
            match screen.apply_terminal_event(event, backend)? {
                ScreenAction::Continue => {}
                ScreenAction::Exit => return Ok(SessionEnd::Exit),
                ScreenAction::ReopenModelPicker => {
                    return Ok(SessionEnd::ReopenModelPicker);
                }
                ScreenAction::OpenCatalog => return Ok(SessionEnd::OpenCatalog),
                ScreenAction::Login => return Ok(SessionEnd::Login),
                ScreenAction::Gateway => return Ok(SessionEnd::Gateway),
            }
        }
        screen.tick();
        screen.poll_clipboard();
        screen.poll_backend();
    }
}

/// Whether `program` is on `PATH`.
fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(program);
        candidate.is_file()
            || (cfg!(windows)
                && ["exe", "cmd", "bat"]
                    .iter()
                    .any(|ext| candidate.with_extension(ext).is_file()))
    })
}

/// Whether a workspace-relative path from the transcript names an existing
/// file or directory. Paths with a parent component never match, so a model
/// cannot make text outside the workspace clickable.
fn file_exists(root: &std::path::Path, path: &str) -> bool {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains("..")
        || path.contains('\\')
        || path.chars().any(char::is_control)
    {
        return false;
    }
    root.join(path).symlink_metadata().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_core::backend::TestBackend;
    use ratatui_core::terminal::Terminal;
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

    /// A turn that never finishes on its own; the test feeds its events.
    struct HangingBackend {
        requests: Vec<ChatRequest>,
        sender: Option<mpsc::Sender<ResponseEvent>>,
        cancels: usize,
    }

    impl CompletionBackend for HangingBackend {
        fn submit(&mut self, request: ChatRequest) -> Receiver<ResponseEvent> {
            self.requests.push(request);
            let (sender, receiver) = mpsc::channel();
            self.sender = Some(sender);
            receiver
        }

        fn cancel(&mut self) {
            self.cancels += 1;
        }
    }

    #[test]
    fn a_connection_failure_offers_a_resend_on_blank_enter_and_shows_reconnects() {
        use crate::error::ErrorKind;
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-resend");
        let mut screen = screen_in(&state);
        let mut backend = HangingBackend {
            requests: Vec::new(),
            sender: None,
            cancels: 0,
        };
        screen.set_input_text("explain");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        let sender = backend.sender.clone().unwrap();
        sender
            .send(ResponseEvent::Reconnecting {
                attempt: 2,
                attempts: 3,
            })
            .unwrap();
        screen.poll_backend();
        let rows = draw_lines(&mut screen, 100, 12);
        assert!(rows
            .iter()
            .any(|row| row.contains("Reconnecting 2/3 · 0.0s · Esc cancels")));
        sender
            .send(ResponseEvent::Failed {
                message: "cannot reach the gateway after 3 connection attempts".into(),
                kind: ErrorKind::Transport,
            })
            .unwrap();
        screen.poll_backend();
        assert!(!screen.response_in_progress());
        assert!(screen.reconnecting.is_none());
        assert!(screen.resend_available);
        assert_eq!(
            screen.messages().last().unwrap().role,
            TranscriptRole::Alert
        );
        let rows = draw_lines(&mut screen, 100, 12);
        assert!(rows
            .iter()
            .any(|row| row.contains("Enter resends the last message")));
        assert!(!rows.iter().any(|row| row.contains("Reconnecting")));

        // Blank Enter sends the same transcript again, without a new row.
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert_eq!(backend.requests.len(), 2);
        assert_eq!(backend.requests[1].messages, backend.requests[0].messages);
        assert!(screen.response_in_progress());
        assert!(!screen.resend_available);
        let roles: Vec<_> = screen.messages().iter().map(|m| m.role).collect();
        assert_eq!(roles, [TranscriptRole::Operator, TranscriptRole::Alert]);

        // Other kinds, and a transport failure after a partial reply, offer
        // nothing: the tail is not a request the operator sent.
        let sender = backend.sender.clone().unwrap();
        sender
            .send(ResponseEvent::TextDelta("partial".into()))
            .unwrap();
        sender
            .send(ResponseEvent::Failed {
                message: "model response stream interrupted".into(),
                kind: ErrorKind::Transport,
            })
            .unwrap();
        screen.poll_backend();
        assert!(!screen.resend_available);
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert_eq!(backend.requests.len(), 2);
        screen.set_input_text("again");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        backend
            .sender
            .clone()
            .unwrap()
            .send(ResponseEvent::Failed {
                message: "model request failed (HTTP 401)".into(),
                kind: ErrorKind::Auth,
            })
            .unwrap();
        screen.poll_backend();
        assert!(!screen.resend_available);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn esc_cancels_a_streaming_response_and_keeps_the_partial_reply() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-cancel");
        let mut screen = screen_in(&state);
        let mut backend = HangingBackend {
            requests: Vec::new(),
            sender: None,
            cancels: 0,
        };
        screen.set_input_text("explain");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert!(screen.response_in_progress());
        let hint = draw_lines(&mut screen, 100, 12);
        assert!(hint
            .iter()
            .any(|row| row.contains("Working · 0.0s · Esc cancels")));
        backend
            .sender
            .as_ref()
            .unwrap()
            .send(ResponseEvent::TextDelta("partial ans".to_string()))
            .unwrap();
        screen.poll_backend();
        assert_eq!(screen.messages().last().unwrap().text, "partial ans");

        screen
            .apply_action(KeyAction::Cancel, &mut backend)
            .unwrap();
        assert_eq!(backend.cancels, 1);
        assert!(!screen.response_in_progress());
        assert!(screen.receiver.is_none());
        let roles: Vec<_> = screen.messages().iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            [
                TranscriptRole::Operator,
                TranscriptRole::Assistant,
                TranscriptRole::Alert
            ]
        );
        assert_eq!(
            screen.messages().last().unwrap().text,
            "response cancelled; the partial reply above stays in the transcript"
        );
        // Events from the abandoned turn no longer reach the screen.
        let _ = backend
            .sender
            .as_ref()
            .unwrap()
            .send(ResponseEvent::TextDelta(" more".to_string()));
        screen.poll_backend();
        assert_eq!(screen.messages()[1].text, "partial ans");
        let finished = draw_lines(&mut screen, 100, 12);
        assert!(!finished.iter().any(|row| row.contains("Working")));

        // The next message carries the partial reply and not the alert.
        screen.set_input_text("continue");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        let texts: Vec<_> = backend.requests[1]
            .messages
            .iter()
            .map(|m| (m.role, m.text.as_str()))
            .collect();
        assert_eq!(
            texts,
            [
                (TranscriptRole::Operator, "explain"),
                (TranscriptRole::Assistant, "partial ans"),
                (TranscriptRole::Operator, "continue"),
            ]
        );
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn esc_before_any_reply_leaves_no_empty_assistant_turn_and_yields_to_selection() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-cancel-early");
        let mut screen = screen_in(&state);
        let mut backend = HangingBackend {
            requests: Vec::new(),
            sender: None,
            cancels: 0,
        };
        screen.set_input_text("explain");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        // An assistant message opened by an empty delta is removed again.
        backend
            .sender
            .as_ref()
            .unwrap()
            .send(ResponseEvent::TextDelta(String::new()))
            .unwrap();
        screen.poll_backend();
        assert_eq!(screen.messages().len(), 2);
        // A live selection owns the first Esc; the response keeps running.
        screen
            .text_selection
            .begin(0, 0, Rect::new(0, 0, 80, 10), 0, 10);
        screen
            .apply_action(KeyAction::Cancel, &mut backend)
            .unwrap();
        assert_eq!(backend.cancels, 0);
        assert!(screen.response_in_progress());
        assert!(screen.text_selection.is_empty());
        screen
            .apply_action(KeyAction::Cancel, &mut backend)
            .unwrap();
        assert_eq!(backend.cancels, 1);
        let roles: Vec<_> = screen.messages().iter().map(|m| m.role).collect();
        assert_eq!(roles, [TranscriptRole::Operator, TranscriptRole::Alert]);
        assert_eq!(screen.messages().last().unwrap().text, "response cancelled");
        // Enter during a response keeps a command draft and cancels nothing;
        // Esc is the one key that ends the turn.
        screen.set_input_text("again");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        screen.set_input_text("/clear");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert_eq!(backend.cancels, 1);
        assert_eq!(screen.input_text(), "/clear");
        assert!(screen.response_in_progress());
        let _ = fs::remove_dir_all(state);
    }

    fn screen_in(state: &Path) -> ChatScreen {
        ChatScreen::load(&Directories::single_root(state), "openai/gpt-4.1")
    }

    #[test]
    fn the_screen_records_each_turn_and_a_cancel_closes_its_branch() {
        use crate::session::{Kind, Role, Status, StoredMessage};
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-record");
        let dirs = Directories::single_root(&state);
        let store = session::Store::new(&dirs);
        let mut screen = screen_in(&state);
        assert!(
            store.list().unwrap().0.is_empty(),
            "nothing before the first turn"
        );

        // A completed turn: operator line, answer, turn ok.
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.set_input_text("hello");
        press(&mut screen, &mut backend, crossterm::event::KeyCode::Enter);
        screen.apply_event(ResponseEvent::TextDelta("hi ".into()));
        screen.apply_event(ResponseEvent::TextDelta("there".into()));
        screen.apply_event(ResponseEvent::Completed);
        // The poll loop clears these when it sees the final event.
        screen.response_in_progress = false;
        screen.receiver = None;

        // A cancelled turn keeps its partial reply on a closed branch.
        screen.set_input_text("again");
        press(&mut screen, &mut backend, crossterm::event::KeyCode::Enter);
        screen.apply_event(ResponseEvent::TextDelta("half".into()));
        screen.cancel_response(&mut backend);

        let (rows, _) = store.list().unwrap();
        assert_eq!(rows.len(), 1);
        let stored = store.open(&rows[0].header.id).unwrap();
        assert_eq!(
            stored.header().cwd,
            screen.workspace_root.display().to_string()
        );
        assert_eq!(stored.header().model, "gpt-4.1");
        let kinds: Vec<String> = stored
            .entries()
            .iter()
            .map(|entry| match &entry.kind {
                Kind::Message(StoredMessage { role, text, .. }) => format!("{role:?} {text}"),
                Kind::Turn(turn) => format!("turn:{}", turn.status.as_str()),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "Operator hello",
                "Assistant hi there",
                "turn:ok",
                "Operator again",
                "Assistant half",
                "turn:cancelled",
            ]
        );
        assert_eq!(stored.active_parent(), Some(3));
        assert!(matches!(
            &stored.entries()[5].kind,
            Kind::Turn(turn) if turn.status == Status::Cancelled && turn.result["origin"] == "tui"
        ));
        assert!(matches!(
            &stored.entries()[0].kind,
            Kind::Message(StoredMessage {
                role: Role::Operator,
                ..
            })
        ));

        // /clear starts another session at the next turn; /model is recorded.
        screen.set_input_text("/clear");
        press(&mut screen, &mut backend, crossterm::event::KeyCode::Enter);
        screen.set_selection("openai/gpt-5");
        screen.set_input_text("third");
        press(&mut screen, &mut backend, crossterm::event::KeyCode::Enter);
        screen.apply_event(ResponseEvent::Completed);
        screen.response_in_progress = false;
        screen.receiver = None;
        let (rows, _) = store.list().unwrap();
        assert_eq!(rows.len(), 2);
        // Both sessions started within the same second, so pick by id.
        let newest = rows
            .iter()
            .find(|row| row.header.id != stored.id())
            .map(|row| store.open(&row.header.id).unwrap())
            .unwrap();
        assert_eq!(
            newest.header().model,
            "gpt-5",
            "the header carries the model at the first turn"
        );
        assert_eq!(newest.entries().len(), 2);

        // Without recording nothing is written.
        let mut quiet = ChatScreen::load_recording(
            &dirs,
            "openai/gpt-4.1",
            session::recorder::Recorder::new(false, "tui"),
        );
        quiet.set_input_text("silent");
        press(&mut quiet, &mut backend, crossterm::event::KeyCode::Enter);
        quiet.apply_event(ResponseEvent::Completed);
        assert_eq!(store.list().unwrap().0.len(), 2);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn a_resumed_screen_takes_the_recorded_settings_and_conversation() {
        use crate::session::{Header, Kind, Role, Status, StoredMessage, Turn};
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-resume");
        let dirs = Directories::single_root(&state);
        let store = session::Store::new(&dirs);
        let mut stored = store
            .create(Header::new("/w", "openai", "gpt-4.1", "low"))
            .unwrap();
        let message = |role, text: &str| {
            Kind::Message(StoredMessage {
                role,
                text: text.into(),
                tool: None,
            })
        };
        let turn = |run_id: &str, status| {
            Kind::Turn(Turn {
                run_id: run_id.into(),
                status,
                prompt_sha256: String::new(),
                result: serde_json::json!({}),
            })
        };
        stored
            .append(
                None,
                vec![
                    message(Role::Operator, "q1"),
                    message(Role::Assistant, "a1"),
                    turn("r1", Status::Ok),
                ],
            )
            .unwrap();
        stored
            .append(
                Some(3),
                vec![
                    Kind::ModelChange {
                        gateway: "openai".into(),
                        model: "gpt-5".into(),
                    },
                    Kind::ThinkingChange {
                        level: "high".into(),
                    },
                    message(Role::Operator, "q2"),
                    message(Role::Assistant, "partial"),
                    turn("r2", Status::Cancelled),
                ],
            )
            .unwrap();
        let id = stored.id().to_string();
        drop(stored);

        let mut screen = ChatScreen::load_resumed(&dirs, store.open(&id).unwrap());
        assert_eq!(
            screen.selection,
            parse_selection("openai/gpt-5"),
            "settings follow the changes"
        );
        assert_eq!(screen.thinking, ThinkingLevel::High);
        let texts: Vec<&str> = screen
            .messages()
            .iter()
            .filter(|m| !m.role.is_notice())
            .map(|m| m.text.as_str())
            .collect();
        assert_eq!(texts, ["q1", "a1"], "the cancelled branch is not replayed");
        assert!(screen
            .messages()
            .iter()
            .any(|m| m.role == TranscriptRole::Notice && m.text.contains("resumed session")));

        // The next turn hangs off the last success and sends the active path.
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.set_input_text("q3");
        press(&mut screen, &mut backend, crossterm::event::KeyCode::Enter);
        screen.apply_event(ResponseEvent::Completed);
        let sent: Vec<&str> = backend.requests[0]
            .messages
            .iter()
            .map(|m| m.text.as_str())
            .collect();
        assert_eq!(sent, ["q1", "a1", "q3"]);
        let after = store.open(&id).unwrap();
        assert_eq!(after.entries().len(), 10);
        assert_eq!(after.entries()[8].parent, Some(3));
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn the_chat_reports_its_state_to_the_multiplexer_with_osc_7501() {
        use crate::program_status::Reporter;
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-status");
        let mut screen = screen_in(&state);
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        screen.status = Reporter::buffered(buffer.clone());
        let reports = |buffer: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>| {
            let written = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
            written
                .split("\x1b]7501;")
                .skip(1)
                .map(|report| {
                    report
                        .trim_end_matches("\x1b\\")
                        .split(':')
                        .filter(|field| !field.starts_with("title="))
                        .collect::<Vec<_>>()
                        .join(":")
                })
                .collect::<Vec<String>>()
        };

        // A send is working; an approval is blocked; the answer is working
        // again; a failure is an error named by its kind.
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.set_input_text("hello");
        press(&mut screen, &mut backend, crossterm::event::KeyCode::Enter);
        let (event, _decisions) = approval(&[("src/a.rs", "src")]);
        screen.apply_event(event);
        screen.answer_approval(approval::ApprovalKey::Approve);
        screen.apply_event(ResponseEvent::ToolCallsRequested {
            calls: vec![backend::ToolCall {
                id: "c1".into(),
                name: "read_file".into(),
                arguments: serde_json::Map::new(),
            }],
            native: None,
        });
        screen.apply_event(ResponseEvent::Failed {
            message: "down".into(),
            kind: crate::error::ErrorKind::Transport,
        });
        assert_eq!(
            reports(&buffer),
            [
                "state=working:app=ahi",
                "state=blocked:kind=permission:app=ahi:msg=cmVhZF9maWxlIHNyYy9hLnJz",
                "state=working:app=ahi",
                "state=working:app=ahi:msg=cmVhZF9maWxl",
                "state=error:app=ahi:msg=dHJhbnNwb3J0",
            ]
        );
        // The title names the directory and the model.
        let written = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        let title = screen.status_title();
        assert!(title.ends_with(" · gpt-4.1"), "{title}");
        assert!(written.contains(":title="), "{written}");

        // A completed turn is done, with the answer's first line.
        buffer.lock().unwrap().clear();
        screen.apply_event(ResponseEvent::TextDelta("All good\nmore".into()));
        screen.apply_event(ResponseEvent::Completed);
        assert_eq!(reports(&buffer), ["state=done:app=ahi:msg=QWxsIGdvb2Q="]);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn git_status_is_probed_off_the_ui_thread_and_lands_on_poll() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-location");
        let mut screen = screen_in(&state);
        // Construction starts a probe; the footer keeps its empty snapshot
        // until the probe lands rather than waiting on `git status`.
        assert!(screen.location_receiver.is_some());
        screen.wait_for_location();
        assert!(screen.location_receiver.is_none());
        assert!(!screen.location_directory.is_empty());
        // A request while one is in flight is remembered, not stacked.
        screen.refresh_location();
        screen.refresh_location();
        assert!(screen.location_dirty);
        screen.wait_for_location();
        screen.poll_location();
        assert!(!screen.location_dirty);
        assert!(screen.location_receiver.is_some());
        screen.wait_for_location();
    }

    #[test]
    fn switching_provider_with_history_posts_a_disclosure_notice_once() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-switch");
        let mut screen = screen_in(&state);
        // An empty transcript has nothing to disclose, at startup or later.
        screen.set_selection("home/qwen");
        assert!(screen.messages().is_empty());
        screen.push_message(ChatMessage::new(TranscriptRole::Operator, "hi".to_string()));
        screen.push_message(ChatMessage::new(
            TranscriptRole::Assistant,
            "hello".to_string(),
        ));
        // Same provider, other model: nothing to disclose.
        screen.set_selection("home/llama");
        assert_eq!(screen.messages().len(), 2);
        screen.set_selection("anthropic/claude-sonnet-4-5");
        let notice = screen.messages().last().unwrap();
        assert_eq!(notice.role, TranscriptRole::Notice);
        assert_eq!(
            notice.text,
            "switched provider from home to anthropic: the 2 messages above will be sent to anthropic with your next message"
        );
        assert_eq!(screen.selection.gateway, "anthropic");
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
                tool: None,
                detail: None,
                elapsed: None,
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
    fn picker_aliases_and_quit_are_local_actions() {
        let state =
            crate::fixture_directory::create_unique_fixture_directory("chat-picker-aliases");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        for (command, expected) in [
            ("/models", ScreenAction::ReopenModelPicker),
            ("/model", ScreenAction::ReopenModelPicker),
            ("/catalog", ScreenAction::OpenCatalog),
        ] {
            screen.set_input_text(command);
            assert_eq!(
                screen
                    .apply_action(KeyAction::Submit, &mut backend)
                    .unwrap(),
                expected
            );
        }
        screen.response_in_progress = true;
        screen.set_input_text("/quit");
        assert_eq!(
            screen
                .apply_action(KeyAction::Submit, &mut backend)
                .unwrap(),
            ScreenAction::Exit
        );
        assert!(backend.requests.is_empty());
        fs::remove_dir_all(state).unwrap();
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
            tool: None,
            detail: None,
            elapsed: None,
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
            tool: None,
            detail: None,
            elapsed: None,
        });
        let idle = draw_lines(&mut screen, 120, 16);
        assert!(idle[idle.len() - 4].ends_with(" 28.4% / 272k ──"));
        assert!(idle[idle.len() - 2].starts_with("/ commands "));
        assert!(idle[idle.len() - 2]["/ commands ".len()..]
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
            tool: None,
            detail: None,
            elapsed: None,
        });
        assert!(!screen.apply_event(ResponseEvent::TextDelta("before".into())));
        assert!(!screen.apply_event(ResponseEvent::ToolResult {
            call_id: "call".into(),
            tool: "read_file".into(),
            content: String::new(),
            output: "ok".into(),
            exit_code: 0,
            detail: None,
            elapsed: std::time::Duration::ZERO,
            record: None,
        }));
        assert!(!screen.apply_event(ResponseEvent::TextDelta("after".into())));
        assert!(!screen.apply_event(ResponseEvent::TextDelta(" more".into())));
        assert!(!screen.apply_event(ResponseEvent::ToolResult {
            call_id: "call".into(),
            tool: "read_file".into(),
            content: String::new(),
            output: "failed (exit 2)".into(),
            exit_code: 2,
            detail: None,
            elapsed: std::time::Duration::ZERO,
            record: None,
        }));
        assert_eq!(screen.messages[3].text, "after more");
        assert_eq!(screen.messages[1].text, "before");
        let mut terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        for (row, text, background) in [
            (0, "question", Color::Rgb(30, 45, 60)),
            (2, "before", Color::Reset),
            (4, "read_file ok", Color::Rgb(30, 60, 45)),
            (6, "after more", Color::Reset),
            (
                8,
                "read_file failed (exit 2) · exit 2",
                Color::Rgb(60, 30, 30),
            ),
        ] {
            let rendered: String = (0..text.chars().count() as u16)
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
                call_id: "call".into(),
                tool: "read_file".into(),
                content: String::new(),
                output: "tool output\nmore output".into(),
                exit_code: 0,
                detail: None,
                elapsed: std::time::Duration::ZERO,
                record: None,
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
    fn slash_settings_filter_navigate_and_apply_without_a_model_call() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-settings");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen
            .apply_action(
                KeyAction::Edit(input::EditorKey::Character('/')),
                &mut backend,
            )
            .unwrap();
        let menu = draw_lines(&mut screen, 80, 16).join("\n");
        assert!(menu.contains("Commands · ↑/↓ select · Enter apply · Esc dismiss"));
        assert!(menu.contains("Choose model"));
        assert!(menu.contains("thinking medium · mouse on"));
        screen.set_input_text("/mouse ");
        let menu = draw_lines(&mut screen, 80, 16).join("\n");
        assert!(menu.contains("Native text selection"));
        assert!(!menu.contains("Choose model"));
        screen
            .apply_action(KeyAction::Edit(input::EditorKey::Down), &mut backend)
            .unwrap();
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert_eq!(screen.input_text(), "");
        assert!(!screen.mouse_capture);
        assert_eq!(screen.messages.last().unwrap().text,
            "mouse off: select/copy text with your terminal or Ctrl+S; use Page Up/Page Down to scroll, /mouse on for mouse controls");
        assert!(backend.requests.is_empty());
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn settings_escape_preserves_input_and_dollar_does_not_open_settings() {
        let state =
            crate::fixture_directory::create_unique_fixture_directory("chat-settings-dismiss");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.set_input_text("/");
        screen
            .apply_action(KeyAction::Cancel, &mut backend)
            .unwrap();
        assert_eq!(screen.input_text(), "/");
        assert!(!draw_lines(&mut screen, 80, 16)
            .join("\n")
            .contains("Commands ·"));
        screen
            .apply_action(
                KeyAction::Edit(input::EditorKey::Character('m')),
                &mut backend,
            )
            .unwrap();
        assert!(draw_lines(&mut screen, 80, 16)
            .join("\n")
            .contains("Commands ·"));
        screen.set_input_text("$skill");
        assert!(!draw_lines(&mut screen, 80, 16)
            .join("\n")
            .contains("Commands ·"));
        assert_eq!(screen.input_text(), "$skill");
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn transcript_drag_highlights_text_and_wheel_keeps_the_selection() {
        use crossterm::event::{KeyModifiers, MouseEvent};
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-drag");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.messages.push(ChatMessage {
            role: TranscriptRole::Assistant,
            text: (0..30)
                .map(|n| format!("line{n:02}"))
                .collect::<Vec<_>>()
                .join("\n"),
            tool: None,
            detail: None,
            elapsed: None,
        });
        screen.set_input_text("keep draft");
        draw_lines(&mut screen, 40, 12);
        let mouse = |kind, column, row| {
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        screen
            .apply_terminal_event(
                mouse(MouseEventKind::Down(MouseButton::Left), 0, 0),
                &mut backend,
            )
            .unwrap();
        screen
            .apply_terminal_event(
                mouse(MouseEventKind::Drag(MouseButton::Left), 5, 0),
                &mut backend,
            )
            .unwrap();
        let rows = transcript::render::rows(&screen.messages, 40);
        assert_eq!(screen.text_selection.text(&rows).as_deref(), Some("line22"));
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        assert_eq!(
            terminal.backend().buffer()[(0, 0)].bg,
            Color::Rgb(55, 85, 120)
        );
        screen
            .apply_terminal_event(mouse(MouseEventKind::ScrollUp, 0, 0), &mut backend)
            .unwrap();
        assert_eq!(screen.scroll_from_bottom, 3);
        assert_eq!(screen.text_selection.text(&rows).as_deref(), Some("line22"));
        screen
            .apply_action(KeyAction::Cancel, &mut backend)
            .unwrap();
        assert_eq!(screen.text_selection.text(&rows), None);
        assert_eq!(screen.input_text(), "keep draft");
        assert!(backend.requests.is_empty());
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn dragging_past_the_bottom_edge_auto_scrolls_and_extends_the_selection() {
        use crossterm::event::{KeyModifiers, MouseEvent};
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-autoscroll");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.messages.push(ChatMessage {
            role: TranscriptRole::Assistant,
            text: (0..30)
                .map(|n| format!("line{n:02}"))
                .collect::<Vec<_>>()
                .join("\n"),
            tool: None,
            detail: None,
            elapsed: None,
        });
        screen.set_input_text("keep draft");
        draw_lines(&mut screen, 40, 12);
        let height = screen.transcript_height;
        let mouse = |kind, column, row| {
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        // Scroll up first so the viewport can move in both directions.
        screen
            .apply_action(KeyAction::ScrollTowardOlder, &mut backend)
            .unwrap();
        assert_eq!(screen.scroll_from_bottom, height);
        let top_row = 30 - 2 * height; // first visible transcript row
        screen
            .apply_terminal_event(
                mouse(MouseEventKind::Down(MouseButton::Left), 0, 1),
                &mut backend,
            )
            .unwrap();
        // Dragging into the input area holds the bottom edge.
        screen
            .apply_terminal_event(
                mouse(
                    MouseEventKind::Drag(MouseButton::Left),
                    5,
                    height as u16 + 2,
                ),
                &mut backend,
            )
            .unwrap();
        assert_eq!(
            screen.text_selection.edge(screen.transcript_area),
            Some(selection::AutoScroll::Newer)
        );
        let rows = transcript::render::rows(&screen.messages, 40);
        let text_before = screen.text_selection.text(&rows).unwrap();
        assert!(text_before.starts_with(&format!("line{:02}", top_row + 1)));
        assert!(text_before.ends_with(&format!("line{:02}", top_row + height - 1)));
        for step in 1..=3 {
            assert!(screen.auto_scroll());
            assert_eq!(screen.scroll_from_bottom, height - step);
        }
        // The anchor stayed on its original row; the focus followed the pointer.
        let text_after = screen.text_selection.text(&rows).unwrap();
        assert!(text_after.starts_with(&format!("line{:02}", top_row + 1)));
        assert!(text_after.ends_with(&format!("line{:02}", top_row + height + 2)));
        // Returning inside the viewport stops the scroll; so does releasing.
        screen
            .apply_terminal_event(
                mouse(MouseEventKind::Drag(MouseButton::Left), 5, 3),
                &mut backend,
            )
            .unwrap();
        assert_eq!(screen.text_selection.edge(screen.transcript_area), None);
        assert!(!screen.auto_scroll());
        screen
            .apply_terminal_event(
                mouse(MouseEventKind::Drag(MouseButton::Left), 5, 0),
                &mut backend,
            )
            .unwrap();
        assert_eq!(
            screen.text_selection.edge(screen.transcript_area),
            Some(selection::AutoScroll::Older)
        );
        assert!(screen.auto_scroll());
        screen
            .apply_terminal_event(
                mouse(MouseEventKind::Up(MouseButton::Left), 5, 0),
                &mut backend,
            )
            .unwrap();
        assert_eq!(screen.text_selection.edge(screen.transcript_area), None);
        assert!(!screen.auto_scroll());
        assert!(screen.pending_copy.is_some() || screen.copy_receiver.is_some());
        assert_eq!(screen.input_text(), "keep draft");
        assert!(backend.requests.is_empty());
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn keyboard_selection_mode_moves_selects_yanks_and_preserves_the_draft() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-vi-select");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.messages.push(ChatMessage {
            role: TranscriptRole::Assistant,
            text: (0..30)
                .map(|n| format!("line{n:02} 界🙂"))
                .collect::<Vec<_>>()
                .join("\n"),
            tool: None,
            detail: None,
            elapsed: None,
        });
        screen.set_input_text("keep draft");
        draw_lines(&mut screen, 40, 12);
        let height = screen.transcript_height;
        let key = |code, modifiers| Event::Key(KeyEvent::new(code, modifiers));
        screen
            .apply_terminal_event(key(KeyCode::Char('s'), KeyModifiers::CONTROL), &mut backend)
            .unwrap();
        assert!(screen.text_selection.keyboard_active());
        assert_eq!(screen.text_selection.cursor().unwrap().row, 29);
        assert!(draw_lines(&mut screen, 40, 12)[height].contains("SELECT"));
        // Plain letters move the cursor instead of editing the draft.
        for _ in 0..height {
            screen
                .apply_terminal_event(key(KeyCode::Char('k'), KeyModifiers::NONE), &mut backend)
                .unwrap();
        }
        assert_eq!(screen.text_selection.cursor().unwrap().row, 29 - height);
        assert_eq!(
            screen.scroll_from_bottom, 1,
            "scrolled to keep the cursor visible"
        );
        screen
            .apply_terminal_event(key(KeyCode::Char('$'), KeyModifiers::SHIFT), &mut backend)
            .unwrap();
        // "lineNN 界🙂": the last grapheme starts after 7 narrow + 1 wide cells.
        assert_eq!(screen.text_selection.cursor().unwrap().column, 9);
        // `v` anchors on the emoji; `h` then `j` move only the focus.
        screen
            .apply_terminal_event(key(KeyCode::Char('v'), KeyModifiers::NONE), &mut backend)
            .unwrap();
        screen
            .apply_terminal_event(key(KeyCode::Char('h'), KeyModifiers::NONE), &mut backend)
            .unwrap();
        screen
            .apply_terminal_event(key(KeyCode::Char('j'), KeyModifiers::NONE), &mut backend)
            .unwrap();
        assert!(screen.text_selection.visual_active());
        assert!(draw_lines(&mut screen, 40, 12)[height].contains("VISUAL"));
        let rows = transcript::render::rows(&screen.messages, 40);
        assert_eq!(
            screen.text_selection.text(&rows).as_deref(),
            Some("🙂\nline22 界")
        );
        screen
            .apply_terminal_event(key(KeyCode::Char('y'), KeyModifiers::NONE), &mut backend)
            .unwrap();
        assert!(!screen.text_selection.keyboard_active());
        assert!(screen.pending_copy.is_some() || screen.copy_receiver.is_some());
        // The highlight stays for a retry; Esc clears it.
        assert_eq!(
            screen.text_selection.text(&rows).as_deref(),
            Some("🙂\nline22 界")
        );
        screen
            .apply_terminal_event(key(KeyCode::Esc, KeyModifiers::NONE), &mut backend)
            .unwrap();
        assert!(screen.text_selection.is_empty());
        assert_eq!(screen.input_text(), "keep draft");
        // Esc inside the mode leaves it without touching the draft either.
        screen.set_input_text("/select");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert!(screen.text_selection.keyboard_active());
        screen
            .apply_terminal_event(key(KeyCode::Esc, KeyModifiers::NONE), &mut backend)
            .unwrap();
        assert!(!screen.text_selection.keyboard_active());
        assert!(backend.requests.is_empty());
        screen.messages.clear();
        screen.dirty = transcript::render::Dirty::From(0);
        screen
            .apply_action(KeyAction::EnterSelectionMode, &mut backend)
            .unwrap();
        assert!(!screen.text_selection.keyboard_active());
        assert!(screen
            .messages
            .last()
            .unwrap()
            .text
            .starts_with("nothing to select"));
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn mouse_commands_restore_native_selection_and_reenable_controls() {
        use crossterm::event::{KeyModifiers, MouseEvent};
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-mouse-mode");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.messages.push(ChatMessage {
            role: TranscriptRole::Operator,
            text: (0..40).map(|n| format!("line {n:02}\n")).collect(),
            tool: None,
            detail: None,
            elapsed: None,
        });
        assert!(draw_lines(&mut screen, 80, 12)[0].contains("Top (Ctrl+K)"));
        screen.set_input_text("/mouse off");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert!(!draw_lines(&mut screen, 80, 12)[0].contains("Top (Ctrl+K)"));
        screen.set_input_text("/mouse on");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert!(draw_lines(&mut screen, 80, 12)[0].contains("Top (Ctrl+K)"));
        assert_eq!(
            screen.messages.last().unwrap().text,
            "mouse on: wheel and jump controls enabled; drag to select/copy, Ctrl+Y copies again, Ctrl+S selects by keyboard"
        );
        screen.set_input_text("/mouse off");
        screen
            .apply_action(KeyAction::Submit, &mut backend)
            .unwrap();
        assert!(!draw_lines(&mut screen, 80, 12)[0].contains("Top (Ctrl+K)"));
        assert_eq!(screen.messages.last().unwrap().text,
            "mouse off: select/copy text with your terminal or Ctrl+S; use Page Up/Page Down to scroll, /mouse on for mouse controls");
        screen.set_input_text("keep draft");
        screen
            .apply_terminal_event(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollUp,
                    column: 1,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                }),
                &mut backend,
            )
            .unwrap();
        assert_eq!(screen.scroll_from_bottom, 0);
        screen
            .apply_action(KeyAction::JumpToTop, &mut backend)
            .unwrap();
        assert!(draw_lines(&mut screen, 80, 12)[0].starts_with("line 00"));
        assert_eq!(screen.input_text(), "keep draft");
        assert!(backend.requests.is_empty());
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn jump_controls_hover_click_and_keep_the_draft() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent};
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-jumps");
        let mut screen = screen_in(&state);
        screen.mouse_capture = true;
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
        screen.mouse_capture = true;
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.messages.push(ChatMessage {
            role: TranscriptRole::Operator,
            text: (0..40).map(|n| format!("line {n:02}\n")).collect(),
            tool: None,
            detail: None,
            elapsed: None,
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
            ratatui_core::layout::Position::new(13, 13)
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
    fn tool_requests_attach_to_the_assistant_turn_without_argument_disclosure() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-tool-request");
        let mut screen = screen_in(&state);
        let call = backend::ToolCall {
            id: "call_a".into(),
            name: "read_file".into(),
            arguments: serde_json::from_value(serde_json::json!({"path": "private-path"})).unwrap(),
        };
        assert!(!screen.apply_event(ResponseEvent::TextDelta("looking".into())));
        assert!(!screen.apply_event(ResponseEvent::ToolCallsRequested {
            calls: vec![call.clone()],
            native: None
        }));
        assert_eq!(
            screen.messages,
            vec![ChatMessage {
                role: TranscriptRole::Assistant,
                text: "looking".into(),
                tool: Some(ToolPayload::Calls {
                    calls: vec![call.clone()],
                    native: None
                }),
                detail: None,
                elapsed: None,
            }]
        );
        // A tool-only assistant turn renders nothing; raw arguments never render.
        assert!(!screen.apply_event(ResponseEvent::ToolCallsRequested {
            calls: vec![call],
            native: None
        }));
        assert_eq!(screen.messages.len(), 2);
        let rendered = draw_lines(&mut screen, 40, 12).join("\n");
        assert!(rendered.contains("looking"));
        assert!(!rendered.contains("private-path"), "{rendered}");
        fs::remove_dir_all(state).unwrap();
    }

    fn approval(
        calls: &[(&str, &str)],
    ) -> (
        ResponseEvent,
        std::sync::mpsc::Receiver<Vec<backend::ToolDecision>>,
    ) {
        let (sender, receiver) = mpsc::channel();
        let calls = calls
            .iter()
            .enumerate()
            .map(|(index, (target, prefix))| backend::ProposedAction {
                call_id: format!("c{index}"),
                tool: "read_file".into(),
                target: target.to_string(),
                grant_prefix: prefix.to_string(),
                detail: String::new(),
            })
            .collect();
        let event = ResponseEvent::ToolApprovalRequested(backend::ApprovalRequest {
            batch: 7,
            calls,
            reply: backend::ApprovalReply::new(7, sender),
        });
        (event, receiver)
    }

    #[test]
    fn clicking_a_code_block_header_copies_its_body_and_a_drag_still_selects() {
        use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-click-copy");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.messages.push(ChatMessage::new(
            TranscriptRole::Assistant,
            "Run this:\n\n```sh\necho hi\n```\n",
        ));
        let lines = draw_lines(&mut screen, 40, 12);
        assert_eq!(lines[0].trim_end(), "Run this:");
        assert_eq!(lines[2].trim_end(), "sh  copy");
        assert_eq!(lines[3].trim_end(), "echo hi");
        let area = screen.transcript_area;
        let mouse = |kind, column, row| {
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        // Press and release on the `copy` word without moving: a click.
        let copy_x = area.x + 4;
        let copy_y = area.y + 2;
        screen
            .apply_terminal_event(
                mouse(MouseEventKind::Down(MouseButton::Left), copy_x, copy_y),
                &mut backend,
            )
            .unwrap();
        screen
            .apply_terminal_event(
                mouse(MouseEventKind::Up(MouseButton::Left), copy_x, copy_y),
                &mut backend,
            )
            .unwrap();
        assert_eq!(screen.copy_label, "Code block copied");
        assert!(screen.copy_receiver.is_some() || screen.pending_copy.is_none());
        // A click on plain text does nothing.
        screen.copy_label = "Selection copied";
        screen
            .apply_terminal_event(
                mouse(MouseEventKind::Down(MouseButton::Left), area.x, area.y),
                &mut backend,
            )
            .unwrap();
        screen
            .apply_terminal_event(
                mouse(MouseEventKind::Up(MouseButton::Left), area.x, area.y),
                &mut backend,
            )
            .unwrap();
        assert_eq!(screen.copy_label, "Selection copied");
        // A drag over the same header selects text instead of activating it.
        screen
            .apply_terminal_event(
                mouse(MouseEventKind::Down(MouseButton::Left), copy_x, copy_y),
                &mut backend,
            )
            .unwrap();
        screen
            .apply_terminal_event(
                mouse(MouseEventKind::Drag(MouseButton::Left), copy_x + 3, copy_y),
                &mut backend,
            )
            .unwrap();
        let rows = transcript::render::rows(&screen.messages, 40);
        assert!(screen.text_selection.text(&rows).is_some());
        let _ = std::fs::remove_dir_all(state);
    }

    #[test]
    fn yank_copies_rendered_text_and_shift_yank_copies_raw_markdown() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-raw-copy");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.messages.push(ChatMessage::new(
            TranscriptRole::Assistant,
            "# Title\n\nSome **bold** words.",
        ));
        draw_lines(&mut screen, 40, 12);
        let key = |code, modifiers| Event::Key(KeyEvent::new(code, modifiers));
        screen
            .apply_terminal_event(key(KeyCode::Char('s'), KeyModifiers::CONTROL), &mut backend)
            .unwrap();
        screen
            .apply_terminal_event(key(KeyCode::Char('V'), KeyModifiers::SHIFT), &mut backend)
            .unwrap();
        screen
            .apply_terminal_event(key(KeyCode::Char('g'), KeyModifiers::NONE), &mut backend)
            .unwrap();
        screen.sync_layout();
        assert_eq!(
            screen.text_selection.text(screen.layout.rows()).as_deref(),
            Some("Title\n\nSome bold words.\n")
        );
        assert_eq!(
            screen
                .text_selection
                .raw_text(screen.layout.rows())
                .as_deref(),
            Some("# Title\n\nSome **bold** words.")
        );
        screen
            .apply_terminal_event(key(KeyCode::Char('Y'), KeyModifiers::SHIFT), &mut backend)
            .unwrap();
        assert_eq!(screen.copy_label, "Selection copied (raw markdown)");
        // `/copy raw` makes `y` yank the source too, and the choice persists.
        screen
            .apply_terminal_event(key(KeyCode::Esc, KeyModifiers::NONE), &mut backend)
            .unwrap();
        screen.set_input_text("/copy raw");
        press(&mut screen, &mut backend, KeyCode::Enter);
        assert_eq!(screen.preferences.copy, preferences::CopyForm::Raw);
        assert_eq!(
            preferences::read(&Directories::single_root(&state)).copy,
            preferences::CopyForm::Raw
        );
        screen.set_input_text("/plain");
        press(&mut screen, &mut backend, KeyCode::Enter);
        assert!(!screen.preferences.markdown);
        let lines = draw_lines(&mut screen, 40, 14);
        assert!(lines.iter().any(|row| row.starts_with("# Title")));
        screen.set_input_text("/clock utc");
        press(&mut screen, &mut backend, KeyCode::Enter);
        screen.set_input_text("/editor zed");
        press(&mut screen, &mut backend, KeyCode::Enter);
        let stored = preferences::read(&Directories::single_root(&state));
        assert_eq!(stored.clock, preferences::Clock::Utc);
        assert_eq!(stored.editor.as_deref(), Some("zed"));
        assert!(!stored.markdown);
        let _ = std::fs::remove_dir_all(state);
    }

    #[test]
    fn a_completed_turn_gets_a_timed_footer_and_the_activity_line_counts() {
        use crossterm::event::KeyCode;
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-footer");
        let mut screen = screen_in(&state);
        screen.preferences.clock = preferences::Clock::Utc;
        let mut backend = HangingBackend {
            requests: Vec::new(),
            sender: None,
            cancels: 0,
        };
        screen.set_input_text("hello");
        press_hanging(&mut screen, &mut backend, KeyCode::Enter);
        assert!(screen.turn_started.is_some());
        let rows = draw_lines(&mut screen, 100, 12);
        assert!(rows
            .iter()
            .any(|row| row.contains("Working · 0.0s · Esc cancels")));
        let sender = backend.sender.as_ref().unwrap();
        sender.send(ResponseEvent::TextDelta("hi".into())).unwrap();
        sender.send(ResponseEvent::Completed).unwrap();
        screen.poll_backend();
        assert!(screen.turn_started.is_none());
        let footer = screen.messages.last().unwrap();
        assert_eq!(footer.role, TranscriptRole::Notice);
        assert!(footer.elapsed.is_some());
        assert!(footer.text.starts_with("done in "), "{}", footer.text);
        assert!(footer.text.ends_with('Z'), "utc clock: {}", footer.text);
        // Right-aligned and dim; never sent to a model.
        let rows = draw_lines(&mut screen, 60, 12);
        let row = rows.iter().find(|row| row.contains("done in")).unwrap();
        assert!(row.trim_end().ends_with('Z'));
        assert!(row.starts_with(' '));
        let _ = std::fs::remove_dir_all(state);
    }

    fn press_hanging(
        screen: &mut ChatScreen,
        backend: &mut HangingBackend,
        code: crossterm::event::KeyCode,
    ) {
        screen
            .apply_terminal_event(
                Event::Key(crossterm::event::KeyEvent::new(
                    code,
                    crossterm::event::KeyModifiers::NONE,
                )),
                backend,
            )
            .unwrap();
    }

    #[test]
    fn the_startup_header_is_a_notice_that_clear_removes() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-header");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.push_header();
        assert_eq!(screen.messages.len(), 1);
        assert_eq!(screen.messages[0].role, TranscriptRole::Notice);
        let lines = draw_lines(&mut screen, 100, 12);
        assert!(lines[0].starts_with(header::WORDMARK[0]));
        assert!(lines[0].contains(&format!("ahi {}", crate::VERSION)));
        assert!(lines[1].contains("gpt-4.1 via openai · thinking medium"));
        screen.set_input_text("/clear");
        press(&mut screen, &mut backend, crossterm::event::KeyCode::Enter);
        assert!(screen.messages.is_empty());
        let _ = std::fs::remove_dir_all(state);
    }

    fn press(
        screen: &mut ChatScreen,
        backend: &mut RecordingBackend,
        code: crossterm::event::KeyCode,
    ) -> ScreenAction {
        screen
            .apply_terminal_event(
                Event::Key(crossterm::event::KeyEvent::new(
                    code,
                    crossterm::event::KeyModifiers::NONE,
                )),
                backend,
            )
            .unwrap()
    }

    #[test]
    fn approval_keys_decide_each_call_then_send_once() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-approval");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.response_in_progress = true;
        let (event, decisions) = approval(&[("src/a.rs", "src"), ("top.rs", ""), ("b", "")]);
        assert!(!screen.apply_event(event));
        assert_eq!(
            screen.messages.last().unwrap().text,
            "Tool approval · 3 calls\n› 1. read_file src/a.rs\n  2. read_file top.rs\n  3. read_file b\n  ▸ Approve once · y\n    Approve for session under src/ · s\n    Deny · n\n    Approve all 3 remaining · a\n    Deny all 3 remaining · Esc"
        );
        let hint = draw_lines(&mut screen, 100, 16).join("\n");
        assert!(
            hint.contains(
                "Approve read_file? ↑/↓ choose · Enter apply · Shift+↑/↓ call · y once · s session under src/"
            ),
            "{hint}"
        );
        press(
            &mut screen,
            &mut backend,
            crossterm::event::KeyCode::Char('s'),
        );
        // No whole-workspace session grant: `s` is ignored at the root.
        press(
            &mut screen,
            &mut backend,
            crossterm::event::KeyCode::Char('s'),
        );
        assert!(decisions.try_recv().is_err());
        let hint = draw_lines(&mut screen, 100, 16).join("\n");
        assert!(!hint.contains("s session"), "{hint}");
        press(
            &mut screen,
            &mut backend,
            crossterm::event::KeyCode::Char('n'),
        );
        assert!(decisions.try_recv().is_err());
        press(
            &mut screen,
            &mut backend,
            crossterm::event::KeyCode::Char('a'),
        );
        assert_eq!(
            decisions.try_recv().unwrap(),
            vec![
                backend::ToolDecision::ApproveForSession,
                backend::ToolDecision::Deny,
                backend::ToolDecision::Approve,
            ]
        );
        assert!(screen.approval.is_none());
        assert_eq!(
            screen.messages.last().unwrap().text,
            "Tool approval · 3 calls\n✓ 1. read_file src/a.rs · approved for session under src/\n✗ 2. read_file top.rs · denied\n✓ 3. read_file b · approved once"
        );
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn arrow_keys_highlight_an_option_and_enter_applies_it() {
        let state =
            crate::fixture_directory::create_unique_fixture_directory("chat-approval-arrows");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.response_in_progress = true;
        let (event, decisions) = approval(&[("src/a.rs", "src"), ("b", "")]);
        assert!(!screen.apply_event(event));
        use crossterm::event::KeyCode;
        // Up at the first option stays there; Down moves to the session grant.
        press(&mut screen, &mut backend, KeyCode::Up);
        press(&mut screen, &mut backend, KeyCode::Down);
        let text = screen.messages.last().unwrap().text.clone();
        assert!(
            text.contains(
                "    Approve once · y\n  ▸ Approve for session under src/ · s\n    Deny · n"
            ),
            "{text}"
        );
        press(&mut screen, &mut backend, KeyCode::Enter);
        assert!(decisions.try_recv().is_err());
        // The last call offers no session grant and no "all remaining"; the
        // highlight starts over and Down clamps at the final option.
        let text = screen.messages.last().unwrap().text.clone();
        assert!(
            text.ends_with("› 2. read_file b\n  ▸ Approve once · y\n    Deny · n"),
            "{text}"
        );
        press(&mut screen, &mut backend, KeyCode::Down);
        press(&mut screen, &mut backend, KeyCode::Down);
        press(&mut screen, &mut backend, KeyCode::Down);
        assert!(screen
            .messages
            .last()
            .unwrap()
            .text
            .ends_with("    Approve once · y\n  ▸ Deny · n"));
        press(&mut screen, &mut backend, KeyCode::Enter);
        assert_eq!(
            decisions.try_recv().unwrap(),
            vec![
                backend::ToolDecision::ApproveForSession,
                backend::ToolDecision::Deny,
            ]
        );
        assert!(screen.approval.is_none());
        // Decided: the option list is gone and nothing was submitted.
        assert_eq!(
            screen.messages.last().unwrap().text,
            "Tool approval · 2 calls\n✓ 1. read_file src/a.rs · approved for session under src/\n✗ 2. read_file b · denied"
        );
        assert!(backend.requests.is_empty());
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn shift_arrows_move_between_calls_and_answers_can_be_revised() {
        let state =
            crate::fixture_directory::create_unique_fixture_directory("chat-approval-calls");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.response_in_progress = true;
        let (event, decisions) = approval(&[("src/a.rs", "src"), ("b", ""), ("c", "")]);
        assert!(!screen.apply_event(event));
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let shift = |screen: &mut ChatScreen, backend: &mut RecordingBackend, code| {
            screen
                .apply_terminal_event(
                    Event::Key(KeyEvent::new(code, KeyModifiers::SHIFT)),
                    backend,
                )
                .unwrap();
        };
        // Shift+Down moves the cursor to call 2; Shift+Up from call 1 wraps to 3.
        shift(&mut screen, &mut backend, KeyCode::Down);
        let text = screen.messages.last().unwrap().text.clone();
        assert!(
            text.contains("  1. read_file src/a.rs\n› 2. read_file b\n  3. read_file c"),
            "{text}"
        );
        shift(&mut screen, &mut backend, KeyCode::Up);
        shift(&mut screen, &mut backend, KeyCode::Up);
        let text = screen.messages.last().unwrap().text.clone();
        assert!(
            text.contains("  2. read_file b\n› 3. read_file c\n"),
            "{text}"
        );
        // Deny call 3; the cursor continues at the next undecided call, 1.
        press(&mut screen, &mut backend, KeyCode::Char('n'));
        let text = screen.messages.last().unwrap().text.clone();
        assert!(
            text.contains("› 1. read_file src/a.rs\n  2. read_file b\n✗ 3. read_file c · denied"),
            "{text}"
        );
        // Approve call 1 (cursor → 2), go back and revise it to a session grant.
        press(&mut screen, &mut backend, KeyCode::Char('y'));
        shift(&mut screen, &mut backend, KeyCode::Up);
        let text = screen.messages.last().unwrap().text.clone();
        assert!(
            text.contains("› 1. read_file src/a.rs · approved once\n"),
            "{text}"
        );
        press(&mut screen, &mut backend, KeyCode::Char('s'));
        assert!(decisions.try_recv().is_err());
        // Only call 2 is undecided: the cursor is there, with no "all" options.
        let text = screen.messages.last().unwrap().text.clone();
        assert!(
            text.ends_with(
                "› 2. read_file b\n✗ 3. read_file c · denied\n  ▸ Approve once · y\n    Deny · n"
            ),
            "{text}"
        );
        press(&mut screen, &mut backend, KeyCode::Enter);
        assert_eq!(
            decisions.try_recv().unwrap(),
            vec![
                backend::ToolDecision::ApproveForSession,
                backend::ToolDecision::Approve,
                backend::ToolDecision::Deny,
            ]
        );
        assert!(screen.approval.is_none());
        assert_eq!(
            screen.messages.last().unwrap().text,
            "Tool approval · 3 calls\n✓ 1. read_file src/a.rs · approved for session under src/\n✓ 2. read_file b · approved once\n✗ 3. read_file c · denied"
        );
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn a_draft_bypasses_approval_keys_and_quit_drops_the_reply() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-approval-quit");
        let mut screen = screen_in(&state);
        let mut backend = RecordingBackend {
            requests: Vec::new(),
        };
        screen.response_in_progress = true;
        let (event, decisions) = approval(&[("src/a.rs", "src")]);
        screen.apply_event(event);
        press(
            &mut screen,
            &mut backend,
            crossterm::event::KeyCode::Char('/'),
        );
        for key in "quit".chars() {
            press(
                &mut screen,
                &mut backend,
                crossterm::event::KeyCode::Char(key),
            );
        }
        assert_eq!(screen.input.contents(), "/quit");
        assert!(decisions.try_recv().is_err());
        let hint = draw_lines(&mut screen, 100, 12).join("\n");
        assert!(
            hint.contains("clear the draft (Ctrl+C) to answer"),
            "{hint}"
        );
        assert_eq!(
            press(&mut screen, &mut backend, crossterm::event::KeyCode::Enter),
            ScreenAction::Exit
        );
        drop(screen);
        // The reply was never sent; dropping the screen closes the channel.
        assert!(matches!(
            decisions.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn a_finished_turn_discards_an_unanswered_approval() {
        let state = crate::fixture_directory::create_unique_fixture_directory("chat-approval-end");
        let mut screen = screen_in(&state);
        let (event, decisions) = approval(&[("src/a.rs", "src")]);
        screen.apply_event(event);
        assert!(screen.apply_event(ResponseEvent::Failed {
            message: "ended".into(),
            kind: crate::error::ErrorKind::Other,
        }));
        assert!(screen.approval.is_none());
        assert!(matches!(
            decisions.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
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
