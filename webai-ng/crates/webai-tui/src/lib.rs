//! ratatui + crossterm frontend with terminal image support (Kitty/iTerm2/Sixel).
//!
//! Stub skeleton for the webai-ng AI browser (M1-4). Defines the session backend
//! (ARCHITECTURE.md §4.11): a background task owning `Arc<AgentSession>` and
//! streaming `SessionEvent`s out over an mpsc channel. The full ratatui app and
//! terminal image pipelines land in M5.

use std::sync::Arc;

use webai_agent::AgentSession;

/// An event streamed from the session backend to the frontend.
#[derive(Debug, Clone)]
pub enum UiEvent {
    Text(String),
}

/// A command sent from the frontend to the session backend.
#[derive(Debug, Clone)]
pub enum UiCommand {
    Send { text: String },
    Shutdown,
}

/// The session backend (ARCHITECTURE.md §4.11 `session.rs`). Stub only exposes
/// the event/command channels; the real tokio task lands in M5.
pub struct SessionBackend {
    session: Arc<AgentSession>,
}

impl SessionBackend {
    pub fn new(session: Arc<AgentSession>) -> Self {
        Self { session }
    }

    pub fn session(&self) -> &Arc<AgentSession> {
        &self.session
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use webai_agent::{AgentLoop, ChatMessage};
    use webai_llm::LlmClient;
    use webai_memory::SharedMemoryStore;

    fn build_session(id: &str) -> Arc<AgentSession> {
        let llm = LlmClient::with_profile_stub("stub");
        let loop_ = Arc::new(AgentLoop::new(
            Arc::new(llm),
            Arc::new(SharedMemoryStore::new()),
            vec![],
        ));
        Arc::new(AgentSession::new(id, loop_, Arc::new(SharedMemoryStore::new())))
    }

    #[test]
    fn backend_exposes_session() {
        let s = build_session("tui-s1");
        let backend = SessionBackend::new(s.clone());
        assert_eq!(backend.session().session_id(), "tui-s1");
    }

    #[test]
    fn session_transcript_is_shared() {
        let s = build_session("tui-s2");
        s.push(ChatMessage::User("render me".into()));
        let backend = SessionBackend::new(s);
        assert_eq!(backend.session().transcript().len(), 1);
    }
}
