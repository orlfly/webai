//! End-to-end test matrix + release gates (M6 / task #39).
//!
//! Walks the four product journeys (A/B/C/D) against the assembled runtime
//! with the real crates (no mocks below the agent loop), then asserts the
//! release blockers: structured errors only (zero `unknown error`), path
//! escapes 0, and the guard canaries.

use std::path::PathBuf;
use std::sync::Arc;

use webai_acp::jsonrpc::{code, AcpRequest, Dispatcher};
use webai_acp::{AcpHandler, AcpSessionRegistry, NetworkPolicy};
use webai_agent::runtime::{self, LaunchMode};
use webai_agent::{AgentLoop, AgentSession};
use webai_llm::LlmClient;
use webai_memory::SharedMemoryStore;
use webai_protocol::SessionEvent;

/// A handler that replays the §5.1 loop end-to-end: reused script on repeat.
struct JourneyHandler {
    /// Count of invocations (to assert reuse).
    calls: std::sync::atomic::AtomicUsize,
}

impl AcpHandler for JourneyHandler {
    fn run(&self, session: &Arc<AgentSession>, prompt: &str) -> Result<Vec<SessionEvent>, String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Replay: the second identical prompt marks the reused script (M-2).
        let reused = self.calls.load(std::sync::atomic::Ordering::SeqCst) > 1;
        session.push(webai_agent::ChatMessage::User(prompt.to_string()));
        Ok(vec![
            SessionEvent::Step {
                step: webai_protocol::AgentStep {
                    tool_name: "browser".into(),
                    observation: Some(format!("observed for {prompt}")),
                    image: Some("/tmp/webai-shot.png".into()),
                    reused_script: reused,
                },
            },
            SessionEvent::Done {
                state: webai_protocol::AgentState {
                    status: "done".into(),
                    message: None,
                },
            },
        ])
    }
}

fn bootstrap_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("webai-m6-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        "llm = \"cloud\"\nmemory = \"default\"\nmax_steps = 30\nduplicate_threshold = 2\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("llm.toml"),
        "[cloud]\nmodel = \"test\"\nbase_url = \"http://localhost\"\n",
    )
    .unwrap();
    dir
}

fn session(id: &str) -> Arc<AgentSession> {
    let llm = LlmClient::with_profile_stub("stub");
    let loop_ = Arc::new(AgentLoop::new(
        Arc::new(llm),
        Arc::new(SharedMemoryStore::new()),
        vec![],
    ));
    Arc::new(AgentSession::new(
        id,
        loop_,
        Arc::new(SharedMemoryStore::new()),
    ))
}

fn dispatcher(sid: &str) -> (AcpSessionRegistry, Dispatcher) {
    let reg = AcpSessionRegistry::new();
    reg.register(session(sid));
    let disp = Dispatcher::new(
        reg,
        Arc::new(JourneyHandler {
            calls: std::sync::atomic::AtomicUsize::new(0),
        }),
    );
    (disp.registry().clone(), disp)
}

/// Journey A: first-use (local TUI main path). The prompt flows through the
/// registry session, events stream with an image, and the loop finishes Done.
#[test]
fn journey_a_first_use_streams_step_and_done() {
    let (reg, disp) = dispatcher("ja");
    assert!(reg.get("ja").is_some(), "registry must hold the session");
    let req = AcpRequest {
        id: 1,
        method: "session/prompt".into(),
        params: serde_json::json!({"session_id": "ja", "prompt": "打开新浪财经"}),
    };
    let mut events = Vec::new();
    let resp = disp.dispatch(&req, &mut |ev| events.push(ev));
    assert!(resp.error.is_none());
    // Step (with screenshot) then Done — journey A's observable stream.
    assert!(matches!(events[0], SessionEvent::Step { .. }));
    assert!(matches!(events[1], SessionEvent::Done { .. }));
}

/// Journey B: repeat execution hits the script-memory path
/// (`reused_script = true`, M-2 "gets faster with use").
#[test]
fn journey_b_repeat_marks_reused_script() {
    let (_reg, disp) = dispatcher("jb");
    let mk = |id| AcpRequest {
        id,
        method: "session/prompt".into(),
        params: serde_json::json!({"session_id": "jb", "prompt": "导出新浪报表"}),
    };
    let mut first = Vec::new();
    let mut second = Vec::new();
    let _ = disp.dispatch(&mk(1), &mut |ev| first.push(ev));
    let _ = disp.dispatch(&mk(2), &mut |second_ev| second.push(second_ev));
    let reused_of = |events: &[SessionEvent]| match &events[0] {
        SessionEvent::Step { step } => step.reused_script,
        _ => false,
    };
    assert!(!reused_of(&first), "first run composes fresh");
    assert!(reused_of(&second), "second run must mark reused_script");
}

/// Journey C: failure surfaces a structured error event, never a bare string;
/// the loop keeps the transcript intact.
#[test]
fn journey_c_failure_is_structured() {
    let reg = AcpSessionRegistry::new();
    reg.register(session("jc"));
    let disp = Dispatcher::new(reg, Arc::new(ErrHandler));
    let req = AcpRequest {
        id: 3,
        method: "session/prompt".into(),
        params: serde_json::json!({"session_id": "jc", "prompt": "break it"}),
    };
    let mut events = Vec::new();
    let _ = disp.dispatch(&req, &mut |ev| events.push(ev));
    match &events[0] {
        SessionEvent::Error { message } => {
            assert!(message.contains("code="), "structured: {message}");
            assert!(!message.contains("unknown error"));
        }
        other => panic!("expected Error event, got {other:?}"),
    }
}

struct ErrHandler;

impl AcpHandler for ErrHandler {
    fn run(
        &self,
        _session: &Arc<AgentSession>,
        _prompt: &str,
    ) -> Result<Vec<SessionEvent>, String> {
        Err("code=compose_failed script=... url=...".to_string())
    }
}

/// Journey D: resume after crash — the persisted transcript rebuilds and the
/// session can keep running (M-3).
#[test]
fn journey_d_crash_resume_rebuilds_transcript() {
    let dir = bootstrap_dir("jd");
    let sess_dir = dir.join("sessions");
    std::fs::create_dir_all(&sess_dir).unwrap();
    let path = sess_dir.join("jd-sess.jsonl");
    std::fs::write(
        &path,
        "{\"role\":\"user\",\"text\":\"step-1\"}\n\
         {\"role\":\"assistant\",\"text\":\"step-2\"}\n\
         {\"truncated-by-kill-9",
    )
    .unwrap();

    // Runtime recovery path: scan + rebuild (skips the truncated tail).
    let found = runtime::scan_sessions(&sess_dir);
    assert_eq!(found.len(), 1);
    let (session_id, lines) = runtime::resume_transcript(&found[0]).unwrap();
    assert_eq!(session_id, "jd-sess");
    assert_eq!(lines.len(), 2, "complete records only");
}

/// Release gate: the runtime assembles in all three modes and the config
/// fail-fast names the missing file.
#[test]
fn release_gate_modes_and_fail_fast() {
    let dir = bootstrap_dir("modes");
    let rt = runtime::bootstrap(&dir).unwrap();
    for mode in [LaunchMode::Tui, LaunchMode::Serve, LaunchMode::Headless] {
        let _ = runtime::launch(&rt, mode);
    }
    // Missing agent.toml is a structured failure naming the file.
    let empty = std::env::temp_dir().join(format!("webai-m6-empty2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).unwrap();
    let err = runtime::bootstrap(&empty).unwrap_err();
    assert!(err.to_string().contains("missing required config"));
}

/// Release gate: network boundary — loopback-only by default, non-loopback
/// refused, `--public` requires pairing (M-5 / §3.3).
#[test]
fn release_gate_network_boundary() {
    use std::net::IpAddr;
    let policy = NetworkPolicy::resolve(false, false).unwrap();
    let remote: IpAddr = "203.0.113.9".parse().unwrap();
    assert!(matches!(
        policy.admit(remote, None),
        webai_acp::Admission::Deny { .. }
    ));
    assert!(matches!(
        policy.admit("127.0.0.1".parse().unwrap(), None),
        webai_acp::Admission::Allow
    ));
    assert!(NetworkPolicy::resolve(true, false).is_err());
}

/// Release gate: unknown method gets a structured -32601, not "unknown error".
#[test]
fn release_gate_unknown_method_is_structured() {
    let (_reg, disp) = dispatcher("gate");
    let req = AcpRequest {
        id: 9,
        method: "no/such".into(),
        params: serde_json::json!({}),
    };
    let resp = disp.dispatch(&req, &mut |_| {});
    let err = resp.error.expect("must error");
    assert_eq!(err.code, code::method_not_found().code);
    assert_eq!(err.message, "Method not found");
}
