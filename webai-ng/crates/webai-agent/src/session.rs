//! `AgentSession` state machine (ARCHITECTURE.md 定论五 / §4.9).
//!
//! A session is a first-class citizen with an explicit lifecycle: `New`
//! (新建) → `Resumed` (恢复/运行) → `Paused` (暂停) → `Closed` (关闭). The
//! state lives in the `webai-agent` crate and is not scattered across the
//! frontends. `AgentSession` also holds the JSONL write handle from
//! `webai-memory` (session logs are owned by webai-memory; the session only
//! keeps the handle) plus the optional shared memory store.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use webai_memory::{JsonlSessionRecorder, MemoryError, SharedMemoryStore};

use crate::AgentLoop;
use crate::ChatMessage;

/// Explicit session lifecycle state (ARCHITECTURE.md §4.9 / FR-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Created but not yet started (新建).
    New,
    /// Active and stepping (恢复 / 运行中).
    Resumed,
    /// Suspended; may be resumed later (暂停).
    Paused,
    /// Terminated; no further transitions (关闭).
    Closed,
}

impl SessionState {
    /// A stable machine-string for logs / wire events.
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionState::New => "new",
            SessionState::Resumed => "running",
            SessionState::Paused => "paused",
            SessionState::Closed => "closed",
        }
    }
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Structured error for an illegal session-state transition (FR-8 §7: no bare
/// strings, a stable code + phase + message).
#[derive(Debug, Clone, thiserror::Error)]
#[error("illegal session transition `{from}` -> `{to}`: {message}")]
pub struct SessionTransitionError {
    pub phase: &'static str,
    pub from: SessionState,
    pub to: SessionState,
    pub message: String,
}

impl SessionTransitionError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        "session_illegal_transition"
    }

    fn new(from: SessionState, to: SessionState, message: impl Into<String>) -> Self {
        Self {
            phase: "session",
            from,
            to,
            message: message.into(),
        }
    }
}

/// Configuration for creating a session via [`AgentSession::create`].
pub struct SessionOptions {
    /// Root dir for the JSONL session log (FR-5 `~/.webai/sessions/`).
    pub sessions_dir: std::path::PathBuf,
    /// Whether to attach a durable JSONL recorder (reused from webai-memory).
    pub enable_jsonl: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            sessions_dir: SessionOptions::default_sessions_dir(),
            enable_jsonl: true,
        }
    }
}

impl SessionOptions {
    fn default_sessions_dir() -> std::path::PathBuf {
        std::env::var("WEBAI_SESSIONS_DIR")
            .map(std::path::PathBuf::from)
            .ok()
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .map(|h| h.join(".webai").join("sessions"))
            })
            .unwrap_or_else(|| std::path::Path::new(".").join(".webai").join("sessions"))
    }
}

/// The durable session container (ARCHITECTURE.md §4.9): transcript +
/// `Arc<AgentLoop>` + explicit state + optional JSONL handle + optional
/// `Arc<SharedMemoryStore>`.
pub struct AgentSession {
    session_id: String,
    transcript: Mutex<Vec<ChatMessage>>,
    state: Mutex<SessionState>,
    loop_: Arc<AgentLoop>,
    memory: Arc<SharedMemoryStore>,
    recorder: Option<JsonlSessionRecorder>,
}

/// Manual `Debug` impl because `AgentLoop` contains a `dyn Tool` which is not
/// `Debug`.
impl std::fmt::Debug for AgentSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSession")
            .field("session_id", &self.session_id)
            .field("state", &self.state.lock().unwrap().as_str())
            .field("transcript_len", &self.transcript.lock().unwrap().len())
            .finish_non_exhaustive()
    }
}

impl AgentSession {
    /// Construct a session in the `New` state. This mirrors the original
    /// constructor with no durable recorder (used by tests and callers that
    /// manage JSONL themselves).
    pub fn new(
        session_id: impl Into<String>,
        loop_: Arc<AgentLoop>,
        memory: Arc<SharedMemoryStore>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            transcript: Mutex::new(Vec::new()),
            state: Mutex::new(SessionState::New),
            loop_,
            memory,
            recorder: None,
        }
    }

    /// Create a session in the `New` state with an attached JSONL recorder
    /// (reusing webai-memory session logs — this crate never writes its own
    /// appender).
    pub fn create(
        session_id: impl Into<String>,
        loop_: Arc<AgentLoop>,
        memory: Arc<SharedMemoryStore>,
        options: SessionOptions,
    ) -> Result<Self, MemoryError> {
        let id = session_id.into();
        let recorder = if options.enable_jsonl {
            Some(JsonlSessionRecorder::new_for_dir(
                &options.sessions_dir,
                &id,
            )?)
        } else {
            None
        };
        Ok(Self {
            session_id: id,
            transcript: Mutex::new(Vec::new()),
            state: Mutex::new(SessionState::New),
            loop_,
            memory,
            recorder,
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn state(&self) -> SessionState {
        *self.state.lock().unwrap()
    }

    /// The JSONL recorder handle (session log, owned by webai-memory).
    pub fn recorder(&self) -> Option<&JsonlSessionRecorder> {
        self.recorder.as_ref()
    }

    /// Serialize the full transcript + current state to a JSONL line for
    /// durable storage (FR-5). Only valid while the session is running.
    pub fn persist_snapshot(&self) -> Result<(), MemoryError> {
        let recorder = self
            .recorder
            .as_ref()
            .ok_or_else(|| MemoryError::BackendUnavailable("no recorder".into()))?;
        let transcript = self.transcript();
        let payload = serde_json::json!({
            "session_id": self.session_id,
            "state": self.state().as_str(),
            "transcript": transcript.iter().map(chat_message_to_json).collect::<Vec<_>>(),
        });
        recorder.record(payload)
    }

    // -- state machine transitions --------------------------------------------

    /// Start a `New` session (→ `Resumed`). Returns a structured error if the
    /// current state is not `New`.
    pub fn resume(&self) -> Result<(), SessionTransitionError> {
        let mut state = self.state.lock().unwrap();
        match *state {
            SessionState::New | SessionState::Paused => {
                *state = SessionState::Resumed;
                Ok(())
            }
            SessionState::Resumed => Err(SessionTransitionError::new(
                *state,
                SessionState::Resumed,
                "session is already running",
            )),
            SessionState::Closed => Err(SessionTransitionError::new(
                *state,
                SessionState::Resumed,
                "a closed session cannot be resumed",
            )),
        }
    }

    /// Resume from a prior session (FR-5). `restore_transcript` rebuilds the
    /// transcript, then the session enters `Resumed` and can keep running.
    pub fn resume_from_snapshot(
        &self,
        restore_transcript: Vec<ChatMessage>,
    ) -> Result<(), SessionTransitionError> {
        let mut state = self.state.lock().unwrap();
        if matches!(*state, SessionState::Closed) {
            return Err(SessionTransitionError::new(
                *state,
                SessionState::Resumed,
                "a closed session cannot be resumed",
            ));
        }
        let mut tr = self.transcript.lock().unwrap();
        tr.clear();
        tr.extend(restore_transcript);
        *state = SessionState::Resumed;
        Ok(())
    }

    /// Pause a running session (→ `Paused`).
    pub fn pause(&self) -> Result<(), SessionTransitionError> {
        let mut state = self.state.lock().unwrap();
        match *state {
            SessionState::Resumed => {
                *state = SessionState::Paused;
                Ok(())
            }
            current => Err(SessionTransitionError::new(
                current,
                SessionState::Paused,
                "only a running session can be paused",
            )),
        }
    }

    /// Move the session to the terminal `Closed` state. No transition is
    /// allowed out of `Closed`.
    pub fn close(&self) -> Result<(), SessionTransitionError> {
        let mut state = self.state.lock().unwrap();
        match *state {
            SessionState::Closed => Err(SessionTransitionError::new(
                *state,
                SessionState::Closed,
                "session is already closed",
            )),
            _ => {
                *state = SessionState::Closed;
                Ok(())
            }
        }
    }

    // -- transcript accessors (backward compatible) --------------------------

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

fn chat_message_to_json(msg: &ChatMessage) -> serde_json::Value {
    match msg {
        ChatMessage::User(text) => serde_json::json!({ "role": "user", "text": text }),
        ChatMessage::Assistant(text) => serde_json::json!({ "role": "assistant", "text": text }),
    }
}

/// Rebuild a transcript from recovered JSONL lines (FR-5 recovery): each line
/// is a `{"role": ...}` object with valid text fields.
pub fn transcript_from_jsonl(line: &str) -> Option<ChatMessage> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let role = value.get("role")?.as_str()?;
    let text = value.get("text")?.as_str()?;
    match role {
        "user" => Some(ChatMessage::User(text.to_owned())),
        "assistant" => Some(ChatMessage::Assistant(text.to_owned())),
        _ => None,
    }
}

/// Default root for file-tool sandboxing (FR-8). Reused by filesystem / memory
/// tools when no explicit root is supplied.
pub fn default_sandbox_root() -> std::path::PathBuf {
    std::env::var("WEBAI_TOOLS_DIR")
        .map(std::path::PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|h| h.join(".webai").join("tools"))
        })
        .unwrap_or_else(|| std::path::Path::new(".").join(".webai").join("tools"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use webai_llm::LlmClient;

    fn build_session(id: &str) -> AgentSession {
        let llm = Arc::new(LlmClient::with_profile_stub("stub"));
        let mem = Arc::new(SharedMemoryStore::new());
        let loop_ = Arc::new(AgentLoop::new(llm, mem, vec![]));
        AgentSession::new(id, loop_, Arc::new(SharedMemoryStore::new()))
    }

    #[test]
    fn new_session_starts_in_new_state() {
        let s = build_session("s1");
        assert_eq!(s.state(), SessionState::New);
    }

    #[test]
    fn resume_pause_close_lifecycle_transitions() {
        let s = build_session("s2");
        s.resume().unwrap();
        assert_eq!(s.state(), SessionState::Resumed);
        s.pause().unwrap();
        assert_eq!(s.state(), SessionState::Paused);
        s.resume().unwrap();
        assert_eq!(s.state(), SessionState::Resumed);
        s.close().unwrap();
        assert_eq!(s.state(), SessionState::Closed);
    }

    #[test]
    fn closed_session_rejects_further_transitions() {
        let s = build_session("s3");
        s.resume().unwrap();
        s.close().unwrap();
        let resume_err = s.resume().unwrap_err();
        assert_eq!(resume_err.code(), "session_illegal_transition");
        assert_eq!(resume_err.from, SessionState::Closed);
        assert_eq!(resume_err.to, SessionState::Resumed);
        // close on closed is also invalid.
        assert!(s.close().is_err());
    }

    #[test]
    fn pause_requires_running_state() {
        let s = build_session("s4");
        // Cannot pause a New session.
        let err = s.pause().unwrap_err();
        assert_eq!(err.from, SessionState::New);
    }

    #[test]
    fn resume_requires_not_closed() {
        let s = build_session("s5");
        s.resume().unwrap();
        s.close().unwrap();
        assert!(s.resume().is_err());
    }

    #[test]
    fn resume_from_snapshot_rebuilds_transcript() {
        let s = build_session("s6");
        s.resume_from_snapshot(vec![
            ChatMessage::User("hi".into()),
            ChatMessage::Assistant("yo".into()),
        ])
        .unwrap();
        assert_eq!(s.state(), SessionState::Resumed);
        assert_eq!(s.transcript().len(), 2);
    }

    #[test]
    fn transcript_jsonl_roundtrip() {
        let s = build_session("s7");
        s.push(ChatMessage::User("hello".into()));
        // Simulate recovery from a JSONL line.
        let line = serde_json::to_string(&serde_json::json!({
            "role": "user", "text": "hello"
        }))
        .unwrap();
        let restored = transcript_from_jsonl(&line).unwrap();
        assert!(matches!(restored, ChatMessage::User(t) if t == "hello"));
    }

    #[test]
    fn session_persist_snapshot_requires_recorder() {
        let s = build_session("s8");
        // No recorder attached via `new`, so persistence degrades cleanly.
        assert!(s.persist_snapshot().is_err());
    }
}
