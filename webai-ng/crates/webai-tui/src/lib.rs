//! ratatui + crossterm frontend with terminal image support (Kitty/iTerm2/Sixel).
//!
//! The TUI frontend talks **only** to the session background service
//! (`session`, ARCHITECTURE.md §4.11): it holds an `Arc<AgentSession>` and
//! streams `SessionEvent`s out over a bounded mpsc channel. The frontend never
//! imports the bridge/webkit layers directly (§2 boundary 1). `app` renders
//! the streaming transcript with the §4.11 keymap and runs the event loop
//! (task #80).

pub mod app;
pub mod run;
pub mod session;

pub use app::{App, ChatLine, KeyAction, PAGE_ROWS, RENDER_TICK_MS};
pub use run::{install_panic_hook, run, run_real, LoopSignal, TerminalGuard};
pub use session::{PromptHandler, SessionBackend, COALESCE_STEP_BURST, EVENT_CHANNEL_CAPACITY};

/// A command the frontend sends to the session background task.
#[derive(Debug, Clone)]
pub enum UiCommand {
    /// Feed a user prompt to the agent loop.
    Send { text: String },
    /// Ask the background task to stop cleanly.
    Shutdown,
}

/// An event streamed from the session backend to the frontend: either a
/// streaming text delta (rendered incrementally) or a status change.
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// Streaming text delta appended to the transcript.
    Delta(String),
    /// The loop finished with a terminal state message.
    Finished(String),
}

/// Build a terminal session state for a completion result (used by the
/// session service to emit `SessionEvent::Done`).
pub(crate) fn done_state(rc: Result<(), String>) -> webai_protocol::AgentState {
    match rc {
        Ok(()) => webai_protocol::AgentState {
            status: "done".into(),
            message: None,
        },
        Err(_) => webai_protocol::AgentState {
            status: "error".into(),
            message: None,
        },
    }
}
