//! AgentLoop, AgentSession, plan, script_memory, history summariser (M4).
//!
//! Defines the `AgentLoop` plan-act-observe driver (`plan_loop`), the tool
//! registry and `AgentSession` state machine (`tools` / `session`), the path
//! sandbox (`sandbox`), script-memory reuse + single-fix repair
//! (`script_memory`), and long-session compression (`summariser`).

use std::sync::Arc;

use webai_llm::LlmClient;
use webai_memory::SharedMemoryStore;

pub mod plan_loop;
pub mod runner;
pub mod sandbox;
pub mod script_memory;
pub mod session;
pub mod summariser;
pub mod tools;

pub use plan_loop::{requires_plan, LoopError, LoopGuards, PLAN_DIRECTIVE, STATE_BLOCK_MARKER};
pub use runner::{
    outcome_code, AgentRunner, AgentStep, RunConfig, StepOutcome, StubExecutor, ToolExecutor,
};
pub use sandbox::{PathSandbox, SandboxError};
pub use script_memory::{RepairError, ReuseHit, ScriptMemory};
pub use session::{AgentSession, SessionOptions, SessionState};
pub use summariser::{HistorySummariser, Role, SummarisedHistory, SummariserConfig, Turn};
pub use tools::{
    AcpNotifyTool, BrowserTool, FilesystemTool, LlmTool, MemoryTool, TerminateTool, ToolRegistry,
};

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

/// The plan-act-observe loop driver (ARCHITECTURE.md §4.9). Holds the guard
/// configuration and dependencies; the LLM-driven step loop and the M4
/// orchestration driver land in `plan_loop::PlanRunner`.
pub struct AgentLoop {
    config: LoopConfig,
    llm: Arc<LlmClient>,
    memory: Arc<SharedMemoryStore>,
    tools: Vec<Arc<dyn Tool>>,
}

impl AgentLoop {
    pub fn new(
        llm: Arc<LlmClient>,
        memory: Arc<SharedMemoryStore>,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Self {
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
        let llm = LlmClient::with_profile_stub("stub");
        let mem = SharedMemoryStore::new();
        let tools: Vec<Arc<dyn Tool>> = vec![];
        let loop_ = AgentLoop::new(Arc::new(llm), Arc::new(mem), tools);
        assert_eq!(loop_.tools().len(), 0);
        assert_eq!(loop_.config().max_steps, 30);
    }
}
