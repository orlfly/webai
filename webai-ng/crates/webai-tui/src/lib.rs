//! ratatui + crossterm frontend with terminal image support (Kitty/iTerm2/Sixel).
//!
//! The TUI frontend talks **only** to the session background service
//! (`session`, ARCHITECTURE.md §4.11): it holds an `Arc<AgentSession>` and
//! streams `SessionEvent`s out over a bounded mpsc channel. The frontend never
//! imports the bridge/webkit layers directly (§2 boundary 1). The full ratatui
//! app and image pipelines land in M5.

pub mod session;

pub use session::{PromptHandler, SessionBackend, COALESCE_STEP_BURST, EVENT_CHANNEL_CAPACITY};

/// A command the frontend sends to the session background task.
#[derive(Debug, Clone)]
pub enum UiCommand {
    /// Feed a user prompt to the agent loop.
    Send { text: String },
    /// Ask the background task to stop cleanly.
    Shutdown,
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
