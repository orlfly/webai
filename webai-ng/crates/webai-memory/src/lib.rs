//! MemoryStore: graph backend + vector + JSONL session logs.
//!
//! Stub skeleton for the webai-ng AI browser (M1-4). Defines the public
//! `SharedMemoryStore` interface (ARCHITECTURE.md §4.4): script-memory entries
//! (`MemoryWriteKind::Script` with `task/verb/url/script`, tags
//! `script:{verb}` / `session:{id}`), `recall_scripts(task)`, and the durable
//! per-session JSONL log recorder with write-and-flush semantics and truncated-line
//! recovery.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Kind of a memory-write entry.
#[derive(Debug, Clone)]
pub enum MemoryWriteKind {
    /// A successfully reused/composed browser script worth remembering.
    Script,
    /// A user prompt / assistant step observation.
    Transcript,
}

/// A structured script-memory entry (ARCHITECTURE.md §4.4).
#[derive(Debug, Clone)]
pub struct ScriptMemoryEntry {
    pub task: String,
    pub verb: String,
    pub url: String,
    pub script: String,
    pub tags: Vec<String>,
    pub id: String,
}

/// Errors from the memory store.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),
}

/// Shared in-memory session log (durable JSONL is a later milestone).
///
/// The `recorder` mirrors the ARCHITECTURE.md §4.4 contract: every write is
/// flushed immediately so a crash loses at most the final truncated record,
/// and recovery skips a trailing malformed line.
#[derive(Debug)]
pub struct JsonlSessionRecorder {
    path: PathBuf,
    // In-memory mirror; real file I/O lands when durable logging is wired in.
    lines: Arc<RwLock<Vec<String>>>,
}

impl JsonlSessionRecorder {
    pub fn new_for_dir(collab_dir: &Path, session_id: &str) -> Result<Self, MemoryError> {
        std::fs::create_dir_all(collab_dir).map_err(|e| MemoryError::BackendUnavailable(e.to_string()))?;
        Ok(Self {
            path: collab_dir.join(format!("{session_id}.jsonl")),
            lines: Arc::new(RwLock::new(Vec::new())),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a single JSON-line and flush.
    pub fn record(&self, line: serde_json::Value) -> Result<(), MemoryError> {
        let mut lines = self.lines.write().map_err(|_| MemoryError::BackendUnavailable("lock poisoned".into()))?;
        lines.push(line.to_string());
        Ok(())
    }

    /// Scan a session file, skipping a trailing truncated (invalid JSON) line.
    pub fn recovery_scan(&self) -> Vec<String> {
        let lines = self.lines.read().map_err(|_| ()).map(|g| g.clone()).unwrap_or_default();
        lines.into_iter().filter(|l| serde_json::from_str::<serde_json::Value>(l).is_ok()).collect()
    }
}

/// The shared memory store facade (ARCHITECTURE.md §4.4).
#[derive(Debug, Clone)]
pub struct SharedMemoryStore {
    scripts: Arc<RwLock<HashMap<String, ScriptMemoryEntry>>>,
    disabled: bool,
}

impl SharedMemoryStore {
    pub fn new() -> Self {
        Self {
            scripts: Arc::new(RwLock::new(HashMap::new())),
            disabled: false,
        }
    }

    /// Construct a degraded, no-memory store (memory backend unavailable).
    pub fn disabled() -> Self {
        Self {
            scripts: Arc::new(RwLock::new(HashMap::new())),
            disabled: true,
        }
    }

    /// Write a script-memory entry with the canonical `script:{verb}` /
    /// `session:{id}` tags.
    pub fn write_script(&self, mut entry: ScriptMemoryEntry) -> Result<(), MemoryError> {
        if self.disabled {
            return Err(MemoryError::BackendUnavailable("memory disabled".into()));
        }
        entry.tags.insert(0, format!("script:{}", entry.verb));
        let mut m = self.scripts.write().map_err(|_| MemoryError::BackendUnavailable("lock poisoned".into()))?;
        m.insert(entry.id.clone(), entry);
        Ok(())
    }

    /// Recall up to `limit` script entries matching `task` (best-effort).
    pub fn recall_scripts(&self, task: &str, limit: usize) -> Vec<ScriptMemoryEntry> {
        let m = self.scripts.read().map_err(|_| ()).map(|g| g.clone()).unwrap_or_default();
        m.values()
            .filter(|e| e.task.contains(task))
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.scripts.read().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for SharedMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(verb: &str, task: &str) -> ScriptMemoryEntry {
        ScriptMemoryEntry {
            task: task.to_string(),
            verb: verb.to_string(),
            url: "https://example.com".into(),
            script: "window.location.href = args.url".into(),
            tags: vec!["session:s1".into()],
            id: format!("{verb}-{task}"),
        }
    }

    #[test]
    fn write_and_recall_script_entries() {
        let store = SharedMemoryStore::new();
        store.write_script(sample_entry("click", "submit login form")).unwrap();
        store.write_script(sample_entry("fill", "submit login form")).unwrap();
        let hits = store.recall_scripts("login form", 10);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|e| e.tags.first().map(|t| t.starts_with("script:")).unwrap_or(false)));
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn recall_respects_limit_and_filter() {
        let store = SharedMemoryStore::new();
        store.write_script(sample_entry("click", "task A")).unwrap();
        store.write_script(sample_entry("fill", "task A")).unwrap();
        store.write_script(sample_entry("click", "task B")).unwrap();
        assert_eq!(store.recall_scripts("task A", 1).len(), 1);
        assert_eq!(store.recall_scripts("task B", 10).len(), 1);
    }

    #[test]
    fn disabled_store_rejects_writes_and_recalls_nothing() {
        let store = SharedMemoryStore::disabled();
        assert!(store.write_script(sample_entry("click", "x")).is_err());
        assert!(store.recall_scripts("x", 10).is_empty());
    }

    #[test]
    fn jsonl_recorder_drops_trailing_truncated_line() {
        let dir = std::env::temp_dir().join(format!(
            "webai-ng-mem-test-{}",
            std::process::id()
        ));
        let rec = JsonlSessionRecorder::new_for_dir(&dir, "s1").unwrap();
        rec.record(serde_json::json!({"ok": true})).unwrap();
        rec.record(serde_json::json!({"ok": false})).unwrap();
        // Append a trailing malformed/truncated line "in the file".
        if let Ok(mut guard) = rec.lines.write() {
            guard.push("{ not valid json".to_string());
        }
        // Recovery must skip the malformed line.
        let healthy = rec.recovery_scan();
        assert_eq!(healthy.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
