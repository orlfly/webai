//! AgentLoop, AgentSession, plan, script_memory, history summariser.
//!
//! Stub skeleton for the webai-ng AI browser (M1-4). Defines the tool registry
//! (`Tool` trait, ARCHITECTURE.md §4.9), the `AgentLoop` plan-act-observe driver,
//! guard flags (`max_steps`, `duplicate_threshold`), and the `AgentSession`
//! transcript container. Real LLM-driven looping lands in M4.

use std::sync::{Arc, Mutex};

use webai_llm::LlmClient;
use webai_memory::SharedMemoryStore;

/// The core tools (browser / memory / filesystem / llm / acp_notify) plus
/// terminate. Each tool handles one call and returns structured output.
/// The trait is intentionally minimal; async dispatch is added in M4.
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
}

/// Agent-loop guard configuration (ARCHITECTURE.md §4.9).
#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub max_steps: u32,
    pub duplicate_threshold: u32,
    pub auto_plan_on_multi_step: bool,
    pub script_memory_enabled: bool,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_steps: 30,
            duplicate_threshold: 2,
            auto_plan_on_multi_step: true,
            script_memory_enabled: true,
        }
    }
}

/// The plan-act-observe loop driver. Stub holds configuration and dependencies;
/// the actual LLM-driven step loop lands in M4.
pub struct AgentLoop {
    config: LoopConfig,
    llm: Arc<LlmClient>,
    memory: Arc<SharedMemoryStore>,
    tools: Vec<Arc<dyn Tool>>,
}

impl AgentLoop {
    pub fn new(llm: Arc<LlmClient>, memory: Arc<SharedMemoryStore>, tools: Vec<Arc<dyn Tool>>) -> Self {
        Self::with_config(llm, memory, tools, LoopConfig::default())
    }

    pub fn with_config(
        llm: Arc<LlmClient>,
        memory: Arc<SharedMemoryStore>,
        tools: Vec<Arc<dyn Tool>>,
        config: LoopConfig,
    ) -> Self {
        Self {
            config,
            llm,
            memory,
            tools,
        }
    }

    pub fn config(&self) -> &LoopConfig {
        &self.config
    }

    pub fn llm(&self) -> &Arc<LlmClient> {
        &self.llm
    }

    pub fn memory(&self) -> &Arc<SharedMemoryStore> {
        &self.memory
    }

    pub fn tools(&self) -> &[Arc<dyn Tool>] {
        &self.tools
    }
}

/// A single chat message in a session transcript.
#[derive(Debug, Clone)]
pub enum ChatMessage {
    User(String),
    Assistant(String),
}

/// The durable session container (ARCHITECTURE.md §4.9): transcript +
/// `Arc<AgentLoop>` + optional `Arc<SharedMemoryStore>`.
pub struct AgentSession {
    session_id: String,
    transcript: Mutex<Vec<ChatMessage>>,
    loop_: Arc<AgentLoop>,
    memory: Arc<SharedMemoryStore>,
}

/// Manual `Debug` impl because `AgentLoop` contains a `dyn Tool` which is not
/// `Debug`.
impl std::fmt::Debug for AgentSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSession")
            .field("session_id", &self.session_id)
            .field("transcript_len", &self.transcript.lock().unwrap().len())
            .finish()
    }
}

impl AgentSession {
    pub fn new(session_id: impl Into<String>, loop_: Arc<AgentLoop>, memory: Arc<SharedMemoryStore>) -> Self {
        Self {
            session_id: session_id.into(),
            transcript: Mutex::new(Vec::new()),
            loop_,
            memory,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn transcript(&self) -> Vec<ChatMessage> {
        self.transcript.lock().unwrap().clone()
    }

    pub fn push(&self, message: ChatMessage) {
        self.transcript.lock().unwrap().push(message);
    }

    pub fn agent_loop(&self) -> &Arc<AgentLoop> {
        &self.loop_
    }

    pub fn memory(&self) -> &Arc<SharedMemoryStore> {
        &self.memory
    }
}

/// A stub tool named `echo` for exercising the registry in tests.
#[cfg(test)]
pub struct EchoTool;

#[cfg(test)]
impl Tool for EchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn description(&self) -> &'static str {
        "stub echo tool for tests"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_sane() {
        let cfg = LoopConfig::default();
        assert_eq!(cfg.max_steps, 30);
        assert_eq!(cfg.duplicate_threshold, 2);
        assert!(cfg.auto_plan_on_multi_step);
        assert!(cfg.script_memory_enabled);
    }

    #[test]
    fn loop_builds_with_tools_and_exposes_them() {
        // llm/memory are plain stubs; constructing them synchronously is fine here.
        let llm = LlmClient::with_profile_stub("stub");
        let mem = SharedMemoryStore::new();
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(EchoTool)];
        let loop_ = AgentLoop::new(Arc::new(llm), Arc::new(mem), tools);
        assert_eq!(loop_.tools().len(), 1);
        assert_eq!(loop_.tools()[0].name(), "echo");
        assert_eq!(loop_.config().max_steps, 30);
    }

    #[test]
    fn session_transcript_records_messages() {
        let llm = LlmClient::with_profile_stub("stub");
        let mem = SharedMemoryStore::new();
        let tools: Vec<Arc<dyn Tool>> = vec![];
        let loop_ = Arc::new(AgentLoop::new(Arc::new(llm), Arc::new(mem), tools));
        let session = AgentSession::new("s1", loop_, Arc::new(SharedMemoryStore::new()));
        session.push(ChatMessage::User("hi".into()));
        session.push(ChatMessage::Assistant("hello".into()));
        assert_eq!(session.transcript().len(), 2);
        assert_eq!(session.session_id(), "s1");
    }
}
