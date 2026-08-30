//! ACP JSON-RPC over WebSocket / line-delimited TCP server.
//!
//! Stub skeleton for the webai-ng AI browser (M1-4). Defines the session registry
//! (ARCHITECTURE.md §4.10): `AcpSessionRegistry` holding `Arc<AgentSession>` per
//! `session_id`, serializing `session/prompt` and `session/close`. The actual
//! WebSocket / TCP server lands in M5.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use webai_agent::AgentSession;

/// Registry of live ACP sessions (ARCHITECTURE.md §4.10). Shared between the ACP
/// server and the TUI so local and remote observers see the same sessions.
#[derive(Debug, Clone, Default)]
pub struct AcpSessionRegistry {
    sessions: Arc<Mutex<HashMap<String, Arc<AgentSession>>>>,
}

impl AcpSessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a session under its id.
    pub fn register(&self, session: Arc<AgentSession>) {
        let id = session.session_id().to_owned();
        self.sessions.lock().unwrap().insert(id, session);
    }

    /// Look up a session by id.
    pub fn get(&self, session_id: &str) -> Option<Arc<AgentSession>> {
        self.sessions.lock().unwrap().get(session_id).cloned()
    }

    /// Remove and return a session by id (used by `session/close`).
    pub fn remove(&self, session_id: &str) -> Option<Arc<AgentSession>> {
        self.sessions.lock().unwrap().remove(session_id)
    }

    pub fn len(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A JSON-RPC request received over an ACP transport.
#[derive(Debug, Clone)]
pub struct AcpRequest {
    pub id: u64,
    pub method: String,
    pub params: serde_json::Value,
}

/// A JSON-RPC response.
#[derive(Debug, Clone)]
pub struct AcpResponse {
    pub id: u64,
    pub result: Option<serde_json::Value>,
    pub error: Option<AcpError>,
}

/// A structured ACP error.
#[derive(Debug, Clone)]
pub struct AcpError {
    pub code: i32,
    pub message: String,
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
    fn registry_register_get_remove() {
        let reg = AcpSessionRegistry::new();
        let s1 = build_session("s1");
        let s2 = build_session("s2");
        reg.register(s1.clone());
        reg.register(s2);
        assert_eq!(reg.len(), 2);
        assert!("s1".eq(reg.get("s1").unwrap().session_id()));
        assert!(reg.get("nope").is_none());
        let removed = reg.remove("s1").unwrap();
        assert_eq!(removed.session_id(), "s1");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn session_holds_transcript_messages() {
        let s = build_session("sx");
        s.push(ChatMessage::User("hi".into()));
        assert_eq!(s.transcript().len(), 1);
    }
}
