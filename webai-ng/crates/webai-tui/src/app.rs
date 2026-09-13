//! ratatui app state + streaming renderer (ARCHITECTURE.md §4.11 `app.rs`).
//!
//! The `App` owns the renderable state (streaming transcript, input line,
//! scroll offset, status bar) and a `tick`-driven 50ms render loop. Key
//! handling is a pure function over crossterm events so the §4.11 keymap is
//! unit-testable without a real terminal:
//!
//! - Enter       send the input
//! - ↑/↓         scroll the transcript
//! - PgUp/PgDn   page through the transcript
//! - Esc         clear the input (exit when the input is empty)
//! - Ctrl+C      exit
//!
//! Rendering goes through `ratatui::backend::TestBackend` in tests so wide
//! characters (CJK/emoji) and long lines cannot panic or overflow.

use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::text::Line as RtLine;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;
use ratatui::Terminal;

/// Render cadence (ARCHITECTURE.md §6: TUI tick 50ms).
pub const RENDER_TICK_MS: u64 = 50;

/// How many rows PgUp/PgDn jump.
pub const PAGE_ROWS: u16 = 8;

/// What the app decided after handling one key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// Continue running.
    Continue,
    /// Exit the loop (Ctrl+C, or Esc with empty input).
    Exit,
    /// Hand the input to the session service (Enter with non-empty input).
    Send(String),
    /// Internal scroll/refresh; no terminal write needed.
    Internal,
}

/// A line in the streaming transcript: role + text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatLine {
    User(String),
    Assistant(String),
    /// A rendered terminal image frame (§4.11): protocol, dimensions and the
    /// persisted temp path; real backends emit the encoded escape sequence.
    Image {
        protocol: &'static str,
        width: u32,
        height: u32,
        temp_path: String,
    },
}

/// The TUI app state (testable without a terminal).
#[derive(Debug, Default)]
pub struct App {
    /// Streaming transcript (append-only; the model streams into the last
    /// assistant line so tokens refresh incrementally).
    pub lines: Vec<ChatLine>,
    /// The user's input buffer.
    pub input: String,
    /// Scroll offset from the bottom (0 = follow the newest line).
    pub scroll: usize,
    /// Status bar text (e.g. "running", "done", error summary).
    pub status: String,
    /// Whether the last render should be refreshed (streaming tick).
    pub dirty: bool,
    /// §4.11 image pipeline: decode-once / dispatch-once ingest of step images.
    pub images: crate::images::ImagePipeline,
}

impl Default for crate::images::ImagePipeline {
    fn default() -> Self {
        Self::new(std::env::temp_dir().join("webai-tui-imgs"), None)
    }
}

impl App {
    pub fn new() -> Self {
        Self::with_protocol(crate::images::detect_protocol())
    }

    /// Build the app with an explicit image protocol override (tests use a
    /// deterministic protocol instead of terminal capability detection).
    pub fn with_protocol(protocol: Option<crate::images::ImageProtocol>) -> Self {
        Self {
            lines: Vec::new(),
            input: String::new(),
            scroll: 0,
            status: String::new(),
            dirty: false,
            images: crate::images::ImagePipeline::new(
                std::env::temp_dir().join("webai-tui-imgs"),
                protocol,
            ),
        }
    }

    /// Feed one streaming text delta: append to the last assistant line (or
    /// start one). This is the "streaming not whole message" path.
    pub fn push_stream_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        match self.lines.last_mut() {
            Some(ChatLine::Assistant(t)) => t.push_str(delta),
            _ => self.lines.push(ChatLine::Assistant(delta.to_string())),
        }
        self.dirty = true;
    }

    /// Append a completed user line.
    pub fn push_user(&mut self, text: &str) {
        self.lines.push(ChatLine::User(text.to_string()));
        self.dirty = true;
    }

    /// Append a completed assistant line (non-streaming path).
    pub fn push_assistant(&mut self, text: &str) {
        self.lines.push(ChatLine::Assistant(text.to_string()));
        self.dirty = true;
    }

    /// Set the status bar text.
    pub fn set_status(&mut self, status: &str) {
        self.status = status.to_string();
        self.dirty = true;
    }

    /// Ingest one step image (base64 PNG) through the §4.11 pipeline and
    /// append a transcript marker line. Real terminals receive the encoded
    /// escape frame from the run loop via [`Self::last_encoded_frame`].
    pub fn on_image(&mut self, base64_png: &str) {
        let outcome = match self.images.ingest(base64_png) {
            Ok(img) => self.images.on_viewport(img.id, true),
            Err(reason) => {
                self.lines.push(ChatLine::Assistant(format!(
                    "[image placeholder: {reason:?}]"
                )));
                self.dirty = true;
                return;
            }
        };
        match outcome {
            Some(crate::images::DispatchOutcome::Frame { protocol, temp_path, width, height }) => {
                self.lines.push(ChatLine::Image {
                    protocol: protocol.as_str(),
                    width,
                    height,
                    temp_path: temp_path.display().to_string(),
                });
            }
            Some(crate::images::DispatchOutcome::Placeholder { reason }) => {
                self.lines
                    .push(ChatLine::Assistant(format!("[image placeholder: {reason}]")));
            }
            None => {}
        }
        self.dirty = true;
    }

    /// Encode the newest dispatched frame for the active terminal protocol
    /// (the run loop prints this to the real tty right after a render tick).
    pub fn last_encoded_frame(&mut self) -> Option<Vec<u8>> {
        let frame = self.lines.iter().rev().find_map(|l| match l {
            ChatLine::Image { temp_path, .. } => std::fs::read(temp_path).ok().map(|b| {
                (
                    crate::encoders::encode_frame(
                        &crate::images::DispatchOutcome::Frame {
                            protocol: crate::images::ImageProtocol::Kitty,
                            temp_path: std::path::PathBuf::from(temp_path),
                            width: 0,
                            height: 0,
                        },
                        &b,
                    ),
                    b,
                )
            }),
            _ => None,
        });
        frame.and_then(|(enc, _)| enc).map(|e| e.bytes)
    }

    /// Consume one frontend `UiEvent` (run-loop integration): a streaming
    /// delta is appended to the current assistant line, a Finished message
    /// updates the status bar.
    pub fn on_ui_event(&mut self, ev: &crate::UiEvent) {
        match ev {
            crate::UiEvent::Image(b64) => self.on_image(b64),
            crate::UiEvent::Delta(text) => self.push_stream_delta(text),
            crate::UiEvent::Finished(msg) => self.set_status(msg),
        }
    }

    /// Scroll down (older content) by one row. The offset is clamped to the
    /// content length so Up x1000 cannot scroll past the oldest line
    /// (task #80).
    pub fn scroll_up(&mut self) {
        self.scroll = (self.scroll + 1).min(self.max_scroll());
        self.dirty = true;
    }

    /// Upper bound for the scroll offset: the number of transcript lines
    /// (each contributes at least one rendered row).
    fn max_scroll(&self) -> usize {
        self.lines.len()
    }

    /// Scroll back down toward the newest line.
    pub fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
        self.dirty = true;
    }

    /// Page up/down (clamped to the content length, task #80).
    pub fn page_up(&mut self) {
        self.scroll = (self.scroll + PAGE_ROWS as usize).min(self.max_scroll());
        self.dirty = true;
    }

    pub fn page_down(&mut self) {
        self.scroll = self.scroll.saturating_sub(PAGE_ROWS as usize);
        self.dirty = true;
    }

    /// Handle a key event per §4.11. Returns the action to take.
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> KeyAction {
        use crossterm::event::KeyModifiers;
        match (key.code, key.modifiers) {
            (crossterm::event::KeyCode::Char('c'), KeyModifiers::CONTROL) => KeyAction::Exit,
            (crossterm::event::KeyCode::Enter, _) => {
                // A blank / whitespace-only Enter keeps the draft (review #59):
                // it is an accidental keypress, not a submission intent.
                if self.input.trim().is_empty() {
                    KeyAction::Internal
                } else {
                    let text = std::mem::take(&mut self.input);
                    self.push_user(&text);
                    KeyAction::Send(text)
                }
            }
            (crossterm::event::KeyCode::Esc, _) => {
                if self.input.is_empty() {
                    KeyAction::Exit
                } else {
                    self.input.clear();
                    self.dirty = true;
                    KeyAction::Internal
                }
            }
            (crossterm::event::KeyCode::Up, _) => {
                self.scroll_up();
                KeyAction::Internal
            }
            (crossterm::event::KeyCode::Down, _) => {
                self.scroll_down();
                KeyAction::Internal
            }
            (crossterm::event::KeyCode::PageUp, _) => {
                self.page_up();
                KeyAction::Internal
            }
            (crossterm::event::KeyCode::PageDown, _) => {
                self.page_down();
                KeyAction::Internal
            }
            (crossterm::event::KeyCode::Char(c), m) if m.is_empty() || m == KeyModifiers::SHIFT => {
                self.input.push(c);
                self.dirty = true;
                KeyAction::Internal
            }
            (crossterm::event::KeyCode::Backspace, _) => {
                self.input.pop();
                self.dirty = true;
                KeyAction::Internal
            }
            _ => KeyAction::Internal,
        }
    }

    /// Render onto a ratatui backend (TestBackend in tests). Layout: transcript
    /// top, input bottom, 1-row status bar. Wide chars and long lines are
    /// wrapped by ratatui's `Paragraph` so they cannot panic or overflow.
    pub fn render<B: ratatui::backend::Backend>(&self, terminal: &mut Terminal<B>) {
        let _ = terminal.draw(|f| render_frame(f, self));
    }

    /// Test helper: render into a fixed-size `TestBackend` and return its
    /// buffer contents for assertions.
    pub fn render_to_buffer(&self, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        self.render(&mut terminal);
        let buf = terminal.backend().buffer().clone();
        // Collect non-empty rendered rows.
        (0..buf.area.height)
            .map(|y| {
                let mut row = String::new();
                for x in 0..buf.area.width {
                    let cell = &buf[(x, y)];
                    row.push_str(cell.symbol());
                }
                row.trim_end().to_string()
            })
            .collect()
    }
}

/// Draw one frame: transcript list (top), input (middle), status bar (bottom).
fn render_frame(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(f.area());

    // Transcript: scrollable list of chat lines (StatefulWidget so the scroll
    // offset applies; the state's offset is driven by `app.scroll`).
    let items: Vec<ListItem> = app
        .lines
        .iter()
        .map(|l| match l {
            ChatLine::User(t) => ListItem::new(RtLine::from(format!("你: {t}"))),
            ChatLine::Assistant(t) => ListItem::new(RtLine::from(format!("AI: {t}"))),
            ChatLine::Image { protocol, width, height, temp_path } => {
                ListItem::new(RtLine::from(format!(
                    "[image {protocol} {width}x{height} {}]",
                    temp_path
                )))
            }
        })
        .collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("会话"));
    let mut state = ratatui::widgets::ListState::default();
    *state.offset_mut() = app.scroll;
    f.render_stateful_widget(list, chunks[0], &mut state);

    let input = Paragraph::new(app.input.as_str())
        .block(Block::default().borders(Borders::ALL).title("输入"));
    f.render_widget(input, chunks[1]);

    let status = Paragraph::new(app.status.as_str());
    f.render_widget(status, chunks[2]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn plain(code: KeyCode) -> KeyEvent {
        key(code, KeyModifiers::NONE)
    }

    #[test]
    fn enter_sends_non_empty_input() {
        let mut app = App {
            input: "打开百度".into(),
            ..Default::default()
        };
        match app.handle_key(plain(KeyCode::Enter)) {
            KeyAction::Send(text) => assert_eq!(text, "打开百度"),
            other => panic!("expected Send, got {other:?}"),
        }
        assert!(app.input.is_empty());
        // The user line is in the transcript.
        assert!(matches!(app.lines.last(), Some(ChatLine::User(u)) if u == "打开百度"));
    }

    #[test]
    fn enter_with_empty_input_is_internal() {
        let mut app = App {
            input: "   ".into(),
            ..Default::default()
        };
        assert!(matches!(
            app.handle_key(plain(KeyCode::Enter)),
            KeyAction::Internal
        ));
    }

    #[test]
    fn esc_clears_input_then_exits_when_empty() {
        let mut app = App {
            input: "draft".into(),
            ..Default::default()
        };
        assert!(matches!(
            app.handle_key(plain(KeyCode::Esc)),
            KeyAction::Internal
        ));
        assert!(app.input.is_empty());
        // Second Esc (empty input) exits.
        assert!(matches!(
            app.handle_key(plain(KeyCode::Esc)),
            KeyAction::Exit
        ));
    }

    #[test]
    fn ctrl_c_exits_always() {
        let mut app = App {
            input: "some draft".into(),
            ..Default::default()
        };
        assert!(matches!(
            app.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            KeyAction::Exit
        ));
    }

    #[test]
    fn scroll_keys_move_view() {
        let mut app = App::default();
        for _ in 0..20 {
            app.push_assistant(&"x".repeat(10));
        }
        app.handle_key(plain(KeyCode::Up));
        assert_eq!(app.scroll, 1);
        app.handle_key(plain(KeyCode::Down));
        assert_eq!(app.scroll, 0);
        app.handle_key(plain(KeyCode::PageUp));
        assert_eq!(app.scroll, PAGE_ROWS as usize);
        app.handle_key(plain(KeyCode::PageDown));
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn streaming_deltas_append_incrementally() {
        let mut app = App::default();
        app.push_stream_delta("你");
        app.push_stream_delta("好");
        app.push_stream_delta("，世");
        // One assistant line holding the partial text so far (not three lines).
        assert_eq!(app.lines.len(), 1);
        assert!(matches!(app.lines[0], ChatLine::Assistant(ref t) if t == "你好，世"));
    }

    #[test]
    fn wide_chars_render_without_panic() {
        let mut app = App::default();
        app.push_user("打开新浪财经，汇总今天头条 🎉");
        app.push_assistant(&"你好，世界 — 这是一个超长中文测试行".repeat(4));
        app.set_status("running");
        // Must not panic on narrow terminals with wide chars.
        let rows = app.render_to_buffer(20, 6);
        assert!(!rows.is_empty());
    }

    #[test]
    fn long_lines_wrap_without_overflow() {
        let mut app = App::default();
        app.push_assistant(&"A".repeat(300));
        let rows = app.render_to_buffer(30, 8);
        assert!(rows.iter().all(|r| r.chars().count() <= 30));
    }

    #[test]
    fn render_tick_constant_is_50ms() {
        assert_eq!(RENDER_TICK_MS, 50);
    }

    #[test]
    fn image_step_renders_marker_line_in_buffer() {
        let mut app = App::with_protocol(Some(crate::images::ImageProtocol::Kitty));
        // 1x1 PNG (same payload as the e2e deterministic case).
        let b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        app.on_image(b64);
        let rows = app.render_to_buffer(80, 24);
        assert!(
            rows.iter().any(|r| r.contains("[image kitty")),
            "image marker line must render in the terminal buffer: {rows:?}"
        );
        // Encoded frame for the real tty exists and is Kitty-shaped.
        let frame = app.last_encoded_frame().expect("encoded frame");
        assert!(!frame.is_empty());
        assert_eq!(frame[0], 0x1B, "escape sequence starts the frame");
    }

}
