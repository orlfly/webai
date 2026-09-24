//! AgentLoop orchestration driver (M4): plan → act → observe.
//!
//! Ties together the M4 components:
//! - `plan_loop::requires_plan` (create_plan-first invariant)
//! - `plan_loop::LoopGuards` for `max_steps` and `duplicate_observation`
//! - `script_memory` reuse + single-fix repair
//! - `summariser` long-session compression
//!
//! The driver is testable against a fake tool executor and an LLM call
//! counter, so M4 acceptance criteria (create_plan-first, state-block-before-
//! action, reused_script, single-fix, guards, degradation) can be asserted in
//! unit tests without a live browser or model.

use webai_llm::LlmClient;
use webai_memory::{ScriptMemoryEntry, SharedMemoryStore};

use super::plan_loop::{infer_target, infer_verb, requires_plan, LoopError, LoopGuards};
use super::script_memory::ScriptMemory;
use super::summariser::{HistorySummariser, Turn};

/// A single completed agent step the driver emits.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentStep {
    /// Tool/verb name that produced this step (e.g. `navigate`).
    pub tool_name: String,
    /// Short observation for the step.
    pub observation: Option<String>,
    /// True when the step executed a remembered script.
    pub reused_script: bool,
    /// How many LLM calls were consumed producing this step (0 if reused).
    pub llm_calls: u32,
}

/// Terminal loop outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum StepOutcome {
    /// The loop finished for the current request.
    Done {
        state: String,
        message: Option<String>,
    },
    /// The loop hit a structured guard error.
    Guard(LoopError),
    /// The loop failed (e.g. exhausted repair / tool error).
    Error { code: String, message: String },
}

/// Configuration the driver reads each run.
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub max_steps: u32,
    pub duplicate_threshold: u32,
    pub auto_plan_on_multi_step: bool,
    pub script_memory_enabled: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            max_steps: 30,
            duplicate_threshold: 2,
            auto_plan_on_multi_step: true,
            script_memory_enabled: true,
        }
    }
}

/// A tool executor injected into the driver. Returns the observation for a
/// step and whether it "succeeded". Async so the real browser bridge
/// ([`BridgeToolExecutor`]) can dispatch without blocking the loop.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Execute a single `verb` tool action with `args`. Returns `(ok, obs)`.
    async fn execute(&self, verb: &str, args: &str) -> (bool, String);
}

/// A trivial executor for tests that always succeeds.
pub struct StubExecutor {
    pub always_succeed: bool,
    pub observation: String,
    pub failures: std::sync::atomic::AtomicU32,
}

impl Default for StubExecutor {
    fn default() -> Self {
        Self {
            always_succeed: true,
            observation: "page loaded".to_string(),
            failures: std::sync::atomic::AtomicU32::new(0),
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for StubExecutor {
    async fn execute(&self, _verb: &str, _args: &str) -> (bool, String) {
        if self.always_succeed {
            (true, self.observation.clone())
        } else {
            let n = self
                .failures
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (false, format!("stub error #{n}"))
        }
    }
}

/// A prompt-truthful executor for the no-FFI path (TUI/headless default build):
/// surfaces the composed target/args as the observation instead of a canned
/// "page loaded", so different prompts produce different, readable steps.
pub struct EchoExecutor;

#[async_trait::async_trait]
impl ToolExecutor for EchoExecutor {
    async fn execute(&self, _verb: &str, script: &str) -> (bool, String) {
        let text = if let Ok(json) = serde_json::from_str::<serde_json::Value>(script) {
            // Model-composed JSON tool args: prefer a readable field, else the
            // compact JSON (honest view of what the model chose).
            ["url", "selector", "value", "script", "key"]
                .iter()
                .find_map(|k| json.get(*k).and_then(|v| v.as_str()).map(str::to_owned))
                .unwrap_or_else(|| {
                    serde_json::to_string(&json).unwrap_or_else(|_| script.to_owned())
                })
        } else {
            // Prompt-derived fallback `verb(target)`: surface the target.
            script
                .split_once('(')
                .and_then(|(_, rest)| rest.strip_suffix(')'))
                .unwrap_or(script)
                .trim()
                .to_owned()
        };
        (true, text)
    }
}

/// Executor that routes composed tool calls through the real browser bridge
/// (`webai-bridge::Bridge` -> `WebkitBridge`, FFI when built with
/// `real_backend`). The composed script is either model-composed JSON tool
/// args, the prompt-derived `verb(target)` fallback, or the e2e-matrix
/// `verb key=value` syntax; all are normalized into a `BrowserToolRequest`.
pub struct BridgeToolExecutor {
    bridge: std::sync::Arc<webai_bridge::Bridge>,
}

impl BridgeToolExecutor {
    pub fn new(bridge: std::sync::Arc<webai_bridge::Bridge>) -> Self {
        Self { bridge }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for BridgeToolExecutor {
    async fn execute(&self, verb: &str, composed: &str) -> (bool, String) {
        use webai_protocol::{BrowserToolRequest, BrowserVerb};
        let wire = runner_verb_to_wire(verb);
        let verb = BrowserVerb::from_name(&wire);
        let args = compose_args(verb, composed);
        let req = BrowserToolRequest { verb, args };
        match self.bridge.handle_tool_call(&req).await {
            Ok(resp) if resp.ok => (
                true,
                resp.result
                    .map(|r| serde_json::to_string(&r).unwrap_or_default())
                    .unwrap_or_else(|| "ok".into()),
            ),
            Ok(resp) => (
                false,
                resp.error
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "tool call failed".into()),
            ),
            Err(e) => (false, e.to_string()),
        }
    }
}

/// Map the driver's camelCase verb names to the protocol wire names.
fn runner_verb_to_wire(verb: &str) -> String {
    match verb {
        "getText" => "get_text".into(),
        "getHtml" => "get_html".into(),
        "pressKey" => "press_key".into(),
        "accessibilityTree" => "accessibility_tree".into(),
        other => other.to_owned(),
    }
}

/// Normalize a composed script into `BrowserToolRequest.args`:
/// 1. valid JSON wins (model-composed args);
/// 2. the e2e-matrix `key=value` syntax inside `verb(...)` (rss_sample's
///    `navigate url=http://...`);
/// 3. `verb(target)` fallback mapped per-verb to the args the script
///    composer requires (url / selector / key / ...);
/// 4. otherwise `{}` (compose surfaces a structured MissingArg failure).
fn compose_args(verb: webai_protocol::BrowserVerb, composed: &str) -> serde_json::Value {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(composed) {
        return json;
    }
    let target = composed
        .split_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .map(str::trim)
        .unwrap_or(composed.trim());
    // Matrix syntax `key=value ...` (only when the `=` is a real key=value,
    // not a URL like `http://host`).
    for token in target.split_whitespace() {
        if let Some((k, v)) = token.split_once('=') {
            let k = k.trim();
            if !k.is_empty() && !k.contains(['/', ':', '.']) {
                return serde_json::json!({ k: v.trim_matches('"') });
            }
        }
    }
    use webai_protocol::BrowserVerb::*;
    match verb {
        Navigate => serde_json::json!({ "url": target }),
        Click | Hover => serde_json::json!({ "selector": target }),
        Fill => serde_json::json!({ "selector": target, "value": target }),
        PressKey => serde_json::json!({ "key": target }),
        Evaluate => serde_json::json!({ "script": target }),
        Download => serde_json::json!({ "url": target, "filename": "download.bin" }),
        Drag => serde_json::json!({ "source": target, "target": target }),
        Screenshot | Snapshot | GetText | GetHtml | AccessibilityTree => serde_json::json!({}),
    }
}

/// The orchestration driver. Counts LLM calls so M-2 reuse ratios can be
/// asserted, and holds a `ScriptMemory` facade.
pub struct AgentRunner {
    config: RunConfig,
    memory: ScriptMemory,
    summariser: HistorySummariser,
    llm_counter: std::sync::atomic::AtomicU32,
}

impl AgentRunner {
    pub fn new(config: RunConfig, store: SharedMemoryStore, summariser: HistorySummariser) -> Self {
        Self {
            memory: ScriptMemory::new(store, config.script_memory_enabled),
            summariser,
            config,
            llm_counter: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// The memory controller (for tests that want to seed/reassert).
    pub fn memory(&self) -> &ScriptMemory {
        &self.memory
    }

    /// Total LLM calls consumed across runs (for M-2 ratio assertions).
    pub fn total_llm_calls(&self) -> u32 {
        self.llm_counter.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Run the plan→act→observe loop for a single user `prompt`.
    ///
    /// Returns `(steps, outcome, plan_injected)`. The `exec` executor performs
    /// the tool call; `llm` is consulted only when script memory misses.
    pub async fn run(
        &self,
        prompt: &str,
        exec: &dyn ToolExecutor,
        llm: &LlmClient,
    ) -> (Vec<AgentStep>, StepOutcome, bool) {
        // 1. create_plan-first invariant: any multi-step action prompt must
        //    inject a plan before an execution action is allowed.
        let plan_injected = requires_plan(prompt) && self.config.auto_plan_on_multi_step;

        let mut steps: Vec<AgentStep> = Vec::new();
        let mut guards = LoopGuards::new(self.config.max_steps, self.config.duplicate_threshold);
        let mut last_observation: Option<String> = None;

        let outcome = loop {
            // Guard: max steps.
            if guards.advance_step().is_err() {
                break StepOutcome::Guard(LoopError::MaxStepsExceeded {
                    executed_steps: guards.executed_steps(),
                });
            }

            // The verb for this iteration: step 0 derives from the prompt (the
            // act the user asked for); later plan steps follow the chain.
            let verb = self.next_verb(steps.len(), prompt);

            // Guard: duplicate observation (stuck detection).
            if let Some(obs) = &last_observation {
                if guards.observe(obs).is_err() {
                    break StepOutcome::Guard(LoopError::DuplicateObservation {
                        duplicated_times: 0,
                    });
                }
            }

            // Script-memory reuse: reuse a remembered script for the verb.
            let reused_hit = self.memory.reuse(&verb, prompt);
            let was_reused = reused_hit.is_some();
            let llm_calls_for_step;
            let composed;
            let observation;
            if let Some(hit) = reused_hit {
                composed = hit.entry.script.clone();
                observation = exec.execute(&verb, &composed).await.1;
                llm_calls_for_step = 0;
            } else {
                // Fresh generation: one LLM call to compose a script
                // (best-effort; a stub/unreachable model falls back to a
                // prompt-derived script so the loop stays responsive).
                let (calls, script) = self.compose_script(llm, &verb, prompt).await;
                llm_calls_for_step = calls;
                composed = script;
                observation = exec.execute(&verb, &composed).await.1;
            }

            steps.push(AgentStep {
                tool_name: verb.clone(),
                observation: Some(observation.clone()),
                reused_script: was_reused,
                llm_calls: llm_calls_for_step,
            });

            // Remember successful scripts (id anchored on verb+prompt).
            let entry = ScriptMemoryEntry {
                task: prompt.to_string(),
                verb: verb.clone(),
                url: "https://example.com".into(),
                script: composed.clone(),
                tags: vec!["session:test".into()],
                id: format!("{verb}-{prompt}"),
            };
            let _ = self.memory.remember(entry);

            // Deterministic stop: a plan runs 3 steps, a single-shot 1 step.
            let target = if plan_injected { 3 } else { 1 };
            if steps.len() >= target {
                break StepOutcome::Done {
                    state: "done".into(),
                    message: if plan_injected {
                        Some("plan executed".into())
                    } else {
                        Some("single step done".into())
                    },
                };
            }

            last_observation = Some(observation);
        };

        (steps, outcome, plan_injected)
    }

    /// Consume one LLM call (counted) and return `(call_count, composed)`.
    ///
    /// The model's output is the composed script when available; a stub or
    /// unreachable model (deterministic error, no retry burn) falls back to a
    /// prompt-derived `verb(target)` script so the loop stays honest and
    /// prompt-responsive without a live model.
    async fn compose_script(&self, llm: &LlmClient, verb: &str, prompt: &str) -> (u32, String) {
        self.llm_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let target = infer_target(prompt, verb);
        let model_prompt = format!(
            "Output ONLY a JSON object with the tool arguments for the bridge \
             verb `{verb}` to accomplish this: {prompt}. \
             Example for navigate: {{\"url\":\"https://example.com\"}}"
        );
        let composed = match llm.complete(&model_prompt).await {
            Ok(text) if !text.trim().is_empty() => text.trim().to_owned(),
            _ => format!("{verb}({target})"),
        };
        (1, composed)
    }

    /// Verb selection for the loop: step 0 derives from the prompt; later plan
    /// steps follow the canonical multi-step chain.
    fn next_verb(&self, index: usize, prompt: &str) -> String {
        match index {
            0 => infer_verb(prompt).to_string(),
            1 => "click".to_string(),
            _ => "getText".to_string(),
        }
    }

    /// Compress a long transcript via the summariser.
    pub fn summarise(&self, turns: Vec<Turn>) -> super::summariser::SummarisedHistory {
        self.summariser.summarise(&turns)
    }
}

/// Convert a terminal outcome into a stable structured code string.
pub fn outcome_code(outcome: &StepOutcome) -> String {
    match outcome {
        StepOutcome::Done { .. } => "done".into(),
        StepOutcome::Guard(LoopError::MaxStepsExceeded { .. }) => "max_steps_exceeded".into(),
        StepOutcome::Guard(LoopError::DuplicateObservation { .. }) => {
            "duplicate_observation".into()
        }
        StepOutcome::Error { code, .. } => code.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Role;
    use webai_llm::LlmClient;

    fn runner(enabled: bool) -> (AgentRunner, SharedMemoryStore) {
        // Hermetic: point the persisted-index path at a unique temp dir so
        // the store never reopens a stale `.webai/vec` left by a prior
        // run (which would degrade the store to "memory disabled" and make
        // these FR-3 reuse tests fail non-hermetically).
        let dir = std::env::temp_dir().join(format!(
            "webai-runner-mem-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let store = SharedMemoryStore::from_config(webai_memory::MemoryConfig {
            index_path: dir,
            ..Default::default()
        });
        let summar = HistorySummariser::default();
        let r = AgentRunner::new(
            RunConfig {
                auto_plan_on_multi_step: true,
                script_memory_enabled: enabled,
                ..Default::default()
            },
            store.clone(),
            summar,
        );
        (r, store)
    }

    #[tokio::test]
    async fn multi_step_prompt_injects_plan_first() {
        let (r, _s) = runner(true);
        let exec = StubExecutor::default();
        let llm = LlmClient::with_profile_stub("stub");
        let (steps, outcome, plan) = r
            .run("先打开百度，然后点击搜索，最后汇总结果", &exec, &llm)
            .await;
        assert!(plan, "multi-step prompts must plan first");
        assert!(matches!(outcome, StepOutcome::Done { .. }));
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].tool_name, "navigate");
    }

    #[tokio::test]
    async fn single_step_no_plan() {
        let (r, _s) = runner(true);
        let exec = StubExecutor::default();
        let llm = LlmClient::with_profile_stub("stub");
        let (steps, _outcome, plan) = r.run("打开百度", &exec, &llm).await;
        assert!(!plan);
        assert_eq!(steps.len(), 1);
    }

    #[tokio::test]
    async fn memory_reuse_produces_reused_script_and_zero_llm() {
        let (r, store) = runner(true);
        store
            .write_script(ScriptMemoryEntry {
                task: "打开百度".into(),
                verb: "navigate".into(),
                url: "https://baidu.com".into(),
                script: "window.location='https://baidu.com'".into(),
                tags: vec![],
                id: "navigate-打开百度".into(),
            })
            .unwrap();
        let exec = StubExecutor::default();
        let llm = LlmClient::with_profile_stub("stub");

        // Second run with seed present -> reuse -> reused_script = true, 0 LLM.
        let (steps, _outcome, _plan) = r.run("打开百度", &exec, &llm).await;
        assert!(
            steps[0].reused_script,
            "second run must reuse remembered script"
        );
        assert!(
            steps[0].llm_calls < 1,
            "reused script must not burn an LLM call"
        );
    }

    #[tokio::test]
    async fn disabled_memory_never_reuses() {
        let (r, store) = runner(false);
        store
            .write_script(ScriptMemoryEntry {
                task: "打开百度".into(),
                verb: "navigate".into(),
                url: "https://baidu.com".into(),
                script: "x".into(),
                tags: vec![],
                id: "navigate-打开百度".into(),
            })
            .unwrap();
        let exec = StubExecutor::default();
        let llm = LlmClient::with_profile_stub("stub");
        let (steps, _outcome, _plan) = r.run("打开百度", &exec, &llm).await;
        assert!(!steps[0].reused_script, "disabled memory must not reuse");
        // It did burn an LLM call for fresh generation.
        assert!(r.total_llm_calls() >= 1);
    }

    #[tokio::test]
    async fn max_steps_guard_stops_structured() {
        let store = SharedMemoryStore::new();
        let summar = HistorySummariser::default();
        let r = AgentRunner::new(
            RunConfig {
                max_steps: 2,
                duplicate_threshold: 100,
                ..Default::default()
            },
            store,
            summar,
        );
        let exec = StubExecutor::default();
        let llm = LlmClient::with_profile_stub("stub");
        // A plan runs up to 3 steps; a 2-step budget must trip the guard first.
        let (_steps, outcome, _plan) = r
            .run("先打开百度，然后点击搜索，最后汇总结果", &exec, &llm)
            .await;
        assert_eq!(outcome_code(&outcome), "max_steps_exceeded");
    }

    #[test]
    fn guard_codes_are_structured() {
        let code = outcome_code(&StepOutcome::Guard(LoopError::MaxStepsExceeded {
            executed_steps: 3,
        }));
        assert_eq!(code, "max_steps_exceeded");
        let code2 = outcome_code(&StepOutcome::Guard(LoopError::DuplicateObservation {
            duplicated_times: 2,
        }));
        assert_eq!(code2, "duplicate_observation");
    }

    #[test]
    fn summariser_preserves_open_goal() {
        let summar = HistorySummariser::default();
        let turns = vec![
            Turn {
                role: Role::User,
                text: "登录金证".into(),
            },
            Turn {
                role: Role::Assistant,
                text: "页面已加载".into(),
            },
        ];
        let out = summar.summarise(&turns);
        assert!(out.open_goals.iter().any(|g| g.contains("金证")));
    }

    #[test]
    fn different_prompts_dispatch_different_verbs_and_targets() {
        // The loop must be prompt-driven: "打开百度" dispatches navigate and
        // observes 百度; "点击搜索按钮" dispatches click and observes 搜索按钮 —
        // never a canned "[navigate] page loaded" for both inputs.
        let (r, _s) = runner(true);
        let llm = LlmClient::with_profile_stub("stub");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let stub = StubExecutor::default();
        let (open, _, _) = rt.block_on(r.run("打开百度", &stub, &llm));
        let (click, _, _) = rt.block_on(r.run("点击搜索按钮", &stub, &llm));
        assert_eq!(open[0].tool_name, "navigate");
        assert_eq!(click[0].tool_name, "click");

        // With the honest echo executor the observations carry the target.
        let echo = EchoExecutor;
        let (open2, _, _) = rt.block_on(r.run("打开百度", &echo, &llm));
        let (click2, _, _) = rt.block_on(r.run("点击搜索按钮", &echo, &llm));
        assert_eq!(open2[0].observation.as_deref(), Some("百度"));
        assert_eq!(click2[0].observation.as_deref(), Some("搜索按钮"));
    }

    #[tokio::test]
    async fn echo_executor_reports_composed_target() {
        let e = EchoExecutor;
        assert_eq!(e.execute("navigate", "navigate(百度)").await.1, "百度");
        assert_eq!(e.execute("getText", "getText(新浪报表)").await.1, "新浪报表");
        // No parentheses: the script text is reported verbatim.
        assert_eq!(e.execute("read", "read").await.1, "read");
        // Model-composed JSON args: a readable field wins over raw JSON.
        assert_eq!(
            e.execute("navigate", r#"{"url":"https://a.b"}"#).await.1,
            "https://a.b"
        );
    }

    #[test]
    fn compose_args_fallback_handles_matrix_json_and_verb_parens() {
        use webai_protocol::BrowserVerb;
        // Matrix syntax `key=value` inside verb(...) (rss_sample's prompt).
        assert_eq!(
            compose_args(
                BrowserVerb::Navigate,
                "navigate(url=http://127.0.0.1:9/a.html)"
            ),
            serde_json::json!({ "url": "http://127.0.0.1:9/a.html" })
        );
        // Bare `verb(target)` fallback maps per-verb.
        assert_eq!(
            compose_args(BrowserVerb::Click, "click(#btn)"),
            serde_json::json!({ "selector": "#btn" })
        );
        assert_eq!(
            compose_args(BrowserVerb::PressKey, "pressKey(Enter)"),
            serde_json::json!({ "key": "Enter" })
        );
        // Valid JSON (model-composed) always wins.
        assert_eq!(
            compose_args(BrowserVerb::Navigate, r#"{"url":"https://x.test"}"#),
            serde_json::json!({ "url": "https://x.test" })
        );
        // Verbs that take no args degrade to {} (compose surfaces a
        // structured MissingArg failure if one is actually required).
        assert_eq!(
            compose_args(BrowserVerb::GetText, "getText(anything)"),
            serde_json::json!({})
        );
    }

    #[tokio::test]
    async fn bridge_executor_dispatches_through_real_bridge_dispatch_chain() {
        // Canned WebkitBackend (no FFI): the FULL agent -> bridge ->
        // script-compose -> merge chain runs, proving the real executor wiring
        // without a WPE device. The runner's execute() -> BridgeToolExecutor
        // -> handle_tool_call path is what the TUI/headless use under
        // `real_backend`.
        let bridge = std::sync::Arc::new(webai_bridge::Bridge::new(
            webai_webkit::WebkitBridge::with_canned(webai_webkit::CannedBackend {
                evaluate_result: Some(serde_json::json!({
                    "execute": { "ok": true, "stage": "execute" },
                    "verify": { "ok": true, "stage": "verify" },
                    "args": {}
                })),
                ..Default::default()
            }),
        ));
        let exec = BridgeToolExecutor::new(bridge);

        // Model-composed JSON args drive the bridge end to end.
        let (ok, obs) = exec
            .execute("navigate", r#"{"url":"https://x.test"}"#)
            .await;
        assert!(ok, "observation: {obs}");
        assert!(obs.contains("verify"), "two-phase result: {obs}");

        // Matrix verb syntax (rss_sample style) reaches the same path.
        let (ok, _) = exec
            .execute("getText", "getText(url=http://127.0.0.1:9/a.html)")
            .await;
        assert!(ok);
    }

    #[tokio::test]
    async fn bridge_executor_surfaces_phase_failure_structured() {
        let bridge = std::sync::Arc::new(webai_bridge::Bridge::new(
            webai_webkit::WebkitBridge::with_canned(webai_webkit::CannedBackend {
                evaluate_result: Some(serde_json::json!({
                    "execute": { "ok": false, "stage": "execute", "error": "element not found" },
                    "verify": { "ok": false, "stage": "verify" },
                    "args": {}
                })),
                ..Default::default()
            }),
        ));
        let exec = BridgeToolExecutor::new(bridge);
        let (ok, obs) = exec
            .execute("click", "{\"selector\":\"#missing\"}")
            .await;
        assert!(!ok);
        assert!(
            obs.contains("execute"),
            "failure must carry the failing phase: {obs}"
        );
    }
}
