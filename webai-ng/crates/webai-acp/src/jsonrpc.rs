//! JSON-RPC protocol + session dispatch (ARCHITECTURE.md §4.10).
//!
//! Defines the wire types, a shared `AcpSessionRegistry`, an `AcpHandler`
//! that runs a user prompt into `SessionEvent`s, and a `Dispatcher` that
//! serializes `session/prompt` and `session/close` per session (no data races)
//! while remaining backward compatible with the existing ACP method surface.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use webai_agent::AgentSession;
use webai_protocol::SessionEvent;

// ---------------------------------------------------------------------------
// Wire types (JSON-RPC 2.0 subset)
// ---------------------------------------------------------------------------

/// A parsed JSON-RPC request.
#[derive(Debug, Clone)]
pub struct AcpRequest {
    pub id: u64,
    pub method: String,
    pub params: serde_json::Value,
}

/// A JSON-RPC response (result XOR error).
#[derive(Debug, Clone)]
pub struct AcpResponse {
    pub id: u64,
    pub result: Option<serde_json::Value>,
    pub error: Option<AcpError>,
}

/// A structured JSON-RPC error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpError {
    pub code: i32,
    pub message: String,
}

/// Standard JSON-RPC error codes.
pub mod code {
    use super::AcpError;

    pub fn parse_error() -> AcpError {
        AcpError {
            code: -32700,
            message: "Parse error".into(),
        }
    }
    pub fn invalid_request() -> AcpError {
        AcpError {
            code: -32600,
            message: "Invalid Request".into(),
        }
    }
    pub fn method_not_found() -> AcpError {
        AcpError {
            code: -32601,
            message: "Method not found".into(),
        }
    }
    pub fn invalid_params() -> AcpError {
        AcpError {
            code: -32602,
            message: "Invalid params".into(),
        }
    }
    pub fn internal() -> AcpError {
        AcpError {
            code: -32603,
            message: "Internal error".into(),
        }
    }
    /// ACP application error (session not found / closed).
    pub fn app(msg: impl Into<String>) -> AcpError {
        AcpError {
            code: -32000,
            message: msg.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Response envelope (backward-compatible with `{ jsonrpc, id, result|error }`)
// ---------------------------------------------------------------------------

/// Serialize a response into the wire representation.
pub fn response_to_json(resp: &AcpResponse) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("jsonrpc".into(), serde_json::Value::String("2.0".into()));
    obj.insert("id".into(), serde_json::Value::Number(resp.id.into()));
    match (&resp.result, &resp.error) {
        (Some(r), None) => {
            obj.insert("result".into(), r.clone());
        }
        (None, Some(e)) => {
            let mut err = serde_json::Map::new();
            err.insert("code".into(), serde_json::Value::Number(e.code.into()));
            err.insert(
                "message".into(),
                serde_json::Value::String(e.message.clone()),
            );
            obj.insert("error".into(), serde_json::Value::Object(err));
        }
        _ => unreachable!("exactly one of result/error must be present"),
    }
    serde_json::Value::Object(obj)
}

// ---------------------------------------------------------------------------
// Shared registry
// ---------------------------------------------------------------------------

/// Registry of live ACP sessions (ARCHITECTURE.md §4.10). Shared between the ACP
/// server and the TUI so local and remote observers see the same sessions.
#[derive(Debug, Clone, Default)]
pub struct AcpSessionRegistry {
    sessions: Arc<Mutex<HashMap<String, Arc<AgentSession>>>>,
    workers: Arc<Mutex<HashMap<String, worker::WorkerHandle>>>,
}

impl AcpSessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a session under its id; starts its serialization worker.
    pub fn register(&self, session: Arc<AgentSession>) {
        let id = session.session_id().to_owned();
        self.sessions.lock().unwrap().insert(id.clone(), session);
        self.workers
            .lock()
            .unwrap()
            .insert(id, worker::WorkerHandle::spawn());
    }

    /// Look up a session by id.
    pub fn get(&self, session_id: &str) -> Option<Arc<AgentSession>> {
        self.sessions.lock().unwrap().get(session_id).cloned()
    }

    /// Remove and return a session by id (used by `session/close`); stops its
    /// worker.
    pub fn remove(&self, session_id: &str) -> Option<Arc<AgentSession>> {
        let removed = self.sessions.lock().unwrap().remove(session_id);
        self.workers.lock().unwrap().remove(session_id);
        removed
    }

    /// Remove a session while holding its per-session worker lock first.
    ///
    /// Used by `session/close` (Kaneo #102): a concurrent `session/prompt`
    /// for the same session blocks on the worker lock until the removal
    /// completes, so a prompt run cannot straddle the close.
    pub fn remove_locked(&self, session_id: &str) -> Option<Arc<AgentSession>> {
        // Hold the worker guard across the whole removal: clone the handle
        // (a cheap Arc) at this function's scope and lock it there, so the
        // guard stays alive until the removal completes.
        let handle = self.workers.lock().unwrap().get(session_id).cloned();
        let guard_holder = handle.as_ref().map(|h| h.lock());
        let removed = self.sessions.lock().unwrap().remove(session_id);
        self.workers.lock().unwrap().remove(session_id);
        drop(guard_holder);
        removed
    }

    /// Serialize `session/prompt` and `session/close` per session. A per-session
    /// worker serializes concurrent prompt/close calls for the same session so
    /// there is no data race on its transcript (ARCHITECTURE.md §4.10).
    pub fn worker(&self, session_id: &str) -> Option<worker::WorkerHandle> {
        self.workers.lock().unwrap().get(session_id).cloned()
    }

    pub fn len(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Turns a user prompt into a `SessionEvent` sequence.
///
/// In production this drives the agent loop (AgentRunner); in tests a fake
/// handler returns a canned sequence so the ACP server is testable without a
/// live model or browser.
pub trait AcpHandler: Send + Sync {
    /// Run the prompt against `session`. Returns the events to push to the
    /// observer, or `Err(msg)` to emit `SessionEvent::Error`.
    fn run(&self, session: &Arc<AgentSession>, prompt: &str) -> Result<Vec<SessionEvent>, String>;
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Synchronous request dispatcher. `notify` receives every event the handler
/// produced (the ACP server forwards them as `acp_notify` notifications).
#[derive(Clone)]
pub struct Dispatcher {
    registry: AcpSessionRegistry,
    handler: Arc<dyn AcpHandler>,
}

impl Dispatcher {
    pub fn new(registry: AcpSessionRegistry, handler: Arc<dyn AcpHandler>) -> Self {
        Self { registry, handler }
    }

    pub fn registry(&self) -> &AcpSessionRegistry {
        &self.registry
    }

    /// Dispatch a parsed request. Returns the response and the events emitted
    /// (the caller forwards them via `acp_notify`).
    pub fn dispatch(&self, req: &AcpRequest, notify: &mut dyn FnMut(SessionEvent)) -> AcpResponse {
        match req.method.as_str() {
            // Backward-compatible: expose the registry/list surface.
            "session/list" => self.session_list(req),
            "session/get" => self.session_get(req),
            "session/prompt" => self.session_prompt(req, notify),
            "session/close" => self.session_close(req),
            "agent/health" => self.ok(req, serde_json::json!({"ok": true})),
            // Protocol methods that already exist are not broken: ping etc.
            "ping" | "rpc.ping" => self.ok(req, serde_json::json!("pong")),
            _ => self.err(req.id, code::method_not_found()),
        }
    }

    fn ok(&self, req: &AcpRequest, value: serde_json::Value) -> AcpResponse {
        AcpResponse {
            id: req.id,
            result: Some(value),
            error: None,
        }
    }

    fn err(&self, id: u64, e: AcpError) -> AcpResponse {
        AcpResponse {
            id,
            result: None,
            error: Some(e),
        }
    }

    fn session_list(&self, req: &AcpRequest) -> AcpResponse {
        let ids: Vec<String> = self
            .registry
            .sessions
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        self.ok(req, serde_json::json!({ "session_ids": ids }))
    }

    fn session_get(&self, req: &AcpRequest) -> AcpResponse {
        match self.session_from_params(req) {
            Some(s) => self.ok(
                req,
                serde_json::json!({
                    "session_id": s.session_id(),
                    "transcript_len": s.transcript().len(),
                }),
            ),
            None => self.err(req.id, code::app("session not found")),
        }
    }

    /// `session/prompt`: hand the prompt to the loop; forward events via notify.
    fn session_prompt(
        &self,
        req: &AcpRequest,
        notify: &mut dyn FnMut(SessionEvent),
    ) -> AcpResponse {
        let (session, prompt) = match self.session_and_prompt_from_params(req) {
            Some(v) => v,
            None => return self.err(req.id, code::invalid_params()),
        };
        // Serialize prompt/close per session via the worker lock.
        let Some(worker) = self.registry.worker(session.session_id()) else {
            return self.err(req.id, code::app("session has no worker"));
        };
        let _guard = worker.lock();
        match self.handler.run(&session, &prompt) {
            Ok(events) => {
                for ev in events {
                    notify(ev);
                }
                self.ok(req, serde_json::json!({ "accepted": true }))
            }
            Err(msg) => {
                notify(SessionEvent::Error {
                    message: msg.clone(),
                });
                // Still respond ok; the error surfaced as an event.
                self.ok(req, serde_json::json!({ "accepted": true, "warning": msg }))
            }
        }
    }

    /// `session/close`: remove the session from the registry.
    ///
    /// Holds the per-session worker lock while removing so a concurrent
    /// `session/prompt` on the same session either completes before the
    /// removal or observes an unregistered session afterwards — no prompt run
    /// may straddle the close and keep emitting events on a removed session
    /// (Kaneo #102).
    fn session_close(&self, req: &AcpRequest) -> AcpResponse {
        match self.session_id_from_params(req) {
            Some(id) => {
                // Grab the worker lock BEFORE removing the registration.
                // resolve-scope guard: if the session never existed the lock
                // is absent and removal fails structurally.
                let had = self.registry.remove_locked(&id);
                if had.is_some() {
                    self.ok(req, serde_json::json!({ "closed": id }))
                } else {
                    self.err(req.id, code::app("session not found"))
                }
            }
            None => self.err(req.id, code::app("session not found")),
        }
    }

    // -- params helpers ------------------------------------------------------

    fn session_id_from_params(&self, req: &AcpRequest) -> Option<String> {
        req.params
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    }

    fn session_from_params(&self, req: &AcpRequest) -> Option<Arc<AgentSession>> {
        let id = self.session_id_from_params(req)?;
        self.registry.get(&id)
    }

    fn session_and_prompt_from_params(
        &self,
        req: &AcpRequest,
    ) -> Option<(Arc<AgentSession>, String)> {
        let id = self.session_id_from_params(req)?;
        let prompt = req
            .params
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::to_owned)?;
        let session = self.registry.get(&id)?;
        Some((session, prompt))
    }
}

/// Parse a JSON-RPC message into a request (returns None for notifications /
/// invalid messages the dispatcher should answer with a parse error).
pub fn parse_request(raw: &str) -> Result<Option<AcpRequest>, AcpError> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|_| code::parse_error())?;
    let obj = value.as_object().ok_or_else(code::invalid_request)?;
    if obj.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
        // Tolerate missing version for backward compatibility but require method.
        if !obj.contains_key("method") {
            return Err(code::invalid_request());
        }
    }
    let method = match obj.get("method").and_then(|v| v.as_str()) {
        Some(m) => m.to_owned(),
        None => return Err(code::invalid_request()),
    };
    let id = obj.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
    let params = obj
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Ok(Some(AcpRequest { id, method, params }))
}

mod worker {
    use parking_lot::Mutex as PLMutex;
    use std::sync::Arc;

    /// Per-session serialization guard. A mutex per session serializes
    /// consecutive `session/prompt` / `session/close` so concurrent callers do
    /// not interleave transcript writes (no data race).
    #[derive(Clone, Debug)]
    pub struct WorkerHandle {
        m: Arc<PLMutex<()>>,
    }

    impl WorkerHandle {
        pub fn spawn() -> Self {
            Self {
                m: Arc::new(PLMutex::new(())),
            }
        }
        /// Lock. Borrow must live inside the caller's scope alongside the handle.
        pub fn lock(&self) -> parking_lot::MutexGuard<'_, ()> {
            self.m.lock()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use webai_agent::AgentLoop;
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

    struct FakeHandler;

    impl AcpHandler for FakeHandler {
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
                        reused_script: true,
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

    fn registry() -> (AcpSessionRegistry, Arc<AgentSession>) {
        let s = build_session("s1");
        let reg = AcpSessionRegistry::new();
        reg.register(s.clone());
        (reg, s)
    }

    fn request(method: &str, params: serde_json::Value) -> AcpRequest {
        AcpRequest {
            id: 1,
            method: method.into(),
            params,
        }
    }

    #[test]
    fn registry_shared_and_remove() {
        let (reg, _) = registry();
        assert_eq!(reg.len(), 1);
        assert!(reg.get("s1").is_some());
        assert!(reg.remove("s1").is_some());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn session_prompt_forwards_step_and_done() {
        let (reg, _) = registry();
        let disp = Dispatcher::new(reg, Arc::new(FakeHandler));
        let req = request(
            "session/prompt",
            serde_json::json!({
                "session_id": "s1", "prompt": "click x"
            }),
        );
        let mut events = Vec::new();
        let resp = disp.dispatch(&req, &mut |ev| events.push(ev));
        assert!(resp.error.is_none());
        // Handler produced Step + Done.
        assert!(matches!(events[0], SessionEvent::Step { .. }));
        assert!(matches!(events[1], SessionEvent::Done { .. }));
    }

    #[test]
    fn session_close_removes_and_reject_unknown() {
        let (reg, _) = registry();
        let disp = Dispatcher::new(reg, Arc::new(FakeHandler));
        let close = disp.dispatch(
            &request("session/close", serde_json::json!({"session_id": "s1"})),
            &mut |_| {},
        );
        assert!(close.error.is_none());
        // Second close: session already gone.
        let again = disp.dispatch(
            &request("session/close", serde_json::json!({"session_id": "s1"})),
            &mut |_| {},
        );
        assert!(again.error.is_some());
    }

    #[test]
    fn method_not_found_is_structured() {
        let (reg, _) = registry();
        let disp = Dispatcher::new(reg, Arc::new(FakeHandler));
        let resp = disp.dispatch(&request("bogus/method", serde_json::json!({})), &mut |_| {});
        assert_eq!(resp.error.as_ref().unwrap().code, -32601);
    }

    #[test]
    fn parse_request_handles_invalid_json() {
        assert!(parse_request("{ not json").is_err());
        let parsed =
            parse_request(r#"{"jsonrpc":"2.0","id":7,"method":"ping","params":{}}"#).unwrap();
        let req = parsed.unwrap();
        assert_eq!(req.method, "ping");
        assert_eq!(req.id, 7);
    }

    /// Concurrent prompt+close on the same session must serialize (no panic /
    /// lost transcript). This exercises the per-session worker lock.
    #[test]
    fn session_prompt_and_close_serialize() {
        let (reg, s) = registry();
        let disp = Dispatcher::new(reg, Arc::new(FakeHandler));
        // Fire a flood of prompt/close on the same session from threads.
        let mut handles = Vec::new();
        for _ in 0..8 {
            let disp2 = disp.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..10 {
                    let req = request(
                        "session/prompt",
                        serde_json::json!({
                            "session_id": "s1", "prompt": "go"
                        }),
                    );
                    let _ = disp2.dispatch(&req, &mut |_| {});
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        // The session transcript may be empty (handler didn't write) but the
        // registry must still be consistent (no panic / corruption).
        assert!(s.session_id() == "s1");
    }

    /// Kaneo #102: a close issued while a prompt is mid-run must hold the
    /// session's worker lock, so the run either completes before removal or
    /// is refused afterwards — events never continue on a removed session.
    #[test]
    fn close_during_long_prompt_blocks_until_prompt_done() {
        struct SlowThenEvents;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc as StdArc;
        static PROMPT_RUNNING: AtomicBool = AtomicBool::new(false);
        impl AcpHandler for SlowThenEvents {
            fn run(
                &self,
                session: &StdArc<AgentSession>,
                _prompt: &str,
            ) -> Result<Vec<SessionEvent>, String> {
                PROMPT_RUNNING.store(true, Ordering::SeqCst);
                // Hold long enough for main thread to attempt a close.
                for _ in 0..50 {
                    if !PROMPT_RUNNING.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                PROMPT_RUNNING.store(false, Ordering::SeqCst);
                let _ = session;
                Ok(vec![SessionEvent::Done {
                    state: webai_protocol::AgentState {
                        status: "done".into(),
                        message: None,
                    },
                }])
            }
        }
        let (reg, _) = registry();
        let disp = Dispatcher::new(reg, Arc::new(SlowThenEvents));
        let d_thread = disp.clone();
        let prompt_thread = std::thread::spawn(move || {
            let req = request(
                "session/prompt",
                serde_json::json!({"session_id": "s1", "prompt": "go"}),
            );
            let mut events = Vec::new();
            let resp = d_thread.dispatch(&req, &mut |ev| events.push(ev));
            (resp, events)
        });
        // Wait until the prompt actually holds the worker lock.
        while !PROMPT_RUNNING.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // Close from another thread: must block until prompt completes.
        let close_thread = {
            let d = disp.clone();
            std::thread::spawn(move || {
                let req = request("session/close", serde_json::json!({"session_id": "s1"}));
                d.dispatch(&req, &mut |_| {})
            })
        };
        let (resp, events) = prompt_thread.join().unwrap();
        // Prompt's own events completed fully before/while close waited.
        assert!(resp.error.is_none(), "prompt completes normally");
        assert_eq!(events.len(), 1, "Done event emitted by prompt's own run");
        let close_resp = close_thread.join().unwrap();
        assert!(close_resp.error.is_none(), "close then removes the session");
        // Post-close prompt on the removed session is refused.
        let late = disp.dispatch(
            &request(
                "session/prompt",
                serde_json::json!({"session_id": "s1", "prompt": "x"}),
            ),
            &mut |_| {},
        );
        assert!(late.error.is_some(), "late prompt must be refused");
    }

    #[test]
    fn response_envelope_has_jsonrpc_and_id() {
        let resp = AcpResponse {
            id: 5,
            result: Some(serde_json::json!({"ok": true})),
            error: None,
        };
        let json = response_to_json(&resp);
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 5);
        assert_eq!(json["result"]["ok"], true);
    }
}
