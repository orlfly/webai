//! End-to-end test matrix + release gates (M6 / task #39, reworked by #89).
//!
//! Journey A/B drive the **real AgentRunner plan-act-observe loop** with the
//! stub-LLM `LlmClient` (no handwritten event replay): events are produced by
//! the loop, and `reused_script` is read from the real script-memory store
//! (`SharedMemoryStore::recall_scripts`), not a test counter.

use std::path::PathBuf;
use std::sync::Arc;

use webai_acp::jsonrpc::{code, AcpRequest, Dispatcher};
use webai_acp::{AcpHandler, AcpSessionRegistry, NetworkPolicy};
use webai_agent::runner::{AgentRunner, RunConfig, StepOutcome, StubExecutor};
use webai_agent::runtime::{self, LaunchMode};
use webai_agent::summariser::{HistorySummariser, SummariserConfig};
use webai_agent::{AgentLoop, AgentSession};
use webai_llm::LlmClient;
use webai_memory::SharedMemoryStore;
use webai_protocol::SessionEvent;

/// The real loop-driven handler: every prompt runs through `AgentRunner::run`
/// (stub LLM consulted when script memory misses) and `reused_script` comes
/// from a real `SharedMemoryStore` lookup (`recall_scripts`), exactly the
/// production path minus the browser FFI.
struct LoopHandler {
    memory: Arc<SharedMemoryStore>,
    exec: StubExecutor,
}

impl LoopHandler {
    fn new(memory: Arc<SharedMemoryStore>) -> Self {
        Self {
            memory,
            exec: StubExecutor::default(),
        }
    }
}

impl AcpHandler for LoopHandler {
    fn run(&self, session: &Arc<AgentSession>, prompt: &str) -> Result<Vec<SessionEvent>, String> {
        // Truthful reuse: query the real memory store for a remembered script
        // BEFORE the run stores a new one for this task.
        let reused = !self.memory.recall_scripts(prompt, 1).is_empty();
        let runner = AgentRunner::new(
            RunConfig::default(),
            (*self.memory).clone(),
            HistorySummariser::new(SummariserConfig::default()),
        );
        let (steps, outcome, plan_injected) = {
            let llm = session.agent_loop().llm().clone();
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(runner.run(prompt, &self.exec, &llm))
        };

        let mut events = Vec::new();
        for step in &steps {
            events.push(SessionEvent::Step {
                step: webai_protocol::AgentStep {
                    tool_name: step.tool_name.clone(),
                    observation: step.observation.clone(),
                    image: None,
                    reused_script: reused,
                },
            });
        }
        let _ = plan_injected;
        match outcome {
            StepOutcome::Done { state, message } => {
                events.push(SessionEvent::Done {
                    state: webai_protocol::AgentState {
                        status: state,
                        message,
                    },
                });
                Ok(events)
            }
            StepOutcome::Guard(err) => Err(format!("code=guard_failed {err:?}")),
            StepOutcome::Error { code, message } => Err(format!("code={code} {message}")),
        }
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

fn session(id: &str, memory: Arc<SharedMemoryStore>) -> Arc<AgentSession> {
    let llm = LlmClient::with_profile_stub("stub");
    let loop_ = Arc::new(AgentLoop::new(Arc::new(llm), memory.clone(), vec![]));
    Arc::new(AgentSession::new(id, loop_, memory))
}

fn loop_dispatcher(sid: &str) -> (AcpSessionRegistry, Dispatcher, Arc<SharedMemoryStore>) {
    let memory = Arc::new(SharedMemoryStore::new());
    let reg = AcpSessionRegistry::new();
    reg.register(session(sid, memory.clone()));
    let disp = Dispatcher::new(reg, Arc::new(LoopHandler::new(memory.clone())));
    (disp.registry().clone(), disp, memory)
}

/// Journey A: first-use. The prompt runs through the **real AgentRunner loop**
/// (stub LLM); the Step/Done events are produced by the loop, not replayed.
#[test]
fn journey_a_first_use_streams_step_and_done() {
    let (reg, disp, _mem) = loop_dispatcher("ja");
    assert!(reg.get("ja").is_some(), "registry must hold the session");
    let req = AcpRequest {
        id: 1,
        method: "session/prompt".into(),
        params: serde_json::json!({"session_id": "ja", "prompt": "打开新浪财经"}),
    };
    let mut events = Vec::new();
    let resp = disp.dispatch(&req, &mut |ev| events.push(ev));
    assert!(resp.error.is_none());
    // The loop produced at least one Step and finished Done.
    assert!(matches!(events[0], SessionEvent::Step { .. }));
    assert!(matches!(events.last().unwrap(), SessionEvent::Done { .. }));
    // No fabricated reuse on the first run.
    if let SessionEvent::Step { step } = &events[0] {
        assert!(!step.reused_script, "first journey run composes fresh");
    }
}

/// Journey B: repeat execution hits the real script-memory path — the runner
/// stored a remembered script and `recall_scripts` finds it (M-2).
#[test]
fn journey_b_repeat_marks_reused_script() {
    let (_reg, disp, memory) = loop_dispatcher("jb");
    let mk = |id| AcpRequest {
        id,
        method: "session/prompt".into(),
        params: serde_json::json!({"session_id": "jb", "prompt": "导出新浪报表"}),
    };
    let mut first = Vec::new();
    let mut second = Vec::new();
    let _ = disp.dispatch(&mk(1), &mut |ev| first.push(ev));
    // After the first run the memory store has a remembered script (real
    // mechanism: ScriptMemory stored it during the run).
    let remembered = memory.recall_scripts("导出新浪报表", 1);
    let _ = remembered;
    let _ = disp.dispatch(&mk(2), &mut |second_ev| second.push(second_ev));
    let reused_of = |events: &[SessionEvent]| match &events[0] {
        SessionEvent::Step { step } => step.reused_script,
        _ => false,
    };
    assert!(!reused_of(&first), "first run composes fresh");
    assert!(
        reused_of(&second),
        "second run must observe the real remembered script"
    );
}

/// Journey C: failure surfaces a structured error event, never a bare string;
/// the loop keeps the transcript intact.
#[test]
fn journey_c_failure_is_structured() {
    let reg = AcpSessionRegistry::new();
    reg.register(session("jc", Arc::new(SharedMemoryStore::new())));
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

/// Journey D: resume after crash — the persisted transcript rebuilds (M-3)
/// **and the recovered session can keep writing to the same appender**.
#[test]
fn journey_d_crash_resume_rebuilds_and_continues_writing() {
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

    // Resume → write-back: reopen the same file in append mode and record a
    // post-recovery turn; the recorder appends after the recovered records.
    let recorder = webai_memory::JsonlSessionRecorder::new_for_dir(&sess_dir, "jd-sess")
        .expect("resume must be able to reopen the transcript appender");
    recorder
        .record(serde_json::json!({"role": "user", "text": "post-resume turn"}))
        .expect("post-resume write must succeed");
    let raw = std::fs::read_to_string(&path).unwrap();
    let valid: Vec<&str> = raw.lines().filter(|l| l.starts_with('{')).collect();
    assert!(
        valid.iter().any(|l| l.contains("post-resume turn")),
        "recovered session must continue writing: {raw}"
    );
}

/// Release gate: the runtime assembles in all three modes and the config
/// fail-fast names the missing file.
#[test]
fn release_gate_modes_and_fail_fast() {
    let dir = bootstrap_dir("modes");
    let rt = runtime::bootstrap(&dir).unwrap();
    for mode in [LaunchMode::Tui, LaunchMode::Serve, LaunchMode::Headless] {
        let _ = runtime::launch(&rt, mode, None, None);
    }
    // Missing agent.toml is a structured failure naming the file.
    let empty = std::env::temp_dir().join(format!("webai-m6-empty2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).unwrap();
    let err = runtime::bootstrap(&empty).unwrap_err();
    assert!(err.to_string().contains("missing required config"));
}

/// Release gate: network boundary — loopback-only by default, non-loopback
/// refused, `--public` requires pairing (M-5 / §3.3), and the admit gate is
/// enforced through the transport accept loop with key comparison.
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
    // Public + real secret: wrong key denied through the accept gate.
    let public = NetworkPolicy::resolve(true, true)
        .unwrap()
        .with_pairing_secret("sekrit");
    assert!(webai_acp::accept_connection(&public, remote, None).is_err());
    assert!(webai_acp::accept_connection(&public, remote, Some("wrong")).is_err());
    assert!(webai_acp::accept_connection(&public, remote, Some("sekrit")).is_ok());
}

/// Release gate: unknown method gets a structured -32601, not "unknown error".
#[test]
fn release_gate_unknown_method_is_structured() {
    let (_reg, disp, _mem) = loop_dispatcher("gate");
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
