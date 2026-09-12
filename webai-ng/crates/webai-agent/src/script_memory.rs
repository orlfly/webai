//! Script-memory reuse + single-fix repair path (ARCHITECTURE 定论四 / §5.2).
//!
//! Before scheduling a browser tool, we consult the shared memory store
//! (`recall_scripts`) and reuse a remembered script for the same verb
//! (`reused_script = true`). On success we remember the script
//! (`{task, verb, url, script}`, tag `script:{verb}`). When a composed or
//! replayed script fails, we run exactly one LLM repair pass with a fixed
//! context; a second failure surfaces a structured error so the AgentLoop
//! decides (change path or ask the user) instead of looping forever.
//!
//! `script_memory_enabled = false` degrades to direct generation with zero
//! memory reads or writes.

use webai_llm::{ChatMessage, ChatRole, LlmClient};
use webai_memory::{MemoryError, ScriptMemoryEntry, SharedMemoryStore};

/// A script-replay decision from the memory layer.
#[derive(Debug, Clone)]
pub struct ReuseHit {
    /// The remembered script entry to execute.
    pub entry: ScriptMemoryEntry,
    /// True when this was pulled from memory (vs freshly composed).
    pub reused_script: bool,
}

/// The script-memory controller: reuse + remember + repair.
///
/// When `enabled` is false, every call is a direct generation (no memory
/// reads and no memory writes).
pub struct ScriptMemory {
    store: SharedMemoryStore,
    enabled: bool,
}

impl ScriptMemory {
    /// Construct with the shared store and the `script_memory_enabled` flag.
    pub fn new(store: SharedMemoryStore, enabled: bool) -> Self {
        Self { store, enabled }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Look for a remembered script for `verb`. Returns `(entry, true)` when a
    /// script with the same verb is found and memory is enabled.
    pub fn reuse(&self, verb: &str, task: &str) -> Option<ReuseHit> {
        if !self.enabled {
            return None;
        }
        for entry in self.store.recall_scripts(task, 8) {
            if entry.verb == verb {
                return Some(ReuseHit {
                    entry,
                    reused_script: true,
                });
            }
        }
        None
    }

    /// Remember a successful script. When disabled this is a no-op success
    /// (degraded run with zero writes).
    pub fn remember(&self, entry: ScriptMemoryEntry) -> Result<(), MemoryError> {
        if !self.enabled {
            return Ok(());
        }
        self.store.write_script(entry)
    }

    /// Update an existing remembered entry after a successful repair, keeping
    /// the same id so repeated learns don't accumulate duplicates.
    pub fn remember_fixed(
        &self,
        prev: &ScriptMemoryEntry,
        fixed_script: impl Into<String>,
    ) -> Result<(), MemoryError> {
        if !self.enabled {
            return Ok(());
        }
        let mut entry = prev.clone();
        entry.script = fixed_script.into();
        self.store.write_script(entry)
    }

    /// Build the fixed repair context (ARCHITECTURE §5.2): failed script + JS
    /// error + ≤3 similar remembered scripts + recent steps + page summary.
    pub fn repair_prompt(
        &self,
        failed_script: &str,
        error_text: &str,
        similar: &[ScriptMemoryEntry],
        recent_steps: &[String],
        page_summary: &str,
    ) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("A browser script failed. Fix the script so it succeeds.\n\n");

        out.push_str("## Failed script\n```js\n");
        out.push_str(failed_script);
        out.push_str("\n```\n");

        out.push_str("\n## Error\n");
        out.push_str(error_text);
        out.push('\n');

        if !similar.is_empty() {
            out.push_str("\n## Similar remembered scripts (max 3)\n");
            for (i, s) in similar.iter().take(3).enumerate() {
                out.push_str(&format!(
                    "[{i}] ({}) {}\n```js\n{}\n```\n",
                    s.verb, s.task, s.script
                ));
            }
        }

        if !recent_steps.is_empty() {
            out.push_str("\n## Recent steps\n");
            for step in recent_steps.iter().rev().take(5) {
                out.push_str("- ");
                out.push_str(step);
                out.push('\n');
            }
        }

        out.push_str("\n## Current page\n");
        out.push_str(page_summary);

        out.push_str("\n\nReturn ONLY the corrected JavaScript, no commentary.");
        out
    }

    /// Run the single-fix repair call. This is *one* LLM call; callers must
    /// not loop on it.
    pub async fn repair_once(&self, llm: &LlmClient, prompt: &str) -> Result<String, RepairError> {
        if !self.enabled {
            return Err(RepairError::Disabled);
        }
        let messages = vec![ChatMessage::Text(ChatRole::User, prompt.to_string())];
        let out = llm
            .complete(&messages_as_prompt(&messages))
            .await
            .map_err(|e| RepairError::Backend(format!("{e}")))?;
        if out.trim().is_empty() {
            return Err(RepairError::Empty);
        }
        Ok(out)
    }
}

/// Serialize a message slice into a single prompt (LLM `complete` takes a
/// plain string). Only the last user text is used for the repair path.
pub fn messages_as_prompt(messages: &[ChatMessage]) -> String {
    let mut out = String::new();
    for m in messages {
        if let ChatMessage::Text(_role, text) = m {
            out.push_str(text);
        } else if let ChatMessage::Image { text, .. } = m {
            out.push_str(text);
        }
    }
    out
}

/// Structured errors from the repair path (FR-3 / §5.2: no bare strings).
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RepairError {
    #[error("script memory disabled; regenerating directly")]
    Disabled,
    #[error("LLM repair returned an empty script")]
    Empty,
    #[error("LLM repair backend failed: {0}")]
    Backend(String),
}

impl RepairError {
    pub fn code(&self) -> &'static str {
        match self {
            RepairError::Disabled => "script_memory_disabled",
            RepairError::Empty => "repair_empty",
            RepairError::Backend(_) => "repair_backend",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(verb: &str, task: &str, script: &str) -> ScriptMemoryEntry {
        ScriptMemoryEntry {
            task: task.into(),
            verb: verb.into(),
            url: "https://example.com".into(),
            script: script.into(),
            tags: vec![],
            id: format!("{verb}-{task}"),
        }
    }

    #[test]
    fn reuse_hits_same_verb_and_marks_reused() {
        let store = SharedMemoryStore::new();
        store
            .write_script(entry("click", "submit login", "window.c('x')"))
            .unwrap();
        let mem = ScriptMemory::new(store, true);
        let hit = mem.reuse("click", "submit login").unwrap();
        assert!(hit.reused_script);
        assert_eq!(hit.entry.script, "window.c('x')");
    }

    #[test]
    fn reuse_ignored_when_disabled() {
        let store = SharedMemoryStore::new();
        store
            .write_script(entry("click", "submit login", "x"))
            .unwrap();
        let mem = ScriptMemory::new(store, false);
        assert!(mem.reuse("click", "submit login").is_none());
        assert!(!mem.enabled());
    }

    #[test]
    fn remember_is_noop_when_disabled() {
        let store = SharedMemoryStore::new();
        let mem = ScriptMemory::new(store.clone(), false);
        mem.remember(entry("click", "t", "s")).unwrap();
        assert_eq!(store.len(), 0, "no memory write when disabled");
    }

    #[test]
    fn remember_fixed_keeps_id_and_updates_script() {
        let store = SharedMemoryStore::new();
        store.write_script(entry("click", "t", "old")).unwrap();
        let mem = ScriptMemory::new(store.clone(), true);
        let prev = store.recall_scripts("t", 1).remove(0);
        mem.remember_fixed(&prev, "new_script".to_string()).unwrap();
        let updated = store.recall_scripts("t", 1).remove(0);
        assert_eq!(updated.id, prev.id);
        assert_eq!(updated.script, "new_script");
    }

    #[test]
    fn repair_context_has_all_fixed_sections() {
        let store = SharedMemoryStore::new();
        let mem = ScriptMemory::new(store, true);
        let prompt = mem.repair_prompt(
            "window.bad()",
            "ReferenceError: bad is not defined",
            &[entry("click", "t", "s1"), entry("fill", "t", "s2")],
            &["step 1".to_string(), "step 2".to_string()],
            "a login page",
        );
        assert!(prompt.contains("window.bad()"));
        assert!(prompt.contains("ReferenceError"));
        assert!(prompt.contains("s1"));
        assert!(prompt.contains("## Recent steps"));
        assert!(prompt.contains("## Current page"));
    }
}
