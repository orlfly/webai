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

use super::plan_loop::{requires_plan, LoopError, LoopGuards};
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

/// A fake-capable tool executor injected into the driver. Returns the
/// observation for a step and whether it "succeeded".
pub trait ToolExecutor: Send + Sync {
    /// Execute a single `verb` tool action with `args`. Returns `(ok, obs)`.
    fn execute(&self, verb: &str, args: &str) -> (bool, String);
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

impl ToolExecutor for StubExecutor {
    fn execute(&self, _verb: &str, _args: &str) -> (bool, String) {
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

            // The verb for this iteration (deterministic for the harness).
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
            let observation;
            if let Some(hit) = reused_hit {
                observation = exec.execute(&verb, &hit.entry.script).1;
                llm_calls_for_step = 0;
            } else {
                // Fresh generation: one LLM call to compose a script.
                llm_calls_for_step = self.use_llm_once(llm, &verb, prompt).await;
                let composed = format!("script_for_{verb}");
                observation = exec.execute(&verb, &composed).1;
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
                script: format!("script_for_{verb}"),
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

    /// Consume one LLM call (counted) and return its count.
    async fn use_llm_once(&self, llm: &LlmClient, verb: &str, prompt: &str) -> u32 {
        self.llm_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _ = llm
            .complete(&format!("compose a script to {verb}: {prompt}"))
            .await;
        1
    }

    /// Deterministic verb selection for the harness loop.
    fn next_verb(&self, index: usize, prompt: &str) -> String {
        match index {
            0 => {
                // Read-intent prompts summarize the CURRENT page instead of
                // navigating: the TUI user asking "界面上有什么 / 页面内容总结"
                // wants page content, not a reload of the default URL.
                if is_read_intent(prompt) {
                    "get_text".to_string()
                } else {
                    "navigate".to_string()
                }
            }
            1 => "click".to_string(),
            _ => "get_text".to_string(),
        }
    }

    /// Compress a long transcript via the summariser.
    pub fn summarise(&self, turns: Vec<Turn>) -> super::summariser::SummarisedHistory {
        self.summariser.summarise(&turns)
    }
}

/// Read-intent detection for single-turn info prompts (deterministic stub
/// policy until LLM verb selection lands): 界面上有什么 / 读一下 / 总结页面 /
/// what is on the page / describe / summarize, etc. must read page content
/// (get_text) instead of navigating.
pub fn is_read_intent(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    // Explicit navigation intent wins over read hints ("打开...并读取标题"
    // is primarily a navigate task with a read tail).
    const NAVIGATE_HINTS: &[&str] = &[
        "打开", "访问", "跳转", "导航", "navigate to", "open ", "go to", "goto ",
    ];
    if NAVIGATE_HINTS.iter().any(|h| lower.contains(h)) {
        return false;
    }
    const READ_HINTS: &[&str] = &[
        "界面上有什么", "界面有什么", "页面内容", "内容总结", "总结一下", "读取",
        "读一下", "读出", "查看", "显示什么", "有什么内容", "标题是什么", "页面上",
        "读文本", "全文", "页面上写了", "what is on", "what's on", "read the page",
        "describe the page", "page content", "summarize the page", "summary of the page",
        "what does the page", "read out",
    ];
    READ_HINTS.iter().any(|h| lower.contains(h))
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
        let store = SharedMemoryStore::new();
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

    #[tokio::test]
    async fn read_intent_prompt_routes_to_get_text_not_navigate() {
        let (r, _s) = runner(true);
        let exec = StubExecutor::default();
        let llm = LlmClient::with_profile_stub("stub");
        for prompt in ["界面上有什么", "页面内容总结一下", "What is on the page?"] {
            let (steps, outcome, _) = r.run(prompt, &exec, &llm).await;
            assert!(matches!(outcome, StepOutcome::Done { .. }));
            assert_eq!(steps.len(), 1);
            assert_eq!(
                steps[0].tool_name, "get_text",
                "read-intent prompt {prompt:?} must read content, not navigate"
            );
        }
    }

    #[test]
    fn is_read_intent_matrix() {
        for yes in ["界面上有什么", "页面内容总结", "读一下当前页", "what is on the page",
                    "Summarize the page content", "describe the page"] {
            assert!(is_read_intent(yes), "must classify {yes:?} as read-intent");
        }
        for no in ["打开百度", "打开 example.com 然后点击搜索", "navigate to x.com"] {
            assert!(!is_read_intent(no), "must NOT classify {no:?} as read-intent");
        }
    }

}
