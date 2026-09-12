//! Six-tool registry (ARCHITECTURE.md §4.9): `browser` / `memory` /
//! `filesystem` / `llm` / `acp_notify` / `terminate`.
//!
//! Each tool implements the `Tool` trait from the crate root. `filesystem` and
//! `memory` carry a `PathSandbox` so non-authorized paths are rejected (FR-8).
//! The registry is the single enumerable source of truth; the loop dispatches
//! by tool name.

use std::path::PathBuf;
use std::sync::Arc;

use webai_llm::LlmClient;
use webai_memory::SharedMemoryStore;

use crate::sandbox::PathSandbox;
use crate::Tool;

/// The `browser` tool: drives the browser via composed scripts. In M4 it
/// dispatches to `webai-bridge`; here it carries the verb surface contract.
pub struct BrowserTool;

impl Tool for BrowserTool {
    fn name(&self) -> &'static str {
        "browser"
    }
    fn description(&self) -> &'static str {
        "drive the browser with script-author verbs (navigate/click/fill/get_text/..)"
    }
}

/// The `memory` tool: read/write the shared memory store under a path sandbox.
pub struct MemoryTool {
    store: SharedMemoryStore,
    sandbox: PathSandbox,
}

impl MemoryTool {
    pub fn new(store: SharedMemoryStore, sandbox_root: impl Into<PathBuf>) -> Self {
        Self {
            store,
            sandbox: PathSandbox::new(sandbox_root),
        }
    }

    pub fn store(&self) -> &SharedMemoryStore {
        &self.store
    }

    pub fn sandbox(&self) -> &PathSandbox {
        &self.sandbox
    }
}

impl Tool for MemoryTool {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn description(&self) -> &'static str {
        "read/write cross-session memory, sandboxed to the allowed directory"
    }
}

/// The `filesystem` tool: read/write files, enforcing the path sandbox.
pub struct FilesystemTool {
    sandbox: PathSandbox,
}

impl FilesystemTool {
    pub fn new(sandbox_root: impl Into<PathBuf>) -> Self {
        Self {
            sandbox: PathSandbox::new(sandbox_root),
        }
    }

    pub fn sandbox(&self) -> &PathSandbox {
        &self.sandbox
    }
}

impl Tool for FilesystemTool {
    fn name(&self) -> &'static str {
        "filesystem"
    }
    fn description(&self) -> &'static str {
        "read/write files, sandboxed to the allowed directory"
    }
}

/// The `llm` tool: call the configured LLM provider (via `LlmClient`).
pub struct LlmTool {
    client: Arc<LlmClient>,
}

impl LlmTool {
    pub fn new(client: Arc<LlmClient>) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &Arc<LlmClient> {
        &self.client
    }
}

impl Tool for LlmTool {
    fn name(&self) -> &'static str {
        "llm"
    }
    fn description(&self) -> &'static str {
        "invoke the configured LLM provider"
    }
}

/// The `acp_notify` tool: push session events to the frontend (TUI / ACP).
pub struct AcpNotifyTool;

impl Tool for AcpNotifyTool {
    fn name(&self) -> &'static str {
        "acp_notify"
    }
    fn description(&self) -> &'static str {
        "notify the frontend of session events"
    }
}

/// The `terminate` tool: end the current agent loop / session.
pub struct TerminateTool;

impl Tool for TerminateTool {
    fn name(&self) -> &'static str {
        "terminate"
    }
    fn description(&self) -> &'static str {
        "end the current agent loop and close the session"
    }
}

/// The full six-tool registry (ARCHITECTURE.md §4.9).
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Construct the default six-tool registry.
    ///
    /// `filesystem_root` / `memory_root` are the sandbox roots for the
    /// filesystem and memory tools (FR-8 path sandbox). They may be the same
    /// or different authorized directories.
    pub fn default(
        llm: Arc<LlmClient>,
        memory: SharedMemoryStore,
        filesystem_root: impl Into<PathBuf>,
        memory_root: impl Into<PathBuf>,
    ) -> Self {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(BrowserTool),
            Arc::new(MemoryTool::new(memory, memory_root)),
            Arc::new(FilesystemTool::new(filesystem_root)),
            Arc::new(LlmTool::new(llm)),
            Arc::new(AcpNotifyTool),
            Arc::new(TerminateTool),
        ];
        Self { tools }
    }

    /// All registered tools.
    pub fn tools(&self) -> &[Arc<dyn Tool>] {
        &self.tools
    }

    /// Look up a tool by (dotted) name, e.g. `"browser"`, `"browser.navigate"`.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let base = name.split('.').next()?;
        self.tools.iter().find(|t| t.name() == base).cloned()
    }

    /// Names of every registered tool, in declaration order.
    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("webai-tools-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn registry() -> ToolRegistry {
        let llm = Arc::new(LlmClient::with_profile_stub("stub"));
        let root = tmp_root("reg");
        ToolRegistry::default(llm, SharedMemoryStore::new(), root.clone(), root)
    }

    #[test]
    fn registry_enumerates_six_tools() {
        let reg = registry();
        assert_eq!(reg.len(), 6);
        assert_eq!(
            reg.names(),
            vec![
                "browser",
                "memory",
                "filesystem",
                "llm",
                "acp_notify",
                "terminate"
            ]
        );
    }

    #[test]
    fn registry_get_resolves_base_and_dotted_name() {
        let reg = registry();
        assert_eq!(reg.get("browser").map(|t| t.name()), Some("browser"));
        assert_eq!(
            reg.get("browser.navigate").map(|t| t.name()),
            Some("browser")
        );
        assert_eq!(reg.get("terminate").map(|t| t.name()), Some("terminate"));
        assert!(reg.get("nope").is_none());
    }

    #[test]
    fn filesystem_tool_sandbox_rejects_escape() {
        let root = tmp_root("fs");
        let tool = FilesystemTool::new(root.clone());
        // Absolute path outside the (temp) root is refused by the guard.
        let resolved = tool.sandbox().sanitize(Path::new("/etc/passwd"));
        assert!(resolved.is_err());
        // A path escaping via ".." is refused.
        assert!(tool
            .sandbox()
            .sanitize(Path::new("../../etc/passwd"))
            .is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
