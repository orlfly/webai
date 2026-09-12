//! JSONL session log (ARCHITECTURE.md §4.4 / §5.3, 定论五).
//!
//! The durable per-session JSONL log lives **only** in this crate. `AgentSession`
//! holds a write handle; there is no second appender anywhere in the repo.
//!
//! Contract:
//! - `~/.webai/sessions/<session_id>.jsonl`
//! - every write is followed by a flush, so a crash loses at most the final
//!   truncated record (FR-5 / M-3)
//! - `recovery_from_persisted_step` parses line-by-line, drops a trailing
//!   truncated (invalid JSON) line, and tolerates a bad middle line by skipping
//!   it rather than failing the whole recovery.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde_json::Value as Json;

/// Errors from the session log.
#[derive(Debug, thiserror::Error)]
pub enum SessionLogError {
    #[error("cannot create session dir {dir}: {err}")]
    MkdirFailed { dir: String, err: String },
    #[error("cannot open session log {path}: {err}")]
    OpenFailed { path: String, err: String },
    #[error("write failed: {0}")]
    WriteFailed(String),
    #[error("flush failed: {0}")]
    FlushFailed(String),
}

/// A durable JSONL session log.
pub struct JsonlSessionLog {
    path: PathBuf,
    file: File,
}

impl JsonlSessionLog {
    /// Open (creating if needed) the session log at
    /// `<sessions_dir>/<session_id>.jsonl`.
    pub fn open(sessions_dir: &Path, session_id: &str) -> Result<Self, SessionLogError> {
        std::fs::create_dir_all(sessions_dir).map_err(|e| SessionLogError::MkdirFailed {
            dir: sessions_dir.display().to_string(),
            err: e.to_string(),
        })?;
        let path = sessions_dir.join(format!("{session_id}.jsonl"));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| SessionLogError::OpenFailed {
                path: path.display().to_string(),
                err: e.to_string(),
            })?;
        Ok(Self { path, file })
    }

    /// The on-disk path of this session log.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a JSON line and flush immediately (write+flush semantics).
    pub fn record(&mut self, value: &Json) -> Result<(), SessionLogError> {
        let mut line = serde_json::to_string(value)
            .map_err(|e| SessionLogError::WriteFailed(e.to_string()))?;
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .map_err(|e| SessionLogError::WriteFailed(e.to_string()))?;
        self.file
            .flush()
            .map_err(|e| SessionLogError::FlushFailed(e.to_string()))
    }

    /// Record a user prompt.
    pub fn record_user_prompt(&mut self, text: &str) -> Result<(), SessionLogError> {
        self.record(&serde_json::json!({ "kind": "user_prompt", "text": text }))
    }

    /// Record an assistant step.
    pub fn record_step(&mut self, step: &Json) -> Result<(), SessionLogError> {
        self.record(&serde_json::json!({ "kind": "step", "step": step }))
    }

    /// Record a finished marker.
    pub fn record_finished(&mut self) -> Result<(), SessionLogError> {
        self.record(&serde_json::json!({ "kind": "finished" }))
    }

    /// Record an error.
    pub fn record_error(&mut self, message: &str) -> Result<(), SessionLogError> {
        self.record(&serde_json::json!({ "kind": "error", "message": message }))
    }

    /// Close the log (flush + drop the file handle).
    pub fn close(mut self) -> Result<(), SessionLogError> {
        self.file
            .flush()
            .map_err(|e| SessionLogError::FlushFailed(e.to_string()))
    }
}

/// Recover a session transcript from a persisted JSONL file.
///
/// Parses line-by-line. A trailing truncated (invalid JSON) line is dropped
/// (crash mid-write). A bad middle line is skipped rather than failing the
/// whole recovery. Returns the list of valid parsed records.
pub fn recovery_from_persisted_step(path: &Path) -> Vec<Json> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue, // unreadable line: skip
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Json>(trimmed) {
            Ok(v) => out.push(v),
            Err(_) => {
                // A bad line: if it's the last line it's a truncated write
                // (drop it); if it's a middle line, skip it (tolerate).
                // We can't know if it's last until we've read all lines, so
                // we buffer and drop a trailing invalid line at the end.
                out.push(Json::Null); // placeholder for invalid line
            }
        }
    }
    // Drop a trailing invalid (Null placeholder) line — the truncated write.
    while out.last() == Some(&Json::Null) {
        out.pop();
    }
    // Remove any remaining invalid placeholders in the middle (bad-line tolerance).
    out.retain(|v| !v.is_null());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique per test (name + pid) so parallel tests that remove their
    /// directory cannot delete another test's log mid-run.
    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("webai-jsonl-{name}-{}", std::process::id()))
    }

    #[test]
    fn write_flush_is_immediately_visible() {
        let dir = temp_dir("write_flush");
        let mut log = JsonlSessionLog::open(&dir, "s1").unwrap();
        log.record_user_prompt("hello").unwrap();
        // After record (which flushes), the file must contain the line.
        let content = std::fs::read_to_string(log.path()).unwrap();
        assert!(content.contains("user_prompt"));
        assert!(content.contains("hello"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_all_kinds_writes_lines() {
        let dir = temp_dir("all_kinds");
        let mut log = JsonlSessionLog::open(&dir, "s2").unwrap();
        log.record_user_prompt("hi").unwrap();
        log.record_step(&serde_json::json!({ "tool": "browser.click" }))
            .unwrap();
        log.record_finished().unwrap();
        log.record_error("boom").unwrap();
        let content = std::fs::read_to_string(log.path()).unwrap();
        assert_eq!(content.lines().count(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_drops_trailing_truncated_line() {
        let dir = temp_dir("recovery_trunc");
        let path = dir.join("s3.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            "{\"kind\":\"user_prompt\",\"text\":\"a\"}\n{\"kind\":\"step\",\"step\":{}}\n{ not valid json",
        )
        .unwrap();
        let recovered = recovery_from_persisted_step(&path);
        assert_eq!(recovered.len(), 2, "truncated last line must be dropped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_skips_bad_middle_line() {
        let dir = temp_dir("recovery_mid");
        let path = dir.join("s4.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            "{\"kind\":\"user_prompt\",\"text\":\"a\"}\n{ bad middle }\n{\"kind\":\"finished\"}\n",
        )
        .unwrap();
        let recovered = recovery_from_persisted_step(&path);
        assert_eq!(
            recovered.len(),
            2,
            "bad middle line must be skipped, not fail"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_empty_or_missing_file_returns_empty() {
        let dir = temp_dir("recovery_empty");
        let path = dir.join("missing.jsonl");
        assert!(recovery_from_persisted_step(&path).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
