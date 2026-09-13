//! MemoryStore: graph backend + vector + JSONL session logs.
//!
//! Implements the dual-channel memory store (ARCHITECTURE.md §4.4, FR-3/FR-6):
//! a graph channel (Kuzu) + vector channel (HNSW) for script-memory entries,
//! plus the durable per-session JSONL log (定论五).

pub mod dual_channel;
pub mod session_log;

pub use dual_channel::{
    MemoryConfig, MemoryError, MemoryWriteKind, ScriptMemoryEntry, SharedMemoryStore,
};
pub use session_log::{recovery_from_persisted_step, JsonlSessionLog, SessionLogError};

/// Shared in-memory session log (legacy recorder kept for compatibility).
///
/// The durable JSONL appender is [`JsonlSessionLog`] (see `session_log`).
#[derive(Debug)]
pub struct JsonlSessionRecorder {
    path: std::path::PathBuf,
    lines: std::sync::Arc<std::sync::RwLock<Vec<String>>>,
}

impl JsonlSessionRecorder {
    pub fn new_for_dir(
        collab_dir: &std::path::Path,
        session_id: &str,
    ) -> Result<Self, MemoryError> {
        std::fs::create_dir_all(collab_dir)
            .map_err(|e| MemoryError::BackendUnavailable(e.to_string()))?;
        Ok(Self {
            path: collab_dir.join(format!("{session_id}.jsonl")),
            lines: std::sync::Arc::new(std::sync::RwLock::new(Vec::new())),
        })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Append a single JSON-line to the in-memory mirror AND to the on-disk
    /// JSONL file (write + flush), so a crash loses at most a truncated tail
    /// that `recovery_scan` / `runtime::resume_transcript` skip.
    pub fn record(&self, line: serde_json::Value) -> Result<(), MemoryError> {
        let serialized = line.to_string();
        {
            let mut lines = self
                .lines
                .write()
                .map_err(|_| MemoryError::BackendUnavailable("lock poisoned".into()))?;
            lines.push(serialized.clone());
        }
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| {
                MemoryError::BackendUnavailable(format!("open {}: {e}", self.path.display()))
            })?;
        writeln!(file, "{serialized}").map_err(|e| {
            MemoryError::BackendUnavailable(format!("write {}: {e}", self.path.display()))
        })?;
        file.flush().map_err(|e| {
            MemoryError::BackendUnavailable(format!("flush {}: {e}", self.path.display()))
        })?;
        Ok(())
    }

    /// Scan a session file, skipping a trailing truncated (invalid JSON) line.
    pub fn recovery_scan(&self) -> Vec<String> {
        let disk = std::fs::read_to_string(&self.path).unwrap_or_default();
        let lines = if disk.is_empty() {
            self.lines.read().map(|g| g.clone()).unwrap_or_default()
        } else {
            disk.lines().map(String::from).collect()
        };
        lines
            .into_iter()
            .filter(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
            .collect()
    }
}
