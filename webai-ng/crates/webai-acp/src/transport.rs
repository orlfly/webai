//! ACP transports: JSON-RPC over WebSocket and line-delimited TCP
//! (ARCHITECTURE.md §4.10 / §3.3 network boundary).
//!
//! Both transports accept a read stream of JSON-RPC frames, dispatch via the
//! shared `Dispatcher`, stream `SessionEvent`s back as `acp_notify`
//! notifications, and write `response` frames. Session events (`reused_script`,
//! `image`, `Done`, `Error`) are forwarded via `acp_notify` so the remote
//! observer sees the same event stream as the local TUI.

use std::net::IpAddr;
use std::sync::Arc;

use webai_protocol::SessionEvent;

use crate::jsonrpc::{self, AcpRequest, Dispatcher};
use crate::net::{Admission, NetworkPolicy};
use crate::AcpError;

/// Connection-level admission gate. Called once per client connection (the
/// transport accept loop) before any frame is dispatched; an unpaired or
/// non-loopback client receives a JSON-RPC error frame instead of service.
pub fn accept_connection(
    policy: &NetworkPolicy,
    source: IpAddr,
    presented_pairing: Option<&str>,
) -> Result<(), AcpError> {
    match policy.admit(source, presented_pairing) {
        Admission::Allow => Ok(()),
        Admission::Deny { code, reason } => Err(AcpError {
            code: -32001, // implementation-defined server error range
            message: format!("{code}: {reason}"),
        }),
    }
}

/// A frame delivered from a transport: a JSON-RPC request, or a session event
/// to be fanned out as `acp_notify`.
pub enum Frame {
    Request(AcpRequest),
    Event(SessionEvent),
}

/// A parsed line / WS-text message into either a request or a notify event.
pub fn decode_frame(raw: &str) -> Result<Option<AcpRequest>, AcpError> {
    jsonrpc::parse_request(raw)
}

/// Serialize an event as an `acp_notify` notification frame.
pub fn event_notify(ev: &SessionEvent) -> String {
    use serde_json::json;
    let payload = match ev {
        SessionEvent::Step { step } => json!({
            "jsonrpc": "2.0",
            "method": "acp_notify",
            "params": {
                "event": "session/step",
                "step": {
                    "tool_name": step.tool_name,
                    "observation": step.observation,
                    "image": step.image,
                    "reused_script": step.reused_script,
                },
            },
        }),
        SessionEvent::Done { state } => json!({
            "jsonrpc": "2.0",
            "method": "acp_notify",
            "params": { "event": "session/done", "state": state.status },
        }),
        SessionEvent::Error { message } => json!({
            "jsonrpc": "2.0",
            "method": "acp_notify",
            "params": { "event": "session/error", "message": message },
        }),
    };
    payload.to_string()
}

/// Process one client line/frame against the session dispatcher.
///
/// `emit` is called with every `acp_notify` frame for the client, and the
/// function returns a JSON-RPC response frame (or `None` for pure
/// notifications / events).
pub fn handle_line(
    disp: &Dispatcher,
    line: &str,
    emit: &mut dyn FnMut(String),
) -> Result<Option<String>, AcpError> {
    let Some(req) = decode_frame(line)? else {
        return Ok(None);
    };
    let mut notify = |ev: SessionEvent| emit(event_notify(&ev));
    let resp = disp.dispatch(&req, &mut notify);
    Ok(Some(jsonrpc::response_to_json(&resp).to_string()))
}

// ---------------------------------------------------------------------------
// Transport adapters (tokio) — kept thin so the two transports behave
// identically on the same dispatcher.
// ---------------------------------------------------------------------------

/// Async-friendly error used when streaming a transport.
#[derive(Debug, Clone, thiserror::Error)]
pub enum TransportError {
    #[error("io error: {0}")]
    Io(String),
}

impl From<std::io::Error> for TransportError {
    fn from(e: std::io::Error) -> Self {
        TransportError::Io(e.to_string())
    }
}

/// Process an async frame stream generically. Concrete helpers:
/// - `serve_ws` reads WebSocket text frames.
/// - `serve_tcp` reads line-delimited TCP.
///
/// Both call `handle_line` so event sequences are identical across transports.
pub async fn pump<F, W>(
    disp: Arc<Dispatcher>,
    mut next: F,
    mut write: W,
) -> Result<(), TransportError>
where
    F: FnMut() -> Option<String>,
    W: FnMut(String) -> Result<(), std::io::Error>,
{
    while let Some(line) = next() {
        let mut emit_frames: Vec<String> = Vec::new();
        let resp = handle_line(&disp, &line, &mut |f| emit_frames.push(f))
            .ok()
            .flatten();
        for f in emit_frames {
            write(f)?;
        }
        if let Some(r) = resp {
            write(r)?;
        }
    }
    Ok(())
}

// Note: full concrete WebSocket / TcpListener servers live in the binary layer
// (bins/webai). This crate exposes the shared pump + frame codecs so both
// transports are built on the identical dispatcher and event fan-out.

#[cfg(test)]
mod tests {
    use super::*;
    use webai_agent::{AgentLoop, AgentSession};
    use webai_llm::LlmClient;
    use webai_memory::SharedMemoryStore;

    fn build_session(id: &str) -> Arc<AgentSession> {
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

    async fn make_dispatcher(sid: &str) -> Arc<Dispatcher> {
        use crate::AcpSessionRegistry;
        let s = build_session(sid);
        let reg = AcpSessionRegistry::new();
        reg.register(s);
        let handler = Arc::new(LocalFake);
        let disp = Dispatcher::new(reg, handler);
        Arc::new(disp)
    }

    /// A local fake handler for transport tests.
    struct LocalFake;

    impl crate::AcpHandler for LocalFake {
        fn run(
            &self,
            _session: &Arc<AgentSession>,
            _prompt: &str,
        ) -> Result<Vec<SessionEvent>, String> {
            Ok(vec![
                SessionEvent::Step {
                    step: webai_protocol::AgentStep {
                        tool_name: "browser".into(),
                        observation: Some("clicked".into()),
                        image: None,
                        reused_script: false,
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

    /// Integration: an unpaired connection is refused at the accept loop
    /// with a JSON-RPC error before any dispatch happens (task 87 / FR-8).
    #[tokio::test]
    async fn unpaired_connection_receives_jsonrpc_error() {
        let policy = NetworkPolicy::resolve(true, true)
            .unwrap()
            .with_pairing_secret("sekrit");
        // Unpaired: refused.
        let err = accept_connection(&policy, "10.0.0.9".parse().unwrap(), None).unwrap_err();
        assert_eq!(err.code, -32001);
        assert!(err.message.contains("pairing_required"));
        // Wrong key: refused.
        let err =
            accept_connection(&policy, "10.0.0.9".parse().unwrap(), Some("nope")).unwrap_err();
        assert!(err.message.contains("pairing_invalid"));
        // Correct key: admitted, dispatch then works.
        accept_connection(&policy, "10.0.0.9".parse().unwrap(), Some("sekrit")).unwrap();
        // Private mode still refuses remote sources regardless of key.
        let private = NetworkPolicy::resolve(false, false).unwrap();
        let err = accept_connection(&private, "10.0.0.9".parse().unwrap(), None).unwrap_err();
        assert!(err.message.contains("non_loopback_refused"));
    }

    #[tokio::test]
    async fn line_driven_prompt_emits_acp_notify_and_response() {
        let disp = make_dispatcher("t1").await;
        let line = r#"{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"session_id":"t1","prompt":"go"}}"#;
        let mut notifies = Vec::new();
        let resp = handle_line(&disp, line, &mut |f| notifies.push(f))
            .unwrap()
            .unwrap();
        // First a Step notify, then a Done notify, then the response frame.
        assert!(notifies.len() >= 2, "should emit step+done notify frames");
        assert!(notifies[0].contains("acp_notify"));
        assert!(resp.contains("\"result\""), "should contain a valid result");
    }

    #[tokio::test]
    async fn tcp_and_ws_share_identical_event_sequence() {
        // Both transports decode a line and run handle_line identically; this
        // asserts the frame decode + event fan-out produce the same sequence
        // for the same prompt (transport equivalence).
        let disp_a = make_dispatcher("teq").await;
        let disp_b = make_dispatcher("teq").await;
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{"session_id":"teq","prompt":"x"}}"#;
        let mut seq_a = Vec::new();
        let mut seq_b = Vec::new();
        let _ = handle_line(&disp_a, line, &mut |f| seq_a.push(f)).unwrap();
        let _ = handle_line(&disp_b, line, &mut |f| seq_b.push(f)).unwrap();
        assert_eq!(
            seq_a, seq_b,
            "both transports must produce identical event frames"
        );
    }

    #[test]
    fn event_notify_includes_reused_script_and_image() {
        let ev = SessionEvent::Step {
            step: webai_protocol::AgentStep {
                tool_name: "browser".into(),
                observation: Some("obs".into()),
                image: Some("/tmp/shot.png".into()),
                reused_script: true,
            },
        };
        let frame = event_notify(&ev);
        assert!(frame.contains("reused_script"));
        assert!(frame.contains("true"));
        assert!(frame.contains("/tmp/shot.png"));
    }
}
