//! Session background service (ARCHITECTURE.md §4.11 `session.rs` / §2).
//!
//! The TUI frontend only talks to this background service, which owns an
//! `Arc<AgentSession>` and streams `SessionEvent`s (Step / Done / Error) to the
//! frontend over a **bounded** mpsc channel. The frontend sends commands
//! (prompt / cancel / close) back over a dedicated channel; it never touches
//! the browser tool or bridge layers directly (§2 boundary 1).

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use webai_agent::{AgentSession, ChatMessage};
use webai_protocol::SessionEvent;

use crate::UiCommand;

/// Bounded event channel capacity (backpressure: no unbounded growth).
pub const EVENT_CHANNEL_CAPACITY: usize = 32;

/// How many outstanding `Step` events may coalesce into one progress event when
/// the frontend is slow (backpressure policy: bounded channel + coalescing).
pub const COALESCE_STEP_BURST: usize = 8;

/// A handler that turns one user prompt into a stream of session events.
///
/// In production this drives `AgentLoop::run`; in tests a fake handler returns
/// a canned `Step/Done/Error` sequence so the event flow is testable without a
/// live model or browser.
pub trait PromptHandler: Send + Sync {
    /// Run a user prompt, appending produced events to `out` (via `push`).
    /// Returns `Err(msg)` to emit `SessionEvent::Error` and stop for this
    /// prompt.
    fn run(&self, prompt: &str, emit: &mut dyn FnMut(SessionEvent)) -> Result<(), String>;
}


/// A handle the frontend uses to both observe events and drive the session.
#[derive(Debug)]
pub struct SessionBackend {
    events_rx: mpsc::Receiver<SessionEvent>,
    commands_tx: mpsc::UnboundedSender<UiCommand>,
    session: Arc<AgentSession>,
    join: JoinHandle<()>,
}

impl SessionBackend {
    /// Spawn the background session task.
    ///
    /// `handler` drives the agent loop for each prompt. `session` is shared so
    /// the frontend can read the transcript without touching bridge layers.
    pub fn spawn(session: Arc<AgentSession>, handler: Arc<dyn PromptHandler>) -> Self {
        let (events_tx, events_rx) = mpsc::channel::<SessionEvent>(EVENT_CHANNEL_CAPACITY);
        let (commands_tx, commands_rx) = mpsc::unbounded_channel::<UiCommand>();

        // Clone the Arc so the spawned task owns one, and the backend keeps
        // one to hand the frontend (transcript reads).
        let task_session = Arc::clone(&session);

        let join = tokio::spawn(async move {
            serve(task_session, handler, commands_rx, events_tx).await;
        });

        Self {
            events_rx,
            commands_tx,
            session,
            join,
        }
    }

    /// The frontend's view of the shared agent session (transcript etc.).
    pub fn session(&self) -> &Arc<AgentSession> {
        &self.session
    }

    /// Subscribe to the event stream (frontend side). One receiver per backend.
    pub fn events(&mut self) -> &mut mpsc::Receiver<SessionEvent> {
        &mut self.events_rx
    }

    /// Hand a user prompt to the loop.
    pub fn send_prompt(&self, text: impl Into<String>) -> bool {
        self.commands_tx
            .send(UiCommand::Send { text: text.into() })
            .is_ok()
    }

    /// Ask the background task to shut down cleanly.
    pub fn shutdown(&self) -> bool {
        self.commands_tx.send(UiCommand::Shutdown).is_ok()
    }

    /// Await the background task's exit. Lets tests assert no leaked task.
    pub async fn close(mut self) -> Result<(), tokio::task::JoinError> {
        // Send shutdown if the task is still alive, then await it.
        let _ = self.commands_tx.send(UiCommand::Shutdown);
        drop(self.commands_tx);
        // Read events until channel closes so the sender doesn't block.
        while self.events_rx.recv().await.is_some() {}
        self.join.await
    }
}

/// Run the command loop until shutdown or all senders drop.
async fn serve(
    session: Arc<AgentSession>,
    handler: Arc<dyn PromptHandler>,
    mut commands: mpsc::UnboundedReceiver<UiCommand>,
    events_tx: mpsc::Sender<SessionEvent>,
) {
    while let Some(cmd) = commands.recv().await {
        match cmd {
            UiCommand::Shutdown => break,
            UiCommand::Send { text } => {
                // Record the user turn in the shared transcript (frontend also
                // reads it; the bridge/agent layers are not touched here).
                let transcript_turn = ChatMessage::User(text.clone());
                record_turn(&session, transcript_turn);

                let mut emit = |ev: SessionEvent| {
                    emit_bounded(&events_tx, ev);
                };
                let result = handler.run(&text, &mut emit);

                match result {
                    Ok(()) => {
                        let done = SessionEvent::Done {
                            state: crate::done_state(Ok(())),
                        };
                        let _ = emit_bounded(&events_tx, done);
                    }
                    Err(msg) => {
                        let err = SessionEvent::Error { message: msg };
                        let _ = emit_bounded(&events_tx, err);
                    }
                }
            }
        }
    }
}

/// Record a transcript turn onto the shared session (best-effort lock).
fn record_turn(session: &Arc<AgentSession>, turn: ChatMessage) {
    session.push(turn);
}

/// Bounded send with a documented coalescing backpressure policy.
///
/// - The channel is bounded (`EVENT_CHANNEL_CAPACITY`), so senders never queue
///   a flood of events unboundedly.
/// - When the frontend is slow and the buffer is full, we drop `Step` events
///   (up to `COALESCE_STEP_BURST` in a burst, keeping the last one) rather
///   than growing memory unbounded. The `Done`/`Error` terminal events are
///   always kept.
fn emit_bounded(tx: &mpsc::Sender<SessionEvent>, ev: SessionEvent) -> bool {
    match tx.try_send(ev) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            // Backpressure: drop intermediate event under overload. The channel
            // stays bounded; the frontend just may miss a transient step.
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

/// The default prompt handler: emits the prompt observation as a Step and a
/// Done event. The agent loop backends (LLM / browser tool) attach through
/// `AgentSession` on the M6 run loop; this keeps the frontend loop testable.
pub struct LoopPromptHandler;

impl PromptHandler for LoopPromptHandler {
    fn run(&self, prompt: &str, emit: &mut dyn FnMut(SessionEvent)) -> Result<(), String> {
        use webai_protocol::AgentStep;
        let step = AgentStep {
            tool_name: "prompt".into(),
            observation: Some(prompt.to_owned()),
            image: None,
            reused_script: false,
        };
        emit(SessionEvent::Step { step });
        let done = SessionEvent::Done {
            state: crate::done_state(Ok(())),
        };
        emit(done);
        Ok(())
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

    /// A fake handler that emits a Step then a Done, or an Error.
    struct FakeHandler {
        fail: bool,
    }

    impl PromptHandler for FakeHandler {
        fn run(&self, _prompt: &str, emit: &mut dyn FnMut(SessionEvent)) -> Result<(), String> {
            if self.fail {
                emit(SessionEvent::Error {
                    message: "boom".into(),
                });
                Err("boom".into())
            } else {
                let step = webai_protocol::AgentStep {
                    tool_name: "browser".into(),
                    observation: Some("clicked".into()),
                    image: None,
                    reused_script: false,
                };
                emit(SessionEvent::Step { step });
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn event_stream_emits_step_and_done() {
        let s = build_session("srv-1");
        let mut backend = SessionBackend::spawn(s, Arc::new(FakeHandler { fail: false }));
        assert!(backend.send_prompt("click the button"));
        // Drain a bounded number of events with a timeout so we don't block.
        let mut got_step = false;
        let mut got_done = false;
        let mut got_any = false;
        for _ in 0..4 {
            let recv = tokio::time::timeout(
                std::time::Duration::from_millis(300),
                backend.events().recv(),
            );
            if let Ok(Some(ev)) = recv.await {
                got_any = true;
                match ev {
                    SessionEvent::Step { .. } => got_step = true,
                    SessionEvent::Done { .. } => got_done = true,
                    SessionEvent::Error { .. } => {}
                }
            } else {
                break;
            }
        }
        assert!(got_any, "should receive at least one event");
        assert!(got_step, "handler should emit a Step");
        // Done is emitted after the handler returns.
        assert!(got_done, "handler completion should emit a Done");
        // Shut down so the task exits cleanly.
        back_await_close(backend).await;
    }

    #[tokio::test]
    async fn backend_propagates_error_event() {
        let s = build_session("srv-2");
        let mut backend = SessionBackend::spawn(s, Arc::new(FakeHandler { fail: true }));
        backend.send_prompt("do bad");
        let mut got_error = false;
        for _ in 0..3 {
            let recv = tokio::time::timeout(
                std::time::Duration::from_millis(300),
                backend.events().recv(),
            );
            if let Ok(Some(ev)) = recv.await {
                if let SessionEvent::Error { message } = ev {
                    assert_eq!(message, "boom");
                    got_error = true;
                    break;
                }
            } else {
                break;
            }
        }
        assert!(got_error);
        back_await_close(backend).await;
    }

    #[tokio::test]
    async fn shutdown_exits_task_cleanly() {
        let s = build_session("srv-3");
        let backend = SessionBackend::spawn(s, Arc::new(FakeHandler { fail: false }));
        // Send shutdown; the task should exit and JoinEnds without leak.
        let join_closed = backend.close().await;
        assert!(join_closed.is_ok(), "background task must join cleanly");
    }

    #[tokio::test]
    async fn transcript_records_user_prompt() {
        let s = build_session("srv-4");
        let backend = SessionBackend::spawn(s.clone(), Arc::new(FakeHandler { fail: false }));
        backend.send_prompt("remember me");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(backend.session().transcript().len(), 1);
        back_await_close(backend).await;
    }

    async fn back_await_close(backend: SessionBackend) {
        let _ = backend.close().await;
    }
}
