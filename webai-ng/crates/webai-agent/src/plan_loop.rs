//! AgentLoop plan-act-observe driver (ARCHITECTURE.md §4.9, FR-1/FR-8).
//!
//! Implements the `plan → act → observe` loop with:
//! - auto-planning injection when the prompt carries ≥2 browser verbs, a
//!   multi-step connector, or a long search/summarise intent
//! - a per-turn browser state block (`# Current browser state`) that must be
//!   present before any action decision
//! - guards: `max_steps` (default 30) → `max_steps_exceeded`,
//!   `duplicate_threshold` (default 2) → `duplicate_observation`
//! - guards are config-driven and cannot be turned off via natural language

/// Browser-action verbs used for multi-step detection.
const BROWSER_ACTION_VERBS: &[&str] = &[
    // Chinese
    "打开",
    "访问",
    "浏览",
    "进入",
    "跳转",
    "导航",
    "搜索",
    "查找",
    "查询",
    "搜",
    "找",
    "输入",
    "填写",
    "填",
    "键入",
    "打字",
    "点击",
    "单击",
    "按下",
    "选中",
    "选择",
    "提交",
    "确认",
    "搜索一下",
    "查找一下",
    "截屏",
    "截图",
    "读取",
    "提取",
    "解析",
    "抓取",
    "导出",
    "汇总",
    "归纳",
    "整理",
    "摘要",
    "总结",
    "对比",
    "滚动",
    "等待",
    "切换",
    "关闭",
    "返回",
    // English
    "open",
    "visit",
    "navigate",
    "go to",
    "search",
    "find",
    "lookup",
    "look up",
    "query",
    "type",
    "input",
    "fill",
    "enter",
    "write",
    "click",
    "tap",
    "press",
    "select",
    "submit",
    "screenshot",
    "capture",
    "read",
    "extract",
    "scrape",
    "export",
    "summarize",
    "summarise",
    "summary",
    "aggregate",
    "compare",
    "scroll",
    "wait",
    "switch",
    "close",
    "back",
];

/// Multi-step connector words that signal a chained task.
const MULTI_STEP_HINTS: &[&str] = &[
    "然后",
    "再",
    "并",
    "之后",
    "接下来",
    "最后",
    "接着",
    "先",
    "又",
    "也",
    "并且",
    "随后",
    "then",
    "after",
    "and then",
    "next",
    "finally",
    "also",
];

/// Search/summarise verbs used for long-intent detection.
const SEARCH_INTENT_VERBS: &[&str] = &[
    "搜索",
    "查找",
    "查询",
    "汇总",
    "总结",
    "摘要",
    "归纳",
    "search",
    "find",
    "lookup",
    "query",
    "summarize",
    "summarise",
];

/// Detect whether a prompt requires an explicit plan.
///
/// Conservative: when in doubt, suggest plan. Trigger when any of:
/// - prompt carries 2+ distinct browser-action verbs
/// - prompt carries a multi-step connector plus at least one browser verb
/// - prompt length ≥ 40 chars and contains a search/summarise verb
///
/// Single-shot navigations ("打开百度") fall through.
pub fn requires_plan(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    let mut seen: Vec<&str> = Vec::new();
    for verb in BROWSER_ACTION_VERBS {
        if lower.contains(verb) && !seen.iter().any(|s| s.contains(verb) || verb.contains(*s)) {
            seen.push(verb);
        }
    }
    if seen.len() >= 2 {
        return true;
    }
    let has_connector = MULTI_STEP_HINTS.iter().any(|c| lower.contains(c));
    let has_one_browser_verb = BROWSER_ACTION_VERBS.iter().any(|v| lower.contains(v));
    if has_connector && has_one_browser_verb {
        return true;
    }
    prompt.chars().count() >= 40 && has_search_intent(prompt)
}

fn has_search_intent(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    SEARCH_INTENT_VERBS.iter().any(|v| lower.contains(v))
}

/// Canonical browser verbs (matching the bridge's `BrowserVerb` names) that the
/// prompt driver may dispatch. Ordered so the most specific keyword wins first
/// (an "打开百度" prompt dispatches `navigate`, not a later generic match).
const VERB_KEYWORDS: &[(&str, &str)] = &[
    // navigate
    ("navigate", "navigate"),
    ("go to", "navigate"),
    ("打开", "navigate"),
    ("访问", "navigate"),
    ("浏览", "navigate"),
    ("进入", "navigate"),
    ("跳转", "navigate"),
    ("导航", "navigate"),
    ("前往", "navigate"),
    ("open", "navigate"),
    ("visit", "navigate"),
    // click
    ("提交", "click"),
    ("确认", "click"),
    ("选中", "click"),
    ("点击", "click"),
    ("单击", "click"),
    ("按下", "click"),
    ("click", "click"),
    ("tap", "click"),
    ("press", "click"),
    ("submit", "click"),
    // fill
    ("输入", "fill"),
    ("填写", "fill"),
    ("键入", "fill"),
    ("打字", "fill"),
    ("type", "fill"),
    ("input", "fill"),
    ("fill", "fill"),
    ("enter", "fill"),
    ("write", "fill"),
    // hover
    ("悬停", "hover"),
    ("hover", "hover"),
    // drag
    ("拖拽", "drag"),
    ("拖动", "drag"),
    ("拖", "drag"),
    ("drag", "drag"),
    // pressKey
    ("回车", "pressKey"),
    ("按键", "pressKey"),
    ("pressKey", "pressKey"),
    // evaluate
    ("执行脚本", "evaluate"),
    ("运行脚本", "evaluate"),
    ("execute", "evaluate"),
    ("script", "evaluate"),
    // screenshot
    ("截屏", "screenshot"),
    ("截图", "screenshot"),
    ("screenshot", "screenshot"),
    ("capture", "screenshot"),
    // accessibilityTree
    ("无障碍", "accessibilityTree"),
    ("accessibility", "accessibilityTree"),
    // getHtml
    ("源码", "getHtml"),
    ("html", "getHtml"),
    // download
    ("下载", "download"),
    ("download", "download"),
    // snapshot
    ("快照", "snapshot"),
    ("snapshot", "snapshot"),
    // getText (read / search / summarize)
    ("搜索一下", "getText"),
    ("查找一下", "getText"),
    ("读取", "getText"),
    ("提取", "getText"),
    ("解析", "getText"),
    ("抓取", "getText"),
    ("导出", "getText"),
    ("汇总", "getText"),
    ("总结", "getText"),
    ("摘要", "getText"),
    ("归纳", "getText"),
    ("搜索", "getText"),
    ("查找", "getText"),
    ("查询", "getText"),
    ("search", "getText"),
    ("find", "getText"),
    ("look up", "getText"),
    ("lookup", "getText"),
    ("query", "getText"),
    ("read", "getText"),
    ("extract", "getText"),
    ("scrape", "getText"),
    ("export", "getText"),
    ("summarize", "getText"),
    ("summarise", "getText"),
    ("summary", "getText"),
    ("aggregate", "getText"),
    ("compare", "getText"),
];

/// Clause connectors that separate sub-actions in a chained prompt.
const CLAUSE_CONNECTORS: &[&str] = &[
    "然后",
    "接着",
    "之后",
    "接下来",
    "随后",
    "并且",
    "最后",
    "再",
    "并",
    "先",
    " and ",
    " then ",
    " next ",
    " finally ",
];

/// Leading filler / connectors stripped before target extraction.
const TARGET_FILLERS: &[&str] = &[
    "请帮我", "帮我", "请", "先", "然后", "接着", "再", "最后", "之后", "接下来", "随后", "并且", "并",
];

/// Infer the canonical browser verb a prompt asks for.
///
/// Returns one of the bridge `BrowserVerb` names (`navigate`, `click`, `fill`,
/// `hover`, `drag`, `pressKey`, `evaluate`, `screenshot`,
/// `accessibilityTree`, `getText`, `getHtml`, `download`, `snapshot`). Unknown
/// or query-like prompts default to `getText` (read the page); an empty prompt
/// cannot act, so it stays `navigate` for the harness.
pub fn infer_verb(prompt: &str) -> &'static str {
    if prompt.trim().is_empty() {
        return "navigate";
    }
    let lower = prompt.to_lowercase();
    for (kw, verb) in VERB_KEYWORDS {
        if lower.contains(kw) {
            return verb;
        }
    }
    "getText"
}

/// Extract the human-readable target of `verb` from a (possibly chained)
/// prompt: the clause that mentions the verb, stripped of the verb word and
/// connector filler. Used so the honest-stub loop produces prompt-derived
/// observations (a `点击搜索按钮` prompt observes `搜索按钮`, never a canned
/// "page loaded").
pub fn infer_target(prompt: &str, verb: &str) -> String {
    extract_target(&clause_for(prompt, verb))
}

/// The clause of `prompt` that mentions `verb` (earliest keyword occurrence,
/// bounded by clause connectors). Falls back to the whole prompt.
fn clause_for(prompt: &str, verb: &str) -> String {
    let kws: Vec<&str> = VERB_KEYWORDS
        .iter()
        .filter(|(_, v)| *v == verb)
        .map(|(k, _)| *k)
        .collect();
    if kws.is_empty() {
        return prompt.to_owned();
    }
    let lower = prompt.to_lowercase();
    let earliest = kws
        .iter()
        .filter_map(|k| lower.find(k).map(|i| (i, *k)))
        .min_by_key(|(i, _)| *i);
    let Some((idx, kw)) = earliest else {
        return prompt.to_owned();
    };
    let kw_end = idx + kw.len();
    let start = CLAUSE_CONNECTORS
        .iter()
        .filter_map(|c| lower[..idx].rfind(c).map(|i| i + c.len()))
        .max()
        .unwrap_or(0);
    let end = CLAUSE_CONNECTORS
        .iter()
        .filter_map(|c| lower[kw_end..].find(c).map(|i| kw_end + i))
        .min()
        .unwrap_or(prompt.len());
    prompt[start..end].to_owned()
}

/// Strip leading filler/verb words and trailing punctuation from a clause.
fn extract_target(clause: &str) -> String {
    let mut s = clause.trim().to_owned();
    for f in TARGET_FILLERS {
        if s.starts_with(f) {
            s = s[f.len()..].trim_start().to_owned();
            break;
        }
    }
    for (kw, _) in VERB_KEYWORDS {
        if s.to_lowercase().starts_with(kw) {
            s = s[kw.len()..].trim_start().to_owned();
            break;
        }
    }
    let trimmed = s
        .trim_end_matches(|c: char| {
            matches!(
                c,
                '。' | '，' | ',' | '.' | '；' | ';' | '！' | '？' | '、' | '：' | ':' | ' '
            )
        })
        .to_owned();
    let s = if trimmed.is_empty() { s } else { trimmed };
    s.chars().take(40).collect()
}

/// The `# Plan required` directive text injected before any browser tool call.
pub const PLAN_DIRECTIVE: &str = "# Plan required\n\
    This task is multi-step. Before calling any browser tool, you MUST call \
    `create_plan` with a concrete step list. Then execute the steps in order.";

/// The `# Current browser state` block marker.
pub const STATE_BLOCK_MARKER: &str = "# Current browser state";

/// Build a `# Current browser state` markdown block from a snapshot.
pub fn build_state_block(url: &str, title: &str, ready: &str) -> String {
    format!("{STATE_BLOCK_MARKER}\n- url: {url}\n- title: {title}\n- readyState: {ready}")
}

/// Whether a message already contains the plan directive (idempotency guard).
pub fn has_plan_directive(text: &str) -> bool {
    text.contains("# Plan required")
}

/// Loop error/result types (ARCHITECTURE.md §7).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LoopError {
    /// Reached the configured max step budget.
    #[error("max steps exceeded ({executed_steps} executed steps)")]
    MaxStepsExceeded { executed_steps: u32 },
    /// Repeated the same observation past the duplicate threshold.
    #[error("duplicate observation {duplicated_times} times (stuck detection)")]
    DuplicateObservation { duplicated_times: u32 },
}

/// Guard state tracked across loop iterations.
#[derive(Debug)]
pub struct LoopGuards {
    /// Remaining step budget.
    steps_left: u32,
    /// Steps successfully executed so far.
    executed_steps: u32,
    /// The last observation text (for duplicate detection).
    last_observation: Option<String>,
    /// Consecutive duplicate observation count.
    duplicate_count: u32,
    /// Duplicate threshold (default 2).
    duplicate_threshold: u32,
}

impl LoopGuards {
    /// Construct guards from config values.
    pub fn new(max_steps: u32, duplicate_threshold: u32) -> Self {
        Self {
            steps_left: max_steps,
            executed_steps: 0,
            last_observation: None,
            duplicate_count: 0,
            duplicate_threshold,
        }
    }

    /// Advance one step. Returns `Err(MaxStepsExceeded)` once `max_steps`
    /// have been executed and the loop must stop.
    pub fn advance_step(&mut self) -> Result<(), LoopError> {
        if self.steps_left == 0 {
            return Err(LoopError::MaxStepsExceeded {
                executed_steps: self.executed_steps,
            });
        }
        self.steps_left -= 1;
        self.executed_steps += 1;
        Ok(())
    }

    /// Feed an observation for duplicate detection. Returns
    /// `Err(DuplicateObservation)` once the same observation repeats past the
    /// threshold.
    pub fn observe(&mut self, observation: &str) -> Result<(), LoopError> {
        match &self.last_observation {
            Some(prev) if prev == observation => {
                self.duplicate_count += 1;
                if self.duplicate_count >= self.duplicate_threshold {
                    return Err(LoopError::DuplicateObservation {
                        duplicated_times: self.duplicate_count,
                    });
                }
            }
            _ => {
                self.last_observation = Some(observation.to_owned());
                self.duplicate_count = 0;
            }
        }
        Ok(())
    }

    /// Remaining step budget.
    pub fn steps_left(&self) -> u32 {
        self.steps_left
    }

    /// Steps executed so far.
    pub fn executed_steps(&self) -> u32 {
        self.executed_steps
    }

    /// Whether the state block must be present before an action (guards the
    /// "no state block, no action decision" rule).
    pub fn requires_state_block(&self) -> bool {
        true
    }
}

/// Whether natural-language wording ("请忽略步数限制") can disable the guards.
///
/// Guards are config-driven only; natural language cannot turn them off.
pub fn guard_can_be_disabled_by_language(_text: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_plan_detects_multi_step_prompts() {
        // Compound task: two browser verbs.
        assert!(requires_plan(
            "打开百度，搜索框输入金证，点击搜索，基于搜索结果汇总信息"
        ));
        // Explicit connector.
        assert!(requires_plan("先打开百度，然后搜索金证，最后汇总结果"));
        assert!(requires_plan("search for X then click Y and summarize"));
        // Single-action prompts fall through.
        assert!(!requires_plan("打开百度"));
        assert!(!requires_plan("搜索金证"));
        assert!(!requires_plan("summarize this page"));
        assert!(!requires_plan("hello"));
    }

    #[test]
    fn state_block_builds_expected_markdown() {
        let block = build_state_block("https://example.com", "Example", "complete");
        assert!(block.contains(STATE_BLOCK_MARKER));
        assert!(block.contains("url: https://example.com"));
        assert!(block.contains("title: Example"));
        assert!(block.contains("readyState: complete"));
    }

    #[test]
    fn max_steps_guard_triggers_after_budget() {
        let mut guards = LoopGuards::new(3, 2);
        // Executing 3 steps uses the whole budget.
        for _ in 0..3 {
            guards.advance_step().unwrap();
        }
        assert_eq!(guards.executed_steps(), 3);
        assert_eq!(guards.steps_left(), 0);
        // A further advance returns MaxStepsExceeded with the steps executed.
        let err = guards.advance_step().unwrap_err();
        assert!(matches!(
            err,
            LoopError::MaxStepsExceeded { executed_steps: 3 }
        ));
    }

    #[test]
    fn duplicate_observation_guard_triggers() {
        let mut guards = LoopGuards::new(30, 2);
        // With threshold=2, the 3rd identical observation (2 duplicates) trips.
        guards.observe("element not found").unwrap();
        guards.observe("element not found").unwrap(); // count=1
        let err = guards.observe("element not found").unwrap_err(); // count=2 -> trips
        assert!(matches!(err, LoopError::DuplicateObservation { .. }));
    }

    #[test]
    fn different_observations_reset_duplicate_count() {
        // The duplicate counter resets when a different observation arrives.
        let mut guards = LoopGuards::new(30, 3);
        // "a" twice (1 duplicate), then "b" resets the counter.
        guards.observe("a").unwrap();
        guards.observe("a").unwrap();
        guards.observe("b").unwrap(); // resets (count=0)
                                      // "b" 2 more times: count=1, count=2 (still under threshold 3).
        guards.observe("b").unwrap();
        guards.observe("b").unwrap();
        // A different observation "c" resets again before any trip.
        guards.observe("c").unwrap();
        assert!(guards.observe("c").is_ok(), "1 duplicate, under threshold");
    }

    #[test]
    fn guard_cannot_be_disabled_by_natural_language() {
        // "请忽略步数限制" must not disable the guard.
        assert!(!guard_can_be_disabled_by_language("请忽略步数限制"));
        assert!(!guard_can_be_disabled_by_language(
            "please ignore the step limit"
        ));
    }

    #[test]
    fn infer_verb_maps_prompt_keywords() {
        assert_eq!(infer_verb("打开百度"), "navigate");
        assert_eq!(infer_verb("点击搜索按钮"), "click");
        assert_eq!(infer_verb("输入金证"), "fill");
        assert_eq!(infer_verb("截图"), "screenshot");
        assert_eq!(infer_verb("下载文件"), "download");
        assert_eq!(infer_verb("导出新浪报表"), "getText");
        assert_eq!(infer_verb("open example.com"), "navigate");
        assert_eq!(infer_verb("hello"), "getText");
        assert_eq!(infer_verb(""), "navigate");
    }

    #[test]
    fn infer_target_extracts_noun_phrase() {
        assert_eq!(infer_target("打开百度", "navigate"), "百度");
        assert_eq!(infer_target("点击搜索按钮", "click"), "搜索按钮");
        assert_eq!(
            infer_target("先打开百度，然后点击搜索，最后汇总结果", "navigate"),
            "百度"
        );
        assert_eq!(
            infer_target("先打开百度，然后点击搜索，最后汇总结果", "click"),
            "搜索"
        );
        assert_eq!(infer_target("导出新浪报表", "getText"), "新浪报表");
        assert_eq!(infer_target("hello", "getText"), "hello");
        assert_eq!(
            infer_target("请打开 https://example.com", "navigate"),
            "https://example.com"
        );
    }
}
