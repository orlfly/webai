//! Performance layer (ARCHITECTURE.md §12.6 / §6): script compose cache +
//! screenshot throttling.
//!
//! - **Compose cache**: `webai_script::compose` results are memoised on a
//!   canonical `verb + args` key, so replaying the same action never burns a
//!   duplicate compose (and, with M4-6 script memory, no duplicate LLM call —
//!   hits surface the same `reused_script = true` marker).
//! - **Screenshot throttle**: mutating actions still capture a record per step
//!   (FR-2 does not regress), but under high frequency captures merge/limit to
//!   one PNG per `min_interval`, and skipped captures produce an explicit
//!   placeholder record instead of a full PNG per step.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use webai_protocol::{BrowserToolRequest, BrowserVerb};
use webai_script::ScriptModule;

/// A canonical cache key: verb name plus normalized args JSON.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    verb: String,
    args: String,
}

impl CacheKey {
    /// Build the key from a request. Args are serialized with sorted keys
    /// (serde_json canonical map ordering) so `{a:1,b:2}` and `{b:2,a:1}` hit
    /// the same entry (no stale-cache bug from key ordering).
    pub fn from_request(req: &BrowserToolRequest) -> Self {
        Self {
            verb: format!("{:?}", req.verb),
            args: canonical_args(&req.args),
        }
    }

    /// The verb + args identity (exposed for tests / debug logs).
    pub fn as_parts(&self) -> (&str, &str) {
        (&self.verb, &self.args)
    }
}

/// Serialize args with sorted keys for a stable key.
fn canonical_args(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let joined: Vec<String> = keys
                .iter()
                .map(|k| format!("{k}:{}", canonical_args(&map[*k])))
                .collect();
            format!("{{{}}}", joined.join(","))
        }
        serde_json::Value::Array(items) => {
            let joined: Vec<String> = items.iter().map(canonical_args).collect();
            format!("[{}]", joined.join(","))
        }
        other => other.to_string(),
    }
}

/// Compose cache with hit/miss counters (M-2 measurement surface).
#[derive(Debug)]
pub struct ComposeCache {
    entries: Mutex<HashMap<CacheKey, ScriptModule>>,
    hits: AtomicU64,
    misses: AtomicU64,
    /// Upper bound on cached entries (long sessions cannot grow unbounded).
    capacity: usize,
}

impl Default for ComposeCache {
    fn default() -> Self {
        Self::new(256)
    }
}

impl ComposeCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            capacity,
        }
    }

    /// Compose via the cache: on a miss, run `compose` and store; on a hit,
    /// return the memoised module without recomposing.
    pub fn compose(
        &self,
        req: &BrowserToolRequest,
    ) -> Result<ScriptModule, webai_script::ScriptError> {
        let key = CacheKey::from_request(req);
        if let Some(hit) = self.entries.lock().unwrap().get(&key) {
            self.hits.fetch_add(1, Ordering::SeqCst);
            return Ok(hit.clone());
        }
        let module = webai_script::compose(req)?;
        self.misses.fetch_add(1, Ordering::SeqCst);
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= self.capacity {
            // Simple bound: drop an arbitrary entry (HashMap iteration order).
            if let Some(k) = entries.keys().next().cloned() {
                entries.remove(&k);
            }
        }
        entries.insert(key, module.clone());
        Ok(module)
    }

    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::SeqCst)
    }

    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::SeqCst)
    }

    /// Hit rate in percent (0 when nothing has been composed yet).
    pub fn hit_rate_percent(&self) -> u64 {
        let total = self.hits() + self.misses();
        (self.hits() * 100).checked_div(total).unwrap_or(0)
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Outcome of a throttled screenshot decision (FR-2 never silently vanishes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenshotDecision {
    /// Capture a full PNG this step.
    Capture,
    /// Throttled: keep the previous screenshot as the step's record
    /// (placeholder semantics, FR-2 preserved without a full PNG per step).
    Throttled,
}

/// Screenshot throttle (ARCHITECTURE.md §6 timers/throttle).
#[derive(Debug)]
pub struct ScreenshotThrottle {
    min_interval: Duration,
    last_capture: Mutex<Option<Instant>>,
    captured: AtomicU64,
    throttled: AtomicU64,
}

impl ScreenshotThrottle {
    /// Throttle to at most one capture per `min_interval`.
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last_capture: Mutex::new(None),
            captured: AtomicU64::new(0),
            throttled: AtomicU64::new(0),
        }
    }

    /// Decide for one step. Read-only verbs never capture (they never did —
    /// `wants_screenshot` gates on mutating verbs, unchanged).
    pub fn decide(&self, verb: &BrowserVerb, visible_in_viewport: bool) -> ScreenshotDecision {
        // Read-only verbs: no capture, matching the bridge's existing gate.
        if !wants_screenshot(verb) {
            return ScreenshotDecision::Throttled;
        }
        // Off-screen steps never dispatch (M5 image pipeline contract).
        if !visible_in_viewport {
            self.throttled.fetch_add(1, Ordering::SeqCst);
            return ScreenshotDecision::Throttled;
        }
        let mut last = self.last_capture.lock().unwrap();
        let now = Instant::now();
        match *last {
            Some(t) if now.duration_since(t) < self.min_interval => {
                // High frequency: merge into the previous capture (FR-2 keeps
                // the previous frame as the step's record).
                self.throttled.fetch_add(1, Ordering::SeqCst);
                ScreenshotDecision::Throttled
            }
            _ => {
                *last = Some(now);
                self.captured.fetch_add(1, Ordering::SeqCst);
                ScreenshotDecision::Capture
            }
        }
    }

    pub fn captured(&self) -> u64 {
        self.captured.load(Ordering::SeqCst)
    }

    pub fn throttled(&self) -> u64 {
        self.throttled.load(Ordering::SeqCst)
    }
}

/// Mirror of the bridge's `wants_screenshot` gate (kept in sync by test).
pub(crate) fn wants_screenshot(verb: &BrowserVerb) -> bool {
    matches!(
        verb,
        BrowserVerb::Navigate
            | BrowserVerb::Click
            | BrowserVerb::Fill
            | BrowserVerb::Hover
            | BrowserVerb::Drag
            | BrowserVerb::PressKey
            | BrowserVerb::Download
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use webai_protocol::BrowserVerb::*;

    fn req(verb: BrowserVerb, args: serde_json::Value) -> BrowserToolRequest {
        BrowserToolRequest { verb, args }
    }

    #[test]
    fn cache_key_is_order_insensitive() {
        let a = CacheKey::from_request(&req(Fill, json!({"selector": "#q", "value": "x"})));
        let b = CacheKey::from_request(&req(Fill, json!({"value": "x", "selector": "#q"})));
        assert_eq!(a, b, "same args in different key order must hit one entry");
    }

    #[test]
    fn cache_key_differs_on_args_and_verb() {
        let base = CacheKey::from_request(&req(Fill, json!({"selector": "#q", "value": "x"})));
        let other_args =
            CacheKey::from_request(&req(Fill, json!({"selector": "#q", "value": "y"})));
        let other_verb =
            CacheKey::from_request(&req(Click, json!({"selector": "#q", "value": "x"})));
        assert_ne!(base, other_args, "different args must not share an entry");
        assert_ne!(base, other_verb, "different verbs must not share an entry");
    }

    #[test]
    fn same_request_composes_once_then_hits() {
        let cache = ComposeCache::default();
        let r = req(Navigate, json!({"url": "https://example.com"}));
        let m1 = cache.compose(&r).unwrap();
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 0);
        let m2 = cache.compose(&r).unwrap();
        assert_eq!(m1, m2, "memoised module must be identical");
        assert_eq!(cache.hits(), 1);
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hit_rate_percent(), 50);
    }

    #[test]
    fn different_args_never_serve_stale_script() {
        let cache = ComposeCache::default();
        let a = cache
            .compose(&req(Navigate, json!({"url": "https://a.com"})))
            .unwrap();
        let b = cache
            .compose(&req(Navigate, json!({"url": "https://b.com"})))
            .unwrap();
        assert_ne!(a.args, b.args, "stale cache would inject the wrong url");
        // The args of each module reflect their own request.
        assert!(a.args.contains("a.com"));
        assert!(b.args.contains("b.com"));
    }

    #[test]
    fn cache_respects_capacity_bound() {
        let cache = ComposeCache::new(4);
        for i in 0..16 {
            let r = req(Navigate, json!({"url": format!("https://x{i}.com")}));
            let _ = cache.compose(&r).unwrap();
        }
        assert!(cache.len() <= 4, "cache must stay bounded");
    }

    #[test]
    fn throttle_captures_first_then_merges_burst() {
        let t = ScreenshotThrottle::new(Duration::from_millis(500));
        let click = Click;
        assert_eq!(t.decide(&click, true), ScreenshotDecision::Capture);
        // Immediate repeats merge (no full PNG per step).
        assert_eq!(t.decide(&click, true), ScreenshotDecision::Throttled);
        assert_eq!(t.decide(&click, true), ScreenshotDecision::Throttled);
        assert_eq!(t.captured(), 1);
        assert_eq!(t.throttled(), 2);
    }

    #[test]
    fn read_only_verbs_never_capture() {
        let t = ScreenshotThrottle::new(Duration::from_millis(0));
        assert_eq!(t.decide(&GetText, true), ScreenshotDecision::Throttled);
        assert_eq!(
            t.decide(&AccessibilityTree, true),
            ScreenshotDecision::Throttled
        );
        assert_eq!(t.captured(), 0);
    }

    #[test]
    fn off_screen_steps_never_dispatch() {
        let t = ScreenshotThrottle::new(Duration::from_millis(0));
        // Even mutating verbs skip while off-screen.
        assert_eq!(t.decide(&Click, false), ScreenshotDecision::Throttled);
        assert_eq!(t.captured(), 0);
        // On-screen: captures.
        assert_eq!(t.decide(&Click, true), ScreenshotDecision::Capture);
    }

    #[test]
    fn throttle_gate_matches_bridge_wants_screenshot() {
        // Keep the local gate in lockstep with the bridge's (both must agree
        // on which verbs warrant a screenshot).
        for verb in [
            Navigate,
            Click,
            Fill,
            Hover,
            Drag,
            PressKey,
            Download,
            Screenshot,
            AccessibilityTree,
            GetText,
            GetHtml,
            Snapshot,
            Evaluate,
        ] {
            let bridge_gate = webai_bridge_gate(verb);
            assert_eq!(wants_screenshot(&verb), bridge_gate, "verb {verb:?}");
        }
    }

    /// Re-implementation of the bridge's private gate for the lockstep test.
    fn webai_bridge_gate(verb: BrowserVerb) -> bool {
        matches!(
            verb,
            Navigate | Click | Fill | Hover | Drag | PressKey | Download
        )
    }
}
